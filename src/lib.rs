//! rust-dispatch-proxy
//!
//! A load-balancing proxy that spreads outgoing connections across several
//! local network interfaces (one per internet connection), so multi-connection
//! traffic uses all of them at once.
//!
//! ```text
//! rust-dispatch-proxy [-lhost 127.0.0.1] [-lport 8080] [-tun] [-quiet] IP[@weight] IP[@weight] ...
//! rust-dispatch-proxy -auto [-system-proxy]
//! rust-dispatch-proxy -tunnel HOST:PORT[@weight] HOST:PORT[@weight] ...
//! rust-dispatch-proxy -list
//! ```
//!
//! One listening port speaks both SOCKS5 and HTTP proxy (CONNECT for HTTPS,
//! plain HTTP forwarding); the protocol is detected from the first byte, so
//! apps can be pointed at it directly or through Windows' proxy setting.
//!
//! With -tun (cargo feature "tun", on by default) a virtual network adapter
//! additionally captures all IPv4 traffic on the system (every app, TCP and
//! UDP) and sends it through the same balancer.
//!
//! With -tunnel the links are upstream SOCKS5 proxies (typically `ssh -D`
//! tunnels) instead of local adapters, as in go-dispatch-proxy.
//!
//! With -auto the links are every adapter that can reach the internet, kept
//! up to date as the network changes, without restarting or dropping
//! connections. -system-proxy (Windows) turns Windows' proxy setting on while
//! there are enough links and off otherwise and on exit.
//!
//! Links race: if the chosen link has not connected within -race ms (default
//! 300), the next one is tried alongside it and the first to connect wins.
//!
//! Command-line compatible with go-dispatch-proxy. Improvements over it:
//!   * Connect timeout (default 4 s) instead of the ~21 s Windows default.
//!   * Automatic failover: if a link fails to connect, the next link is tried.
//!   * Links that keep failing are skipped for a short cooldown.
//!   * Weighted least-active-connections balancing instead of blind rotation.
//!   * Correct SOCKS5 parsing (no partial-read bugs), handshake timeout,
//!     proper half-close handling, larger copy buffers, TCP_NODELAY.
//!   * No per-connection logging unless -verbose is given.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::pin::Pin;
use std::process::exit;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

mod auto;
mod http;
mod log;
mod net;
mod status;
#[cfg(windows)]
mod sysproxy;
#[cfg(feature = "tun")]
mod tun_mode;

use net::Egress;

// ---------------------------------------------------------------- settings

pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const DNS_TIMEOUT: Duration = Duration::from_secs(10);
/// Per direction, per connection. Measured on loopback: 256 KB moves about
/// 1.3x the data of 64 KB per CPU second. Either size is far faster than
/// typical internet links; the larger one just leaves more headroom.
pub(crate) const COPY_BUFFER: usize = 256 * 1024;
const FAILS_BEFORE_COOLDOWN: u32 = 2;
const COOLDOWN_MS: u64 = 10_000;
const DEFAULT_RACE_MS: u64 = 300;
/// A link is raced after the race delay or this many times its usual
/// connect time, whichever is longer, so a link that is merely slower than
/// the others still gets to win its own connections.
const RACE_LATENCY_FACTOR: u64 = 3;

// SOCKS5 reply codes
const REP_SUCCESS: u8 = 0x00;
const REP_GENERAL_FAILURE: u8 = 0x01;
const REP_NETWORK_UNREACHABLE: u8 = 0x03;
const REP_HOST_UNREACHABLE: u8 = 0x04;
const REP_CONNECTION_REFUSED: u8 = 0x05;
const REP_TTL_EXPIRED: u8 = 0x06;
const REP_COMMAND_NOT_SUPPORTED: u8 = 0x07;
const REP_ADDRTYPE_NOT_SUPPORTED: u8 = 0x08;

// ---------------------------------------------------------------- logging

static QUIET: AtomicBool = AtomicBool::new(false);
static VERBOSE: AtomicBool = AtomicBool::new(false);

macro_rules! info {
    ($($t:tt)*) => { if !QUIET.load(Relaxed) { log::emit("INFO", format_args!($($t)*)); } };
}
macro_rules! warn {
    ($($t:tt)*) => { if !QUIET.load(Relaxed) { log::emit("WARN", format_args!($($t)*)); } };
}
macro_rules! debug {
    ($($t:tt)*) => { if VERBOSE.load(Relaxed) && !QUIET.load(Relaxed) { log::emit("DEBUG", format_args!($($t)*)); } };
}

#[cfg_attr(not(any(windows, feature = "tun")), allow(dead_code))]
pub(crate) fn log_info(msg: String) {
    info!("{msg}");
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn log_warn(msg: String) {
    warn!("{msg}");
}

pub(crate) fn log_debug(msg: String) {
    debug!("{msg}");
}

fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

// ---------------------------------------------------------------- load balancer

/// An upstream SOCKS5 proxy used as a link in -tunnel mode.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Upstream {
    host: String,
    port: u16,
}

/// How a link reaches the internet.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Via {
    /// Directly, out of a local network adapter.
    Adapter(Egress),
    /// Through an upstream SOCKS5 proxy (-tunnel).
    Upstream(Upstream),
}

impl std::fmt::Display for Via {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Via::Adapter(e) => write!(f, "{}", e.ip),
            Via::Upstream(u) if u.host.contains(':') => write!(f, "[{}]:{}", u.host, u.port),
            Via::Upstream(u) => write!(f, "{}:{}", u.host, u.port),
        }
    }
}

pub(crate) struct Link {
    pub(crate) via: Via,
    pub(crate) weight: u64,
    pub(crate) active: AtomicUsize,
    consecutive_fails: AtomicU32,
    pub(crate) down_until_ms: AtomicU64,
    /// Smoothed time to connect, in microseconds (0 = not measured yet).
    pub(crate) connect_us: AtomicU64,
    // Totals for the status page.
    pub(crate) connections: AtomicU64,
    pub(crate) failures: AtomicU64,
    pub(crate) bytes_up: AtomicU64,
    pub(crate) bytes_down: AtomicU64,
}

impl Link {
    fn new(via: Via, weight: u64) -> Self {
        Link {
            via,
            weight,
            active: AtomicUsize::new(0),
            consecutive_fails: AtomicU32::new(0),
            down_until_ms: AtomicU64::new(0),
            connect_us: AtomicU64::new(0),
            connections: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            bytes_up: AtomicU64::new(0),
            bytes_down: AtomicU64::new(0),
        }
    }

    fn report_success(&self, took: Duration) {
        self.consecutive_fails.store(0, Relaxed);
        self.down_until_ms.store(0, Relaxed);
        let sample = (took.as_micros() as u64).max(1);
        let old = self.connect_us.load(Relaxed);
        // Moving average over roughly the last 8 connections.
        self.connect_us.store(if old == 0 { sample } else { (old * 7 + sample) / 8 }, Relaxed);
    }

