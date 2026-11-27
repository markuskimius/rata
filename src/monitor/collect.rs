//! Taking samples with sysinfo.

use std::collections::HashSet;
use std::net::IpAddr;

use sysinfo::{
    CpuRefreshKind, DiskRefreshKind, Disks, MemoryRefreshKind, Networks, Pid, ProcessRefreshKind,
    ProcessesToUpdate, RefreshKind, System, UpdateKind,
};

use super::config::MonitorConfig;
use super::sample::{DiskRow, NetRow, Sample, ServiceRow, SystemRow};
use crate::config::Service;
use crate::datetime::RataTime;
use crate::env::Rata;
use crate::process;
use crate::service::State;

/// Filesystem types that are never recorded.
const PSEUDO_FS: &[&str] = &[
    "autofs",
    "binfmt_misc",
    "bpf",
    "cgroup",
    "cgroup2",
    "configfs",
    "debugfs",
    "devfs",
    "devpts",
    "devtmpfs",
    "efivarfs",
    "fdescfs",
    "fusectl",
    "hugetlbfs",
    "mqueue",
    "nsfs",
    "nullfs",
    "overlay",
    "proc",
    "pstore",
    "ramfs",
    "securityfs",
    "squashfs",
    "sysfs",
    "tmpfs",
    "tracefs",
];

/// Mount point prefixes that are never recorded.
const PSEUDO_MOUNTS: &[&str] = &["/System/Volumes/", "/private/var/vm", "/snap/"];

/// Keeps sysinfo state between samples, so CPU use and I/O amounts can be
/// measured over each interval.
pub struct Collector {
    sys: System,
    disks: Disks,
    nets: Networks,
    seen_disks: HashSet<String>,
    seen_nets: HashSet<String>,
    /// The next sample is the first: it covers less than a whole interval,
    /// so its interval amounts (I/O, network) are left empty.
    first: bool,
}

fn host_refresh() -> RefreshKind {
    RefreshKind::nothing()
        .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
        .with_memory(MemoryRefreshKind::everything())
}

