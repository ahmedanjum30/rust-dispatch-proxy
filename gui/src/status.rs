//! Reads the proxy's /status.json once a second on a background thread.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU16, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

#[derive(Clone, Deserialize)]
pub struct Status {
    pub version: String,
    pub uptime_s: u64,
    pub links: Vec<LinkStatus>,
}

#[derive(Clone, Deserialize)]
pub struct LinkStatus {
    pub link: String,
    pub adapter: String,
    pub active: u64,
    pub connections: u64,
    pub failures: u64,
    pub connect_ms: f64,
    pub bytes_down: u64,
    pub bytes_up: u64,
    pub cooldown_s: u64,
}

/// The latest reading; `status` is None while the proxy isn't answering.
#[derive(Clone)]
pub struct Reading {
    pub at: Instant,
    pub status: Option<Status>,
}

/// Fetches the status from a proxy on 127.0.0.1:`port`, if one answers.
pub fn fetch(port: u16) -> Option<Status> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(b"GET /status.json HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text.split_once("\r\n\r\n")?;
    if !head.starts_with("HTTP/1.1 200") {
        return None;
    }
    serde_json::from_str(body).ok()
}

/// Starts the poller. It reads the port to ask from `port` each time and
/// asks `repaint` to redraw after every reading.
pub fn spawn_poller(port: Arc<AtomicU16>, repaint: impl Fn() + Send + 'static) -> Arc<Mutex<Reading>> {
    let latest = Arc::new(Mutex::new(Reading {
        at: Instant::now(),
        status: None,
    }));
    let shared = latest.clone();
    std::thread::spawn(move || {
        loop {
            let status = fetch(port.load(Relaxed));
            *shared.lock().unwrap_or_else(|e| e.into_inner()) = Reading { at: Instant::now(), status };
            repaint();
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    latest
}
