# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [1.2.0] - 2026-10-14

### Added
- `proxy-gui.exe`, a Windows control panel: start, stop and restart the
  proxy (it runs hidden), see each connection's live speed, change the
  settings, start it at login, and open the status page or log. Closing the
  window leaves the proxy running. `--background` and `--stop` do the same
  without a window.
- `-system-proxy-off`: switch the Windows proxy setting off if it points at
  this proxy, then exit. Useful after the proxy was stopped from outside.

### Changed
- Builds use Rust 1.99.0, pinned in `rust-toolchain.toml`.

### Fixed
- The minimum Rust version is 1.88, not 1.85 as stated before: a dependency
  of TUN mode needs 1.88.

## [1.1.0] - 2026-10-12

### Added
- `-auto`: use every adapter that can reach the internet, and add or drop
  links as the network changes, without restarting or cutting open
  connections. Event-driven on Windows (no polling); Linux checks every 5 s.
- `-system-proxy` (Windows): turn the system proxy setting on while at least
  `-min-links` links (default 2) are up, off otherwise, and off on Ctrl+C or
  when the console is closed. Replaces a separate watcher script.
- Link racing (`-race MS`, default 300): if a link hasn't connected within
  the delay (or 3x its usual connect time, if longer), the next link starts
  alongside it and the first to connect wins. Losing attempts finish in the
  background and report their real outcome, so a slower link is never
  punished for losing, while a hanging one still times out into cooldown.
- Status page at `http://127.0.0.1:8080/status` (and `/status.json`): live
  per-link speed, open connections, connect time, failures and totals.
- `-log FILE`, and timestamps on every log line.

### Performance
- Copy buffers raised from 64 KB to 256 KB per direction, and one worker
  thread per CPU core instead of two. This raises the proxy's own ceiling,
  measured on loopback across 8 streams: 15 Gbit/s before, 19 Gbit/s with
  the larger buffers alone, 32 Gbit/s with both changes. It does not make
  downloads over typical home connections faster, because the proxy was
  already far faster than the links. The cost is about 512 KB of memory
  per open connection.

### Changed
- TUN mode: UDP flows such as QUIC (HTTP/3) now count towards a link's load,
  so new connections go to less busy links, and they show on the status page.
- Moved to the Rust 2024 edition. Building from source now needs Rust 1.85
  or newer.

## [1.0.0] - 2026-10-12

First public release.

### Added
- SOCKS5 proxy (`CONNECT`, IPv4 and domain targets) that spreads connections
  across several network adapters.
- HTTP proxy on the same port (`CONNECT` tunnelling and plain-HTTP
  forwarding), so it can be used as the Windows system proxy. The protocol is
  detected automatically.
- TUN mode (`-tun`): a Wintun virtual adapter that captures all IPv4 TCP and
  UDP traffic on the system and balances it. Available as the default `tun`
  cargo feature.
- Tunnel mode (`-tunnel HOST:PORT ...`): balance across upstream SOCKS5
  proxies such as `ssh -D` tunnels, as in go-dispatch-proxy. Hostnames are
  passed to the upstream unresolved.
- Weighted least-active-connections balancing with `IP@weight`.
- Automatic failover to the next link, and a 10-second cooldown for links
  that fail repeatedly.
- Configurable connect timeout (`-timeout`, default 4 s).
- Outgoing sockets pinned to their adapter (`IP_UNICAST_IF` on Windows,
  `SO_BINDTODEVICE` on Linux).
- `-list`, `-quiet`, `-verbose` and `-version` options; command-line
  compatibility with go-dispatch-proxy.
- Warning when listening on a non-loopback address without authentication.

[Unreleased]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.2.0...HEAD
[1.2.0]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/ahmedanjum30/rust-dispatch-proxy/releases/tag/v1.0.0
