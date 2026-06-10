//! `PreUpdate` systems for the hand-tracking plugin.
//!
//! Systems, chained in order:
//!
//! 1. [`poll_all_providers`] — calls `poll()` on every registered provider,
//!    stamping each emitted frame with the provider's [`super::provider::ProviderId`].
//! 2. [`surface_leap_wedge`] — surfaces wedge-state changes as
//!    [`LeapWedgeChanged`] + a log line.
//! 3. [`fuse_hand_frames`] — combines all provider frames into a single
//!    [`super::state::FusedHandFrame`] stream.
//! 4. [`sync_hand_entities`] — diffs fused frames against [`TrackedHand`]
//!    entities, spawning / updating / despawning as needed.
//! 5. [`mirror_state_resource`] — derives the [`HandTrackingState`] resource
//!    and [`ButtonInput<HandButton>`] resource from the entity query each tick.
//! 6. [`detect_gestures`] — examines previous-vs-current button state and
//!    emits [`HandGestureEvent`] for each transition.
//!
//! All systems run in `PreUpdate` under the same `InputSystems` set Bevy uses
//! for its own input systems, so downstream `Update` consumers see fresh
//! state.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bevy::input::ButtonInput;
use bevy::prelude::*;
use smallvec::SmallVec;

use super::button::{HandButton, PRESS_THRESHOLD, RELEASE_THRESHOLD};
use super::gesture::HandGestureEvent;
use super::provider::{ProviderId, ProviderRegistry};
use super::state::{
    FusedHand, FusedHandFrame, HandTrackingFrame, HandTrackingState, PrimaryState, MAX_HANDS,
};
use crate::input::entity::{
    BoneCenters, CameraDistance, GrabStrength, HandId, Landmarks, PalmPosition, PalmVelocity,
    PinchStrength, Provenance, TrackedHand,
};
use crate::input::hand::LandmarkIndex;

/// Calls `poll()` on every registered provider, stamping each emitted
/// frame with the provider's ID before re-emitting it into the shared
/// `Messages<HandTrackingFrame>` stream.
///
/// Runs first in the input chain so subsequent systems see this frame's
/// data in the same tick.
pub fn poll_all_providers(
    time: Res<'_, Time>,
    mut registry: ResMut<'_, ProviderRegistry>,
    mut frames: ResMut<'_, Messages<HandTrackingFrame>>,
) {
    let now = time.elapsed();
    for slot in registry.iter_mut() {
        // Each provider polls into a scratch buffer, then we stamp the
        // provider ID before re-emitting. This avoids requiring every
        // provider to know its own ID.
        let mut scratch = Messages::<HandTrackingFrame>::default();
        slot.inner.poll(now, &mut scratch);
        for mut frame in scratch.drain() {
            frame.provider = slot.id;
            frames.write(frame);
        }
    }
}

/// Edge-triggered change in the primary provider's wedge state. `wedged = true`
/// on entering a wedge, `false` on recovery. A future recovery increment
/// subscribes to this; for now it is consumed only for logging.
#[derive(Message, Debug, Clone, Copy)]
pub struct LeapWedgeChanged {
    /// `true` = just entered a wedge; `false` = just recovered.
    pub wedged: bool,
    /// Monotonic time (`Time::elapsed`) of the transition.
    pub at: Duration,
}

/// Surfaces wedge-state changes: edge-detects `PrimaryState::DeviceWedged` from
/// the primary provider's status and emits [`LeapWedgeChanged`] + a `tracing`
/// line on each transition. Reads the same `primary_status()` the LED reads, so
/// the LED, log, and message can't disagree.
///
/// Runs every `PreUpdate` tick; allocation-free (snapshot read + `Local` compare)
/// and logs edge-only, per the "zero work when idle" budget.
pub fn surface_leap_wedge(
    registry: Res<'_, ProviderRegistry>,
    time: Res<'_, Time>,
    mut wedge_changed: ResMut<'_, Messages<LeapWedgeChanged>>,
    mut was_wedged: Local<'_, bool>,
) {
    let wedged = matches!(
        registry.primary_status().primary(),
        PrimaryState::DeviceWedged
    );
    if wedged == *was_wedged {
        return;
    }
    *was_wedged = wedged;
    wedge_changed.write(LeapWedgeChanged {
        wedged,
        at: time.elapsed(),
    });
    if wedged {
        tracing::warn!(
            "Leap service wedged: device attached but frame stream dead — \
             recovery on macOS is a physical USB replug"
        );
    } else {
        tracing::info!("Leap service recovered: hand-tracking frames resumed");
    }
}

