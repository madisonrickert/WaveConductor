//! Hand-tracking entity model.
//!
//! Plan 11.6 introduces an entity-per-hand representation so per-hand
//! attached state (`HandMesh` visuals, future per-hand audio voices, future
//! gesture state machines) gets Bevy-native lifecycle: when the hand
//! leaves the tracking volume, the entity despawns and its children go
//! with it.
//!
//! The seam between providers and consumers is `sync_hand_entities`
//! (in `super::systems`), which diffs incoming `FusedHandFrame`s against
//! existing [`TrackedHand`] entities, keyed by `(provider, raw_id)`.
//!
//! ## What sketches consume
//!
//! Sketches that want per-hand behaviour query `Query<&TrackedHand, ...>`
//! and the relevant per-hand components. The `HandTrackingState` resource
//! (mirrored from this query) remains available for systems that prefer
//! the resource idiom — `pointer_merge_system` keeps using it.

use bevy::math::Vec3;
use bevy::prelude::*;
use bevy::reflect::Reflect;

use super::hand::LANDMARK_COUNT;
use super::provider::ProviderId;

/// Marker for any currently-tracked hand entity. Spawned by
/// `sync_hand_entities` when a new `(provider, raw_id)` appears in a fused
/// frame; despawned when that pair disappears.
///
/// `Transform` + `Visibility` are required components so Bevy's hierarchy
/// system has a consistent set of ancestor transform/visibility components
/// when child entities (e.g. `HandMesh` bone spheres in
/// `wc_sketches::line::hand_mesh`) are spawned underneath. Without them,
/// the engine emits a `B0004` warning every tick — and at production
/// volume (one per child entity per tick) the warning storm noticeably
/// stalls the main thread.
#[derive(Component, Debug, Reflect)]
#[reflect(Component)]
#[require(Transform, Visibility)]
pub struct TrackedHand;

/// Provider-local stable identifier. Two consecutive frames with the same
/// `HandId` on the same provider mean "same physical hand". IDs may be
/// reused after a hand leaves the tracking volume.
#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct HandId(pub u32);

/// Source provider + provider-local raw id. Used by `sync_hand_entities`
/// to key its entity-lookup table.
#[derive(Component, Debug, Clone, Copy)]
pub struct Provenance {
    /// Which provider produced this hand.
    pub provider: ProviderId,
    /// Provider-local raw identifier (mirrors `HandId` value).
    pub raw_id: u32,
}

/// Palm centroid in Leap-device coordinates (millimeters).
/// Origin: device center. Axes: +X right, +Y up (away from sensor surface),
/// +Z toward the user (with the device's rounded edge facing the user).
#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct PalmPosition(pub Vec3);

/// Palm velocity in mm/s.
#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct PalmVelocity(pub Vec3);

/// Pinch (thumb-index proximity) in `[0, 1]`.
#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct PinchStrength(pub f32);

/// Grab (fist closure) in `[0, 1]`.
#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct GrabStrength(pub f32);

/// 21-landmark `MediaPipe`-style layout. Filled by the provider.
#[derive(Component, Debug, Clone)]
pub struct Landmarks(pub [Vec3; LANDMARK_COUNT]);

/// 20 bone centers, in finger-then-bone order (5 fingers x 4 bones).
///
/// Used by `HandMesh` rendering. Filled directly from `leaprs::Bone::center()`
/// by `LeaprsProvider`; future `MediaPipe` provider will compute midpoints
/// between consecutive landmarks of the same finger.
#[derive(Component, Debug, Clone)]
pub struct BoneCenters(pub [Vec3; BONE_COUNT]);

/// Number of bones per hand. 5 digits x 4 bones each.
pub const BONE_COUNT: usize = 20;
