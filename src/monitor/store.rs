//! The CSV files in `$RATA/work/monitor`.
//!
//! Each kind of row has its own files, one per local day:
//! `<category>_YYYYMMDD.csv`. If a file's header differs from the current
//! one (for example, after an upgrade added a column), the writer starts
//! `<category>_YYYYMMDD.1.csv`, then `.2`, and so on. Files are only ever
//! appended to, and only today's files are written: once a day is over, its
//! files never change.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use jiff::civil::Date;

use super::csv;
use super::sample::{DiskRow, Fields, NetRow, Row, Sample, ServiceRow, SystemRow};

/// File name for `category` on `date`, part `n`.
pub fn file_name(category: &str, date: Date, n: u32) -> String {
    let base = format!(
        "{category}_{:04}{:02}{:02}",
        date.year(),
        date.month(),
        date.day()
    );
    if n == 0 {
        format!("{base}.csv")
    } else {
        format!("{base}.{n}.csv")
    }
}

/// Parse a file name into its category, date, and part number.
pub fn parse_file_name(name: &str) -> Option<(&str, Date, u32)> {
    let stem = name.strip_suffix(".csv")?;
    let (stem, n) = match stem.rsplit_once('.') {
        Some((s, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            (s, n.parse().ok()?)
        }
        _ => (stem, 0),
    };
    let (category, ymd) = stem.rsplit_once('_')?;
    if category.is_empty() || ymd.len() != 8 || !ymd.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let date = Date::new(
        ymd[..4].parse().ok()?,
        ymd[4..6].parse().ok()?,
        ymd[6..].parse().ok()?,
    )
    .ok()?;
    Some((category, date, n))
}

/// Every monitor file in `dir` for `category`, sorted oldest first.
pub fn files(dir: &Path, category: &str) -> Vec<(Date, u32, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(Date, u32, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            let (c, date, n) = parse_file_name(name.to_str()?)?;
            (c == category).then(|| (date, n, e.path()))
        })
        .collect();
    out.sort();
    out
}

/// Writes samples to `dir`, appending to today's files.
#[derive(Debug)]
pub struct Writer {
    dir: PathBuf,
    current: HashMap<&'static str, (Date, PathBuf)>,
}

impl Writer {
    /// A writer for `dir`, which is created if missing.
    pub fn new(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Writer {
            dir: dir.to_path_buf(),
            current: HashMap::new(),
        })
    }

    /// Append every row of `sample` to the files for the sample's local date.
    ///
    /// Each category is written with a single `write` call. The first error
    /// is returned after trying every category.
    pub fn write(&mut self, sample: &Sample) -> io::Result<()> {
        let date = sample.time.datetime().date();
        let results = [
            self.append(date, sample.system.as_slice()),
            self.append(date, &sample.disks),
            self.append(date, &sample.net),
            self.append(date, &sample.services),
        ];
        results.into_iter().collect()
    }

    fn append<R: Row>(&mut self, date: Date, rows: &[R]) -> io::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let path = match self.current.get(R::CATEGORY) {
            Some((d, p)) if *d == date => p.clone(),
            _ => {
                let p = self.choose_file::<R>(date)?;
                self.current.insert(R::CATEGORY, (date, p.clone()));
                p
            }
        };
        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        let mut text = String::new();
        let len = f.metadata()?.len();
        if len == 0 {
            csv::write_record(&mut text, R::HEADER);
        } else if !ends_with_newline(&path)? {
            // The last write was cut off; keep the next row on its own line.
            text.push('\n');
        }
        for r in rows {
            csv::write_record(&mut text, &r.to_record());
        }
        f.write_all(text.as_bytes())?;
        f.flush()
    }

    /// The file to append `R` rows to on `date`: the newest part whose
    /// header matches, or a new part.
    fn choose_file<R: Row>(&self, date: Date) -> io::Result<PathBuf> {
        let parts: Vec<(u32, PathBuf)> = files(&self.dir, R::CATEGORY)
            .into_iter()
            .filter(|(d, _, _)| *d == date)
            .map(|(_, n, p)| (n, p))
            .collect();
        match parts.last() {
            None => Ok(self.dir.join(file_name(R::CATEGORY, date, 0))),
            Some((n, p)) => match read_header(p)? {
                Some(h) if h == R::HEADER => Ok(p.clone()),
                None => Ok(p.clone()),
                Some(_) => Ok(self.dir.join(file_name(R::CATEGORY, date, n + 1))),
            },
        }
    }
}

