//! Service operations: start, stop, restart, status, and check.
//!
//! These functions do the work and return structured results; the `rata`
//! CLI only formats them. They do not take the rata lock; callers that
//! change services (`start`, `stop`, `restart`) should hold a
//! [`Lock`](crate::lock::Lock).

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::{Config, Service};
use crate::env::Rata;
use crate::error::Result;
use crate::logfile;
use crate::process::{self, Graceful, ProcessInfo, ProcessTable};

/// How long after starting a service rata checks that it is still running.
pub const START_CHECK_DELAY: Duration = Duration::from_millis(500);

/// How long rata waits for a process to disappear after force-stopping it.
pub const KILL_WAIT: Duration = Duration::from_secs(5);

const POLL: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------- start

/// What happened when starting one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    /// The service was started and its process matches the config.
    Started {
        /// Matching PIDs (the started process first, when it matches).
        pids: Vec<u32>,
        /// The new log file.
        log: PathBuf,
    },
    /// A matching process was already running; nothing was started.
    AlreadyRunning {
        /// Matching PIDs.
        pids: Vec<u32>,
    },
    /// The process started and is still running, but no process matches the
    /// service's psline (or start command), so rata cannot detect or stop it.
    /// The process is left running.
    Unmatched {
        /// The started process.
        pid: u32,
        /// `"psline"` or `"start command"`: what failed to match.
        field: &'static str,
        /// The new log file.
        log: PathBuf,
    },
    /// The service could not be started.
    Failed {
        /// Why, in one line.
        reason: String,
        /// The log file, if one was created.
        log: Option<PathBuf>,
    },
}

/// The result of starting one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartReport {
    /// Service name.
    pub name: String,
    /// What happened.
    pub outcome: StartOutcome,
}

impl StartReport {
    /// Whether the service is running after the start.
    pub fn ok(&self) -> bool {
        matches!(
            self.outcome,
            StartOutcome::Started { .. } | StartOutcome::AlreadyRunning { .. }
        )
    }
}

/// Start each service that is not already running, in the given order.
///
/// All services are launched first; then rata waits [`START_CHECK_DELAY`]
/// once and checks every new process, so starting many services takes about
/// half a second in total.
pub fn start(rata: &Rata, services: &[&Service]) -> Result<Vec<StartReport>> {
    rata.ensure_runtime_dirs()?;
    let table = process::scan();
    let mut outcomes: Vec<Option<StartOutcome>> = vec![None; services.len()];
    let mut launched = Vec::new();

    for (i, svc) in services.iter().enumerate() {
        let matcher = svc.match_command(rata);
        let running = table.find(&matcher);
        if !running.is_empty() {
            outcomes[i] = Some(StartOutcome::AlreadyRunning {
                pids: running.iter().map(|p| p.pid).collect(),
            });
            continue;
        }
        let (log_path, log_file) = match logfile::create(&rata.log_dir(), &svc.name) {
            Ok(x) => x,
            Err(e) => {
                outcomes[i] = Some(StartOutcome::Failed {
                    reason: format!(
                        "cannot create log file in {}: {e}",
                        rata.display_path(&rata.log_dir())
                    ),
                    log: None,
                });
                continue;
            }
        };
        let cmd = svc.start_command(rata);
        let mut log_for_errors = log_file.try_clone().ok();
        match process::spawn_detached(&cmd, rata, log_file) {
            Ok(child) => launched.push((i, child, log_path)),
            Err(e) => {
                let reason = format!("cannot run {}: {}", cmd.program(), describe_io(&e));
                if let Some(f) = log_for_errors.as_mut() {
                    let _ = writeln!(f, "rata: {reason}");
                }
                outcomes[i] = Some(StartOutcome::Failed {
                    reason,
                    log: Some(log_path),
                });
            }
        }
    }

    if !launched.is_empty() {
        let deadline = Instant::now() + START_CHECK_DELAY;
        let mut exited: Vec<Option<ExitStatus>> = vec![None; launched.len()];
        loop {
            for (j, (_, child, _)) in launched.iter_mut().enumerate() {
                if exited[j].is_none()
                    && let Ok(Some(status)) = child.try_wait()
                {
                    exited[j] = Some(status);
                }
            }
            if exited.iter().all(Option::is_some) || Instant::now() >= deadline {
                break;
            }
            thread::sleep(POLL);
        }
        let table = process::scan();
        for ((i, child, log), status) in launched.into_iter().zip(exited) {
            let svc = services[i];
            outcomes[i] = Some(match status {
                Some(status) => StartOutcome::Failed {
                    reason: format!(
                        "exited within {}ms ({})",
                        START_CHECK_DELAY.as_millis(),
                        describe_exit(status)
                    ),
                    log: Some(log),
                },
                None => {
                    let mut pids: Vec<u32> = table
                        .find(&svc.match_command(rata))
                        .iter()
                        .map(|p| p.pid)
                        .collect();
                    if pids.is_empty() {
                        StartOutcome::Unmatched {
                            pid: child.id(),
                            field: if svc.psline.is_some() {
                                "psline"
                            } else {
                                "start command"
                            },
                            log,
                        }
                    } else {
                        if let Some(pos) = pids.iter().position(|p| *p == child.id()) {
                            pids.swap(0, pos);
                        }
                        StartOutcome::Started { pids, log }
                    }
                }
            });
        }
    }

    Ok(services
        .iter()
        .zip(outcomes)
        .map(|(svc, outcome)| StartReport {
            name: svc.name.clone(),
            outcome: outcome.expect("every service has an outcome"),
        })
        .collect())
}

