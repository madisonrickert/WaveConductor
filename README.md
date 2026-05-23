# WaveConductor

[![License](https://img.shields.io/github/license/madisonrickert/WaveConductor)](LICENSE)

Interactive art gallery. Five generative-art sketches with hand-tracking and audio reactivity.

> **v5 is under construction on the `rewrite/bevy` branch.** The current shipping release is v4 — see [Releases](../../releases) for binaries.
>
> v5 is a from-scratch rewrite in Rust on the Bevy engine, designed for multi-hour unattended thermal stability. See `docs/superpowers/specs/2026-05-22-bevy-rewrite-design.md` for the design.

## Development (v5)

```sh
cargo run -p waveconductor
```

Requires Rust 1.89+. Pinned via `rust-toolchain.toml`.

### Linux build prerequisites

On Debian/Ubuntu, install Bevy's native dependencies:

```sh
sudo apt-get install -y \
    libasound2-dev libudev-dev \
    libwayland-dev libxkbcommon-dev \
    libx11-dev libxcursor-dev libxi-dev libxrandr-dev
```

macOS and Windows have no extra prerequisites beyond Rust.

## Documentation

- `AGENTS.md` — coding standards
- `docs/superpowers/specs/` — design specs
- `docs/superpowers/plans/` — implementation plans
- `docs/adr/` — architecture decision records

## License

See [LICENSE](LICENSE).