    fn report_failure(&self) {
        self.failures.fetch_add(1, Relaxed);
        let fails = self.consecutive_fails.fetch_add(1, Relaxed) + 1;
        if fails >= FAILS_BEFORE_COOLDOWN {
            self.down_until_ms.store(now_ms() + COOLDOWN_MS, Relaxed);
            warn!("{} failed {} times in a row, skipping it for {} s", self.via, fails, COOLDOWN_MS / 1000);
        }
    }

    /// How long to give this link before starting the next one alongside it.
    fn race_delay(&self, base: Duration) -> Duration {
        base.max(Duration::from_micros(self.connect_us.load(Relaxed) * RACE_LATENCY_FACTOR))
    }
}

/// A snapshot of the links. Connections take one when they start, so the
/// set can be replaced (-auto) without disturbing connections in flight.
pub(crate) type Links = Arc<Vec<Arc<Link>>>;

pub(crate) struct Balancer {
    links: RwLock<Links>,
    rotation: AtomicUsize,
    connect_timeout: Duration,
    /// Start the next link alongside a slow one after this long (None: only
    /// after a failure).
    race_delay: Option<Duration>,
}

/// Keeps a link's active-connection count accurate for as long as it lives.
pub(crate) struct ActiveGuard(Arc<Link>);

impl ActiveGuard {
    pub(crate) fn new(link: &Arc<Link>) -> Self {
        link.active.fetch_add(1, Relaxed);
        ActiveGuard(link.clone())
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Relaxed);
    }
}

/// An open connection through a link. Counts its traffic towards the link,
/// and keeps the link's active-connection count up for as long as it lives.
pub(crate) struct Conn {
    stream: TcpStream,
    guard: ActiveGuard,
}

impl Conn {
    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.stream.local_addr()
    }

    #[cfg(test)]
    fn link(&self) -> &Arc<Link> {
        &self.guard.0
    }
}

impl AsyncRead for Conn {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let result = Pin::new(&mut this.stream).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = result {
            this.guard.0.bytes_down.fetch_add((buf.filled().len() - before) as u64, Relaxed);
        }
        result
    }
}

impl AsyncWrite for Conn {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.stream).poll_write(cx, data);
        if let Poll::Ready(Ok(n)) = result {
            this.guard.0.bytes_up.fetch_add(n as u64, Relaxed);
        }
        result
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

impl Balancer {
    fn new(links: Vec<(Via, u64)>, connect_timeout: Duration, race_delay: Option<Duration>) -> Self {
        let links = links.into_iter().map(|(via, w)| Arc::new(Link::new(via, w))).collect();
        Balancer {
            links: RwLock::new(Arc::new(links)),
            rotation: AtomicUsize::new(0),
            connect_timeout,
            race_delay,
        }
    }

    pub(crate) fn links(&self) -> Links {
        self.links.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Replaces the set of links. Links that are still wanted keep their
    /// counters and cooldown; connections on removed links carry on until
    /// they close. Returns whether anything changed.
    fn set_links(&self, wanted: Vec<(Via, u64)>) -> bool {
        let mut slot = self.links.write().unwrap_or_else(PoisonError::into_inner);
        let new: Vec<Arc<Link>> = wanted
            .into_iter()
            .map(|(via, w)| match slot.iter().find(|l| l.via == via && l.weight == w) {
                Some(existing) => existing.clone(),
                None => Arc::new(Link::new(via, w)),
            })
            .collect();
        let changed = new.len() != slot.len() || new.iter().zip(slot.iter()).any(|(a, b)| !Arc::ptr_eq(a, b));
        if changed {
            *slot = Arc::new(new);
        }
        changed
    }

    /// Weighted least-active-connections: pick the link with the lowest
    /// (active + 1) / weight. Links in cooldown are skipped unless every
    /// untried link is in cooldown. The rotating start index spreads ties.
    pub(crate) fn pick(&self, links: &[Arc<Link>], tried: &[bool]) -> Option<usize> {
        let n = links.len();
        let now = now_ms();
        let start = self.rotation.fetch_add(1, Relaxed);

        for allow_cooling in [false, true] {
            let mut best: Option<(usize, u64, u64)> = None; // (index, load, weight)
            for k in 0..n {
                let i = (start + k) % n;
                if tried[i] {
                    continue;
                }
                let link = &links[i];
                if !allow_cooling && link.down_until_ms.load(Relaxed) > now {
                    continue;
                }
                let load = link.active.load(Relaxed) as u64 + 1;
                let better = match best {
                    None => true,
                    // load/weight < best_load/best_weight, without division
                    Some((_, bl, bw)) => load * bw < bl * link.weight,
                };
                if better {
                    best = Some((i, load, link.weight));
                }
            }
            if let Some((i, _, _)) = best {
                return Some(i);
            }
        }
        None
    }
}

// ---------------------------------------------------------------- SOCKS5

#[derive(Clone)]
pub(crate) enum Target {
    Ip(SocketAddrV4),
    Domain(String, u16),
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Ip(a) => write!(f, "{a}"),
            Target::Domain(h, p) => write!(f, "{h}:{p}"),
        }
    }
}

enum Request {
    Connect(Target),
    Rejected, // a SOCKS error reply was already sent
}

async fn send_reply(client: &mut TcpStream, code: u8, bound: Option<SocketAddrV4>) -> io::Result<()> {
    let b = bound.unwrap_or(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));
    let mut buf = [5, code, 0, 1, 0, 0, 0, 0, 0, 0];
    buf[4..8].copy_from_slice(&b.ip().octets());
    buf[8..10].copy_from_slice(&b.port().to_be_bytes());
    client.write_all(&buf).await
}

fn bad_data(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Reads the SOCKS5 greeting and CONNECT request.
async fn read_request(client: &mut TcpStream) -> io::Result<Request> {
    // Greeting: VER, NMETHODS, METHODS...
    let mut head = [0u8; 2];
    client.read_exact(&mut head).await?;
    if head[0] != 5 {
        return Err(bad_data("not a SOCKS5 client (configure the app for SOCKS v5)"));
    }
    let mut methods = vec![0u8; head[1] as usize];
    client.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        client.write_all(&[5, 0xFF]).await?; // no acceptable auth method
        return Ok(Request::Rejected);
    }
    client.write_all(&[5, 0]).await?; // no authentication

    // Request: VER, CMD, RSV, ATYP, DST.ADDR, DST.PORT
    let mut req = [0u8; 4];
    client.read_exact(&mut req).await?;
    if req[0] != 5 {
        return Err(bad_data("bad SOCKS request version"));
    }
    let cmd = req[1];

    let target = match req[3] {
        1 => {
            let mut ip = [0u8; 4];
            client.read_exact(&mut ip).await?;
            let port = client.read_u16().await?;
            Target::Ip(SocketAddrV4::new(Ipv4Addr::from(ip), port))
        }
        3 => {
            let len = client.read_u8().await? as usize;
            let mut name = vec![0u8; len];
            client.read_exact(&mut name).await?;
            let port = client.read_u16().await?;
            Target::Domain(String::from_utf8_lossy(&name).into_owned(), port)
        }
        4 => {
            // IPv6 destination: links are bound to IPv4 addresses, so refuse.
            let mut skip = [0u8; 18];
            client.read_exact(&mut skip).await?;
            send_reply(client, REP_ADDRTYPE_NOT_SUPPORTED, None).await?;
            return Ok(Request::Rejected);
        }
        _ => {
            send_reply(client, REP_ADDRTYPE_NOT_SUPPORTED, None).await?;
            return Ok(Request::Rejected);
        }
    };

    if cmd != 1 {
        // Only CONNECT (TCP) is supported; no BIND or UDP ASSOCIATE.
        send_reply(client, REP_COMMAND_NOT_SUPPORTED, None).await?;
        return Ok(Request::Rejected);
    }
    Ok(Request::Connect(target))
}

