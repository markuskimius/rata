//! Dropped-packet counters per network interface, which sysinfo does not
//! provide.
//!
//! | Platform | Received | Sent |
//! |---|---|---|
//! | Linux | `/sys/class/net/<if>/statistics/rx_dropped` | `.../tx_dropped` |
//! | macOS | `ifi_iqdrops` (input queue drops) | `ifm_snd_drops` (send queue drops) |
//! | Windows | `InDiscards` (`GetIfTable2`) | `OutDiscards` |
//!
//! Interface names match sysinfo's. Values are totals since the interface
//! came up; the collector turns them into amounts per interval. Other
//! platforms report nothing, and the columns stay empty.

use std::collections::HashMap;

/// Total dropped packets on one interface, where the platform reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Drops {
    /// Packets dropped on receive.
    pub rx: Option<u64>,
    /// Packets dropped on send.
    pub tx: Option<u64>,
}

/// The current drop totals of every interface, by name.
pub fn read() -> HashMap<String, Drops> {
    platform::read()
}

#[cfg(target_os = "linux")]
mod platform {
    use super::Drops;
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;

    pub fn read() -> HashMap<String, Drops> {
        let mut out = HashMap::new();
        let Ok(dir) = fs::read_dir("/sys/class/net") else {
            return out;
        };
        for entry in dir.flatten() {
            let stats = entry.path().join("statistics");
            let get = |file: &str| -> Option<u64> {
                fs::read_to_string(Path::new(&stats).join(file))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()
            };
            out.insert(
                entry.file_name().to_string_lossy().into_owned(),
                Drops {
                    rx: get("rx_dropped"),
                    tx: get("tx_dropped"),
                },
            );
        }
        out
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::Drops;
    use std::collections::HashMap;
    use std::ptr::null_mut;

    /// Reads the same `NET_RT_IFLIST2` sysctl as sysinfo.
    pub fn read() -> HashMap<String, Drops> {
        let mut out = HashMap::new();
        let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
        let mut len: libc::size_t = 0;
        // SAFETY: sysctl with a valid MIB; the first call only sizes the
        // buffer, the second fills it, and every message is read within the
        // length the kernel returned.
        unsafe {
            if libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as _,
                null_mut(),
                &mut len,
                null_mut(),
                0,
            ) < 0
            {
                return out;
            }
            // Room for interfaces that appear between the two calls.
            len += len / 8;
            let mut buf = vec![0u8; len];
            if libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as _,
                buf.as_mut_ptr().cast(),
                &mut len,
                null_mut(),
                0,
            ) < 0
            {
                return out;
            }
            let mut off = 0usize;
            while off + size_of::<libc::if_msghdr>() <= len {
                let ifm = buf.as_ptr().add(off).cast::<libc::if_msghdr>();
                let msglen = std::ptr::read_unaligned(&raw const (*ifm).ifm_msglen) as usize;
                if msglen == 0 {
                    break;
                }
                let kind = std::ptr::read_unaligned(&raw const (*ifm).ifm_type);
                if kind == libc::RTM_IFINFO2 as u8 && off + size_of::<libc::if_msghdr2>() <= len {
                    let m: libc::if_msghdr2 =
                        std::ptr::read_unaligned(buf.as_ptr().add(off).cast());
                    let mut name = [0 as libc::c_char; libc::IFNAMSIZ + 6];
                    let p = libc::if_indextoname(m.ifm_index as _, name.as_mut_ptr());
                    if !p.is_null() {
                        let name = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
                        out.insert(
                            name,
                            Drops {
                                rx: Some(m.ifm_data.ifi_iqdrops),
                                tx: u64::try_from(m.ifm_snd_drops).ok(),
                            },
                        );
                    }
                }
                off += msglen;
            }
        }
        out
    }
}

#[cfg(windows)]
mod platform {
    use super::Drops;
    use std::collections::HashMap;
    use windows_sys::Win32::Foundation::NO_ERROR;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GetIfTable2, MIB_IF_TABLE2,
    };

    /// Reads the same `GetIfTable2` table as sysinfo, which names
    /// interfaces by their alias.
    pub fn read() -> HashMap<String, Drops> {
        let mut out = HashMap::new();
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        // SAFETY: GetIfTable2 allocates the table; rows are read within
        // NumEntries, and the table is freed with FreeMibTable.
        unsafe {
            if GetIfTable2(&mut table) != NO_ERROR || table.is_null() {
                return out;
            }
            let n = (*table).NumEntries as usize;
            let rows = std::ptr::addr_of!((*table).Table)
                .cast::<windows_sys::Win32::NetworkManagement::IpHelper::MIB_IF_ROW2>();
            for i in 0..n {
                let row = &*rows.add(i);
                let end = row
                    .Alias
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(row.Alias.len());
                let Ok(name) = String::from_utf16(&row.Alias[..end]) else {
                    continue;
                };
                out.entry(name).or_insert(Drops {
                    rx: Some(row.InDiscards),
                    tx: Some(row.OutDiscards),
                });
            }
            FreeMibTable(table.cast());
        }
        out
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod platform {
    use super::Drops;
    use std::collections::HashMap;

    pub fn read() -> HashMap<String, Drops> {
        HashMap::new()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_without_failing() {
        let drops = super::read();
        if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
            assert!(!drops.is_empty(), "no interfaces found");
            assert!(drops.values().any(|d| d.rx.is_some()));
        }
    }
}