/// Examines `ButtonInput<HandButton>::just_pressed` / `just_released` and
/// emits a [`HandGestureEvent`] for each.
pub fn detect_gestures(
    time: Res<'_, Time>,
    buttons: Res<'_, ButtonInput<HandButton>>,
    mut events: ResMut<'_, Messages<HandGestureEvent>>,
) {
    for button in buttons.get_just_pressed() {
        events.write(HandGestureEvent::Pressed {
            button: *button,
            at: time.elapsed(),
        });
    }
    for button in buttons.get_just_released() {
        events.write(HandGestureEvent::Released {
            button: *button,
            at: time.elapsed(),
        });
    }
}

// ---- helpers ----

fn pick_button(chirality: super::hand::Chirality, is_grab: bool) -> HandButton {
    use super::hand::Chirality;
    match (chirality, is_grab) {
        (Chirality::Left, false) => HandButton::LeftPinch,
        (Chirality::Right, false) => HandButton::RightPinch,
        (Chirality::Left, true) => HandButton::LeftGrab,
        (Chirality::Right, true) => HandButton::RightGrab,
    }
}

fn update_button(buttons: &mut ButtonInput<HandButton>, button: HandButton, strength: f32) {
    let was_pressed = buttons.pressed(button);
    if !was_pressed && strength >= PRESS_THRESHOLD {
        buttons.press(button);
    } else if was_pressed && strength < RELEASE_THRESHOLD {
        buttons.release(button);
    }
}

// ---- Phase 6: fuse_hand_frames + sync_hand_entities ----

/// Fuses incoming [`HandTrackingFrame`]s from all providers into a single
/// [`FusedHandFrame`] stream.
///
/// Plan 11.6: trivial passthrough — exactly one provider is registered
/// in normal operation, and the fused frame is a direct copy with
/// per-hand `provider` tagging.
///
/// Future plans will add per-chirality precedence (Primary > Simulator;
/// Leap > `MediaPipe` among Primary).
pub fn fuse_hand_frames(
    mut reader: MessageReader<'_, '_, HandTrackingFrame>,
    mut writer: MessageWriter<'_, FusedHandFrame>,
) {
    for frame in reader.read() {
        let hands: SmallVec<[FusedHand; MAX_HANDS]> = frame
            .hands
            .iter()
            .map(|h| {
                let bone_centers = bone_centers_from_landmarks(&h.landmarks);
                FusedHand {
                    provider: frame.provider,
                    raw_id: h.id,
                    chirality: h.chirality,
                    palm_position: h.palm_position,
                    palm_velocity: h.palm_velocity,
                    pinch_strength: h.pinch_strength,
                    grab_strength: h.grab_strength,
                    camera_distance_mm: h.camera_distance_mm,
                    landmarks: h.landmarks,
                    bone_centers,
                }
            })
            .collect();
        writer.write(FusedHandFrame {
            hands,
            timestamp: frame.timestamp,
        });
    }
}

