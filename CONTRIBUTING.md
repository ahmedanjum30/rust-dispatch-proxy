# Contributing

Thanks for helping improve rust-dispatch-proxy.

## Reporting bugs

Please open an issue and include:

- the version (`rust-dispatch-proxy -version`) and your OS
- the exact command line you ran
- the output with `-verbose`. Remove any public IP addresses you don't want to share.
- what you expected to happen and what happened instead

## Development

The project is built with Rust 1.99.0, pinned in `rust-toolchain.toml` ([rustup](https://rustup.rs/) installs it automatically), and uses the Rust 2024 edition. The proxy also builds with Rust 1.88 or newer.

```sh
cargo build                          # debug build with TUN mode
cargo build --no-default-features    # proxy only, no TUN dependencies
cargo test                           # unit tests
cargo test --no-default-features
cargo build -p proxy-gui             # the Windows control panel (gui/)
cargo test -p proxy-gui
```

On Windows without Visual Studio, use the GNU toolchain (`rustup default stable-gnu`). Building also needs MinGW's `dlltool` and `as`, for example from [WinLibs](https://winlibs.com/) (`winget install BrechtSanders.WinLibs.POSIX.UCRT`). Put its `mingw64\bin` folder first on `PATH`.

Before opening a pull request, make sure these pass. CI runs them on Windows and Linux:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
```

Some features can't be tested in CI, so please test them by hand on Windows if you change them:

- **TUN mode** (`src/tun_mode.rs`, `src/net.rs`): from an administrator terminal, check that browsing, a multi-connection download and a UDP app (for example a video call) all work, and that traffic returns to normal after you exit.
- **`-auto`** (`src/auto.rs`): check that the links logged at startup match the adapters that have a gateway in `ipconfig`. Then connect or disconnect one (for example Wi-Fi), and check that a "Network changed" line appears within a few seconds and that open downloads keep going.
- **`-system-proxy`** (`src/sysproxy.rs`): check that the setting under Settings → Network & internet → Proxy turns on with 2+ links, and that it turns off when you press Ctrl+C. Use another `-lport` so a proxy you're already running isn't affected.

## Guidelines

- Keep the binary small and dependency-light. Discuss new dependencies in an issue first.
- Keep the command line compatible with go-dispatch-proxy where it overlaps.
- Add a unit test for logic that can be tested without a network (balancing, parsing).
- Add an entry under `[Unreleased]` in `CHANGELOG.md` for user-visible changes.

## Releasing (maintainers)

1. Update `version` in `Cargo.toml` and move the `[Unreleased]` changelog entries under a new version heading.
2. Commit, then tag: `git tag v1.2.3 && git push --tags`.
3. The release workflow builds the binaries, bundles `wintun.dll` into the Windows archives, and publishes a GitHub release.

## License of contributions

The project is under the [PolyForm Noncommercial License 1.0.0](LICENSE), and Ahmad Anjum also offers commercial licenses. By contributing, you agree that your contributions are distributed under that license, and that Ahmad Anjum may also license them to others under other terms, including commercial licenses.