pub(crate) async fn resolve(target: &Target) -> Vec<SocketAddrV4> {
    match target {
        Target::Ip(a) => vec![*a],
        Target::Domain(host, port) => match timeout(DNS_TIMEOUT, tokio::net::lookup_host((host.as_str(), *port))).await {
            Ok(Ok(addrs)) => addrs
                .filter_map(|a| match a {
                    SocketAddr::V4(v4) => Some(v4),
                    SocketAddr::V6(_) => None,
                })
                .collect(),
            _ => Vec::new(),
        },
    }
}

/// Why one attempt to connect through a link failed.
enum Failure {
    /// The link itself is broken (cannot connect, timeout, bad upstream):
    /// try another link and count it towards the link's cooldown.
    Link(io::Error),
    /// The destination answered and refused: another link would get the same
    /// answer, so give up.
    Refused(io::Error),
    /// The link works but the upstream could not reach the destination: try
    /// another link, but do not blame this one.
    Remote(io::Error),
}

async fn connect_via(egress: &Egress, dst: SocketAddrV4, limit: Duration) -> Result<TcpStream, Failure> {
    let socket = net::tcp_socket(egress).map_err(Failure::Link)?;
    match timeout(limit, socket.connect(SocketAddr::V4(dst))).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(e)) if e.kind() == io::ErrorKind::ConnectionRefused => Err(Failure::Refused(e)),
        Ok(Err(e)) => Err(Failure::Link(e)),
        Err(_) => Err(Failure::Link(io::Error::new(io::ErrorKind::TimedOut, "connect timed out"))),
    }
}

/// Opens a connection to `target` through an upstream SOCKS5 proxy. Domain
/// names are passed on unresolved, so DNS happens at the far end.
async fn connect_upstream(up: &Upstream, target: &Target, limit: Duration) -> Result<TcpStream, Failure> {
    let work = async {
        let mut s = TcpStream::connect((up.host.as_str(), up.port)).await.map_err(Failure::Link)?;
        let _ = s.set_nodelay(true);

        // Greeting: version 5, one method, "no authentication".
        s.write_all(&[5, 1, 0]).await.map_err(Failure::Link)?;
        let mut choice = [0u8; 2];
        s.read_exact(&mut choice).await.map_err(Failure::Link)?;
        if choice != [5, 0] {
            return Err(Failure::Link(bad_data("upstream is not a SOCKS5 proxy without authentication")));
        }

        let mut req = vec![5, 1, 0];
        match target {
            Target::Ip(a) => {
                req.push(1);
                req.extend_from_slice(&a.ip().octets());
                req.extend_from_slice(&a.port().to_be_bytes());
            }
            Target::Domain(host, port) => {
                let Ok(len) = u8::try_from(host.len()) else {
                    return Err(Failure::Refused(bad_data("host name too long")));
                };
                req.push(3);
                req.push(len);
                req.extend_from_slice(host.as_bytes());
                req.extend_from_slice(&port.to_be_bytes());
            }
        }
        s.write_all(&req).await.map_err(Failure::Link)?;

        // Reply: VER, REP, RSV, ATYP, BND.ADDR, BND.PORT
        let mut head = [0u8; 4];
        s.read_exact(&mut head).await.map_err(Failure::Link)?;
        if head[0] != 5 {
            return Err(Failure::Link(bad_data("bad SOCKS reply from upstream")));
        }
        match head[1] {
            REP_SUCCESS => {}
            REP_CONNECTION_REFUSED => {
                return Err(Failure::Refused(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    "refused (reported by upstream)",
                )));
            }
            code => {
                let kind = match code {
                    REP_HOST_UNREACHABLE => io::ErrorKind::HostUnreachable,
                    REP_NETWORK_UNREACHABLE => io::ErrorKind::NetworkUnreachable,
                    REP_TTL_EXPIRED => io::ErrorKind::TimedOut,
                    _ => io::ErrorKind::Other,
                };
                return Err(Failure::Remote(io::Error::new(kind, format!("upstream replied with error {code:#04x}"))));
            }
        }
        let rest = match head[3] {
            1 => 4 + 2,
            4 => 16 + 2,
            3 => s.read_u8().await.map_err(Failure::Link)? as usize + 2,
            _ => return Err(Failure::Link(bad_data("bad address type in upstream reply"))),
        };
        let mut skip = vec![0u8; rest];
        s.read_exact(&mut skip).await.map_err(Failure::Link)?;
        Ok::<TcpStream, Failure>(s)
    };
    match timeout(limit, work).await {
        Ok(result) => result,
        Err(_) => Err(Failure::Link(io::Error::new(io::ErrorKind::TimedOut, "upstream timed out"))),
    }
}

fn reply_code_for(err: &io::Error) -> u8 {
    match err.kind() {
        io::ErrorKind::ConnectionRefused => REP_CONNECTION_REFUSED,
        io::ErrorKind::TimedOut => REP_TTL_EXPIRED,
        io::ErrorKind::HostUnreachable | io::ErrorKind::NotFound => REP_HOST_UNREACHABLE,
        io::ErrorKind::NetworkUnreachable | io::ErrorKind::AddrNotAvailable => REP_NETWORK_UNREACHABLE,
        _ => REP_GENERAL_FAILURE,
    }
}

/// Looks at the first byte to tell SOCKS5 (0x05) from HTTP, then hands off.
async fn route_client(client: TcpStream, balancer: Arc<Balancer>) {
    let mut first = [0u8; 1];
    match timeout(HANDSHAKE_TIMEOUT, client.peek(&mut first)).await {
        Ok(Ok(1)) if first[0] == 5 => handle_socks(client, balancer).await,
        Ok(Ok(1)) => http::handle_http(client, balancer).await,
        _ => {}
    }
}