fn ends_with_newline(path: &Path) -> io::Result<bool> {
    let mut f = File::open(path)?;
    if f.seek(SeekFrom::End(-1)).is_err() {
        return Ok(true);
    }
    let mut b = [0u8; 1];
    f.read_exact(&mut b)?;
    Ok(b[0] == b'\n')
}

/// The header of a CSV file, or `None` if the file has no complete line.
fn read_header(path: &Path) -> io::Result<Option<Vec<String>>> {
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; 4096];
    let n = f.read(&mut buf)?;
    let text = String::from_utf8_lossy(&buf[..n]);
    let (mut records, _) = csv::parse_records(&text);
    Ok(if records.is_empty() {
        None
    } else {
        Some(records.swap_remove(0))
    })
}

/// The rows of the newest sample in `category`'s newest file that has one.
///
/// Reads only the end of the file, so it stays fast on large files.
pub fn latest_rows<R: Row>(dir: &Path) -> Vec<R> {
    for (_, _, path) in files(dir, R::CATEGORY).into_iter().rev() {
        if let Ok(rows) = last_rows_of_file::<R>(&path)
            && !rows.is_empty()
        {
            return rows;
        }
    }
    Vec::new()
}

fn last_rows_of_file<R: Row>(path: &Path) -> io::Result<Vec<R>> {
    let Some(header) = read_header(path)? else {
        return Ok(Vec::new());
    };
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let mut chunk: u64 = 64 * 1024;
    loop {
        let start = len.saturating_sub(chunk);
        f.seek(SeekFrom::Start(start))?;
        let mut buf = Vec::with_capacity((len - start) as usize);
        (&mut f).take(len - start).read_to_end(&mut buf)?;
        let mut text = String::from_utf8_lossy(&buf).into_owned();
        if start > 0 {
            // Drop the partial line at the start of the chunk.
            match text.find('\n') {
                Some(i) => {
                    text.drain(..=i);
                }
                None => text.clear(),
            }
        }
        let (mut records, _) = csv::parse_records(&text);
        if start == 0 && !records.is_empty() {
            records.remove(0); // the header
        }
        let rows: Vec<R> = records
            .iter()
            .filter_map(|r| R::from_fields(&Fields::new(&header, r)))
            .collect();
        let Some(last) = rows.last().map(|r| r.time()) else {
            if start == 0 {
                return Ok(Vec::new());
            }
            chunk *= 4;
            continue;
        };
        let first_of_last = rows.iter().rposition(|r| r.time() != last);
        match first_of_last {
            // Every row in the chunk has the last time: the sample may
            // start before the chunk.
            None if start > 0 => {
                chunk *= 4;
                continue;
            }
            None => return Ok(rows),
            Some(i) => return Ok(rows.into_iter().skip(i + 1).collect()),
        }
    }
}

/// The newest sample in `dir`, combining the newest rows of each category.
///
/// The sample time is the newest of the categories' times. A category whose
/// newest rows are from an older sample (for example, host stats that were
/// turned off) is left out. Returns `None` if nothing has been recorded.
pub fn latest(dir: &Path) -> Option<Sample> {
    let system: Vec<SystemRow> = latest_rows(dir);
    let disks: Vec<DiskRow> = latest_rows(dir);
    let net: Vec<NetRow> = latest_rows(dir);
    let services: Vec<ServiceRow> = latest_rows(dir);
    let time = [
        system.first().map(|r| r.time),
        disks.first().map(|r| r.time),
        net.first().map(|r| r.time),
        services.first().map(|r| r.time),
    ]
    .into_iter()
    .flatten()
    .max_by_key(|t| t.timestamp)?;
    let same = |t: crate::datetime::RataTime| t.timestamp == time.timestamp;
    Some(Sample {
        time,
        system: system.into_iter().next().filter(|r| same(r.time)),
        disks: disks.into_iter().filter(|r| same(r.time)).collect(),
        net: net.into_iter().filter(|r| same(r.time)).collect(),
        services: services.into_iter().filter(|r| same(r.time)).collect(),
    })
}

