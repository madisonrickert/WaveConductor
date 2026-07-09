# Alpha.5 · Plan 01 — GPU Memory Leak Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the per-frame GPU resource leak that kills the app with `DeviceLost: Out of memory` after 5–13 minutes, and evict the two unbounded post-process bind-group caches that a resize would strand.

**Architecture:** The backdrop-blur composite paint callback currently calls `Box::leak` on a freshly created `BindGroup` every frame, per frosted widget, permanently pinning both the bind group and the uniform buffer it references. We move buffer and bind-group creation from `render()` into the `update()` hook (which receives `&mut World`), store them in a render-world resource keyed by a stable `egui::Id`, and let `render()` borrow them for the `'pass` lifetime directly from `&'pass World`. Separately, the Line and Dots post-process nodes cache bind groups in a never-cleared `HashMap<TextureViewId, BindGroup>`; we clear those when the camera's physical target size changes, mirroring the eviction pattern already used in `hand_mesh/bone_composite.rs`.

**Tech Stack:** Rust, Bevy 0.19, `bevy_egui` 0.40, wgpu.

## Global Constraints

Copied verbatim from `AGENTS.md` and the spec. Every task's requirements implicitly include this section.

- **Never allocate in a hot path.** Per-frame Bevy systems, the audio callback, and continuously-running worker threads. Pre-allocate at init and reuse; refill with `vec.clear()` (keeps capacity) instead of reallocating.
- **No `unwrap()` or `expect()` in non-test code** unless the panic is documented as an invariant violation.
- **No `as` casts on numeric types** where `From` / `TryFrom` / `u32::try_from` would work.
- `///` rustdoc on every public item (struct, enum, trait, fn, module). Module-level `//!` on every module root.
- **Never strip comments during refactors.** Update stale comments rather than removing them.
- Public API at the top, private helpers at the bottom, tests in a `#[cfg(test)] mod tests` block at the file footer.
- One concept per file. ~300 lines is a guideline, not a hard cap.
- **CI gates**, all of which must pass before a task is complete:
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features --workspace -- -D warnings`
  - `cargo nextest run --workspace --all-features`
  - `cargo test --doc --workspace`
  - `cargo doc --no-deps --workspace --document-private-items` (CI runs with `RUSTDOCFLAGS="-D warnings"`)
  - `cargo xtask check-secrets`
- **Branch:** all work lands on `windows-remediation`, branched from `v5-alpha` **after** `configurable-attract-mode-timeout` merges.
- **Do not** put `bevy/dynamic_linking` in any manifest `[features]` table. Use `cargo rund` for manual smoke tests.

The clippy gate uses `--all-targets`, not `--lib`. `--lib` skips the test
target, and two of this plan's own test snippets tripped `range_plus_one`
and `used_underscore_binding` there before this was caught.

## Testing note (deviation from the spec)

The spec proposed asserting bounded allocations in `crates/wc-core/tests/ui_blur.rs`. **That will not work as a CI gate.** Every test in that file is `#[ignore]`d, because `DefaultPlugins` pulls in winit, which requires the macOS main thread while cargo's test runner uses worker threads (see `ui_blur.rs:7-18`). `cargo nextest` skips ignored tests, so an assertion added there would never run in CI.

Instead, Task 1 factors the slot bookkeeping into a GPU-free generic (`SlotBook<T>`) whose eviction and bounded-growth properties are unit-tested with `T = ()` in an ordinary `#[cfg(test)] mod tests` block. The regression test that actually matters — *"a widget painted every frame for 5000 frames occupies exactly one slot"* — runs on every CI push with no GPU.

---

### Task 1: `SlotBook<T>` — frame-stamped, self-evicting slot storage