/// One connection attempt in flight. Its guard holds the link; it yields how
/// long the attempt took and its result.
type Attempt = Pin<Box<dyn Future<Output = (ActiveGuard, Duration, Result<TcpStream, Failure>)> + Send>>;

/// Connects to `target` through the balancer. Used by the SOCKS5 and HTTP
/// handlers and by TUN mode.
///
/// Links race: the best link is tried first; if it has not connected within
/// its race delay (the -race delay, or 3x its usual connect time if that is
/// longer), or fails, the next link is started alongside it, and the first
/// to connect wins. Attempts that lost keep running in the background and
/// report how they really ended, so a link that is merely slower than the
/// winner is not punished, while one that hangs still times out and ends up
/// in cooldown.
pub(crate) async fn dial(balancer: &Balancer, target: &Target) -> io::Result<Conn> {
    let links = balancer.links();
    if links.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NetworkUnreachable, "no internet connections available"));
    }

    // Adapter links connect to IP addresses; upstream links take the name as it is.
    let dests = if links.iter().any(|l| matches!(l.via, Via::Adapter(_))) {
        let dests = resolve(target).await;
        if dests.is_empty() {
            warn!("could not resolve {target} to an IPv4 address");
            return Err(io::Error::new(io::ErrorKind::NotFound, "could not resolve host"));
        }
        dests
    } else {
        Vec::new()
    };

    let mut tried = vec![false; links.len()];
    let mut racing: Vec<Attempt> = Vec::new();
    let mut attempts = 0;
    let mut race_at = tokio::time::Instant::now();
    let mut last_err: Option<io::Error> = None;

    // Each pass starts the next link (if any is left), then waits for an
    // attempt to finish or for the race delay.
    loop {
        if let Some(i) = balancer.pick(&links, &tried) {
            tried[i] = true;
            let link = &links[i];
            let guard = ActiveGuard::new(link);
            let dst = (!dests.is_empty()).then(|| dests[attempts % dests.len()]);
            let limit = balancer.connect_timeout;
            let target = target.clone();
            racing.push(Box::pin(async move {
                let started = Instant::now();
                let result = match (&guard.0.via, dst) {
                    (Via::Adapter(egress), Some(dst)) => connect_via(egress, dst, limit).await,
                    (Via::Adapter(_), None) => Err(Failure::Link(io::Error::other("no address to connect to"))),
                    (Via::Upstream(up), _) => connect_upstream(up, &target, limit).await,
                };
                (guard, started.elapsed(), result)
            }));
            attempts += 1;
            if let Some(base) = balancer.race_delay {
                race_at = tokio::time::Instant::now() + link.race_delay(base);
            }
        }
        if racing.is_empty() {
            break;
        }

        // Wait for an attempt to finish, or for the race delay if another
        // link is still available to start.
        let first_done = std::future::poll_fn(|cx| {
            for (pos, attempt) in racing.iter_mut().enumerate() {
                if let Poll::Ready(out) = attempt.as_mut().poll(cx) {
                    return Poll::Ready((pos, out));
                }
            }
            Poll::Pending
        });
        let more_links = tried.iter().any(|t| !t);
        let (pos, (guard, took, result)) = if balancer.race_delay.is_some() && more_links {
            match tokio::time::timeout_at(race_at, first_done).await {
                Ok(done) => done,
                Err(_) => continue, // slow: start the next link alongside
            }
        } else {
            first_done.await
        };
        drop(racing.swap_remove(pos));
        let link = &guard.0;

        match result {
            Ok(stream) => {
                link.report_success(took);
                link.connections.fetch_add(1, Relaxed);
                for other in racing.drain(..) {
                    tokio::spawn(settle(other));
                }
                let _ = stream.set_nodelay(true);
                debug!("{target} -> via {} ({} ms)", link.via, took.as_millis());
                return Ok(Conn { stream, guard });
            }
            Err(Failure::Refused(e)) => {
                debug!("{target} refused the connection");
                return Err(e); // every link would get the same answer
            }
            Err(Failure::Remote(e)) => {
                debug!("{target} via {} failed: {e}", link.via);
                last_err = Some(e);
            }
            Err(Failure::Link(e)) => {
                warn!("{target} via {} failed: {e}", link.via);
                link.report_failure();
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| io::Error::other("no links available")))
}

/// Lets an attempt that lost a race finish, and judges its link by how it
/// really ended. A connection that does get made is closed straight away.
async fn settle(attempt: Attempt) {
    let (guard, took, result) = attempt.await;
    match result {
        Ok(_) => guard.0.report_success(took),
        Err(Failure::Link(e)) => {
            debug!("{} failed after losing a race: {e}", guard.0.via);
            guard.0.report_failure();
        }
        Err(_) => {}
    }
}

async fn handle_socks(mut client: TcpStream, balancer: Arc<Balancer>) {
    let _ = client.set_nodelay(true);

    let target = match timeout(HANDSHAKE_TIMEOUT, read_request(&mut client)).await {
        Ok(Ok(Request::Connect(t))) => t,
        Ok(Ok(Request::Rejected)) => return,
        Ok(Err(e)) => {
            debug!("handshake failed: {e}");
            return;
        }
        Err(_) => {
            debug!("handshake timed out");
            return;
        }
    };

    let mut remote = match dial(&balancer, &target).await {
        Ok(ok) => ok,
        Err(e) => {
            let _ = send_reply(&mut client, reply_code_for(&e), None).await;
            return;
        }
    };

    let bound = match remote.local_addr() {
        Ok(SocketAddr::V4(a)) => Some(a),
        _ => None,
    };
    if send_reply(&mut client, REP_SUCCESS, bound).await.is_err() {
        return;
    }

    // Relay both directions; handles half-close correctly.
    let _ = tokio::io::copy_bidirectional_with_sizes(&mut client, &mut remote, COPY_BUFFER, COPY_BUFFER).await;
    // `remote` drops here, which decrements the link's active count.
}

// ---------------------------------------------------------------- CLI

struct Config {
    lhost: Ipv4Addr,
    lport: u16,
    connect_timeout: Duration,
    race_delay: Option<Duration>,
    tun: bool,
    auto: bool,
    system_proxy: SystemProxy,
    links: Vec<(Via, u64)>,
}

/// -system-proxy: whether to manage Windows' proxy setting, and how many
/// links are needed before it is turned on.
#[derive(Clone, Copy)]
#[cfg_attr(not(windows), allow(dead_code))]
struct SystemProxy {
    enabled: bool,
    min_links: usize,
}

impl SystemProxy {
    /// Turns the setting on when there are enough links, off otherwise.
    fn sync(self, links: usize) {
        #[cfg(windows)]
        if self.enabled {
            sysproxy::set(links >= self.min_links);
        }
        #[cfg(not(windows))]
        let _ = links;
    }
}

