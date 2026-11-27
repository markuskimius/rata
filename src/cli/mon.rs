//! `rata mon`: show the newest sample recorded by the rata monitor.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::config::{Config, Service};
use crate::env::Rata;
use crate::error::Error;
use crate::monitor::sample::{DiskRow, NetRow, ServiceRow, SystemRow};
use crate::monitor::{self, MonitorConfig, Sample};
use crate::service::{self, State};

use super::{EXIT_ERROR, EXIT_NOT_RUNNING, EXIT_OK};

/// How often `--watch` redraws.
const WATCH_EVERY: Duration = Duration::from_secs(1);

/// Options for `rata mon`.
pub(super) struct Options {
    pub names: Vec<String>,
    pub json: bool,
    pub watch: bool,
    pub quiet: bool,
}

/// Whether the service that runs rata-monitor is running.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MonitorState {
    Running(String),
    Stopped(String),
    NotConfigured,
}

impl MonitorState {
    fn as_str(&self) -> &'static str {
        match self {
            MonitorState::Running(_) => "running",
            MonitorState::Stopped(_) => "stopped",
            MonitorState::NotConfigured => "not configured",
        }
    }
}

/// Everything one screen of `rata mon` shows.
struct View<'a> {
    sample: Option<Sample>,
    interval: Duration,
    age: Option<Duration>,
    fresh: bool,
    monitor: MonitorState,
    /// Host sections are shown (no names given, and host stats recorded).
    host: bool,
    services: Vec<(&'a Service, Option<ServiceRow>)>,
}

impl View<'_> {
    fn exit_code(&self) -> u8 {
        let all_running = self
            .services
            .iter()
            .all(|(_, r)| r.as_ref().is_some_and(|r| r.state == State::Running));
        if self.fresh && all_running {
            EXIT_OK
        } else {
            EXIT_NOT_RUNNING
        }
    }

    /// Why the data is missing or out of date, if it is.
    fn warning(&self) -> Option<String> {
        if self.fresh {
            return None;
        }
        let why = match &self.monitor {
            MonitorState::Running(name) => {
                format!("{name} is running but has not recorded a sample since; see its log")
            }
            MonitorState::Stopped(name) => {
                format!("{name} is not running (start it with: rata start {name})")
            }
            MonitorState::NotConfigured => {
                "no service in service.toml runs rata-monitor (see 'rata help mon')".to_string()
            }
        };
        Some(match self.age {
            None => match &self.monitor {
                MonitorState::Running(name) => format!(
                    "no samples recorded yet; {name} takes its first one within {}s of starting",
                    (monitor::FIRST_SAMPLE_DELAY + self.interval).as_secs()
                ),
                _ => format!("no samples recorded yet; {why}"),
            },
            Some(age) => format!("latest sample is {} ago; {why}", duration(age.as_secs())),
        })
    }
}

/// Run `rata mon` and return its exit code.
pub(super) fn run(opts: &Options) -> Result<u8, Error> {
    let rata = Rata::from_env()?;
    if opts.watch && opts.quiet {
        eprintln!("error: --watch cannot be used with --quiet");
        return Ok(EXIT_ERROR);
    }
    if !opts.watch {
        let config = Config::load(&rata)?;
        let services = config.select(&opts.names)?;
        let mcfg = MonitorConfig::load(&rata)?;
        let view = build(&rata, &config, &services, &mcfg, opts.names.is_empty());
        if opts.quiet {
            // Exit status only.
        } else if opts.json {
            let doc = to_json(&view);
            let _ = writeln!(
                io::stdout().lock(),
                "{}",
                serde_json::to_string_pretty(&doc).expect("JSON is serializable")
            );
        } else {
            if let Some(w) = view.warning() {
                eprintln!("warning: {w}");
            }
            let _ = io::stdout().lock().write_all(render(&view).as_bytes());
        }
        return Ok(view.exit_code());
    }

    enable_ansi();
    loop {
        // Re-read everything each time, so config changes show up too.
        let screen = match Config::load(&rata).and_then(|config| {
            let services = config.select(&opts.names)?;
            let mcfg = MonitorConfig::load(&rata)?;
            let view = build(&rata, &config, &services, &mcfg, opts.names.is_empty());
            let mut s = String::new();
            if let Some(w) = view.warning() {
                let _ = writeln!(s, "warning: {w}\n");
            }
            s.push_str(&render(&view));
            Ok(s)
        }) {
            Ok(s) => s,
            Err(e) => format!("error: {e}\n"),
        };
        let mut out = io::stdout().lock();
        // Home the cursor, draw, and clear what is left of the old screen.
        let screen = screen.replace('\n', "\x1b[K\n");
        if out
            .write_all(format!("\x1b[H{screen}\x1b[J").as_bytes())
            .and_then(|_| out.flush())
            .is_err()
        {
            return Ok(EXIT_OK);
        }
        drop(out);
        std::thread::sleep(WATCH_EVERY);
    }
}