**Files:**
- Create: `crates/wc-core/src/ui/blur/slots.rs`
- Modify: `crates/wc-core/src/ui/blur/mod.rs` (add `mod slots;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub(crate) const SLOT_EVICT_FRAMES: u64 = 600;`
  - `pub(crate) struct SlotBook<T>` with `Default`
  - `pub(crate) fn SlotBook::<T>::tick(&mut self)`
  - `pub(crate) fn SlotBook::<T>::frame(&self) -> u64`
  - `pub(crate) fn SlotBook::<T>::get(&self, id: egui::Id) -> Option<&T>`
  - `pub(crate) fn SlotBook::<T>::insert(&mut self, id: egui::Id, gpu: T)`
  - `pub(crate) fn SlotBook::<T>::touch(&mut self, id: egui::Id) -> Option<&mut T>`
  - `pub(crate) fn SlotBook::<T>::scratch_and_touch(&mut self, id: egui::Id) -> Option<(&mut Vec<u8>, &mut T)>`
  - `pub(crate) fn SlotBook::<T>::len(&self) -> usize`

- [ ] **Step 1: Write the failing test**

Create `crates/wc-core/src/ui/blur/slots.rs` containing *only* the test module for now, so it fails to compile against a missing `SlotBook`:

```rust
#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;

    fn id(n: u64) -> egui::Id {
        egui::Id::new(n)
    }

    #[test]
    fn tick_increments_the_frame_counter() {
        let mut book = SlotBook::<()>::default();
        assert_eq!(book.frame(), 0);
        book.tick();
        assert_eq!(book.frame(), 1);
    }

    #[test]
    fn a_widget_painted_every_frame_creates_its_payload_exactly_once() {
        // This is the regression test for the `Box::leak` bug: the old code
        // allocated a fresh bind group + uniform buffer per widget per frame
        // and never freed either.
        //
        // Counting inserts is what makes this non-vacuous. Asserting only
        // `len() == 1` would pass even if `touch()` never worked, because
        // `insert()` overwrites the entry for a key that already exists — and
        // an insert per frame is precisely the per-frame GPU allocation this
        // module exists to prevent.
        let mut book = SlotBook::<u32>::default();
        let mut payloads_created = 0_u32;
        for _ in 0..5_000 {
            book.tick();
            if book.touch(id(1)).is_none() {
                book.insert(id(1), 0);
                payloads_created += 1;
            }
        }
        assert_eq!(
            payloads_created, 1,
            "the widget's GPU payload must be created once, not once per frame"
        );
        assert_eq!(book.len(), 1, "and it must occupy exactly one slot");
    }

    #[test]
    fn slot_survives_to_the_horizon_and_is_evicted_one_frame_past_it() {
        let mut book = SlotBook::<()>::default();
        book.tick(); // frame 1
        book.insert(id(1), ()); // last_seen = 1

        for _ in 0..SLOT_EVICT_FRAMES {
            book.tick();
        }
        // frame = 1 + 600 = 601, age = 600 == horizon
        assert_eq!(book.len(), 1, "slot must survive at exactly the horizon");

        book.tick();
        // frame = 602, age = 601 > horizon
        assert_eq!(book.len(), 0, "slot must be evicted one frame past the horizon");
    }

    #[test]
    fn touch_refreshes_last_seen_and_prevents_eviction() {
        let mut book = SlotBook::<()>::default();
        book.tick();
        book.insert(id(1), ());
        for _ in 0..(SLOT_EVICT_FRAMES * 3) {
            book.tick();
            assert!(book.touch(id(1)).is_some(), "touched slot must never be evicted");
        }
        assert_eq!(book.len(), 1);
    }

    #[test]
    fn unrelated_slots_are_evicted_independently() {
        let mut book = SlotBook::<()>::default();
        book.tick();
        book.insert(id(1), ());
        book.insert(id(2), ());
        // `0..=N` rather than `0..(N + 1)`: clippy::range_plus_one is denied.
        for _ in 0..=SLOT_EVICT_FRAMES {
            book.tick();
            let _ = book.touch(id(1));
        }
        assert!(book.get(id(1)).is_some(), "touched slot survives");
        assert!(book.get(id(2)).is_none(), "untouched slot is evicted");
        assert_eq!(book.len(), 1);
    }

    #[test]
    fn scratch_and_touch_reuses_the_staging_buffer_capacity() {
        let mut book = SlotBook::<u32>::default();
        book.tick();
        book.insert(id(1), 7);

        let capacity_after_first_use = {
            let (scratch, gpu) = book.scratch_and_touch(id(1)).expect("slot exists");
            assert_eq!(*gpu, 7);
            scratch.extend_from_slice(&[0_u8; 64]);
            scratch.capacity()
        };
        assert!(capacity_after_first_use >= 64);

        book.tick();
        let (scratch, _) = book.scratch_and_touch(id(1)).expect("slot exists");
        scratch.clear();
        assert_eq!(
            scratch.capacity(),
            capacity_after_first_use,
            "clear() must retain capacity — no reallocation on the hot path"
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

First register the module. In `crates/wc-core/src/ui/blur/mod.rs`, next to the existing `mod` declarations, add:

```rust
pub(crate) mod slots;
```

Run: `cargo test -p wc-core --lib ui::blur::slots 2>&1 | head -20`

Expected: FAIL to compile, `cannot find type SlotBook in this scope`.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/wc-core/src/ui/blur/slots.rs`, above the test module:

```rust
//! Frame-stamped slot storage for per-widget GPU resources.
//!
//! ## Why this exists
//!
//! The backdrop-blur composite paint callback needs a uniform buffer and a
//! bind group *per frosted widget*, and `render()` needs to hand the render
//! pass a `&'pass BindGroup`. The original implementation created both every
//! frame inside `render()` and used `Box::leak` to satisfy the `'pass`
//! lifetime, which permanently pinned every bind group and — because a bind
//! group holds an `Arc` on everything it binds — every uniform buffer as well.
//! At 60 fps with several visible widgets that exhausted GPU memory in minutes.
//!
//! ## How this fixes it
//!
//! `bevy_egui` calls `EguiBevyPaintCallbackImpl::update` (which gets
//! `&mut World`) for every paint callback before it calls `render` (which gets
//! `&'pass World`) for any of them. So `update` can create and store the GPU
//! resources in a render-world resource, and `render` can borrow them out of
//! `&'pass World` for free — no leak required.
//!
//! Slots are keyed by a stable [`egui::Id`] rather than by index. Two
//! properties of `bevy_egui` 0.40 forbid an index- or cursor-based pairing
//! between `update` and `render` (verified against `bevy_egui-0.40.0`,
//! `src/render/render_pass.rs`):
//!
//! 1. The `RenderEntity` handed to both hooks is **per egui context**, not per
//!    callback (`src/render/systems.rs:303`), so every widget shares it.
//! 2. `render` is guarded by `if viewport.width_px > 0 && viewport.height_px > 0`
//!    (`render_pass.rs:218`) while `update` is not. A zero-sized widget gets an
//!    `update` with no matching `render`, which would desynchronize any cursor.
//!
//! ## Eviction
//!
//! Every slot records the frame it was last touched. [`SlotBook::tick`] runs
//! once per frame, increments the counter, and drops any slot untouched for
//! more than [`SLOT_EVICT_FRAMES`] frames. This bounds the map if widget ids
//! churn (e.g. a panel that is rebuilt with a different id), while a widget
//! painted continuously keeps exactly one slot forever.

// Transient. `SlotBook` has no non-test caller until Task 4 wires it into
// `callback.rs`, so the lib target (compiled without `cfg(test)`) sees the whole
// type as dead code and `clippy -D warnings` fails. Task 4, Step 6 removes this
// attribute and verifies clippy stays clean without it.
#![allow(dead_code)]

use std::collections::HashMap;

use bevy_egui::egui;

/// Frames a slot may go untouched before it is dropped.
///
/// 600 frames is roughly 10 seconds at 60 fps: long enough that a widget
/// hidden behind a transient state change keeps its GPU resources, short
/// enough that a churning id cannot accumulate.
pub(crate) const SLOT_EVICT_FRAMES: u64 = 600;

/// A frame-stamped map from [`egui::Id`] to a per-widget GPU payload, plus a
/// reusable CPU staging buffer.
///
/// Generic over the payload so the bookkeeping can be unit-tested without a
/// GPU (see the tests at the file footer, which instantiate `SlotBook<()>`).
pub(crate) struct SlotBook<T> {
    /// Monotonic frame counter, advanced by [`SlotBook::tick`].
    frame: u64,
    /// Live slots, keyed by the widget's stable egui id.
    slots: HashMap<egui::Id, Slot<T>>,
    /// Staging buffer reused across frames for uniform encoding. Owned here so
    /// the composite callback never allocates on the render hot path.
    scratch: Vec<u8>,
}

impl<T> Default for SlotBook<T> {
    fn default() -> Self {
        Self {
            frame: 0,
            slots: HashMap::new(),
            scratch: Vec::new(),
        }
    }
}

impl<T> SlotBook<T> {
    /// Advance to the next frame and drop slots untouched for longer than
    /// [`SLOT_EVICT_FRAMES`].
    ///
    /// Must be called exactly once per rendered frame, before any `update`
    /// hook touches the book.
    ///
    /// On `u64` wraparound a stale slot's age computes as 0 and it becomes
    /// immortal rather than being evicted. At 60 fps that is ~10 billion years
    /// away, so we accept silently-wrong over a panic on the render hot path.
    pub(crate) fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        let frame = self.frame;
        self.slots
            .retain(|_, slot| frame.saturating_sub(slot.last_seen) <= SLOT_EVICT_FRAMES);
    }

    /// The current frame counter.
    ///
    /// Test-only: production code never reads the counter directly. Without the
    /// `cfg(test)` gate, rustc's `dead_code` lint fires when the lib target is
    /// compiled without `cfg(test)`, and CI runs clippy with `-D warnings`.
    #[cfg(test)]
    pub(crate) fn frame(&self) -> u64 {
        self.frame
    }

    /// Borrow a slot's payload without refreshing its `last_seen` stamp.
    ///
    /// Used by `render`, which holds only `&World` and must not mutate.
    pub(crate) fn get(&self, id: egui::Id) -> Option<&T> {
        self.slots.get(&id).map(|slot| &slot.gpu)
    }

    /// Insert (or replace) a slot, stamped with the current frame.
    pub(crate) fn insert(&mut self, id: egui::Id, gpu: T) {
        let last_seen = self.frame;
        self.slots.insert(id, Slot { gpu, last_seen });
    }

    /// Refresh a slot's `last_seen` stamp and borrow its payload mutably.
    ///
    /// Returns `None` if the slot does not exist.
    ///
    /// Test-only: production code uses [`SlotBook::scratch_and_touch`], which
    /// does the same refresh and also hands back the staging buffer. Gated for
    /// the same `dead_code` reason as `frame()`.
    #[cfg(test)]
    pub(crate) fn touch(&mut self, id: egui::Id) -> Option<&mut T> {
        let frame = self.frame;
        self.slots.get_mut(&id).map(|slot| {
            slot.last_seen = frame;
            &mut slot.gpu
        })
    }

    /// Refresh a slot and borrow the shared staging buffer alongside it.
    ///
    /// The two borrows are disjoint struct fields, which is why this cannot be
    /// expressed as two separate method calls.
    pub(crate) fn scratch_and_touch(&mut self, id: egui::Id) -> Option<(&mut Vec<u8>, &mut T)> {
        let frame = self.frame;
        let slot = self.slots.get_mut(&id)?;
        slot.last_seen = frame;
        Some((&mut self.scratch, &mut slot.gpu))
    }

    /// Number of live slots.
    ///
    /// Test-only, for the bounded-growth assertions. Gated for the same
    /// `dead_code` reason as `frame()`.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }
}

