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

## Deployment hardware

WaveConductor's primary install target is an unattended gallery kiosk running for 8+ hours at a stretch. The constraints that shape the box: a WebGPU-class GPU (compute-shader particle path, no WebGL2/CPU fallback), a Leap Motion Controller as the primary input modality, and sustained thermal stability over peak FPS.

### Recommended

- **ASUS NUC 14 Pro** (Core Ultra 7 + Arc iGPU) or **Minisforum UM890 Pro** (Ryzen 9 8945HS + Radeon 780M)
- 16 GB DDR5 / 256 GB NVMe
- **Windows 11** — the best-supported path for the Ultraleap Gemini SDK

### Minimum

- Intel 11th/12th gen with Iris Xe (96 EU), or AMD Ryzen 5000-series with Radeon iGPU
- 8 GB RAM, USB 3.0 (Leap 1 USB-A, Leap 2 USB-C)
- Vulkan 1.2 / DX12 / Metal-capable
- Fanned chassis — fanless mini-PCs typically thermal-throttle before the 8-hour soak finishes

### Hand-tracking by OS

| OS | Leap Motion support | Notes |
|---|---|---|
| Windows 11 | ✓ Full | Ultraleap Gemini SDK — the well-trodden path. |
| Ubuntu 22.04 LTS | ✓ Works | Gemini ships a `.deb`; only Ubuntu LTS is officially supported. |
| macOS | ✗ | Ultraleap dropped macOS support with Gemini (V5). The Leap Motion Controller 2 has no macOS driver. A Mac mini is excellent for everything else, but cannot drive the kiosk's Leap input. |

The 8-hour soak harness (`cargo xtask soak-test --duration 8h`) is the authoritative gate before any release tag — confirm any candidate box passes it on the deployment hardware before committing to an install.

## Documentation

- `AGENTS.md` — coding standards
- `docs/superpowers/specs/` — design specs
- `docs/superpowers/plans/` — implementation plans
- `docs/adr/` — architecture decision records

## License

See [LICENSE](LICENSE).
