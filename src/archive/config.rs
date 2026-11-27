//! The archive configuration file, `$RATA/work/cfg/archive.toml`.
//!
//! The file is optional: without it, `rata archive` does nothing. See
//! `docs/init.archive.toml` for every setting.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use jiff::ZonedDateTime;
use jiff::civil::Date;
use serde::Deserialize;
use toml::Spanned;

use super::glob::Glob;
use crate::cmdline::RATA_PLACEHOLDER;
use crate::env::Rata;
use crate::error::{Error, Result};

/// The only archive config format version this release understands.
pub const ARCHIVE_CONFIG_VERSION: i64 = 1;

/// A loaded and validated `archive.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveConfig {
    /// Where the config was read from, or would be.
    pub path: PathBuf,
    /// Whether the file exists. Without it there are no rules.
    pub exists: bool,
    /// The rules, in file order. Each file is handled by the first rule
    /// that matches it.
    pub rules: Vec<Rule>,
}

/// One `[[rule]]`: what to do with matching files in one directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// `path` as written in the config.
    pub path_text: String,
    /// The directory, with `${RATA}` replaced and relative paths resolved
    /// against `$RATA`.
    pub dir: PathBuf,
    /// File name patterns (see [`glob`](super::glob)). A pattern also
    /// matches the name with `.gz` added, so compressed files stay under the
    /// same rule.
    pub patterns: Vec<Glob>,
    /// File name patterns to leave out, even if `patterns` match. These
    /// also match with `.gz` added.
    pub exclude: Vec<Glob>,
    /// Compress files this old with gzip, in place.
    pub compress_after: Option<Age>,
    /// What finally happens to a file in `dir`, if anything.
    pub then: Option<Then>,
}

/// How a file finally leaves a rule's directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Then {
    /// Move files this old to a permanent archive. rata never reads,
    /// changes, or deletes anything there afterwards.
    Move {
        /// The age.
        after: Age,
        /// The destination directory.
        to: Template,
    },
    /// Delete files this old (compressed or not) from the rule's directory.
    Delete {
        /// The age.
        after: Age,
    },
}

/// How old a file must be, by its last-modified time: a number of units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Age {
    /// How many units (at least 1).
    pub n: u32,
    /// The unit.
    pub unit: Unit,
}

/// The unit of an [`Age`]. Every unit but hours counts calendar dates in
/// local time, not elapsed time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Hours since the file was last modified (`h`, `hour`, `hours`).
    Hours,
    /// Calendar days (`d`, `day`, `days`): with 3, a file last modified on
    /// the 24th is due on the 27th.
    Days,
    /// Weeks of 7 calendar days (`w`, `week`, `weeks`).
    Weeks,
    /// Calendar months (`mo`, `month`, `months`): with 1, a file last
    /// modified on Mar 15 is due on Apr 15, and one modified on Jan 31 is
    /// due on Feb 28 (or 29).
    Months,
    /// Calendar years (`y`, `year`, `years`): with 1, a file last modified
    /// on Mar 15 is due on Mar 15 the next year.
    Years,
}

impl Age {
    /// Parse a number and a unit, like `12h`, `3d`, `2w`, `6mo`, `1y`, or
    /// spelled out with or without a space, like `90 days` or `1 year`.
    pub fn parse(text: &str) -> std::result::Result<Age, String> {
        let bad = || {
            format!(
                "invalid age \"{text}\" (expected a number and a unit: h, d, w, mo, or y, like \"3d\" or \"6 months\")"
            )
        };
        let s = text.trim();
        let digits = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        let n: u32 = s[..digits].parse().map_err(|_| bad())?;
        let unit = match s[digits..].trim().to_ascii_lowercase().as_str() {
            "h" | "hour" | "hours" => Unit::Hours,
            "d" | "day" | "days" => Unit::Days,
            "w" | "week" | "weeks" => Unit::Weeks,
            "mo" | "month" | "months" => Unit::Months,
            "y" | "year" | "years" => Unit::Years,
            "m" | "min" | "mins" | "minute" | "minutes" => {
                return Err(format!(
                    "invalid age \"{text}\": \"m\" is not a unit; write \"mo\" for months (minutes are not supported)"
                ));
            }
            _ => return Err(bad()),
        };
        if n == 0 {
            return Err(format!("age must be at least 1 (got \"{text}\")"));
        }
        let max = match unit {
            Unit::Years => 1_000,
            Unit::Months => 12_000,
            Unit::Weeks => 52_000,
            Unit::Days => 365_000,
            Unit::Hours => 8_760_000,
        };
        if n > max {
            return Err(format!("age \"{text}\" is too long (at most 1000 years)"));
        }
        Ok(Age { n, unit })
    }

