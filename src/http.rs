//! HTTP proxy support, so Windows' own proxy setting (Settings > Network >
//! Proxy) can point at this program.
//!
//!   * HTTPS (and anything else) arrives as `CONNECT host:port`. We connect
//!     through the balancer and then just relay bytes; the encryption stays
//!     end to end between the app and the website.
//!   * Plain HTTP arrives as `GET http://host/path`. We rewrite it to a normal
//!     request, connect through the balancer, and relay. The request is sent
//!     with "Connection: close", so the website closes the connection after
//!     answering and the browser opens a new proxy connection for its next
//!     request. (A client that pipelines a second request before that close
//!     would have it sent to the same website; browsers do not do this.)

use std::io;
use std::net::Ipv4Addr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::{Balancer, COPY_BUFFER, HANDSHAKE_TIMEOUT, Target, dial};

const MAX_HEAD: usize = 64 * 1024;

/// Hop-by-hop headers that must not be forwarded to the website.
const DROP_HEADERS: [&str; 5] = ["proxy-connection", "connection", "keep-alive", "proxy-authorization", "te"];

#[derive(Debug, PartialEq)]
pub(crate) enum ParsedRequest {
    /// CONNECT host:port
    Tunnel { host: String, port: u16 },
    /// Plain HTTP: where to connect and the rewritten request head to send.
    Forward { host: String, port: u16, head: Vec<u8> },
}

/// Reads until the end of the request head. Returns (head length, buffer);
/// the buffer may already hold the first bytes of a request body.
async fn read_head(client: &mut TcpStream) -> io::Result<(usize, Vec<u8>)> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let n = client.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "client closed"));
        }
        let search_from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf[search_from..].windows(4).position(|w| w == b"\r\n\r\n") {
            return Ok((search_from + pos + 4, buf));
        }
        if buf.len() > MAX_HEAD {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "request head too large"));
        }
    }
}

/// Splits "host:port", "[v6]:port" or "host" (using `default_port`).
pub(crate) fn split_host_port(authority: &str, default_port: u16) -> Option<(String, u16)> {
    let authority = authority.rsplit_once('@').map(|(_, a)| a).unwrap_or(authority);
    if authority.is_empty() {
        return None;
    }
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if after.is_empty() => default_port,
            None => return None,
        };
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => Some((host.to_string(), port.parse().ok()?)),
        Some(_) => None,
        None => Some((authority.to_string(), default_port)),
    }
}

pub(crate) fn parse_request(head: &str) -> Result<ParsedRequest, &'static str> {
    let mut lines = head.split("\r\n");
    let request_line = lines.next().ok_or("empty request")?;
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next()) else {
        return Err("malformed request line");
    };
    if !version.starts_with("HTTP/") {
        return Err("not an HTTP request");
    }

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_host_port(target, 443).ok_or("bad CONNECT target")?;
        return Ok(ParsedRequest::Tunnel { host, port });
    }

    // Plain HTTP through a proxy uses an absolute URL: http://host[:port]/path
    let rest = target
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("http://"))
        .map(|_| &target[7..])
        .ok_or("only http:// URLs or CONNECT are supported")?;
    let split_at = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(split_at);
    let (host, port) = split_host_port(authority, 80).ok_or("bad host in URL")?;
    let path = match path {
        "" => "/".to_string(),
        p if p.starts_with('?') => format!("/{p}"),
        p => p.to_string(),
    };

    let mut out = format!("{method} {path} {version}\r\n");
    let mut has_host = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let name = line.split(':').next().unwrap_or("").trim().to_ascii_lowercase();
        if DROP_HEADERS.contains(&name.as_str()) {
            continue;
        }
        if name == "host" {
            has_host = true;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    if !has_host {
        out.push_str(&format!("Host: {authority}\r\n"));
    }
    out.push_str("Connection: close\r\n\r\n");
    Ok(ParsedRequest::Forward {
        host,
        port,
        head: out.into_bytes(),
    })
}

async fn respond_error(client: &mut TcpStream, status: &str) {
    let body = format!("rust-dispatch-proxy: {status}\n");
    let msg = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = client.write_all(msg.as_bytes()).await;
}