/// One widget's GPU payload plus the frame on which it was last painted.
///
/// Private helper, placed below the public API per AGENTS.md.
struct Slot<T> {
    /// The GPU resources this widget owns.
    gpu: T,
    /// Value of the `SlotBook` frame counter the last time this slot was
    /// touched. Compared against the current frame by [`SlotBook::tick`].
    last_seen: u64,
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wc-core --lib ui::blur::slots`

Expected: PASS, 6 tests.

- [ ] **Step 5: Run the scoped gate and commit**

The workspace-wide clippy gate is deliberately *not* run here: it takes long enough to time out a subagent, and the controller runs it between tasks.

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --lib ui::blur::slots
git add crates/wc-core/src/ui/blur/slots.rs crates/wc-core/src/ui/blur/mod.rs
git commit -m "feat(ui/blur): add SlotBook, frame-stamped per-widget GPU slot storage"
```

---

### Task 2: Extract composite uniform math into a pure, testable function

**Files:**
- Modify: `crates/wc-core/src/ui/blur/callback.rs:272-306` (the geometry + uniform block inside `render`)

**Interfaces:**
- Consumes: `CompositeUniforms` (already defined at `callback.rs:89-100`).
- Produces: `pub(crate) fn composite_uniforms(screen_size_px: [u32; 2], pixels_per_point: f32, rect: egui::Rect, corner_radius_points: f32) -> Option<CompositeUniforms>`
  - Returns `None` when either screen dimension is zero (the existing silent-bail condition at `callback.rs:278-280`).

This function currently exists only as inline code inside `render()`, and is therefore untested. Task 4 moves it into `update()`; extracting it first means Task 4 is a pure plumbing change.

- [ ] **Step 1: Write the failing test**

Add to the footer of `crates/wc-core/src/ui/blur/callback.rs` (create the `mod tests` block if absent):

```rust
#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
#[allow(
    clippy::used_underscore_binding,
    reason = "`_pad` is shader struct padding; asserting it stays 0.0 is the point"
)]
mod tests {
    use super::*;

    #[test]
    fn uniforms_map_a_rect_to_normalised_uvs_at_unit_scale() {
        let rect = egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(300.0, 150.0));
        let u = composite_uniforms([1000, 500], 1.0, rect, 8.0).expect("non-zero screen");

        assert!((u.uv_rect.x - 0.1).abs() < 1e-6, "uv min x");
        assert!((u.uv_rect.y - 0.1).abs() < 1e-6, "uv min y");
        assert!((u.uv_rect.z - 0.3).abs() < 1e-6, "uv max x");
        assert!((u.uv_rect.w - 0.3).abs() < 1e-6, "uv max y");
        assert!((u.half_extent.x - 100.0).abs() < 1e-6, "half width in px");
        assert!((u.half_extent.y - 50.0).abs() < 1e-6, "half height in px");
        assert!((u.corner_radius - 8.0).abs() < 1e-6);
        assert!((u._pad - 0.0).abs() < 1e-6);
    }

    #[test]
    fn uniforms_scale_points_to_physical_pixels_by_pixels_per_point() {
        let rect = egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(300.0, 150.0));
        let u = composite_uniforms([1000, 500], 2.0, rect, 8.0).expect("non-zero screen");

        // Rect is in points; screen_size_px is already physical.
        assert!((u.uv_rect.x - 0.2).abs() < 1e-6);
        assert!((u.uv_rect.y - 0.2).abs() < 1e-6);
        assert!((u.uv_rect.z - 0.6).abs() < 1e-6);
        assert!((u.uv_rect.w - 0.6).abs() < 1e-6);
        assert!((u.half_extent.x - 200.0).abs() < 1e-6);
        assert!((u.half_extent.y - 100.0).abs() < 1e-6);
        assert!((u.corner_radius - 16.0).abs() < 1e-6, "corner radius is scaled too");
    }

    #[test]
    fn uniforms_bail_on_a_zero_sized_screen() {
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(10.0, 10.0));
        assert!(composite_uniforms([0, 500], 1.0, rect, 0.0).is_none());
        assert!(composite_uniforms([1000, 0], 1.0, rect, 0.0).is_none());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p wc-core --lib ui::blur::callback 2>&1 | head -20`

Expected: FAIL to compile, `cannot find function composite_uniforms in this scope`.

- [ ] **Step 3: Write the implementation**

Add this function to `crates/wc-core/src/ui/blur/callback.rs`, immediately below the `CompositeUniforms` struct definition (i.e. after line 100):

```rust
/// Convert a panel rect in egui points into the [`CompositeUniforms`] the
/// composite shader expects.
///
/// `screen_size_px` is the egui render target size in *physical pixels*;
/// `rect` and `corner_radius_points` are in *egui points*. Both are scaled by
/// `pixels_per_point` where the shader needs physical pixels.
///
/// Returns `None` when either screen dimension is zero, which happens on the
/// first frame before the window reports a size. Callers bail silently.
pub(crate) fn composite_uniforms(
    screen_size_px: [u32; 2],
    pixels_per_point: f32,
    rect: egui::Rect,
    corner_radius_points: f32,
) -> Option<CompositeUniforms> {
    // `screen_size_px` is [width, height] of the egui render target in
    // physical pixels. We use it to normalise the panel rect into UVs.
    let screen_w = screen_size_px[0] as f32;
    let screen_h = screen_size_px[1] as f32;
    if screen_w <= 0.0 || screen_h <= 0.0 {
        return None;
    }

    let ppp = pixels_per_point;
    // Convert panel rect (points) → physical pixels → [0,1] UVs.
    let uv_min = Vec2::new(rect.min.x * ppp / screen_w, rect.min.y * ppp / screen_h);
    let uv_max = Vec2::new(rect.max.x * ppp / screen_w, rect.max.y * ppp / screen_h);

    // Half-extent in physical pixels, used by the SDF in the shader.
    let half_extent = Vec2::new((rect.width() * ppp) * 0.5, (rect.height() * ppp) * 0.5);

    Some(CompositeUniforms {
        uv_rect: Vec4::new(uv_min.x, uv_min.y, uv_max.x, uv_max.y),
        half_extent,
        corner_radius: corner_radius_points * ppp,
        _pad: 0.0,
    })
}
```

Then replace the inline block at `callback.rs:272-306` (from the `// --- Geometry conversion ---` comment through the `let uniforms = CompositeUniforms { ... };` statement) with:

```rust
        // --- Geometry conversion ---

        let Some(uniforms) = composite_uniforms(
            info.screen_size_px,
            info.pixels_per_point,
            self.rect,
            self.corner_radius,
        ) else {
            return;
        };
```

Leave the rest of `render()` untouched for now — including the `Box::leak`. Task 4 removes it. This task must be a behaviour-preserving refactor.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wc-core --lib ui::blur::callback`

Expected: PASS, 3 tests.

- [ ] **Step 5: Run the full gate and commit**

```bash
cargo fmt --all
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo test -p wc-core --lib ui::blur
git add crates/wc-core/src/ui/blur/callback.rs
git commit -m "refactor(ui/blur): extract composite_uniforms into a pure tested function"
```

---

### Task 3: Give each frosted widget a stable `egui::Id`

**Files:**
- Modify: `crates/wc-core/src/ui/blur/callback.rs:216-223` (struct definition)
- Modify: `crates/wc-core/src/ui/frame.rs:78-89` (construction site)
- Modify: `crates/wc-core/src/ui/buttons.rs:331-339` (construction site)

**Interfaces:**
- Consumes: nothing.
- Produces: `BackdropBlurPaintCallback` gains `pub id: egui::Id`. Task 4 keys `SlotBook` by it.

Both construction sites already have an `egui::Response` in scope whose `.id` is the stable per-widget id. `buttons.rs` even documents that it uses `response.id` as an animation key "so each button animates independently" (`buttons.rs:308-310`) — exactly the stability property we need.

- [ ] **Step 1: Add the field**

In `crates/wc-core/src/ui/blur/callback.rs`, change the struct (currently at lines 216-223) to:

```rust
pub struct BackdropBlurPaintCallback {
    /// Stable per-widget egui id, used to key this widget's GPU slot in
    /// `SlotBook`. Must be the same value on every frame the widget is
    /// painted, and distinct from every other frosted widget's id. Both
    /// construction sites pass `response.id`, which egui derives from the
    /// containing `Ui` and the widget's allocation order.
    pub id: egui::Id,
    /// Corner radius of the panel in egui points. Converted to physical pixels
    /// at render time via `info.pixels_per_point`.
    pub corner_radius: f32,
    /// Panel bounding rect in egui points. Used to compute UVs into the blur
    /// texture and to derive the SDF half-extent.
    pub rect: egui::Rect,
}
```

- [ ] **Step 2: Run the build to verify it fails**

Run: `cargo check -p wc-core 2>&1 | head -20`

Expected: FAIL, `missing field id in initializer of BackdropBlurPaintCallback` at both `frame.rs` and `buttons.rs`.

- [ ] **Step 3: Update both construction sites**

In `crates/wc-core/src/ui/frame.rs`, the callback is constructed at line 81. The surrounding `let (outer_rect, response) = ui.allocate_exact_size(desired, egui::Sense::hover());` (line 71) already binds `response`. Change the initializer to:

```rust
            BackdropBlurPaintCallback {
                id: response.id,
                // BackdropBlurPaintCallback stores corner_radius as f32 for
                // shader uniform upload (physical-pixel conversion happens there).
                corner_radius: f32::from(options.corner_radius),
                rect: outer_rect,
            },
```

In `crates/wc-core/src/ui/buttons.rs`, the callback is constructed at line 334, inside `overlay_icon_button`, where `response` is bound at line 304. Change the initializer to:

```rust
                BackdropBlurPaintCallback {
                    id: response.id,
                    corner_radius: f32::from(style.button_corner_radius),
                    rect,
                },
```

- [ ] **Step 4: Run the build to verify it passes**

Run: `cargo check -p wc-core`

Expected: PASS, no errors.

- [ ] **Step 5: Run the full gate and commit**

```bash
cargo fmt --all
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run -p wc-core
git add crates/wc-core/src/ui/blur/callback.rs crates/wc-core/src/ui/frame.rs crates/wc-core/src/ui/buttons.rs
git commit -m "feat(ui/blur): carry a stable egui::Id on BackdropBlurPaintCallback"
```

---

### Task 4: Remove `Box::leak` — create in `update`, borrow in `render`

**Files:**
- Modify: `crates/wc-core/src/ui/blur/callback.rs` (`update`, `render`, add `CompositeSlots` + `tick_composite_slots`)
- Modify: `crates/wc-core/src/ui/blur/mod.rs:114-145` (register the resource and the tick system)

**Interfaces:**
- Consumes: `SlotBook<T>` (Task 1), `composite_uniforms` (Task 2), `BackdropBlurPaintCallback::id` (Task 3).
- Produces:
  - `#[derive(Resource, Default)] pub(crate) struct CompositeSlots(pub(crate) SlotBook<CompositeGpu>);`
  - `pub(crate) struct CompositeGpu { buffer: Buffer, bind_group: BindGroup, blur_view: TextureViewId }`
  - `pub(crate) fn tick_composite_slots(slots: ResMut<'_, CompositeSlots>)`

- [ ] **Step 1: Add the imports and the new types**

In `crates/wc-core/src/ui/blur/callback.rs`, extend the `bevy::render::render_resource` import list (currently lines 40-46) with `BindGroup`, `Buffer`, `BufferDescriptor`, and `TextureViewId`:

```rust
use bevy::render::render_resource::{
    BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingType,
    Buffer, BufferBindingType, BufferDescriptor, BufferUsages, CachedRenderPipelineId,
    ColorTargetState, ColorWrites, FragmentState, MultisampleState, PipelineCache, PrimitiveState,
    RenderPipelineDescriptor, SamplerBindingType, ShaderStages, ShaderType, TextureFormat,
    TextureSampleType, TextureViewDimension, TextureViewId, VertexState,
};
```

And add, below the `CompositePipeline` struct:

```rust
/// GPU resources owned by one frosted widget's composite draw.
///
/// The bind group holds `Arc` references to the blur texture view, the
/// sampler, and `buffer`. Keeping the `BindGroup` alive here — rather than
/// leaking it — is what bounds the app's GPU memory. The bind group is rebuilt
/// only when `blur_view` changes (i.e. on a window resize); the buffer's
/// *contents* are rewritten every frame via `Queue::write_buffer`, which does
/// not invalidate the binding.
pub(crate) struct CompositeGpu {
    /// Per-widget `CompositeUniforms` buffer (32 bytes).
    buffer: Buffer,
    /// Bind group over (blur texture view, sampler, `buffer`).
    bind_group: BindGroup,
    /// Id of the blur texture view this bind group was built against. When the
    /// blur texture is reallocated (resize), the id changes and the bind group
    /// must be rebuilt or it would sample a freed texture.
    blur_view: TextureViewId,
}

/// Render-world storage for every frosted widget's [`CompositeGpu`].
///
/// Populated by [`BackdropBlurPaintCallback::update`], read by
/// [`BackdropBlurPaintCallback::render`], advanced and pruned once per frame by
/// [`tick_composite_slots`].
#[derive(Resource, Default)]
pub(crate) struct CompositeSlots(pub(crate) super::slots::SlotBook<CompositeGpu>);

/// Advance the composite slot book one frame and evict stale widgets.
///
/// Registered in `Render` under `RenderSystems::PrepareResources`, which runs
/// before the render graph — and therefore before `bevy_egui`'s
/// `prepare_egui_pass` node invokes any paint callback's `update`.
pub(crate) fn tick_composite_slots(mut slots: ResMut<'_, CompositeSlots>) {
    slots.0.tick();
}
```

- [ ] **Step 2: Replace `update` with the resource-creating implementation**

Replace the empty `update` at `callback.rs:225-236` with:

```rust
impl EguiBevyPaintCallbackImpl for BackdropBlurPaintCallback {
    /// Create or refresh this widget's [`CompositeGpu`] slot.
    ///
    /// `bevy_egui` calls `update` for every paint callback (from the
    /// `prepare_egui_pass` render-graph node) before it calls `render` for any
    /// of them, so writing here and reading in `render` is sound. We create the
    /// uniform buffer and bind group **once per widget**, not once per frame:
    /// the buffer contents are rewritten with `write_buffer`, and the bind
    /// group is rebuilt only when the blur texture view is reallocated.
    ///
    /// Bails silently on any missing resource, mirroring `render`.
    fn update(
        &self,
        info: egui::PaintCallbackInfo,
        _render_entity: RenderEntity,
        _pipeline_key: EguiPipelineKey,
        world: &mut World,
    ) {
        let Some(uniforms) = composite_uniforms(
            info.screen_size_px,
            info.pixels_per_point,
            self.rect,
            self.corner_radius,
        ) else {
            return;
        };

        // Bail before `resource_scope` panics on a missing resource. In headless
        // tests without a RenderApp the plugin never inits this.
        if world.get_resource::<CompositeSlots>().is_none() {
            return;
        }

        let id = self.id;
        world.resource_scope(|world: &mut World, mut slots: Mut<'_, CompositeSlots>| {
            let Some(pipeline_data) = world.get_resource::<CompositePipeline>() else {
                return;
            };
            let Some(blur_texture) = world.get_resource::<super::BackdropBlurTexture>() else {
                return;
            };
            let pipeline_cache = world.resource::<PipelineCache>();
            let device = world.resource::<RenderDevice>();
            let queue = world.resource::<RenderQueue>();

            let blur_view = blur_texture.view.id();

            // Rebuild only when absent or when the blur texture was reallocated.
            let stale = slots
                .0
                .get(id)
                .is_none_or(|gpu: &CompositeGpu| gpu.blur_view != blur_view);
            if stale {
                let buffer = device.create_buffer(&BufferDescriptor {
                    label: Some("backdrop_blur_composite_uniforms"),
                    size: CompositeUniforms::min_size().get(),
                    usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let layout =
                    pipeline_cache.get_bind_group_layout(&pipeline_data.bind_group_layout_descriptor);
                let bind_group = device.create_bind_group(
                    Some("backdrop_blur_composite_bind_group"),
                    &layout,
                    &BindGroupEntries::sequential((
                        &blur_texture.view,
                        &blur_texture.sampler,
                        buffer.as_entire_binding(),
                    )),
                );
                slots.0.insert(
                    id,
                    CompositeGpu {
                        buffer,
                        bind_group,
                        blur_view,
                    },
                );
            }

            // Rewrite the uniform contents every frame through the reusable
            // staging buffer. `clear()` retains capacity, so steady state does
            // not allocate (the project's no-hot-path-allocation rule).
            let Some((scratch, gpu)) = slots.0.scratch_and_touch(id) else {
                return;
            };
            {
                use bevy::render::render_resource::encase;
                scratch.clear();
                let mut staging = encase::UniformBuffer::new(std::mem::take(scratch));
                // `write` only fails if the staging buffer is too small. `encase`
                // grows a `Vec` backing store as needed, so a failure here is an
                // invariant violation and a panic is correct.
                #[allow(clippy::expect_used)]
                staging
                    .write(&uniforms)
                    .expect("CompositeUniforms: write to staging buffer");
                queue.write_buffer(&gpu.buffer, 0, staging.as_ref());
                *scratch = staging.into_inner();
            }
        });
    }
```

- [ ] **Step 3: Replace `render` with the borrowing implementation**

Replace the whole body of `render` (`callback.rs:250` through the closing brace of the method) with:

```rust
    /// Draw the blurred backdrop quad.
    ///
    /// All GPU resource creation happened in [`Self::update`]. This method only
    /// looks up the pipeline and this widget's slot, then issues the draw. The
    /// `&'pass BindGroup` that `set_bind_group` requires is borrowed straight
    /// out of `world: &'pass World`, which is why no `Box::leak` is needed.
    ///
    /// The vertex shader triangulates the quad from a const array indexed by
    /// `@builtin(vertex_index)`, so no vertex buffer is bound.
    fn render<'pass>(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut bevy::render::render_phase::TrackedRenderPass<'pass>,
        _render_entity: RenderEntity,
        _pipeline_key: EguiPipelineKey,
        world: &'pass World,
    ) {
        // --- Resource lookups — bail silently on any miss ---

        let Some(pipeline_data) = world.get_resource::<CompositePipeline>() else {
            return;
        };
        let pipeline_cache = world.resource::<PipelineCache>();
        let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline_data.pipeline) else {
            // Pipeline still compiling on the first few frames; not an error.
            return;
        };
        let Some(slots) = world.get_resource::<CompositeSlots>() else {
            return;
        };
        // Absent when `update` bailed this frame (e.g. blur texture not yet
        // allocated). The caller's tint rect still paints, so the panel
        // degrades to a solid translucent fill.
        let Some(gpu) = slots.0.get(self.id) else {
            return;
        };

        // --- Draw ---

        render_pass.set_render_pipeline(pipeline);
        render_pass.set_bind_group(0, &gpu.bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }
}
```

Delete the now-unused `RenderDevice` / `RenderQueue` usage in `render` if the compiler flags it, and confirm `Box::leak` no longer appears in the file.

- [ ] **Step 4: Update the module docs**

At `callback.rs:27-28`, the module doc currently reads:

```
//! A fresh per-frame `CompositeUniforms` buffer is uploaded for each panel
//! rect (32 bytes; acceptable once-per-visible-panel cost).
```

Replace with:

```
//! Each panel owns one persistent 32-byte `CompositeUniforms` buffer, created
//! on first paint and rewritten in place every frame via `Queue::write_buffer`.
//! Buffers and bind groups live in `CompositeSlots`, keyed by the widget's
//! stable `egui::Id`, and are evicted after `SLOT_EVICT_FRAMES` frames without
//! a paint. Nothing is allocated on the render hot path.
```

- [ ] **Step 5: Register the resource and the tick system**

In `crates/wc-core/src/ui/blur/mod.rs`, change `setup_render_app` (lines 114-125) to add the tick system alongside `ensure_blur_texture`:

```rust
    fn setup_render_app(app: &mut App) {
        // In Bevy 0.18, get_sub_app_mut returns Option<&mut SubApp>.
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.add_systems(
            Render,
            (
                ensure_blur_texture,
                // Advances the composite slot book and evicts widgets that have
                // not painted recently. Must run before the render graph, where
                // bevy_egui's prepare_egui_pass node invokes `update`.
                callback::tick_composite_slots,
            )
                .in_set(RenderSystems::PrepareResources),
        );
        node::setup_render_systems(render_app);
        node::setup_render_graph(render_app);
    }
