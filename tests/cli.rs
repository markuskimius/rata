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
        // Kill anything this test left running.
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