/// The TUN adapter has an IPv4 address too, but must never be used as a link.
fn is_own_adapter(name: &str) -> bool {
    #[cfg(feature = "tun")]
    {
        name == tun_mode::TUN_NAME
    }
    #[cfg(not(feature = "tun"))]
    {
        let _ = name;
        false
    }
}

pub(crate) fn local_ipv4s() -> Vec<Egress> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback() && !is_own_adapter(&i.name))
        .filter_map(|i| match i.ip() {
            IpAddr::V4(ip) => Some(Egress {
                ip,
                if_index: i.index,
                if_name: i.name,
            }),
            IpAddr::V6(_) => None,
        })
        .collect()
}

#[cfg(feature = "tun")]
const TUN_HELP: &str = "\x20 -tun            also capture ALL system traffic through a virtual adapter\n\
                        \x20                 (Windows: run as administrator, wintun.dll next to the exe)\n";
#[cfg(not(feature = "tun"))]
const TUN_HELP: &str = "\x20 -tun            (not available: this build was compiled without the \"tun\" feature)\n";

fn usage() -> ! {
    eprintln!(
        "rust-dispatch-proxy {}\n\
         Combine several internet connections by spreading connections across them.\n\
         \n\
         Usage: rust-dispatch-proxy [options] IP[@weight] [IP[@weight] ...]\n\
         \x20      rust-dispatch-proxy [options] -auto\n\
         \x20      rust-dispatch-proxy [options] -tunnel HOST:PORT[@weight] [HOST:PORT[@weight] ...]\n\
         \n\
         Options:\n\
         \x20 -lhost ADDR     address to listen on (default 127.0.0.1)\n\
         \x20 -lport PORT     port to listen on, SOCKS5 and HTTP (default 8080)\n\
         \x20 -auto           use every adapter that can reach the internet, and follow\n\
         \x20                 network changes while running (no addresses needed)\n\
         \x20 -system-proxy   Windows: turn the system proxy setting on while there are\n\
         \x20                 enough links, and off when there aren't or on exit\n\
         \x20 -min-links N    links needed before -system-proxy turns it on (default 2)\n\
         \x20 -system-proxy-off  Windows: switch the system proxy setting off if it points\n\
         \x20                 at this proxy (-lhost/-lport), then exit\n\
         \x20 -timeout SECS   connect timeout per link (default 4)\n\
         \x20 -race MS        start the next link if one hasn't connected within MS\n\
         \x20                 milliseconds; first to connect wins (default 300, 0 = off)\n\
         {TUN_HELP}\
         \x20 -tunnel         links are upstream SOCKS5 proxies (e.g. `ssh -D` tunnels)\n\
         \x20                 instead of local addresses\n\
         \x20 -list           list usable local IPv4 addresses and exit\n\
         \x20 -quiet          no log output\n\
         \x20 -verbose        log every connection\n\
         \x20 -log FILE       write the log to FILE instead of the console\n\
         \x20 -version        print the version and exit\n\
         \x20 -help           show this help\n\
         \n\
         Status page: open http://127.0.0.1:8080/status (your -lhost/-lport) in a browser.\n\
         \n\
         Examples:\n\
         \x20 rust-dispatch-proxy 192.168.1.10@3 10.0.0.5@1\n\
         \x20 rust-dispatch-proxy -auto -system-proxy\n\
         \x20 rust-dispatch-proxy -tunnel 127.0.0.1:7777 127.0.0.1:7778",
        env!("CARGO_PKG_VERSION")
    );
    exit(2);
}

fn fatal(msg: String) -> ! {
    log::emit("FATAL", format_args!("{msg}"));
    exit(1);
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Config {
    let mut lhost = Ipv4Addr::LOCALHOST;
    let mut lport: u16 = 8080;
    let mut connect_timeout = Duration::from_secs(4);
    let mut race_ms = DEFAULT_RACE_MS;
    let mut tun = false;
    let mut tunnel = false;
    let mut auto = false;
    let mut system_proxy = SystemProxy {
        enabled: false,
        min_links: 2,
    };
    let mut log_path: Option<String> = None;
    let mut system_proxy_off = false;
    let mut specs = Vec::new();

    while let Some(arg) = args.next() {
        if let Some(flag) = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-')) {
            let (name, inline) = match flag.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (flag.to_string(), None),
            };
            let mut value = |n: &str| {
                inline
                    .clone()
                    .or_else(|| args.next())
                    .unwrap_or_else(|| fatal(format!("-{n} needs a value")))
            };
            match name.as_str() {
                "lhost" => {
                    let v = value("lhost");
                    lhost = v.parse().unwrap_or_else(|_| fatal(format!("Invalid host {v}")));
                }
                "lport" => {
                    let v = value("lport");
                    lport = v.parse().ok().filter(|p| *p > 0).unwrap_or_else(|| fatal(format!("Invalid port {v}")));
                }
                "timeout" => {
                    let v = value("timeout");
                    let secs: f64 = v
                        .parse()
                        .ok()
                        .filter(|s: &f64| *s > 0.0)
                        .unwrap_or_else(|| fatal(format!("Invalid timeout {v}")));
                    connect_timeout = Duration::from_secs_f64(secs);
                }
                "race" => {
                    let v = value("race");
                    race_ms = v
                        .parse()
                        .unwrap_or_else(|_| fatal(format!("Invalid race delay {v} (milliseconds, 0 = off)")));
                }
                "auto" => {
                    if !cfg!(any(windows, target_os = "linux")) {
                        fatal("-auto is only supported on Windows and Linux".into());
                    }
                    auto = true;
                }
                "system-proxy" => {
                    if !cfg!(windows) {
                        fatal("-system-proxy is only available on Windows".into());
                    }
                    system_proxy.enabled = true;
                }
                "system-proxy-off" => {
                    if !cfg!(windows) {
                        fatal("-system-proxy-off is only available on Windows".into());
                    }
                    system_proxy_off = true;
                }
                "log" => log_path = Some(value("log")),
                "min-links" => {
                    let v = value("min-links");
                    system_proxy.min_links = v.parse().unwrap_or_else(|_| fatal(format!("Invalid number {v}")));
                }
                "tun" => {
                    if !cfg!(feature = "tun") {
                        fatal("This build was compiled without TUN support (cargo feature \"tun\")".into());
                    }
                    if !cfg!(any(windows, target_os = "linux")) {
                        fatal("TUN mode is only supported on Windows and Linux".into());
                    }
                    tun = true;
                }
                "quiet" => QUIET.store(true, Relaxed),
                "verbose" | "v" => VERBOSE.store(true, Relaxed),
                "list" => {
                    println!("--- Listing the available addresses for dispatching");
                    for e in local_ipv4s() {
                        println!("[+] {}, IPv4:{}", e.if_name, e.ip);
                    }
                    exit(0);
                }
                "version" | "V" => {
                    println!("rust-dispatch-proxy {}", env!("CARGO_PKG_VERSION"));
                    exit(0);
                }
                "tunnel" => tunnel = true,
                "h" | "help" => usage(),
                _ => fatal(format!("Unknown option {arg}")),
            }
        } else {
            specs.push(arg);
        }
    }

    if let Some(path) = &log_path {
        log::to_file(path).unwrap_or_else(|e| fatal(format!("Could not open log file {path}: {e}")));
    }
    if tun && tunnel {
        fatal("-tun cannot be combined with -tunnel (TUN mode also forwards UDP, which SOCKS tunnels cannot carry)".into());
    }
    if auto && tunnel {
        fatal("-auto finds local adapters; it cannot be combined with -tunnel".into());
    }
    if auto && !specs.is_empty() {
        fatal("-auto finds the connections itself; leave out the addresses".into());
    }
    if system_proxy_off {
        // Switch Windows' proxy setting off if it points at this proxy, and exit.
        #[cfg(windows)]
        {
            let host = if lhost.is_unspecified() { Ipv4Addr::LOCALHOST } else { lhost };
            let changed = sysproxy::turn_off_if_ours(&format!("{host}:{lport}"));
            println!(
                "{}",
                if changed {
                    "Windows proxy setting OFF"
                } else {
                    "Windows proxy setting left as it was"
                }
            );
        }
        exit(0);
    }
    if !auto && specs.is_empty() {
        usage();
    }

    let links = if auto {
        Vec::new() // detected at startup
    } else if tunnel {
        upstream_links(&specs)
    } else {
        adapter_links(&specs)
    };
    Config {
        lhost,
        lport,
        connect_timeout,
        race_delay: (race_ms > 0).then(|| Duration::from_millis(race_ms)),
        tun,
        auto,
        system_proxy,
        links,
    }
}

