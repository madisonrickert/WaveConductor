# macOS AVFoundation Webcam Capture Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace nokhwa's macOS webcam backend with an in-house `objc2` AVFoundation `FrameSource`, dropping the abandoned `core-video-sys`/`objc 0.2` chain on macOS while keeping nokhwa for Linux/Windows, and add an idle hardware capture-rate throttle.

**Architecture:** A new `AvfFrameSource` implements the existing `FrameSource` trait and is built on the worker thread via the existing `SourceFactory` seam. AVFoundation pushes frames to a sample-buffer delegate on a dispatch queue; the delegate fills an `Arc<Mutex<LatestFrame>>` single-slot that `next_frame`/`discard_frame` drain (newest-wins, alloc-free in steady state). nokhwa is gated to non-macOS targets; macOS uses the objc2 backend. A new `FrameSource::set_capture_throttle` hook lets the worker lower the camera's hardware frame rate while idle.

**Tech Stack:** Rust, Bevy (host app), `objc2` 0.6 + `objc2-av-foundation`/`objc2-core-video`/`objc2-core-media`/`dispatch2` (macOS), `nokhwa` 0.10 (Linux/Windows), `rtrb` (worker ring), `thiserror`.

**Spec:** `docs/superpowers/specs/2026-06-21-macos-avfoundation-capture-design.md`

## Global Constraints

- **Dev run:** `cargo rund` (dynamic-linked debug alias); never launch the bare `target/` binary. `cargo run -p waveconductor` is the static fallback.
- **CI gates (all must pass on macOS and Linux):** `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features --workspace -- -D warnings`; `cargo nextest run --workspace --all-features`; `cargo test --doc --workspace`; `cargo doc --no-deps --workspace --document-private-items`; `cargo deny check`; `cargo xtask check-secrets`.
- **`--all-features` must compile on every OS:** target-gated optional deps referenced from a feature follow the existing `thermal-sensor-macos = ["dep:macmon"]` pattern (a `dep:` for an absent target is a no-op).
- **Never** add `bevy/dynamic_linking` to a manifest `[features]` table (alias-only).
- **No `unwrap()`/`expect()`** in non-test code unless the panic is a documented invariant violation.
- **No `as` casts** on numeric types where `From`/`TryFrom`/`u32::try_from` work; where a float→int clamp forces it, follow the existing `#[allow(clippy::as_conversions, ...)]` + `reason` pattern in `capture/nokhwa.rs`.
- **No allocation in hot paths:** the delegate callback (~30 Hz for the session lifetime) and the worker loop. Reuse buffers with `clear()` + `extend_from_slice`; pre-size on the struct.
- **Audio-thread Mutex ban does not apply here:** the `LatestFrame` mutex is on the worker/dispatch threads, not the audio callback. Hold it only across a memcpy.
- **Docs:** `///` on every public item, `//!` on each module root, inline `//` stating the invariant each `unsafe` block relies on.
- **File size:** keep files under ~300 lines, one concept per file.
- **objc2:** stay on `objc2` 0.6.x (we already build 0.6.4) — no new objc2 major. New crates are MIT/Apache (already allowed in `deny.toml`); no `deny.toml` change.
- **Constants:** idle camera/inference rate is `IDLE_INFERENCE_HZ = 4` (in `worker.rs`).
- **Library docs:** per the project `ctx7` rule, fetch current `objc2` / `objc2-av-foundation` / `objc2-core-video` docs (docs.rs or `npx ctx7@latest`) before writing FFI calls — do not extrapolate objc2 0.6 signatures from memory.

---

### Task 1: Split `capture.rs` into a `capture/` module (pure refactor)

Behavior-preserving move so platform backends get their own files (current `capture.rs` is 565 lines, mixes portable + nokhwa). No gating changes yet — nokhwa stays compiled on all targets here.

**Files:**
- Delete: `crates/wc-core/src/input/providers/mediapipe/capture.rs`
- Create: `crates/wc-core/src/input/providers/mediapipe/capture/mod.rs` (portable: `Frame`, `CaptureError`, `FrameSource`, `MockFrameSource` + their tests)
- Create: `crates/wc-core/src/input/providers/mediapipe/capture/nokhwa.rs` (`NokhwaFrameSource`, `choose_camera_format`, `yuyv_to_rgb` + their tests)
- Unchanged: `mediapipe/mod.rs` keeps its existing `mod capture;` line (Rust resolves `capture/mod.rs` automatically)

**Interfaces:**
- Consumes: nothing new.
- Produces: same public paths as today — `capture::Frame`, `capture::CaptureError`, `capture::FrameSource`, `capture::MockFrameSource`, `capture::NokhwaFrameSource`. The split must keep every existing `use super::capture::{...}` import resolving unchanged.

- [ ] **Step 1: Create `capture/mod.rs` with the portable half**

Move verbatim from `capture.rs` into `capture/mod.rs`: the module `//!` doc and `#![allow(dead_code)]`, `CaptureError`, `Frame` (+ `expected_len`/`is_consistent`/`fit_to`), the `FrameSource` trait, `MockFrameSource` (+ `new`/`looping`/`solid` and its `FrameSource` impl), and the portable tests (`frame_fit_to_sizes_buffer`, `solid_source_yields_one_frame_then_stops`, `looping_source_repeats_the_last_frame`, `buffer_is_reused_across_frames`, `discard_frame_consumes_the_sequence_like_next_frame`, `mock_source_has_no_format_label`). Add at the bottom of the types section:

```rust
/// Production webcam backend, selected per platform.
#[cfg(all(feature = "hand-tracking-mediapipe-camera", not(target_os = "macos")))]
mod nokhwa;
#[cfg(all(feature = "hand-tracking-mediapipe-camera", not(target_os = "macos")))]
pub use nokhwa::NokhwaFrameSource;
```

Note: this already platform-gates `nokhwa.rs` to non-macOS. That is intentional and safe **only because** Step 4 below temporarily keeps the gate matching today. See Step 4.