```

And in `finish` (lines 139-145), init the resource next to the pipeline:

```rust
        render_app.init_resource::<node::BackdropBlurPipeline>();
        render_app.init_resource::<callback::CompositePipeline>();
        render_app.init_resource::<callback::CompositeSlots>();
```

> **Note:** `tick_composite_slots` is registered exactly once. Registering the same system twice in one schedule creates an ambiguous `SystemTypeSet` and panics at startup when anything orders against it.

- [ ] **Step 6: Remove the transient dead-code allow, verify no leak remains, run the gate**

`SlotBook` now has non-test callers (`update`, `render`, `tick_composite_slots`), so the transient attribute added in Task 1 must go. Delete these six lines from the top of `crates/wc-core/src/ui/blur/slots.rs`:

```rust
// Transient. `SlotBook` has no non-test caller until Task 4 wires it into
// `callback.rs`, so the lib target (compiled without `cfg(test)`) sees the whole
// type as dead code and `clippy -D warnings` fails. Task 4, Step 6 removes this
// attribute and verifies clippy stays clean without it.
#![allow(dead_code)]
```

If clippy then reports `dead_code` on any `SlotBook` method, that method has no production caller and the wiring in Steps 2, 3, and 5 is incomplete. Fix the wiring; do not restore the attribute.

```bash
rg -n "allow\(dead_code\)" crates/wc-core/src/ui/blur/slots.rs   # expect: no matches
rg -n "Box::leak" crates/    # expect: no matches
cargo fmt --all
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
```

Expected: `rg` prints nothing; all gates pass.

- [ ] **Step 7: Manual smoke test**

Run: `cargo rund`

Expected: the app launches, panels and buttons show frosted-glass backdrops indistinguishable from before, and no `wgpu` validation errors appear in the log. Open the settings panel (Shift+D for the dev panel), leave it open for a minute, and confirm memory in Activity Monitor is flat rather than climbing.

- [ ] **Step 8: Commit**

```bash
git add crates/wc-core/src/ui/blur/callback.rs crates/wc-core/src/ui/blur/mod.rs
git commit -m "fix(ui/blur): stop leaking a bind group and uniform buffer every frame

