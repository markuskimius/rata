//! Finding, starting, and stopping operating-system processes.
//!
//! Only processes owned by the current user are considered. rata's own
//! process, threads, and zombie processes are never listed.

use std::fs::File;
use std::io;
use std::process::Child;

use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

use crate::cmdline::CommandLine;
use crate::env::Rata;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;
#[cfg(windows)]
pub use windows::{ctrl_break_helper_main, set_ctrl_break_helper};

/// Name of the graceful stop signal on this platform, for messages.
pub const GRACEFUL_SIGNAL: &str = if cfg!(windows) {
    "CTRL_BREAK"
} else {
    "SIGTERM"
};

/// Name of the forced stop on this platform, for messages.
pub const FORCE_SIGNAL: &str = if cfg!(windows) {
    "TerminateProcess"
} else {
    "SIGKILL"
};

/// One process, as seen by a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessInfo {
    /// Process ID.
    pub pid: u32,
    /// Parent process ID, if known.
    #[serde(skip)]
    pub parent: Option<u32>,
    /// Process name reported by the OS (may be truncated).
    #[serde(skip)]
    pub name: String,
    /// Full argument list, program first. Empty if it could not be read.
    pub cmdline: Vec<String>,
}

impl ProcessInfo {
    /// The command line joined with single spaces, or a placeholder if unreadable.
    pub fn display_cmdline(&self) -> String {
        if self.cmdline.is_empty() {
            format!("({}: command line unreadable)", self.name)
        } else {
            self.cmdline.join(" ")
        }
    }
}

/// A snapshot of the current user's processes.
#[derive(Debug, Clone, Default)]
pub struct ProcessTable {
    procs: Vec<ProcessInfo>,
}

/// Take a snapshot of the current user's processes.
pub fn scan() -> ProcessTable {
    let mut sys = System::new();
    refresh(&mut sys, ProcessesToUpdate::All);
    table_from_system(&sys, false)
}

/// Build a table from an already refreshed [`System`]: the processes owned
/// by the current user, without threads or zombies. rata's own process is
/// left out unless `include_self` is set.
///
/// The `System` must have been refreshed with command lines and users.
pub fn table_from_system(sys: &System, include_self: bool) -> ProcessTable {
    let me = Pid::from_u32(std::process::id());
    let my_user = sys.process(me).and_then(|p| p.user_id()).cloned();
    let mut procs: Vec<ProcessInfo> = sys
        .processes()
        .iter()
        .filter(|(pid, p)| {
            (include_self || **pid != me)
                && p.thread_kind().is_none()
                && p.status() != ProcessStatus::Zombie
                && my_user.as_ref().is_none_or(|u| p.user_id() == Some(u))
        })
        .map(|(pid, p)| ProcessInfo {
            pid: pid.as_u32(),
            parent: p.parent().map(|pp| pp.as_u32()),
            name: p.name().to_string_lossy().into_owned(),
            cmdline: p
                .cmd()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        })
        .collect();
    procs.sort_by_key(|p| p.pid);
    ProcessTable { procs }
}

fn refresh(sys: &mut System, which: ProcessesToUpdate<'_>) {
    sys.refresh_processes_specifics(
        which,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_user(UpdateKind::Always),
    );
}

impl ProcessTable {
    /// Build a table from a list of processes (useful for tests).
    pub fn from_processes(procs: Vec<ProcessInfo>) -> Self {
        ProcessTable { procs }
    }

    /// Every process in the snapshot.
    pub fn processes(&self) -> &[ProcessInfo] {
        &self.procs
    }

    /// Processes whose command line exactly matches `cmd`.
    pub fn find(&self, cmd: &CommandLine) -> Vec<&ProcessInfo> {
        self.procs
            .iter()
            .filter(|p| cmd.matches(&p.cmdline))
            .collect()
    }

    /// Processes running the same program as `cmd` that do not match it.
    pub fn near_misses(&self, cmd: &CommandLine) -> Vec<&ProcessInfo> {
        self.procs
            .iter()
            .filter(|p| {
                !cmd.matches(&p.cmdline)
                    && match p.cmdline.first() {
                        Some(prog) => cmd.program_matches(prog),
                        None => cmd.program_matches(&p.name),
                    }
            })
            .collect()
    }

