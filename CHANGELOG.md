# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [1.5.1] - 2026-11-18

### Added
- Control panel: a tick box next to each connection. Untick one to stop
  using it, tick it to use it again. The change applies within a second,
  without restarting the proxy, and downloads already running on a
  switched-off connection carry on until they finish. Switched-off
  connections stay in the list, and the choice is remembered by adapter
  name, so it survives address changes and restarts.
- `-exclude-file FILE`: don't use the adapters named in FILE (names or
  addresses, one per line). The proxy re-reads it every second. The status
  data lists switched-off adapters under `excluded`.

## [1.5.0] - 2026-11-18

### Added
- SOCKS5 UDP relaying (`UDP ASSOCIATE`), in both `rust-dispatch-proxy` and
  `proxy-gui.exe`. Apps that send UDP through SOCKS5 (games, voice chat,
  QUIC, DNS) now work through the proxy. Each UDP session goes out through
  one link, so the far end always sees one address; separate sessions are
  spread over the links like TCP connections. Only the client that opened
  a session can use its relay, and the session ends when the client closes
  its SOCKS connection. UDP traffic is counted on the status page. Not
  available with `-tunnel`, whose SOCKS5 upstreams can't carry it.

## [1.4.4] - 2026-11-08

### Fixed
- With a VPN such as Cloudflare WARP connected, every site failed with
  "502 Bad Gateway": the VPN blocks connections that go around it (Windows
  error 10013), and the proxy pins every connection to an adapter. Such a
  connection is now sent the way Windows routes it instead, through the VPN.
  While the VPN is on, traffic goes through its single tunnel and isn't
  spread over your connections; the log says so once instead of logging
  every failure. TUN mode is unchanged.

## [1.4.3] - 2026-11-04

### Changed
- Control panel updates keep GitHub's file name: **Update now** saves the
  new version next to the current one (e.g.
  `proxy-gui-v1.4.3-x86_64-pc-windows-msvc.exe`) instead of overwriting it,
  so the file name always says which version it is. The new version then
  takes over: it restarts the proxy from itself with exactly the options the
  running proxy had, points start-at-login at itself, and deletes the old
  file.
- A copy left with an older version's name by the 1.4.1/1.4.2 updater moves
  itself to the right name the first time its window opens.

### Fixed
- Control panel: if start-at-login points at a file that no longer exists
  (e.g. an older version deleted by hand), opening the window points it at
  the current file.

## [1.4.2] - 2026-11-04

### Fixed
- Control panel: Check for updates no longer fails with `GitHub answered with
  status 403`. It used GitHub's API, which allows 60 requests an hour per
  internet address; it now reads the latest release from github.com itself,
  which has no such limit.

## [1.4.1] - 2026-11-04

### Added
- Control panel: a **Check for updates** button. When a newer release is out,
  **Update now** downloads it, puts it in place of `proxy-gui.exe`, and
  restarts the proxy (if it was running) and the window. Nothing is checked
  or downloaded unless you press the button.

### Fixed
- Control panel: pressing Start or Stop no longer leaves the window jumbled.
  Messages have a fixed place, and leftovers of the old layout are repainted
  when a notice appears or goes away. The window grows if a notice would push
  the bottom out of view.

### Changed
- Release and CI workflows use the Node 24 versions of their GitHub actions
  and a fixed Ubuntu 24.04 runner, so the builds show no deprecation warnings.

## [1.4.0] - 2026-11-04

### Changed
- The control panel is rebuilt with Windows' own controls (buttons,
  checkboxes, edit boxes and a list view) instead of a GUI library.
  `proxy-gui.exe` goes from about 6 MB to under 1 MB, uses about 2 MB of
  memory while open instead of about 135 MB, looks and scales like a
  built-in Windows tool, and builds about twice as fast. It now follows
  Windows' light style.
- The connections heading says when the proxy isn't running, and the message
  line and the Windows-apps notice only take room when they have something
  to show.

## [1.3.1] - 2026-11-04

### Added
- Control panel: when Microsoft Store and other Windows apps can't reach the
  proxy (Windows blocks its sandboxed apps from connecting to programs on the
  same PC), it says so and fixes it in one click, with a single administrator
  prompt. Settings shows the status and can repeat it for newly installed apps.

### Fixed
- A connection timeout only counts against a link once another link has
  reached the same destination. Unreachable destinations, such as offline
  torrent peers, no longer put healthy links into cooldown.
- The log says once when a link goes into cooldown and once when it works
  again, instead of repeating it for every failed connection.

## [1.3.0] - 2026-10-14

### Changed
- `proxy-gui.exe` is now a single, standalone file: the proxy is built into
  it, so it needs nothing else. It runs the proxy as a hidden copy of itself
  (`proxy-gui.exe --proxy ...`), which keeps running when the window closes.
  It can still control a separate `rust-dispatch-proxy.exe` on the same port.
  TUN mode remains in the separate `rust-dispatch-proxy.exe`.
- The proxy is now also a library (`src/lib.rs`), shared by both programs.

### Fixed
- Control panel: the status light showed as a box, the "Downloaded" column
  was pushed to the far right, number headers now line up with their values,
  and the "closing this window doesn't stop the proxy" note shows again.

## [1.2.1] - 2026-10-14

### Changed
- The control panel is now a separate download (`proxy-gui-<version>-<platform>.exe`)
  instead of being inside the zip; put it in the same folder as
  `rust-dispatch-proxy.exe`. If the proxy is missing, it says where to get it.

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

[Unreleased]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.5.1...HEAD
[1.5.1]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.5.0...v1.5.1
[1.5.0]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.4.4...v1.5.0
[1.4.4]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.4.3...v1.4.4
[1.4.3]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.4.2...v1.4.3
[1.4.2]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.4.1...v1.4.2
[1.4.1]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.4.0...v1.4.1
[1.4.0]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.3.1...v1.4.0
[1.3.1]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.3.0...v1.3.1
[1.3.0]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.2.1...v1.3.0
[1.2.1]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.2.0...v1.2.1
[1.2.0]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/ahmedanjum30/rust-dispatch-proxy/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/ahmedanjum30/rust-dispatch-proxy/releases/tag/v1.0.0
