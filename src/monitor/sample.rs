//! What one sample records, and how each kind of row maps to CSV columns.
//!
//! Byte counts are integers. `read`, `written`, `rx_*`, `tx_*`, and
//! `disk_*` are amounts during the interval that ended at `time` (since the
//! previous sample), not totals. Empty fields mean "not available".

use std::collections::HashMap;

use serde::Serialize;

use crate::datetime::RataTime;

/// One kind of row, stored in its own `<CATEGORY>_YYYYMMDD.csv` files.
pub trait Row: Sized {
    /// The file name prefix, e.g. `system`.
    const CATEGORY: &'static str;
    /// The CSV header, in column order. The first column is always `time`.
    const HEADER: &'static [&'static str];
    /// The sample time.
    fn time(&self) -> RataTime;
    /// The values, in [`Row::HEADER`] order.
    fn to_record(&self) -> Vec<String>;
    /// Build a row from values looked up by column name. Returns `None` if
    /// a required column is missing or invalid.
    fn from_fields(f: &Fields<'_>) -> Option<Self>;
}

/// The values of one CSV record, by column name.
pub struct Fields<'a> {
    map: HashMap<&'a str, &'a str>,
}

impl<'a> Fields<'a> {
    /// Pair `header` names with `record` values.
    pub fn new(header: &'a [String], record: &'a [String]) -> Self {
        Fields {
            map: header
                .iter()
                .map(String::as_str)
                .zip(record.iter().map(String::as_str))
                .collect(),
        }
    }

    fn get(&self, name: &str) -> Option<&'a str> {
        self.map.get(name).copied()
    }

    fn str(&self, name: &str) -> Option<String> {
        self.get(name).map(str::to_string)
    }

    fn time(&self) -> Option<RataTime> {
        self.get("time")?.parse().ok()
    }

    /// A required number.
    fn num<T: std::str::FromStr>(&self, name: &str) -> Option<T> {
        self.get(name)?.parse().ok()
    }

    /// An optional number: empty or missing is `None`.
    fn opt<T: std::str::FromStr>(&self, name: &str) -> Option<T> {
        self.get(name).filter(|s| !s.is_empty())?.parse().ok()
    }
}

fn opt<T: ToString>(v: Option<T>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

fn pct(v: f64) -> String {
    format!("{v:.1}")
}

/// Host-wide stats.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SystemRow {
    /// Sample time.
    pub time: RataTime,
    /// CPU use across all cores, 0-100.
    pub cpu_pct: f64,
    /// 1, 5, and 15 minute load averages (not on Windows).
    pub load: Option<[f64; 3]>,
    /// Number of logical CPUs.
    pub cores: u64,
    /// Physical memory, in bytes.
    pub mem_total: u64,
    /// Memory in use (not available to new programs), in bytes:
    /// `mem_total - mem_avail`.
    pub mem_used: u64,
    /// Memory available to new programs, in bytes.
    pub mem_avail: u64,
    /// Swap space, in bytes.
    pub swap_total: u64,
    /// Swap in use, in bytes.
    pub swap_used: u64,
    /// Seconds since the host booted.
    pub uptime_s: u64,
}

impl Row for SystemRow {
    const CATEGORY: &'static str = "system";
    const HEADER: &'static [&'static str] = &[
        "time",
        "cpu_pct",
        "load1",
        "load5",
        "load15",
        "cores",
        "mem_total",
        "mem_used",
        "mem_avail",
        "swap_total",
        "swap_used",
        "uptime_s",
    ];

    fn time(&self) -> RataTime {
        self.time
    }

    fn to_record(&self) -> Vec<String> {
        let load = |i: usize| opt(self.load.map(|l| format!("{:.2}", l[i])));
        vec![
            self.time.to_string(),
            pct(self.cpu_pct),
            load(0),
            load(1),
            load(2),
            self.cores.to_string(),
            self.mem_total.to_string(),
            self.mem_used.to_string(),
            self.mem_avail.to_string(),
            self.swap_total.to_string(),
            self.swap_used.to_string(),
            self.uptime_s.to_string(),
        ]
    }

    fn from_fields(f: &Fields<'_>) -> Option<Self> {
        let load = match (f.opt("load1"), f.opt("load5"), f.opt("load15")) {
            (Some(a), Some(b), Some(c)) => Some([a, b, c]),
            _ => None,
        };
        Some(SystemRow {
            time: f.time()?,
            cpu_pct: f.num("cpu_pct")?,
            load,
            cores: f.num("cores")?,
            mem_total: f.num("mem_total")?,
            mem_used: f.num("mem_used")?,
            mem_avail: f.num("mem_avail")?,
            swap_total: f.num("swap_total")?,
            swap_used: f.num("swap_used")?,
            uptime_s: f.num("uptime_s")?,
        })
    }
}