pub(crate) async fn handle_http(mut client: TcpStream, balancer: Arc<Balancer>) {
    let _ = client.set_nodelay(true);

    let (head_len, buf) = match timeout(HANDSHAKE_TIMEOUT, read_head(&mut client)).await {
        Ok(Ok(v)) => v,
        _ => return,
    };
    let Ok(head) = std::str::from_utf8(&buf[..head_len]) else {
        respond_error(&mut client, "400 Bad Request").await;
        return;
    };
    let early_body = &buf[head_len..];

    let request = match parse_request(head) {
        Ok(r) => r,
        Err(why) => {
            crate::log_debug(format!("rejected HTTP request: {why}"));
            respond_error(&mut client, "400 Bad Request").await;
            return;
        }
    };

    let (host, port) = match &request {
        ParsedRequest::Tunnel { host, port } | ParsedRequest::Forward { host, port, .. } => (host.clone(), *port),
    };
    let target = match host.parse::<Ipv4Addr>() {
        Ok(ip) => Target::Ip(std::net::SocketAddrV4::new(ip, port)),
        Err(_) => Target::Domain(host, port),
    };

    let (mut remote, _guard) = match dial(&balancer, &target).await {
        Ok(ok) => ok,
        Err(e) => {
            let status = if e.kind() == io::ErrorKind::TimedOut {
                "504 Gateway Timeout"
            } else {
                "502 Bad Gateway"
            };
            respond_error(&mut client, status).await;
            return;
        }
    };

    let sent = match &request {
        ParsedRequest::Tunnel { .. } => client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await,
        ParsedRequest::Forward { head, .. } => remote.write_all(head).await,
    };
    if sent.is_err() {
        return;
    }
    if !early_body.is_empty() && remote.write_all(early_body).await.is_err() {
        return;
    }

    let _ = tokio::io::copy_bidirectional_with_sizes(&mut client, &mut remote, COPY_BUFFER, COPY_BUFFER).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_forms() {
        assert_eq!(split_host_port("example.com:443", 80), Some(("example.com".into(), 443)));
        assert_eq!(split_host_port("example.com", 80), Some(("example.com".into(), 80)));
        assert_eq!(split_host_port("1.2.3.4:8080", 80), Some(("1.2.3.4".into(), 8080)));
        assert_eq!(split_host_port("[::1]:443", 80), Some(("::1".into(), 443)));
        assert_eq!(split_host_port("user:pw@host.com:81", 80), Some(("host.com".into(), 81)));
        assert_eq!(split_host_port("host:notaport", 80), None);
        assert_eq!(split_host_port("", 80), None);
    }

    #[test]
    fn connect_request() {
        let r = parse_request("CONNECT www.google.com:443 HTTP/1.1\r\nHost: www.google.com:443\r\n\r\n").unwrap();
        assert_eq!(
            r,
            ParsedRequest::Tunnel {
                host: "www.google.com".into(),
                port: 443
            }
        );
    }

    #[test]
    fn plain_http_is_rewritten() {
        let head = "GET http://example.com:8080/a/b?x=1 HTTP/1.1\r\nHost: example.com:8080\r\n\
                    Proxy-Connection: keep-alive\r\nConnection: keep-alive\r\nAccept: */*\r\n\r\n";
        let ParsedRequest::Forward { host, port, head } = parse_request(head).unwrap() else {
            panic!()
        };
        assert_eq!((host.as_str(), port), ("example.com", 8080));
        let head = String::from_utf8(head).unwrap();
        assert!(head.starts_with("GET /a/b?x=1 HTTP/1.1\r\n"));
        assert!(head.contains("Host: example.com:8080\r\n"));
        assert!(head.contains("Accept: */*\r\n"));
        assert!(!head.to_lowercase().contains("keep-alive"));
        assert!(head.ends_with("Connection: close\r\n\r\n"));
    }

    #[test]
    fn plain_http_root_and_query_paths() {
        for (url, expect) in [("http://h.com", "GET / "), ("http://h.com?q", "GET /?q "), ("HTTP://H.com/x", "GET /x ")] {
            let ParsedRequest::Forward { head, .. } = parse_request(&format!("GET {url} HTTP/1.1\r\n\r\n")).unwrap() else {
                panic!()
            };
            assert!(String::from_utf8(head).unwrap().starts_with(expect), "{url}");
        }
    }

    #[test]
    fn rejects_non_proxy_requests() {
        assert!(parse_request("GET /index.html HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_request("GET https://x.com/ HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_request("garbage\r\n\r\n").is_err());
    }
}