    /// Whether a file last modified at `mtime` has reached this age at `now`.
    pub fn reached(&self, mtime: &ZonedDateTime, now: &ZonedDateTime) -> bool {
        let n = i64::from(self.n);
        let days = |d: i64| now.date().duration_since(mtime.date()).as_secs() / 86_400 >= d;
        let calendar = |span: std::result::Result<jiff::Span, jiff::Error>| {
            span.ok()
                .and_then(|s| mtime.date().checked_add(s).ok())
                .is_some_and(|due| due <= now.date())
        };
        match self.unit {
            Unit::Hours => now.timestamp().duration_since(mtime.timestamp()).as_secs() >= n * 3600,
            Unit::Days => days(n),
            Unit::Weeks => days(n * 7),
            Unit::Months => calendar(jiff::Span::new().try_months(n)),
            Unit::Years => calendar(jiff::Span::new().try_years(n)),
        }
    }

    /// The shortest and longest this age can be, in hours (a month is 28
    /// to 31 days, a year 365 or 366).
    fn hour_range(&self) -> (u64, u64) {
        let n = u64::from(self.n);
        match self.unit {
            Unit::Hours => (n, n),
            Unit::Days => (n * 24, n * 24),
            Unit::Weeks => (n * 168, n * 168),
            Unit::Months => (n * 28 * 24, n * 31 * 24),
            Unit::Years => (n * 365 * 24, n * 366 * 24),
        }
    }

    /// Calendar months, for months and years (a year is exactly 12 months
    /// on the calendar).
    fn months(&self) -> Option<u64> {
        match self.unit {
            Unit::Months => Some(u64::from(self.n)),
            Unit::Years => Some(u64::from(self.n) * 12),
            _ => None,
        }
    }

    /// Whether this age always comes at or after `other`, so that a file
    /// could never be at least `other` old but younger than this.
    fn never_shorter_than(&self, other: &Age) -> bool {
        match (self.months(), other.months()) {
            (Some(a), Some(b)) => a >= b,
            _ => self.hour_range().0 >= other.hour_range().1,
        }
    }
}

impl std::fmt::Display for Age {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let unit = match self.unit {
            Unit::Hours => "h",
            Unit::Days => "d",
            Unit::Weeks => "w",
            Unit::Months => "mo",
            Unit::Years => "y",
        };
        write!(f, "{}{unit}", self.n)
    }
}

/// A `move_to` directory with `{yyyy}`, `{mm}`, and `{dd}` tokens, filled
/// from the file's last-modified date (local time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    /// `move_to` as written in the config.
    pub text: String,
    parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text(String),
    Year,
    Month,
    Day,
}

impl Template {
    /// Parse `text`, replacing `${RATA}` with `rata_root`. `{{` and `}}`
    /// stand for literal braces.
    pub fn parse(text: &str, rata_root: &str) -> std::result::Result<Template, String> {
        let mut parts = Vec::new();
        let mut lit = String::new();
        let mut rest = text;
        while let Some(c) = rest.chars().next() {
            if let Some(r) = rest.strip_prefix(RATA_PLACEHOLDER) {
                lit.push_str(rata_root);
                rest = r;
            } else if let Some(r) = rest.strip_prefix("{{") {
                lit.push('{');
                rest = r;
            } else if let Some(r) = rest.strip_prefix("}}") {
                lit.push('}');
                rest = r;
            } else if c == '{' {
                let end = rest
                    .find('}')
                    .ok_or_else(|| format!("unclosed {{ in move_to \"{text}\""))?;
                let part = match &rest[..=end] {
                    "{yyyy}" => Part::Year,
                    "{mm}" => Part::Month,
                    "{dd}" => Part::Day,
                    other => {
                        return Err(format!(
                            "unknown token {other} in move_to (expected {{yyyy}}, {{mm}}, or {{dd}}; write {{{{ for a literal {{)"
                        ));
                    }
                };
                if !lit.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut lit)));
                }
                parts.push(part);
                rest = &rest[end + 1..];
            } else if c == '}' {
                return Err(format!(
                    "unmatched }} in move_to \"{text}\" (write }}}} for a literal }})"
                ));
            } else {
                lit.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
        if !lit.is_empty() {
            parts.push(Part::Text(lit));
        }
        Ok(Template {
            text: text.to_string(),
            parts,
        })
    }

    /// The directory for a file last modified on `date`. A relative result
    /// is resolved against `$RATA`.
    pub fn render(&self, date: Date, root: &Path) -> PathBuf {
        let mut s = String::new();
        for p in &self.parts {
            match p {
                Part::Text(t) => s.push_str(t),
                Part::Year => s.push_str(&format!("{:04}", date.year())),
                Part::Month => s.push_str(&format!("{:02}", date.month())),
                Part::Day => s.push_str(&format!("{:02}", date.day())),
            }
        }
        normalize(&root.join(s))
    }

    /// Whether the template has date tokens.
    pub fn has_tokens(&self) -> bool {
        self.parts.iter().any(|p| !matches!(p, Part::Text(_)))
    }
}

