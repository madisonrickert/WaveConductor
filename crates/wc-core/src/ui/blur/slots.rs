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
        assert_eq!(
            book.len(),
            0,
            "slot must be evicted one frame past the horizon"
        );
    }

    #[test]
    fn touch_refreshes_last_seen_and_prevents_eviction() {
        let mut book = SlotBook::<()>::default();
        book.tick();
        book.insert(id(1), ());
        for _ in 0..(SLOT_EVICT_FRAMES * 3) {
            book.tick();
            assert!(
                book.touch(id(1)).is_some(),
                "touched slot must never be evicted"
            );
        }
        assert_eq!(book.len(), 1);
    }

    #[test]
    fn unrelated_slots_are_evicted_independently() {
        let mut book = SlotBook::<()>::default();
        book.tick();
        book.insert(id(1), ());
        book.insert(id(2), ());
        for _ in 0..(SLOT_EVICT_FRAMES + 1) {
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