fn build<'a>(
    rata: &Rata,
    config: &'a Config,
    services: &[&'a Service],
    mcfg: &MonitorConfig,
    all: bool,
) -> View<'a> {
    let sample = monitor::latest(rata);
    let age = sample.as_ref().map(|s| {
        let d = jiff::Timestamp::now().duration_since(s.time.timestamp);
        Duration::from_secs(d.as_secs().max(0) as u64)
    });
    let monitor = monitor_state(rata, config);
    // Recent data from a monitor that has stopped will not be updated.
    let fresh =
        age.is_some_and(|a| a <= mcfg.interval * 2) && !matches!(monitor, MonitorState::Stopped(_));
    let rows = |name: &str| {
        sample
            .as_ref()
            .and_then(|s| s.services.iter().find(|r| r.service == name).cloned())
    };
    View {
        host: all && sample.as_ref().is_some_and(|s| s.system.is_some()),
        services: services.iter().map(|s| (*s, rows(&s.name))).collect(),
        monitor,
        sample,
        interval: mcfg.interval,
        age,
        fresh,
    }
}

/// Find the service that runs rata-monitor and whether it is running.
fn monitor_state(rata: &Rata, config: &Config) -> MonitorState {
    let is_monitor = |svc: &&Service| {
        let cmd = svc.start_command(rata);
        Path::new(cmd.program())
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| {
                if cfg!(windows) {
                    s.eq_ignore_ascii_case(monitor::PROGRAM)
                } else {
                    s == monitor::PROGRAM
                }
            })
    };
    let Some(svc) = config.services.iter().find(is_monitor) else {
        return MonitorState::NotConfigured;
    };
    match service::status(rata, &[svc]).first().map(|r| r.state) {
        Some(State::Running) => MonitorState::Running(svc.name.clone()),
        _ => MonitorState::Stopped(svc.name.clone()),
    }
}

// ---------------------------------------------------------------- text

fn render(v: &View<'_>) -> String {
    let mut s = String::new();
    let Some(sample) = &v.sample else {
        return s;
    };
    let interval = v.interval.as_secs();
    let age = v.age.map(|a| duration(a.as_secs())).unwrap_or_default();
    let _ = writeln!(s, "Sampled {}   {age} ago, every {interval}s", sample.time);

    if v.host
        && let Some(sys) = &sample.system
    {
        host(&mut s, sys);
        if !sample.disks.is_empty() {
            disks(&mut s, &sample.disks, interval);
        }
        if !sample.net.is_empty() {
            net(&mut s, &sample.net, interval);
        }
    }
    if !v.services.is_empty() {
        services(&mut s, &v.services);
    }
    s
}

fn host(s: &mut String, sys: &SystemRow) {
    let parts: Vec<String> = [
        sysinfo::System::host_name(),
        sysinfo::System::long_os_version(),
        Some(format!("up {}", duration(sys.uptime_s))),
    ]
    .into_iter()
    .flatten()
    .collect();
    let _ = writeln!(s);
    let _ = writeln!(s, "HOST     {}", parts.join("   "));
    let load = sys
        .load
        .map(|l| format!("   load {:.2} {:.2} {:.2}", l[0], l[1], l[2]))
        .unwrap_or_default();
    let _ = writeln!(
        s,
        "CPU      {:.1}%{load}   {} cores",
        sys.cpu_pct, sys.cores
    );
    let pct = if sys.mem_total > 0 {
        sys.mem_used as f64 * 100.0 / sys.mem_total as f64
    } else {
        0.0
    };
    let _ = writeln!(
        s,
        "MEMORY   {} of {} ({pct:.0}%)   swap {} of {}",
        bytes(sys.mem_used),
        bytes(sys.mem_total),
        bytes(sys.swap_used),
        bytes(sys.swap_total)
    );
}

