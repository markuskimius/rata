//! The service configuration file, `$RATA/work/cfg/service.toml`.
//!
//! ```toml
//! version = 1                                  # optional; only 1 is accepted
//!
//! [[service]]
//! name           = "WEB_API"                     # required
//! description    = "Web API Server"              # optional
//! start          = "${RATA}/bin/api --port 8080" # required; string or array
//! psline         = "..."                         # optional; defaults to start
//! stop_timeout   = 10                            # optional; seconds; default 10
//! explicit_start = false                         # optional; see Service
//! explicit_stop  = false                         # optional; see Service
//! ```
//!
//! Service names are case-sensitive in the file. On the command line they
//! match case-insensitively when only one service matches (see
//! [`Config::service`]).

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use toml::Spanned;

use crate::cmdline::CommandLine;
use crate::env::Rata;
use crate::error::{Error, Result};

/// The only config format version this release understands.
pub const CONFIG_VERSION: i64 = 1;

/// Seconds `stop` waits for a graceful exit before force-killing.
pub const DEFAULT_STOP_TIMEOUT: u64 = 10;

/// Largest allowed `stop_timeout`, in seconds.
pub const MAX_STOP_TIMEOUT: u64 = 3600;

/// Longest allowed service name.
pub const MAX_NAME_LEN: usize = 64;

/// A command as written in the config: a string to split, or an argument array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum CommandSpec {
    /// A string, split into arguments with the platform's rules.
    Line(String),
    /// An argument array, used as-is.
    Args(Vec<String>),
}

impl<'de> Deserialize<'de> for CommandSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = CommandSpec;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string or an array of strings")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> std::result::Result<CommandSpec, E> {
                Ok(CommandSpec::Line(s.to_string()))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<CommandSpec, A::Error> {
                let mut args = Vec::new();
                while let Some(a) = seq.next_element::<String>()? {
                    args.push(a);
                }
                Ok(CommandSpec::Args(args))
            }
        }
        d.deserialize_any(V)
    }
}

/// One configured service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// Identifier: ASCII letters, digits, and `_`; not starting with a digit.
    pub name: String,
    /// Short human-readable name.
    pub description: Option<String>,
    /// Command run by `rata start`.
    pub start: CommandSpec,
    /// Command line that identifies the running process; defaults to `start`.
    pub psline: Option<CommandSpec>,
    /// Per-service graceful stop timeout, in seconds.
    pub stop_timeout: Option<u64>,
    /// Bare `start` and `restart` skip this service; it starts only when
    /// named or with `--all`.
    pub explicit_start: bool,
    /// Bare `stop` and `restart` skip this service; it stops only when named
    /// or with `--all`.
    pub explicit_stop: bool,
}

impl Service {
    /// The command line used to find this service's processes.
    pub fn match_spec(&self) -> &CommandSpec {
        self.psline.as_ref().unwrap_or(&self.start)
    }

    /// The `start` command, split and expanded.
    pub fn start_command(&self, rata: &Rata) -> CommandLine {
        CommandLine::from_spec(&self.start, rata.root_str())
            .expect("validated when the config was loaded")
    }

    /// The command line processes must match, split and expanded.
    pub fn match_command(&self, rata: &Rata) -> CommandLine {
        CommandLine::from_spec(self.match_spec(), rata.root_str())
            .expect("validated when the config was loaded")
    }

    /// The graceful stop timeout from the config, or the default.
    pub fn stop_timeout(&self) -> Duration {
        Duration::from_secs(self.stop_timeout.unwrap_or(DEFAULT_STOP_TIMEOUT))
    }
}

/// A loaded and validated `service.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Where the config was read from.
    pub path: PathBuf,
    /// Config format version.
    pub version: i64,
    /// Services, in file order.
    pub services: Vec<Service>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: Option<Spanned<i64>>,
    #[serde(default)]
    service: Vec<RawService>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawService {
    name: Spanned<String>,
    description: Option<Spanned<String>>,
    start: Spanned<CommandSpec>,
    psline: Option<Spanned<CommandSpec>>,
    stop_timeout: Option<Spanned<i64>>,
    #[serde(default)]
    explicit_start: bool,
    #[serde(default)]
    explicit_stop: bool,
}

