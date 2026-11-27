//! Archiving old files (`rata archive`).
//!
//! Rules in `$RATA/work/cfg/archive.toml` ([`ArchiveConfig`]) say, for each
//! directory, which files to compress and whether to finally move them to
//! a permanent archive or delete them. Ages are measured from each file's
//! last-modified time, which compressing and moving keep.
//!
//! Every action applies only to files in a rule's `path`. rata never
//! reads, changes, or deletes anything under `move_to` after a move.
//!
//! The steps are safe to interrupt. Compressing writes
//! `<name>.gz.rata-tmp`, renames it to `<name>.gz`, then removes the
//! original. A move across file systems copies to `<name>.rata-tmp` in the
//! destination first. A later run finishes or redoes whatever an
//! interrupted run left.
//!
//! Files that may still be written are never touched, whatever their age:
//! the current log of each running service and today's monitor files.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use jiff::ZonedDateTime;

use crate::config::Config;
use crate::env::Rata;
use crate::error::{Error, Result};
use crate::lock::Lock;

pub mod config;
pub mod glob;

pub use config::{Age, ArchiveConfig, Rule, Template, Then, Unit};
pub use glob::Glob;

/// Suffix of the temporary files written while compressing or copying.
pub const TMP_SUFFIX: &str = ".rata-tmp";

/// File name of the archive lock in `$RATA/work/run`.
pub const LOCK_FILE: &str = "archive.lock";

/// What to do with one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Compress it in place.
    Compress,
    /// Compress it, then move the `.gz` to the directory.
    CompressAndMove(PathBuf),
    /// Move it (already compressed, or the rule does not compress) to the
    /// directory.
    Move(PathBuf),
    /// Delete it.
    Delete,
}

/// One planned or completed action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The file in the rule's directory.
    pub path: PathBuf,
    /// What was (or, in a dry run, would be) done.
    pub action: Action,
    /// The compressed file, if compressing was part of the action.
    pub compressed: Option<PathBuf>,
    /// Where the file ended up, if it was moved.
    pub moved_to: Option<PathBuf>,
    /// Why the action failed, if it did. The file is left as it was, or as
    /// far as the action got (for example compressed but not moved).
    pub error: Option<String>,
}

impl Entry {
    /// Whether the action succeeded (or, in a dry run, was planned).
    pub fn ok(&self) -> bool {
        self.error.is_none()
    }
}

/// What a run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveReport {
    /// Every action, in the order done.
    pub entries: Vec<Entry>,
    /// Problems that kept a whole directory from being looked at, and
    /// leftover temporary files that could not be removed.
    pub problems: Vec<String>,
    /// Rule directories that do not exist (and so were skipped).
    pub missing_dirs: Vec<PathBuf>,
}

impl ArchiveReport {
    /// Whether everything succeeded.
    pub fn ok(&self) -> bool {
        self.problems.is_empty() && self.entries.iter().all(Entry::ok)
    }
}

/// Files that may still be written, which archiving never touches: the
/// current log of each running service, and today's monitor files.
pub fn protected_files(rata: &Rata, config: &Config) -> Vec<PathBuf> {
    let mut v = crate::service::current_logs(rata, config);
    v.extend(crate::monitor::store::live_files(&rata.monitor_dir()));
    v
}

/// Take the archive lock (`$RATA/work/run/archive.lock`) without waiting.
pub fn lock(rata: &Rata) -> Result<Lock> {
    match Lock::acquire_named(rata, LOCK_FILE, Duration::ZERO) {
        Err(Error::LockBusy(p)) => Err(Error::ArchiveBusy(p)),
        r => r,
    }
}

/// Decide what a rule does with a file last modified at `mtime`. `name` is
/// the file's name; a name ending in `.gz` counts as compressed.
pub fn decide(
    rule: &Rule,
    name: &str,
    mtime: &ZonedDateTime,
    now: &ZonedDateTime,
    root: &Path,
) -> Option<Action> {
    let compressed = name.ends_with(".gz");
    match &rule.then {
        Some(Then::Delete { after }) if after.reached(mtime, now) => return Some(Action::Delete),
        Some(Then::Move { after, to }) if after.reached(mtime, now) => {
            let dir = to.render(mtime.date(), root);
            return Some(if rule.compress_after.is_some() && !compressed {
                Action::CompressAndMove(dir)
            } else {
                Action::Move(dir)
            });
        }
        _ => {}
    }
    match rule.compress_after {
        Some(c) if !compressed && c.reached(mtime, now) => Some(Action::Compress),
        _ => None,
    }
}