fn disks(s: &mut String, rows: &[DiskRow], interval: u64) {
    let mut t = Table::new(&[
        ("DISK", false),
        ("SIZE", true),
        ("USED", true),
        ("FREE", true),
        ("USE%", true),
        ("READ/S", true),
        ("WRITE/S", true),
    ]);
    for d in rows {
        let pct = if d.total > 0 {
            format!("{:.0}%", d.used as f64 * 100.0 / d.total as f64)
        } else {
            "-".into()
        };
        t.row(vec![
            d.mount.clone(),
            bytes(d.total),
            bytes(d.used),
            bytes(d.avail),
            pct,
            rate(d.read, interval),
            rate(d.written, interval),
        ]);
    }
    let _ = writeln!(s);
    s.push_str(&t.render());
}

fn net(s: &mut String, rows: &[NetRow], interval: u64) {
    let mut t = Table::new(&[
        ("NETWORK", false),
        ("RX/S", true),
        ("TX/S", true),
        ("ERRORS", true),
    ]);
    for n in rows {
        let errors = match (n.rx_errors, n.tx_errors) {
            (None, None) => "-".into(),
            (a, b) => (a.unwrap_or(0) + b.unwrap_or(0)).to_string(),
        };
        t.row(vec![
            n.iface.clone(),
            rate(n.rx_bytes, interval),
            rate(n.tx_bytes, interval),
            errors,
        ]);
    }
    let _ = writeln!(s);
    s.push_str(&t.render());
}

fn services(s: &mut String, rows: &[(&Service, Option<ServiceRow>)]) {
    let mut t = Table::new(&[
        ("SERVICE", false),
        ("STATE", false),
        ("PID", true),
        ("PROCS", true),
        ("CPU", true),
        ("RSS", true),
        ("UPTIME", true),
    ]);
    let dash = || "-".to_string();
    for (svc, row) in rows {
        let Some(r) = row else {
            t.row(vec![
                svc.name.clone(),
                "no data".into(),
                dash(),
                dash(),
                dash(),
                dash(),
                dash(),
            ]);
            continue;
        };
        t.row(vec![
            svc.name.clone(),
            r.state.as_str().into(),
            r.pid.map_or_else(dash, |p| p.to_string()),
            r.procs.map_or_else(dash, |p| p.to_string()),
            r.cpu_pct.map_or_else(dash, |c| format!("{c:.1}%")),
            r.rss.map_or_else(dash, bytes),
            r.uptime_s.map_or_else(dash, duration),
        ]);
    }
    let _ = writeln!(s);
    s.push_str(&t.render());
}

/// A text table with left- or right-aligned columns.
struct Table {
    head: Vec<(String, bool)>,
    rows: Vec<Vec<String>>,
}

impl Table {
    fn new(head: &[(&str, bool)]) -> Self {
        Table {
            head: head.iter().map(|(h, r)| (h.to_string(), *r)).collect(),
            rows: Vec::new(),
        }
    }

    fn row(&mut self, cells: Vec<String>) {
        self.rows.push(cells);
    }

    fn render(&self) -> String {
        let widths: Vec<usize> = (0..self.head.len())
            .map(|i| {
                self.rows
                    .iter()
                    .map(|r| r[i].chars().count())
                    .chain([self.head[i].0.len()])
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let line = |cells: Vec<&str>| {
            let mut l = String::new();
            for (i, c) in cells.iter().enumerate() {
                let w = widths[i];
                if i > 0 {
                    l.push_str("  ");
                }
                if self.head[i].1 {
                    let _ = write!(l, "{c:>w$}");
                } else {
                    let _ = write!(l, "{c:<w$}");
                }
            }
            format!("{}\n", l.trim_end())
        };
        let mut out = line(self.head.iter().map(|(h, _)| h.as_str()).collect());
        for r in &self.rows {
            out.push_str(&line(r.iter().map(String::as_str).collect()));
        }
        out
    }
}

/// `0 B`, `340 KiB`, `1.2 MiB`, `9.0 GiB`: one decimal below 10.
fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", UNITS[unit])
    } else {
        format!("{v:.0} {}", UNITS[unit])
    }
}

/// Bytes during an interval, as a per-second rate.
fn rate(n: Option<u64>, interval: u64) -> String {
    match n {
        Some(n) => bytes(n / interval.max(1)),
        None => "-".into(),
    }
}