fn describe_exit(status: ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("code {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return format!("killed by signal {sig}");
        }
    }
    status.to_string()
}

fn describe_io(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "program not found".into(),
        io::ErrorKind::PermissionDenied => "permission denied".into(),
        _ => e.to_string(),
    }
}

// ---------------------------------------------------------------- stop

/// Why a stop needed force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForceReason {
    /// The process did not exit within the graceful timeout.
    Timeout(Duration),
    /// No graceful stop was possible (Windows: shared or unattachable console).
    NoGraceful,
}

/// Why one process could not be stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopFailureKind {
    /// The OS refused the signal or termination request.
    PermissionDenied {
        /// The signal or call that was refused (e.g. `SIGTERM`).
        signal: &'static str,
    },
    /// The process survived the forced stop.
    StillRunning {
        /// How long rata waited after the forced stop.
        waited: Duration,
    },
    /// Any other error.
    Other(String),
}

/// One process that could not be stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopFailure {
    /// Process ID.
    pub pid: u32,
    /// Why.
    pub kind: StopFailureKind,
}

/// What happened when stopping one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    /// Every matching process stopped.
    Stopped {
        /// The PIDs that were stopped.
        pids: Vec<u32>,
        /// Set if any process had to be force-stopped.
        forced: Option<ForceReason>,
    },
    /// No matching process was running.
    NotRunning,
    /// At least one matching process is still running.
    Failed {
        /// The PIDs that did stop.
        stopped: Vec<u32>,
        /// The PIDs that did not, and why.
        failures: Vec<StopFailure>,
    },
}

/// The result of stopping one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopReport {
    /// Service name.
    pub name: String,
    /// What happened.
    pub outcome: StopOutcome,
}

impl StopReport {
    /// Whether the service is stopped afterwards.
    pub fn ok(&self) -> bool {
        !matches!(self.outcome, StopOutcome::Failed { .. })
    }
}

