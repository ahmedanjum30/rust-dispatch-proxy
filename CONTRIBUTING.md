# Contributing

Thanks for helping improve rust-dispatch-proxy.

## Reporting bugs

Please open an issue and include:

- the version (`rust-dispatch-proxy -version`) and your OS
- the exact command line you ran
- the output with `-verbose`. Remove any public IP addresses you don't want to share.
- what you expected to happen and what happened instead

## Development

You need a recent stable [Rust toolchain](https://rustup.rs/).

```sh
cargo build                          # debug build with TUN mode
cargo build --no-default-features    # proxy only, no TUN dependencies
cargo test                           # unit tests
cargo test --no-default-features
```

Before opening a pull request, make sure these pass. CI runs them on Windows and Linux:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
```

TUN mode can't be tested in CI. If you change `src/tun_mode.rs` or `src/net.rs`, please test by hand on Windows from an administrator terminal. Check that browsing, a multi-connection download and a UDP app (for example a video call) all work, and that traffic returns to normal after you exit.

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
