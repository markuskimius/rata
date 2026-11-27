//! Formatting of CLI results.
//!
//! Normal results go to stdout (suppressed by `--quiet`); failures and their
//! explanations go to stderr. Write errors (such as a closed pipe) are ignored.

use std::io::{self, Write};

use crate::config::{Action, Config, Selection, Service, Skipped};
use crate::env::Rata;
use crate::process::ProcessInfo;
use crate::service::{
    CheckReport, ForceReason, StartOutcome, StartReport, State, StatusReport, StopFailure,
    StopFailureKind, StopOutcome, StopReport,
};

/// Width of the state column in start/stop output ("stopped (forced)").
const STATE_WIDTH: usize = 16;

/// How many near misses to list per service.
const MAX_NEAR_MISSES: usize = 10;

pub(super) struct Printer<'a> {
    rata: &'a Rata,
    quiet: bool,
    name_width: usize,
}

macro_rules! out {
    ($self:ident, $($arg:tt)*) => {
        if !$self.quiet {
            let _ = writeln!(io::stdout().lock(), $($arg)*);
        }
    };
}

macro_rules! err {
    ($($arg:tt)*) => {
        let _ = writeln!(io::stderr().lock(), $($arg)*);
    };
}

impl<'a> Printer<'a> {
    pub(super) fn new(rata: &'a Rata, services: &[&Service], quiet: bool) -> Self {
        let name_width = services
            .iter()
            .map(|s| s.name.len())
            .max()
            .unwrap_or(0)
            .max(4);
        Printer {
            rata,
            quiet,
            name_width,
        }
    }

    /// A printer sized for every service in `sel`, including skipped ones.
    pub(super) fn for_selection(rata: &'a Rata, sel: &Selection<'_>, quiet: bool) -> Self {
        let all: Vec<&Service> = sel
            .services
            .iter()
            .copied()
            .chain(sel.skipped.iter().map(|s| s.service))
            .collect();
        Self::new(rata, &all, quiet)
    }