- [ ] **Step 2: Create `capture/nokhwa.rs` with the nokhwa half**

Move verbatim from `capture.rs`: `NokhwaFrameSource` (+ `MAX_CAPTURE_*`/`MIN_CAPTURE_*`/`TARGET_AREA` consts, `choose_camera_format`, `open`, the `FrameSource` impl, `yuyv_to_rgb`) and the `camera_format_tests` module. Add a module `//!` doc and `use super::{CaptureError, Frame, FrameSource};` plus `use crate::input::providers::mediapipe::capture::Frame;` as needed (use the `super::` path). Keep the existing `#[allow(...)]` attributes on `yuyv_to_rgb`'s clamp.

- [ ] **Step 3: Delete the old file**

```bash
git rm crates/wc-core/src/input/providers/mediapipe/capture.rs
```

- [ ] **Step 4: Temporarily relax the nokhwa gate to all targets**

In `capture/mod.rs` from Step 1, the nokhwa gate is `not(target_os = "macos")`, but the Cargo dependency still pulls nokhwa on macOS until Task 7. To keep Task 1 a pure refactor that compiles on macOS unchanged, set the gate to feature-only for now:

```rust
#[cfg(feature = "hand-tracking-mediapipe-camera")]
mod nokhwa;
#[cfg(feature = "hand-tracking-mediapipe-camera")]
pub use nokhwa::NokhwaFrameSource;
```

Task 7 restores the `not(target_os = "macos")` gate at the same time it moves the Cargo dependency. Leave a `// TODO(Task 7): gate to not(target_os = "macos") when the macOS backend lands` comment on both lines.

- [ ] **Step 5: Verify the refactor compiles and all existing tests pass**

Run:
```bash
cargo fmt --all
cargo nextest run -p wc-core --features hand-tracking-mediapipe-camera
cargo nextest run -p wc-core
cargo clippy -p wc-core --all-targets --features hand-tracking-mediapipe-camera -- -D warnings
```
Expected: builds clean; every previously-passing capture test (`frame_fit_to_sizes_buffer`, the mock-source tests, the `camera_format_tests`) still passes. No behavior change.

- [ ] **Step 6: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/capture
git commit -m "refactor(capture): split capture.rs into capture/ module"
```

---

### Task 2: Add `FrameSource::set_capture_throttle` + worker edge-trigger dispatch

The worker calls `set_capture_throttle(throttled)` only when the idle-throttle flag flips. Default impl is a no-op (mock + nokhwa); the macOS backend overrides it in Task 6.

**Files:**
- Modify: `crates/wc-core/src/input/providers/mediapipe/capture/mod.rs` (add trait method)
- Modify: `crates/wc-core/src/input/providers/mediapipe/worker.rs` (edge-trigger in `run_worker_loop`; recording test source; `IDLE_INFERENCE_HZ` doc note)

**Interfaces:**
- Consumes: `FrameSource` (Task 1), `MediaPipeLiveTuning::idle_throttle()` (existing, returns `bool`), `run_worker_loop` (existing).
- Produces: `FrameSource::set_capture_throttle(&mut self, throttled: bool)` (default no-op). Task 6's `AvfFrameSource` overrides it.

- [ ] **Step 1: Add the trait method (default no-op) to `capture/mod.rs`**

In the `FrameSource` trait, after `format_label`:

```rust
    /// Hint that the app entered (`true`) or left (`false`) the idle/screensaver
    /// throttle. Backends that can lower the *hardware* capture rate do so here,
    /// shedding sensor/ISP work beyond the worker's decode-skipping. Called by
    /// the worker only on transitions (edge-triggered), never per frame.
    ///
    /// Default: no-op. Implemented by [`AvfFrameSource`] on macOS; a documented
    /// follow-up for the nokhwa V4L2/MediaFoundation backends.
    fn set_capture_throttle(&mut self, _throttled: bool) {}
```

- [ ] **Step 2: Add a recording test source + write the failing edge-trigger test**

In `worker.rs`'s `#[cfg(test)] mod tests`, add a source that serves looping solid frames and records every `set_capture_throttle` argument into a shared log:

```rust
/// Test source that serves looping solid frames and records throttle toggles.
struct ThrottleRecordingSource {
    inner: MockFrameSource,
    log: Arc<std::sync::Mutex<Vec<bool>>>,
}

impl FrameSource for ThrottleRecordingSource {
    fn next_frame(&mut self, out: &mut Frame) -> Result<bool, CaptureError> {
        self.inner.next_frame(out)
    }
    fn discard_frame(&mut self) -> Result<bool, CaptureError> {
        self.inner.discard_frame()
    }
    fn set_capture_throttle(&mut self, throttled: bool) {
        self.log.lock().expect("throttle log poisoned").push(throttled);
    }
}

fn throttle_recording_source(log: Arc<std::sync::Mutex<Vec<bool>>>) -> SourceFactory {
    Box::new(move || {
        let mut f = Frame::default();
        f.fit_to(64, 48);
        let src: Box<dyn FrameSource> =
            Box::new(ThrottleRecordingSource { inner: MockFrameSource::looping(vec![f]), log });
        Ok(src)
    })
}

#[test]
fn worker_edge_triggers_capture_throttle_on_idle_change() {
    let log = Arc::new(std::sync::Mutex::new(Vec::<bool>::new()));
    let cell = tuning(false);
    let (producer, _consumer) = rtrb::RingBuffer::<WorkerMsg>::new(64);
    let mut handle = spawn_worker(
        throttle_recording_source(Arc::clone(&log)),
        empty_pipeline(),
        30,
        Arc::clone(&cell),
        producer,
    );

    // Wait for the initial sync call (false), then flip to idle, then back.
    let wait_len = |n: usize| {
        for _ in 0..200 {
            if log.lock().unwrap().len() >= n {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    };
    assert!(wait_len(1), "no initial throttle sync");
    cell.set_idle_throttle(true);
    assert!(wait_len(2), "idle transition not dispatched");
    cell.set_idle_throttle(false);
    assert!(wait_len(3), "active transition not dispatched");
    handle.stop();

    assert_eq!(*log.lock().unwrap(), vec![false, true, false]);
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo nextest run -p wc-core worker_edge_triggers_capture_throttle_on_idle_change`
Expected: FAIL — the log is empty (worker never calls `set_capture_throttle`), so `wait_len(1)` returns false.