impl Rule {
    /// Whether the rule handles a file name: a `match` pattern matches it
    /// and no `exclude` pattern does, each directly or with a trailing `.gz`
    /// removed.
    pub fn matches(&self, name: &str) -> bool {
        let any = |globs: &[Glob]| {
            let m = |n: &str| globs.iter().any(|g| g.matches(n));
            m(name) || name.strip_suffix(".gz").is_some_and(m)
        };
        any(&self.patterns) && !any(&self.exclude)
    }

    /// The `match` patterns as written, joined by spaces, then the `exclude`
    /// ones after `except`.
    pub fn describe_patterns(&self) -> String {
        let join = |g: &[Glob]| {
            g.iter()
                .map(|g| g.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        if self.exclude.is_empty() {
            join(&self.patterns)
        } else {
            format!("{} except {}", join(&self.patterns), join(&self.exclude))
        }
    }

    /// A one-line description: `compress after 3d, delete after 90d`.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(a) = self.compress_after {
            parts.push(format!("compress after {a}"));
        }
        match &self.then {
            Some(Then::Move { after, to }) => {
                parts.push(format!("move after {after} to {}", to.text))
            }
            Some(Then::Delete { after }) => parts.push(format!("delete after {after}")),
            None => {}
        }
        parts.join(", ")
    }
}

