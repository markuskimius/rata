//! End-to-end tests: run the real `rata` binary against the `testsvc` example.
//!
//! Every test gets its own temporary `$RATA` and a unique `--tag`, so tests
//! running in parallel never match each other's processes.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

/// Path to the built `testsvc` example, building it if needed.
fn testsvc() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        // Examples are uplifted to <target>/<profile>/examples. Test binaries
        // live somewhere below <target>/<profile>, and the exact folder differs
        // between Cargo versions, so search upward from the test binary.
        let file = format!("testsvc{}", std::env::consts::EXE_SUFFIX);
        let exe = std::env::current_exe().unwrap();
        let found = exe
            .ancestors()
            .skip(1)
            .map(|dir| dir.join("examples").join(&file))
            .find(|p| p.is_file());
        let path = found.unwrap_or_else(|| exe.parent().unwrap().join("examples").join(&file));
        if !path.exists() {
            let ok = StdCommand::new(env!("CARGO"))
                .args(["build", "--example", "testsvc"])
                .status()
                .unwrap()
                .success();
            assert!(ok, "could not build the testsvc example");
            return exe
                .ancestors()
                .skip(1)
                .map(|dir| dir.join("examples").join(&file))
                .find(|p| p.is_file())
                .expect("testsvc example not found after building it");
        }
        path
    })
}

struct Env {
    dir: tempfile::TempDir,
    tag: String,
}