- [ ] **Step 4: Implement the edge-trigger in `run_worker_loop`**

In `worker.rs`, in `run_worker_loop` (around the existing `let mut dropped_frames = 0_u64;` initializers, before the `while` loop), add:

```rust
    // Edge-triggered hardware-throttle dispatch: tell the source when the idle
    // flag flips so a capable backend (macOS AVFoundation) drops its hardware
    // capture rate. `None` forces a sync call on the first iteration.
    let mut last_throttle: Option<bool> = None;
```

Inside the loop, immediately after `let idle_throttled = tuning.idle_throttle();`:

```rust
        if last_throttle != Some(idle_throttled) {
            source.set_capture_throttle(idle_throttled);
            last_throttle = Some(idle_throttled);
        }
```

- [ ] **Step 5: Update the `IDLE_INFERENCE_HZ` doc to note the camera-rate match**

Append to the `IDLE_INFERENCE_HZ` doc comment in `worker.rs`:

```rust
/// On backends that honor [`FrameSource::set_capture_throttle`] (macOS
/// AVFoundation), the *camera* drops to this same rate while idle, so the
/// freshest frame is at most one period (250 ms) old when processed — the
/// identical staleness bound the inference cap already imposes. No added wake
/// latency; the sensor/ISP simply do less work.
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo nextest run -p wc-core worker_edge_triggers_capture_throttle_on_idle_change`
Expected: PASS — log is `[false, true, false]`.

- [ ] **Step 7: Run the broader worker + capture suite and lints**

Run:
```bash
cargo nextest run -p wc-core --features hand-tracking-mediapipe-camera
cargo clippy -p wc-core --all-targets --features hand-tracking-mediapipe-camera -- -D warnings
```
Expected: all pass; no regression in the existing throttle/rate tests.

- [ ] **Step 8: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/capture/mod.rs crates/wc-core/src/input/providers/mediapipe/worker.rs
git commit -m "feat(capture): edge-trigger set_capture_throttle on idle transitions"
```

---

### Task 3: Create `capture/avfoundation.rs` scaffold + `bgra_to_rgb` pure helper

The macOS pixel repack: BGRA (with row-stride padding) to tightly-packed RGB8. Pure, fully unit-testable without a camera.

**Files:**
- Create: `crates/wc-core/src/input/providers/mediapipe/capture/avfoundation.rs`
- Modify: `crates/wc-core/src/input/providers/mediapipe/capture/mod.rs` (declare the module, macOS-gated)

**Interfaces:**
- Consumes: nothing.
- Produces: `fn bgra_to_rgb(bgra: &[u8], bytes_per_row: usize, width: u32, height: u32, out: &mut Vec<u8>)` — writes `width*height*3` RGB bytes into `out` (resized in place). Used by Task 4.

- [ ] **Step 1: Declare the macOS-gated module in `capture/mod.rs`**

```rust
#[cfg(all(feature = "hand-tracking-mediapipe-camera", target_os = "macos"))]
mod avfoundation;
#[cfg(all(feature = "hand-tracking-mediapipe-camera", target_os = "macos"))]
pub use avfoundation::AvfFrameSource;
```

(`AvfFrameSource` does not exist until Task 6; add only the `mod avfoundation;` line now and the `pub use` line commented with `// TODO(Task 6)`, or add the `pub use` in Task 6. To keep the module compiling, add the `pub use` line in Task 6’s Step where `AvfFrameSource` is defined.)

For Task 3, add only:
```rust
#[cfg(all(feature = "hand-tracking-mediapipe-camera", target_os = "macos"))]
mod avfoundation;
```

- [ ] **Step 2: Create `avfoundation.rs` with the module doc and the failing test**

```rust
//! macOS webcam capture via AVFoundation on the maintained `objc2` framework
//! crates. Replaces nokhwa's `core-video-sys`/`objc 0.2` backend on macOS while
//! nokhwa keeps Linux/Windows. Frames arrive on a dispatch-queue delegate and
//! are drained by the worker through a single-slot [`LatestFrame`].
#![allow(dead_code)] // backend wired into `open_camera_source` in Task 7.

/// Repack camera BGRA (byte order B,G,R,A, possibly row-padded so
/// `bytes_per_row >= width*4`) into tightly-packed RGB8 in `out`.
///
/// `out` is resized to `width*height*3` and reused across frames (the worker
/// owns it). Only the first `width*4` bytes of each row are pixel data; the
/// remainder up to `bytes_per_row` is stride padding and is skipped.
pub(super) fn bgra_to_rgb(
    bgra: &[u8],
    bytes_per_row: usize,
    width: u32,
    height: u32,
    out: &mut Vec<u8>,
) {
    todo!("implemented in Step 4")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repacks_bgra_dropping_alpha_and_swapping_channels() {
        // 2x1 image, no padding. Pixel0 = B,G,R,A = 10,20,30,255 -> RGB 30,20,10.
        // Pixel1 = 40,50,60,128 -> RGB 60,50,40.
        let bgra = [10u8, 20, 30, 255, 40, 50, 60, 128];
        let mut out = Vec::new();
        bgra_to_rgb(&bgra, 8, 2, 1, &mut out);
        assert_eq!(out, vec![30, 20, 10, 60, 50, 40]);
    }

    #[test]
    fn skips_row_stride_padding() {
        // 1x2 image, bytes_per_row = 8 but width*4 = 4 (4 padding bytes/row).
        // Row0 px = 1,2,3,255 -> 3,2,1 ; padding 99,99,99,99 ignored.
        // Row1 px = 4,5,6,255 -> 6,5,4.
        let bgra = [1u8, 2, 3, 255, 99, 99, 99, 99, 4, 5, 6, 255, 88, 88, 88, 88];
        let mut out = Vec::new();
        bgra_to_rgb(&bgra, 8, 1, 2, &mut out);
        assert_eq!(out, vec![3, 2, 1, 6, 5, 4]);
    }

    #[test]
    fn reuses_buffer_capacity() {
        let bgra = [10u8, 20, 30, 255];
        let mut out = Vec::with_capacity(3);
        bgra_to_rgb(&bgra, 4, 1, 1, &mut out);
        let ptr = out.as_ptr();
        bgra_to_rgb(&bgra, 4, 1, 1, &mut out);
        assert_eq!(out.as_ptr(), ptr, "same dimensions must not reallocate");
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo nextest run -p wc-core --features hand-tracking-mediapipe-camera repacks_bgra_dropping_alpha`
Expected: FAIL — `todo!` panics.

