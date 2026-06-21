# WaveConductor v5 — macOS AVFoundation Webcam Capture Backend

**Date:** 2026-06-21
**Workstream:** Dependency hygiene + thermal — replace nokhwa's macOS capture path with a maintained `objc2` AVFoundation backend
**Status:** Design approved, ready for implementation plan
**Scope window:** ~2–4 focused days (objc2 capture backend + push→pull bridge + idle hardware throttle + feature gating + file split + build.rs cleanup + tests)
**Branch:** off `v5-alpha`; merges back to `v5-alpha` on Madison's sign-off

## Goal

Remove the abandoned [`core-video-sys`](https://github.com/LuoZijun/rust-core-video-sys) crate (and the entire stale, unmaintained generation of macOS binding crates it anchors) from WaveConductor's dependency tree **on macOS**, without giving up cross-platform webcam capture on Linux and Windows.

We do this by replacing **only nokhwa's macOS backend** with a small, in-house AVFoundation capture backend built on the maintained `objc2` framework crates, and gating `nokhwa` itself to non-macOS targets. nokhwa stays the capture provider on Linux (V4L2) and Windows (MediaFoundation), where it touches no Apple crates anyway.

The new backend slots into the existing `FrameSource` trait with no change to the worker's capture→process→publish loop, the pipeline, or any consumer. As a bonus thermal win not present today, it lowers the **hardware** camera capture rate while the app is idle/screensaver, shedding sensor/ISP work beyond the worker's existing decode-skipping.

## Background

### Why this work exists

`core-video-sys` 0.1.4 (last released 2020-05-06) reaches our tree only on macOS, only when the `hand-tracking-mediapipe-camera` feature is on:

```
core-video-sys 0.1.4  ←  nokhwa-bindings-macos 0.2.4  ←  nokhwa 0.10.11  ←  wc-core
```

It is **not a security vulnerability** — `cargo deny check advisories` passes clean, and it binds a stable Apple system framework. The cost is hygiene and maintenance:

- It is the **sole** reason we compile an old, unmaintained generation of macOS crates that nothing else in the tree needs: `objc 0.2.7` (superseded by `objc2`), `metal 0.18.0`, `cocoa 0.20.2`, `cocoa-foundation 0.2.1`, `core-graphics 0.19.2`, and a duplicate `core-foundation-sys 0.7.0`.
- That `objc 0.2` chain already costs us a concrete maintenance burden: a ~60-line `relink_objc_exception_for_dynamic_linking` hack in `crates/waveconductor/build.rs` that `-force_load`s `objc_exception`'s C shim, because `bevy/dynamic_linking` (`cargo rund`) drops the `cargo:rustc-link-lib` directive and the link fails without it.

Meanwhile the maintained replacement is already largely in our tree: we carry **28 `objc2` crate entries** (via Bevy/winit), including `objc2 0.6.4`, `dispatch2`, `objc2-metal`, `objc2-quartz-core`, `objc2-core-image`, and `objc2-core-foundation`.

### Why not just bump or switch libraries

- **Bumping nokhwa won't help.** We are already on the latest `nokhwa` 0.10.11 (released 2026-05-15) and its newest `nokhwa-bindings-macos` 0.2.4 (same date) still pins `core-video-sys` 0.1.4. The chain is frozen upstream.
- **No Rust camera library is meaningfully better than nokhwa for our needs** (library survey, 2026-06-21):

| Crate | macOS stack | Maintained? | Verdict |
|---|---|---|---|
| **nokhwa** (current) | `core-video-sys` + `objc 0.2` | active (0.10.11, May 2026) | Most mature, best cross-platform. Only wart: the stale macOS chain. **Keep for Linux/Windows.** |
| **kamera** | `objc2 0.4` + `icrate` | abandoned (v0.0.2, Aug 2023) | Only objc2-based one, but pre-alpha, no format enumeration, pins an old objc2. Trading one abandoned crate for a more abandoned one. |
| **ccap-rs** | vendored C++ lib | recent (Mar 2026) | Maintained but a C++ binding (adds a C++ toolchain to the build, complicates macOS dynamic-linking + CI), very niche (298 downloads). |
| **crabcamera** | `nokhwa` + `objc 0.2` | active | Worse: still depends on nokhwa **and** objc 0.2, plus tauri/tokio/openh264/cpal. Eliminated. |

The problem was never nokhwa as a whole — it is specifically nokhwa's macOS binding chain. So the surgical fix is to replace that one backend in our own code, which keeps full cross-platform support.

### Deployment constraint

Camera capture must run on **macOS + Linux/Windows** (confirmed by Madison, 2026-06-21). That rules out a macOS-only library swap and is why nokhwa stays for the other two platforms.

### Out of scope (possible follow-ups, not this spec)

- **Upstream PR** migrating `nokhwa-bindings-macos` to `objc2-core-video` (would fix this for the whole ecosystem and eventually let us drop our macOS backend). Independent of this work; tracked separately.
- **Hardware idle frame-rate drop on Linux/Windows** (V4L2 / MediaFoundation). The idle hardware throttle in this spec is macOS-only; nokhwa stays a no-op there. This is a documented asymmetry, not a silent gap.

## Architecture

### The `FrameSource` seam (unchanged construction)

The worker already constructs its capture source through a `SourceFactory` — a `Send` `FnOnce` that builds a `Box<dyn FrameSource>` **on the worker thread**. This exists precisely so `!Send` camera backends (nokhwa's AVFoundation camera today, ours tomorrow) are built where they are used. The new backend reuses this seam verbatim; nothing about worker construction changes.

nokhwa's macOS backend already runs an `AVCaptureVideoDataOutput` delegate internally and `frame()` reads the latest delivered buffer. So the new backend replicates today's underlying push behavior — it does not add a new always-on capture cost.

### New component: `AvfFrameSource`

Lives in a new `crates/wc-core/src/input/providers/mediapipe/capture/avfoundation.rs`, gated `cfg(all(feature = "hand-tracking-mediapipe-camera", target_os = "macos"))`. Implements `FrameSource`.

**Capture setup** (on the worker thread, via the factory):

1. **Device resolution by index** via `AVCaptureDeviceDiscoverySession` (built-in wide-angle + external camera device types). An out-of-range index falls back to the default video device — parity with today's `NokhwaFrameSource::open(camera_index: u32)`.
2. `AVCaptureSession` with `sessionPreset = AVCaptureSessionPreset640x480` (the existing target resolution; the pipeline downscales to a small inference input regardless).
3. `AVCaptureDeviceInput(device)` added to the session.
4. `AVCaptureVideoDataOutput` with:
   - `videoSettings = { kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA }` — AVFoundation does the YUV→BGRA conversion in optimized system code, so we never decode MJPEG/YUYV on macOS (the entire `choose_camera_format`/`yuyv_to_rgb`/MJPEG branch is nokhwa-only).
   - `alwaysDiscardsLateVideoFrames = true` — newest-wins for free.
5. A serial `dispatch2` queue carries the sample-buffer delegate (`setSampleBufferDelegate:queue:`).
6. Cache `activeFormat`'s default min-frame-duration (full-rate) for restoring after idle; build the format label (e.g. `"640x480 BGRA @30"`); `startRunning`.

**Delegate** (objc2 `define_class!`, implements `AVCaptureVideoDataOutputSampleBufferDelegate`): on `captureOutput:didOutputSampleBuffer:fromConnection:`:
- `CMSampleBufferGetImageBuffer` → `CVImageBuffer`/`CVPixelBuffer`.
- `CVPixelBufferLockBaseAddress` (read-only) → `CVPixelBufferGetBaseAddress` / `GetBytesPerRow` / `GetWidth` / `GetHeight`.
- Copy the BGRA bytes + width/height/bytesPerRow into the shared `Arc<Mutex<LatestFrame>>` slot, reusing the slot's `Vec` capacity (`clear()` + `extend_from_slice`), and bump a `generation: u64` counter.
- `CVPixelBufferUnlockBaseAddress`.

This delegate runs ~30×/sec for the session lifetime — a hot path by AGENTS' definition — so it performs **no per-frame heap allocation after warmup** (capacity-reused buffer).

### Push→pull bridge: `Arc<Mutex<LatestFrame>>` single-slot

The delegate (dispatch queue thread) is the producer; `AvfFrameSource` (worker thread) is the consumer. The slot holds the latest BGRA bytes, dimensions, row stride, and a generation counter.

- `next_frame(out)`: lock; if `generation` advanced since last seen, swap the BGRA bytes into a worker-local scratch buffer and read dims, update last-seen generation, unlock; then repack BGRA→RGB into the caller's reused `Frame.rgb` (dropping alpha, swapping R/B, honoring `bytesPerRow` row-stride padding). Returns `Ok(true)`. If no new generation, returns `Ok(false)`.
- `discard_frame()`: lock; if `generation` advanced, advance last-seen generation, unlock; return `Ok(true)`/`Ok(false)`. **Skips the BGRA→RGB repack** — this is the thermal-throttle CPU win, the same contract `NokhwaFrameSource::discard_frame` provides today.
- `format_label()`: returns the cached format string.

**Why a `Mutex` is acceptable here:** the lock is held only for a memcpy, on the **worker thread**, not the audio or render hot path. AGENTS bans `Mutex` specifically on the *audio* thread. Considered and rejected alternatives: an `rtrb` 1-capacity ring (already a dep, but moving values forces a return-the-buffer handshake to stay alloc-free — more plumbing for a one-deep handoff) and a `triple_buffer` crate (clean, but a new dependency for what a ~20-line mutex slot solves).

### Idle hardware capture throttle

**New trait method on `FrameSource`**, default no-op:

```rust
/// Hint that the app entered (`true`) or left (`false`) the idle/screensaver
/// throttle. Backends that can lower the *hardware* capture rate do so here,
/// shedding sensor/ISP work beyond the worker's decode-skipping. Default no-op.
fn set_capture_throttle(&mut self, _throttled: bool) {}
```

- **`AvfFrameSource`**: `lockForConfiguration` on the device, set `activeVideoMinFrameDuration = CMTime(1, IDLE_INFERENCE_HZ)` when throttled (caps the camera at 4 Hz), restore the cached full-rate duration when not, `unlockForConfiguration`. Device configuration from the worker thread while the session runs on its own queue is permitted as long as we lock/unlock and do not hold the lock long.
- **`MockFrameSource`**: no-op in production, but extended with a recorded call-log for the worker edge-trigger test (below).
- **`NokhwaFrameSource`** (Linux/Windows): no-op for now — documented follow-up.

**Worker change**: the run loop already re-reads the shared `MediaPipeLiveTuning` idle-throttle flag every iteration. We add **edge-triggered** dispatch: track the previous throttle state and, only when it flips, call `source.set_capture_throttle(now_throttled)` exactly once — no per-iteration device reconfiguration. The idle camera rate reuses `IDLE_INFERENCE_HZ` (4 Hz); there is no point capturing faster than we process while idle.

**Wake-latency contract is preserved.** Capturing at 4 Hz when idle means the freshest frame is ≤250 ms old when processed — the exact staleness bound the 4 Hz *processing* throttle already imposes. The `IDLE_INFERENCE_HZ` doc comment is updated to state that the camera and inference rates now match while idle, so there is no added wake latency; the camera simply does less work. A minor cost to note: changing `activeVideoMinFrameDuration` may cause a brief exposure/AWB re-adjust on the idle↔active transition.

## Dependencies & feature gating

Follows the **proven `thermal-sensor-macos` / `macmon` pattern** already in this repo: a target-gated optional dependency referenced from a feature compiles cleanly under CI's `--all-features` (on non-matching targets, `dep:` resolves to a no-op).

`crates/wc-core/Cargo.toml`:

```toml
# nokhwa moves off macOS (keeps V4L2 on Linux, MediaFoundation on Windows):
[target.'cfg(all(not(target_arch = "wasm32"), not(target_os = "macos")))'.dependencies]
nokhwa = { workspace = true, optional = true }

# macOS capture stack (all optional; objc2 0.6.x — no new objc2 version):
[target.'cfg(target_os = "macos")'.dependencies]
objc2-av-foundation = { version = "0.3", optional = true, features = [/* AVCaptureSession, device, data output, delegate, discovery session, presets */] }
objc2-core-video    = { version = "0.3", optional = true }
objc2-core-media    = { version = "0.3", optional = true }
dispatch2           = { version = "0.3", optional = true }
# objc2 / objc2-foundation / objc2-core-foundation are already transitive; name
# the ones we use directly as direct deps.
```

Feature:

```toml
hand-tracking-mediapipe-camera = [
  "hand-tracking-mediapipe",
  "dep:nokhwa",               # no-op on macOS (target-absent)
  "dep:objc2-av-foundation", "dep:objc2-core-video",
  "dep:objc2-core-media", "dep:dispatch2",   # no-op on non-macOS
]
```

On macOS, `dep:nokhwa` resolves to nothing and the objc2 deps activate; on Linux/Windows it is the reverse. CI `--all-features` compiles the correct backend per OS (compiling the backend needs no camera; only running does).

**Build-cost reality:** `dispatch2` and most needed `objc2-*` framework crates are already in our lock (28 objc2 entries today). Incremental additions are ~7 thin auto-generated binding crates (`objc2-av-foundation`, `objc2-core-media`, `objc2-core-video`, and their pulls `objc2-avf-audio`, `objc2-image-io`, `objc2-media-toolbox`, `objc2-open-gl`), all on the `objc2 0.6.4` we already compile. macOS simultaneously **sheds** `core-video-sys`, `metal 0.18`, `cocoa`, `cocoa-foundation`, `core-graphics 0.19`, `objc 0.2.7`, `objc_exception`, `block 0.1`, and `nokhwa` + `nokhwa-core` + `nokhwa-bindings-macos`. Net: roughly neutral on crate count, an unmaintained→maintained swap with no new objc2 version.

**`deny.toml`:** no change needed. New crates are MIT/Apache (objc2 is MIT), crates.io-sourced (allowed registry). The existing `paste` / `RUSTSEC-2024-0436` ignore is unrelated (it is wgpu's *newer* `metal`, not the 0.18 we drop).

## File organization

`capture.rs` is currently 565 lines — over the ~300-line AGENTS guideline — and mixes the portable trait/types/mock with the platform-specific nokhwa backend. Splitting it is a targeted improvement done as part of this work:

- **`capture/mod.rs`** — portable: `Frame`, `CaptureError`, the `FrameSource` trait (with the new `set_capture_throttle`), `MockFrameSource`, and module-level docs. No platform `cfg` in the portable parts.
- **`capture/nokhwa.rs`** — `NokhwaFrameSource`, `choose_camera_format`, `yuyv_to_rgb`, and their tests. Gated `cfg(all(feature = "hand-tracking-mediapipe-camera", not(target_os = "macos")))`.
- **`capture/avfoundation.rs`** — `AvfFrameSource`, the `define_class!` delegate, the `LatestFrame` slot, BGRA→RGB, format-label formatting, and their tests. Gated `cfg(all(feature = "hand-tracking-mediapipe-camera", target_os = "macos"))`.

Aligns with AGENTS ("one concept per file", platform-specific code separated, portable modules free of `cfg` blocks).

## build.rs cleanup

Delete `relink_objc_exception_for_dynamic_linking` and its call from `crates/waveconductor/build.rs`. It exists **only** for nokhwa's `objc 0.2` / `objc_exception` on macOS; once nokhwa does not build on macOS, `libexception.a` is never produced and the function is dead code. Removing it eliminates the ~60-line `-force_load` hack and its module doc. The Windows LeapC.dll copy and the `cargo:rerun-if-changed` line stay.

## Testing & verification

Three layers, all approved:

1. **CI unit tests (no camera, run everywhere):**
   - BGRA→RGB repack correctness, **including `bytesPerRow` row-stride padding** (a pixel buffer whose stride exceeds `width*4`).
   - `format_label` string formatting.
   - Pure camera-index→device-selection logic (kept as a pure function over a device list where possible).
   - **Worker edge-trigger test:** `MockFrameSource` gains a recorded `set_capture_throttle` call-log; toggling the idle-throttle flag across worker iterations must produce exactly the expected edge-triggered transitions (no duplicate calls while the state is steady).
2. **`#[ignore]` local smoke test (macOS, real camera):** open the default device, start the session, assert ≥1 frame arrives within a timeout; assert `set_capture_throttle(true)` lowers the delivered frame rate. Stays out of CI (no camera there); runnable via `cargo test -- --ignored` on Madison's Mac.
3. **Manual + soak:** `cargo rund --features hand-tracking-mediapipe-camera`, wave hands, confirm tracking + the dev-panel format label + the idle camera-rate drop; the pre-release **8-hour soak** covers the steady-state thermal path.

CI matrix already runs `--all-features` on macOS and Linux, so each compiles its backend. `cargo clippy --all-targets --all-features --workspace -- -D warnings` must stay green over the objc2 `unsafe` code (objc2 0.6 minimizes the unsafe surface; we annotate where required).

## Risks & open questions

- **Camera permission (TCC).** AVFoundation capture triggers the macOS camera-permission prompt and needs `NSCameraUsageDescription`. nokhwa already hits this today (it *is* AVFoundation underneath), so there is **no new permission surface** — but whatever satisfies it today (an Info.plist in the `.app` bundle, or terminal-attributed permission under `cargo rund`) must carry over. Flagged to verify on first run; capture the finding in the implementation.
- **`unsafe` surface.** objc2 message sends and the `define_class!` delegate are `unsafe`. Contained to `avfoundation.rs`, documented per AGENTS' contract-comment rules (each `unsafe` block states the invariant it relies on).
- **Rate-switch hiccup.** Changing `activeVideoMinFrameDuration` on idle↔active may cause a brief exposure re-adjust. Acceptable for an idle transition; noted.
- **Throttle asymmetry.** The hardware-rate drop is macOS-only for now; Linux/Windows nokhwa stays a no-op. Conscious, documented follow-up — not a silent gap.

## Success criteria

- `cargo tree -i core-video-sys` returns nothing on macOS with `--all-features`; `objc 0.2.7`, `metal 0.18`, `cocoa 0.20`, and `objc_exception` are gone from the macOS build.
- nokhwa still builds and provides capture on Linux and Windows.
- macOS hand-tracking works at least as well as today via `cargo rund` (manual smoke), and the dev panel shows the negotiated format.
- The camera's hardware capture rate measurably drops while idle/screensaver and restores on wake.
- All CI gates pass on macOS and Linux: `fmt`, `clippy -D warnings`, `nextest`, `cargo test --doc`, `cargo doc`, `cargo deny`, `cargo xtask check-secrets`.
- The `build.rs` `objc_exception` hack is removed.
- 8-hour soak passes before any release tag.