BackdropBlurPaintCallback::render called Box::leak on a freshly created
BindGroup each frame, per frosted widget. A wgpu BindGroup is an owning
handle, so the leaked wrapper pinned both the bind group and the uniform
buffer it bound, permanently. On an integrated GPU this exhausted memory
in 5-13 minutes (DeviceLost: Out of memory); on a discrete GPU it merely
took longer.

Buffer and bind-group creation now happen in the update() hook, which
receives &mut World, and are stored in a CompositeSlots render-world
resource keyed by a stable egui::Id. render() borrows the bind group from
&'pass World, which satisfies set_bind_group's lifetime without leaking.

Slots are keyed by id rather than paired positionally because bevy_egui
shares one RenderEntity across all callbacks in a context, and skips
render() for zero-sized viewports where it still calls update()."
```

---

### Task 5: Evict the Line and Dots post-process bind-group caches

**Files:**
- Modify: `crates/wc-sketches/src/line/post_process.rs:316-364`
- Modify: `crates/wc-sketches/src/dots/post_process.rs:268-317`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: nothing consumed later. Independent of Tasks 1–4.

Both systems hold `Local<'_, HashMap<TextureViewId, BindGroup>>` with no eviction. Their comments concede the flaw: *"a resize recreates the view targets (new ids → fresh entries), leaving the stale pair resident — a bounded, rare cost for a kiosk app that never resizes."* Plan 02 introduces F11 fullscreen and resize handling, which breaks that assumption. Each stranded entry pins a full-screen `Rgba16Float` texture.

