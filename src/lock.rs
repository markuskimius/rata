//! The lock that keeps two rata commands from changing services at once.
//!
//! `start`, `stop`, and `restart` hold an exclusive lock on
//! `$RATA/work/run/rata.lock`. `rata archive` uses its own lock,
//! `archive.lock`, so archiving never holds up service commands. The
//! operating system releases a lock when the file is closed or the process
//! exits, so a crash never leaves a stale lock.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::env::Rata;
use crate::error::{Error, Result};

/// How long to wait for the lock by default.
pub const DEFAULT_LOCK_WAIT: Duration = Duration::from_secs(30);

/// Environment variable that overrides the lock wait, in seconds.
pub const LOCK_WAIT_VAR: &str = "RATA_LOCK_TIMEOUT";

/// A held rata lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
    _file: File,
}

impl Lock {
    /// Take the lock on `$RATA/work/run/rata.lock`, waiting up to `wait`.
    pub fn acquire(rata: &Rata, wait: Duration) -> Result<Lock> {
        Self::acquire_named(rata, "rata.lock", wait)
    }

    /// Take the lock on `$RATA/work/run/<file_name>`, waiting up to `wait`
    /// (zero tries once).
    pub fn acquire_named(rata: &Rata, file_name: &str, wait: Duration) -> Result<Lock> {
        rata.ensure_runtime_dirs()?;
        let path = rata.run_dir().join(file_name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| Error::io(format!("cannot open {}", path.display()), e))?;
        let deadline = Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Lock { path, _file: file }),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(TryLockError::WouldBlock) => return Err(Error::LockBusy(path)),
                Err(TryLockError::Error(e)) => {
                    return Err(Error::io(format!("cannot lock {}", path.display()), e));
                }
            }
        }
    }

    /// The lock's wait limit: `RATA_LOCK_TIMEOUT` seconds if set, else 30s.
    pub fn wait_limit() -> Duration {
        std::env::var(LOCK_WAIT_VAR)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map_or(DEFAULT_LOCK_WAIT, Duration::from_secs)
    }

    /// Path of the lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