/// One mounted filesystem.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiskRow {
    /// Sample time.
    pub time: RataTime,
    /// Mount point (a drive such as `C:\` on Windows).
    pub mount: String,
    /// Filesystem type.
    pub fs: String,
    /// Size, in bytes.
    pub total: u64,
    /// Space in use, in bytes.
    pub used: u64,
    /// Space available, in bytes.
    pub avail: u64,
    /// Bytes read during the interval.
    pub read: Option<u64>,
    /// Bytes written during the interval.
    pub written: Option<u64>,
}

impl Row for DiskRow {
    const CATEGORY: &'static str = "disk";
    const HEADER: &'static [&'static str] = &[
        "time", "mount", "fs", "total", "used", "avail", "read", "written",
    ];

    fn time(&self) -> RataTime {
        self.time
    }

    fn to_record(&self) -> Vec<String> {
        vec![
            self.time.to_string(),
            self.mount.clone(),
            self.fs.clone(),
            self.total.to_string(),
            self.used.to_string(),
            self.avail.to_string(),
            opt(self.read),
            opt(self.written),
        ]
    }

    fn from_fields(f: &Fields<'_>) -> Option<Self> {
        Some(DiskRow {
            time: f.time()?,
            mount: f.str("mount")?,
            fs: f.str("fs")?,
            total: f.num("total")?,
            used: f.num("used")?,
            avail: f.num("avail")?,
            read: f.opt("read"),
            written: f.opt("written"),
        })
    }
}

/// One network interface.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NetRow {
    /// Sample time.
    pub time: RataTime,
    /// Interface name.
    pub iface: String,
    /// Bytes received during the interval.
    pub rx_bytes: Option<u64>,
    /// Bytes sent during the interval.
    pub tx_bytes: Option<u64>,
    /// Packets received during the interval.
    pub rx_packets: Option<u64>,
    /// Packets sent during the interval.
    pub tx_packets: Option<u64>,
    /// Receive errors during the interval.
    pub rx_errors: Option<u64>,
    /// Send errors during the interval.
    pub tx_errors: Option<u64>,
    /// Packets dropped on receive during the interval (where the platform
    /// reports it; see [`netdrops`](super::netdrops)).
    pub rx_dropped: Option<u64>,
    /// Packets dropped on send during the interval.
    pub tx_dropped: Option<u64>,
}

impl Row for NetRow {
    const CATEGORY: &'static str = "net";
    const HEADER: &'static [&'static str] = &[
        "time",
        "iface",
        "rx_bytes",
        "tx_bytes",
        "rx_packets",
        "tx_packets",
        "rx_errors",
        "tx_errors",
        "rx_dropped",
        "tx_dropped",
    ];

    fn time(&self) -> RataTime {
        self.time
    }

    fn to_record(&self) -> Vec<String> {
        vec![
            self.time.to_string(),
            self.iface.clone(),
            opt(self.rx_bytes),
            opt(self.tx_bytes),
            opt(self.rx_packets),
            opt(self.tx_packets),
            opt(self.rx_errors),
            opt(self.tx_errors),
            opt(self.rx_dropped),
            opt(self.tx_dropped),
        ]
    }

    fn from_fields(f: &Fields<'_>) -> Option<Self> {
        Some(NetRow {
            time: f.time()?,
            iface: f.str("iface")?,
            rx_bytes: f.opt("rx_bytes"),
            tx_bytes: f.opt("tx_bytes"),
            rx_packets: f.opt("rx_packets"),
            tx_packets: f.opt("tx_packets"),
            rx_errors: f.opt("rx_errors"),
            tx_errors: f.opt("tx_errors"),
            rx_dropped: f.opt("rx_dropped"),
            tx_dropped: f.opt("tx_dropped"),
        })
    }
}

/// One configured service: its whole process tree, summed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServiceRow {
    /// Sample time.
    pub time: RataTime,
    /// Service name.
    pub service: String,
    /// `running` or `stopped`.
    pub state: crate::service::State,
    /// The first matching process.
    pub pid: Option<u32>,
    /// Processes in the tree: matching processes and their descendants.
    pub procs: Option<u64>,
    /// CPU use of the tree; 100 is one full core.
    pub cpu_pct: Option<f64>,
    /// Resident memory of the tree, in bytes.
    pub rss: Option<u64>,
    /// Bytes the tree read during the interval.
    pub disk_read: Option<u64>,
    /// Bytes the tree wrote during the interval.
    pub disk_written: Option<u64>,
    /// Seconds since the oldest matching process started.
    pub uptime_s: Option<u64>,
}

