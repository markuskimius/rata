//! TEST HELPER — not a usage example.
//!
//! A fake long-running service used by rata's end-to-end tests. It lives in
//! `examples/` because `cargo test` builds examples automatically, while
//! `cargo install rata` and release builds never ship them.
//!
//! Usage: testsvc [--tag <s>] [--ignore-term] [--exit-now <code>] [--exit-after <secs>]
//!
//! * `--tag <s>`        ignored; makes the command line unique per test
//! * `--ignore-term`    ignore SIGTERM (Unix) / CTRL_BREAK (Windows)
//! * `--exit-now <c>`   exit immediately with code `c`
//! * `--exit-after <s>` exit with code 0 after `s` seconds

use std::io::Write;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut exit_after = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--tag" => i += 1,
            "--ignore-term" => ignore_term(),
            "--exit-now" => {
                let code = args.get(i + 1).and_then(|c| c.parse().ok()).unwrap_or(1);
                println!("testsvc exiting with code {code}");
                std::process::exit(code);
            }
            "--exit-after" => {
                exit_after = args.get(i + 1).and_then(|s| s.parse::<u64>().ok());
                i += 1;
            }
            other => {
                eprintln!("testsvc: unknown argument {other}");
                std::process::exit(64);
            }
        }
        i += 1;
    }
    println!("testsvc started pid {}", std::process::id());
    let _ = std::io::stdout().flush();
    let start = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(100));
        if exit_after.is_some_and(|s| start.elapsed() >= Duration::from_secs(s)) {
            return;
        }
    }
}

#[cfg(unix)]
fn ignore_term() {
    // SAFETY: installing SIG_IGN for SIGTERM.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
}

#[cfg(windows)]
fn ignore_term() {
    unsafe extern "system" fn handler(_ctrl: u32) -> windows_sys::core::BOOL {
        1
    }
    // SAFETY: registering a handler that ignores every console control event.
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handler), 1);
    }
}