/// Derive 20 bone centers from the 21-landmark layout.
///
/// Used as a fallback when a provider hasn't supplied direct bone centers
/// in [`HandTrackingFrame`] (e.g., the mock provider). `LeaprsProvider`
/// supplies them directly in its own frames and short-circuits this path.
///
/// Layout: for each finger (Thumb, Index, Middle, Ring, Pinky), 4 bones —
/// metacarpal, proximal, intermediate, distal — computed as midpoints of
/// the joint pairs:
///
/// - Metacarpal: midpoint(Wrist, MCP)
/// - Proximal:    midpoint(MCP, PIP)
/// - Intermediate: midpoint(PIP, DIP)
/// - Distal:      midpoint(DIP, TIP)
///
/// Thumb edge case: thumb has IP instead of PIP/DIP. We approximate by
/// reusing IP for both, so the bone count stays 4 per finger.
#[allow(
    clippy::similar_names,
    reason = "landmark names like t_ip/t_tip are anatomical abbreviations that must stay this close \
              to the LandmarkIndex variants for readability; renaming them would obscure the mapping"
)]
fn bone_centers_from_landmarks(
    landmarks: &[bevy::math::Vec3; crate::input::hand::LANDMARK_COUNT],
) -> [bevy::math::Vec3; 20] {
    use LandmarkIndex as L;

    let mid = |a: bevy::math::Vec3, b: bevy::math::Vec3| (a + b) * 0.5;

    let wrist = landmarks[L::Wrist.as_index()];

    // Thumb
    let t_cmc = landmarks[L::ThumbCmc.as_index()];
    let t_mcp = landmarks[L::ThumbMcp.as_index()];
    let t_ip = landmarks[L::ThumbIp.as_index()];
    let t_tip = landmarks[L::ThumbTip.as_index()];

    // Index
    let i_mcp = landmarks[L::IndexMcp.as_index()];
    let i_pip = landmarks[L::IndexPip.as_index()];
    let i_dip = landmarks[L::IndexDip.as_index()];
    let i_tip = landmarks[L::IndexTip.as_index()];

    // Middle
    let m_mcp = landmarks[L::MiddleMcp.as_index()];
    let m_pip = landmarks[L::MiddlePip.as_index()];
    let m_dip = landmarks[L::MiddleDip.as_index()];
    let m_tip = landmarks[L::MiddleTip.as_index()];

    // Ring
    let r_mcp = landmarks[L::RingMcp.as_index()];
    let r_pip = landmarks[L::RingPip.as_index()];
    let r_dip = landmarks[L::RingDip.as_index()];
    let r_tip = landmarks[L::RingTip.as_index()];

    // Pinky
    let p_mcp = landmarks[L::PinkyMcp.as_index()];
    let p_pip = landmarks[L::PinkyPip.as_index()];
    let p_dip = landmarks[L::PinkyDip.as_index()];
    let p_tip = landmarks[L::PinkyTip.as_index()];

    [
        // Thumb
        mid(wrist, t_cmc),
        mid(t_cmc, t_mcp),
        mid(t_mcp, t_ip),
        mid(t_ip, t_tip),
        // Index
        mid(wrist, i_mcp),
        mid(i_mcp, i_pip),
        mid(i_pip, i_dip),
        mid(i_dip, i_tip),
        // Middle
        mid(wrist, m_mcp),
        mid(m_mcp, m_pip),
        mid(m_pip, m_dip),
        mid(m_dip, m_tip),
        // Ring
        mid(wrist, r_mcp),
        mid(r_mcp, r_pip),
        mid(r_pip, r_dip),
        mid(r_dip, r_tip),
        // Pinky
        mid(wrist, p_mcp),
        mid(p_mcp, p_pip),
        mid(p_pip, p_dip),
        mid(p_dip, p_tip),
    ]
}

/// Read-only components fetched per [`TrackedHand`] entity during the
/// mirror pass. Named to satisfy `clippy::type_complexity`.
type TrackedHandReadComponents<'w> = (
    &'w HandId,
    &'w crate::input::hand::Chirality,
    &'w PalmPosition,
    &'w PalmVelocity,
    &'w PinchStrength,
    &'w GrabStrength,
    &'w CameraDistance,
    &'w Landmarks,
);

/// Components fetched per [`TrackedHand`] entity during update.
/// Named to satisfy `clippy::type_complexity`.
type TrackedHandComponents<'w> = (
    &'w crate::input::hand::Chirality,
    &'w mut PalmPosition,
    &'w mut PalmVelocity,
    &'w mut PinchStrength,
    &'w mut GrabStrength,
    &'w mut CameraDistance,
    &'w mut Landmarks,
    &'w mut BoneCenters,
);