impl Env {
    fn new(test: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("work").join("cfg")).unwrap();
        Env {
            dir,
            tag: format!("{test}-{}-{nanos}", std::process::id()),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The tag for one service in this test.
    fn tag(&self, svc: &str) -> String {
        format!("{}-{svc}", self.tag)
    }

    /// A TOML array that runs testsvc tagged for `svc`, plus extra args.
    fn svc(&self, svc: &str, extra: &[&str]) -> String {
        let mut args = vec![
            testsvc().display().to_string(),
            "--tag".into(),
            self.tag(svc),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        let quoted: Vec<String> = args.iter().map(|a| format!("'{a}'")).collect();
        format!("[{}]", quoted.join(", "))
    }

    fn write_config(&self, body: &str) {
        std::fs::write(self.root().join("work/cfg/service.toml"), body).unwrap();
    }

    fn rata(&self, args: &[&str]) -> Command {
        let mut c = Command::cargo_bin("rata").unwrap();
        c.env("RATA", self.root())
            .env("RATA_LOCK_TIMEOUT", "5")
            // A hang (e.g. a service holding rata's stdout open) fails the
            // test instead of stalling CI.
            .timeout(Duration::from_secs(60))
            .args(args);
        c
    }

    fn status_json(&self) -> Value {
        let out = self.rata(&["status", "--json"]).output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn pids(&self, name: &str) -> Vec<u64> {
        let v = self.status_json();
        let svc = v["services"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .unwrap()
            .clone();
        svc["processes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["pid"].as_u64().unwrap())
            .collect()
    }

    /// The common two-service config.
    fn two_services(&self) {
        self.write_config(&format!(
            "[[service]]\nname = \"WEB\"\ndescription = \"Web Server\"\nstart = {}\n\n\
             [[service]]\nname = \"WORKER\"\nstart = {}\n",
            self.svc("web", &[]),
            self.svc("worker", &[])
        ));
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // Stop services without a tag (such as RATA_MONITOR) that this
        // test left running.
        if self.root().join("work/cfg/service.toml").is_file() {
            let _ = StdCommand::new(env!("CARGO_BIN_EXE_rata"))
                .env("RATA", self.root())
                .args(["-q", "stop", "--all", "-t", "2"])
                .output();
        }
        // Kill anything else this test left running.
        let table = rata::process::scan();
        for p in table.processes() {
            if p.cmdline.iter().any(|a| a.starts_with(&self.tag)) {
                let _ = rata::process::kill(p, &table);
            }
        }
    }
}

#[test]
fn lifecycle() {
    let env = Env::new("lifecycle");
    env.two_services();

    env.rata(&["start"]).assert().code(0).stdout(
        predicate::str::contains("WEB     started")
            .and(predicate::str::contains("WORKER  started")),
    );
    env.rata(&["status"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("Web Server").and(predicate::str::contains("running")));

    let v = env.status_json();
    assert_eq!(v["version"], 1);
    for s in v["services"].as_array().unwrap() {
        assert_eq!(s["state"], "running", "{s}");
        let log = s["log"].as_str().unwrap();
        assert!(Path::new(log).exists());
    }

    env.rata(&["stop"])
        .assert()
        .code(0)
        .stdout(predicate::str::is_match(r"(?s)WORKER\s+stopped.*WEB\s+stopped").unwrap());
    env.rata(&["status"]).assert().code(3);
    env.rata(&["stop"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("not running"));
}

#[test]
fn start_skips_running_services() {
    let env = Env::new("skip");
    env.two_services();
    env.rata(&["start", "WEB"]).assert().code(0);
    let pid = env.pids("WEB");
    env.rata(&["start"]).assert().code(0).stdout(
        predicate::str::contains("already running")
            .and(predicate::str::contains("WORKER  started")),
    );
    assert_eq!(env.pids("WEB"), pid);
}

#[test]
fn named_services_only() {
    let env = Env::new("named");
    env.two_services();
    env.rata(&["start", "WORKER"]).assert().code(0);
    env.rata(&["status", "WORKER"]).assert().code(0);
    env.rata(&["status", "WEB"]).assert().code(3);
    env.rata(&["status"]).assert().code(3);
}

#[test]
fn restart_replaces_process() {
    let env = Env::new("restart");
    env.two_services();
    env.rata(&["start"]).assert().code(0);
    let before = env.pids("WEB");
    env.rata(&["restart", "WEB"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("stopped").and(predicate::str::contains("started")));
    let after = env.pids("WEB");
    assert_eq!(after.len(), 1);
    assert_ne!(before, after);
}

#[test]
fn unknown_service_is_an_error() {
    let env = Env::new("unknown");
    env.two_services();
    env.rata(&["start", "nope"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "unknown service \"nope\" (configured: WEB, WORKER)",
        ));
    env.rata(&["status"]).assert().code(3);
}

#[test]
fn detects_and_stops_manual_start() {
    let env = Env::new("manual");
    env.two_services();
    let mut child = StdCommand::new(testsvc())
        .args(["--tag", &env.tag("web")])
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(env.pids("WEB"), [child.id() as u64]);
    env.rata(&["start", "WEB"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("already running"));
    env.rata(&["stop", "WEB"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("stopped"));
    child.wait().unwrap();
    env.rata(&["status", "WEB"]).assert().code(3);
}

#[test]
fn extra_arguments_do_not_match() {
    let env = Env::new("exact");
    env.two_services();
    let mut child = StdCommand::new(testsvc())
        .args(["--tag", &env.tag("web"), "--exit-after", "30"])
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    env.rata(&["status", "-v", "WEB"])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("near miss"));
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn forced_stop() {
    let env = Env::new("forced");
    env.write_config(&format!(
        "[[service]]\nname = \"STUBBORN\"\nstart = {}\nstop_timeout = 1\n",
        env.svc("stubborn", &["--ignore-term"])
    ));
    env.rata(&["start"]).assert().code(0);
    env.rata(&["stop"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("stopped (forced)"));
    env.rata(&["status"]).assert().code(3);
}

#[test]
fn fast_failure() {
    let env = Env::new("fastfail");
    env.write_config(&format!(
        "[[service]]\nname = \"CRASHY\"\nstart = {}\n",
        env.svc("crashy", &["--exit-now", "3"])
    ));
    env.rata(&["start"]).assert().code(1).stderr(
        predicate::str::contains("FAILED")
            .and(predicate::str::contains("exited within 500ms (code 3)")),
    );
}

#[test]
fn missing_program() {
    let env = Env::new("missing");
    env.write_config("[[service]]\nname = \"GHOST\"\nstart = \"${RATA}/no-such-program\"\n");
    env.rata(&["start"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("program not found"));
}

#[test]
fn unmatched_psline() {
    let env = Env::new("unmatched");
    env.write_config(&format!(
        "[[service]]\nname = \"ODD\"\nstart = {}\npsline = \"something else\"\n",
        env.svc("odd", &[])
    ));
    env.rata(&["start"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("no process matches its psline"));
    env.rata(&["status"]).assert().code(3);
}

#[test]
fn log_file_gets_output() {
    let env = Env::new("logs");
    env.two_services();
    env.rata(&["start", "WEB"]).assert().code(0);
    let logs: Vec<_> = std::fs::read_dir(env.root().join("work/log"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(logs.len(), 1);
    let re = regex_lite(&logs[0]);
    assert!(re, "unexpected log name {}", logs[0]);
}

fn regex_lite(name: &str) -> bool {
    // WEB_YYYYMMDD_HHMMSS.n.log
    let Some(rest) = name.strip_prefix("WEB_") else {
        return false;
    };
    let b = rest.as_bytes();
    rest.len() >= 21
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'_'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && rest[15..].starts_with('.')
        && rest.ends_with(".log")
}

#[test]
fn check_reports_matches() {
    let env = Env::new("check");
    env.two_services();
    env.rata(&["start", "WEB"]).assert().code(0);
    env.rata(&["check"]).assert().code(0).stdout(
        predicate::str::contains("OK (2 services)")
            .and(predicate::str::contains("matches    1 process"))
            .and(predicate::str::contains("none (not running)")),
    );
}

#[test]
fn invalid_config() {
    let env = Env::new("invalid");
    env.write_config("[[service]]\nname = \"1bad\"\nstart = \"x\"\n");
    env.rata(&["check"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "service.toml:2:8: invalid service name \"1bad\"",
        ));
    env.write_config("[[service]]\nname = \"OK\"\nstart = \"x\"\npslin = \"y\"\n");
    env.rata(&["start"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown field `pslin`"));
}

#[test]
fn environment_errors() {
    let env = Env::new("envs");
    let mut c = Command::cargo_bin("rata").unwrap();
    c.env_remove("RATA").arg("start");
    c.assert().code(2).stderr(predicate::str::contains(
        "the RATA environment variable is not set",
    ));

    let mut c = Command::cargo_bin("rata").unwrap();
    c.env("RATA", "relative/dir").arg("status");
    c.assert()
        .code(2)
        .stderr(predicate::str::contains("must be an absolute path"));

    let missing = env.root().join("nope");
    let mut c = Command::cargo_bin("rata").unwrap();
    c.env("RATA", &missing).arg("status");
    c.assert()
        .code(2)
        .stderr(predicate::str::contains("does not exist"));

    env.rata(&["status"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("config file not found"));
}

#[test]
fn lock_contention() {
    let env = Env::new("lock");
    env.two_services();
    let rata_root = rata::Rata::from_path(env.root()).unwrap();
    let _held = rata::lock::Lock::acquire(&rata_root, std::time::Duration::from_secs(1)).unwrap();
    env.rata(&["start"])
        .env("RATA_LOCK_TIMEOUT", "1")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("another rata command is running"));
    // status does not take the lock.
    env.rata(&["status"]).assert().code(3);
}

#[test]
fn help_and_usage() {
    let env = Env::new("help");
    for args in [&["help"][..], &["--help"], &["-h"]] {
        env.rata(args).assert().code(0).stdout(
            predicate::str::contains("Commands:").and(predicate::str::contains("Exit codes:")),
        );
    }
    env.rata(&["help", "stop"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(
            "Usage: rata stop [OPTIONS] [NAME]...",
        ));
    env.rata(&["help", "nope"]).assert().code(2);
    env.rata(&[])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no command given"));
    env.rata(&["--version"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn names_ignore_case_on_the_command_line() {
    let env = Env::new("case");
    env.two_services();
    env.rata(&["start", "web"])
        .assert()
        .code(0)
        .stdout(predicate::str::is_match(r"^WEB\s+started").unwrap());
    env.rata(&["status", "Web"]).assert().code(0);
    env.rata(&["stop", "wEb"])
        .assert()
        .code(0)
        .stdout(predicate::str::is_match(r"^WEB\s+stopped").unwrap());
}

#[test]
fn explicit_start_and_stop() {
    let env = Env::new("explicit");
    env.write_config(&format!(
        "[[service]]\nname = \"MON\"\nstart = {}\nexplicit_stop = true\n\n\
         [[service]]\nname = \"BATCH\"\nstart = {}\nexplicit_start = true\n",
        env.svc("MON", &[]),
        env.svc("BATCH", &[])
    ));
    env.rata(&["start"])
        .assert()
        .code(0)
        .stdout(
            predicate::str::contains("MON    started").and(predicate::str::contains(
                "BATCH  skipped           explicit_start; start it with: rata start BATCH",
            )),
        );
    assert!(env.pids("BATCH").is_empty());

    // Bare stop and restart leave MON running.
    let mon = env.pids("MON");
    env.rata(&["stop"]).assert().code(0).stdout(
        predicate::str::contains("BATCH  not running").and(predicate::str::contains(
            "MON    skipped           explicit_stop; stop it with: rata stop MON",
        )),
    );
    env.rata(&["restart"]).assert().code(0).stdout(
        predicate::str::contains("MON    skipped").and(predicate::str::contains("BATCH  skipped")),
    );
    assert_eq!(env.pids("MON"), mon);

    // -q hides skipped lines.
    env.rata(&["-q", "stop"])
        .assert()
        .code(0)
        .stdout(predicate::str::is_empty());

    // Naming a service, or --all, includes it.
    env.rata(&["start", "batch"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("BATCH  started"));
    env.rata(&["stop", "--all"]).assert().code(0).stdout(
        predicate::str::contains("MON    stopped").and(predicate::str::contains("BATCH  stopped")),
    );
    env.rata(&["start", "--all"]).assert().code(0);
    assert_eq!(env.pids("BATCH").len(), 1);
    env.rata(&["stop", "--all", "MON"]).assert().code(2);
    env.rata(&["stop", "MON", "BATCH"]).assert().code(0);

    env.rata(&["check"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(
            "explicit   stop (only when named or with --all)",
        ));
}

#[test]
fn init_sets_up_an_empty_directory() {
    let env = Env::new("init");
    let root = env.root().join("fresh");

    // DIR argument; RATA unset.
    let mut c = Command::cargo_bin("rata").unwrap();
    c.env_remove("RATA").arg("init").arg(&root);
    c.assert().code(0).stdout(
        predicate::str::contains("Created")
            .and(predicate::str::contains("service.toml"))
            .and(predicate::str::contains("monitor.toml"))
            .and(predicate::str::contains("archive.toml"))
            .and(predicate::str::contains("Next steps:")),
    );
    let svc = std::fs::read_to_string(root.join("work/cfg/service.toml")).unwrap();
    assert!(svc.contains("name          = \"RATA_MONITOR\""));
    assert!(root.join("work/monitor").is_dir());

    // The new install is valid.
    let mut c = Command::cargo_bin("rata").unwrap();
    c.env("RATA", &root).arg("check");
    c.assert()
        .code(0)
        .stdout(predicate::str::contains("RATA_MONITOR  rata Monitor"));

    // Not again: the directory is no longer empty.
    let mut c = Command::cargo_bin("rata").unwrap();
    c.env("RATA", &root).arg("init");
    c.assert()
        .code(2)
        .stderr(predicate::str::contains("is not empty (found work").and(
            predicate::str::contains("rata init only sets up a new, empty directory"),
        ));

    // $RATA is used without DIR; hidden files do not count.
    let hidden = env.root().join("hidden");
    std::fs::create_dir(&hidden).unwrap();
    std::fs::write(hidden.join(".DS_Store"), "").unwrap();
    let mut c = Command::cargo_bin("rata").unwrap();
    c.env("RATA", &hidden).arg("init");
    c.assert().code(0);
    assert!(hidden.join("work/cfg/monitor.toml").is_file());

    // No DIR and no RATA.
    let mut c = Command::cargo_bin("rata").unwrap();
    c.env_remove("RATA").arg("init");
    c.assert()
        .code(2)
        .stderr(predicate::str::contains("rata init needs a directory"));

    // Relative DIR.
    let mut c = Command::cargo_bin("rata").unwrap();
    c.arg("init").arg("relative/dir");
    c.assert()
        .code(2)
        .stderr(predicate::str::contains("DIR must be an absolute path"));
}

/// Path to the built `rata-monitor` binary.
fn rata_monitor() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rata-monitor"))
}

/// Read `work/monitor/<category>_*.csv` as lines (header first).
fn monitor_lines(env: &Env, category: &str) -> Vec<String> {
    let dir = env.root().join("work/monitor");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|d| d.map(|e| e.unwrap().path()).collect())
        .unwrap_or_default();
    files.retain(|p| {
        p.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(&format!("{category}_"))
    });
    files.sort();
    files
        .last()
        .map(|p| {
            std::fs::read_to_string(p)
                .unwrap()
                .lines()
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Wait until `work/monitor/service_*.csv` has at least `n` data rows.
fn wait_for_rows(env: &Env, n: usize) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while monitor_lines(env, "service").len() < n + 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "rata-monitor wrote no samples; log: {:?}",
            std::fs::read_dir(env.root().join("work/log"))
                .unwrap()
                .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

impl Env {
    /// A config with RATA_MONITOR (5 second samples) and two test services.
    ///
    /// The monitor runs from a copy in `$RATA/bin`, so its command line is
    /// unique to this test.
    fn monitored(&self, system: bool) {
        let bin = self.root().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(format!("rata-monitor{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(rata_monitor(), &exe).unwrap();
        self.write_config(&format!(
            "[[service]]\nname = \"RATA_MONITOR\"\nstart = ['{}']\nexplicit_stop = true\n\n\
             [[service]]\nname = \"WEB\"\nstart = {}\n\n\
             [[service]]\nname = \"IDLE\"\nstart = {}\nexplicit_start = true\n",
            exe.display(),
            self.svc("WEB", &[]),
            self.svc("IDLE", &[]),
        ));
        std::fs::write(
            self.root().join("work/cfg/monitor.toml"),
            format!("interval = 5\nsystem = {system}\n"),
        )
        .unwrap();
    }
}

#[test]
fn monitor_records_samples() {
    let env = Env::new("monitor");
    env.monitored(true);
    env.rata(&["check"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(
            "monitor.toml OK (every 5s, host stats on)",
        ));
    env.rata(&["start"]).assert().code(0);
    wait_for_rows(&env, 3);

    let service = monitor_lines(&env, "service");
    assert_eq!(
        service[0],
        "time,service,state,pid,procs,cpu_pct,rss,disk_read,disk_written,uptime_s"
    );
    let rows: Vec<Vec<&str>> = service[1..]
        .iter()
        .map(|l| l.split(',').collect())
        .collect();
    let row = |name: &str| rows.iter().find(|r| r[1] == name).unwrap().clone();
    assert_eq!(row("RATA_MONITOR")[2], "running");
    assert_eq!(row("WEB")[2], "running");
    assert_eq!(row("WEB")[3], env.pids("WEB")[0].to_string());
    assert!(row("WEB")[6].parse::<u64>().unwrap() > 0, "rss");
    // The first sample covers less than an interval: no I/O amounts.
    assert_eq!(row("WEB")[7..9], ["", ""]);
    assert_eq!(row("IDLE")[2..], ["stopped", "", "", "", "", "", "", ""]);
    // Times are in the rata datetime format.
    let time: rata::datetime::RataTime = row("WEB")[0].parse().unwrap();
    let age = jiff::Timestamp::now().duration_since(time.timestamp);
    assert!(age.as_secs() < 30, "{age:?}");

    let net = monitor_lines(&env, "net");
    if !net.is_empty() {
        assert!(net[0].ends_with(",rx_errors,tx_errors,rx_dropped,tx_dropped"));
    }
    let system = monitor_lines(&env, "system");
    assert!(system[0].starts_with("time,cpu_pct,load1,load5,load15,cores,mem_total"));
    assert!(system.len() >= 2);

    // A graceful stop: the monitor logs that it is stopping.
    env.rata(&["stop", "RATA_MONITOR"]).assert().code(0).stdout(
        predicate::str::contains("RATA_MONITOR  stopped")
            .and(predicate::str::contains("forced").not()),
    );
    let log = std::fs::read_dir(env.root().join("work/log"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().contains("RATA_MONITOR_"))
        .unwrap();
    let log = std::fs::read_to_string(log).unwrap();
    assert!(log.contains("rata-monitor stopping"), "{log}");
    env.rata(&["stop"]).assert().code(0);
}

#[test]
fn monitor_without_host_stats() {
    let env = Env::new("monsvc");
    env.monitored(false);
    env.rata(&["start", "RATA_MONITOR"]).assert().code(0);
    wait_for_rows(&env, 3);
    for category in ["system", "disk", "net"] {
        assert!(monitor_lines(&env, category).is_empty(), "{category}");
    }
    env.rata(&["stop", "--all"]).assert().code(0);
}

#[test]
fn monitor_rejects_arguments_and_bad_config() {
    let env = Env::new("monargs");
    let mut c = Command::new(rata_monitor());
    c.env("RATA", env.root()).arg("--bogus");
    c.assert()
        .code(2)
        .stderr(predicate::str::contains("unexpected argument"));
    let mut c = Command::new(rata_monitor());
    c.arg("--version");
    c.assert()
        .code(0)
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
    std::fs::write(env.root().join("work/cfg/monitor.toml"), "interval = 1\n").unwrap();
    let mut c = Command::new(rata_monitor());
    c.env("RATA", env.root()).timeout(Duration::from_secs(10));
    c.assert().code(2).stderr(predicate::str::contains(
        "interval must be between 5 and 3600",
    ));
    env.write_config("");
    env.rata(&["check"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("monitor.toml:1:12"));
}

#[test]
fn mon_shows_latest_sample() {
    let env = Env::new("mon");
    env.monitored(true);
    env.rata(&["mon"]).assert().code(3).stderr(predicate::str::contains(
        "no samples recorded yet; RATA_MONITOR is not running (start it with: rata start RATA_MONITOR)",
    ));
    env.rata(&["start"]).assert().code(0);
    wait_for_rows(&env, 3);

    // IDLE (explicit_start) is not running, so the exit status is 3.
    env.rata(&["mon"]).assert().code(3).stdout(
        predicate::str::is_match(r"^Sampled \d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d{9} [+-]\d{4}")
            .unwrap()
            .and(predicate::str::contains("\nHOST "))
            .and(predicate::str::contains("\nMEMORY "))
            .and(predicate::str::is_match(r"\nRATA_MONITOR +running +\d+").unwrap())
            .and(predicate::str::is_match(r"\nWEB +running +\d+ +1 ").unwrap())
            .and(predicate::str::is_match(r"\nIDLE +stopped +- ").unwrap()),
    );
    // Names: only those services, no host stats; any case.
    env.rata(&["mon", "web"]).assert().code(0).stdout(
        predicate::str::contains("WEB ")
            .and(predicate::str::contains("HOST").not())
            .and(predicate::str::contains("IDLE").not()),
    );
    env.rata(&["mon", "nope"]).assert().code(2);
    env.rata(&["-q", "mon", "WEB"])
        .assert()
        .code(0)
        .stdout(predicate::str::is_empty());

    let out = env.rata(&["mon", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["version"], 1);
    assert_eq!(v["fresh"], true);
    assert_eq!(v["monitor"], "running");
    assert_eq!(v["interval_secs"], 5);
    assert!(v["host"]["mem_total"].as_u64().unwrap() > 0);
    let t: rata::datetime::RataTime = v["time"].as_str().unwrap().parse().unwrap();
    assert!(t.timestamp <= jiff::Timestamp::now());
    let svcs = v["services"].as_array().unwrap();
    assert_eq!(svcs.len(), 3);
    assert_eq!(svcs[1]["name"], "WEB");
    assert_eq!(svcs[1]["state"], "running");
    assert!(svcs[1]["rss"].as_u64().unwrap() > 0);
    assert_eq!(svcs[2]["state"], "stopped");
    assert!(svcs[2]["pid"].is_null());

    // --watch redraws until killed.
    let mut child = StdCommand::new(env!("CARGO_BIN_EXE_rata"))
        .env("RATA", env.root())
        .args(["mon", "--watch", "WEB"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(2500));
    child.kill().unwrap();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.matches("\x1b[H").count() >= 2, "{text:?}");
    assert!(text.contains("WEB "), "{text:?}");
    env.rata(&["mon", "--watch", "--json"]).assert().code(2);
    env.rata(&["-q", "mon", "--watch"]).assert().code(2);

    // Once the monitor stops, its data is out of date.
    env.rata(&["stop", "RATA_MONITOR"]).assert().code(0);
    env.rata(&["mon", "WEB"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "RATA_MONITOR is not running (start it with: rata start RATA_MONITOR)",
        ));
}

/// Set a file's last-modified time to `days` days ago.
fn age_file(path: &Path, days: u64) {
    let t = SystemTime::now() - Duration::from_secs(days * 86_400);
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(t))
        .unwrap();
}

#[test]
fn archive_compresses_and_deletes_but_not_live_logs() {
    let env = Env::new("archive");
    let log = env.root().join("work/log");

    // Without archive.toml, nothing is done.
    env.two_services();
    env.rata(&["archive"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(
            "archive.toml not found; nothing to archive",
        ));

    std::fs::write(
        env.root().join("work/cfg/archive.toml"),
        rata::init::ARCHIVE_TEMPLATE,
    )
    .unwrap();
    env.rata(&["check"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(
            "compress after 3d, delete after 90d",
        ));

    // The running service's log is old but still being written.
    env.rata(&["start", "WEB"]).assert().code(0);
    let v = env.status_json();
    let live = PathBuf::from(v["services"][0]["log"].as_str().unwrap());
    age_file(&live, 100);

    let old = log.join("WORKER_20260801_000000.0.log");
    let mid = log.join("WORKER_20261101_000000.0.log");
    std::fs::write(&old, "old").unwrap();
    std::fs::write(&mid, "mid").unwrap();
    age_file(&old, 100);
    age_file(&mid, 10);

    env.rata(&["archive", "--dry-run"]).assert().code(0).stdout(
        predicate::str::contains("would delete    work").and(predicate::str::contains(
            "dry run: 1 to compress, 1 to delete",
        )),
    );
    assert!(old.exists() && mid.exists());

    env.rata(&["archive"]).assert().code(0).stdout(
        predicate::str::contains("compressed  work")
            .and(predicate::str::contains("WORKER_20261101_000000.0.log.gz"))
            .and(predicate::str::contains("1 compressed, 1 deleted")),
    );
    assert!(!old.exists() && !mid.exists());
    assert!(log.join("WORKER_20261101_000000.0.log.gz").is_file());
    assert!(live.is_file());

    env.rata(&["-q", "archive"])
        .assert()
        .code(0)
        .stdout(predicate::str::is_empty());

    // A file that cannot be moved makes the run fail with exit 1.
    std::fs::write(
        env.root().join("work/cfg/archive.toml"),
        "[[rule]]\npath = \"work/log\"\nmatch = [\"WORKER_*.log\"]\nmove_after = \"1d\"\nmove_to = \"archive\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(env.root().join("archive")).unwrap();
    std::fs::write(
        env.root().join("archive/WORKER_20261101_000000.0.log.gz"),
        "other",
    )
    .unwrap();
    env.rata(&["archive"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("FAILED"));
    assert!(log.join("WORKER_20261101_000000.0.log.gz").is_file());
}

#[test]
fn archive_config_errors_lock_and_help() {
    let env = Env::new("archive-errors");
    env.two_services();
    let cfg = env.root().join("work/cfg/archive.toml");

    // An invalid archive.toml stops both check and archive with exit 2.
    std::fs::write(
        &cfg,
        "[[rule]]\npath = \"work/log\"\nmatch = [\"*.log\"]\nmove_after = \"30d\"\nmove_to = \"a\"\ndelete_after = \"90d\"\n",
    )
    .unwrap();
    for cmd in ["check", "archive"] {
        env.rata(&[cmd]).assert().code(2).stderr(
            predicate::str::contains("archive.toml:6:16").and(predicate::str::contains(
                "can't set both move_after and delete_after",
            )),
        );
    }
    std::fs::write(
        &cfg,
        "[[rule]]\npath = \"work/log\"\nmatch = [\"*.{log\"]\ndelete_after = \"3m\"\n",
    )
    .unwrap();
    env.rata(&["check"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unclosed {"));

    // check lists the rules, with exclude patterns and spelled-out ages.
    std::fs::write(
        &cfg,
        "[[rule]]\npath = \"work/log\"\nmatch = [\"*.log\"]\nexclude = [\"RATA_*\"]\ncompress_after = \"1 week\"\nmove_after = \"6 months\"\nmove_to = \"${RATA}/archive/{yyyy}\"\n",
    )
    .unwrap();
    env.rata(&["check"]).assert().code(0).stdout(
        predicate::str::contains("archive work/cfg/archive.toml OK (1 rule)").and(
            predicate::str::contains(
                "*.log except RATA_*  compress after 1w, move after 6mo to ${RATA}/archive/{yyyy}",
            ),
        ),
    );

    // A second run while one holds the lock exits 2 at once; a dry run
    // does not need the lock.
    let rata = rata::Rata::from_path(env.root()).unwrap();
    let held = rata::archive::lock(&rata).unwrap();
    env.rata(&["archive"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("another rata archive is running"));
    env.rata(&["archive", "-n"]).assert().code(0);
    // Service commands use a different lock.
    env.rata(&["status"]).assert().code(3);
    drop(held);
    env.rata(&["archive"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("nothing to archive"));

    // Help.
    env.rata(&["help"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("archive  ").and(predicate::str::contains("Archiving:")));
    env.rata(&["help", "archive"]).assert().code(0).stdout(
        predicate::str::contains("--dry-run").and(predicate::str::contains("rata archive -q")),
    );
}