/// Plan, and unless `dry_run`, carry out every rule at time `now`.
/// `on_entry` is called after each action.
pub fn run(
    rata: &Rata,
    config: &ArchiveConfig,
    protected: &[PathBuf],
    now: &ZonedDateTime,
    dry_run: bool,
    mut on_entry: impl FnMut(&Entry),
) -> ArchiveReport {
    let protected: HashSet<PathBuf> = protected
        .iter()
        .map(|p| fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect();
    let mut claimed: HashSet<PathBuf> = HashSet::new();
    let mut report = ArchiveReport::default();
    for rule in &config.rules {
        let dir = match fs::canonicalize(&rule.dir) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                report.missing_dirs.push(rule.dir.clone());
                continue;
            }
            Err(e) => {
                report
                    .problems
                    .push(format!("cannot read {}: {e}", rata.display_path(&rule.dir)));
                continue;
            }
        };
        let files = match list_files(&dir) {
            Ok(f) => f,
            Err(e) => {
                report
                    .problems
                    .push(format!("cannot read {}: {e}", rata.display_path(&rule.dir)));
                continue;
            }
        };
        for (name, mtime) in files {
            // `real` (canonical) is for comparing; `path` is for showing.
            let real = dir.join(&name);
            let path = rule.dir.join(&name);
            if let Some(orig) = name.strip_suffix(TMP_SUFFIX) {
                // Left by an interrupted run; the lock means no run is
                // writing it now. The original is still in place.
                if rule.matches(orig)
                    && claimed.insert(real)
                    && !dry_run
                    && let Err(e) = fs::remove_file(&path)
                {
                    report.problems.push(format!(
                        "cannot remove leftover {}: {e}",
                        rata.display_path(&path)
                    ));
                }
                continue;
            }
            if !rule.matches(&name) || protected.contains(&real) || !claimed.insert(real) {
                continue;
            }
            let Ok(mzoned) = ZonedDateTime::try_from(mtime) else {
                continue;
            };
            let Some(action) = decide(rule, &name, &mzoned, now, rata.root()) else {
                continue;
            };
            let entry = if dry_run {
                planned(path, action)
            } else {
                // A step of an earlier entry may have handled this file
                // (x.log.gz next to x.log after an interrupted run).
                if fs::symlink_metadata(&path).is_err() {
                    continue;
                }
                execute(path, action, mtime)
            };
            // Files this run made are not for later rules to pick up.
            if let Some(gz) = &entry.compressed {
                claimed.insert(dir.join(gz.file_name().unwrap_or_default()));
            }
            if let Some(to) = &entry.moved_to {
                claimed.insert(fs::canonicalize(to).unwrap_or_else(|_| to.clone()));
            }
            on_entry(&entry);
            report.entries.push(entry);
        }
    }
    report
}

/// Regular files directly in `dir` with UTF-8 names, by name.
fn list_files(dir: &Path) -> io::Result<Vec<(String, SystemTime)>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir)? {
        let e = e?;
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_file() {
            continue;
        }
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        let Ok(mtime) = e.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        out.push((name, mtime));
    }
    out.sort();
    Ok(out)
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn planned(path: PathBuf, action: Action) -> Entry {
    let gz = with_suffix(&path, ".gz");
    let name = path.file_name().unwrap_or_default().to_owned();
    let (compressed, moved_to) = match &action {
        Action::Compress => (Some(gz), None),
        Action::CompressAndMove(d) => {
            let n = gz.file_name().unwrap_or_default().to_owned();
            (Some(gz), Some(d.join(n)))
        }
        Action::Move(d) => (None, Some(d.join(name))),
        Action::Delete => (None, None),
    };
    Entry {
        path,
        action,
        compressed,
        moved_to,
        error: None,
    }
}

fn execute(path: PathBuf, action: Action, mtime: SystemTime) -> Entry {
    let mut entry = Entry {
        path,
        action: action.clone(),
        compressed: None,
        moved_to: None,
        error: None,
    };
    let result = (|| -> std::result::Result<(), String> {
        let mut current = entry.path.clone();
        if matches!(action, Action::Compress | Action::CompressAndMove(_)) {
            current = compress(&current, mtime).map_err(|e| format!("compress: {e}"))?;
            entry.compressed = Some(current.clone());
        }
        match &action {
            Action::CompressAndMove(dir) | Action::Move(dir) => {
                let to = move_file(&current, dir, mtime).map_err(|e| format!("move: {e}"))?;
                entry.moved_to = Some(to);
            }
            Action::Delete => fs::remove_file(&current).map_err(|e| format!("delete: {e}"))?,
            Action::Compress => {}
        }
        Ok(())
    })();
    entry.error = result.err();
    entry
}