/// `42s`, `5m 03s`, `2h 01m`, `6d 03:12`.
fn duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        format!("{d}d {h:02}:{m:02}")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

// ---------------------------------------------------------------- JSON

fn to_json(v: &View<'_>) -> Value {
    let sample = v.sample.as_ref();
    let host = if v.host {
        sample.and_then(|s| s.system.as_ref()).map(|sys| {
            json!({
                "name": sysinfo::System::host_name(),
                "os": sysinfo::System::long_os_version(),
                "uptime_secs": sys.uptime_s,
                "cpu_pct": sys.cpu_pct,
                "load": sys.load,
                "cores": sys.cores,
                "mem_total": sys.mem_total,
                "mem_used": sys.mem_used,
                "mem_avail": sys.mem_avail,
                "swap_total": sys.swap_total,
                "swap_used": sys.swap_used,
            })
        })
    } else {
        None
    };
    let disks: Vec<Value> = if v.host {
        sample
            .map(|s| &s.disks[..])
            .unwrap_or_default()
            .iter()
            .map(|d| {
                json!({
                    "mount": d.mount, "fs": d.fs, "total": d.total, "used": d.used,
                    "avail": d.avail, "read": d.read, "written": d.written,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let net: Vec<Value> = if v.host {
        sample
            .map(|s| &s.net[..])
            .unwrap_or_default()
            .iter()
            .map(|n| {
                json!({
                    "iface": n.iface, "rx_bytes": n.rx_bytes, "tx_bytes": n.tx_bytes,
                    "rx_packets": n.rx_packets, "tx_packets": n.tx_packets,
                    "rx_errors": n.rx_errors, "tx_errors": n.tx_errors,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let services: Vec<Value> = v
        .services
        .iter()
        .map(|(svc, r)| match r {
            Some(r) => json!({
                "name": svc.name, "state": r.state, "pid": r.pid, "procs": r.procs,
                "cpu_pct": r.cpu_pct, "rss": r.rss, "disk_read": r.disk_read,
                "disk_written": r.disk_written, "uptime_secs": r.uptime_s,
            }),
            None => json!({
                "name": svc.name, "state": null, "pid": null, "procs": null,
                "cpu_pct": null, "rss": null, "disk_read": null,
                "disk_written": null, "uptime_secs": null,
            }),
        })
        .collect();
    json!({
        "version": 1,
        "time": sample.map(|s| s.time),
        "age_secs": v.age.map(|a| a.as_secs()),
        "interval_secs": v.interval.as_secs(),
        "fresh": v.fresh,
        "monitor": v.monitor.as_str(),
        "host": host,
        "disks": disks,
        "net": net,
        "services": services,
    })
}

// ---------------------------------------------------------------- terminal

/// Let `--watch`'s cursor movement work in the Windows console.
#[cfg(windows)]
fn enable_ansi() {
    use windows_sys::Win32::System::Console::{
        CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle,
        STD_OUTPUT_HANDLE, SetConsoleMode,
    };
    // SAFETY: console mode calls on our own stdout handle.
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode: CONSOLE_MODE = 0;
        if GetConsoleMode(h, &mut mode) != 0 {
            SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(windows))]
fn enable_ansi() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(340 * 1024), "340 KiB");
        assert_eq!(bytes(1258291), "1.2 MiB");
        assert_eq!(bytes(17179869184), "16 GiB");
        assert_eq!(bytes(9663676416), "9.0 GiB");
        assert_eq!(rate(Some(30 * 2048), 30), "2.0 KiB");
        assert_eq!(rate(None, 30), "-");
        assert_eq!(duration(42), "42s");
        assert_eq!(duration(303), "5m 03s");
        assert_eq!(duration(7260), "2h 01m");
        assert_eq!(duration(529920), "6d 03:12");
    }

    #[test]
    fn table_alignment() {
        let mut t = Table::new(&[("NAME", false), ("SIZE", true)]);
        t.row(vec!["/".into(), "494 GiB".into()]);
        t.row(vec!["/Volumes/Data".into(), "1.8 TiB".into()]);
        assert_eq!(
            t.render(),
            "NAME              SIZE\n/              494 GiB\n/Volumes/Data  1.8 TiB\n"
        );
    }
}
