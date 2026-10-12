//! System-wide mode: a virtual network adapter (Wintun on Windows) that
//! captures all IPv4 traffic and sends it out through the load balancer.
//!
//! How it fits together:
//!   1. Create the adapter and give it a private address (198.18.0.1).
//!   2. Add routes 0.0.0.0/1 and 128.0.0.0/1 through it. Together they cover
//!      every IPv4 address and beat the normal 0.0.0.0/0 default routes,
//!      without changing those routes. Local network subnets keep their more
//!      specific routes, so LAN traffic is untouched.
//!   3. A user-space TCP/IP stack (ipstack) turns the captured packets back
//!      into TCP connections and UDP flows.
//!   4. Each one is sent out a real adapter chosen by the balancer, using
//!      sockets pinned to that adapter (see net.rs) so nothing loops back.
//!
//! The adapter and its routes belong to this process: when it exits or is
//! killed, Windows removes them and traffic falls back to normal routing.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use ipstack::{IpStackStream, IpStackTcpStream, IpStackUdpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

use crate::net;
use crate::{dial, Balancer, Target, Via, COPY_BUFFER};

pub const TUN_NAME: &str = "DispatchProxy";
const TUN_ADDR: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 1);
const TUN_MASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 252);
const MTU: u16 = 9000; // large packets = fewer packets to process
const TCP_IDLE_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
const UDP_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const DNS_IDLE_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(windows)]
const ADAPTER_GUID: u128 = 0x6a3f_1c2e_9b47_4d18_a5e2_7f0c_3d91_b864;

fn to_io<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::other(e.to_string())
}

/// Creates the adapter and serves it until an unrecoverable error.
pub async fn run(balancer: Arc<Balancer>, connect_timeout: Duration) -> io::Result<()> {
    let mut config = tun::Configuration::default();
    config.tun_name(TUN_NAME).address(TUN_ADDR).netmask(TUN_MASK).mtu(MTU).up();

    #[cfg(windows)]
    {
        let dll = std::env::current_exe()?
            .parent()
            .map(|dir| dir.join("wintun.dll"))
            .ok_or_else(|| io::Error::other("cannot locate program folder"))?;
        if !dll.exists() {
            return Err(io::Error::other(format!("wintun.dll not found at {}", dll.display())));
        }
        config.metric(1);
        config.platform_config(|p| {
            p.device_guid(ADAPTER_GUID);
            p.wintun_file(dll.as_os_str().to_owned());
        });
    }

    #[cfg(target_os = "linux")]
    config.platform_config(|p| {
        p.ensure_root_privileges(true);
    });

    let device = tun::create_as_async(&config).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("denied") || msg.contains("Access") || msg.contains("privilege") {
            io::Error::other(format!("{msg} (TUN mode needs administrator rights)"))
        } else {
            io::Error::other(msg)
        }
    })?;

    #[cfg(windows)]
    {
        use tun::AbstractDeviceExt;
        let luid = device.tun_luid();
        add_route(luid, Ipv4Addr::new(0, 0, 0, 0), 1)?;
        add_route(luid, Ipv4Addr::new(128, 0, 0, 0), 1)?;
    }

    #[cfg(windows)]
    crate::log_info(format!(
        "TUN adapter '{TUN_NAME}' is up: all IPv4 traffic now goes through the balancer"
    ));
    #[cfg(not(windows))]
    crate::log_info(format!(
        "TUN device '{TUN_NAME}' is up. Routes are not added automatically on this platform; \
         route traffic into it yourself (e.g. 0.0.0.0/1 and 128.0.0.0/1 dev {TUN_NAME})"
    ));

    let mut stack_config = ipstack::IpStackConfig::default();
    stack_config.mtu(MTU).map_err(to_io)?;
    let mut tcp = ipstack::TcpConfig::default();
    tcp.timeout = TCP_IDLE_TIMEOUT;
    tcp.max_unacked_bytes = 1024 * 1024;
    tcp.read_buffer_size = 64 * 1024;
    tcp.options = Some(vec![ipstack::TcpOptions::MaximumSegmentSize(MTU - 40)]);
    stack_config.with_tcp_config(tcp);
    stack_config.udp_timeout(UDP_IDLE_TIMEOUT);

    let mut stack = ipstack::IpStack::new(stack_config, device);

    loop {
        match stack.accept().await.map_err(to_io)? {
            IpStackStream::Tcp(flow) => {
                tokio::spawn(handle_tcp(flow, balancer.clone(), connect_timeout));
            }
            IpStackStream::Udp(flow) => {
                tokio::spawn(handle_udp(flow, balancer.clone()));
            }
            // ICMP (ping) and other protocols are not forwarded.
            IpStackStream::UnknownTransport(_) | IpStackStream::UnknownNetwork(_) => {}
        }
    }
}

fn is_own_subnet(ip: &Ipv4Addr) -> bool {
    let mask = u32::from(TUN_MASK);
    u32::from(*ip) & mask == u32::from(TUN_ADDR) & mask
}