fn other(msg: String) -> io::Error {
    io::Error::other(msg)
}

fn display_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// gzip `src` to `src.gz` with the same modified time and permissions, then
/// remove `src`. Returns the `.gz` path.
fn compress(src: &Path, mtime: SystemTime) -> io::Result<PathBuf> {
    let gz = with_suffix(src, ".gz");
    if fs::symlink_metadata(&gz).is_ok() {
        // An interrupted run renamed the .gz into place but did not remove
        // the original, or someone else made it.
        let same = readers_equal(
            flate2::read::GzDecoder::new(BufReader::new(File::open(&gz)?)),
            BufReader::new(File::open(src)?),
        )
        .unwrap_or(false);
        if !same {
            return Err(other(format!(
                "{} already exists and has different contents",
                display_name(&gz)
            )));
        }
        fs::remove_file(src)?;
        return Ok(gz);
    }
    let tmp = with_suffix(&gz, TMP_SUFFIX);
    let result = (|| {
        let before = fs::metadata(src)?;
        let mut input = BufReader::new(File::open(src)?);
        let out = BufWriter::new(File::create(&tmp)?);
        let mut enc = flate2::write::GzEncoder::new(out, flate2::Compression::default());
        io::copy(&mut input, &mut enc)?;
        let file = enc.finish()?.into_inner().map_err(|e| e.into_error())?;
        let after = fs::metadata(src)?;
        if after.len() != before.len() || after.modified()? != mtime {
            return Err(other(format!(
                "{} changed while being compressed",
                display_name(src)
            )));
        }
        file.set_modified(mtime)?;
        file.sync_all()?;
        drop(file);
        fs::set_permissions(&tmp, before.permissions())?;
        fs::rename(&tmp, &gz)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::remove_file(src)?;
    Ok(gz)
}

/// Move `src` into `dir` (created if missing), never replacing a file.
/// Returns the new path.
fn move_file(src: &Path, dir: &Path, mtime: SystemTime) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let dest = dir.join(src.file_name().unwrap_or_default());
    if fs::symlink_metadata(&dest).is_ok() {
        // An interrupted copy finished but did not remove the original, or
        // the archive already has a file by that name.
        let same = readers_equal(
            BufReader::new(File::open(&dest)?),
            BufReader::new(File::open(src)?),
        )
        .unwrap_or(false);
        if !same {
            return Err(other(format!(
                "a different {} is already in the archive; move_to never replaces a file",
                display_name(&dest)
            )));
        }
        fs::remove_file(src)?;
        return Ok(dest);
    }
    match fs::rename(src, &dest) {
        Ok(()) => return Ok(dest),
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {}
        Err(e) => return Err(e),
    }
    let tmp = with_suffix(&dest, TMP_SUFFIX);
    let result = (|| {
        fs::copy(src, &tmp)?;
        let f = fs::OpenOptions::new().write(true).open(&tmp)?;
        f.set_modified(mtime)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, &dest)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::remove_file(src)?;
    Ok(dest)
}

/// Whether two streams have the same bytes.
fn readers_equal(mut a: impl Read, mut b: impl Read) -> io::Result<bool> {
    let mut ba = vec![0u8; 64 * 1024];
    let mut bb = vec![0u8; 64 * 1024];
    loop {
        let na = read_full(&mut a, &mut ba)?;
        let nb = read_full(&mut b, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::FileTimes;

    struct T {
        _tmp: tempfile::TempDir,
        rata: Rata,
        now: ZonedDateTime,
    }

    impl T {
        fn new(config: &str) -> (T, ArchiveConfig) {
            let tmp = tempfile::tempdir().unwrap();
            let rata = Rata::from_path(tmp.path()).unwrap();
            fs::create_dir_all(rata.log_dir()).unwrap();
            let c = ArchiveConfig::parse(config, Path::new("archive.toml"), &rata).unwrap();
            let now = ZonedDateTime::now();
            (
                T {
                    _tmp: tmp,
                    rata,
                    now,
                },
                c,
            )
        }

        /// Write `work/log/<name>`, last modified `days` days ago.
        fn file(&self, name: &str, text: &str, days: i64) -> PathBuf {
            let p = self.rata.log_dir().join(name);
            fs::write(&p, text).unwrap();
            self.age(&p, days);
            p
        }

        fn age(&self, p: &Path, days: i64) {
            let t = SystemTime::now() - Duration::from_secs((days * 86_400) as u64);
            let f = fs::OpenOptions::new().write(true).open(p).unwrap();
            f.set_times(FileTimes::new().set_modified(t)).unwrap();
        }

        fn run(&self, c: &ArchiveConfig, protected: &[PathBuf], dry_run: bool) -> ArchiveReport {
            run(&self.rata, c, protected, &self.now, dry_run, |_| {})
        }
    }

    const COMPRESS_DELETE: &str = "[[rule]]\npath = \"work/log\"\nmatch = [\"*.log\"]\ncompress_after = \"3d\"\ndelete_after = \"90d\"\n";
    const COMPRESS_MOVE: &str = "[[rule]]\npath = \"work/log\"\nmatch = [\"*.log\"]\ncompress_after = \"3d\"\nmove_after = \"30d\"\nmove_to = \"archive/{yyyy}\"\n";

    fn gunzip(p: &Path) -> String {
        let mut s = String::new();
        flate2::read::GzDecoder::new(File::open(p).unwrap())
            .read_to_string(&mut s)
            .unwrap();
        s
    }

    #[test]
    fn compress_then_delete() {
        let (t, c) = T::new(COMPRESS_DELETE);
        let young = t.file("young.log", "y", 1);
        let old = t.file("old.log", "old text", 5);
        let ancient = t.file("ancient.log", "a", 100);
        let ancient_gz = t.file("ancient2.log.gz", "not even gzip", 100);
        let other = t.file("notes.txt", "n", 100);
        let mtime = fs::metadata(&old).unwrap().modified().unwrap();

        let dry = t.run(&c, &[], true);
        assert_eq!(dry.entries.len(), 3);
        assert!(old.exists() && ancient.exists());

        let r = t.run(&c, &[], false);
        assert!(r.ok(), "{r:?}");
        let gz = with_suffix(&old, ".gz");
        assert!(young.exists() && other.exists());
        assert!(!old.exists() && !ancient.exists() && !ancient_gz.exists());
        assert_eq!(gunzip(&gz), "old text");
        assert_eq!(fs::metadata(&gz).unwrap().modified().unwrap(), mtime);

        // Once compressed, it is deleted as the .gz when it is old enough.
        t.age(&gz, 91);
        let r = t.run(&c, &[], false);
        assert_eq!(r.entries[0].action, Action::Delete);
        assert!(!gz.exists());
        assert!(t.run(&c, &[], false).entries.is_empty());
    }

    #[test]
    fn protected_files_are_left_alone() {
        let (t, c) = T::new(COMPRESS_DELETE);
        let live = t.file("live.log", "l", 100);
        let r = t.run(&c, std::slice::from_ref(&live), false);
        assert!(r.entries.is_empty());
        assert!(live.exists());
    }

    #[test]
    fn compress_then_move() {
        let (t, c) = T::new(COMPRESS_MOVE);
        let mid = t.file("mid.log", "m", 10);
        let old = t.file("old.log", "o", 40);
        let r = t.run(&c, &[], false);
        assert!(r.ok(), "{r:?}");
        assert!(with_suffix(&mid, ".gz").exists());
        let year = ZonedDateTime::try_from(SystemTime::now() - Duration::from_secs(40 * 86_400))
            .unwrap()
            .date()
            .year();
        let moved = t
            .rata
            .root()
            .join("archive")
            .join(year.to_string())
            .join("old.log.gz");
        assert_eq!(r.entries[1].moved_to.as_deref(), Some(moved.as_path()));
        assert_eq!(gunzip(&moved), "o");
        assert!(!old.exists() && !with_suffix(&old, ".gz").exists());
    }

    #[test]
    fn move_never_replaces() {
        let (t, c) = T::new(
            "[[rule]]\npath = \"work/log\"\nmatch = [\"*.log\"]\nmove_after = \"1d\"\nmove_to = \"archive\"\n",
        );
        let a = t.file("a.log", "new", 5);
        let b = t.file("b.log", "same", 5);
        let dir = t.rata.root().join("archive");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.log"), "old").unwrap();
        fs::write(dir.join("b.log"), "same").unwrap();
        let r = t.run(&c, &[], false);
        assert!(!r.ok());
        let errors: Vec<_> = r.entries.iter().filter(|e| !e.ok()).collect();
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0]
                .error
                .as_ref()
                .unwrap()
                .contains("a different a.log")
        );
        assert!(a.exists());
        assert_eq!(fs::read_to_string(dir.join("a.log")).unwrap(), "old");
        // An identical file there means an earlier move got that far.
        assert!(!b.exists());
    }

    #[test]
    fn finishes_interrupted_runs() {
        let (t, c) = T::new(COMPRESS_DELETE);
        // Interrupted while writing the .gz: the temporary file is removed
        // and the original compressed again.
        let a = t.file("a.log", "aaa", 5);
        let tmp = t.file("a.log.gz.rata-tmp", "partial", 0);
        // Interrupted after the rename: the original is removed.
        let b = t.file("b.log", "bbb", 5);
        let b_gz = with_suffix(&b, ".gz");
        compress(&b, fs::metadata(&b).unwrap().modified().unwrap()).unwrap();
        fs::write(&b, "bbb").unwrap();
        t.age(&b, 5);
        // A .gz with other contents is an error, and nothing is lost.
        let c_log = t.file("c.log", "ccc", 5);
        let c_gz = with_suffix(&c_log, ".gz");
        fs::copy(&b_gz, &c_gz).unwrap();

        let r = t.run(&c, &[], false);
        assert!(!tmp.exists());
        assert_eq!(gunzip(&with_suffix(&a, ".gz")), "aaa");
        assert!(!b.exists() && b_gz.exists());
        assert!(c_log.exists());
        let failed: Vec<_> = r.entries.iter().filter(|e| !e.ok()).collect();
        assert_eq!(failed.len(), 1, "{r:?}");
        assert_eq!(failed[0].path, c_log);
    }

    #[test]
    fn first_rule_wins_and_missing_dirs_are_skipped() {
        let (t, c) = T::new(
            "[[rule]]\npath = \"work/nope\"\nmatch = [\"*\"]\ndelete_after = \"1d\"\n\
             [[rule]]\npath = \"work/log\"\nmatch = [\"*.log\"]\ncompress_after = \"1d\"\n\
             [[rule]]\npath = \"${RATA}/work/log\"\nmatch = [\"*\"]\ndelete_after = \"1d\"\n",
        );
        let a = t.file("a.log", "a", 5);
        let b = t.file("b.txt", "b", 5);
        let r = t.run(&c, &[], false);
        assert!(r.ok(), "{r:?}");
        assert_eq!(r.missing_dirs.len(), 1);
        assert!(with_suffix(&a, ".gz").exists());
        assert!(!b.exists());
    }
}

#[cfg(test)]
mod move_tests {
    use super::*;
    use std::fs::FileTimes;

    #[test]
    fn moves_uncompressed_and_honors_exclude() {
        let tmp = tempfile::tempdir().unwrap();
        let rata = Rata::from_path(tmp.path()).unwrap();
        fs::create_dir_all(rata.log_dir()).unwrap();
        let c = ArchiveConfig::parse(
            "[[rule]]\npath = \"work/log\"\nmatch = [\"*.log\"]\nexclude = [\"RATA_MONITOR_*\"]\n\
             move_after = \"1mo\"\nmove_to = \"archive\"\n",
            Path::new("archive.toml"),
            &rata,
        )
        .unwrap();
        let old = SystemTime::now() - Duration::from_secs(70 * 86_400);
        for name in ["WEB_1.log", "RATA_MONITOR_1.log"] {
            let p = rata.log_dir().join(name);
            fs::write(&p, name).unwrap();
            let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.set_times(FileTimes::new().set_modified(old)).unwrap();
        }
        let r = run(&rata, &c, &[], &ZonedDateTime::now(), false, |_| {});
        assert!(r.ok(), "{r:?}");
        assert_eq!(r.entries.len(), 1);
        let moved = rata.root().join("archive").join("WEB_1.log");
        assert_eq!(fs::read_to_string(&moved).unwrap(), "WEB_1.log");
        assert_eq!(fs::metadata(&moved).unwrap().modified().unwrap(), old);
        assert!(rata.log_dir().join("RATA_MONITOR_1.log").exists());
    }
}