/// Stop each service, in **reverse** of the given order.
///
/// `timeout` overrides every service's `stop_timeout`. `on_report` is called
/// as each service finishes, so callers can show progress.
pub fn stop(
    rata: &Rata,
    services: &[&Service],
    timeout: Option<Duration>,
    mut on_report: impl FnMut(&StopReport),
) -> Vec<StopReport> {
    let mut reports = Vec::with_capacity(services.len());
    for svc in services.iter().rev() {
        let report = StopReport {
            name: svc.name.clone(),
            outcome: stop_one(rata, svc, timeout.unwrap_or_else(|| svc.stop_timeout())),
        };
        on_report(&report);
        reports.push(report);
    }
    reports
}

fn stop_one(rata: &Rata, svc: &Service, timeout: Duration) -> StopOutcome {
    let table = process::scan();
    let procs: Vec<ProcessInfo> = table
        .find(&svc.match_command(rata))
        .into_iter()
        .cloned()
        .collect();
    if procs.is_empty() {
        return StopOutcome::NotRunning;
    }

    let mut failures = Vec::new();
    let mut waiting = Vec::new();
    let mut kill_now = Vec::new();
    for p in &procs {
        match process::terminate(p, &table) {
            Ok(Graceful::Sent) => waiting.push(p.clone()),
            Ok(Graceful::Unavailable) => kill_now.push(p.clone()),
            Err(e) => failures.push(failure(p.pid, &e, process::GRACEFUL_SIGNAL)),
        }
    }

    let timed_out = wait_for_exit(&waiting, timeout);
    let forced = if !kill_now.is_empty() {
        Some(ForceReason::NoGraceful)
    } else if !timed_out.is_empty() {
        Some(ForceReason::Timeout(timeout))
    } else {
        None
    };

    let mut killed = Vec::new();
    for p in timed_out.into_iter().chain(kill_now) {
        match process::kill(&p, &table) {
            Ok(()) => killed.push(p),
            Err(e) => failures.push(failure(p.pid, &e, process::FORCE_SIGNAL)),
        }
    }
    for p in wait_for_exit(&killed, KILL_WAIT) {
        failures.push(StopFailure {
            pid: p.pid,
            kind: StopFailureKind::StillRunning { waited: KILL_WAIT },
        });
    }

    let failed: Vec<u32> = failures.iter().map(|f| f.pid).collect();
    let stopped: Vec<u32> = procs
        .iter()
        .map(|p| p.pid)
        .filter(|pid| !failed.contains(pid))
        .collect();
    if failures.is_empty() {
        StopOutcome::Stopped {
            pids: stopped,
            forced,
        }
    } else {
        StopOutcome::Failed { stopped, failures }
    }
}

fn failure(pid: u32, e: &io::Error, signal: &'static str) -> StopFailure {
    let kind = if e.kind() == io::ErrorKind::PermissionDenied {
        StopFailureKind::PermissionDenied { signal }
    } else {
        StopFailureKind::Other(format!("{signal} failed: {e}"))
    };
    StopFailure { pid, kind }
}

/// Wait up to `timeout` for `procs` to exit; return the ones still running.
fn wait_for_exit(procs: &[ProcessInfo], timeout: Duration) -> Vec<ProcessInfo> {
    let deadline = Instant::now() + timeout;
    let mut remaining = procs.to_vec();
    loop {
        remaining = process::still_running(&remaining);
        if remaining.is_empty() || Instant::now() >= deadline {
            return remaining;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------- restart

/// The result of `restart`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartReport {
    /// Stop results, in reverse order.
    pub stopped: Vec<StopReport>,
    /// Start results, in order, for services that stopped successfully.
    pub started: Vec<StartReport>,
}

impl RestartReport {
    /// Whether every service stopped and started again.
    pub fn ok(&self) -> bool {
        self.stopped.iter().all(StopReport::ok) && self.started.iter().all(StartReport::ok)
    }
}

/// Stop the services (reverse order), then start the ones that stopped (in order).
pub fn restart(
    rata: &Rata,
    services: &[&Service],
    timeout: Option<Duration>,
    on_stop: impl FnMut(&StopReport),
) -> Result<RestartReport> {
    let stopped = stop(rata, services, timeout, on_stop);
    let to_start: Vec<&Service> = services
        .iter()
        .copied()
        .filter(|s| stopped.iter().any(|r| r.name == s.name && r.ok()))
        .collect();
    let started = start(rata, &to_start)?;
    Ok(RestartReport { stopped, started })
}

// ---------------------------------------------------------------- status

/// Whether a service is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// At least one matching process is running.
    Running,
    /// No matching process is running.
    Stopped,
}

/// The status of one service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusReport {
    /// Service name.
    pub name: String,
    /// Service description.
    pub description: Option<String>,
    /// Running or stopped.
    pub state: State,
    /// The command line processes must match, split into arguments.
    #[serde(skip)]
    pub psline: Vec<String>,
    /// Matching processes.
    pub processes: Vec<ProcessInfo>,
    /// The most recent log file, if any.
    pub log: Option<PathBuf>,
    /// Processes running the same program that do not match.
    pub near_misses: Vec<ProcessInfo>,
}

