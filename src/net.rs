//! Creating outgoing sockets that are pinned to one physical network adapter.
//!
//! Binding to the adapter's IP address usually decides the route on its own,
//! but pinning the socket to the adapter itself makes sure the operating
//! system never sends a connection out of a different adapter (for example
//! when two connections share a subnet, or a VPN adds its own routes).
//!
//! In TUN mode pinning is essential: the TUN adapter's routes are more
//! specific than the real default routes, so without it our own outgoing
//! traffic would be sent back into the TUN and loop forever.
//!   * Windows: IP_UNICAST_IF socket option with the adapter's interface index.
//!   * Linux:   SO_BINDTODEVICE with the adapter's name.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use tokio::net::TcpSocket;
#[cfg(feature = "tun")]
use tokio::net::UdpSocket;

/// Where an outgoing socket should leave the machine.
#[derive(Clone, Debug)]
pub struct Egress {
    pub ip: Ipv4Addr,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub if_index: Option<u32>,
    pub if_name: String,
}

pub fn tcp_socket(egress: &Egress) -> io::Result<TcpSocket> {
    let socket = TcpSocket::new_v4()?;
    pin_tcp(&socket, egress)?;
    socket.bind(SocketAddr::V4(SocketAddrV4::new(egress.ip, 0)))?;
    Ok(socket)
}

#[cfg(feature = "tun")]
pub fn udp_socket(egress: &Egress) -> io::Result<UdpSocket> {
    let std_socket = std::net::UdpSocket::bind(SocketAddrV4::new(egress.ip, 0))?;
    std_socket.set_nonblocking(true)?;
    let socket = UdpSocket::from_std(std_socket)?;
    pin_udp(&socket, egress)?;
    Ok(socket)
}

// ------------------------------------------------------------------ Windows

#[cfg(windows)]
fn set_unicast_if(raw: std::os::windows::io::RawSocket, if_index: u32) -> io::Result<()> {
    use windows_sys::Win32::Networking::WinSock::{setsockopt, WSAGetLastError, IPPROTO_IP, IP_UNICAST_IF, SOCKET_ERROR};
    // For IPv4 the interface index must be passed in network byte order.
    let value: u32 = if_index.to_be();
    let result = unsafe {
        setsockopt(
            raw as _,
            IPPROTO_IP,
            IP_UNICAST_IF,
            &value as *const u32 as *const u8,
            std::mem::size_of::<u32>() as i32,
        )
    };
    if result == SOCKET_ERROR {
        return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
    }
    Ok(())
}

#[cfg(windows)]
fn pin_tcp(socket: &TcpSocket, egress: &Egress) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    match egress.if_index {
        Some(index) => set_unicast_if(socket.as_raw_socket(), index),
        None => Ok(()),
    }
}

#[cfg(all(windows, feature = "tun"))]
fn pin_udp(socket: &UdpSocket, egress: &Egress) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    match egress.if_index {
        Some(index) => set_unicast_if(socket.as_raw_socket(), index),
        None => Ok(()),
    }
}

// ------------------------------------------------------------------ Linux

#[cfg(target_os = "linux")]
fn pin_tcp(socket: &TcpSocket, egress: &Egress) -> io::Result<()> {
    socket.bind_device(Some(egress.if_name.as_bytes()))
}

#[cfg(all(target_os = "linux", feature = "tun"))]
fn pin_udp(socket: &UdpSocket, egress: &Egress) -> io::Result<()> {
    socket.bind_device(Some(egress.if_name.as_bytes()))
}

// ------------------------------------------------------------------ others

#[cfg(not(any(windows, target_os = "linux")))]
fn pin_tcp(_: &TcpSocket, _: &Egress) -> io::Result<()> {
    Ok(())
}

#[cfg(all(not(any(windows, target_os = "linux")), feature = "tun"))]
fn pin_udp(_: &UdpSocket, _: &Egress) -> io::Result<()> {
    Ok(())
}
