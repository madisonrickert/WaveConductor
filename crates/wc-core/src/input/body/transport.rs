//! Worker↔main transport: the result-ring message enum and the recycled
//! mask/edge payload pool.
//!
//! Per-slot landmarks and status cross the result ring as plain values. The
//! 256 KB RGBA mask (+ up to 32 KB of edge points) rides in a
//! [`BodyFramePayload`] `Box` cycled through TWO rings: worker→main inside
//! [`BodyWorkerMsg::Frame`] (a pointer move, no copy), main→worker on a
//! dedicated recycle ring after the main thread has copied the bytes out.
//! [`PAYLOAD_POOL_SIZE`] boxes are allocated once at start
//! ([`seed_payload_pool`]); steady state allocates nothing (AGENTS.md
//! hot-path rule — the worker loop is a hot path). If the pool is momentarily
//! dry (main thread stalled), the worker simply emits a payload-less frame:
//! landmarks stay fresh, the mask update skips a frame.
//!
//! The payload mask is **RGBA-interleaved** ([`super::MASK_BYTES`]): channel
//! `i` = slot `i`'s coverage, matching the pinned [`super::MaskTexture`]
//! channel convention. Edges are slot-ordered with per-slot counts, matching
//! `SilhouetteEdges`.

use std::time::Duration;

use bevy::math::Vec3;
use rtrb::Producer;

use super::pipeline::PoseDiagnostics;
use super::{
    BodyLandmark, BodyTrackingStatus, EdgePoint, BODY_LANDMARK_COUNT, MASK_BYTES, MAX_EDGE_POINTS,
    MAX_TRACKED_BODIES,
};

/// Number of pooled mask/edge payloads: one in flight at the worker, one in
/// the result ring, one being consumed on the main thread.
pub const PAYLOAD_POOL_SIZE: usize = 3;

/// Result-ring depth (messages, not frames — status/diagnostics ride along).
pub const RESULT_RING_CAPACITY: usize = 64;

/// A pooled mask + edge-list buffer, reused for the life of the worker.
pub struct BodyFramePayload {
    /// [`MASK_BYTES`] RGBA-interleaved bytes (channel `i` = slot `i`),
    /// written in place by the per-slot mask processors.
    pub mask: Vec<u8>,
    /// Slot-ordered edge points for this frame (capacity
    /// [`MAX_EDGE_POINTS`], clear-refilled).
    pub edges: Vec<EdgePoint>,
    /// Per-slot edge counts partitioning `edges` (ascending slot order).
    pub edge_slot_counts: [usize; MAX_TRACKED_BODIES],
}

impl BodyFramePayload {
    /// Allocate one payload (called only while seeding the pool).
    #[must_use]
    pub fn new() -> Self {
        Self {
            mask: vec![0; MASK_BYTES],
            edges: Vec::with_capacity(MAX_EDGE_POINTS),
            edge_slot_counts: [0; MAX_TRACKED_BODIES],
        }
    }
}

impl Default for BodyFramePayload {
    fn default() -> Self {
        Self::new()
    }
}

/// Seed the recycle ring with [`PAYLOAD_POOL_SIZE`] fresh payloads — the only
/// payload allocations of a worker's lifetime.
pub fn seed_payload_pool(recycle: &mut Producer<Box<BodyFramePayload>>) {
    for _ in 0..PAYLOAD_POOL_SIZE {
        // The ring is sized PAYLOAD_POOL_SIZE + 1, so seeding cannot fail;
        // dropping on the impossible error is still safe (just a smaller pool).
        let _ = recycle.push(Box::new(BodyFramePayload::new()));
    }
}

/// One slot's share of a processed frame (unsmoothed; the main thread's
/// per-slot One-Euro pass smooths at poll rate).
#[derive(Clone, Copy, Debug)]
pub struct SlotFrame {
    /// Whether this slot tracked a person this frame (detector hit while
    /// idle, landmark-confirmed while active). On round-robin frames a
    /// skipped-but-active slot stays `true` with its last landmarks held.
    pub present: bool,
    /// Track confidence (see `TrackedBody::confidence`).
    pub confidence: f32,
    /// Content-normalized landmarks + visibility.
    pub landmarks: [BodyLandmark; BODY_LANDMARK_COUNT],
    /// Metric world landmarks.
    pub world_landmarks: [Vec3; BODY_LANDMARK_COUNT],
    /// Fraction of this person's bbox inside the camera frame (`1.0` = fully
    /// visible); see `TrackedBody::crop_fraction`.
    pub crop_fraction: f32,
    /// Normalized bbox area; see `TrackedBody::size`.
    pub size: f32,
}

impl Default for SlotFrame {
    fn default() -> Self {
        Self {
            present: false,
            confidence: 0.0,
            landmarks: [BodyLandmark::default(); BODY_LANDMARK_COUNT],
            world_landmarks: [Vec3::ZERO; BODY_LANDMARK_COUNT],
            crop_fraction: 0.0,
            size: 0.0,
        }
    }
}