/// The status of each service, in the given order.
pub fn status(rata: &Rata, services: &[&Service]) -> Vec<StatusReport> {
    let table = process::scan();
    services
        .iter()
        .map(|svc| {
            let m = svc.match_command(rata);
            let processes: Vec<ProcessInfo> = table.find(&m).into_iter().cloned().collect();
            StatusReport {
                name: svc.name.clone(),
                description: svc.description.clone(),
                state: if processes.is_empty() {
                    State::Stopped
                } else {
                    State::Running
                },
                psline: m.args().to_vec(),
                processes,
                log: logfile::latest(&rata.log_dir(), &svc.name),
                near_misses: table.near_misses(&m).into_iter().cloned().collect(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------- check

/// Descriptions longer than this get a warning from `check`.
pub const LONG_DESCRIPTION: usize = 60;

/// What `check` found for one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    /// Service name.
    pub name: String,
    /// Service description.
    pub description: Option<String>,
    /// The `start` command, split and expanded.
    pub start: Vec<String>,
    /// The `psline`, split and expanded, if set.
    pub psline: Option<Vec<String>>,
    /// Processes matching right now.
    pub matches: Vec<ProcessInfo>,
    /// Processes running the same program that do not match.
    pub near_misses: Vec<ProcessInfo>,
    /// Problems that do not make the config invalid.
    pub warnings: Vec<String>,
}

/// Inspect every service in a (valid) config against the running processes.
pub fn check(rata: &Rata, config: &Config) -> Vec<CheckReport> {
    check_with(rata, config, &process::scan())
}

/// Like [`check`], against a given process table.
pub fn check_with(rata: &Rata, config: &Config, table: &ProcessTable) -> Vec<CheckReport> {
    config
        .services
        .iter()
        .map(|svc| {
            let start = svc.start_command(rata);
            let m = svc.match_command(rata);
            let matches: Vec<ProcessInfo> = table.find(&m).into_iter().cloned().collect();
            let mut warnings = Vec::new();
            if start.program_is_relative_path() {
                warnings.push(format!(
                    "start runs a relative path (\"{}\"); a process started by hand from another directory will not match. Use ${{RATA}}/{} instead",
                    start.program(),
                    start.program()
                ));
            }
            if matches.len() > 1 {
                let what = if svc.psline.is_some() { "psline" } else { "start" };
                warnings.push(format!("{what} matches {} processes", matches.len()));
            }
            if let Some(d) = &svc.description
                && d.chars().count() > LONG_DESCRIPTION {
                    warnings.push(format!(
                        "description is longer than {LONG_DESCRIPTION} characters; it is meant as a short name"
                    ));
                }
            CheckReport {
                name: svc.name.clone(),
                description: svc.description.clone(),
                start: start.args().to_vec(),
                psline: svc.psline.as_ref().map(|_| m.args().to_vec()),
                near_misses: table.near_misses(&m).into_iter().cloned().collect(),
                matches,
                warnings,
            }
        })
        .collect()
}