/// -auto: one link per adapter that can reach the internet.
fn detected_links() -> Vec<(Via, u64)> {
    auto::detect().into_iter().map(|e| (Via::Adapter(e), 1)).collect()
}

fn log_links(links: &[Arc<Link>]) {
    if links.is_empty() {
        info!("No internet connections found yet; waiting for one");
    }
    for (i, link) in links.iter().enumerate() {
        match &link.via {
            Via::Adapter(e) => info!("Link {}: {} ({}), weight {}", i + 1, e.ip, e.if_name, link.weight),
            Via::Upstream(_) => info!("Link {}: SOCKS5 upstream {}, weight {}", i + 1, link.via, link.weight),
        }
    }
}

/// Splits "address@weight" (weight defaults to 1).
fn split_weight(spec: &str) -> Result<(&str, u64), String> {
    let (addr, weight_s) = spec.split_once('@').unwrap_or((spec, "1"));
    let weight = weight_s
        .parse()
        .ok()
        .filter(|w| *w > 0)
        .ok_or_else(|| format!("Invalid weight for {addr}"))?;
    Ok((addr, weight))
}

fn adapter_links(specs: &[String]) -> Vec<(Via, u64)> {
    let local = local_ipv4s();
    let mut links: Vec<(Via, u64)> = Vec::new();
    let mut seen = Vec::new();
    for spec in specs {
        let (ip_s, weight) = match split_weight(spec) {
            Ok(v) => v,
            Err(e) => fatal(e),
        };
        let ip: Ipv4Addr = ip_s
            .parse()
            .unwrap_or_else(|_| fatal(format!("Invalid address {ip_s} (use -tunnel for upstream proxies)")));
        let Some(egress) = local.iter().find(|e| e.ip == ip) else {
            warn!("{ip} is not assigned to any network adapter, skipping it");
            continue;
        };
        if seen.contains(&ip) {
            continue;
        }
        seen.push(ip);
        links.push((Via::Adapter(egress.clone()), weight));
    }
    if links.is_empty() {
        fatal("None of the given addresses belong to this machine (see -list)".into());
    }
    links
}

/// Parses "host:port[@weight]" for -tunnel mode.
fn parse_upstream(spec: &str) -> Result<(Upstream, u64), String> {
    let (addr, weight) = split_weight(spec)?;
    match http::split_host_port(addr, 0) {
        Some((host, port)) if port > 0 => Ok((Upstream { host, port }, weight)),
        _ => Err(format!("Invalid tunnel address {addr} (expected HOST:PORT)")),
    }
}

fn upstream_links(specs: &[String]) -> Vec<(Via, u64)> {
    let mut links: Vec<(Via, u64)> = Vec::new();
    let mut seen = Vec::new();
    for spec in specs {
        let (up, weight) = match parse_upstream(spec) {
            Ok(v) => v,
            Err(e) => fatal(e),
        };
        if seen.contains(&up) {
            continue;
        }
        seen.push(up.clone());
        links.push((Via::Upstream(up), weight));
    }
    links
}

// ---------------------------------------------------------------- entry point