/// Diff incoming [`FusedHandFrame`]s against existing [`TrackedHand`]
/// entities, keyed by `(provider, raw_id)`. Spawns new entities, updates
/// existing ones in place, despawns ones whose key didn't appear this tick.
///
/// The lookup table is a `Local<HashMap>` rather than a resource — it's
/// system-private state, no other system reads it.
#[allow(
    clippy::implicit_hasher,
    reason = "`Local<HashMap>` is initialized by Bevy's FromWorld; the hasher cannot be injected \
              via a type parameter at a system function call site"
)]
pub fn sync_hand_entities(
    mut commands: Commands<'_, '_>,
    mut entity_table: Local<'_, HashMap<(ProviderId, u32), Entity>>,
    mut reader: MessageReader<'_, '_, FusedHandFrame>,
    mut tracked: Query<'_, '_, TrackedHandComponents<'_>, With<TrackedHand>>,
) {
    let mut seen_this_tick: HashSet<(ProviderId, u32)> = HashSet::new();
    let mut any_frame_received = false;

    for frame in reader.read() {
        any_frame_received = true;
        for hand in &frame.hands {
            let key = (hand.provider, hand.raw_id);
            seen_this_tick.insert(key);

            if let Some(&entity) = entity_table.get(&key) {
                if let Ok((
                    _chirality,
                    mut palm,
                    mut vel,
                    mut pinch,
                    mut grab,
                    mut dist,
                    mut lms,
                    mut bones,
                )) = tracked.get_mut(entity)
                {
                    palm.0 = hand.palm_position;
                    vel.0 = hand.palm_velocity;
                    pinch.0 = hand.pinch_strength;
                    grab.0 = hand.grab_strength;
                    dist.0 = hand.camera_distance_mm;
                    lms.0 = hand.landmarks;
                    bones.0 = hand.bone_centers;
                }
            } else {
                let entity = commands
                    .spawn((
                        TrackedHand,
                        HandId(hand.raw_id),
                        Provenance {
                            provider: hand.provider,
                            raw_id: hand.raw_id,
                        },
                        hand.chirality,
                        PalmPosition(hand.palm_position),
                        PalmVelocity(hand.palm_velocity),
                        PinchStrength(hand.pinch_strength),
                        GrabStrength(hand.grab_strength),
                        CameraDistance(hand.camera_distance_mm),
                        Landmarks(hand.landmarks),
                        BoneCenters(hand.bone_centers),
                    ))
                    .id();
                entity_table.insert(key, entity);
            }
        }
    }

    // Despawn pass: skip when no FusedHandFrame arrived this tick. The Leap
    // provider's poll cadence and the Bevy schedule are independent, so a
    // tick with zero events is normal and means "the hand is still there,
    // we just didn't get a new sample yet" — NOT "the hand disappeared".
    // The despawn signal is an explicitly empty frame (verified by the
    // `tracked_hand_despawns_when_hand_leaves_frame_stream` test, which
    // sends `frame_with(vec![], _)`).
    if !any_frame_received {
        return;
    }

    let stale: Vec<((ProviderId, u32), Entity)> = entity_table
        .iter()
        .filter(|(key, _)| !seen_this_tick.contains(key))
        .map(|(k, e)| (*k, *e))
        .collect();

    for (key, entity) in stale {
        commands.entity(entity).despawn();
        entity_table.remove(&key);
    }
}

/// Each tick, mirror [`HandTrackingState`] (and `ButtonInput<HandButton>`)
/// from the current [`TrackedHand`] entity query.
///
/// This keeps existing resource-style consumers (`pointer_merge_system`,
/// and any future systems that prefer the resource idiom) working without
/// refactor while the new entity model becomes the source of truth.
///
/// Runs after `sync_hand_entities` in the input chain — derives state from
/// queries, not from raw frames.
pub fn mirror_state_resource(
    tracked: Query<'_, '_, TrackedHandReadComponents<'_>, With<TrackedHand>>,
    time: Res<'_, Time>,
    mut state: ResMut<'_, HandTrackingState>,
    mut buttons: ResMut<'_, ButtonInput<HandButton>>,
) {
    use smallvec::SmallVec;

    use crate::input::hand::Hand;

    let now = time.elapsed();

    // Build a fresh frame snapshot from the entity query.
    let mut hands: SmallVec<[Hand; MAX_HANDS]> = SmallVec::new();
    for (id, chirality, palm, vel, pinch, grab, dist, lms) in tracked.iter() {
        hands.push(Hand {
            id: id.0,
            chirality: *chirality,
            palm_position: palm.0,
            // palm_normal isn't tracked per-entity yet — sketches that need
            // it can extend this in a future plan. Default to Vec3::Y.
            palm_normal: bevy::math::Vec3::Y,
            palm_velocity: vel.0,
            pinch_strength: pinch.0,
            grab_strength: grab.0,
            landmarks: lms.0,
            camera_distance_mm: dist.0,
        });
    }
    let frame = HandTrackingFrame {
        // Best-effort tag for the resource view — the resource doesn't
        // expose provenance, so this stamping only matters for ingest()
        // implementations that read frame.provider. None do today.
        provider: ProviderId::Leap,
        hands,
        timestamp: now,
    };
    state.ingest(&frame);

    // Re-derive `ButtonInput<HandButton>` from the just-mirrored state.
    // bypass_change_detection().clear() resets just_pressed/just_released
    // cleanly each frame so threshold-cross events fire on the right tick.
    buttons.bypass_change_detection().clear();
    for hand in state.iter() {
        update_button(
            &mut buttons,
            pick_button(hand.chirality, false),
            hand.pinch_strength,
        );
        update_button(
            &mut buttons,
            pick_button(hand.chirality, true),
            hand.grab_strength,
        );
    }
}