    /// PIDs of every descendant of `pid` in the snapshot.
    pub fn descendants(&self, pid: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut frontier = vec![pid];
        while let Some(parent) = frontier.pop() {
            for p in &self.procs {
                if p.parent == Some(parent) && p.pid != pid && !out.contains(&p.pid) {
                    out.push(p.pid);
                    frontier.push(p.pid);
                }
            }
        }
        out
    }
}

/// Of `procs`, the ones still running: the PID exists, is not a zombie, and
/// still has the same command line (so a reused PID is not mistaken for it).
pub fn still_running(procs: &[ProcessInfo]) -> Vec<ProcessInfo> {
    if procs.is_empty() {
        return Vec::new();
    }
    let pids: Vec<Pid> = procs.iter().map(|p| Pid::from_u32(p.pid)).collect();
    let mut sys = System::new();
    refresh(&mut sys, ProcessesToUpdate::Some(&pids));
    procs
        .iter()
        .filter(|info| {
            sys.process(Pid::from_u32(info.pid)).is_some_and(|p| {
                p.status() != ProcessStatus::Zombie
                    && p.cmd().iter().map(|a| a.to_string_lossy()).eq(info
                        .cmdline
                        .iter()
                        .map(|s| std::borrow::Cow::Borrowed(s.as_str())))
            })
        })
        .cloned()
        .collect()
}

/// Start `cmd` in the background, detached from rata's terminal, with
/// working directory `$RATA`, stdin closed, and stdout/stderr going to `log`.
pub fn spawn_detached(cmd: &CommandLine, rata: &Rata, log: File) -> io::Result<Child> {
    platform::spawn_detached(cmd, rata, log)
}

/// Result of asking a process to stop gracefully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Graceful {
    /// The graceful stop signal was delivered (or the process was already gone).
    Sent,
    /// A graceful stop is not possible for this process; force-stop it instead.
    /// The text says why, e.g. `shared console with pid 812 (cmd.exe)`.
    Unavailable(String),
}

/// Ask `proc` to stop gracefully (SIGTERM on Unix, CTRL_BREAK on Windows).
pub fn terminate(proc: &ProcessInfo, table: &ProcessTable) -> io::Result<Graceful> {
    platform::terminate(proc, table)
}

/// Force-stop `proc` (SIGKILL on Unix, TerminateProcess on Windows).
pub fn kill(proc: &ProcessInfo, table: &ProcessTable) -> io::Result<()> {
    platform::kill(proc, table)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: u32, cmd: &[&str]) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent: Some(parent),
            name: cmd
                .first()
                .map_or("x", |c| c.rsplit('/').next().unwrap())
                .to_string(),
            cmdline: cmd.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn find_and_near_misses() {
        let t = ProcessTable::from_processes(vec![
            p(10, 1, &["/r/bin/api", "--port", "1"]),
            p(11, 1, &["/r/bin/api", "--port", "2"]),
            p(12, 1, &["vim", "/r/bin/api"]),
            p(13, 1, &[]),
        ]);
        let cmd =
            CommandLine::from_args(vec!["/r/bin/api".into(), "--port".into(), "1".into()]).unwrap();
        assert_eq!(t.find(&cmd).iter().map(|p| p.pid).collect::<Vec<_>>(), [10]);
        assert_eq!(
            t.near_misses(&cmd)
                .iter()
                .map(|p| p.pid)
                .collect::<Vec<_>>(),
            [11]
        );
    }

    #[test]
    fn descendants_walks_tree() {
        let t = ProcessTable::from_processes(vec![
            p(10, 1, &["a"]),
            p(11, 10, &["b"]),
            p(12, 11, &["c"]),
            p(13, 1, &["d"]),
        ]);
        let mut d = t.descendants(10);
        d.sort();
        assert_eq!(d, [11, 12]);
    }

    #[test]
    fn scan_excludes_self() {
        let me = std::process::id();
        assert!(scan().processes().iter().all(|p| p.pid != me));
    }
}
