//! Worker↔main transport: the result-ring message enum and the recycled
//! mask/edge payload pool.
//!
//! Landmarks and status cross the result ring as plain values. The 256 KB
//! mask (+ up to 32 KB of edge points) rides in a [`BodyFramePayload`] `Box`
//! cycled through TWO rings: worker→main inside [`BodyWorkerMsg::Frame`]
//! (a pointer move, no copy), main→worker on a dedicated recycle ring after
//! the main thread has copied the bytes out. [`PAYLOAD_POOL_SIZE`] boxes are
//! allocated once at start ([`seed_payload_pool`]); steady state allocates
//! nothing (AGENTS.md hot-path rule — the worker loop is a hot path). If the
//! pool is momentarily dry (main thread stalled), the worker simply emits a
//! payload-less frame: landmarks stay fresh, the mask update skips a frame.

use std::time::Duration;

use bevy::math::Vec3;
use rtrb::Producer;

use super::{
    BodyLandmark, BodyTrackingStatus, EdgePoint, BODY_LANDMARK_COUNT, MASK_SIZE, MAX_EDGE_POINTS,
};

/// Number of pooled mask/edge payloads: one in flight at the worker, one in
/// the result ring, one being consumed on the main thread.
pub const PAYLOAD_POOL_SIZE: usize = 3;

/// Result-ring depth (messages, not frames — status/diagnostics ride along).
pub const RESULT_RING_CAPACITY: usize = 64;

/// A pooled mask + edge-list buffer, reused for the life of the worker.
pub struct BodyFramePayload {
    /// `MASK_SIZE`² `R8Unorm` bytes, written in place by the mask processor.
    pub mask: Vec<u8>,
    /// Edge points for this frame (capacity [`MAX_EDGE_POINTS`], clear-refilled).
    pub edges: Vec<EdgePoint>,
}

impl BodyFramePayload {
    /// Allocate one payload (called only while seeding the pool).
    #[must_use]
    pub fn new() -> Self {
        Self {
            mask: vec![0; MASK_SIZE * MASK_SIZE],
            edges: Vec::with_capacity(MAX_EDGE_POINTS),
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

/// One processed body frame, published by the worker.
pub struct BodyFrame {
    /// Whether a person was tracked in this frame (detector hit while idle,
    /// landmark-confirmed while active).
    pub present: bool,
    /// Track confidence (see `BodyTrackingState::confidence`).
    pub confidence: f32,
    /// Content-normalized landmarks + visibility (unsmoothed; the main
    /// thread's One-Euro pass smooths at poll rate).
    pub landmarks: [BodyLandmark; BODY_LANDMARK_COUNT],
    /// Metric world landmarks (unsmoothed).
    pub world_landmarks: [Vec3; BODY_LANDMARK_COUNT],
    /// Worker-relative capture timestamp.
    pub timestamp: Duration,
    /// Mask + edges, when a pooled buffer was available and the full pipeline
    /// ran (absent for idle detector-only probes and under pool exhaustion).
    pub payload: Option<Box<BodyFramePayload>>,
}

/// A message from the body worker to the main thread.
// The Frame payload (66 landmark/world/velocity vectors) dwarfs Status;
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
///
/// Note: Task 9 adds a `pipeline: PoseDiagnostics` field here once the
/// pipeline module exists; Task 8 (this module) has no pipeline yet, so the
/// struct carries only the worker-loop counters for now.
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
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;

    #[test]
    fn payload_preallocates_mask_and_edge_capacity() {
        let p = BodyFramePayload::new();
        assert_eq!(p.mask.len(), MASK_SIZE * MASK_SIZE);
        assert!(p.edges.is_empty());
        assert_eq!(p.edges.capacity(), MAX_EDGE_POINTS);
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
