//! Unix process control: `setsid` on start, SIGTERM/SIGKILL on stop.
//!
//! A service rata started is a session leader (`setsid`), so its signals go
//! to its whole process group, which includes any children it started. A
//! process started some other way gets the signal on its PID only, so rata
//! never signals an interactive shell's job group.

use std::fs::File;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use super::{Graceful, ProcessInfo, ProcessTable};
use crate::cmdline::CommandLine;
use crate::env::{RATA_VAR, Rata};

pub(super) fn spawn_detached(cmd: &CommandLine, rata: &Rata, log: File) -> io::Result<Child> {
    let program = cmd.program();
    let mut c = if Path::new(program).is_relative() && program.contains('/') {
        // Resolve relative paths against $RATA, but keep argv[0] as written
        // so the process's command line still matches the config.
        let mut c = Command::new(rata.root().join(program));
        c.arg0(program);
        c
    } else {
        Command::new(program)
    };
    c.args(&cmd.args()[1..])
        .current_dir(rata.root())
        .env(RATA_VAR, rata.root())
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        c.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    c.spawn()
}

pub(super) fn terminate(proc: &ProcessInfo, _table: &ProcessTable) -> io::Result<Graceful> {
    signal(proc.pid, libc::SIGTERM)?;
    Ok(Graceful::Sent)
}

pub(super) fn kill(proc: &ProcessInfo, _table: &ProcessTable) -> io::Result<()> {
    signal(proc.pid, libc::SIGKILL)
}

/// Send `sig` to the process group if `pid` leads its own session, else to `pid`.
fn signal(pid: u32, sig: libc::c_int) -> io::Result<()> {
    let pid = pid as libc::pid_t;
    // SAFETY: plain syscalls on integer arguments.
    let target = unsafe {
        if libc::getsid(pid) == pid && libc::getpgid(pid) == pid {
            -pid
        } else {
            pid
        }
    };
    // SAFETY: as above.
    if unsafe { libc::kill(target, sig) } == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        Ok(()) // already gone
    } else {
        Err(err)
    }
}
