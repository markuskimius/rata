//! Setting up a new rata install (`rata init`).
//!
//! [`init`] creates the `work/` tree, a `service.toml` that runs the rata
//! monitor, a commented `monitor.toml`, and an `archive.toml` that
//! compresses old logs and monitor files after 3 days and deletes them
//! after 90. It only works in a directory
//! that does not exist yet or is empty; hidden entries (names starting with
//! `.`, and on Windows files with the hidden attribute) do not count.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::env::{RATA_VAR, Rata};
use crate::error::{Error, Result};

/// The `service.toml` that `rata init` writes.
pub const SERVICE_TEMPLATE: &str = include_str!("../docs/init.service.toml");

/// The `monitor.toml` that `rata init` writes.
pub const MONITOR_TEMPLATE: &str = include_str!("../docs/monitor.example.toml");

/// The `archive.toml` that `rata init` writes.
pub const ARCHIVE_TEMPLATE: &str = include_str!("../docs/init.archive.toml");

/// What [`init`] created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    /// The new install's root directory.
    pub root: PathBuf,
    /// Whether the root directory itself was created.
    pub created_root: bool,
    /// Files and directories created, in creation order. Directories end
    /// with a path separator when displayed by the CLI.
    pub created: Vec<PathBuf>,
}

/// The directory to set up: `dir` if given, otherwise `$RATA`.
pub fn target_dir(dir: Option<PathBuf>) -> Result<PathBuf> {
    let (path, what) = match dir {
        Some(d) => (d, "DIR"),
        None => match std::env::var_os(RATA_VAR) {
            Some(v) if !v.is_empty() => (PathBuf::from(v), "RATA"),
            _ => return Err(Error::InitNoDir),
        },
    };
    let s = path
        .to_str()
        .ok_or_else(|| Error::RataNotUnicode(path.clone()))?;
    if !path.is_absolute() {
        return Err(Error::InitNotAbsolute {
            what,
            path: s.to_string(),
        });
    }
    Ok(path)
}

/// Set up a new rata install in `root` (see the module docs).
pub fn init(root: &Path) -> Result<InitReport> {
    let created_root = match fs::metadata(root) {
        Ok(m) if m.is_dir() => {
            if let Some(found) = first_visible_entry(root)? {
                return Err(Error::InitNotEmpty {
                    dir: root.to_path_buf(),
                    found,
                });
            }
            false
        }
        Ok(_) => return Err(Error::InitNotDir(root.to_path_buf())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(root)
                .map_err(|e| Error::io(format!("cannot create {}", root.display()), e))?;
            true
        }
        Err(e) => return Err(Error::io(format!("cannot read {}", root.display()), e)),
    };

    let rata = Rata::from_path(root)?;
    let mut created = Vec::new();
    for dir in [
        rata.cfg_dir(),
        rata.log_dir(),
        rata.run_dir(),
        rata.monitor_dir(),
    ] {
        fs::create_dir_all(&dir)
            .map_err(|e| Error::io(format!("cannot create {}", dir.display()), e))?;
        created.push(dir);
    }
    for (path, text) in [
        (rata.config_path(), SERVICE_TEMPLATE),
        (rata.monitor_config_path(), MONITOR_TEMPLATE),
        (rata.archive_config_path(), ARCHIVE_TEMPLATE),
    ] {
        write_new(&path, text)?;
        created.push(path);
    }
    Ok(InitReport {
        root: root.to_path_buf(),
        created_root,
        created,
    })
}

/// Write `text` to `path`, failing if the file already exists.
fn write_new(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| Error::io(format!("cannot create {}", path.display()), e))?;
    f.write_all(text.as_bytes())
        .map_err(|e| Error::io(format!("cannot write {}", path.display()), e))
}

/// The first entry of `dir` that is not hidden, by name.
fn first_visible_entry(dir: &Path) -> Result<Option<String>> {
    let entries =
        fs::read_dir(dir).map_err(|e| Error::io(format!("cannot read {}", dir.display()), e))?;
    let mut visible: Vec<(String, bool)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| !is_hidden(e))
        .map(|e| {
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            (e.file_name().to_string_lossy().into_owned(), is_dir)
        })
        .collect();
    visible.sort();
    Ok(visible
        .into_iter()
        .next()
        .map(|(name, is_dir)| if is_dir { format!("{name}/") } else { name }))
}

fn is_hidden(entry: &fs::DirEntry) -> bool {
    entry.file_name().to_string_lossy().starts_with('.') || has_hidden_attribute(entry)
}

#[cfg(windows)]
fn has_hidden_attribute(entry: &fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    entry
        .metadata()
        .is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
}

#[cfg(not(windows))]
fn has_hidden_attribute(_entry: &fs::DirEntry) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn creates_layout_in_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("new").join("install");
        let r = init(&root).unwrap();
        assert!(r.created_root);
        let rata = Rata::from_path(&root).unwrap();
        for d in [rata.log_dir(), rata.run_dir(), rata.monitor_dir()] {
            assert!(d.is_dir(), "{}", d.display());
        }
        let c = Config::load(&rata).unwrap();
        assert_eq!(c.services.len(), 1);
        assert_eq!(c.services[0].name, "RATA_MONITOR");
        assert!(c.services[0].explicit_stop);
        assert!(rata.monitor_config_path().is_file());
        assert!(rata.archive_config_path().is_file());
    }

    #[test]
    fn hidden_entries_do_not_count() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".DS_Store"), "").unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        let r = init(tmp.path()).unwrap();
        assert!(!r.created_root);
    }

    #[test]
    fn refuses_non_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("work")).unwrap();
        fs::write(tmp.path().join("zz"), "").unwrap();
        let e = init(tmp.path()).unwrap_err();
        assert!(
            matches!(&e, Error::InitNotEmpty { found, .. } if found == "work/"),
            "{e}"
        );
        assert!(!tmp.path().join("work").join("cfg").exists());
    }

    #[test]
    fn refuses_file() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("f");
        fs::write(&f, "").unwrap();
        assert!(matches!(init(&f), Err(Error::InitNotDir(_))));
    }

    #[test]
    fn target_must_be_absolute() {
        assert!(matches!(
            target_dir(Some("rel".into())),
            Err(Error::InitNotAbsolute { what: "DIR", .. })
        ));
    }
}
