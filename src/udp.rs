//! SOCKS5 UDP ASSOCIATE (RFC 1928, section 7): relaying a client's UDP
//! datagrams out over one of the links.
//!
//! The client asks for a relay over its TCP connection, then sends each
//! datagram to the relay's UDP port with a small header naming the
//! destination. The relay sends the payload on from a socket pinned to one
//! link, and wraps replies in the same header on the way back. The session
//! lasts as long as the client keeps its TCP connection open.
//!
//! A whole session uses one link: games, voice calls and QUIC expect every
//! packet of a session to come from the same address, so spreading datagrams
//! over links would break them. Separate sessions are spread like TCP
//! connections.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use tokio::io::AsyncReadExt;
use tokio::net::{TcpStream, UdpSocket};

use crate::{ActiveGuard, Balancer, REP_GENERAL_FAILURE, REP_NETWORK_UNREACHABLE, REP_SUCCESS, Target, Via, net, resolve, send_reply};

/// The largest UDP payload, plus room for the SOCKS header.
const DATAGRAM: usize = 65_535;
/// RSV(2) FRAG(1) ATYP(1) IPv4(4) PORT(2): the header on datagrams to the client.
const REPLY_HEADER: usize = 10;

/// Serves one UDP ASSOCIATE request on `control`, until the client closes it.
pub(crate) async fn associate(mut control: TcpStream, balancer: Arc<Balancer>) {
    let (Ok(SocketAddr::V4(peer)), Ok(SocketAddr::V4(local))) = (control.peer_addr(), control.local_addr()) else {
        let _ = send_reply(&mut control, REP_GENERAL_FAILURE, None).await;
        return;
    };

    // Only adapter links can carry UDP; upstream SOCKS proxies (-tunnel) can't.
    let links = balancer.links();
    let not_adapter: Vec<bool> = links.iter().map(|l| !matches!(l.via, Via::Adapter(_))).collect();
    let Some(link) = balancer.pick(&links, &not_adapter).map(|i| links[i].clone()) else {
        let _ = send_reply(&mut control, REP_NETWORK_UNREACHABLE, None).await;
        return;
    };
    let Via::Adapter(egress) = &link.via else { return };

    // The relay listens on the address the client reached us on.
    let relay = match UdpSocket::bind(SocketAddrV4::new(*local.ip(), 0)).await {
        Ok(s) => s,
        Err(e) => {
            crate::log_debug(format!("UDP relay: could not open a port: {e}"));
            let _ = send_reply(&mut control, REP_GENERAL_FAILURE, None).await;
            return;
        }
    };
    let mut out = match net::udp_socket(egress) {
        Ok(s) => s,
        Err(e) => {
            crate::log_debug(format!("UDP relay: could not open a socket on {}: {e}", egress.ip));
            let _ = send_reply(&mut control, REP_GENERAL_FAILURE, None).await;
            return;
        }
    };
    let bound = match relay.local_addr() {
        Ok(SocketAddr::V4(a)) => a,
        _ => return,
    };
    if send_reply(&mut control, REP_SUCCESS, Some(bound)).await.is_err() {
        return;
    }

    let _guard = ActiveGuard::new(&link);
    link.connections.fetch_add(1, Relaxed);
    crate::log_debug(format!("UDP relay {bound} for {peer} via {}", egress.ip));

    let mut client: Option<SocketAddr> = None;
    let mut names: HashMap<(String, u16), SocketAddrV4> = HashMap::new();
    let mut from_client = vec![0u8; DATAGRAM];
    let mut from_remote = vec![0u8; DATAGRAM + REPLY_HEADER];
    let mut ignored = [0u8; 64];
    let mut fell_back = false;

    loop {
        tokio::select! {
            // The session ends when the client closes its TCP connection.
            read = control.read(&mut ignored) => {
                if matches!(read, Ok(0) | Err(_)) {
                    break;
                }
            }

            got = relay.recv_from(&mut from_client) => {
                let Ok((n, from)) = got else { continue };
                // Only the client that asked, from the first port it uses.
                if from.ip() != SocketAddr::V4(peer).ip() || client.is_some_and(|c| c != from) {
                    continue;
                }
                client = Some(from);
                let Some((target, payload)) = parse_datagram(&from_client[..n]) else { continue };
                let Some(dst) = destination(&target, &mut names).await else { continue };
                match out.send_to(payload, dst).await {
                    Ok(sent) => {
                        link.bytes_up.fetch_add(sent as u64, Relaxed);
                    }
                    // Blocked by a VPN such as Cloudflare WARP: let the
                    // system route this session (through the VPN) instead.
                    Err(e) if e.kind() == io::ErrorKind::PermissionDenied && balancer.unpinned_fallback && !fell_back => {
                        fell_back = true;
                        crate::warn_blocked(egress);
                        if let Ok(free) = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).await {
                            out = free;
                            if let Ok(sent) = out.send_to(payload, dst).await {
                                link.bytes_up.fetch_add(sent as u64, Relaxed);
                            }
                        }
                    }
                    Err(_) => {}
                }
            }

            got = out.recv_from(&mut from_remote[REPLY_HEADER..]) => {
                let Ok((n, SocketAddr::V4(from))) = got else { continue };
                let Some(client) = client else { continue };
                write_header(&mut from_remote[..REPLY_HEADER], from);
                if relay.send_to(&from_remote[..REPLY_HEADER + n], client).await.is_ok() {
                    link.bytes_down.fetch_add(n as u64, Relaxed);
                }
            }
        }
    }
}

