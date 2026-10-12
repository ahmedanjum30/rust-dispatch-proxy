# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- `-auto`: use every adapter that can reach the internet, and add or drop
  links as the network changes, without restarting or cutting open
  connections. Event-driven on Windows (no polling); Linux checks every 5 s.
- `-system-proxy` (Windows): turn the system proxy setting on while at least
  `-min-links` links (default 2) are up, off otherwise, and off on Ctrl+C or
  when the console is closed. Replaces a separate watcher script.
- Link racing (`-race MS`, default 300): if a link hasn't connected within
  the delay, the next link starts alongside it and the first to connect wins.
  A link that loses to a later attempt counts as a failure, so a hanging link
  goes into cooldown instead of slowing every new connection.

### Changed
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

[Unreleased]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/ahmedanjum30/rust-dispatch-proxy/releases/tag/v1.0.0
