//! Service log files: `$RATA/work/log/<name>_YYYYMMDD_HHMMSS.n.log`.
//!
//! Every start gets a new file. The timestamp is local time; `n` starts at 0
//! and increases while a file with that name already exists.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use jiff::civil::DateTime;

/// The file name for a start of `name` at local wall-clock `time` with counter `n`.
pub fn file_name(name: &str, time: &DateTime, n: u32) -> String {
    format!(
        "{name}_{:04}{:02}{:02}_{:02}{:02}{:02}.{n}.log",
        time.year(),
        time.month(),
        time.day(),
        time.hour(),
        time.minute(),
        time.second()
    )
}

/// Create a new log file for `name` in `dir`, stamped with the current local time.
pub fn create(dir: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    create_at(dir, name, &jiff::ZonedDateTime::now().datetime())
}

/// Create a new log file for `name` in `dir`, stamped with `time`.
///
/// The file is created only if no file with that name exists, so two rata
/// processes can never write to the same log.
pub fn create_at(dir: &Path, name: &str, time: &DateTime) -> io::Result<(PathBuf, File)> {
    for n in 0.. {
        let path = dir.join(file_name(name, time, n));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((path, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("u32 counter exhausted")
}

/// The most recent log file for `name` in `dir`, by timestamp and counter.
pub fn latest(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let file = e.file_name();
            let key = parse(file.to_str()?, name)?;
            Some((key, e.path()))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, p)| p)
}

/// Parse `<name>_YYYYMMDD_HHMMSS.n.log` into a sortable key.
fn parse(file: &str, name: &str) -> Option<(String, String, u64)> {
    let rest = file
        .strip_prefix(name)?
        .strip_prefix('_')?
        .strip_suffix(".log")?;
    let (date, rest) = rest.split_once('_')?;
    let (time, n) = rest.split_once('.')?;
    let digits = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(date, 8)
        || !digits(time, 6)
        || n.is_empty()
        || !n.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((date.to_string(), time.to_string(), n.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: i8) -> DateTime {
        jiff::civil::date(2026, 11, 25).at(14, 32, s, 0)
    }

    #[test]
    fn name_format() {
        assert_eq!(
            file_name("WEB_API", &t(7), 0),
            "WEB_API_20261125_143207.0.log"
        );
    }

    #[test]
    fn counter_increments_on_collision() {
        let dir = tempfile::tempdir().unwrap();
        let (a, _) = create_at(dir.path(), "web", &t(7)).unwrap();
        let (b, _) = create_at(dir.path(), "web", &t(7)).unwrap();
        let (c, _) = create_at(dir.path(), "web", &t(7)).unwrap();
        assert!(a.ends_with("web_20261125_143207.0.log"));
        assert!(b.ends_with("web_20261125_143207.1.log"));
        assert!(c.ends_with("web_20261125_143207.2.log"));
    }

    #[test]
    fn latest_picks_newest_for_that_name_only() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(latest(dir.path(), "web"), None);
        create_at(dir.path(), "web", &t(1)).unwrap();
        for _ in 0..11 {
            create_at(dir.path(), "web", &t(2)).unwrap();
        }
        create_at(dir.path(), "WEB_API", &t(9)).unwrap();
        fs::write(dir.path().join("web_notes.log"), "").unwrap();
        let l = latest(dir.path(), "web").unwrap();
        assert!(l.ends_with("web_20261125_143202.10.log"), "{l:?}");
        let l = latest(dir.path(), "WEB_API").unwrap();
        assert!(l.ends_with("WEB_API_20261125_143209.0.log"));
    }
}
