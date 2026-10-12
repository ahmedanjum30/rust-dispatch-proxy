//! The status page, served on the proxy's own port: open
//! http://127.0.0.1:8080/status in a browser (or /status.json for scripts).
//!
//! The server only reports running totals; the page polls them once a
//! second and works out the speeds itself, so nothing is measured or stored
//! while nobody is looking.

use std::fmt::Write;
use std::sync::atomic::Ordering::Relaxed;

use crate::{Balancer, Via, now_ms};

pub(crate) fn json(balancer: &Balancer) -> String {
    let now = now_ms();
    let mut out = format!("{{\"version\":\"{}\",\"uptime_s\":{},\"links\":[", env!("CARGO_PKG_VERSION"), now / 1000);
    for (i, link) in balancer.links().iter().enumerate() {
        let (name, adapter) = match &link.via {
            Via::Adapter(e) => (e.ip.to_string(), e.if_name.clone()),
            Via::Upstream(_) => (link.via.to_string(), "SOCKS5 upstream".to_string()),
        };
        let _ = write!(
            out,
            "{}{{\"link\":\"{}\",\"adapter\":\"{}\",\"weight\":{},\"active\":{},\"connections\":{},\"failures\":{},\
             \"connect_ms\":{:.1},\"bytes_down\":{},\"bytes_up\":{},\"cooldown_s\":{}}}",
            if i > 0 { "," } else { "" },
            escape(&name),
            escape(&adapter),
            link.weight,
            link.active.load(Relaxed),
            link.connections.load(Relaxed),
            link.failures.load(Relaxed),
            link.connect_us.load(Relaxed) as f64 / 1000.0,
            link.bytes_down.load(Relaxed),
            link.bytes_up.load(Relaxed),
            link.down_until_ms.load(Relaxed).saturating_sub(now).div_ceil(1000),
        );
    }
    out.push_str("]}");
    out
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

pub(crate) const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>rust-dispatch-proxy status</title>
<style>
  :root { --bg: #ffffff; --fg: #1f2328; --muted: #656d76; --line: #d0d7de; --bar: #2f81f7; --bad: #cf222e; }
  @media (prefers-color-scheme: dark) { :root { --bg: #0d1117; --fg: #e6edf3; --muted: #8d96a0; --line: #30363d; --bar: #4493f8; --bad: #f85149; } }
  body { background: var(--bg); color: var(--fg); font: 14px/1.5 system-ui, sans-serif; margin: 24px; }
  h1 { font-size: 18px; margin: 0 0 4px; }
  .muted { color: var(--muted); }
  table { border-collapse: collapse; margin-top: 16px; width: 100%; max-width: 1000px; }
  th, td { text-align: right; padding: 6px 10px; border-bottom: 1px solid var(--line); white-space: nowrap; }
  th:first-child, td:first-child { text-align: left; }
  th { color: var(--muted); font-weight: 600; }
  tfoot td { font-weight: 600; border-bottom: none; }
  .bar { height: 4px; background: var(--bar); border-radius: 2px; margin-top: 3px; }
  .bad { color: var(--bad); }
</style>
</head>
<body>
<h1>rust-dispatch-proxy</h1>
<div class="muted" id="meta">connecting...</div>
<table>
  <thead><tr><th>Link</th><th>Download</th><th>Upload</th><th>Open</th><th>Connections</th><th>Connect</th><th>Failures</th><th>Total down</th><th>Total up</th></tr></thead>
  <tbody id="rows"></tbody>
  <tfoot><tr id="total"></tr></tfoot>
</table>
<script>
let last = null;
const mbit = (bytes, secs) => (bytes * 8 / 1e6 / secs);
const size = b => b >= 1e9 ? (b / 1e9).toFixed(2) + " GB" : (b / 1e6).toFixed(1) + " MB";
const esc = s => s.replace(/[&<>"]/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
async function tick() {
  try {
    const r = await fetch("/status.json", { cache: "no-store" });
    const s = await r.json();
    const now = performance.now() / 1000;
    const prev = last && new Map(last.s.links.map(l => [l.link, l]));
    const dt = last ? now - last.t : 0;
    const rates = s.links.map(l => {
      const p = prev && prev.get(l.link);
      return p && dt > 0 ? [mbit(l.bytes_down - p.bytes_down, dt), mbit(l.bytes_up - p.bytes_up, dt)] : [0, 0];
    });
    const peak = Math.max(1, ...rates.map(r => r[0]));
    let html = "", down = 0, up = 0, open = 0, conns = 0, tdown = 0, tup = 0;
    s.links.forEach((l, i) => {
      const [d, u] = rates[i];
      down += d; up += u; open += l.active; conns += l.connections; tdown += l.bytes_down; tup += l.bytes_up;
      const state = l.cooldown_s > 0 ? ` <span class="bad">cooling down ${l.cooldown_s}s</span>` : "";
      html += `<tr><td>${esc(l.link)} <span class="muted">${esc(l.adapter)}${l.weight > 1 ? " @" + l.weight : ""}</span>${state}</td>` +
        `<td>${d.toFixed(1)} Mbit/s<div class="bar" style="width:${(100 * d / peak).toFixed(0)}%"></div></td>` +
        `<td>${u.toFixed(1)} Mbit/s</td><td>${l.active}</td><td>${l.connections}</td>` +
        `<td>${l.connect_ms ? l.connect_ms.toFixed(0) + " ms" : "-"}</td>` +
        `<td${l.failures ? ' class="bad"' : ""}>${l.failures}</td><td>${size(l.bytes_down)}</td><td>${size(l.bytes_up)}</td></tr>`;
    });
    document.getElementById("rows").innerHTML = html || `<tr><td colspan="9" class="muted">No internet connections found yet</td></tr>`;
    document.getElementById("total").innerHTML =
      `<td>Total</td><td>${down.toFixed(1)} Mbit/s</td><td>${up.toFixed(1)} Mbit/s</td><td>${open}</td><td>${conns}</td><td></td><td></td><td>${size(tdown)}</td><td>${size(tup)}</td>`;
    const h = Math.floor(s.uptime_s / 3600), m = Math.floor(s.uptime_s % 3600 / 60);
    document.getElementById("meta").textContent = `version ${s.version} · running ${h}h ${m}m · ${s.links.length} link(s) · updates every second`;
    last = { s, t: now };
  } catch (e) {
    document.getElementById("meta").textContent = "proxy not reachable";
  }
}
tick();
setInterval(tick, 1000);
</script>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_json_strings() {
        assert_eq!(escape(r#"Wi-Fi "home"\x"#), r#"Wi-Fi \"home\"\\x"#);
        assert_eq!(escape("a\nb"), "a\\u000ab");
    }
}
