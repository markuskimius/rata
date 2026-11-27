//! Error types shared by every rata module.

use std::io;
use std::path::PathBuf;

/// Convenience alias for results returned by rata.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors that stop a rata command before it acts on any service.
///
/// Every variant maps to exit code 2 in the `rata` CLI.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `RATA` is not set (or is empty).
    #[error(
        "the RATA environment variable is not set\n\
         Set RATA to the root directory of your rata install, for example:\n  \
         export RATA=/opt/rata          (bash/zsh)\n  \
         $env:RATA = \"C:\\rata\"          (PowerShell)"
    )]
    RataNotSet,

    /// `RATA` is set to a relative path.
    #[error("RATA must be an absolute path (got \"{0}\")")]
    RataNotAbsolute(String),

    /// `RATA` contains characters that are not valid Unicode.
    #[error("RATA must be valid Unicode (got {0:?})")]
    RataNotUnicode(PathBuf),

    /// `RATA` points at something that does not exist.
    #[error("RATA directory does not exist: {}", .0.display())]
    RataMissing(PathBuf),

    /// `RATA` points at something that is not a directory.
    #[error("RATA is not a directory: {}", .0.display())]
    RataNotDir(PathBuf),

    /// `$RATA/work/cfg/service.toml` does not exist.
    #[error("config file not found: {}", .0.display())]
    ConfigNotFound(PathBuf),

    /// The config file exists but could not be read.
    #[error("cannot read config file {}: {source}", path.display())]
    ConfigRead {
        /// Path of the config file.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },

    /// The config file is not valid TOML, or does not match the expected shape.
    #[error("{}: {message}", path.display())]
    ConfigParse {
        /// Path of the config file.
        path: PathBuf,
        /// Parser message, including line and column.
        message: String,
    },

    /// The config file parsed, but a value is not allowed.
    #[error("{}:{line}:{column}: {message}", path.display())]
    ConfigInvalid {
        /// Path of the config file.
        path: PathBuf,
        /// 1-based line of the offending value.
        line: usize,
        /// 1-based column of the offending value.
        column: usize,
        /// What is wrong.
        message: String,
    },

    /// A service named on the command line is not in the config.
    #[error("unknown service \"{name}\" (configured: {})", if configured.is_empty() { "none".to_string() } else { configured.join(", ") })]
    UnknownService {
        /// The name that was asked for.
        name: String,
        /// All configured service names, in config order.
        configured: Vec<String>,
    },

    /// A service named on the command line matches several services when
    /// case is ignored, and none exactly.
    #[error("service name \"{name}\" is ambiguous: it matches {} (type the name exactly)", matches.join(", "))]
    AmbiguousService {
        /// The name that was asked for.
        name: String,
        /// The services it matches ignoring case, in config order.
        matches: Vec<String>,
    },

    /// Another rata command held the lock for longer than the wait limit.
    #[error("another rata command is running (lock: {})", .0.display())]
    LockBusy(PathBuf),

    /// Any other I/O failure, with a description of what rata was doing.
    #[error("{context}: {source}")]
    Io {
        /// What rata was doing.
        context: String,
        /// Underlying I/O error.
        source: io::Error,
    },
}

impl Error {
    /// Wrap an I/O error with a description of what rata was doing.
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }
}
