//! The `RATA` environment variable and the directory layout under it.
//!
//! ```text
//! $RATA/
//! └── work/
//!     ├── cfg/service.toml   service configuration
//!     ├── cfg/monitor.toml   monitor configuration (optional)
//!     ├── log/               one log file per service start
//!     ├── monitor/           stats recorded by rata-monitor
//!     └── run/               lock file
//! ```

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Name of the environment variable that holds the rata root directory.
pub const RATA_VAR: &str = "RATA";

/// A validated rata root directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rata {
    root: PathBuf,
    root_str: String,
}

impl Rata {
    /// Read and validate the `RATA` environment variable.
    pub fn from_env() -> Result<Self> {
        Self::from_var(std::env::var_os(RATA_VAR))
    }

    /// Validate a value as if it were the `RATA` environment variable.
    ///
    /// `None` or an empty value means "not set".
    pub fn from_var(value: Option<OsString>) -> Result<Self> {
        match value {
            Some(v) if !v.is_empty() => Self::from_path(PathBuf::from(v)),
            _ => Err(Error::RataNotSet),
        }
    }

    /// Validate a path as a rata root: absolute, Unicode, an existing directory.
    pub fn from_path(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let root_str = root
            .to_str()
            .ok_or_else(|| Error::RataNotUnicode(root.clone()))?
            .to_string();
        if !root.is_absolute() {
            return Err(Error::RataNotAbsolute(root_str));
        }
        match fs::metadata(&root) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err(Error::RataNotDir(root)),
            Err(_) => return Err(Error::RataMissing(root)),
        }
        Ok(Rata { root, root_str })
    }

    /// The root directory, exactly as given.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The root directory as a string. This is the text `${RATA}` expands to.
    pub fn root_str(&self) -> &str {
        &self.root_str
    }

    /// `$RATA/work`
    pub fn work_dir(&self) -> PathBuf {
        self.root.join("work")
    }

    /// `$RATA/work/cfg`
    pub fn cfg_dir(&self) -> PathBuf {
        self.work_dir().join("cfg")
    }

    /// `$RATA/work/log`
    pub fn log_dir(&self) -> PathBuf {
        self.work_dir().join("log")
    }

    /// `$RATA/work/run`
    pub fn run_dir(&self) -> PathBuf {
        self.work_dir().join("run")
    }

    /// `$RATA/work/monitor`
    pub fn monitor_dir(&self) -> PathBuf {
        self.work_dir().join("monitor")
    }

    /// `$RATA/work/cfg/service.toml`
    pub fn config_path(&self) -> PathBuf {
        self.cfg_dir().join("service.toml")
    }

    /// `$RATA/work/cfg/monitor.toml`
    pub fn monitor_config_path(&self) -> PathBuf {
        self.cfg_dir().join("monitor.toml")
    }

    /// Create `work/log` and `work/run` if they are missing.
    pub fn ensure_runtime_dirs(&self) -> Result<()> {
        for dir in [self.log_dir(), self.run_dir()] {
            fs::create_dir_all(&dir)
                .map_err(|e| Error::io(format!("cannot create {}", dir.display()), e))?;
        }
        Ok(())
    }

    /// Show `path` relative to `$RATA` when it is inside it, for shorter output.
    pub fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_and_empty() {
        assert!(matches!(Rata::from_var(None), Err(Error::RataNotSet)));
        assert!(matches!(
            Rata::from_var(Some(OsString::new())),
            Err(Error::RataNotSet)
        ));
    }

    #[test]
    fn relative() {
        assert!(matches!(
            Rata::from_path("rata"),
            Err(Error::RataNotAbsolute(s)) if s == "rata"
        ));
    }

    #[test]
    fn missing_and_not_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope");
        assert!(matches!(
            Rata::from_path(&missing),
            Err(Error::RataMissing(_))
        ));
        let file = tmp.path().join("file");
        fs::write(&file, "").unwrap();
        assert!(matches!(Rata::from_path(&file), Err(Error::RataNotDir(_))));
    }

    #[test]
    fn layout() {
        let tmp = tempfile::tempdir().unwrap();
        let rata = Rata::from_path(tmp.path()).unwrap();
        assert_eq!(
            rata.config_path(),
            tmp.path().join("work").join("cfg").join("service.toml")
        );
        rata.ensure_runtime_dirs().unwrap();
        assert!(rata.log_dir().is_dir());
        assert!(rata.run_dir().is_dir());
        assert_eq!(
            rata.display_path(&rata.log_dir().join("x.log")),
            Path::new("work")
                .join("log")
                .join("x.log")
                .display()
                .to_string()
        );
    }
}