/// Splits a datagram from the client into its destination and payload.
/// Fragmented datagrams (FRAG != 0) and IPv6 destinations are dropped.
fn parse_datagram(d: &[u8]) -> Option<(Target, &[u8])> {
    if d.len() < 4 || d[0] != 0 || d[1] != 0 || d[2] != 0 {
        return None;
    }
    match d[3] {
        1 if d.len() >= 10 => {
            let ip = Ipv4Addr::new(d[4], d[5], d[6], d[7]);
            let port = u16::from_be_bytes([d[8], d[9]]);
            Some((Target::Ip(SocketAddrV4::new(ip, port)), &d[10..]))
        }
        3 if d.len() >= 5 => {
            let len = d[4] as usize;
            let end = 5 + len;
            if d.len() < end + 2 {
                return None;
            }
            let host = String::from_utf8_lossy(&d[5..end]).into_owned();
            let port = u16::from_be_bytes([d[end], d[end + 1]]);
            Some((Target::Domain(host, port), &d[end + 2..]))
        }
        _ => None,
    }
}

/// The header in front of a datagram sent back to the client: where it came from.
fn write_header(h: &mut [u8], from: SocketAddrV4) {
    h[..4].copy_from_slice(&[0, 0, 0, 1]);
    h[4..8].copy_from_slice(&from.ip().octets());
    h[8..10].copy_from_slice(&from.port().to_be_bytes());
}