impl ServiceRow {
    /// A row for a service with no matching process.
    pub fn stopped(time: RataTime, service: &str) -> Self {
        ServiceRow {
            time,
            service: service.to_string(),
            state: crate::service::State::Stopped,
            pid: None,
            procs: None,
            cpu_pct: None,
            rss: None,
            disk_read: None,
            disk_written: None,
            uptime_s: None,
        }
    }
}

impl Row for ServiceRow {
    const CATEGORY: &'static str = "service";
    const HEADER: &'static [&'static str] = &[
        "time",
        "service",
        "state",
        "pid",
        "procs",
        "cpu_pct",
        "rss",
        "disk_read",
        "disk_written",
        "uptime_s",
    ];

    fn time(&self) -> RataTime {
        self.time
    }

    fn to_record(&self) -> Vec<String> {
        vec![
            self.time.to_string(),
            self.service.clone(),
            self.state.as_str().to_string(),
            opt(self.pid),
            opt(self.procs),
            opt(self.cpu_pct.map(pct)),
            opt(self.rss),
            opt(self.disk_read),
            opt(self.disk_written),
            opt(self.uptime_s),
        ]
    }

    fn from_fields(f: &Fields<'_>) -> Option<Self> {
        let state = match f.get("state")? {
            "running" => crate::service::State::Running,
            "stopped" => crate::service::State::Stopped,
            _ => return None,
        };
        Some(ServiceRow {
            time: f.time()?,
            service: f.str("service")?,
            state,
            pid: f.opt("pid"),
            procs: f.opt("procs"),
            cpu_pct: f.opt("cpu_pct"),
            rss: f.opt("rss"),
            disk_read: f.opt("disk_read"),
            disk_written: f.opt("disk_written"),
            uptime_s: f.opt("uptime_s"),
        })
    }
}

/// Everything recorded at one sample time.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sample {
    /// Sample time.
    pub time: RataTime,
    /// Host stats; `None` when `system = false`.
    pub system: Option<SystemRow>,
    /// Filesystems (empty when `system = false`).
    pub disks: Vec<DiskRow>,
    /// Network interfaces (empty when `system = false`).
    pub net: Vec<NetRow>,
    /// Configured services, in config order.
    pub services: Vec<ServiceRow>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> RataTime {
        "2026-11-26 14:30:00.000000000 -0500 ".parse().unwrap()
    }

    fn round_trip<R: Row + PartialEq + std::fmt::Debug>(row: R) {
        let header: Vec<String> = R::HEADER.iter().map(|s| s.to_string()).collect();
        let record = row.to_record();
        assert_eq!(record.len(), header.len());
        assert_eq!(R::from_fields(&Fields::new(&header, &record)).unwrap(), row);
    }

    #[test]
    fn rows_round_trip() {
        round_trip(SystemRow {
            time: t(),
            cpu_pct: 12.4,
            load: Some([1.82, 1.64, 1.55]),
            cores: 10,
            mem_total: 17179869184,
            mem_used: 9663676416,
            mem_avail: 7516192768,
            swap_total: 0,
            swap_used: 0,
            uptime_s: 529920,
        });
        round_trip(DiskRow {
            time: t(),
            mount: "/".into(),
            fs: "apfs".into(),
            total: 10,
            used: 4,
            avail: 6,
            read: None,
            written: Some(5),
        });
        round_trip(NetRow {
            time: t(),
            iface: "en0".into(),
            rx_bytes: Some(1),
            tx_bytes: Some(2),
            rx_packets: Some(3),
            tx_packets: Some(4),
            rx_errors: Some(0),
            tx_errors: None,
            rx_dropped: Some(7),
            tx_dropped: None,
        });
        round_trip(ServiceRow::stopped(t(), "CACHE"));
        round_trip(ServiceRow {
            time: t(),
            service: "WEB_API".into(),
            state: crate::service::State::Running,
            pid: Some(4121),
            procs: Some(3),
            cpu_pct: Some(3.1),
            rss: Some(85983232),
            disk_read: Some(0),
            disk_written: Some(4096),
            uptime_s: Some(7260),
        });
    }

    #[test]
    fn columns_are_found_by_name() {
        // A file from a version with an extra column still reads.
        let header: Vec<String> = ["extra", "time", "iface"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let rec: Vec<String> = ["x", &t().to_string(), "en0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let row = NetRow::from_fields(&Fields::new(&header, &rec)).unwrap();
        assert_eq!(row.iface, "en0");
        assert_eq!(row.rx_bytes, None);
    }
}
