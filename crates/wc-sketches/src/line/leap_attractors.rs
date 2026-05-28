//! Per-hand attractor for the Line sketch.
//!
//! Ports v4's `computeLeapAttractorPower` continuous-power model
//! (`.worktrees/v4/src/particles/leapAttractorPower.ts`) onto v5's
//! `TrackedHand` entity model: each tracked hand gets its own
//! [`LineHandAttractor`] component while Line is the active sketch,
//! holding the current power + projected world position. Line's particle
//! stepping collects attractors from this query alongside the singleton
//! `MouseAttractorState`.

use bevy::prelude::*;
use wc_core::input::entity::{GrabStrength, PalmPosition, TrackedHand};
use wc_core::input::projection::palm_to_world;
use wc_core::sketch::sketch_active;

use wc_core::lifecycle::state::AppState;

/// v4 attack-speed for Line's grab-to-power smoothing.
/// (`.worktrees/v4/src/sketches/line/index.ts` `LEAP_POWER_CONFIG`.)
pub const LINE_HAND_ATTACK_SPEED: f32 = 0.005;

/// v4 decay-speed: when grab is below threshold, `power *= 0.5` per frame.
pub const LINE_HAND_DECAY_SPEED: f32 = 0.5;

/// v4 grab threshold: Line responds to any non-zero grab.
pub const LINE_HAND_GRAB_THRESHOLD: f32 = 0.0;

/// Per-hand attractor state. Lives on each [`TrackedHand`] entity while
/// `AppState::Line` is active.
#[derive(Component, Debug, Default, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct LineHandAttractor {
    /// Current attractor power.
    pub power: f32,
    /// World-space position derived from `palm_to_world`.
    pub position: Vec2,
}

/// Marker resource pointing at the entity whose [`LineHandAttractor`]
/// should drive the gravity focal point this frame. Set by
/// `pick_line_focal_hand`; read by particle / post-process code.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct LineFocalHand(pub Option<Entity>);

/// Plugin wiring: attaches the [`LineHandAttractor`] component when Line
/// is active and a new [`TrackedHand`] spawns, removes it on exit, runs
/// the per-frame power + position update system.
pub struct LineLeapAttractorsPlugin;

impl Plugin for LineLeapAttractorsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LineFocalHand>()
            .register_type::<LineHandAttractor>()
            .add_systems(
                Update,
                (
                    ensure_line_attractors,
                    update_line_hand_attractors,
                    pick_line_focal_hand,
                )
                    .chain()
                    .run_if(sketch_active(AppState::Line)),
            )
            .add_systems(OnExit(AppState::Line), detach_all_line_attractors);
    }
}

/// Reconcile pass (runs while Line is the active sketch): attach
/// [`LineHandAttractor`] to every [`TrackedHand`] that doesn't already have it.
///
/// Replaces an earlier `Add<TrackedHand>` observer gated on `AppState::Line`.
/// That observer missed hands that were already being tracked when Line began —
/// hand-tracking runs in `PreUpdate`, *before* the `StateTransition` into Line,
/// so those hands were added while the state was still `Home` and never got an
/// attractor (no gravity pull from a hand held up as you entered the sketch).
/// A `Without<LineHandAttractor>` reconcile is timing-independent and idempotent
/// — see [`crate::line::hand_mesh::ensure_bone_meshes`], which fixes the
/// identical issue for the bone visuals.
fn ensure_line_attractors(
    mut commands: Commands<'_, '_>,
    new_hands: Query<'_, '_, Entity, (With<TrackedHand>, Without<LineHandAttractor>)>,
) {
    for hand in &new_hands {
        commands.entity(hand).insert(LineHandAttractor::default());
    }
}

/// Cleanup: remove `LineHandAttractor` from all entities on Line exit.
fn detach_all_line_attractors(
    mut commands: Commands<'_, '_>,
    query: Query<'_, '_, Entity, (With<TrackedHand>, With<LineHandAttractor>)>,
) {
    for entity in &query {
        commands.entity(entity).remove::<LineHandAttractor>();
    }
}

/// Per-frame: compute the v4 continuous power model and projected world
/// position for each hand's [`LineHandAttractor`].
fn update_line_hand_attractors(
    mut hands: Query<
        '_,
        '_,
        (&PalmPosition, &GrabStrength, &mut LineHandAttractor),
        With<TrackedHand>,
    >,
    window: Single<'_, '_, &Window>,
) {
    let window_size = Vec2::new(window.width(), window.height());

    for (palm, grab, mut attractor) in &mut hands {
        attractor.position = palm_to_world(palm.0, window_size);

        if grab.0 > LINE_HAND_GRAB_THRESHOLD {
            // v4: wanted = grab^1.5 * 5^((-z + 350) / 160)
            let grab_component = grab.0.powf(1.5);
            let depth_modulator = 5.0_f32.powf((-palm.0.z + 350.0) / 160.0);
            let wanted = grab_component * depth_modulator;
            // EMA toward wanted at the attack rate.
            attractor.power = attractor.power * (1.0 - LINE_HAND_ATTACK_SPEED)
                + wanted * LINE_HAND_ATTACK_SPEED;
        } else {
            // v4: power *= decay (geometric decay, no floor for Line).
            attractor.power *= LINE_HAND_DECAY_SPEED;
        }
    }
}

/// Pick the hand entity that drives the gravity focal point this frame.
/// v4's choice was "the first hand the controller reported" — in our
/// entity model that's the lowest-index `Entity`, since Bevy assigns
/// entity ids monotonically.
fn pick_line_focal_hand(
    hands: Query<'_, '_, Entity, (With<TrackedHand>, With<LineHandAttractor>)>,
    mut focal: ResMut<'_, LineFocalHand>,
) {
    focal.0 = hands.iter().min_by_key(|e| e.index());
}
