//! The monitor configuration file, `$RATA/work/cfg/monitor.toml`.
//!
//! The file is optional; see `docs/monitor.example.toml` for every setting.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use toml::Spanned;

use crate::env::Rata;
use crate::error::{Error, Result};

/// The only monitor config format version this release understands.
pub const MONITOR_CONFIG_VERSION: i64 = 1;

/// Seconds between samples by default.
pub const DEFAULT_INTERVAL: u64 = 30;

/// Smallest allowed `interval`, in seconds.
pub const MIN_INTERVAL: u64 = 5;

/// Largest allowed `interval`, in seconds.
pub const MAX_INTERVAL: u64 = 3600;

/// A loaded and validated `monitor.toml` (or the defaults, without one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorConfig {
    /// Where the config was read from, or would be.
    pub path: PathBuf,
    /// Whether the file exists. Without it, every setting is the default.
    pub exists: bool,
    /// Time between samples.
    pub interval: Duration,
    /// Whether to record host stats (system, disks, network).
    pub system: bool,
    /// Glob patterns of mount points to leave out.
    pub disk_exclude: Vec<String>,
    /// Glob patterns of network interfaces to leave out.
    pub net_exclude: Vec<String>,
}

impl MonitorConfig {
    /// The defaults, for a config file at `path` that does not exist.
    pub fn defaults(path: &Path) -> Self {
        MonitorConfig {
            path: path.to_path_buf(),
            exists: false,
            interval: Duration::from_secs(DEFAULT_INTERVAL),
            system: true,
            disk_exclude: Vec::new(),
            net_exclude: Vec::new(),
        }
    }

    /// Load `$RATA/work/cfg/monitor.toml`, or the defaults if it is missing.
    pub fn load(rata: &Rata) -> Result<Self> {
        Self::load_from(&rata.monitor_config_path())
    }

    /// Load a monitor config from an explicit path, or the defaults if it is
    /// missing.
    pub fn load_from(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::defaults(path)),
            Err(e) => Err(Error::ConfigRead {
                path: path.to_path_buf(),
                source: e,
            }),
        }
    }

    /// Parse and validate monitor config text. `path` is used in messages.
    pub fn parse(text: &str, path: &Path) -> Result<Self> {
        let raw: Raw = toml::from_str(text).map_err(|e| Error::ConfigParse {
            path: path.to_path_buf(),
            message: e.to_string().trim_end().to_string(),
        })?;
        let invalid = |offset: usize, message: String| {
            let (line, column) = crate::config::line_col(text, offset);
            Error::ConfigInvalid {
                path: path.to_path_buf(),
                line,
                column,
                message,
            }
        };
        if let Some(v) = &raw.version
            && *v.get_ref() != MONITOR_CONFIG_VERSION
        {
            return Err(invalid(
                v.span().start,
                format!(
                    "unsupported config version {} (expected {MONITOR_CONFIG_VERSION})",
                    v.get_ref()
                ),
            ));
        }
        let interval = match &raw.interval {
            None => DEFAULT_INTERVAL,
            Some(i) => {
                let secs = *i.get_ref();
                if !(MIN_INTERVAL as i64..=MAX_INTERVAL as i64).contains(&secs) {
                    return Err(invalid(
                        i.span().start,
                        format!(
                            "interval must be between {MIN_INTERVAL} and {MAX_INTERVAL} seconds (got {secs})"
                        ),
                    ));
                }
                secs as u64
            }
        };
        Ok(MonitorConfig {
            path: path.to_path_buf(),
            exists: true,
            interval: Duration::from_secs(interval),
            system: raw.system.unwrap_or(true),
            disk_exclude: raw.disks.exclude,
            net_exclude: raw.net.exclude,
        })
    }

    /// Whether `mount` matches one of the `[disks] exclude` patterns.
    pub fn excludes_disk(&self, mount: &str) -> bool {
        self.disk_exclude.iter().any(|p| glob_match(p, mount))
    }

    /// Whether `iface` matches one of the `[net] exclude` patterns.
    pub fn excludes_net(&self, iface: &str) -> bool {
        self.net_exclude.iter().any(|p| glob_match(p, iface))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    version: Option<Spanned<i64>>,
    interval: Option<Spanned<i64>>,
    system: Option<bool>,
    #[serde(default)]
    disks: RawExclude,
    #[serde(default)]
    net: RawExclude,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawExclude {
    #[serde(default)]
    exclude: Vec<String>,
}

/// Match `text` against a glob `pattern`: `*` matches any run of
/// characters (including none), `?` matches one character, and everything
/// else matches itself. Case matters.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<MonitorConfig> {
        MonitorConfig::parse(text, Path::new("monitor.toml"))
    }

    #[test]
    fn defaults_and_full() {
        let d = parse("").unwrap();
        assert_eq!(d.interval, Duration::from_secs(30));
        assert!(d.system);
        let c = parse(
            "version = 1\ninterval = 60\nsystem = false\n[disks]\nexclude = [\"/Volumes/*\"]\n[net]\nexclude = [\"utun*\"]\n",
        )
        .unwrap();
        assert_eq!(c.interval, Duration::from_secs(60));
        assert!(!c.system);
        assert!(c.excludes_disk("/Volumes/Backup"));
        assert!(!c.excludes_disk("/"));
        assert!(c.excludes_net("utun3"));
    }

    #[test]
    fn example_file_is_valid_and_matches_defaults() {
        let c = parse(include_str!("../../docs/monitor.example.toml")).unwrap();
        let d = MonitorConfig::defaults(Path::new("monitor.toml"));
        assert_eq!((c.interval, c.system), (d.interval, d.system));
    }

    #[test]
    fn errors() {
        let e = parse("interval = 2\n").unwrap_err().to_string();
        assert_eq!(
            e,
            "monitor.toml:1:12: interval must be between 5 and 3600 seconds (got 2)"
        );
        assert!(
            parse("intervl = 30\n")
                .unwrap_err()
                .to_string()
                .contains("unknown field `intervl`")
        );
        assert!(
            parse("version = 2\n")
                .unwrap_err()
                .to_string()
                .contains("unsupported config version 2")
        );
        assert!(
            parse("[disks]\nexclud = []\n")
                .unwrap_err()
                .to_string()
                .contains("unknown field `exclud`")
        );
    }

    #[test]
    fn globs() {
        assert!(glob_match("*", ""));
        assert!(glob_match("a*c", "abbbc"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(glob_match("/System/Volumes/*", "/System/Volumes/VM"));
        assert!(!glob_match("lo", "lo0"));
        assert!(glob_match("*.*", "a.b"));
        assert!(!glob_match("abc", "ABC"));
    }
}
