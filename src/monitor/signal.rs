//! Stop requests from `rata stop` and from the operating system.
//!
//! * Unix: SIGTERM (`rata stop`, system shutdown), SIGINT, and SIGHUP.
//! * Windows: CTRL_BREAK (`rata stop`), CTRL_C, and CTRL_CLOSE, CTRL_LOGOFF,
//!   and CTRL_SHUTDOWN. For the last three, Windows ends the process as soon
//!   as the handler returns, so the handler waits (briefly) for the monitor
//!   to finish its current sample.
//!
//! The handlers only set a flag; the monitor checks it between samples.

use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);

/// Whether a stop has been requested.
pub fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// Ask the monitor to stop, as a signal would.
pub fn request_stop() {
    STOP.store(true, Ordering::SeqCst);
}

/// Record that the monitor has finished, so a waiting handler can return.
pub fn mark_done() {
    DONE.store(true, Ordering::SeqCst);
}

/// Install the handlers.
#[cfg(unix)]
pub fn install() {
    extern "C" fn handler(_sig: libc::c_int) {
        STOP.store(true, Ordering::SeqCst);
    }
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic, which is
        // async-signal-safe.
        unsafe {
            libc::signal(sig, handler as *const () as libc::sighandler_t);
        }
    }
}

/// Install the handlers.
#[cfg(windows)]
pub fn install() {
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
        SetConsoleCtrlHandler,
    };

    unsafe extern "system" fn handler(ctrl: u32) -> windows_sys::core::BOOL {
        STOP.store(true, Ordering::SeqCst);
        match ctrl {
            CTRL_C_EVENT | CTRL_BREAK_EVENT => 1,
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
                // Windows allows about 5 seconds before ending the process.
                for _ in 0..40 {
                    if DONE.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                1
            }
            _ => 0,
        }
    }
    // SAFETY: registering a handler that only touches atomics and sleeps.
    unsafe {
        SetConsoleCtrlHandler(Some(handler), 1);
    }
}