impl ArchiveConfig {
    /// Load `$RATA/work/cfg/archive.toml`, or no rules if it is missing.
    pub fn load(rata: &Rata) -> Result<Self> {
        let path = rata.archive_config_path();
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text, &path, rata),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(ArchiveConfig {
                path,
                exists: false,
                rules: Vec::new(),
            }),
            Err(e) => Err(Error::ConfigRead { path, source: e }),
        }
    }

    /// Parse and validate archive config text. `path` is used in messages.
    pub fn parse(text: &str, path: &Path, rata: &Rata) -> Result<Self> {
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
            && *v.get_ref() != ARCHIVE_CONFIG_VERSION
        {
            return Err(invalid(
                v.span().start,
                format!(
                    "unsupported config version {} (expected {ARCHIVE_CONFIG_VERSION})",
                    v.get_ref()
                ),
            ));
        }
        let root = rata.root();
        let mut rules = Vec::new();
        for spanned in &raw.rule {
            let start = spanned.span().start;
            let r = spanned.get_ref();
            let path_text = r.path.get_ref().clone();
            if path_text.trim().is_empty() {
                return Err(invalid(
                    r.path.span().start,
                    "path must not be empty".into(),
                ));
            }
            let dir = normalize(&root.join(path_text.replace(RATA_PLACEHOLDER, rata.root_str())));
            for (forbidden, name) in [(rata.cfg_dir(), "work/cfg"), (rata.run_dir(), "work/run")] {
                if dir == normalize(&forbidden) {
                    return Err(invalid(
                        r.path.span().start,
                        format!("path must not be {name}: rata needs those files"),
                    ));
                }
            }

            if r.patterns.get_ref().is_empty() {
                return Err(invalid(
                    r.patterns.span().start,
                    "match must list at least one file name pattern, like [\"*.log\"]".into(),
                ));
            }
            let globs = |field: &str, list: &Spanned<Vec<String>>| -> Result<Vec<Glob>> {
                list.get_ref()
                    .iter()
                    .map(|p| {
                        if p.is_empty() || p.contains('/') {
                            return Err(invalid(
                                list.span().start,
                                format!(
                                    "{field} patterns are file names, not paths (got \"{p}\"); put the directory in path"
                                ),
                            ));
                        }
                        Glob::new(p).map_err(|m| invalid(list.span().start, format!("{field}: {m}")))
                    })
                    .collect()
            };
            let patterns = globs("match", &r.patterns)?;
            let exclude = match &r.exclude {
                Some(list) => globs("exclude", list)?,
                None => Vec::new(),
            };

            let age = |s: &Option<Spanned<String>>| -> Result<Option<(Age, usize)>> {
                match s {
                    None => Ok(None),
                    Some(s) => Age::parse(s.get_ref())
                        .map(|a| Some((a, s.span().start)))
                        .map_err(|m| invalid(s.span().start, m)),
                }
            };
            let compress = age(&r.compress_after)?;
            let mv = age(&r.move_after)?;
            let delete = age(&r.delete_after)?;

            if let (Some(_), Some((_, at))) = (mv, delete) {
                return Err(invalid(
                    at,
                    "a rule can't set both move_after and delete_after: moved files are permanent, and rata never deletes from move_to".into(),
                ));
            }
            let then = match (mv, &r.move_to, delete) {
                (Some((after, _)), Some(to), _) => {
                    let t = Template::parse(to.get_ref(), rata.root_str())
                        .map_err(|m| invalid(to.span().start, m))?;
                    if to.get_ref().trim().is_empty() {
                        return Err(invalid(to.span().start, "move_to must not be empty".into()));
                    }
                    if !t.has_tokens() && t.render(Date::default(), root) == dir {
                        return Err(invalid(
                            to.span().start,
                            "move_to must be a different directory from path".into(),
                        ));
                    }
                    Some(Then::Move { after, to: t })
                }
                (Some((_, at)), None, _) => {
                    return Err(invalid(at, "move_after needs move_to".into()));
                }
                (None, Some(to), _) => {
                    return Err(invalid(to.span().start, "move_to needs move_after".into()));
                }
                (None, None, Some((after, _))) => Some(Then::Delete { after }),
                (None, None, None) => None,
            };
            let Some(_) = compress.or(mv).or(delete) else {
                return Err(invalid(
                    start,
                    "a rule needs at least one of compress_after, move_after, or delete_after"
                        .into(),
                ));
            };
            if let (Some((c, at)), Some(t)) = (compress, &then) {
                let (after, what) = match t {
                    Then::Move { after, .. } => (after, "move_after"),
                    Then::Delete { after } => (after, "delete_after"),
                };
                if c.never_shorter_than(after) {
                    return Err(invalid(
                        at,
                        format!(
                            "compress_after ({c}) must be shorter than {what} ({after}), or nothing would be compressed"
                        ),
                    ));
                }
            }
            rules.push(Rule {
                path_text,
                dir,
                patterns,
                exclude,
                compress_after: compress.map(|(a, _)| a),
                then,
            });
        }
        Ok(ArchiveConfig {
            path: path.to_path_buf(),
            exists: true,
            rules,
        })
    }
}

