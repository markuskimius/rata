//! The rata monitor: host and per-service stats recorded as CSV.
//!
//! `rata-monitor` (run as the `RATA_MONITOR` service) takes a sample every
//! `interval` seconds (default 30), on the clock, and appends it to daily
//! CSV files in `$RATA/work/monitor`:
//!
//! | File | One row per |
//! |---|---|
//! | `system_YYYYMMDD.csv` | sample: CPU, load, memory, swap, uptime |
//! | `disk_YYYYMMDD.csv` | mounted filesystem: space, bytes read and written |
//! | `net_YYYYMMDD.csv` | network interface: bytes, packets, errors, drops |
//! | `service_YYYYMMDD.csv` | configured service: state, CPU, memory, I/O, uptime |
//!
//! See [`sample`] for the columns. Times use the
//! [rata datetime format](crate::datetime). `rata mon` shows the newest
//! sample ([`store::latest`]).
//!
//! The monitor never deletes files. Only today's files are written
//! ([`store::live_files`]); older files never change.

pub mod collect;
pub mod config;
pub mod csv;
pub mod netdrops;
pub mod sample;
pub mod signal;
pub mod store;

use std::io::Write;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use config::MonitorConfig;
pub use sample::Sample;

use crate::config::Config;
use crate::env::Rata;
use crate::error::{Error, Result};

/// The least time between startup and the first sample.
pub const FIRST_SAMPLE_DELAY: Duration = Duration::from_secs(5);

/// The name of the monitor's program, as written in `service.toml`.
pub const PROGRAM: &str = "rata-monitor";

/// The monitor files in `$RATA/work/monitor` that may still change.
pub fn live_files(rata: &Rata) -> Vec<std::path::PathBuf> {
    store::live_files(&rata.monitor_dir())
}

/// The newest sample recorded in `$RATA/work/monitor`, if any.
pub fn latest(rata: &Rata) -> Option<Sample> {
    store::latest(&rata.monitor_dir())
}

/// The next time after `now` that is a whole multiple of `interval` since
/// the Unix epoch.
pub fn next_boundary(now: SystemTime, interval: Duration) -> SystemTime {
    let since = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let step = interval.as_nanos().max(1);
    let next = (since.as_nanos() / step + 1) * step;
    UNIX_EPOCH + Duration::from_nanos(next as u64)
}

/// Run the monitor until a stop is requested. Messages go to stderr, which
/// is the service's log file when rata starts it.
pub fn run(rata: &Rata) -> Result<()> {
    let cfg = MonitorConfig::load(rata)?;
    let dir = rata.monitor_dir();
    let mut writer = store::Writer::new(&dir)
        .map_err(|e| Error::io(format!("cannot create {}", dir.display()), e))?;
    signal::install();

    log(&format!(
        "rata-monitor {} started: every {}s, host stats {}, writing to {}",
        env!("CARGO_PKG_VERSION"),
        cfg.interval.as_secs(),
        if cfg.system { "on" } else { "off" },
        rata.display_path(&dir)
    ));

    let mut services = Services::new();
    services.reload(rata);
    let mut collector = collect::Collector::new();
    // The first sample comes at the first boundary at least FIRST_SAMPLE_DELAY
    // after startup, so its CPU figures are measured over a few seconds.
    // Its interval amounts are left empty (see Collector::sample).
    let mut next = next_boundary(
        SystemTime::now() + FIRST_SAMPLE_DELAY - Duration::from_nanos(1),
        cfg.interval,
    );
    let mut last_error: Option<String> = None;

    while wait_until(next) {
        services.reload(rata);
        let sample = collector.sample(rata, &cfg, &services.list);
        match writer.write(&sample) {
            Ok(()) => {
                if last_error.take().is_some() {
                    log("writing samples again");
                }
            }
            Err(e) => {
                let msg = format!("cannot write a sample to {}: {e}", rata.display_path(&dir));
                if last_error.as_deref() != Some(msg.as_str()) {
                    log(&msg);
                    last_error = Some(msg);
                }
            }
        }
        next += cfg.interval;
        let now = SystemTime::now();
        if next <= now {
            // The clock jumped or the machine slept: resume on the clock.
            next = next_boundary(now, cfg.interval);
        }
    }
    log("rata-monitor stopping");
    signal::mark_done();
    Ok(())
}

/// Sleep until `t`. Returns false if a stop was requested first.
fn wait_until(t: SystemTime) -> bool {
    loop {
        if signal::stop_requested() {
            return false;
        }
        match t.duration_since(SystemTime::now()) {
            Ok(left) if !left.is_zero() => thread::sleep(left.min(Duration::from_millis(200))),
            _ => return !signal::stop_requested(),
        }
    }
}

fn log(msg: &str) {
    let time = crate::datetime::RataTime::now();
    let _ = writeln!(std::io::stderr().lock(), "{time} {msg}");
}

/// The services to record, re-read from `service.toml` before each sample.
/// If the file becomes invalid, the last valid list is kept.
struct Services {
    list: Vec<crate::config::Service>,
    error: Option<String>,
}

impl Services {
    fn new() -> Self {
        Services {
            list: Vec::new(),
            error: None,
        }
    }

    fn reload(&mut self, rata: &Rata) {
        match Config::load(rata) {
            Ok(c) => {
                if self.error.take().is_some() {
                    log("service config is valid again");
                }
                self.list = c.services;
            }
            Err(e) => {
                let msg = e.to_string();
                if self.error.as_deref() != Some(msg.as_str()) {
                    log(&format!(
                        "cannot read the service config ({msg}); recording the last valid list"
                    ));
                    self.error = Some(msg);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries() {
        let t = |s: u64, ms: u64| UNIX_EPOCH + Duration::from_secs(s) + Duration::from_millis(ms);
        let i = Duration::from_secs(30);
        assert_eq!(next_boundary(t(1000, 0), i), t(1020, 0));
        assert_eq!(next_boundary(t(1019, 999), i), t(1020, 0));
        assert_eq!(next_boundary(t(1020, 0), i), t(1050, 0));
    }
}