The fix mirrors `crates/wc-sketches/src/hand_mesh/bone_composite.rs:237,276-280`, which pairs the map with the id of the resource whose change invalidates it. Here the invalidating signal is the camera's physical target size: a resize reallocates the view targets.

- [ ] **Step 1: Change the Line cache to a size-keyed pair**

In `crates/wc-sketches/src/line/post_process.rs`, change the system parameter at line 322 from:

```rust
    mut bind_group_cache: Local<'_, HashMap<TextureViewId, BindGroup>>,
```

to:

```rust
    mut bind_group_cache: Local<'_, (Option<UVec2>, HashMap<TextureViewId, BindGroup>)>,
```

Add `UVec2` to the `bevy::prelude` or math import if not already in scope (it is re-exported by `bevy::prelude::*`).

- [ ] **Step 2: Clear on target-size change, and fix the stale comment**

Replace the comment and `entry` call at lines 355-364 with:

```rust
    // Reuse the bind group for this source view if we have built it before.
    // `post_process_write` cycles `source` between two stable views, so after the
    // first two frames every frame is a cache hit — no per-frame
    // `create_bind_group` on the render hot path (the project's
    // no-hot-path-allocation rule). The other two entries (persistent uniform
    // buffer + sampler) never change, and `write_buffer` updates the uniform
    // contents without invalidating the binding.
    //
    // A resize reallocates the view targets, minting new `TextureViewId`s. We
    // clear the map on that transition, dropping the bind groups that still
    // referenced the old (now freed) full-screen HDR targets. Without this the
    // map would grow by two entries per resize for the life of the process —
    // each pinning an `Rgba16Float` screen-sized texture. Steady state holds
    // exactly two entries. Same shape as `hand_mesh::bone_composite`.
    let target_size = camera.physical_target_size;
    if bind_group_cache.0 != target_size {
        bind_group_cache.1.clear();
        bind_group_cache.0 = target_size;
    }
    let bind_group = bind_group_cache.1.entry(source.id()).or_insert_with(|| {
```