#[cfg(test)]
mod wedge_surface_tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use bevy::prelude::*;

    use super::{surface_leap_wedge, LeapWedgeChanged};
    use crate::input::provider::{
        HandTrackingProvider, ProviderId, ProviderRegistry, ProviderRole,
    };
    use crate::input::state::{
        DevicePresence, HandTrackingError, HandTrackingFrame, ProviderDiagnostics, ProviderStatus,
        ServiceConnection, TrackingFlow,
    };

    /// Test provider whose wedge state is flipped from the test via a shared flag.
    struct StubProvider {
        wedged: Arc<AtomicBool>,
    }

    impl HandTrackingProvider for StubProvider {
        fn start(&mut self) -> Result<(), HandTrackingError> {
            Ok(())
        }
        fn stop(&mut self) {}
        fn poll(&mut self, _now: Duration, _out: &mut Messages<HandTrackingFrame>) {}
        fn status(&self) -> ProviderStatus {
            ProviderStatus {
                service: ServiceConnection::Connected,
                device: DevicePresence::Attached,
                streaming: TrackingFlow::NotStreaming,
                wedged: self.wedged.load(Ordering::Relaxed),
                ..ProviderStatus::default()
            }
        }
        fn diagnostics(&self) -> ProviderDiagnostics {
            ProviderDiagnostics::default()
        }
    }

    fn app_with_stub(flag: Arc<AtomicBool>) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_message::<LeapWedgeChanged>();
        let mut registry = ProviderRegistry::default();
        registry.register(
            ProviderId::Leap,
            ProviderRole::Primary,
            Box::new(StubProvider { wedged: flag }),
        );
        app.insert_resource(registry);
        app.add_systems(Update, surface_leap_wedge);
        app
    }

    fn drain(app: &mut App) -> Vec<LeapWedgeChanged> {
        app.world_mut()
            .resource_mut::<Messages<LeapWedgeChanged>>()
            .drain()
            .collect()
    }

    #[test]
    fn emits_enter_then_clear_edges() {
        let flag = Arc::new(AtomicBool::new(true));
        let mut app = app_with_stub(flag.clone());

        app.update();
        let msgs = drain(&mut app);
        assert_eq!(msgs.len(), 1, "one enter edge");
        assert!(msgs[0].wedged);

        flag.store(false, Ordering::Relaxed);
        app.update();
        let msgs = drain(&mut app);
        assert_eq!(msgs.len(), 1, "one clear edge");
        assert!(!msgs[0].wedged);
    }

    #[test]
    fn no_edge_when_state_unchanged() {
        let flag = Arc::new(AtomicBool::new(false)); // never wedged
        let mut app = app_with_stub(flag);
        app.update();
        app.update();
        assert!(drain(&mut app).is_empty());
    }

    #[test]
    fn no_repeat_emit_while_held_wedged() {
        let flag = Arc::new(AtomicBool::new(true));
        let mut app = app_with_stub(flag);
        app.update(); // tick 1: emits the enter edge
        let _ = drain(&mut app); // consume it
        app.update(); // tick 2: still wedged, no new edge
        assert!(
            drain(&mut app).is_empty(),
            "a held wedge must not re-emit each tick"
        );
    }
}
