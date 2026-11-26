//! Windows process control.
//!
//! * Start: each service gets its own hidden console and process group
//!   (`CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP`).
//! * Graceful stop: a small helper process (rata itself, run with the hidden
//!   `__ctrl-break` command and no console of its own) attaches to the
//!   service's console and sends CTRL_BREAK. It does this only when every
//!   process on that console belongs to the service, so a user's shell is
//!   never signaled. The helper keeps rata's own console untouched.
//! * Forced stop: `TerminateProcess` on the process and its descendants.

use std::fs::File;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Once, OnceLock};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    SetHandleInformation,
};
use windows_sys::Win32::System::Console::{
    AttachConsole, CTRL_BREAK_EVENT, FreeConsole, GenerateConsoleCtrlEvent, GetConsoleProcessList,
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS, OpenProcess, PROCESS_TERMINATE,
    TerminateProcess,
};

use super::{Graceful, ProcessInfo, ProcessTable};
use crate::cmdline::CommandLine;
use crate::env::{RATA_VAR, Rata};

static HELPER: OnceLock<PathBuf> = OnceLock::new();

/// Helper exit code: CTRL_BREAK was sent.
const HELPER_SENT: i32 = 0;
/// Helper exit code: the console is shared or cannot be attached; force-stop instead.
const HELPER_UNAVAILABLE: i32 = 3;

/// Register the executable that implements the hidden `__ctrl-break` command.
///
/// Without a helper, graceful stops are unavailable on Windows and services
/// are force-stopped. The `rata` CLI registers itself.
pub fn set_ctrl_break_helper(exe: PathBuf) {
    let _ = HELPER.set(exe);
}

/// Stop rata's own stdin, stdout, and stderr from leaking into services.
///
/// `CreateProcess` passes every inheritable handle to the child, not just the
/// ones in its startup info. The std handles rata received from its parent
/// are inheritable, so without this a service would hold rata's stdout open
/// for as long as it runs, and whatever reads rata's output (a pipe, `$(...)`,
/// a test harness) would never see end-of-file. Rust's `Stdio::inherit`
/// duplicates the handle when it needs an inheritable one, so this is safe.
fn disinherit_std_handles() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: GetStdHandle/SetHandleInformation on our own std handles;
            // failures (no handle, not a real handle) are harmless and ignored.
            unsafe {
                let h = GetStdHandle(id);
                if !h.is_null() && h != INVALID_HANDLE_VALUE {
                    SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
                }
            }
        }
    });
}

pub(super) fn spawn_detached(cmd: &CommandLine, rata: &Rata, log: File) -> io::Result<Child> {
    disinherit_std_handles();
    let program = cmd.program();
    let path = Path::new(program);
    let exe = if path.is_relative() && (program.contains('\\') || program.contains('/')) {
        rata.root().join(path)
    } else {
        path.to_path_buf()
    };
    Command::new(exe)
        .args(&cmd.args()[1..])
        .current_dir(rata.root())
        .env(RATA_VAR, rata.root())
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .spawn()
}

pub(super) fn terminate(proc: &ProcessInfo, table: &ProcessTable) -> io::Result<Graceful> {
    let Some(helper) = HELPER.get() else {
        return Ok(Graceful::Unavailable);
    };
    let mut allowed = table.descendants(proc.pid);
    allowed.push(proc.pid);
    let status = Command::new(helper)
        .arg("__ctrl-break")
        .arg(proc.pid.to_string())
        .args(allowed.iter().map(|p| p.to_string()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS)
        .status()?;
    Ok(match status.code() {
        Some(HELPER_SENT) => Graceful::Sent,
        _ => Graceful::Unavailable,
    })
}

pub(super) fn kill(proc: &ProcessInfo, table: &ProcessTable) -> io::Result<()> {
    for child in table.descendants(proc.pid) {
        let _ = terminate_pid(child);
    }
    terminate_pid(proc.pid)
}

fn terminate_pid(pid: u32) -> io::Result<()> {
    // SAFETY: Win32 calls with a handle we open and close here.
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h.is_null() {
            let err = io::Error::last_os_error();
            // The process no longer exists.
            if err.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                return Ok(());
            }
            return Err(err);
        }
        let ok = TerminateProcess(h, 1);
        let err = io::Error::last_os_error();
        CloseHandle(h);
        if ok == 0 { Err(err) } else { Ok(()) }
    }
}

unsafe extern "system" fn ignore_ctrl(_ctrl_type: u32) -> windows_sys::core::BOOL {
    1
}

/// Body of the hidden `__ctrl-break <pid> [allowed-pid...]` command.
///
/// Returns the process exit code: 0 if CTRL_BREAK was sent, 3 if the console
/// is shared with other processes or cannot be attached, 1 on other failures.
pub fn ctrl_break_helper_main(pid: u32, allowed: &[u32]) -> i32 {
    let me = std::process::id();
    // SAFETY: console API calls on our own (helper) process.
    unsafe {
        FreeConsole();
        if AttachConsole(pid) == 0 {
            return HELPER_UNAVAILABLE;
        }
        let mut list = [0u32; 256];
        let n = GetConsoleProcessList(list.as_mut_ptr(), list.len() as u32) as usize;
        let shared =
            n == 0 || n > list.len() || list[..n].iter().any(|p| *p != me && !allowed.contains(p));
        if shared {
            FreeConsole();
            return HELPER_UNAVAILABLE;
        }
        SetConsoleCtrlHandler(Some(ignore_ctrl), 1);
        let ok = GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 0);
        FreeConsole();
        if ok == 0 { 1 } else { HELPER_SENT }
    }
}