Leave the `or_insert_with` closure body unchanged.

- [ ] **Step 3: Apply the identical change to Dots**

In `crates/wc-sketches/src/dots/post_process.rs`, make the same two edits: the `Local` type at line 274, and the cache-clear plus `bind_group_cache.1.entry(...)` at line 317. The surrounding comment differs only in the bind-group label (`dots_post_bind_group`); update its stale "kiosk app that never resizes" sentence to the same replacement text as Step 2.

- [ ] **Step 4: Verify no unevicted caches remain**

```bash
rg -n "Local<'_, HashMap<TextureViewId, BindGroup>>" crates/    # expect: no matches
```

Expected: no matches. Every remaining `TextureViewId`-keyed cache is paired with an invalidation key.

- [ ] **Step 5: Run the gate**

```bash
cargo fmt --all
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
```

Expected: all pass.

- [ ] **Step 6: Verify rendering is unchanged**

Run: `cargo xtask capture --list` to find the Line and Dots scenarios, then capture each and compare against its baseline. The bind group is rebuilt with identical bindings, so output must be pixel-identical.

```bash
cargo xtask capture <line-scenario>
cargo xtask capture <dots-scenario>
```

Expected: no diff against baselines. If the captures come back all-black, check that the app window is foregrounded — a backgrounded capture returns `[0,0,0]` frames and is an environment problem, not a regression.

- [ ] **Step 7: Commit**

