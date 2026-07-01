# WaveConductor

[![Release](https://img.shields.io/github/v/release/madisonrickert/WaveConductor)](https://github.com/madisonrickert/WaveConductor/releases)
[![Build](https://img.shields.io/github/actions/workflow/status/madisonrickert/WaveConductor/release.yml)](https://github.com/madisonrickert/WaveConductor/actions)
[![Deploy](https://img.shields.io/github/actions/workflow/status/madisonrickert/WaveConductor/deploy-web.yml?label=deploy)](https://madisonrickert.github.io/WaveConductor/)
[![License](https://img.shields.io/github/license/madisonrickert/WaveConductor)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Web-blue)]()

![WaveConductor](screenshot.png)

Interactive art gallery built with React, Three.js/WebGL, and Web Audio. Features generative visualizations that respond to mouse, touch, and Leap Motion hand tracking. Runs as a website or a self-contained Electron desktop app.

## Coming in v5: a from-scratch Rust port

The next major version is a ground-up rewrite in Rust on the [Bevy](https://bevyengine.org/) engine, in active development on the [`v5-alpha`](https://github.com/madisonrickert/WaveConductor/tree/v5-alpha) branch. It trades the React / Three.js / WebGL / Electron stack for a single native binary, built around the gallery-kiosk install as its design target: multi-hour unattended thermal stability over peak frame rate.

Highlights in progress:

- **Native cross-platform binaries** for macOS, Windows, and Linux, replacing the Electron wrapper.
- **Compute-shader particle simulation** on a WebGPU-class GPU (Metal, DX12, or Vulkan), with no WebGL2 or CPU fallback.
- **GPU-accelerated hand tracking** through an in-process MediaPipe pipeline on ONNX Runtime (CoreML on macOS, DirectML on Windows), alongside direct LeapC device support.
- **Multi-hour soak stability** from per-platform thermal sensing and a frame-rate governor for unattended, day-long runs.

The current shipping release is v4, documented below. Early v5 alpha builds appear on the [Releases](../../releases) page, marked as pre-releases.

## Features

- **5 interactive sketches** — generative visualizations built with Three.js/WebGL, each with unique physics and rendering
- **Generative audio** — every sketch produces real-time audio driven by its simulation state
- **Mouse, touch, and Leap Motion input** — immersive tactile interaction from a variety of input styles
- **Screensaver mode** — auto-activates after 30 seconds of idle
- **Advanced settings panel** — per-sketch tuning (particle density, quality, gamma) via `Shift`+`D` or gear icon
- **Electron desktop app** — fullscreen kiosk mode with display sleep prevention, auto-launching Leap Motion Websocket compatibility server
- **Cross-platform builds** — DMG for macOS, portable exe for Windows, and a browser target for web portfolio use

## Sketches

- **flame** — Iterated function system fractal driven by your name, with generative audio
- **line** — Particle line that responds to mouse/touch/Leap Motion attractors
- **dots** — Particle grid with gravitational attractors
- **cymatics** — Chladni plate vibration patterns with Leap Motion control
- **waves** — Audio-reactive wave visualization

## Development

Install [Node.js](https://nodejs.org/), then:

```sh
npm install
npm run start
```

Opens at http://localhost:5173. Supports hot module replacement.

## Web Build

```sh
npm run build     # Production build to dist/
npm run preview   # Serve the production build locally
```

## Electron App

```sh
npm run electron:dev       # Electron + Vite HMR dev mode
npm run electron:build     # Build renderer + main process
npm run electron:package   # Package into DMG (macOS) or portable exe (Windows)
```

### Windows build requirements

- Windows Developer Mode enabled (Settings > System > For developers) — required for electron-builder's code signing cache extraction

### Cross-compiling

To cross-compile for Windows from macOS, install Wine (`brew install --cask wine-stable`) then:

```sh
npm run electron:build && npx electron-builder --win
```

The Electron app auto-launches the Ultraleap WebSocket binary (if present in `bin/`) and enables audio autoplay without user gesture.

## Installation

Download the latest release from the [Releases](../../releases) page.

**macOS:** The app is self-signed but not notarized, so Gatekeeper will show a warning on first launch. Right-click the app and choose **Open**, then click **Open** in the dialog. You only need to do this once. Alternatively, run `xattr -cr /Applications/WaveConductor.app` from Terminal.

**Windows:** SmartScreen may show "Windows protected your PC" since the exe is not signed. Click **More info**, then **Run anyway**.

## Releasing

1. Bump `version` in `package.json` and commit
2. Run `npm run release:tag` to create and push the git tag
3. GitHub Actions builds macOS DMG + Windows exe and creates a draft release
4. Review the draft on the [Releases](../../releases) page, edit notes if needed, then publish

The web build deploys to GitHub Pages automatically on every push to `main`.

## Keyboard Shortcuts

| Key          | Action                          |
|--------------|---------------------------------|
| `1`–`5`      | Jump to sketch (1=line, 2=flame, 3=dots, 4=cymatics, 5=waves) |
| `z` / `←`    | Previous sketch                 |
| `x` / `→`    | Next sketch                     |
| `Escape`     | Return to home / gallery        |
| `v`          | Toggle volume on/off            |
| `Shift+D`    | Toggle advanced settings panel  |
| `F11`        | Toggle fullscreen (Electron)    |
| `Alt+F4`     | Quit application (Electron/Windows) |

## Leap Motion

Optional. Sketches support [Leap Motion](https://www.ultraleap.com/) hand tracking.

Compatible with Leap Motion Software 4.x out of the box. For 5.x+ (Gemini), the [UltraleapTrackingWebSocket](https://github.com/madisonrickert/UltraleapTrackingWebSocket) compatibility layer is needed. Leap appears to be abandonware at this point so that link is Madison's updated fork. Pre-built binaries are included for macOS (Apple Silicon & Intel) and Windows:

```sh
npm run leap-websocket
```

In Electron mode, this binary is launched automatically on startup.