/// The monitor files that may still be written to: today's files (local
/// date). Every other file in `dir` is closed and will not change.
pub fn live_files(dir: &Path) -> Vec<PathBuf> {
    let today = jiff::ZonedDateTime::now().date();
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .and_then(parse_file_name)
                .is_some_and(|(_, d, _)| d == today)
        })
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datetime::RataTime;
    use crate::service::State;

    fn t(s: &str) -> RataTime {
        format!("2026-11-26 {s}.000000000 -0500 ").parse().unwrap()
    }

    fn sample(time: RataTime, services: &[&str]) -> Sample {
        Sample {
            time,
            system: None,
            disks: Vec::new(),
            net: Vec::new(),
            services: services
                .iter()
                .map(|s| ServiceRow::stopped(time, s))
                .collect(),
        }
    }

    #[test]
    fn names() {
        let d = jiff::civil::date(2026, 11, 26);
        assert_eq!(file_name("net", d, 0), "net_20261126.csv");
        assert_eq!(file_name("net", d, 2), "net_20261126.2.csv");
        assert_eq!(
            parse_file_name("service_20261126.csv"),
            Some(("service", d, 0))
        );
        assert_eq!(
            parse_file_name("service_20261126.3.csv"),
            Some(("service", d, 3))
        );
        assert_eq!(parse_file_name("service_2026112.csv"), None);
        assert_eq!(parse_file_name("service_20261326.csv"), None);
        assert_eq!(parse_file_name("notes.txt"), None);
    }

    #[test]
    fn write_then_read_latest() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = Writer::new(tmp.path()).unwrap();
        w.write(&sample(t("14:29:30"), &["A", "B"])).unwrap();
        w.write(&sample(t("14:30:00"), &["A", "B", "C"])).unwrap();
        let text = fs::read_to_string(tmp.path().join("service_20261126.csv")).unwrap();
        assert_eq!(text.lines().count(), 6);
        assert!(text.starts_with("time,service,state,"));
        let s = latest(tmp.path()).unwrap();
        assert_eq!(s.time, t("14:30:00"));
        let names: Vec<_> = s.services.iter().map(|r| r.service.as_str()).collect();
        assert_eq!(names, ["A", "B", "C"]);
        assert!(s.services.iter().all(|r| r.state == State::Stopped));
        assert!(s.system.is_none());
        // No other categories were written.
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }

    #[test]
    fn partial_line_is_skipped_and_not_merged() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = Writer::new(tmp.path()).unwrap();
        w.write(&sample(t("14:29:30"), &["A"])).unwrap();
        let path = tmp.path().join("service_20261126.csv");
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"2026-11-26 14:30:00.0000").unwrap();
        // The cut-off row is ignored by readers...
        assert_eq!(latest(tmp.path()).unwrap().time, t("14:29:30"));
        // ...and the next write starts a new line.
        let mut w = Writer::new(tmp.path()).unwrap();
        w.write(&sample(t("14:31:00"), &["A"])).unwrap();
        assert_eq!(latest(tmp.path()).unwrap().time, t("14:31:00"));
    }

    #[test]
    fn header_change_starts_a_new_part() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("service_20261126.csv"),
            "time,service\n2026-11-26 14:00:00.000000000 -0500 ,OLD\n",
        )
        .unwrap();
        let mut w = Writer::new(tmp.path()).unwrap();
        w.write(&sample(t("14:30:00"), &["A"])).unwrap();
        assert!(tmp.path().join("service_20261126.1.csv").is_file());
        assert_eq!(latest(tmp.path()).unwrap().services[0].service, "A");
    }

    #[test]
    fn large_file_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = Writer::new(tmp.path()).unwrap();
        let many: Vec<String> = (0..3000).map(|i| format!("SVC_{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        w.write(&sample(t("14:00:00"), &refs)).unwrap();
        w.write(&sample(t("14:00:30"), &refs)).unwrap();
        let s = latest(tmp.path()).unwrap();
        assert_eq!(s.services.len(), 3000);
        assert_eq!(s.time, t("14:00:30"));
    }

    #[test]
    fn categories_from_older_samples_are_left_out() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = Writer::new(tmp.path()).unwrap();
        let mut s = sample(t("14:00:00"), &["A"]);
        s.net.push(NetRow {
            time: t("14:00:00"),
            iface: "en0".into(),
            rx_bytes: Some(1),
            tx_bytes: Some(1),
            rx_packets: None,
            tx_packets: None,
            rx_errors: None,
            tx_errors: None,
            rx_dropped: None,
            tx_dropped: None,
        });
        w.write(&s).unwrap();
        w.write(&sample(t("14:00:30"), &["A"])).unwrap();
        let s = latest(tmp.path()).unwrap();
        assert!(s.net.is_empty());
        assert_eq!(s.services.len(), 1);
    }
}
