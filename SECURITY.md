# Security policy

## Supported versions

Security fixes are made for the latest release only.

## Reporting a vulnerability

Please **don't** open a public issue for security problems. Report them privately through GitHub's [private vulnerability reporting](https://github.com/ahmedanjum30/rust-dispatch-proxy/security/advisories/new) instead.

Include a description of the issue, steps to reproduce, and the impact you expect. You should get a reply within a week.

## Scope and design notes

- The proxy has **no authentication** by design and listens on `127.0.0.1` by default. Exposing it with `-lhost` on an untrusted network turns it into an open proxy. That's expected behaviour and is documented, not a vulnerability.
- TUN mode needs administrator rights and installs routes that send all IPv4 traffic through the program. The routes are removed when the program exits.
- Issues we do want to hear about include crashes or memory exhaustion caused by malformed client input, traffic leaving through the wrong adapter in a way that defeats pinning, and requests reaching a different host than the client asked for.
