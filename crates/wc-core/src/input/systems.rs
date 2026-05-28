//! `PreUpdate` systems for the hand-tracking plugin.
//!
//! Three systems, chained:
//!
//! 1. [`poll_all_providers`] — calls `poll()` on every registered provider,
//!    stamping each emitted frame with the provider's [`super::provider::ProviderId`].
//! 2. [`update_hand_tracking_state`] — folds raw frames into the
//!    [`HandTrackingState`] resource and [`ButtonInput<HandButton>`] resource.
//! 3. [`detect_gestures`] — examines previous-vs-current button state and
//!    emits [`HandGestureEvent`] for each transition.
//!
//! All three run in `PreUpdate` under the same `InputSystems` set Bevy uses
//! for its own input systems, so downstream `Update` consumers see fresh
//! state.

use std::collections::{HashMap, HashSet};

use bevy::input::ButtonInput;
use bevy::prelude::*;
use smallvec::SmallVec;

use super::button::{HandButton, PRESS_THRESHOLD, RELEASE_THRESHOLD};
use super::gesture::HandGestureEvent;
use super::provider::{ProviderId, ProviderRegistry};
use super::state::{FusedHand, FusedHandFrame, HandTrackingFrame, HandTrackingState, MAX_HANDS};
use crate::input::entity::{
    BoneCenters, GrabStrength, HandId, Landmarks, PalmPosition, PalmVelocity, PinchStrength,
    Provenance, TrackedHand,
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

/// Folds raw frames into the [`HandTrackingState`] resource and updates the
/// [`ButtonInput<HandButton>`] resource based on pinch/grab strength
/// crossings.
///
/// Hysteresis: a button is `press`'d when strength rises above
/// [`PRESS_THRESHOLD`], `release`'d when it falls below [`RELEASE_THRESHOLD`].
/// The gap prevents flicker around the boundary.
pub fn update_hand_tracking_state(
    mut reader: MessageReader<'_, '_, HandTrackingFrame>,
    mut state: ResMut<'_, HandTrackingState>,
    mut buttons: ResMut<'_, ButtonInput<HandButton>>,
) {
    // Clear last-frame edge state before processing new events.
    buttons.bypass_change_detection().clear();

    // Process all frames that arrived this tick (typically 1).
    for frame in reader.read() {
        state.ingest(frame);
    }

    // Update button state from the now-current HandTrackingState. We re-derive
    // every frame from continuous strengths rather than tracking edges in the
    // provider — this keeps the truth in one place.
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

/// Components fetched per [`TrackedHand`] entity during update.
/// Named to satisfy `clippy::type_complexity`.
type TrackedHandComponents<'w> = (
    &'w crate::input::hand::Chirality,
    &'w mut PalmPosition,
    &'w mut PalmVelocity,
    &'w mut PinchStrength,
    &'w mut GrabStrength,
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

    for frame in reader.read() {
        for hand in &frame.hands {
            let key = (hand.provider, hand.raw_id);
            seen_this_tick.insert(key);

            if let Some(&entity) = entity_table.get(&key) {
                if let Ok((_chirality, mut palm, mut vel, mut pinch, mut grab, mut lms, mut bones)) =
                    tracked.get_mut(entity)
                {
                    palm.0 = hand.palm_position;
                    vel.0 = hand.palm_velocity;
                    pinch.0 = hand.pinch_strength;
                    grab.0 = hand.grab_strength;
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
                        Landmarks(hand.landmarks),
                        BoneCenters(hand.bone_centers),
                    ))
                    .id();
                entity_table.insert(key, entity);
            }
        }
    }

    // Despawn entities whose key didn't appear in any frame this tick.
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