    /// One line per service a bare command skipped.
    pub(super) fn skipped(&self, action: Action, skipped: &[Skipped<'_>]) {
        for s in skipped {
            let verb = action.name();
            out!(
                self,
                "{}",
                self.line(
                    &s.service.name,
                    "skipped",
                    &format!(
                        "{}; {verb} it with: rata {verb} {}",
                        s.reason.field(),
                        s.service.name
                    )
                )
            );
        }
    }

    pub(super) fn note(&self, msg: &str) {
        out!(self, "{msg}");
    }

    fn line(&self, name: &str, state: &str, rest: &str) -> String {
        let w = self.name_width;
        format!("{name:<w$}  {state:<STATE_WIDTH$}  {rest}")
            .trim_end()
            .to_string()
    }

    fn path(&self, p: &std::path::Path) -> String {
        self.rata.display_path(p)
    }

    pub(super) fn start(&self, r: &StartReport) {
        match &r.outcome {
            StartOutcome::Started { pids, log } => {
                out!(
                    self,
                    "{}",
                    self.line(
                        &r.name,
                        "started",
                        &format!("pid {}  log {}", pids_str(pids), self.path(log))
                    )
                );
            }
            StartOutcome::AlreadyRunning { pids } => {
                out!(
                    self,
                    "{}",
                    self.line(
                        &r.name,
                        "already running",
                        &format!("pid {}", pids_str(pids))
                    )
                );
            }
            StartOutcome::Unmatched { pid, field, log } => {
                err!(
                    "{}",
                    self.line(
                        &r.name,
                        "FAILED",
                        &format!("started pid {pid}, but no process matches its {field}; rata cannot detect or stop it")
                    )
                );
                err!(
                    "error: {}: set psline to the process's actual command line (see 'rata check'); log {}",
                    r.name,
                    self.path(log)
                );
            }
            StartOutcome::Failed { reason, log } => {
                let see = match log {
                    Some(l) => format!(", see {}", self.path(l)),
                    _ => String::new(),
                };
                err!(
                    "{}",
                    self.line(&r.name, "FAILED", &format!("{reason}{see}"))
                );
            }
        }
    }

    pub(super) fn stop(&self, r: &StopReport) {
        match &r.outcome {
            StopOutcome::NotRunning => out!(self, "{}", self.line(&r.name, "not running", "")),
            StopOutcome::Stopped { pids, forced: None } => {
                out!(
                    self,
                    "{}",
                    self.line(&r.name, "stopped", &format!("pid {}", pids_str(pids)))
                );
            }
            StopOutcome::Stopped {
                pids,
                forced: Some(why),
            } => {
                let note = match why {
                    ForceReason::Timeout(t) => {
                        format!("did not exit within {}s; killed", t.as_secs())
                    }
                    ForceReason::NoGraceful(why) => {
                        format!("no graceful stop possible ({why}); killed")
                    }
                };
                out!(
                    self,
                    "{}",
                    self.line(
                        &r.name,
                        "stopped (forced)",
                        &format!("pid {}  {note}", pids_str(pids))
                    )
                );
            }
            StopOutcome::Failed { stopped, failures } => {
                let failed: Vec<u32> = failures.iter().map(|f| f.pid).collect();
                let short = failures.first().map(short_failure).unwrap_or_default();
                err!(
                    "{}",
                    self.line(
                        &r.name,
                        "FAILED",
                        &format!("pid {}  {short}", pids_str(&failed))
                    )
                );
                if stopped.is_empty() {
                    for f in failures {
                        err!(
                            "error: {} (pid {}): {}",
                            r.name,
                            f.pid,
                            long_failure(&f.kind)
                        );
                    }
                } else {
                    let total = stopped.len() + failures.len();
                    for f in failures {
                        err!(
                            "error: {}: stopped {} of {total} matching processes; pid {} could not be stopped ({})",
                            r.name,
                            stopped.len(),
                            f.pid,
                            short_failure(f)
                        );
                    }
                }
            }
        }
    }

    pub(super) fn status(&self, reports: &[StatusReport], verbose: bool) {
        let w = self.name_width;
        let pid_w = reports
            .iter()
            .map(|r| pids_cell(&r.processes).len())
            .max()
            .unwrap_or(0)
            .max(3);
        out!(
            self,
            "{}",
            format!(
                "{:<w$}  {:<7}  {:<pid_w$}  DESCRIPTION",
                "NAME", "STATE", "PID"
            )
            .trim_end()
        );
        for r in reports {
            let state = match r.state {
                State::Running => "running",
                State::Stopped => "stopped",
            };
            let line = format!(
                "{:<w$}  {:<7}  {:<pid_w$}  {}",
                r.name,
                state,
                pids_cell(&r.processes),
                r.description.as_deref().unwrap_or("")
            );
            out!(self, "{}", line.trim_end());
            if verbose {
                out!(self, "    match      {}", r.psline.join(" "));
                for p in &r.processes {
                    out!(self, "    pid {:<6} {}", p.pid, p.display_cmdline());
                }
                if let Some(log) = &r.log {
                    out!(self, "    log        {}", self.path(log));
                }
                self.near_misses("    ", &r.near_misses);
            }
        }
    }

    pub(super) fn status_json(&self, reports: &[StatusReport]) {
        let doc = serde_json::json!({ "version": 1, "services": reports });
        let _ = writeln!(
            io::stdout().lock(),
            "{}",
            serde_json::to_string_pretty(&doc).expect("status is serializable")
        );
    }

    pub(super) fn check(&self, config: &Config, reports: &[CheckReport]) {
        let n = reports.len();
        out!(
            self,
            "config  {} OK ({n} service{})",
            self.path(&config.path),
            if n == 1 { "" } else { "s" }
        );
        for r in reports {
            out!(self, "");
            match &r.description {
                Some(d) => out!(self, "{}  {d}", r.name),
                None => out!(self, "{}", r.name),
            }
            out!(self, "  start      {:?}", r.start);
            match &r.psline {
                Some(ps) => out!(self, "  psline     {ps:?}"),
                None => out!(self, "  psline     (same as start)"),
            }
            let explicit: Vec<&str> = [(r.explicit_start, "start"), (r.explicit_stop, "stop")]
                .into_iter()
                .filter_map(|(on, what)| on.then_some(what))
                .collect();
            if !explicit.is_empty() {
                out!(
                    self,
                    "  explicit   {} (only when named or with --all)",
                    explicit.join(", ")
                );
            }
            match r.matches.len() {
                0 => out!(self, "  matches    none (not running)"),
                n => out!(
                    self,
                    "  matches    {n} process{}: pid {}",
                    if n == 1 { "" } else { "es" },
                    r.matches
                        .iter()
                        .map(|p| p.pid.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
            self.near_misses("  ", &r.near_misses);
            for w in &r.warnings {
                out!(self, "  warning    {w}");
            }
        }
    }

    fn near_misses(&self, indent: &str, misses: &[ProcessInfo]) {
        for p in misses.iter().take(MAX_NEAR_MISSES) {
            out!(
                self,
                "{indent}near miss  pid {} {}",
                p.pid,
                p.display_cmdline()
            );
        }
        if misses.len() > MAX_NEAR_MISSES {
            out!(
                self,
                "{indent}near miss  ... and {} more",
                misses.len() - MAX_NEAR_MISSES
            );
        }
    }
}

/// What `rata init` created, and what to do next.
pub(super) fn init(r: &crate::init::InitReport, quiet: bool) {
    if quiet {
        return;
    }
    let mut o = io::stdout().lock();
    let root = r.root.display();
    let verb = if r.created_root { "Created" } else { "Set up" };
    let _ = writeln!(o, "{verb} {root}");
    let sep = std::path::MAIN_SEPARATOR;
    for p in &r.created {
        let rel = p.strip_prefix(&r.root).unwrap_or(p).display();
        let slash = if p.is_dir() {
            sep.to_string()
        } else {
            String::new()
        };
        let _ = writeln!(o, "  {rel}{slash}");
    }
    let _ = writeln!(o, "\nNext steps:");
    let set =
        std::env::var_os(crate::env::RATA_VAR).is_some_and(|v| std::path::Path::new(&v) == r.root);
    if !set {
        if cfg!(windows) {
            let _ = writeln!(o, "  $env:RATA = \"{root}\"");
        } else {
            let quoted = shell_words::quote(&r.root.to_string_lossy()).into_owned();
            let _ = writeln!(o, "  export RATA={quoted}");
        }
    }
    let _ = writeln!(o, "  rata check");
    let _ = writeln!(o, "  rata start");
}

/// `4121`, `4121,4122,4123`, or `4121 +3` for longer lists.
fn pids_str(pids: &[u32]) -> String {
    if pids.len() > 3 {
        format!("{} +{}", pids[0], pids.len() - 1)
    } else {
        pids.iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn pids_cell(procs: &[ProcessInfo]) -> String {
    if procs.is_empty() {
        "-".into()
    } else {
        pids_str(&procs.iter().map(|p| p.pid).collect::<Vec<_>>())
    }
}

fn short_failure(f: &StopFailure) -> String {
    match &f.kind {
        StopFailureKind::PermissionDenied { .. } => "permission denied".into(),
        StopFailureKind::StillRunning { waited } => {
            format!("still running {}s after kill", waited.as_secs())
        }
        StopFailureKind::Other(msg) => msg.clone(),
    }
}

fn long_failure(kind: &StopFailureKind) -> String {
    match kind {
        StopFailureKind::PermissionDenied { signal } => {
            if cfg!(windows) {
                "access denied. The process may be running as Administrator; try again from an elevated PowerShell.".into()
            } else {
                format!(
                    "permission denied sending {signal}. The process may be protected or running under different credentials."
                )
            }
        }
        StopFailureKind::StillRunning { waited } => format!(
            "still running {}s after {}. The process may be stuck in an uninterruptible system call.",
            waited.as_secs(),
            crate::process::FORCE_SIGNAL
        ),
        StopFailureKind::Other(msg) => msg.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_lists() {
        assert_eq!(pids_str(&[1]), "1");
        assert_eq!(pids_str(&[1, 2, 3]), "1,2,3");
        assert_eq!(pids_str(&[1, 2, 3, 4]), "1 +3");
    }
}
