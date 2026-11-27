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

use std::fs::{self, File};
use std::io::{self, Write};
use std::mem::ManuallyDrop;
use std::os::windows::io::FromRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Once, OnceLock};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, STILL_ACTIVE,
    SetHandleInformation,
};
use windows_sys::Win32::System::Console::{
    AttachConsole, CTRL_BREAK_EVENT, FreeConsole, GenerateConsoleCtrlEvent, GetConsoleProcessList,
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS, GetExitCodeProcess, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess,
};

use super::{Graceful, ProcessInfo, ProcessTable};
use crate::cmdline::CommandLine;
use crate::env::{RATA_VAR, Rata};

static HELPER: OnceLock<PathBuf> = OnceLock::new();

/// Helper exit code: CTRL_BREAK was sent.
const HELPER_SENT: i32 = 0;
/// Helper exit code: sending CTRL_BREAK failed.
const HELPER_SEND_FAILED: i32 = 1;
/// Helper exit code: the console is shared; the other PIDs are on stdout.
const HELPER_SHARED: i32 = 3;
/// Helper exit code: the process's console cannot be attached.
const HELPER_NO_CONSOLE: i32 = 4;

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
    } else if let Some(copy) = bundled_copy(program, rata) {
        copy
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

/// Run programs installed with rata from a copy, so upgrades are not blocked.
///
/// Windows does not let a running program's file be replaced, so a service
/// running `rata-monitor.exe` from the install directory would make every
/// upgrade fail. When a service's program is a bare name (no directory) and
/// a program of that name is installed next to `rata.exe`, rata copies it to
/// `$RATA\work\run\bin` (when the copy is missing or differs in size or
/// modification time) and runs the copy. A bare psline matches any
/// directory, so detection and stopping are unaffected. Returns `None`, and
/// the program runs as written, if there is nothing to copy or copying fails.
fn bundled_copy(program: &str, rata: &Rata) -> Option<PathBuf> {
    if program.contains(['\\', '/', ':']) {
        return None;
    }
    // `name`, `name.exe`, and `name.EXE` all name the same program; the
    // copy is always `name.exe`.
    let stem = match program
        .len()
        .checked_sub(4)
        .and_then(|i| program.get(i..).map(|e| (i, e)))
    {
        Some((i, ext)) if ext.eq_ignore_ascii_case(".exe") => &program[..i],
        _ => program,
    };
    let file = format!("{stem}.exe");
    let installed = std::env::current_exe().ok()?.parent()?.join(&file);
    if !installed.is_file() {
        return None;
    }
    let dir = rata.run_dir().join("bin");
    fs::create_dir_all(&dir).ok()?;
    let copy = dir.join(&file);
    if !same_file(&installed, &copy) {
        // Copy under a temporary name first, so a failed copy never leaves
        // a broken program behind.
        let tmp = dir.join(format!("{file}.tmp"));
        if fs::copy(&installed, &tmp).is_err() || fs::rename(&tmp, &copy).is_err() {
            let _ = fs::remove_file(&tmp);
            return None;
        }
    }
    Some(copy)
}

/// Whether `a` and `b` have the same size and modification time.
/// (`fs::copy` keeps the modification time on Windows.)
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.len() == b.len() && a.modified().ok() == b.modified().ok(),
        _ => false,
    }
}

pub(super) fn terminate(proc: &ProcessInfo, table: &ProcessTable) -> io::Result<Graceful> {
    let Some(helper) = HELPER.get() else {
        return Ok(Graceful::Unavailable("no CTRL_BREAK helper".into()));
    };
    let mut allowed = table.descendants(proc.pid);
    allowed.push(proc.pid);
    let out = Command::new(helper)
        .arg("__ctrl-break")
        .arg(proc.pid.to_string())
        .args(allowed.iter().map(|p| p.to_string()))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS)
        .output()?;
    Ok(match out.status.code() {
        Some(HELPER_SENT) => Graceful::Sent,
        Some(HELPER_SHARED) => Graceful::Unavailable(shared_reason(&out.stdout, table)),
        Some(HELPER_NO_CONSOLE) => Graceful::Unavailable("no console to attach to".into()),
        Some(HELPER_SEND_FAILED) => Graceful::Unavailable("CTRL_BREAK failed".into()),
        code => Graceful::Unavailable(format!("CTRL_BREAK helper exited with {code:?}")),
    })
}