/// One processed body frame (all slots), published by the worker.
pub struct BodyFrame {
    /// Per-slot results, indexed by stable slot.
    pub slots: [SlotFrame; MAX_TRACKED_BODIES],
    /// Worker-relative capture timestamp.
    pub timestamp: Duration,
    /// Mask + edges, when a pooled buffer was available and the full pipeline
    /// ran (absent for idle detector-only probes and under pool exhaustion).
    pub payload: Option<Box<BodyFramePayload>>,
}

impl BodyFrame {
    /// Whether any slot tracked a person this frame (the idle-wake signal).
    #[must_use]
    pub fn any_present(&self) -> bool {
        self.slots.iter().any(|s| s.present)
    }
}

/// A message from the body worker to the main thread.
// The Frame payload (4 slots × 66 landmark/world vectors) dwarfs Status;
// boxing it would add a per-frame heap allocation for a 64-entry ring, so the
// size asymmetry is the better trade (same call as the hand worker's msg).
#[allow(clippy::large_enum_variant)]
pub enum BodyWorkerMsg {
    /// One processed frame.
    Frame(BodyFrame),
    /// The inference backend label, sent once after the worker builds its
    /// sessions (models load on the worker thread — see the worker docs).
    Backend(&'static str),
    /// Lifecycle status change.
    Status(BodyTrackingStatus),
    /// Worker/pipeline counters for the most recent processed frame.
    Diagnostics(BodyWorkerDiagnostics),
    /// A pipeline/model error string (rare path — the allocation never
    /// touches the steady-state loop).
    Error(String),
    /// The negotiated camera format label, sent once when the source opens.
    CameraFormat(String),
}

/// Worker-side counters + pipeline diagnostics for one processed frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct BodyWorkerDiagnostics {
    /// Cumulative camera-frame drops (rate cap / idle throttle), distinct
    /// from ring backpressure below — same split as the hand worker.
    pub dropped_frames: u64,
    /// Cumulative result-ring backpressure drops (slow main-thread consumer).
    pub ring_full_drops: u64,
    /// Wall time acquiring + decoding the processed frame.
    pub capture_decode: Duration,
    /// Wall time since the previous processed frame (effective inference
    /// period).
    pub inference_interval: Duration,
    /// Cumulative pipeline (inference) errors.
    pub pipeline_errors: u64,
    /// Whether the idle throttle was requested for this frame.
    pub idle_throttled: bool,
    /// Pipeline-stage metrics for the latest frame.
    pub pipeline: PoseDiagnostics,
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;

    #[test]
    fn payload_preallocates_rgba_mask_and_edge_capacity() {
        let p = BodyFramePayload::new();
        assert_eq!(p.mask.len(), MASK_BYTES, "RGBA-interleaved mask bytes");
        assert!(p.edges.is_empty());
        assert_eq!(p.edges.capacity(), MAX_EDGE_POINTS);
        assert_eq!(p.edge_slot_counts, [0; MAX_TRACKED_BODIES]);
    }

    #[test]
    fn body_frame_any_present_scans_all_slots() {
        let mut frame = BodyFrame {
            slots: [SlotFrame::default(); MAX_TRACKED_BODIES],
            timestamp: Duration::ZERO,
            payload: None,
        };
        assert!(!frame.any_present());
        frame.slots[3].present = true;
        assert!(frame.any_present());
    }

    #[test]
    fn pool_round_trip_reuses_the_same_buffers() {
        // The steady-state contract: after seeding, the same PAYLOAD_POOL_SIZE
        // heap buffers cycle worker→main→worker forever — no new allocation.
        let (mut recycle_tx, mut recycle_rx) =
            rtrb::RingBuffer::<Box<BodyFramePayload>>::new(PAYLOAD_POOL_SIZE + 1);
        seed_payload_pool(&mut recycle_tx);

        let mut seen = std::collections::HashSet::new();
        for cycle in 0..(PAYLOAD_POOL_SIZE * 5) {
            // "Worker": claim a payload, fill it, hand it to "main".
            let mut payload = recycle_rx.pop().expect("pool never runs dry in lockstep");
            seen.insert(payload.mask.as_ptr());
            payload.mask[0] = u8::try_from(cycle % 256).expect("bounded");
            payload.edges.clear();
            // "Main": consume, then recycle.
            recycle_tx.push(payload).expect("recycle ring never full");
        }
        assert_eq!(
            seen.len(),
            PAYLOAD_POOL_SIZE,
            "exactly the seeded buffers must circulate"
        );
    }

    #[test]
    fn pool_exhaustion_is_observable_not_blocking() {
        let (mut recycle_tx, mut recycle_rx) =
            rtrb::RingBuffer::<Box<BodyFramePayload>>::new(PAYLOAD_POOL_SIZE + 1);
        seed_payload_pool(&mut recycle_tx);
        // Drain the whole pool (main thread stalled, nothing recycled)…
        for _ in 0..PAYLOAD_POOL_SIZE {
            let _held = recycle_rx.pop().expect("seeded");
        }
        // …the next claim reports empty instead of blocking or allocating.
        assert!(recycle_rx.pop().is_err());
    }
}