fn process_refresh() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_cmd(UpdateKind::Always)
        .with_user(UpdateKind::OnlyIfNotSet)
        .with_cpu()
        .with_memory()
        .with_disk_usage()
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector {
    /// Take the first measurements. The first [`Collector::sample`] reports
    /// usage since this call.
    pub fn new() -> Self {
        let mut c = Collector {
            sys: System::new_with_specifics(host_refresh()),
            disks: Disks::new_with_refreshed_list_specifics(DiskRefreshKind::everything()),
            nets: Networks::new_with_refreshed_list(),
            seen_disks: HashSet::new(),
            seen_nets: HashSet::new(),
            first: true,
        };
        c.sys
            .refresh_processes_specifics(ProcessesToUpdate::All, true, process_refresh());
        c.seen_disks = c
            .disks
            .iter()
            .map(|d| d.mount_point().to_string_lossy().into_owned())
            .collect();
        c.seen_nets = c.nets.keys().cloned().collect();
        c
    }

    /// Measure everything and return the sample. `services` are the
    /// configured services, in config order.
    ///
    /// The first sample after [`Collector::new`] usually covers less than a
    /// whole interval, so its interval amounts (disk I/O, network traffic,
    /// and per-service disk I/O) are `None`. CPU use is a rate and is
    /// recorded as usual.
    pub fn sample(&mut self, rata: &Rata, cfg: &MonitorConfig, services: &[Service]) -> Sample {
        let first = std::mem::replace(&mut self.first, false);
        self.sys.refresh_specifics(host_refresh());
        self.sys
            .refresh_processes_specifics(ProcessesToUpdate::All, true, process_refresh());
        if cfg.system {
            self.disks
                .refresh_specifics(true, DiskRefreshKind::everything());
            self.nets.refresh(true);
        }
        let time = RataTime::now();
        let services = self.services(time, rata, services, first);
        if !cfg.system {
            return Sample {
                time,
                system: None,
                disks: Vec::new(),
                net: Vec::new(),
                services,
            };
        }
        Sample {
            time,
            system: Some(self.system_row(time)),
            disks: self.disk_rows(time, cfg, first),
            net: self.net_rows(time, cfg, first),
            services,
        }
    }

    fn system_row(&self, time: RataTime) -> SystemRow {
        let load = if cfg!(windows) {
            None
        } else {
            let l = System::load_average();
            Some([l.one, l.five, l.fifteen])
        };
        SystemRow {
            time,
            cpu_pct: f64::from(self.sys.global_cpu_usage()),
            load,
            cores: self.sys.cpus().len() as u64,
            mem_total: self.sys.total_memory(),
            // Not sysinfo's used_memory(), whose meaning differs between
            // platforms: this way used + avail = total everywhere.
            mem_used: self
                .sys
                .total_memory()
                .saturating_sub(self.sys.available_memory()),
            mem_avail: self.sys.available_memory(),
            swap_total: self.sys.total_swap(),
            swap_used: self.sys.used_swap(),
            uptime_s: System::uptime(),
        }
    }

    fn disk_rows(&mut self, time: RataTime, cfg: &MonitorConfig, first: bool) -> Vec<DiskRow> {
        let mut rows = Vec::new();
        let mut mounts = HashSet::new();
        for d in self.disks.iter() {
            let mount = d.mount_point().to_string_lossy().into_owned();
            let fs = d.file_system().to_string_lossy().into_owned();
            if PSEUDO_FS.contains(&fs.as_str())
                || PSEUDO_MOUNTS.iter().any(|p| mount.starts_with(p))
                || cfg.excludes_disk(&mount)
                || d.total_space() == 0
                || !mounts.insert(mount.clone())
            {
                continue;
            }
            // A newly mounted disk's first I/O figures are totals, not
            // amounts during the interval.
            let new = self.seen_disks.insert(mount.clone()) || first;
            let usage = d.usage();
            let total = d.total_space();
            let avail = d.available_space().min(total);
            rows.push(DiskRow {
                time,
                mount,
                fs,
                total,
                used: total - avail,
                avail,
                read: (!new).then_some(usage.read_bytes),
                written: (!new).then_some(usage.written_bytes),
            });
        }
        rows.sort_by(|a, b| a.mount.cmp(&b.mount));
        rows
    }

    fn net_rows(&mut self, time: RataTime, cfg: &MonitorConfig, first: bool) -> Vec<NetRow> {
        let mut rows = Vec::new();
        for (name, n) in self.nets.iter() {
            let loopback = n.ip_networks().iter().any(|ip| match ip.addr {
                IpAddr::V4(a) => a.is_loopback(),
                IpAddr::V6(a) => a.is_loopback(),
            });
            if loopback
                || name == "lo"
                || name == "lo0"
                || (n.total_received() == 0 && n.total_transmitted() == 0)
                || cfg.excludes_net(name)
            {
                continue;
            }
            let new = self.seen_nets.insert(name.clone()) || first;
            let v = |x: u64| (!new).then_some(x);
            rows.push(NetRow {
                time,
                iface: name.clone(),
                rx_bytes: v(n.received()),
                tx_bytes: v(n.transmitted()),
                rx_packets: v(n.packets_received()),
                tx_packets: v(n.packets_transmitted()),
                rx_errors: v(n.errors_on_received()),
                tx_errors: v(n.errors_on_transmitted()),
            });
        }
        rows.sort_by(|a, b| a.iface.cmp(&b.iface));
        rows
    }

    fn services(
        &self,
        time: RataTime,
        rata: &Rata,
        services: &[Service],
        first: bool,
    ) -> Vec<ServiceRow> {
        let table = process::table_from_system(&self.sys, true);
        services
            .iter()
            .map(|svc| {
                let matched = table.find(&svc.match_command(rata));
                if matched.is_empty() {
                    return ServiceRow::stopped(time, &svc.name);
                }
                // On Windows, every service has a conhost.exe child: the
                // hidden console rata gives it. It is not part of the
                // service, so it (and anything under it) is left out.
                let hosts: Vec<u32> = table
                    .processes()
                    .iter()
                    .filter(|p| cfg!(windows) && p.name.eq_ignore_ascii_case("conhost.exe"))
                    .flat_map(|p| std::iter::once(p.pid).chain(table.descendants(p.pid)))
                    .collect();
                let mut tree: Vec<u32> = Vec::new();
                for p in &matched {
                    for pid in std::iter::once(p.pid).chain(table.descendants(p.pid)) {
                        if !tree.contains(&pid) && !hosts.contains(&pid) {
                            tree.push(pid);
                        }
                    }
                }
                let (mut cpu, mut rss, mut read, mut written) = (0f64, 0u64, 0u64, 0u64);
                for pid in &tree {
                    if let Some(p) = self.sys.process(Pid::from_u32(*pid)) {
                        cpu += f64::from(p.cpu_usage());
                        rss += p.memory();
                        let du = p.disk_usage();
                        read += du.read_bytes;
                        written += du.written_bytes;
                    }
                }
                let uptime = matched
                    .iter()
                    .filter_map(|p| self.sys.process(Pid::from_u32(p.pid)))
                    .map(|p| p.run_time())
                    .max();
                ServiceRow {
                    time,
                    service: svc.name.clone(),
                    state: State::Running,
                    pid: Some(matched[0].pid),
                    procs: Some(tree.len() as u64),
                    cpu_pct: Some(cpu),
                    rss: Some(rss),
                    disk_read: (!first).then_some(read),
                    disk_written: (!first).then_some(written),
                    uptime_s: uptime,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_leaves_interval_amounts_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let rata = Rata::from_path(tmp.path()).unwrap();
        let cfg = MonitorConfig::defaults(&rata.monitor_config_path());
        let me = Service {
            name: "ME".into(),
            description: None,
            start: crate::config::CommandSpec::Args(std::env::args().collect::<Vec<_>>()),
            psline: None,
            stop_timeout: None,
            explicit_start: false,
            explicit_stop: false,
        };
        let mut c = Collector::new();
        let s = c.sample(&rata, &cfg, std::slice::from_ref(&me));
        let sys = s.system.as_ref().unwrap();
        assert!(sys.mem_total > 0 && sys.cores > 0);
        assert!(
            s.disks
                .iter()
                .all(|d| d.read.is_none() && d.written.is_none())
        );
        assert!(
            s.net
                .iter()
                .all(|n| n.rx_bytes.is_none() && n.tx_errors.is_none())
        );
        let row = &s.services[0];
        assert_eq!(row.state, State::Running, "the test process matches itself");
        assert!(row.cpu_pct.is_some() && row.rss.is_some());
        assert_eq!((row.disk_read, row.disk_written), (None, None));

        let s = c.sample(&rata, &cfg, std::slice::from_ref(&me));
        assert!(
            s.disks
                .iter()
                .all(|d| d.read.is_some() && d.written.is_some())
        );
        assert!(s.net.iter().all(|n| n.rx_bytes.is_some()));
        assert!(s.services[0].disk_read.is_some());
    }
}