/// Runs the proxy with a command line (without the program name), as
/// rust-dispatch-proxy.exe does. Used by its own `main` and by proxy-gui.exe,
/// which runs the proxy as `proxy-gui.exe --proxy ARGS...`. Returns only by
/// exiting the process.
pub fn run(args: impl Iterator<Item = String>) {
    now_ms(); // start the uptime clock
    let cfg = parse_args(args);

    // One worker thread per CPU core (tokio's default): measured fastest.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap_or_else(|e| fatal(format!("Could not start runtime: {e}")));

    runtime.block_on(async move {
        let links = if cfg.auto {
            auto::start_watching(); // before detecting, so no change is missed
            detected_links()
        } else {
            cfg.links
        };
        let balancer = Arc::new(Balancer::new(links, cfg.connect_timeout, cfg.race_delay));
        log_links(&balancer.links());

        let bind = SocketAddrV4::new(cfg.lhost, cfg.lport);
        let listener = TcpListener::bind(bind)
            .await
            .unwrap_or_else(|e| fatal(format!("Could not start local server on {bind}: {e}")));
        info!("Proxy started at {bind} (SOCKS5 and HTTP)");
        if !cfg.lhost.is_loopback() {
            warn!(
                "Listening on {} without authentication: anyone who can reach this address can use the proxy",
                cfg.lhost
            );
        }

        let system_proxy = cfg.system_proxy;
        #[cfg(windows)]
        if system_proxy.enabled {
            // Apps reach the proxy on loopback even when it listens on all addresses.
            let host = if cfg.lhost.is_unspecified() { Ipv4Addr::LOCALHOST } else { cfg.lhost };
            sysproxy::init(format!("{host}:{}", cfg.lport));
            let count = balancer.links().len();
            if !cfg.auto && count < system_proxy.min_links {
                warn!("-system-proxy stays off: {count} link(s) given, -min-links is {}", system_proxy.min_links);
            }
        }
        system_proxy.sync(balancer.links().len());

        if cfg.auto {
            let balancer = balancer.clone();
            tokio::spawn(async move {
                loop {
                    auto::changed().await;
                    if balancer.set_links(detected_links()) {
                        let links = balancer.links();
                        info!("Network changed: {} connection(s) in use", links.len());
                        log_links(&links);
                        system_proxy.sync(links.len());
                    }
                }
            });
        }

        #[cfg(feature = "tun")]
        {
            if cfg.tun {
                let balancer = balancer.clone();
                tokio::spawn(async move {
                    if let Err(e) = tun_mode::run(balancer).await {
                        // Keep the proxy running even if TUN mode fails.
                        log::emit("ERROR", format_args!("TUN mode stopped: {e}. The SOCKS5/HTTP proxy is still running."));
                    }
                });
            }
        }
        #[cfg(not(feature = "tun"))]
        let _ = cfg.tun;

        loop {
            match listener.accept().await {
                Ok((client, _)) => {
                    tokio::spawn(route_client(client, balancer.clone()));
                }
                Err(e) => {
                    warn!("Could not accept connection: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(n: u8) -> Via {
        Via::Adapter(Egress {
            ip: Ipv4Addr::new(10, 0, 0, n),
            if_index: None,
            if_name: String::new(),
        })
    }

    fn balancer(weights: &[u64]) -> Balancer {
        let links = weights.iter().enumerate().map(|(i, &w)| (adapter(i as u8 + 1), w)).collect();
        Balancer::new(links, Duration::from_secs(4), None)
    }

    #[test]
    fn spreads_by_weight() {
        // 40 long-lived connections over links weighted 3:1 -> 30 / 10
        let b = balancer(&[3, 1]);
        let links = b.links();
        let mut guards = Vec::new();
        for _ in 0..40 {
            let i = b.pick(&links, &[false, false]).unwrap();
            guards.push(ActiveGuard::new(&links[i]));
        }
        assert_eq!(links[0].active.load(Relaxed), 30);
        assert_eq!(links[1].active.load(Relaxed), 10);
        drop(guards);
        assert_eq!(links[0].active.load(Relaxed), 0);
    }

    #[test]
    fn equal_weights_alternate() {
        let b = balancer(&[1, 1, 1]);
        let links = b.links();
        let mut guards = Vec::new();
        for _ in 0..30 {
            let i = b.pick(&links, &[false; 3]).unwrap();
            guards.push(ActiveGuard::new(&links[i]));
        }
        for l in links.iter() {
            assert_eq!(l.active.load(Relaxed), 10);
        }
    }

    #[test]
    fn failover_skips_tried_links() {
        let b = balancer(&[1, 1]);
        let links = b.links();
        let first = b.pick(&links, &[false, false]).unwrap();
        let mut tried = [false, false];
        tried[first] = true;
        let second = b.pick(&links, &tried).unwrap();
        assert_ne!(first, second);
        assert_eq!(b.pick(&links, &[true, true]), None);
    }

    #[test]
    fn failing_link_goes_into_cooldown_and_recovers() {
        let b = balancer(&[1, 1]);
        let links = b.links();
        links[0].report_failure();
        links[0].report_failure(); // second consecutive failure -> cooldown
        for _ in 0..20 {
            assert_eq!(b.pick(&links, &[false, false]), Some(1));
        }
        // If the only other link was already tried, the cooling link is still used
        assert_eq!(b.pick(&links, &[false, true]), Some(0));
        // A success clears the cooldown
        links[0].report_success(Duration::ZERO);
        let picks: Vec<_> = (0..10).map(|_| b.pick(&links, &[false, false]).unwrap()).collect();
        assert!(picks.contains(&0));
    }

    #[test]
    fn no_links_picks_nothing() {
        let b = balancer(&[]);
        assert_eq!(b.pick(&b.links(), &[]), None);
    }

    #[test]
    fn set_links_keeps_state_of_remaining_links() {
        let b = balancer(&[1, 1]);
        let before = b.links();
        let guard = ActiveGuard::new(&before[1]); // a connection open on link 2

        // Same set again: nothing changes.
        assert!(!b.set_links(vec![(adapter(1), 1), (adapter(2), 1)]));

        // Link 1 goes away, link 3 appears.
        assert!(b.set_links(vec![(adapter(2), 1), (adapter(3), 1)]));
        let after = b.links();
        assert_eq!(after.len(), 2);
        assert!(Arc::ptr_eq(&after[0], &before[1])); // same link object, same counters
        assert_eq!(after[0].active.load(Relaxed), 1);
        assert_eq!(after[1].via, adapter(3));

        // The old snapshot is untouched, and the open connection still counts.
        assert_eq!(before.len(), 2);
        drop(guard);
        assert_eq!(after[0].active.load(Relaxed), 0);

        // Everything gone (e.g. all cables unplugged).
        assert!(b.set_links(Vec::new()));
        assert!(b.links().is_empty());
    }

    #[test]
    fn upstream_specs() {
        let up = |h: &str, p| Upstream { host: h.into(), port: p };
        assert_eq!(parse_upstream("127.0.0.1:7777"), Ok((up("127.0.0.1", 7777), 1)));
        assert_eq!(parse_upstream("127.0.0.1:7778@3"), Ok((up("127.0.0.1", 7778), 3)));
        assert_eq!(parse_upstream("proxy.example.com:1080"), Ok((up("proxy.example.com", 1080), 1)));
        assert_eq!(parse_upstream("[::1]:1080@2"), Ok((up("::1", 1080), 2)));
        assert!(parse_upstream("127.0.0.1").is_err()); // port is required
        assert!(parse_upstream("127.0.0.1:0").is_err());
        assert!(parse_upstream("127.0.0.1:7777@0").is_err());
        assert!(parse_upstream("127.0.0.1:7777@x").is_err());
    }

    /// Runs a fake upstream SOCKS5 server that expects one CONNECT request,
    /// checks it, and answers with `reply`.
    async fn fake_upstream(expected_request: Vec<u8>, reply: u8) -> Upstream {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 3];
            s.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            s.write_all(&[5, 0]).await.unwrap();
            let mut req = vec![0u8; expected_request.len()];
            s.read_exact(&mut req).await.unwrap();
            assert_eq!(req, expected_request);
            s.write_all(&[5, reply, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
            if reply == REP_SUCCESS {
                s.write_all(b"hello").await.unwrap();
            }
        });
        Upstream {
            host: "127.0.0.1".into(),
            port,
        }
    }

    #[tokio::test]
    async fn upstream_passes_domain_unresolved() {
        let mut expected = vec![5, 1, 0, 3, 11];
        expected.extend_from_slice(b"example.com");
        expected.extend_from_slice(&443u16.to_be_bytes());
        let up = fake_upstream(expected, REP_SUCCESS).await;

        let target = Target::Domain("example.com".into(), 443);
        let Ok(mut s) = connect_upstream(&up, &target, Duration::from_secs(5)).await else {
            panic!("connect failed")
        };
        let mut data = [0u8; 5];
        s.read_exact(&mut data).await.unwrap();
        assert_eq!(&data, b"hello");
    }

    #[tokio::test]
    async fn upstream_errors_are_classified() {
        let ip_request = |port: u16| {
            let mut r = vec![5, 1, 0, 1, 10, 1, 2, 3];
            r.extend_from_slice(&port.to_be_bytes());
            r
        };
        let target = Target::Ip(SocketAddrV4::new(Ipv4Addr::new(10, 1, 2, 3), 80));

        let up = fake_upstream(ip_request(80), REP_CONNECTION_REFUSED).await;
        assert!(matches!(
            connect_upstream(&up, &target, Duration::from_secs(5)).await,
            Err(Failure::Refused(_))
        ));

        let up = fake_upstream(ip_request(80), REP_HOST_UNREACHABLE).await;
        assert!(matches!(
            connect_upstream(&up, &target, Duration::from_secs(5)).await,
            Err(Failure::Remote(_))
        ));

        // Nothing listening: the link itself is broken.
        let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = dead.local_addr().unwrap().port();
        drop(dead);
        let up = Upstream {
            host: "127.0.0.1".into(),
            port,
        };
        assert!(matches!(
            connect_upstream(&up, &target, Duration::from_secs(5)).await,
            Err(Failure::Link(_))
        ));
    }

    // ------------------------------------------------------------ racing

    /// An upstream that accepts connections and then never answers, like a
    /// link whose internet has silently stopped working.
    async fn hung_upstream() -> Upstream {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = listener.accept().await {
                held.push(s);
            }
        });
        Upstream {
            host: "127.0.0.1".into(),
            port,
        }
    }

    /// An upstream that accepts any number of IPv4 CONNECT requests, each
    /// after `delay` (a working link that is just slower).
    async fn working_upstream_after(delay: Duration) -> Upstream {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    let mut greeting = [0u8; 3];
                    s.read_exact(&mut greeting).await.unwrap();
                    s.write_all(&[5, 0]).await.unwrap();
                    let mut req = [0u8; 10];
                    s.read_exact(&mut req).await.unwrap();
                    s.write_all(&[5, REP_SUCCESS, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
                    let _ = s.read(&mut [0u8; 64]).await; // hold until the client closes
                });
            }
        });
        Upstream {
            host: "127.0.0.1".into(),
            port,
        }
    }

    async fn working_upstream() -> Upstream {
        working_upstream_after(Duration::ZERO).await
    }

    fn upstream_balancer(ups: Vec<Upstream>, connect_timeout: Duration, race: Option<Duration>) -> Balancer {
        Balancer::new(ups.into_iter().map(|u| (Via::Upstream(u), 1)).collect(), connect_timeout, race)
    }

    fn some_target() -> Target {
        Target::Ip(SocketAddrV4::new(Ipv4Addr::new(10, 1, 2, 3), 80))
    }

    #[tokio::test]
    async fn hung_link_is_raced_and_then_judged_by_its_timeout() {
        // The hung link is picked first; the race starts the working one
        // after 50 ms instead of waiting out the connect timeout.
        let b = upstream_balancer(
            vec![hung_upstream().await, working_upstream().await],
            Duration::from_millis(400),
            Some(Duration::from_millis(50)),
        );
        let links = b.links();
        let started = Instant::now();
        let conn = dial(&b, &some_target()).await.expect("dial failed");
        assert!(started.elapsed() < Duration::from_millis(300), "took {:?}", started.elapsed());
        assert!(Arc::ptr_eq(conn.link(), &links[1]));

        // The losing attempt is still running, so not judged yet...
        assert_eq!(links[0].consecutive_fails.load(Relaxed), 0);
        assert_eq!(links[0].active.load(Relaxed), 1);
        // ...until its own connect timeout runs out.
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(links[0].consecutive_fails.load(Relaxed), 1);
        assert_eq!(links[0].failures.load(Relaxed), 1);
        assert_eq!(links[0].active.load(Relaxed), 0);
    }

    #[tokio::test]
    async fn slow_link_that_loses_a_race_is_not_punished() {
        let b = upstream_balancer(
            vec![working_upstream_after(Duration::from_millis(150)).await, working_upstream().await],
            Duration::from_secs(2),
            Some(Duration::from_millis(50)),
        );
        let links = b.links();
        let conn = dial(&b, &some_target()).await.expect("dial failed");
        assert!(Arc::ptr_eq(conn.link(), &links[1]));

        tokio::time::sleep(Duration::from_millis(400)).await;
        // It did connect in the end: a success, with its connect time learned.
        assert_eq!(links[0].consecutive_fails.load(Relaxed), 0);
        assert_eq!(links[0].failures.load(Relaxed), 0);
        assert!(links[0].connect_us.load(Relaxed) >= 150_000);
        assert_eq!(links[0].active.load(Relaxed), 0);
    }

    #[test]
    fn race_delay_follows_each_links_connect_time() {
        let link = Link::new(adapter(1), 1);
        let base = Duration::from_millis(300);
        assert_eq!(link.race_delay(base), base); // nothing measured yet
        link.report_success(Duration::from_millis(40));
        assert_eq!(link.race_delay(base), base); // fast link: the base delay
        for _ in 0..30 {
            link.report_success(Duration::from_millis(250));
        }
        // A link that usually takes ~250 ms gets ~750 ms before it is raced.
        let d = link.race_delay(base);
        assert!(d > Duration::from_millis(700) && d <= Duration::from_millis(750), "{d:?}");
    }

    #[tokio::test]
    async fn traffic_and_connections_are_counted() {
        let b = upstream_balancer(vec![working_upstream().await], Duration::from_secs(2), None);
        let link = b.links()[0].clone();
        let mut conn = dial(&b, &some_target()).await.expect("dial failed");
        conn.write_all(b"hello").await.unwrap();
        assert_eq!(link.bytes_up.load(Relaxed), 5);
        assert_eq!(link.connections.load(Relaxed), 1);
        assert_eq!(link.active.load(Relaxed), 1);
        drop(conn);
        assert_eq!(link.active.load(Relaxed), 0);
    }

    #[tokio::test]
    async fn without_racing_the_slow_link_times_out_first() {
        let b = upstream_balancer(vec![hung_upstream().await, working_upstream().await], Duration::from_millis(300), None);
        let started = Instant::now();
        let conn = dial(&b, &some_target()).await.expect("dial failed");
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(Arc::ptr_eq(conn.link(), &b.links()[1]));
    }

    #[tokio::test]
    async fn no_links_fails_cleanly() {
        let b = upstream_balancer(Vec::new(), Duration::from_secs(1), Some(Duration::from_millis(100)));
        let err = dial(&b, &some_target()).await.err().expect("should fail");
        assert_eq!(err.kind(), io::ErrorKind::NetworkUnreachable);
    }
}