/// Describe the foreign processes the helper found on the console, e.g.
/// `shared console with pid 812 (cmd.exe)`.
fn shared_reason(stdout: &[u8], table: &ProcessTable) -> String {
    let others: Vec<String> = String::from_utf8_lossy(stdout)
        .split_whitespace()
        .filter_map(|s| s.parse::<u32>().ok())
        .map(
            |pid| match table.processes().iter().find(|p| p.pid == pid) {
                Some(p) if !p.name.is_empty() => format!("{pid} ({})", p.name),
                _ => pid.to_string(),
            },
        )
        .collect();
    if others.is_empty() {
        "shared console".into()
    } else {
        format!("shared console with pid {}", others.join(", "))
    }
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
        let h = OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        );
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
        // TerminateProcess fails with "access denied" on a process that is
        // already exiting; that counts as stopped.
        let mut code = 0u32;
        let exited = GetExitCodeProcess(h, &mut code) != 0 && code != STILL_ACTIVE as u32;
        CloseHandle(h);
        if ok == 0 && !exited { Err(err) } else { Ok(()) }
    }
}

unsafe extern "system" fn ignore_ctrl(_ctrl_type: u32) -> windows_sys::core::BOOL {
    1
}

/// Body of the hidden `__ctrl-break <pid> [allowed-pid...]` command.
///
/// Returns the process exit code: 0 if CTRL_BREAK was sent, 3 if the console
/// is shared with other processes (their PIDs are written to stdout), 4 if it
/// cannot be attached, 1 if sending failed.
pub fn ctrl_break_helper_main(pid: u32, allowed: &[u32]) -> i32 {
    let me = std::process::id();
    // SAFETY: console API calls on our own (helper) process. The stdout
    // handle is taken before the console changes and is never closed here.
    unsafe {
        let mut out = ManuallyDrop::new(File::from_raw_handle(GetStdHandle(STD_OUTPUT_HANDLE)));
        FreeConsole();
        if AttachConsole(pid) == 0 {
            return HELPER_NO_CONSOLE;
        }
        let mut list = [0u32; 256];
        let n = GetConsoleProcessList(list.as_mut_ptr(), list.len() as u32) as usize;
        let others: Vec<u32> = list[..n.min(list.len())]
            .iter()
            .copied()
            .filter(|p| *p != me && !allowed.contains(p))
            .collect();
        if n == 0 || n > list.len() || !others.is_empty() {
            FreeConsole();
            let text: Vec<String> = others.iter().map(|p| p.to_string()).collect();
            let _ = writeln!(out, "{}", text.join(" "));
            return HELPER_SHARED;
        }
        SetConsoleCtrlHandler(Some(ignore_ctrl), 1);
        let ok = GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 0);
        FreeConsole();
        if ok == 0 {
            HELPER_SEND_FAILED
        } else {
            HELPER_SENT
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_programs_run_from_a_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let rata = Rata::from_path(tmp.path()).unwrap();
        // The test binary stands in for a program installed next to rata.
        let me = std::env::current_exe().unwrap();
        let stem = me.file_stem().unwrap().to_str().unwrap().to_string();

        let copy = bundled_copy(&stem, &rata).expect("copied");
        assert_eq!(
            copy,
            rata.run_dir().join("bin").join(me.file_name().unwrap())
        );
        assert!(same_file(&me, &copy));
        // Up to date: used as-is. The .exe suffix is optional, in any case.
        let upper = format!("{stem}.EXE");
        assert_eq!(
            bundled_copy(&upper, &rata).unwrap().file_name(),
            copy.file_name()
        );
        assert!(
            !rata
                .run_dir()
                .join("bin")
                .join(format!("{}.tmp", me.file_name().unwrap().to_str().unwrap()))
                .exists()
        );

        // Paths and unknown programs are left alone.
        assert!(bundled_copy(&me.display().to_string(), &rata).is_none());
        assert!(bundled_copy("no-such-program-here", &rata).is_none());
    }
}
