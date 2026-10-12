//! -auto: use every network adapter that can reach the internet, and follow
//! network changes while running (a cable plugged in, Wi-Fi joining or
//! dropping, a phone tethered), without restarting.
//!
//! An adapter counts as usable when it has an IPv4 address and a default
//! route (0.0.0.0/0), i.e. a gateway to the internet.
//!   * Windows: the routing table from GetIpForwardTable2, and change
//!     notifications from NotifyUnicastIpAddressChange / NotifyRouteChange2,
//!     so nothing runs until the network actually changes.
//!   * Linux: /proc/net/route, checked every few seconds.

use std::time::Duration;

use crate::net::Egress;

/// Network changes arrive in bursts while DHCP and routes settle; wait this
/// long after the first one before looking.
const SETTLE: Duration = Duration::from_secs(3);
#[cfg(not(windows))]
const POLL: Duration = Duration::from_secs(5);

/// Adapters that can currently reach the internet, sorted by address.
pub(crate) fn detect() -> Vec<Egress> {
    let gateways = default_route_interfaces();
    let mut found: Vec<Egress> = crate::local_ipv4s()
        .into_iter()
        .filter(|e| !e.ip.is_link_local() && gateways.has(e))
        .collect();
    found.sort_by_key(|e| e.ip);
    found.dedup_by_key(|e| e.ip);
    found
}

/// Starts listening for network changes. Call before the first `detect()`,
/// so a change in between is not missed.
pub(crate) fn start_watching() {
    #[cfg(windows)]
    windows::subscribe();
}

/// Waits until the network may have changed.
pub(crate) async fn changed() {
    #[cfg(windows)]
    {
        if windows::subscribe() {
            windows::NOTIFY.notified().await;
        } else {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }
    #[cfg(not(windows))]
    tokio::time::sleep(POLL).await;

    tokio::time::sleep(SETTLE).await;
}

// ------------------------------------------------------------------ Windows

#[cfg(windows)]
struct Gateways(Vec<u32>); // interface indexes with a default route

#[cfg(windows)]
impl Gateways {
    fn has(&self, e: &Egress) -> bool {
        e.if_index.is_some_and(|i| self.0.contains(&i))
    }
}

#[cfg(windows)]
fn default_route_interfaces() -> Gateways {
    use windows_sys::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIpForwardTable2, MIB_IPFORWARD_TABLE2};
    use windows_sys::Win32::Networking::WinSock::AF_INET;

    let mut indexes = Vec::new();
    unsafe {
        let mut table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
        if GetIpForwardTable2(AF_INET, &mut table) != 0 || table.is_null() {
            return Gateways(indexes);
        }
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        for row in rows {
            let is_default = row.DestinationPrefix.PrefixLength == 0 && row.DestinationPrefix.Prefix.Ipv4.sin_addr.S_un.S_addr == 0;
            if is_default && !indexes.contains(&row.InterfaceIndex) {
                indexes.push(row.InterfaceIndex);
            }
        }
        FreeMibTable(table as *const _);
    }
    Gateways(indexes)
}

#[cfg(windows)]
mod windows {
    use std::sync::OnceLock;
    use tokio::sync::Notify;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        MIB_IPFORWARD_ROW2, MIB_NOTIFICATION_TYPE, MIB_UNICASTIPADDRESS_ROW, NotifyRouteChange2, NotifyUnicastIpAddressChange,
    };
    use windows_sys::Win32::Networking::WinSock::AF_INET;

    /// Woken by Windows on every IPv4 address or route change. notify_one
    /// stores a permit when nobody is waiting, so a change is never missed.
    pub(super) static NOTIFY: Notify = Notify::const_new();
    static SUBSCRIBED: OnceLock<bool> = OnceLock::new();

    unsafe extern "system" fn on_address(_: *const core::ffi::c_void, _: *const MIB_UNICASTIPADDRESS_ROW, _: MIB_NOTIFICATION_TYPE) {
        NOTIFY.notify_one();
    }

    unsafe extern "system" fn on_route(_: *const core::ffi::c_void, _: *const MIB_IPFORWARD_ROW2, _: MIB_NOTIFICATION_TYPE) {
        NOTIFY.notify_one();
    }

    /// Registers for address and route changes on first call. Returns false
    /// if Windows refused, in which case the caller falls back to polling.
    pub(super) fn subscribe() -> bool {
        *SUBSCRIBED.get_or_init(|| {
            // The handles are never cancelled: we listen for the whole life
            // of the process.
            let ok = unsafe {
                let mut h1: HANDLE = std::ptr::null_mut();
                let mut h2: HANDLE = std::ptr::null_mut();
                NotifyUnicastIpAddressChange(AF_INET, Some(on_address), std::ptr::null(), false, &mut h1) == 0
                    && NotifyRouteChange2(AF_INET, Some(on_route), std::ptr::null(), false, &mut h2) == 0
            };
            if !ok {
                crate::log_warn("Could not subscribe to network changes; checking every 10 s instead".into());
            }
            ok
        })
    }
}

// ------------------------------------------------------------------ Linux and others

#[cfg(not(windows))]
struct Gateways(Vec<String>); // interface names with a default route

#[cfg(not(windows))]
impl Gateways {
    fn has(&self, e: &Egress) -> bool {
        self.0.contains(&e.if_name)
    }
}

#[cfg(not(windows))]
fn default_route_interfaces() -> Gateways {
    Gateways(parse_proc_route(&std::fs::read_to_string("/proc/net/route").unwrap_or_default()))
}

/// Interface names that have a default route in /proc/net/route.
#[cfg_attr(windows, allow(dead_code))]
fn parse_proc_route(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        // Iface Destination Gateway Flags RefCnt Use Metric Mask ...
        if cols.len() >= 8 && cols[1] == "00000000" && cols[7] == "00000000" && !names.iter().any(|n| n == cols[0]) {
            names.push(cols[0].to_string());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_route_default_interfaces() {
        let table = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                     eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
                     eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n\
                     wlan0\t00000000\t012BA8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
                     docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0\n";
        assert_eq!(parse_proc_route(table), vec!["eth0".to_string(), "wlan0".to_string()]);
    }
}