impl Config {
    /// Load `$RATA/work/cfg/service.toml`.
    pub fn load(rata: &Rata) -> Result<Config> {
        Self::load_from(&rata.config_path())
    }

    /// Load a config file from an explicit path.
    pub fn load_from(path: &Path) -> Result<Config> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(Error::ConfigNotFound(path.to_path_buf()));
            }
            Err(e) => {
                return Err(Error::ConfigRead {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        };
        Self::parse(&text, path)
    }

    /// Parse and validate config text. `path` is used in error messages.
    pub fn parse(text: &str, path: &Path) -> Result<Config> {
        let raw: RawConfig = toml::from_str(text).map_err(|e| Error::ConfigParse {
            path: path.to_path_buf(),
            message: e.to_string().trim_end().to_string(),
        })?;
        let invalid = |span: Range<usize>, message: String| {
            let (line, column) = line_col(text, span.start);
            Error::ConfigInvalid {
                path: path.to_path_buf(),
                line,
                column,
                message,
            }
        };

        let version = match raw.version {
            Some(v) if *v.get_ref() != CONFIG_VERSION => {
                return Err(invalid(
                    v.span(),
                    format!(
                        "unsupported config version {} (expected {CONFIG_VERSION})",
                        v.get_ref()
                    ),
                ));
            }
            _ => CONFIG_VERSION,
        };

        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut services = Vec::with_capacity(raw.service.len());
        for s in raw.service {
            let name = s.name.get_ref();
            if let Err(msg) = validate_name(name) {
                return Err(invalid(s.name.span(), msg));
            }
            if let Some(first) = seen.get(name) {
                return Err(invalid(
                    s.name.span(),
                    format!("duplicate service name \"{name}\" (first defined on line {first})"),
                ));
            }
            seen.insert(name.clone(), line_col(text, s.name.span().start).0);

            if let Some(d) = &s.description
                && d.get_ref().contains(['\n', '\r'])
            {
                return Err(invalid(
                    d.span(),
                    format!("service \"{name}\": description must be a single line"),
                ));
            }
            for (field, spec) in [("start", Some(&s.start)), ("psline", s.psline.as_ref())] {
                let Some(spec) = spec else { continue };
                // Validate with a placeholder root; only the shape matters here.
                if let Err(msg) = CommandLine::from_spec(spec.get_ref(), "/") {
                    return Err(invalid(
                        spec.span(),
                        format!("service \"{name}\": {field}: {msg}"),
                    ));
                }
            }
            let stop_timeout = match &s.stop_timeout {
                None => None,
                Some(t) => {
                    let secs = *t.get_ref();
                    if !(1..=MAX_STOP_TIMEOUT as i64).contains(&secs) {
                        return Err(invalid(
                            t.span(),
                            format!(
                                "service \"{name}\": stop_timeout must be between 1 and {MAX_STOP_TIMEOUT} seconds (got {secs})"
                            ),
                        ));
                    }
                    Some(secs as u64)
                }
            };
            services.push(Service {
                name: s.name.into_inner(),
                description: s.description.map(Spanned::into_inner),
                start: s.start.into_inner(),
                psline: s.psline.map(Spanned::into_inner),
                stop_timeout,
                explicit_start: s.explicit_start,
                explicit_stop: s.explicit_stop,
            });
        }
        Ok(Config {
            path: path.to_path_buf(),
            version,
            services,
        })
    }

    /// Look up a service by name: an exact match, or else the only service
    /// whose name matches ignoring ASCII case.
    pub fn service(&self, name: &str) -> Option<&Service> {
        self.lookup(name).ok()
    }

    /// Like [`Config::service`], with an error that says what went wrong.
    pub fn lookup(&self, name: &str) -> Result<&Service> {
        if let Some(s) = self.services.iter().find(|s| s.name == name) {
            return Ok(s);
        }
        let folded: Vec<&Service> = self
            .services
            .iter()
            .filter(|s| s.name.eq_ignore_ascii_case(name))
            .collect();
        match folded.as_slice() {
            [one] => Ok(one),
            [] => Err(Error::UnknownService {
                name: name.to_string(),
                configured: self.services.iter().map(|s| s.name.clone()).collect(),
            }),
            many => Err(Error::AmbiguousService {
                name: name.to_string(),
                matches: many.iter().map(|s| s.name.clone()).collect(),
            }),
        }
    }

    /// The services named in `names`, in config order. An empty list selects
    /// every service. Unknown names are an error.
    pub fn select(&self, names: &[String]) -> Result<Vec<&Service>> {
        if names.is_empty() {
            return Ok(self.services.iter().collect());
        }
        let mut chosen = Vec::with_capacity(names.len());
        for n in names {
            chosen.push(self.lookup(n)?.name.as_str());
        }
        Ok(self
            .services
            .iter()
            .filter(|s| chosen.contains(&s.name.as_str()))
            .collect())
    }

    /// The services a `start`, `stop`, or `restart` acts on, and the ones it
    /// skips.
    ///
    /// Named services are always included. With no names, every service is
    /// included except those whose `explicit_start` / `explicit_stop` flag
    /// applies to `action`, unless `all` is set.
    pub fn select_for(&self, action: Action, names: &[String], all: bool) -> Result<Selection<'_>> {
        let candidates = self.select(names)?;
        if !names.is_empty() || all {
            return Ok(Selection {
                services: candidates,
                skipped: Vec::new(),
            });
        }
        let mut sel = Selection {
            services: Vec::new(),
            skipped: Vec::new(),
        };
        for svc in candidates {
            match action.skip_reason(svc) {
                Some(reason) => sel.skipped.push(Skipped {
                    service: svc,
                    reason,
                }),
                None => sel.services.push(svc),
            }
        }
        Ok(sel)
    }
}