async fn handle_tcp(mut flow: IpStackTcpStream, balancer: Arc<Balancer>, connect_timeout: Duration) {
    let SocketAddr::V4(dst) = flow.peer_addr() else { return };
    if is_own_subnet(dst.ip()) {
        return;
    }
    // On failure the flow is dropped, which resets the app's connection.
    let Ok((mut remote, _guard)) = dial(&balancer, &Target::Ip(dst), connect_timeout).await else { return };
    let _ = tokio::io::copy_bidirectional_with_sizes(&mut flow, &mut remote, COPY_BUFFER, COPY_BUFFER).await;
    let _ = flow.shutdown().await;
}

enum UdpEvent {
    FromApp(io::Result<usize>),
    FromRemote(io::Result<usize>),
}

async fn handle_udp(mut flow: IpStackUdpStream, balancer: Arc<Balancer>) {
    let SocketAddr::V4(dst) = flow.peer_addr() else { return };
    if is_own_subnet(dst.ip()) {
        return;
    }

    // Pin this flow to one link, failing over if a socket can't be set up.
    let mut tried = vec![false; balancer.links.len()];
    let mut socket = None;
    while let Some(i) = balancer.pick(&tried) {
        tried[i] = true;
        // TUN mode is never combined with -tunnel, so links are adapters.
        let Via::Adapter(egress) = &balancer.links[i].via else { continue };
        match net::udp_socket(egress) {
            Ok(s) => match s.connect(SocketAddr::V4(dst)).await {
                Ok(()) => {
                    socket = Some(s);
                    break;
                }
                Err(e) => crate::log_debug(format!("UDP {dst} via {} failed: {e}", egress.ip)),
            },
            Err(e) => crate::log_debug(format!("UDP socket on {} failed: {e}", egress.ip)),
        }
    }
    let Some(socket) = socket else { return };

    let idle = if dst.port() == 53 { DNS_IDLE_TIMEOUT } else { UDP_IDLE_TIMEOUT };
    let mut from_app = vec![0u8; 65_535];
    let mut from_remote = vec![0u8; 65_535];

    loop {
        let event = timeout(idle, async {
            tokio::select! {
                r = flow.read(&mut from_app) => UdpEvent::FromApp(r),
                r = socket.recv(&mut from_remote) => UdpEvent::FromRemote(r),
            }
        })
        .await;

        match event {
            Ok(UdpEvent::FromApp(Ok(n))) if n > 0 => {
                if socket.send(&from_app[..n]).await.is_err() {
                    break;
                }
            }
            Ok(UdpEvent::FromRemote(Ok(n))) => {
                if flow.write(&from_remote[..n]).await.is_err() {
                    break;
                }
            }
            _ => break, // idle timeout, app side closed, or socket error
        }
    }
}

/// Adds an on-link route through the TUN adapter (Windows).
/// Routes are not persistent: they vanish with the adapter.
#[cfg(windows)]
fn add_route(luid: u64, destination: Ipv4Addr, prefix_len: u8) -> io::Result<()> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{CreateIpForwardEntry2, InitializeIpForwardEntry, MIB_IPFORWARD_ROW2};
    use windows_sys::Win32::NetworkManagement::Ndis::NET_LUID_LH;
    use windows_sys::Win32::Networking::WinSock::AF_INET;

    const ERROR_OBJECT_ALREADY_EXISTS: u32 = 5010;

    unsafe {
        let mut row: MIB_IPFORWARD_ROW2 = std::mem::zeroed();
        InitializeIpForwardEntry(&mut row);
        row.InterfaceLuid = NET_LUID_LH { Value: luid };
        row.DestinationPrefix.Prefix.si_family = AF_INET;
        row.DestinationPrefix.Prefix.Ipv4.sin_family = AF_INET;
        row.DestinationPrefix.Prefix.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes(destination.octets());
        row.DestinationPrefix.PrefixLength = prefix_len;
        // Next hop 0.0.0.0 = on-link: packets are handed straight to the adapter.
        row.NextHop.si_family = AF_INET;
        row.NextHop.Ipv4.sin_family = AF_INET;
        row.Metric = 0;
        row.Protocol = 3; // MIB_IPPROTO_NETMGMT

        let status = CreateIpForwardEntry2(&row);
        if status != 0 && status != ERROR_OBJECT_ALREADY_EXISTS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_subnet_is_ignored() {
        assert!(is_own_subnet(&Ipv4Addr::new(198, 18, 0, 0)));
        assert!(is_own_subnet(&Ipv4Addr::new(198, 18, 0, 2)));
        assert!(is_own_subnet(&Ipv4Addr::new(198, 18, 0, 3)));
        assert!(!is_own_subnet(&Ipv4Addr::new(198, 18, 0, 4)));
        assert!(!is_own_subnet(&Ipv4Addr::new(8, 8, 8, 8)));
    }
}