/// Remove `.` and resolve `..` without touching the file system.
pub(crate) fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(c);
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    version: Option<Spanned<i64>>,
    #[serde(default)]
    rule: Vec<Spanned<RawRule>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    path: Spanned<String>,
    #[serde(rename = "match")]
    patterns: Spanned<Vec<String>>,
    exclude: Option<Spanned<Vec<String>>>,
    compress_after: Option<Spanned<String>>,
    move_after: Option<Spanned<String>>,
    move_to: Option<Spanned<String>>,
    delete_after: Option<Spanned<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rata() -> (tempfile::TempDir, Rata) {
        let tmp = tempfile::tempdir().unwrap();
        let r = Rata::from_path(tmp.path()).unwrap();
        (tmp, r)
    }

    fn parse(text: &str) -> Result<ArchiveConfig> {
        let (_t, r) = rata();
        ArchiveConfig::parse(text, Path::new("archive.toml"), &r)
    }

    fn err(text: &str) -> String {
        parse(text).unwrap_err().to_string()
    }

    #[test]
    fn init_template_is_valid() {
        let c = parse(crate::init::ARCHIVE_TEMPLATE).unwrap();
        assert_eq!(c.rules.len(), 2);
        for r in &c.rules {
            assert_eq!(r.compress_after, Some(days(3)));
            assert_eq!(r.then, Some(Then::Delete { after: days(90) }));
        }
        assert_eq!(c.rules[0].describe(), "compress after 3d, delete after 90d");
    }

    fn days(n: u32) -> Age {
        Age {
            n,
            unit: Unit::Days,
        }
    }

    #[test]
    fn ages() {
        let age = |n, unit| Ok(Age { n, unit });
        assert_eq!(Age::parse("12h"), age(12, Unit::Hours));
        assert_eq!(Age::parse("3d"), age(3, Unit::Days));
        assert_eq!(Age::parse("2w"), age(2, Unit::Weeks));
        assert_eq!(Age::parse("6mo"), age(6, Unit::Months));
        assert_eq!(Age::parse("1y"), age(1, Unit::Years));
        assert_eq!(Age::parse("90 days"), age(90, Unit::Days));
        assert_eq!(Age::parse("1 Year"), age(1, Unit::Years));
        assert_eq!(Age::parse("3months"), age(3, Unit::Months));
        assert_eq!(Age::parse(" 1 hour "), age(1, Unit::Hours));
        assert!(Age::parse("0d").is_err());
        assert!(Age::parse("3").is_err());
        assert!(Age::parse("d").is_err());
        assert!(Age::parse("-3d").is_err());
        assert!(Age::parse("3 fortnights").is_err());
        assert!(Age::parse("2000y").unwrap_err().contains("too long"));
        assert!(
            Age::parse("3m")
                .unwrap_err()
                .contains("write \"mo\" for months")
        );
        assert!(Age::parse("").is_err());
        assert_eq!(Age::parse("6 months").unwrap().to_string(), "6mo");

        let tz = jiff::tz::TimeZone::fixed(jiff::tz::Offset::constant(-4));
        let at = |y: i16, m: i8, d: i8, h: i8| {
            jiff::civil::date(y, m, d)
                .at(h, 0, 0, 0)
                .to_zoned_datetime(tz.clone())
                .unwrap()
        };
        let p = |s| Age::parse(s).unwrap();
        // Calendar days: late on the 24th is 3 days old early on the 27th.
        assert!(p("3d").reached(&at(2026, 11, 24, 23), &at(2026, 11, 27, 1)));
        assert!(!p("3d").reached(&at(2026, 11, 25, 0), &at(2026, 11, 27, 23)));
        assert!(p("1w").reached(&at(2026, 11, 20, 0), &at(2026, 11, 27, 0)));
        assert!(!p("1w").reached(&at(2026, 11, 21, 0), &at(2026, 11, 27, 23)));
        assert!(p("2h").reached(&at(2026, 11, 27, 1), &at(2026, 11, 27, 3)));
        assert!(!p("3h").reached(&at(2026, 11, 27, 1), &at(2026, 11, 27, 3)));
        // Calendar months, clamped to the end of shorter months.
        assert!(p("1mo").reached(&at(2026, 3, 15, 23), &at(2026, 4, 15, 0)));
        assert!(!p("1mo").reached(&at(2026, 3, 15, 0), &at(2026, 4, 14, 23)));
        assert!(p("1mo").reached(&at(2026, 1, 31, 0), &at(2026, 2, 28, 0)));
        assert!(!p("1mo").reached(&at(2026, 1, 31, 0), &at(2026, 2, 27, 0)));
        assert!(p("1y").reached(&at(2025, 11, 27, 0), &at(2026, 11, 27, 0)));
        assert!(!p("1y").reached(&at(2025, 11, 28, 0), &at(2026, 11, 27, 0)));
        assert!(p("1y").reached(&at(2024, 2, 29, 0), &at(2025, 2, 28, 0)));
    }

    #[test]
    fn templates() {
        let t = Template::parse("${RATA}/a/{yyyy}/{mm}-{dd}/{{x}}", "/r").unwrap();
        assert!(t.has_tokens());
        let d = jiff::civil::date(2026, 3, 7);
        assert_eq!(
            t.render(d, Path::new("/elsewhere")),
            Path::new("/r/a/2026/03-07/{x}")
        );
        let rel = Template::parse("archive/{yyyy}", "/r").unwrap();
        assert_eq!(rel.render(d, Path::new("/r")), Path::new("/r/archive/2026"));
        assert!(
            Template::parse("a/{yy}", "/r")
                .unwrap_err()
                .contains("unknown token {yy}")
        );
        assert!(
            Template::parse("a/{yyyy", "/r")
                .unwrap_err()
                .contains("unclosed")
        );
        assert!(
            Template::parse("a/}", "/r")
                .unwrap_err()
                .contains("unmatched }")
        );
    }

    #[test]
    fn matching() {
        let c = parse(
            "[[rule]]\npath = \"work/log\"\nmatch = [\"*.log\", \"x?\"]\nexclude = [\"RATA_*\"]\ncompress_after = \"1d\"\n",
        )
        .unwrap();
        let r = &c.rules[0];
        assert!(r.matches("a.log"));
        assert!(r.matches("a.log.gz"));
        assert!(r.matches("xy.gz"));
        assert!(!r.matches("a.log.1"));
        assert!(!r.matches("a.gz"));
        assert!(!r.matches("RATA_MONITOR_20261127_000000.0.log"));
        assert!(!r.matches("RATA_MONITOR_20261127_000000.0.log.gz"));
        assert_eq!(r.describe_patterns(), "*.log x? except RATA_*");
        assert!(r.dir.ends_with(Path::new("work").join("log")));
    }

    #[test]
    fn errors() {
        let rule =
            |body: &str| format!("[[rule]]\npath = \"work/log\"\nmatch = [\"*.log\"]\n{body}");
        assert!(err(&rule("")).contains("needs at least one of"));
        assert!(
            err(&rule(
                "move_after = \"9d\"\nmove_to = \"a\"\ndelete_after = \"9d\"\n"
            ))
            .contains("can't set both move_after and delete_after")
        );
        assert!(err(&rule("move_after = \"9d\"\n")).contains("move_after needs move_to"));
        assert!(err(&rule("move_to = \"a\"\n")).contains("move_to needs move_after"));
        assert!(
            err(&rule(
                "move_after = \"9d\"\nmove_to = \"${RATA}/work/log\"\n"
            ))
            .contains("different directory from path")
        );
        assert_eq!(
            err(&rule("compress_after = \"2w\"\ndelete_after = \"14d\"\n")),
            "archive.toml:4:18: compress_after (2w) must be shorter than delete_after (14d), or nothing would be compressed"
        );
        // A month is 28 to 31 days: only reject what can never work.
        assert!(parse(&rule("compress_after = \"30d\"\ndelete_after = \"1mo\"\n")).is_ok());
        assert!(
            err(&rule("compress_after = \"2mo\"\ndelete_after = \"40d\"\n"))
                .contains("must be shorter")
        );
        assert!(
            err(&rule("compress_after = \"1y\"\ndelete_after = \"12mo\"\n"))
                .contains("must be shorter")
        );
        assert!(
            parse(&rule(
                "compress_after = \"11mo\"\ndelete_after = \"1 year\"\n"
            ))
            .is_ok()
        );
        assert!(
            err(&rule("exclude = [\"a[b\"]\ndelete_after = \"1d\"\n"))
                .contains("exclude: pattern \"a[b\": unclosed [")
        );
        assert!(
            err("[[rule]]\npath = \"work/log\"\nmatch = [\"*.{log\"]\ndelete_after = \"1d\"\n")
                .contains("match: pattern \"*.{log\": unclosed {")
        );
        assert!(err(&rule("delete_after = \"soon\"\n")).contains("invalid age \"soon\""));
        assert!(err(&rule("delet_after = \"9d\"\n")).contains("unknown field `delet_after`"));
        assert!(
            err("[[rule]]\npath = \"work/cfg\"\nmatch = [\"*\"]\ndelete_after = \"1d\"\n")
                .contains("must not be work/cfg")
        );
        assert!(
            err("[[rule]]\npath = \"work/log\"\nmatch = []\ndelete_after = \"1d\"\n")
                .contains("at least one file name pattern")
        );
        assert!(
            err("[[rule]]\npath = \"work\"\nmatch = [\"log/*\"]\ndelete_after = \"1d\"\n")
                .contains("file names, not paths")
        );
        assert!(
            err("[[rule]]\nmatch = [\"*\"]\ndelete_after = \"1d\"\n")
                .contains("missing field `path`")
        );
        assert!(err("version = 2\n").contains("unsupported config version 2"));
    }
}