/// A command that changes services.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `rata start`
    Start,
    /// `rata stop`
    Stop,
    /// `rata restart`
    Restart,
}

impl Action {
    /// The command name, for messages.
    pub fn name(self) -> &'static str {
        match self {
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
        }
    }

    /// The flag that makes a bare command skip `svc`, if any.
    pub fn skip_reason(self, svc: &Service) -> Option<SkipReason> {
        let start = svc.explicit_start && matches!(self, Action::Start | Action::Restart);
        let stop = svc.explicit_stop && matches!(self, Action::Stop | Action::Restart);
        if stop {
            Some(SkipReason::ExplicitStop)
        } else if start {
            Some(SkipReason::ExplicitStart)
        } else {
            None
        }
    }
}

/// Why a bare command skipped a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The service sets `explicit_start = true`.
    ExplicitStart,
    /// The service sets `explicit_stop = true`.
    ExplicitStop,
}

impl SkipReason {
    /// The config field, e.g. `explicit_stop`.
    pub fn field(self) -> &'static str {
        match self {
            SkipReason::ExplicitStart => "explicit_start",
            SkipReason::ExplicitStop => "explicit_stop",
        }
    }
}

/// A service that a bare command skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Skipped<'a> {
    /// The service.
    pub service: &'a Service,
    /// Which flag caused the skip.
    pub reason: SkipReason,
}

/// The result of [`Config::select_for`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection<'a> {
    /// Services to act on, in config order.
    pub services: Vec<&'a Service>,
    /// Services skipped because of `explicit_start` / `explicit_stop`, in
    /// config order.
    pub skipped: Vec<Skipped<'a>>,
}

/// Check that `name` is a valid service identifier.
pub fn validate_name(name: &str) -> std::result::Result<(), String> {
    let mut chars = name.chars();
    let ok_first = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_');
    let ok_rest = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok_first || !ok_rest {
        return Err(format!(
            "invalid service name \"{name}\": use letters, digits, and _, not starting with a digit"
        ));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(format!(
            "invalid service name \"{name}\": longer than {MAX_NAME_LEN} characters"
        ));
    }
    Ok(())
}

/// Convert a byte offset into a 1-based (line, column) pair.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, column)
}
