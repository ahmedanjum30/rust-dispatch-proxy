# rust-dispatch-proxy

[![Support on Patreon](https://img.shields.io/badge/Support%20on-Patreon-F96854?style=for-the-badge&logo=patreon&logoColor=white)](https://www.patreon.com/cw/ahmedanjum30)

**Combine several internet connections into one.** `rust-dispatch-proxy` is a small, fast load-balancing proxy that spreads your outgoing connections across every network adapter you give it (Ethernet, Wi-Fi, a USB-tethered phone, a second ISP, and so on). Downloads and other multi-connection traffic then use all of your connections at once.

[![CI](https://github.com/ahmedanjum30/rust-dispatch-proxy/actions/workflows/ci.yml/badge.svg)](https://github.com/ahmedanjum30/rust-dispatch-proxy/actions/workflows/ci.yml)
[![License: PolyForm Noncommercial](https://img.shields.io/badge/license-PolyForm%20Noncommercial-blue.svg)](LICENSE)

Free for personal and other non-commercial use. See [License](#license).

```
                        ┌──────────────────────────┐      ┌── Ethernet ──► ISP A
  browser, downloader,  │                          │      │
  any app ─────────────►│   rust-dispatch-proxy    ├──────┼── Wi-Fi ─────► ISP B
  (SOCKS5 / HTTP / TUN) │   weighted balancer      │      │
                        └──────────────────────────┘      └── Phone USB ─► 4G/5G
```

It's a Rust rewrite of [go-dispatch-proxy](https://github.com/extremecoders-re/go-dispatch-proxy) that keeps its command line (`-lhost`, `-lport`, `-list`, `-tunnel`, `IP@ratio`), with smarter balancing, automatic failover, an HTTP proxy, and an optional system-wide mode. See [Coming from go-dispatch-proxy](#coming-from-go-dispatch-proxy).

---

## Features

- **Three ways in**
  - **SOCKS5** for any app that supports a SOCKS proxy (browsers, download managers, torrent clients, `curl`).
  - **HTTP proxy** on the same port, so you can point **Windows' own proxy setting** at it and every app that follows that setting is balanced.
  - **TUN mode** (`-tun`) adds a virtual network adapter that captures **all IPv4 traffic from every app**, TCP and UDP, with no per-app setup.
- **Two kinds of links.** Combine local network adapters, or use `-tunnel` to combine upstream SOCKS5 proxies such as `ssh -D` tunnels to several servers.
- **Weighted least-connections balancing.** Each new connection goes to the link with the lowest `(active + 1) / weight`, so a 3:1 weighting really carries three times the connections.
- **Hands-off mode.** With `-auto` it finds every connection that can reach the internet by itself, and adds or drops links as cables, Wi-Fi and tethered phones come and go, **without restarting and without cutting open connections**. With `-system-proxy` it also switches the Windows proxy setting on and off for you.
- **Racing links.** If a link hasn't connected within 300 ms (or 3× its usual connect time, if that's longer), the next link is tried alongside it and the first to connect wins. A flaky link costs a fraction of a second, not a timeout. The losing attempt finishes in the background, so a link that's only slower isn't punished, while one that hangs still times out and gets skipped.
- **Automatic failover.** If a link fails, the next one is tried right away. A link that fails twice in a row is skipped for 10 seconds and is used again as soon as it works.
- **Live status page.** Open `http://127.0.0.1:8080/status` to see each link's download and upload speed, open connections, connect time and failures, updated every second.
- **Fast failure.** It uses a 4-second connect timeout instead of the ~21-second Windows default.
- **Traffic stays on the adapter you chose.** Outgoing sockets are pinned to the adapter itself (`IP_UNICAST_IF` / `SO_BINDTODEVICE`), not just to its IP address.
- **Small and quiet.** It's a single static binary with no config file and no per-connection logging unless you ask for it.

## How it works

```
 new connection ─► pick link with lowest (active+1)/weight ─► connect through that adapter
                         │                                        │
                         │◄── failed, or not connected after ─────┘
                         │    300 ms? start the next link too;
                         │    first to connect wins
                         ▼
                   relay bytes both ways until either side closes
```

Balancing is **per connection**, not per packet. A single connection, such as one file download stream, always uses one link. The speedup comes from apps that open many connections in parallel: download managers, torrent clients, browsers loading many resources, cloud sync, and so on.

## Choosing a mode

| | SOCKS5 / HTTP proxy | Windows proxy setting | TUN mode (`-tun`) |
|---|---|---|---|
| Which apps are balanced | Apps you configure | Apps that follow the system proxy (most browsers, many desktop apps) | **All apps** |
| UDP (games, QUIC, VoIP) | No | No | Yes, each flow stays on one link |
| Admin rights | No | No | **Yes** |
| Extra files | None | None | `wintun.dll` (included in releases) |

The SOCKS5/HTTP proxy always runs. `-tun` adds the system-wide mode on top of it.

## Installation

### Download a release

Grab the archive for your platform from the [Releases](https://github.com/ahmedanjum30/rust-dispatch-proxy/releases) page. Windows archives already contain `wintun.dll`, which TUN mode needs. Keep it in the same folder as `rust-dispatch-proxy.exe`.

### Build from source

You need [Rust](https://rustup.rs/) 1.85 or newer.

```sh
git clone https://github.com/ahmedanjum30/rust-dispatch-proxy.git
cd rust-dispatch-proxy
cargo build --release
# binary: target/release/rust-dispatch-proxy(.exe)
```

To build only the SOCKS5/HTTP proxy, without TUN mode and its dependencies:

```sh
cargo build --release --no-default-features
```

For TUN mode on Windows, download Wintun from [wintun.net](https://www.wintun.net/) and copy `wintun/bin/<arch>/wintun.dll` (`amd64` for most PCs) next to the executable.

## Quick start

### The easy way (Windows)

Connect all your internet connections, for example Ethernet plus a USB-tethered phone, then run:

```
> rust-dispatch-proxy -auto -system-proxy
[INFO] Link 1: 192.168.1.10 (Ethernet), weight 1
[INFO] Link 2: 192.168.43.25 (Wi-Fi), weight 1
[INFO] Proxy started at 127.0.0.1:8080 (SOCKS5 and HTTP)
[INFO] Windows proxy setting ON
```

That's all. Here's what it does:
- **Finds the links itself:** it uses every adapter that has a gateway to the internet.
- **Follows network changes:** when a connection joins or drops, it updates the links within a few seconds. Open connections aren't cut.
- **Manages the Windows proxy setting:** the setting is on while at least 2 connections are up (`-min-links` changes that number). It turns off when fewer are up, and when you stop the proxy with Ctrl+C or by closing its window.

To see it working, open **http://127.0.0.1:8080/status** in a browser. It shows each connection's live speed and how many connections each one carries.

If the program is force-killed (Task Manager, `taskkill /f`), it can't switch the setting off. Logging off or shutting down may leave it on too. If the proxy starts at login (see [Running at startup](#running-at-startup-windows)), that's harmless: it sets the setting again a moment after you log in. Otherwise, start the proxy again or turn the setting off under **Settings → Network & internet → Proxy**.

### Choosing the connections yourself

**1. Connect all your internet connections** and find their local IPv4 addresses:

```
> rust-dispatch-proxy -list
--- Listing the available addresses for dispatching
[+] Ethernet, IPv4:192.168.1.10
[+] Wi-Fi, IPv4:192.168.43.25
```

Each connection needs its **own adapter with its own gateway**. Two IPs on the same router don't give you more bandwidth.

**2. Start the proxy** with the addresses to combine:

```
> rust-dispatch-proxy 192.168.1.10 192.168.43.25
[INFO] Link 1: 192.168.1.10 (Ethernet), weight 1
[INFO] Link 2: 192.168.43.25 (Wi-Fi), weight 1
[INFO] Proxy started at 127.0.0.1:8080 (SOCKS5 and HTTP)
```

**3. Send traffic through it.** Use one of the options below.

### Option A: configure an app directly

Set the app's proxy to **SOCKS5** (or HTTP) at `127.0.0.1`, port `8080`. For example:

```sh
curl -x socks5h://127.0.0.1:8080 https://example.com
curl -x http://127.0.0.1:8080   https://example.com
```

### Option B: Windows proxy setting

Add `-system-proxy` and the program manages the setting for you (see [the easy way](#the-easy-way-windows)).

To set it by hand instead, open **Settings → Network & internet → Proxy → Manual proxy setup**, turn on **Use a proxy server**, and enter address `127.0.0.1`, port `8080`. Browsers and other apps that follow the system proxy are now balanced. Turn the setting off again when you stop the proxy, or those apps will lose their internet connection.

### Option C: TUN mode, everything on the system

Run from an **administrator** terminal:

```
> rust-dispatch-proxy -tun 192.168.1.10 192.168.43.25
[INFO] Proxy started at 127.0.0.1:8080 (SOCKS5 and HTTP)
[INFO] TUN adapter 'DispatchProxy' is up: all IPv4 traffic now goes through the balancer
```

A virtual adapter named `DispatchProxy` appears, and routes `0.0.0.0/1` and `128.0.0.0/1` send all IPv4 traffic into it. These routes are more specific than your normal default route, so they win without changing it. Your local network keeps working because its routes are more specific still. When the program exits (Ctrl+C, closed window or crash), Windows removes the adapter and its routes, and traffic goes back to normal.

Don't use the Windows proxy setting together with TUN mode. It's unnecessary.

## Tunnel mode: combining SOCKS5 proxies

With `-tunnel`, the links are upstream SOCKS5 proxies instead of local adapters. The classic use is several SSH dynamic-forwarding tunnels, each to a different server:

```sh
ssh -N -D 7777 user@server-a
ssh -N -D 7778 user@server-b

rust-dispatch-proxy -tunnel 127.0.0.1:7777 127.0.0.1:7778@2
```

- Links are written `HOST:PORT[@weight]`. The host can be an IP address, a hostname or `[IPv6]`.
- Hostnames that apps ask for are passed to the upstream **unresolved**, so DNS lookups happen at the far end.
- Failover and cooldown work as with adapters. If a tunnel can't be reached, the next one is tried. If a working tunnel reports that the destination is unreachable, the next tunnel is tried, but the first one isn't put into cooldown.
- Upstreams must accept SOCKS5 without authentication, which is the case for `ssh -D`.
- `-tunnel` can't be combined with `-tun`, because TUN mode also forwards UDP, which these tunnels can't carry.

## Weights

Add `@weight` after an address to send it a larger share of connections. Use roughly the ratio of the links' speeds:

```sh
# 100 Mbps fibre + 25 Mbps 4G: about 4:1
rust-dispatch-proxy 192.168.1.10@4 192.168.43.25@1
```

The default weight is `1`. go-dispatch-proxy calls this the *contention ratio*, and the syntax is the same.

## Command-line reference

```
rust-dispatch-proxy [options] IP[@weight] [IP[@weight] ...]
rust-dispatch-proxy [options] -tunnel HOST:PORT[@weight] [HOST:PORT[@weight] ...]
```

| Option | Default | Description |
|---|---|---|
| `-lhost ADDR` | `127.0.0.1` | Address to listen on. See [Security](#security) before changing it. |
| `-lport PORT` | `8080` | Port for the SOCKS5 and HTTP proxy. |
| `-auto` | off | Use every adapter that can reach the internet, and follow network changes while running. Don't pass addresses with it. Windows and Linux. |
| `-system-proxy` | off | Windows: turn the system proxy setting on while there are enough links, off when there aren't, and off on exit. |
| `-min-links N` | `2` | How many links `-system-proxy` needs before it turns the setting on. |
| `-timeout SECS` | `4` | Connect timeout per link before failing over. Fractions are allowed (`2.5`). |
| `-race MS` | `300` | If a link hasn't connected within this many milliseconds, start the next link alongside it. `0` turns racing off. |
| `-tun` | off | Also capture all system IPv4 traffic through a virtual adapter. |
| `-tunnel` | off | Links are upstream SOCKS5 proxies (`HOST:PORT`) instead of local addresses. See [Tunnel mode](#tunnel-mode-combining-socks5-proxies). |
| `-list` | | List usable local IPv4 addresses and exit. |
| `-quiet` | | No log output. |
| `-verbose` | | Log every connection, which link it used and how long it took to connect. |
| `-log FILE` | | Write the log to `FILE` instead of the console. The file starts fresh each run. Lines are timestamped. |
| `-version` | | Print the version and exit. |
| `-help` | | Show usage. |

Options also work with two dashes (`--lport 1080`) and with `=` (`-lport=1080`).

Exit codes: `0` for success, `1` for a fatal error such as a bad address or a busy port, and `2` for a usage error.

## Coming from go-dispatch-proxy

Existing go-dispatch-proxy command lines, including `-tunnel`, work unchanged.

| | go-dispatch-proxy | rust-dispatch-proxy |
|---|---|---|
| Balancing | Rotates through links by contention ratio | Picks the link with the fewest active connections, adjusted by weight |
| A link is down | Connection fails after the OS timeout (~21 s on Windows) | Fails over to the next link after 4 s (`-timeout`) and skips the dead link for 10 s |
| Protocols | SOCKS5 | SOCKS5 and HTTP proxy on the same port |
| Windows system proxy | No | Yes |
| All apps, including UDP | No | `-tun` |
| Traffic kept on the chosen adapter | Bound to the IP address | Bound to the IP address and pinned to the adapter |
| `-tunnel` (balance across SSH SOCKS tunnels) | Yes | Yes, with failover; hostnames resolved at the far end |
| Logging | Every connection | Only with `-verbose` |

## Running at startup (Windows)

To start it hidden at log-on, save this as `rust-dispatch-proxy.vbs` in your Startup folder. Press Win+R, type `shell:startup` and press Enter to open it.

```vb
CreateObject("Wscript.Shell").Run """C:\Tools\rust-dispatch-proxy\rust-dispatch-proxy.exe"" -auto -system-proxy -log ""C:\Tools\rust-dispatch-proxy\proxy.log""", 0, False
```

With `-auto` the command line never needs updating when your local addresses change. To stop it, end `rust-dispatch-proxy.exe` in Task Manager, then switch the proxy setting off (see [the easy way](#the-easy-way-windows)).

TUN mode needs admin rights, so for `-tun` use a scheduled task instead, from an administrator PowerShell:

```powershell
$exe     = "C:\Tools\rust-dispatch-proxy\rust-dispatch-proxy.exe"
$action  = New-ScheduledTaskAction -Execute $exe -Argument "-tun -auto -quiet"
$trigger = New-ScheduledTaskTrigger -AtLogOn
Register-ScheduledTask -TaskName "rust-dispatch-proxy" -Action $action -Trigger $trigger -RunLevel Highest
```

## Limitations

- **One connection uses one link.** A single TCP stream is never split across links, so a single-stream download or a video call doesn't get faster. Apps that open many connections do.
- **Your public IP changes from connection to connection.** Sites that tie a login session to your IP address, such as some banks, CAPTCHAs and streaming services, may log you out or ask you to verify. Use those sites without the proxy, or with a single link.
- **IPv4 only.** IPv6 destinations are refused by the SOCKS5/HTTP proxy, and TUN mode doesn't capture IPv6 traffic, which goes out normally and unbalanced. To balance everything in TUN mode, turn off IPv6 on your adapters.
- **No ICMP.** In TUN mode `ping` and `tracert` don't work. TCP and UDP do.
- **DNS** for proxied hostnames uses the system resolver. In TUN mode, apps' own DNS queries are captured and balanced like any other UDP traffic.
- **SOCKS5:** only `CONNECT` is supported, with no `BIND` or `UDP ASSOCIATE`, and no username/password authentication.
- **TUN mode** is fully automatic on Windows only. On Linux the device is created but you must add the routes yourself, for example `ip route add 0.0.0.0/1 dev DispatchProxy` and `ip route add 128.0.0.0/1 dev DispatchProxy`. Treat it as experimental. Other platforms support only the SOCKS5/HTTP proxy.

## Security

The proxy has **no authentication**. By default it listens only on `127.0.0.1`, so only programs on your own machine can use it. If you set `-lhost 0.0.0.0` to share your connections with other devices on your network, anyone who can reach that port can use your internet connection. Only do this on a network you trust, and block the port at your firewall from anywhere else. Anyone who can reach the port can also see the status page, which shows your connections' local addresses and traffic. The program prints a warning when it listens on a non-loopback address.

HTTPS traffic is relayed as-is. The proxy never sees or changes encrypted content.

See [SECURITY.md](SECURITY.md) for how to report a vulnerability.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `x.x.x.x is not assigned to any network adapter` | The address changed or the adapter is down. Run `-list` and use the current addresses. |
| `Could not start local server on 127.0.0.1:8080` | Something else is using the port. Pick another with `-lport`. |
| `TUN mode stopped: ... needs administrator rights` | Run the terminal as administrator. |
| `wintun.dll not found at ...` | Put `wintun.dll` for your CPU architecture next to the `.exe`. |
| Only one link ever shows traffic | Check that each address belongs to a *different* adapter with its own gateway, and that the app opens more than one connection. The [status page](http://127.0.0.1:8080/status) shows how connections are spread. |
| A link keeps being "skipped for 10 s" | That connection is failing to reach the internet, or is much slower than the others. Check it on its own. |
| Apps have no internet after the proxy stopped | It was force-killed while `-system-proxy` had the setting on. Start the proxy again, or turn the setting off under **Settings → Network & internet → Proxy**. |
| `-auto` doesn't use an adapter | Only adapters with a default gateway count. Check that the adapter shows a gateway in `ipconfig`. |

## Project layout

```
src/
├── main.rs       CLI, load balancer and link racing, SOCKS5 server and upstream client, protocol detection
├── auto.rs       -auto: finding adapters with internet access and following network changes
├── sysproxy.rs   -system-proxy: the Windows proxy setting
├── status.rs     The status page (/status and /status.json)
├── log.rs        Timestamped log output (console or -log file)
├── http.rs       HTTP proxy (CONNECT and plain-HTTP forwarding)
├── net.rs        Sockets pinned to a specific network adapter
└── tun_mode.rs   System-wide mode: Wintun adapter, routes, user-space TCP/IP stack
```

## Contributing

Bug reports and pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Support the project

rust-dispatch-proxy is free for personal use. If it saves you time or gets more out of your connections, you can support its development on **[Patreon](https://www.patreon.com/cw/ahmedanjum30)**. Thank you!

## Acknowledgements

- [go-dispatch-proxy](https://github.com/extremecoders-re/go-dispatch-proxy) by extremecoders-re, the original motivation for this project, and the NodeJS [dispatch-proxy](https://github.com/Morhaus/dispatch-proxy) that inspired it.
- [Wintun](https://www.wintun.net/) by the WireGuard project, the virtual adapter driver behind TUN mode on Windows.
- [ipstack](https://crates.io/crates/ipstack) and [tun](https://crates.io/crates/tun), which provide the user-space TCP/IP stack and the TUN device.

## License

rust-dispatch-proxy is **source-available** under the [PolyForm Noncommercial License 1.0.0](LICENSE).

- **Allowed:** personal use, hobby projects, study and research, and use by charities, schools, public research organisations and government bodies. You can read, modify and share the code for these purposes.
- **Not allowed:** any commercial use, including use by or for a business, in a commercial product or service, or to make money.

**Commercial licenses** are available. To use rust-dispatch-proxy commercially, contact [Ahmad Anjum on GitHub](https://github.com/ahmedanjum30).

Release archives for Windows also include `wintun.dll`, which is distributed under its own license (`WINTUN-LICENSE.txt` in the archive).
