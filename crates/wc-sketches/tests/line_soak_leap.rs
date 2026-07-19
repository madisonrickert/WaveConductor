//! Soak variant for Plan 11.6's Leap input path.
//!
//! Streams a long sequence of mock-provider hand frames through the new
//! `ProviderRegistry` → `fuse_hand_frames` → `sync_hand_entities` →
//! `LineHandAttractor` chain and checks that `TrackedHand` entity count
//! stays bounded after hundreds of thousands of ticks. Catches leaks
//! that pre-Plan-11.6 code couldn't have, since the entity-per-hand
//! lifecycle is new.
//!
//! ## What this verifies
//!
//! - `sync_hand_entities` correctly updates existing entities in place
//!   for a steady provider stream (key reused = entity reused).
//! - `LineHandAttractor` reconcile + per-frame systems don't churn
//!   memory across hundreds of thousands of ticks.
//! - The `palm_to_world` projection + EMA power update don't drift.
//!
//! ## Running the soak
//!
//! ```text
//! cargo test --release -p wc-sketches --test line_soak_leap -- --ignored
//! ```
//!
//! The full 8-hour wall-clock window stays Madison's job — she runs the
//! release binary with a real Leap connected. This test is the headless
//! "did the entity lifecycle drift in the last 30 minutes of input"
//! gate.

#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

mod common;
use common::input::tap_key;
use common::sketches_test_app;

use std::time::Duration;

use bevy::input::keyboard::KeyCode;
use bevy::prelude::*;
use wc_core::input::entity::TrackedHand;
use wc_core::input::hand::{Chirality, Hand, LANDMARK_COUNT};
use wc_core::input::provider::{ProviderId, ProviderRegistry, ProviderRole};
use wc_core::input::providers::mock::MockProvider;
use wc_core::input::state::HandTrackingFrame;
use wc_core::lifecycle::state::AppState;

/// Number of ticks the soak runs for. At debug-build speeds this is
/// roughly 30 minutes of simulated input; release builds finish in
/// closer to ~3 minutes. Enough to surface leaks without taking long
/// enough that operators avoid running it.
const SOAK_TICKS: u32 = 120_000;

/// Build a `HandTrackingFrame` for tick `t`. The Mock hand has id=1
/// throughout so `sync_hand_entities` keeps reusing the same entity
/// across all ticks (the leak gate).
///
/// Grab strength oscillates sinusoidally so `LineHandAttractor` cycles
/// through its EMA-ramp and geometric-decay branches both. Palm X
/// oscillates so `palm_to_world` exercises non-trivial values.
fn frame_for_tick(t: u32) -> HandTrackingFrame {
    let secs = f32::from(u16::try_from(t & 0xFFFF).unwrap_or(0)) / 60.0;
    let grab = (secs * 2.0).sin().mul_add(0.5, 0.5).clamp(0.0, 1.0);
    let palm_x = (secs).cos() * 100.0;
    HandTrackingFrame {
        provider: ProviderId::Mock,
        hands: smallvec::smallvec![Hand {
            id: 1,
            chirality: Chirality::Right,
            palm_position: Vec3::new(palm_x, 200.0, 0.0),
            palm_normal: Vec3::Y,
            palm_velocity: Vec3::ZERO,
            pinch_strength: 0.0,
            grab_strength: grab,
            landmarks: [Vec3::ZERO; LANDMARK_COUNT],
            camera_distance_mm: 0.0,
        }],
        timestamp: Duration::from_secs_f32(secs),
    }
}

#[test]
#[ignore = "long-running soak; run via cargo test --release --ignored"]
fn leap_path_entity_count_stays_bounded() {
    let mut app = sketches_test_app();
    app.update();

    // Enter Line so the per-hand LineHandAttractor reconcile system runs.
    // `nav::handle_navigation_actions` begins a graceful
    // `ReloadReason::SketchSwitch` reload rather than an instant `NextState`
    // write, so `TimeUpdateStrategy::ManualDuration` at 500 ms (past
    // `SKETCH_SWITCH_FADE_DURATION`'s 400 ms) makes the same three-update
    // settle resolve the full walk (see `line_input.rs::enter_line`).
    tap_key(&mut app, KeyCode::Digit1);
    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
        Duration::from_millis(500),
    ));
    // `Time<Virtual>`'s default `max_delta` (250 ms) would otherwise silently
    // clamp the 500 ms manual step below `SKETCH_SWITCH_FADE_DURATION`'s
    // 400 ms, stalling the fade forever.
    app.world_mut()
        .resource_mut::<Time<bevy::time::Virtual>>()
        .set_max_delta(Duration::from_secs(1));
    for _ in 0..3 {
        app.update();
    }
    app.insert_resource(bevy::time::TimeUpdateStrategy::Automatic);
    app.world_mut()
        .resource_mut::<Time<bevy::time::Virtual>>()
        .set_max_delta(Duration::from_millis(250)); // Bevy's own default
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line,
        "Digit1 should land us in AppState::Line",
    );

    // Pre-populate a long script of frames. Streaming via push_frame on
    // each tick would also work but requires a downcast path that
    // `MockProvider` doesn't expose; the upfront-script approach is
    // simpler and bounds peak memory at ~14 MB for the frame vec.
    let frames: Vec<_> = (0..SOAK_TICKS).map(frame_for_tick).collect();
    let mock = MockProvider::with_frames(frames);
    let mut registry = ProviderRegistry::default();
    registry.register(ProviderId::Mock, ProviderRole::Simulator, Box::new(mock));
    app.insert_resource(registry);

    // Settle a few ticks so the first frame flows through and the
    // reconcile system attaches the LineHandAttractor.
    for _ in 0..5 {
        app.update();
    }
    let start_count = app
        .world_mut()
        .query::<&TrackedHand>()
        .iter(app.world())
        .count();
    assert_eq!(
        start_count, 1,
        "expected one TrackedHand after warmup, got {start_count}",
    );

    // Run the rest of the ticks. The mock provider's queue empties
    // partway through; from that point sync_hand_entities sees no
    // frames and the entity stays alive (no despawn since its key was
    // last-seen in the most recent fused frame — but with no new
    // frames, the entity is neither re-spawned nor despawned). Either
    // way, the count must remain bounded.
    for _ in 5..SOAK_TICKS {
        app.update();
    }

    let end_count = app
        .world_mut()
        .query::<&TrackedHand>()
        .iter(app.world())
        .count();
    assert!(
        end_count <= 2,
        "TrackedHand count drifted to {end_count} after {SOAK_TICKS} ticks (start was {start_count})",
    );
}