- [ ] **Step 4: Implement `bgra_to_rgb`**

```rust
pub(super) fn bgra_to_rgb(
    bgra: &[u8],
    bytes_per_row: usize,
    width: u32,
    height: u32,
    out: &mut Vec<u8>,
) {
    let w = width as usize;
    let h = height as usize;
    out.clear();
    out.resize(w * h * 3, 0);
    for row in 0..h {
        let row_start = row * bytes_per_row;
        let src_row = &bgra[row_start..row_start + w * 4];
        let dst_row = &mut out[row * w * 3..(row + 1) * w * 3];
        for (px, rgb) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(3)) {
            rgb[0] = px[2]; // R
            rgb[1] = px[1]; // G
            rgb[2] = px[0]; // B
        }
    }
}
```

Note `width as usize` / `height as usize` are widening casts; if clippy flags `as_conversions`, switch to `usize::try_from(width).unwrap_or(0)` to match the codebase's no-`as` rule.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo nextest run -p wc-core --features hand-tracking-mediapipe-camera bgra`
Expected: PASS (all three tests).

- [ ] **Step 6: Lints + commit**

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --features hand-tracking-mediapipe-camera -- -D warnings
git add crates/wc-core/src/input/providers/mediapipe/capture
git commit -m "feat(capture): BGRA->RGB repack helper for macOS capture"
```

---

### Task 4: `LatestFrame` single-slot + drain logic

The shared producer→consumer slot the delegate fills and `next_frame`/`discard_frame` drain. Generation-counter newest-wins, no per-frame alloc. Pure-testable by storing into the slot directly (no camera).

**Files:**
- Modify: `crates/wc-core/src/input/providers/mediapipe/capture/avfoundation.rs`

**Interfaces:**
- Consumes: `bgra_to_rgb` (Task 3), `Frame` (`super::Frame`).
- Produces:
  - `struct LatestFrame { bgra: Vec<u8>, width: u32, height: u32, bytes_per_row: usize, generation: u64 }`
  - `LatestFrame::store(&mut self, bgra: &[u8], width: u32, height: u32, bytes_per_row: usize)` — capacity-reusing copy, `generation += 1`.
  - `LatestFrame::take_into(&self, last_gen: &mut u64, out: &mut Frame) -> bool` — if newer, repack into `out`, advance `*last_gen`, return `true`.
  - `LatestFrame::consume(&self, last_gen: &mut u64) -> bool` — if newer, advance `*last_gen`, return `true` (no repack).
  Used by Task 6's `AvfFrameSource` behind an `Arc<Mutex<LatestFrame>>`.

- [ ] **Step 1: Write failing tests for store/take_into/consume**

Add to `avfoundation.rs`'s `tests` module:

```rust
    use super::super::Frame;

    #[test]
    fn store_then_take_into_produces_rgb_once() {
        let mut slot = LatestFrame::default();
        slot.store(&[10, 20, 30, 255], 1, 1, 4);
        let mut last = 0u64;
        let mut out = Frame::default();
        assert!(slot.take_into(&mut last, &mut out), "first take sees new frame");
        assert_eq!(out.width, 1);
        assert_eq!(out.rgb, vec![30, 20, 10]);
        assert!(!slot.take_into(&mut last, &mut out), "no new frame since");
    }

    #[test]
    fn consume_advances_without_repacking() {
        let mut slot = LatestFrame::default();
        slot.store(&[1, 2, 3, 255], 1, 1, 4);
        let mut last = 0u64;
        assert!(slot.consume(&mut last), "consume sees the stored frame");
        let mut out = Frame::default();
        assert!(!slot.take_into(&mut last, &mut out), "consume already advanced the generation");
    }

    #[test]
    fn store_reuses_capacity() {
        let mut slot = LatestFrame::default();
        slot.store(&[1, 2, 3, 255], 1, 1, 4);
        let ptr = slot.bgra.as_ptr();
        slot.store(&[4, 5, 6, 255], 1, 1, 4);
        assert_eq!(slot.bgra.as_ptr(), ptr, "same size must not reallocate");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p wc-core --features hand-tracking-mediapipe-camera store_then_take_into`
Expected: FAIL — `LatestFrame` undefined.

- [ ] **Step 3: Implement `LatestFrame`**

```rust
use super::Frame;

/// Single-slot latest-frame handoff: the AVFoundation delegate `store`s the
/// newest BGRA frame; the worker drains it via `take_into`/`consume`. Behind an
/// `Arc<Mutex<_>>` shared between the dispatch queue and the worker thread.
#[derive(Default)]
pub(super) struct LatestFrame {
    bgra: Vec<u8>,
    width: u32,
    height: u32,
    bytes_per_row: usize,
    /// Monotonic counter; a reader advances its own `last_gen` to this.
    generation: u64,
}

impl LatestFrame {
    /// Copy the newest BGRA frame in, reusing capacity (no realloc at steady
    /// size). Runs on the delegate's dispatch queue — a hot path; alloc-free.
    pub(super) fn store(&mut self, bgra: &[u8], width: u32, height: u32, bytes_per_row: usize) {
        self.bgra.clear();
        self.bgra.extend_from_slice(bgra);
        self.width = width;
        self.height = height;
        self.bytes_per_row = bytes_per_row;
        self.generation = self.generation.wrapping_add(1);
    }

    /// If a frame newer than `*last_gen` is present, repack it into `out`,
    /// advance `*last_gen`, and return `true`. Else return `false`.
    pub(super) fn take_into(&self, last_gen: &mut u64, out: &mut Frame) -> bool {
        if self.generation == *last_gen {
            return false;
        }
        out.fit_to(self.width, self.height);
        bgra_to_rgb(&self.bgra, self.bytes_per_row, self.width, self.height, &mut out.rgb);
        *last_gen = self.generation;
        true
    }

    /// Like `take_into` but skips the repack — the worker's over-budget drain.
    pub(super) fn consume(&self, last_gen: &mut u64) -> bool {
        if self.generation == *last_gen {
            return false;
        }
        *last_gen = self.generation;
        true
    }
}
```

`Frame::fit_to` sets `out.width`/`out.height` and resizes `out.rgb`; `bgra_to_rgb` then refills it (the double resize is benign — `fit_to` sizes it, `bgra_to_rgb`'s `clear()`+`resize` keeps the same capacity). If a clippy lint objects to the redundant resize, drop the `fit_to` call and set `out.width`/`out.height` explicitly before `bgra_to_rgb`.

- [ ] **Step 4: Run to verify the tests pass**

Run: `cargo nextest run -p wc-core --features hand-tracking-mediapipe-camera --no-fail-fast LatestFrame store_then_take consume_advances store_reuses`
Expected: PASS.

- [ ] **Step 5: Lints + commit**

```bash
cargo clippy -p wc-core --all-targets --features hand-tracking-mediapipe-camera -- -D warnings
git add crates/wc-core/src/input/providers/mediapipe/capture/avfoundation.rs
git commit -m "feat(capture): LatestFrame single-slot handoff for macOS capture"
```

---

### Task 5: Pure helpers — `select_device_index` + `format_label`

Device-index selection (parity with nokhwa's `open(camera_index)` fallback) and the diagnostics format string. Both pure.

**Files:**
- Modify: `crates/wc-core/src/input/providers/mediapipe/capture/avfoundation.rs`

**Interfaces:**
- Produces:
  - `fn select_device_index(device_count: usize, requested: u32) -> Option<usize>` — `Some(requested)` if in range, else `None` (caller uses the system default device).
  - `fn format_label(width: u32, height: u32, fps: u32) -> String` — e.g. `"640x480 BGRA @30"`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn device_index_in_range_is_selected() {
        assert_eq!(select_device_index(3, 0), Some(0));
        assert_eq!(select_device_index(3, 2), Some(2));
    }

    #[test]
    fn out_of_range_index_falls_back_to_default() {
        assert_eq!(select_device_index(3, 3), None);
        assert_eq!(select_device_index(0, 0), None);
    }

    #[test]
    fn format_label_reads_like_the_nokhwa_label() {
        assert_eq!(format_label(640, 480, 30), "640x480 BGRA @30");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p wc-core --features hand-tracking-mediapipe-camera device_index format_label`
Expected: FAIL — undefined functions.

- [ ] **Step 3: Implement both helpers**

```rust
/// Choose which enumerated capture device to open. Returns `Some(index)` when
/// `requested` is in range, or `None` to fall back to the system default video
/// device — parity with nokhwa's `open(camera_index)` graceful fallback.
pub(super) fn select_device_index(device_count: usize, requested: u32) -> Option<usize> {
    let idx = usize::try_from(requested).ok()?;
    (idx < device_count).then_some(idx)
}

/// Human-readable label for the negotiated capture format (dev-panel diagnostics).
pub(super) fn format_label(width: u32, height: u32, fps: u32) -> String {
    format!("{width}x{height} BGRA @{fps}")
}
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `cargo nextest run -p wc-core --features hand-tracking-mediapipe-camera device_index format_label`
Expected: PASS.

- [ ] **Step 5: Lints + commit**

```bash
cargo clippy -p wc-core --all-targets --features hand-tracking-mediapipe-camera -- -D warnings
git add crates/wc-core/src/input/providers/mediapipe/capture/avfoundation.rs
git commit -m "feat(capture): device-index selection and format label for macOS capture"
```

---

### Task 6: AVFoundation session, delegate, and `AvfFrameSource` (objc2 I/O glue)

The unsafe FFI core: add the objc2 deps, build the capture session + sample-buffer delegate, implement `FrameSource` over the `LatestFrame` slot, and the hardware throttle. No camera in CI, so this task's gates are compile + clippy + an `#[ignore]`d local smoke test + `cargo rund`.

**IMPORTANT:** Fetch current `objc2` 0.6, `objc2-av-foundation` 0.3, `objc2-core-video` 0.3, `objc2-core-media` 0.3, and `dispatch2` docs (docs.rs or `npx ctx7@latest docs <id> "AVCaptureVideoDataOutput sample buffer delegate define_class CVPixelBuffer lock base address"`) and verify every method name/signature below before relying on it — objc2 0.6 APIs must not be guessed from memory. The structure below is correct; the exact objc2 call spellings are the thing to confirm.

**Files:**
- Modify: `crates/wc-core/Cargo.toml` (add macOS objc2 deps; extend the `-camera` feature)
- Modify: `crates/wc-core/src/input/providers/mediapipe/capture/avfoundation.rs` (the backend)
- Modify: `crates/wc-core/src/input/providers/mediapipe/capture/mod.rs` (`pub use avfoundation::AvfFrameSource;`)

**Interfaces:**
- Consumes: `LatestFrame`/`bgra_to_rgb`/`select_device_index`/`format_label` (Tasks 3–5); `FrameSource`, `Frame`, `CaptureError` (Task 1); `set_capture_throttle` (Task 2); `IDLE_INFERENCE_HZ` (re-export or hardcode 4 with a `// = IDLE_INFERENCE_HZ` note — prefer importing it).
- Produces: `pub struct AvfFrameSource` with `pub fn open(camera_index: u32) -> Result<Self, CaptureError>` and `impl FrameSource for AvfFrameSource`. Consumed by Task 7's `open_camera_source`.

- [ ] **Step 1: Add the macOS dependencies and extend the feature**

In `crates/wc-core/Cargo.toml`, add a macOS target dependency block (or extend the existing one) — keep `nokhwa` where it is for now (Task 7 moves it):

```toml
[target.'cfg(target_os = "macos")'.dependencies]
# existing: macmon = { version = "=0.7.0", optional = true }
objc2 = { version = "0.6", optional = true }
objc2-foundation = { version = "0.3", optional = true }
objc2-av-foundation = { version = "0.3", optional = true, features = [
    # Confirm the exact feature names against docs.rs/objc2-av-foundation; you
    # need AVCaptureSession, AVCaptureDevice, AVCaptureDeviceInput,
    # AVCaptureVideoDataOutput + its sample-buffer delegate protocol,
    # AVCaptureDeviceDiscoverySession, AVCaptureConnection, and the session presets.
] }
objc2-core-video = { version = "0.3", optional = true }
objc2-core-media = { version = "0.3", optional = true }
dispatch2 = { version = "0.3", optional = true }
```

Extend the feature:

```toml
hand-tracking-mediapipe-camera = [
    "hand-tracking-mediapipe",
    "dep:nokhwa",
    "dep:objc2", "dep:objc2-foundation", "dep:objc2-av-foundation",
    "dep:objc2-core-video", "dep:objc2-core-media", "dep:dispatch2",
]
```

Run `cargo tree -p wc-core -i objc2 --features hand-tracking-mediapipe-camera` and confirm objc2 resolves to a single 0.6.x (no new major). Expected: `objc2 v0.6.4` (or later 0.6.x).

- [ ] **Step 2: Write the `#[ignore]`d local smoke test (real camera)**

Add to `avfoundation.rs`'s `tests` module — it stays out of CI (no camera) and is run manually with `cargo test -- --ignored`:

```rust
    #[test]
    #[ignore = "requires a real camera; run locally with --ignored on macOS"]
    fn opens_default_camera_and_delivers_a_frame() {
        let mut src = AvfFrameSource::open(0).expect("open default camera");
        let mut out = Frame::default();
        let mut got = false;
        for _ in 0..200 {
            if src.next_frame(&mut out).expect("frame read") {
                got = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(got, "no frame within ~2s");
        assert!(out.is_consistent() && out.width > 0);
        assert!(src.format_label().is_some());
    }
```

- [ ] **Step 3: Implement the delegate class**

Using objc2 `define_class!` (verify the 0.6 macro form), declare a class that conforms to `AVCaptureVideoDataOutputSampleBufferDelegate` and holds an `Arc<Mutex<LatestFrame>>` ivar. In `captureOutput:didOutputSampleBuffer:fromConnection:`:
1. `CMSampleBufferGetImageBuffer(sample_buffer)` → `CVImageBuffer`/`CVPixelBuffer` (null-check; return early if none).
2. `CVPixelBufferLockBaseAddress(pb, kCVPixelBufferLock_ReadOnly)`.
3. Read `CVPixelBufferGetBaseAddress`, `CVPixelBufferGetBytesPerRow`, `CVPixelBufferGetWidth`, `CVPixelBufferGetHeight`. Build a `&[u8]` of `bytes_per_row * height` via `std::slice::from_raw_parts` (document the safety invariant: pointer valid + length correct while locked).
4. Lock the `Arc<Mutex<LatestFrame>>`, call `store(bytes, width as u32, height as u32, bytes_per_row)`.
5. `CVPixelBufferUnlockBaseAddress(pb, kCVPixelBufferLock_ReadOnly)`.

Each `unsafe` block gets an inline `// SAFETY:` comment naming the invariant. Confirm pixel format is BGRA (set in Step 4); if a non-BGRA buffer ever arrives, skip it (the videoSettings request prevents this, but guard rather than mis-read).

- [ ] **Step 4: Implement `AvfFrameSource::open`**

Structure (verify each objc2 call):
1. Enumerate devices with `AVCaptureDeviceDiscoverySession` (built-in wide-angle + external device types, video media type). Map `select_device_index(devices.len(), camera_index)` to a device, else the default video device via `AVCaptureDevice::default(AVMediaTypeVideo)`. `CaptureError::NoCamera` if none.
2. Build `AVCaptureSession`; set `sessionPreset = AVCaptureSessionPreset640x480`.
3. `AVCaptureDeviceInput::fromDevice(device)`; `addInput` (check `canAddInput`).
4. `AVCaptureVideoDataOutput`; set `videoSettings` dict `{ kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA }`; `setAlwaysDiscardsLateVideoFrames(true)`; create a serial `dispatch2` queue; `setSampleBufferDelegate(delegate, queue)`; `addOutput` (check `canAddOutput`).
5. Cache the device's active-format default min frame duration (`activeFormat.videoSupportedFrameRateRanges` min duration) for restoring after idle.
6. Read `activeFormat` dimensions + the negotiated fps for `format_label(w, h, fps)`.
7. `startRunning`.
8. Store on the struct: `session`, `device`, the `Arc<Mutex<LatestFrame>>`, a `scratch`/`last_generation`, the cached full-rate `CMTime`, the format `String`, and the dispatch queue (keep it alive). The struct is `!Send` (held on the worker thread); only the `Arc<Mutex<LatestFrame>>` crosses to the delegate queue.

```rust
pub struct AvfFrameSource {
    // objc2 retained objects (exact types per objc2-av-foundation):
    // session: Retained<AVCaptureSession>,
    // device: Retained<AVCaptureDevice>,
    // _delegate: Retained<FrameDelegate>,
    // _queue: dispatch2::Queue,
    latest: std::sync::Arc<std::sync::Mutex<LatestFrame>>,
    last_generation: u64,
    full_rate_min_frame_duration: /* CMTime */,
    format: String,
}
```

- [ ] **Step 5: Implement `FrameSource` for `AvfFrameSource`**

```rust
impl FrameSource for AvfFrameSource {
    fn format_label(&self) -> Option<&str> {
        Some(&self.format)
    }

    fn next_frame(&mut self, out: &mut Frame) -> Result<bool, CaptureError> {
        let slot = self.latest.lock().map_err(|_| CaptureError::Read("frame slot poisoned".into()))?;
        Ok(slot.take_into(&mut self.last_generation, out))
    }

    fn discard_frame(&mut self) -> Result<bool, CaptureError> {
        let slot = self.latest.lock().map_err(|_| CaptureError::Read("frame slot poisoned".into()))?;
        Ok(slot.consume(&mut self.last_generation))
    }

    fn set_capture_throttle(&mut self, throttled: bool) {
        // Lock the device, set activeVideoMinFrameDuration to 1/IDLE_INFERENCE_HZ
        // when throttled (caps the hardware rate), restore the cached full-rate
        // duration otherwise. lockForConfiguration/unlockForConfiguration around
        // the change; a lock failure is non-fatal (log + skip) — never panic on
        // the worker thread. Verify CMTime construction + the AVCaptureDevice
        // setters against objc2-av-foundation/objc2-core-media docs.
        // Target idle duration: CMTime { value: 1, timescale: IDLE_INFERENCE_HZ }.
    }
}
```

Import `IDLE_INFERENCE_HZ` from the worker module (`use super::super::worker::IDLE_INFERENCE_HZ;`) so the camera rate provably matches the inference cap; if it is not `pub`, make it `pub(crate)` in `worker.rs` (note that one-line visibility change in this step's commit).

- [ ] **Step 6: Export `AvfFrameSource` from `capture/mod.rs`**

Add (next to the `mod avfoundation;` line from Task 3):
```rust
#[cfg(all(feature = "hand-tracking-mediapipe-camera", target_os = "macos"))]
pub use avfoundation::AvfFrameSource;
```

- [ ] **Step 7: Compile, lint, and run the local smoke test on macOS**

Run:
```bash
cargo build -p wc-core --features hand-tracking-mediapipe-camera
cargo clippy -p wc-core --all-targets --features hand-tracking-mediapipe-camera -- -D warnings
cargo test -p wc-core --features hand-tracking-mediapipe-camera -- --ignored opens_default_camera_and_delivers_a_frame
```
Expected: builds clean; clippy green over the `unsafe` blocks; the ignored test opens the camera and receives a frame. If the camera-permission prompt appears, grant it (see Task 7 / spec TCC note).

- [ ] **Step 8: Commit**

```bash
git add crates/wc-core/Cargo.toml crates/wc-core/src/input/providers/mediapipe/capture Cargo.lock
git commit -m "feat(capture): AVFoundation camera backend on objc2 (macOS)"
```

---

### Task 7: Cut over macOS to the new backend; remove nokhwa + the build.rs hack on macOS

Flip construction to `AvfFrameSource` on macOS, move `nokhwa` off macOS, restore the platform gate on `nokhwa.rs`, and delete the now-dead `objc_exception` build hack.

**Files:**
- Modify: `crates/wc-core/Cargo.toml` (move `nokhwa` to a non-macOS target table)
- Modify: `crates/wc-core/src/input/providers/mediapipe/capture/mod.rs` (restore `not(target_os = "macos")` gate on `nokhwa`)
- Modify: `crates/wc-core/src/input/providers/mediapipe/mod.rs:435` (`open_camera_source` platform branch)
- Modify: `crates/waveconductor/build.rs` (delete `relink_objc_exception_for_dynamic_linking` + call)

**Interfaces:**
- Consumes: `AvfFrameSource::open` (Task 6), `NokhwaFrameSource::open` (Task 1).
- Produces: `open_camera_source` returns the platform-correct backend.

- [ ] **Step 1: Move `nokhwa` off macOS in Cargo.toml**

Change the `nokhwa` dependency line from the `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` table to a non-macOS table:

```toml
[target.'cfg(all(not(target_arch = "wasm32"), not(target_os = "macos")))'.dependencies]
nokhwa = { workspace = true, optional = true }
```

Leave `"dep:nokhwa"` in the `hand-tracking-mediapipe-camera` feature list — on macOS it resolves to a no-op (the `macmon` pattern).

- [ ] **Step 2: Restore the platform gate on the nokhwa backend**

In `capture/mod.rs`, change the two `#[cfg(feature = "hand-tracking-mediapipe-camera")]` lines guarding `mod nokhwa;` / `pub use nokhwa::NokhwaFrameSource;` (the Task 1 TODO) to:

```rust
#[cfg(all(feature = "hand-tracking-mediapipe-camera", not(target_os = "macos")))]
mod nokhwa;
#[cfg(all(feature = "hand-tracking-mediapipe-camera", not(target_os = "macos")))]
pub use nokhwa::NokhwaFrameSource;
```

Remove the `// TODO(Task 7)` comments.

- [ ] **Step 3: Branch `open_camera_source` by platform**

Replace the body of `open_camera_source` in `mediapipe/mod.rs` (currently building `NokhwaFrameSource`):

```rust
fn open_camera_source(camera_index: u32) -> Result<Box<dyn FrameSource>, CaptureError> {
    #[cfg(all(feature = "hand-tracking-mediapipe-camera", target_os = "macos"))]
    {
        let source = capture::AvfFrameSource::open(camera_index)?;
        let boxed: Box<dyn FrameSource> = Box::new(source);
        Ok(boxed)
    }
    #[cfg(all(feature = "hand-tracking-mediapipe-camera", not(target_os = "macos")))]
    {
        let source = capture::NokhwaFrameSource::open(camera_index)?;
        let boxed: Box<dyn FrameSource> = Box::new(source);
        Ok(boxed)
    }
    #[cfg(not(feature = "hand-tracking-mediapipe-camera"))]
    {
        let _ = camera_index;
        Err(CaptureError::NoCamera(
            "build with the hand-tracking-mediapipe-camera feature".into(),
        ))
    }
}
```

- [ ] **Step 4: Delete the `objc_exception` build hack**

In `crates/waveconductor/build.rs`: delete the `relink_objc_exception_for_dynamic_linking` function and its call from `main`, and the lines in the module `//!` doc describing it. Keep the Windows `LeapC.dll` copy and the `cargo:rerun-if-changed` line. The function is dead once nokhwa (and thus `objc_exception`) no longer builds on macOS.

- [ ] **Step 5: Verify nokhwa and the stale chain are gone on macOS**

Run on macOS:
```bash
cargo tree -p waveconductor -i core-video-sys --features waveconductor/... 2>&1 || echo "core-video-sys ABSENT (expected)"
cargo tree -p wc-core --features hand-tracking-mediapipe-camera -i core-video-sys 2>&1 | head
cargo tree -p wc-core --features hand-tracking-mediapipe-camera -i objc 2>&1 | head
cargo tree -p wc-core --features hand-tracking-mediapipe-camera -i nokhwa 2>&1 | head
```
Expected: `core-video-sys`, `objc` (0.2), and `nokhwa` all report **not found** in the wc-core macOS tree with the camera feature. (`error: package ID specification ... did not match any packages` is the success signal.)

- [ ] **Step 6: Build + smoke the real app on macOS**

Run:
```bash
cargo build -p wc-core --features hand-tracking-mediapipe-camera
cargo rund --features waveconductor/hand-tracking-mediapipe-camera
```
(Confirm the exact `cargo rund` feature-forwarding form against `.cargo/config.toml`; the alias may already enable the camera feature.) Wave a hand: tracking works, the dev panel shows the BGRA format label, and entering idle/screensaver visibly drops the camera capture rate (and restores on wake). Grant the camera-permission prompt if it appears, and record in the commit body whether `cargo rund` (non-bundled) prompts correctly or needs an Info.plist note for the bundled `.app`.

- [ ] **Step 7: Commit**

```bash
git add crates/wc-core/Cargo.toml crates/wc-core/src/input/providers/mediapipe Cargo.lock crates/waveconductor/build.rs
git commit -m "feat(capture): cut macOS over to AVFoundation, drop nokhwa + objc_exception hack"
```

---

### Task 8: Full verification pass

Run every CI gate on the dev machine (macOS) and confirm the spec's success criteria. No new code unless a gate fails.

**Files:** none (verification only; fixes land in the relevant task's file if a gate fails).

- [ ] **Step 1: Run the full gate suite**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
```
Expected: all pass. (`cargo doc` may emit the ~29 known pre-existing intra-doc warnings — non-fatal. `rustfmt.toml` nightly-feature warnings on stable are expected.)

- [ ] **Step 2: Confirm the success criteria from the spec**

- `cargo tree` (macOS, `--all-features`): `core-video-sys`, `objc 0.2`, `metal 0.18`, `objc_exception`, `nokhwa` all absent from the wc-core tree.
- nokhwa still builds on a non-macOS target: `cargo build -p wc-core --target x86_64-unknown-linux-gnu --features hand-tracking-mediapipe-camera` (if a Linux toolchain/cross is available; otherwise rely on CI's Linux leg).
- macOS hand-tracking works via `cargo rund` (Task 7 Step 6).
- Idle camera-rate drop observed and restores on wake (Task 7 Step 6).
- `build.rs` `objc_exception` hack removed (Task 7 Step 4).

- [ ] **Step 3: Note the soak obligation**

The pre-release 8-hour soak (AGENTS) covers the steady-state thermal path and is required before any release tag — out of band for this branch, but record it as the remaining gate in the PR description.

- [ ] **Step 4: Final commit if any gate fixes were needed**

```bash
git add -A
git commit -m "chore(capture): verification fixes for macOS AVFoundation backend"
```

---

## Self-Review

**Spec coverage:**
- macOS AVFoundation backend → Tasks 3–6. ✓
- nokhwa gated to non-macOS, kept for Linux/Windows → Task 7 Steps 1–3. ✓
- Push→pull `Arc<Mutex<LatestFrame>>` single-slot, newest-wins, alloc-free → Task 4. ✓
- `set_capture_throttle` trait method + worker edge-trigger + wake-latency doc → Task 2. ✓
- Idle hardware frame-duration drop → Task 6 Step 5 (`set_capture_throttle` impl). ✓
- Session preset 640×480 + BGRA, AVFoundation does the decode → Task 6 Step 4. ✓
- File split `capture/{mod,nokhwa,avfoundation}.rs` → Tasks 1, 3. ✓
- Feature gating via the `macmon`/`dep:` pattern → Task 6 Step 1, Task 7 Step 1. ✓
- build.rs `objc_exception` hack removal → Task 7 Step 4. ✓
- Tests: pure units (BGRA→RGB, slot, device index, format label, worker edge-trigger) → Tasks 2–5; `#[ignore]`d smoke → Task 6 Step 2; manual + soak → Tasks 7–8. ✓
- deny.toml unchanged (new crates MIT/Apache, crates.io) → Global Constraints; verified in Task 8. ✓
- TCC camera permission note → Task 6 Step 7, Task 7 Step 6. ✓

**Placeholder scan:** The only intentionally-deferred detail is the exact `objc2-av-foundation` cargo `features` list and the precise objc2 0.6 method spellings in Task 6 — explicitly flagged as "verify against docs.rs/ctx7," per the project rule against extrapolating library APIs. All pure-logic steps carry complete code. The `full_rate_min_frame_duration: /* CMTime */` field type is left as the objc2 `CMTime` type to confirm in Task 6 — same FFI-verification rationale.

**Type consistency:** `LatestFrame::store/take_into/consume`, `bgra_to_rgb(bgra, bytes_per_row, width, height, out)`, `select_device_index(device_count, requested) -> Option<usize>`, `format_label(width, height, fps) -> String`, `set_capture_throttle(&mut self, throttled: bool)`, and `AvfFrameSource::open(camera_index: u32)` are referenced consistently across Tasks 2–7. ✓