/// The address to send to; names are looked up once per session.
async fn destination(target: &Target, names: &mut HashMap<(String, u16), SocketAddrV4>) -> Option<SocketAddrV4> {
    match target {
        Target::Ip(a) => Some(*a),
        Target::Domain(host, port) => {
            let key = (host.clone(), *port);
            if let Some(a) = names.get(&key) {
                return Some(*a);
            }
            let a = *resolve(target).await.first()?;
            names.insert(key, a);
            Some(a)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ipv4_and_name_datagrams() {
        let d = [0, 0, 0, 1, 8, 8, 4, 4, 0, 53, b'h', b'i'];
        let (t, p) = parse_datagram(&d).unwrap();
        assert!(matches!(t, Target::Ip(a) if a == SocketAddrV4::new(Ipv4Addr::new(8, 8, 4, 4), 53)));
        assert_eq!(p, b"hi");

        let mut d = vec![0, 0, 0, 3, 11];
        d.extend_from_slice(b"example.com");
        d.extend_from_slice(&443u16.to_be_bytes());
        d.extend_from_slice(b"x");
        let (t, p) = parse_datagram(&d).unwrap();
        assert!(matches!(t, Target::Domain(ref h, 443) if h == "example.com"));
        assert_eq!(p, b"x");
    }

    #[test]
    fn drops_fragments_ipv6_and_short_datagrams() {
        assert!(parse_datagram(&[0, 0, 1, 1, 1, 2, 3, 4, 0, 80]).is_none()); // FRAG = 1
        assert!(parse_datagram(&[0, 0, 0, 4, 0, 0]).is_none()); // IPv6
        assert!(parse_datagram(&[0, 0, 0, 1, 1, 2]).is_none()); // cut short
        assert!(parse_datagram(&[0, 0, 0, 3, 20, b'a']).is_none()); // name cut short
    }

    #[tokio::test]
    async fn relays_a_session_end_to_end() {
        use std::time::Duration;
        use tokio::io::AsyncWriteExt;
        use tokio::time::timeout;

        // A UDP echo server standing in for a game or DNS server.
        let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let SocketAddr::V4(echo_addr) = echo.local_addr().unwrap() else {
            panic!()
        };
        tokio::spawn(async move {
            let mut b = [0u8; 1500];
            while let Ok((n, from)) = echo.recv_from(&mut b).await {
                let _ = echo.send_to(&b[..n], from).await;
            }
        });

        // The proxy, with this PC's loopback as its only link.
        let lo = crate::Egress {
            ip: Ipv4Addr::LOCALHOST,
            if_index: None,
            if_name: String::new(),
        };
        let balancer = Arc::new(Balancer::new(vec![(Via::Adapter(lo), 1)], Duration::from_secs(4), None));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = listener.local_addr().unwrap();
        let b = balancer.clone();
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            crate::handle_socks(s, b).await;
        });

        // SOCKS5: no authentication, then UDP ASSOCIATE from 0.0.0.0:0.
        let mut control = TcpStream::connect(proxy).await.unwrap();
        control.write_all(&[5, 1, 0]).await.unwrap();
        let mut choice = [0u8; 2];
        control.read_exact(&mut choice).await.unwrap();
        assert_eq!(choice, [5, 0]);
        control.write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
        let mut reply = [0u8; 10];
        control.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[..4], [5, REP_SUCCESS, 0, 1]);
        let relay = SocketAddrV4::new(
            Ipv4Addr::new(reply[4], reply[5], reply[6], reply[7]),
            u16::from_be_bytes([reply[8], reply[9]]),
        );

        // A datagram to the echo server comes back, wrapped in the header.
        let me = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut d = vec![0, 0, 0, 1, 127, 0, 0, 1];
        d.extend_from_slice(&echo_addr.port().to_be_bytes());
        d.extend_from_slice(b"ping");
        me.send_to(&d, relay).await.unwrap();
        let mut got = [0u8; 100];
        let n = timeout(Duration::from_secs(2), me.recv(&mut got)).await.expect("no reply").unwrap();
        assert_eq!(got[..n], d[..]); // same header (from the echo server) and payload

        // Another program on this PC can't use the client's relay.
        let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        stranger.send_to(&d, relay).await.unwrap();
        assert!(timeout(Duration::from_millis(300), stranger.recv(&mut got)).await.is_err());

        // Counted on the link like TCP traffic.
        let link = &balancer.links()[0];
        assert_eq!(link.bytes_up.load(Relaxed), 4);
        assert_eq!(link.bytes_down.load(Relaxed), 4);
        assert_eq!(link.active.load(Relaxed), 1);

        // Closing the TCP connection ends the session.
        drop(control);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(link.active.load(Relaxed), 0);
    }

    #[tokio::test]
    async fn refuses_udp_in_tunnel_mode() {
        use std::time::Duration;
        use tokio::io::AsyncWriteExt;

        let up = crate::Upstream {
            host: "127.0.0.1".into(),
            port: 1,
        };
        let balancer = Arc::new(Balancer::new(vec![(Via::Upstream(up), 1)], Duration::from_secs(4), None));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            crate::handle_socks(s, balancer).await;
        });
        let mut control = TcpStream::connect(proxy).await.unwrap();
        control.write_all(&[5, 1, 0, 5, 3, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
        let mut reply = [0u8; 12];
        control.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[2..4], [5, REP_NETWORK_UNREACHABLE]);
    }

    #[test]
    fn writes_reply_headers() {
        let mut h = [9u8; REPLY_HEADER];
        write_header(&mut h, SocketAddrV4::new(Ipv4Addr::new(1, 2, 3, 4), 0x1234));
        assert_eq!(h, [0, 0, 0, 1, 1, 2, 3, 4, 0x12, 0x34]);
    }
}