```bash
git add crates/wc-sketches/src/line/post_process.rs crates/wc-sketches/src/dots/post_process.rs
git commit -m "fix(sketches): evict post-process bind-group caches on resize

Both Line and Dots cached post-process bind groups in a never-cleared
Local<HashMap<TextureViewId, BindGroup>>, betting that a kiosk app never
resizes. F11 fullscreen and the incoming resize-invalidation work break
that bet, and each stranded entry pins a full-screen Rgba16Float texture
for the life of the process.

Pair the map with the camera's physical target size and clear it on
change, matching hand_mesh::bone_composite."
```

---

### Task 6: Ban `Box::leak`, and make AGENTS.md true

**Files:**
- Modify: `clippy.toml`
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: Tasks 4 and 5 must be complete, or `-D warnings` will fail on the code they remove.
- Produces: nothing.

- [ ] **Step 1: Add the lint**

Append to `clippy.toml`:

```toml
# `Box::leak` in a render callback leaks the GPU resources the leaked handle
# owns: a wgpu BindGroup holds Arc references to every resource it binds, so a
# leaked BindGroup pins its uniform buffer, texture view, and sampler forever.
# This cost v5.0.0-alpha.4 a release. See
# docs/superpowers/specs/2026-07-08-windows-remediation-design.md
disallowed-methods = [
    { path = "std::boxed::Box::leak", reason = "leaks the GPU resources the handle owns; store in a render-world resource and borrow for 'pass instead" },
]
```

- [ ] **Step 2: Verify the lint fires and then passes**

First confirm the lint is wired up by temporarily reintroducing a leak. In any `wc-core` function body, add `let _ = Box::leak(Box::new(1_u8));` then run:

Run: `cargo clippy -p wc-core --all-features -- -D warnings 2>&1 | grep -A2 disallowed`

Expected: an error naming `disallowed_methods` and quoting the `reason` string.

If clippy instead reports "unknown method path", the correct path is `alloc::boxed::Box::leak`; substitute it and re-run.

Now remove the temporary line and run:

Run: `cargo clippy --all-targets --all-features --workspace -- -D warnings`

Expected: PASS with no `disallowed_methods` findings.

- [ ] **Step 3: Correct the false claim in AGENTS.md**

In `AGENTS.md`, under **Application performance**, replace this bullet:

```markdown
- GPU resources: every per-sketch resource is owned by an entity tagged with the sketch's marker component, despawned on `OnExit` to release VRAM.
```

with:

```markdown
- GPU resources are released by three distinct mechanisms, and it matters which one applies:
  1. **Entity-owned** resources (meshes, materials, storage buffers held via `Handle`s on a sketch's root entity) are released when `OnExit` despawns that entity.
  2. **Render-world `Resource`s** are *not* touched by an entity despawn. `ExtractResourcePlugin` does not propagate removals, so each one needs an explicit removal system (see `remove_particle_sim_params_if_absent` in `line/particles/compute.rs` and its siblings).
  3. **Render-world `Local` caches** (bind groups keyed by `TextureViewId`, per-widget GPU slots) are owned by no entity and survive every state transition. Each must be bounded and must evict on the change that invalidates it — see `hand_mesh/bone_composite.rs` for the pattern.
  A resource that fits none of these three leaks. `Box::leak` on a wgpu handle always leaks, because the handle owns `Arc` references to everything it binds; `clippy.toml` bans it.
```

- [ ] **Step 4: Reinforce the hot-path rule**

In `AGENTS.md`, under **Application performance**, extend the "Never allocate in a hot path" bullet's list of hot paths. Change:

```markdown
A hot path is *any* code that runs repeatedly for the life of a session, not just the render frame: per-frame Bevy systems, the audio callback, **and continuously-running worker/background threads**
```

to:

```markdown
A hot path is *any* code that runs repeatedly for the life of a session, not just the render frame: per-frame Bevy systems, **egui paint-callback `update`/`render` hooks**, the audio callback, **and continuously-running worker/background threads**
```

- [ ] **Step 5: Run the full gate**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
```

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add clippy.toml AGENTS.md
git commit -m "chore(lint): ban Box::leak; correct AGENTS.md's GPU-resource claim

AGENTS.md claimed every per-sketch GPU resource is entity-owned and
despawned on OnExit. It is not: render-world Resources need explicit
removal systems, and render-world Local caches need bounded eviction.
Both exceptions produced shipped leaks. Document the three mechanisms
and ban the method that caused the worst one."
```

---

## Self-Review

**Spec coverage.** This plan implements spec workstreams 1 (Tasks 1–4), 2 (Task 5), and 9 (Task 6). Spec workstreams 3–8 and 7a/7b are covered by Plans 02–08 and are explicitly out of scope here.

**Deviation from the spec, recorded deliberately.** The spec called for a bounded-allocation assertion in `crates/wc-core/tests/ui_blur.rs`. That file's tests are all `#[ignore]`d because winit requires the macOS main thread, so `cargo nextest` never runs them and the assertion would have been decorative. Task 1 relocates the regression test to a GPU-free unit test over `SlotBook<T>`, where `a_widget_painted_every_frame_occupies_exactly_one_slot` runs on every CI push. The spec should be amended to match.

**Type consistency.** `CompositeSlots` wraps `SlotBook<CompositeGpu>` and is referenced as `slots.0` in `update`, `render`, and `tick_composite_slots`. `SlotBook::get` returns `Option<&T>` (used in `render` and in the staleness check); `SlotBook::touch` returns `Option<&mut T>` (used only in tests); `SlotBook::scratch_and_touch` returns `Option<(&mut Vec<u8>, &mut T)>` (used in `update`). `composite_uniforms` takes `[u32; 2]` to match `egui::PaintCallbackInfo::screen_size_px` and `f32` for `pixels_per_point`, and returns `Option<CompositeUniforms>`.

**Ordering constraint.** Task 6 must run last: its clippy lint would fail the build while Task 4's `Box::leak` still exists. Tasks 1→2→3→4 are strictly ordered. Task 5 is independent of 1–4 and may be done in parallel or out of order, but before Task 6.

**Risk carried forward.** Task 4 depends on a `bevy_egui` invocation-order property (all `update` calls precede all `render` calls) verified against the vendored 0.40.0 source, not against a documented API guarantee. The `egui::Id` keying makes the code robust to both the shared `RenderEntity` and the `update`/`render` count mismatch, so a future `bevy_egui` bump degrades to a slot-lifetime bug rather than silent cross-panel corruption. Re-verify `src/render/render_pass.rs` on any `bevy_egui` upgrade.
