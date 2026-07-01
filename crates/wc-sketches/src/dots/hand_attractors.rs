//! Per-hand attractor for the Dots sketch.
//!
//! Ports v4's `computeLeapAttractorPower` continuous-power model with the
//! Dots-specific config (`.worktrees/v4/src/sketches/dots/index.ts`
//! `LEAP_POWER_CONFIG`: `attackSpeed=0.005`, `decaySpeed=0.5`,
//! `grabThreshold=0.1`, `powerFloor=0.05`) onto v5's `TrackedHand` entity
//! model: each tracked hand gets its own [`DotsHandAttractor`] component while
//! Dots is the active sketch, holding the current power and projected world
//! position.
//!
//! The Dots particle sim reads these components in
//! [`super::systems::sim_params::update_dots_sim_params`] and appends them
//! to the shared attractor array after the mouse (mirror of the Line path).
//!
//! # v4 carry-forward
//!
//! v4 shared `computeLeapAttractorPower` and `mapLeapToThreePosition` between
//! Line and Dots (`.worktrees/v4/src/particles/leapAttractorPower.ts` +
//! `.worktrees/v4/src/leap/util.ts`). In v5 the curve and projection are
//! duplicated between `crate::line::leap_attractors` and this module — the
//! Dots config constants differ from Line's, and the two sketches are not yet
//! wired through a shared `particles/` sub-crate. Extract to
//! `wc-sketches/src/particles/` when a third sketch needs the same curve.

use bevy::prelude::*;
use wc_core::input::entity::{GrabStrength, PalmPosition, TrackedHand};
use wc_core::input::projection::palm_to_world;
use wc_core::lifecycle::state::AppState;
use wc_core::sketch::sketch_active;

// ---------------------------------------------------------------------------
// v4 LEAP_POWER_CONFIG constants (Dots-specific)
// ---------------------------------------------------------------------------

/// v4 Dots `LEAP_POWER_CONFIG.attackSpeed`.
/// EMA weight applied to the `wanted` power each frame while grabbing.
pub const DOTS_HAND_ATTACK_SPEED: f32 = 0.005;

/// v4 Dots `LEAP_POWER_CONFIG.decaySpeed`.
/// Per-frame multiplier on `power` when grab is below threshold.
pub const DOTS_HAND_DECAY_SPEED: f32 = 0.5;

/// v4 Dots `LEAP_POWER_CONFIG.grabThreshold`.
/// Grab strength at or below this value is treated as "not grabbing."
pub const DOTS_HAND_GRAB_THRESHOLD: f32 = 0.1;

/// v4 Dots `LEAP_POWER_CONFIG.powerFloor`.
/// When the decayed power falls below this floor it is zeroed out so
/// nearly-released hands don't contribute a residual attractor force.
/// Line omits the floor; Dots adds it (v4 parity).
pub const DOTS_HAND_POWER_FLOOR: f32 = 0.05;

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

/// Per-hand attractor state. Lives on each [`TrackedHand`] entity while
/// `AppState::Dots` is active.
///
/// Mirrors [`crate::line::leap_attractors::LineHandAttractor`].
#[derive(Component, Debug, Default, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct DotsHandAttractor {
    /// Current attractor power, advanced each frame by `dots_leap_power`.
    pub power: f32,
    /// World-space position derived from `dots_palm_to_world`.
    pub position: Vec2,
}

// ---------------------------------------------------------------------------
// Pure functions (the v4 curve + projection)
// ---------------------------------------------------------------------------

/// v4 `computeLeapAttractorPower` with the Dots config baked in.
///
/// # Decay branch (`grab <= DOTS_HAND_GRAB_THRESHOLD`)
///
/// `decayed = power * DOTS_HAND_DECAY_SPEED`. If `decayed <
/// DOTS_HAND_POWER_FLOOR` the function returns `0.0` (floor snap) — Line
/// omits this floor; Dots adds it so hands don't contribute a residual pull
/// indefinitely after the grab releases. Otherwise returns `decayed`.
///
/// # Attack branch (`grab > DOTS_HAND_GRAB_THRESHOLD`)
///
/// ```text
/// wanted = grab^1.5 * 5^((-palm_z + 350) / 160)
/// result = power * (1 - DOTS_HAND_ATTACK_SPEED) + wanted * DOTS_HAND_ATTACK_SPEED
/// ```
///
/// The depth modulator `5^((-z + 350) / 160)` evaluates to `1×` at
/// `z = 350 mm` (the Leap far-plane) and increases as the hand comes
/// closer — matching v4 (`.worktrees/v4/src/particles/leapAttractorPower.ts`).
///
/// # v4 carry-forward
///
/// v4 shared this curve between Line and Dots. In v5 it is duplicated here
/// and in `crate::line::leap_attractors::update_line_hand_attractors` because
/// the two sketches carry different configs (Line has no floor, no threshold).
/// Extract to `particles/` when a third sketch needs it.
pub(crate) fn dots_leap_power(power: f32, grab: f32, palm_z: f32) -> f32 {
    if grab <= DOTS_HAND_GRAB_THRESHOLD {
        let decayed = power * DOTS_HAND_DECAY_SPEED;
        if decayed < DOTS_HAND_POWER_FLOOR {
            return 0.0;
        }
        return decayed;
    }

    // v4: wanted = grab^1.5 * 5^((-z + 350) / 160)
    let grab_component = grab.powf(1.5);
    let depth_modulator = 5.0_f32.powf((-palm_z + 350.0) / 160.0);
    let wanted = grab_component * depth_modulator;
    // EMA toward wanted at the attack rate.
    power * (1.0 - DOTS_HAND_ATTACK_SPEED) + wanted * DOTS_HAND_ATTACK_SPEED
}

/// Project a Leap palm position (mm) to Dots world space.
///
/// Thin wrapper around [`wc_core::input::projection::palm_to_world`] —
/// identical mapping, named separately so Dots owns its projection symbol
/// and a future `particles/` extraction can rename once at a single call site.
///
/// # v4 carry-forward
///
/// v4 shared `mapLeapToThreePosition` between Line and Dots. In v5 both
/// sketches call the same underlying `wc_core` implementation; this wrapper
/// is the Dots-local name. Extract to `particles/` alongside [`dots_leap_power`].
pub(crate) fn dots_palm_to_world(palm_mm: Vec3, window_size: Vec2) -> Vec2 {
    palm_to_world(palm_mm, window_size)
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Plugin wiring: attaches the [`DotsHandAttractor`] component when Dots is
/// active and a new [`TrackedHand`] spawns, removes it on exit, and runs the
/// per-frame power + position update system.
///
/// Mirrors [`crate::line::leap_attractors::LineLeapAttractorsPlugin`].
pub struct DotsLeapAttractorsPlugin;

impl Plugin for DotsLeapAttractorsPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<DotsHandAttractor>().add_systems(
            Update,
            (ensure_dots_attractors, update_dots_hand_attractors)
                .chain()
                .run_if(sketch_active(AppState::Dots)),
        );
        app.add_systems(OnExit(AppState::Dots), detach_all_dots_attractors);
    }
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// Reconcile pass (runs while Dots is the active sketch): attach
/// [`DotsHandAttractor`] to every [`TrackedHand`] that doesn't already have one.
///
/// Timing-independent and idempotent — mirrors
/// `crate::line::leap_attractors::ensure_line_attractors`. The
/// `Without<DotsHandAttractor>` query catches hands that were already being
/// tracked when Dots became active (hand-tracking runs in `PreUpdate`, before
/// the `StateTransition`, so those hands were added before the `OnEnter` and
/// cannot be caught by an `Add<TrackedHand>` observer gated on this state).
fn ensure_dots_attractors(
    mut commands: Commands<'_, '_>,
    new_hands: Query<'_, '_, Entity, (With<TrackedHand>, Without<DotsHandAttractor>)>,
) {
    for hand in &new_hands {
        commands.entity(hand).insert(DotsHandAttractor::default());
    }
}

/// Cleanup: remove [`DotsHandAttractor`] from all entities on Dots exit.
///
/// Mirrors `detach_all_line_attractors`.
fn detach_all_dots_attractors(
    mut commands: Commands<'_, '_>,
    query: Query<'_, '_, Entity, (With<TrackedHand>, With<DotsHandAttractor>)>,
) {
    for entity in &query {
        commands.entity(entity).remove::<DotsHandAttractor>();
    }
}

/// Per-frame: compute the v4 continuous power model and projected world
/// position for each hand's [`DotsHandAttractor`].
///
/// Runs inside the `sketch_active(AppState::Dots)` gate — zero-cost while Dots
/// is idle.
///
/// `pub(crate)` so [`crate::dots::DotsPlugin`] can express the ordering
/// constraint `update_dots_post_params.after(update_dots_hand_attractors)`.
pub(crate) fn update_dots_hand_attractors(
    mut hands: Query<
        '_,
        '_,
        (&PalmPosition, &GrabStrength, &mut DotsHandAttractor),
        With<TrackedHand>,
    >,
    window: Single<'_, '_, &Window>,
) {
    let window_size = Vec2::new(window.width(), window.height());

    for (palm, grab, mut attractor) in &mut hands {
        attractor.position = dots_palm_to_world(palm.0, window_size);
        attractor.power = dots_leap_power(attractor.power, grab.0, palm.0.z);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic float arithmetic is the test subject"
)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    // -----------------------------------------------------------------------
    // dots_leap_power unit tests
    // -----------------------------------------------------------------------

    /// Below-threshold: power decays by `DECAY_SPEED` (×0.5). When the decayed
    /// value falls below `POWER_FLOOR` (0.05) the function zeroes it out.
    ///
    /// v4 test case: `computeLeapAttractorPower(0.08, 0, 350, DOTS_CONFIG)`:
    /// `0.08 × 0.5 = 0.04 < 0.05 (floor)` → returns 0.
    #[test]
    fn below_threshold_decays_and_zeroes_below_floor() {
        // 0.08 * 0.5 = 0.04 < 0.05 floor → zeroed.
        assert_eq!(
            dots_leap_power(0.08, 0.0, 350.0),
            0.0,
            "power decayed to 0.04 which is below floor 0.05; expected 0.0"
        );
    }

    /// Below-threshold decay returns the decayed value when it stays at or
    /// above the power floor.
    ///
    /// v4 test case: `computeLeapAttractorPower(1.0, 0, 350, DOTS_CONFIG)`:
    /// `1.0 × 0.5 = 0.5 >= 0.05 floor` → returns 0.5.
    #[test]
    fn below_threshold_decay_above_floor_is_returned() {
        let result = dots_leap_power(1.0, 0.0, 350.0);
        assert!(
            (result - 0.5).abs() < 1e-6,
            "1.0 * 0.5 = 0.5 (above floor), expected 0.5, got {result}"
        );
    }

    /// Grab at the Dots threshold (0.1) is still treated as "not grabbing."
    ///
    /// v4 test case: `computeLeapAttractorPower(10, 0.05, 350, DOTS_CONFIG)`:
    /// grab 0.05 <= threshold 0.1 → decay branch → `10 * 0.5 = 5.0`.
    #[test]
    fn grab_at_threshold_uses_decay_branch() {
        // grab == DOTS_HAND_GRAB_THRESHOLD (0.1): exactly at threshold, decay branch.
        let at_threshold = dots_leap_power(1.0, DOTS_HAND_GRAB_THRESHOLD, 0.0);
        let expected_decay = 1.0 * DOTS_HAND_DECAY_SPEED;
        assert!(
            (at_threshold - expected_decay).abs() < 1e-6,
            "grab at threshold uses decay branch: expected {expected_decay}, got {at_threshold}"
        );
    }

    /// Above-threshold: EMAs toward `grab^1.5 * 5^((-z+350)/160)`.
    ///
    /// v4 test case: `computeLeapAttractorPower(0, 1.0, 350, DOTS_CONFIG)`:
    /// `wanted = 1.0^1.5 * 5^0 = 1.0`
    /// `result = 0 * 0.995 + 1.0 * 0.005 = 0.005`
    #[test]
    fn above_threshold_ema_toward_wanted() {
        // power=0, grab=1.0, z=350 (depth modulator = 5^0 = 1.0):
        // wanted = 1.0^1.5 * 1.0 = 1.0
        // result = 0.0 * 0.995 + 1.0 * 0.005 = 0.005
        let result = dots_leap_power(0.0, 1.0, 350.0);
        assert!(
            (result - 0.005).abs() < 1e-6,
            "expected 0.005 (one EMA step toward wanted=1.0), got {result}"
        );
    }

    /// Depth modulator: closer palm (lower Z) produces higher wanted power.
    ///
    /// At z=190 (<350): `depth_modulator` = 5^(160/160) = 5 > 1.
    #[test]
    fn closer_palm_increases_wanted_power() {
        let far = dots_leap_power(0.0, 1.0, 350.0); // modulator = 1.0
        let close = dots_leap_power(0.0, 1.0, 190.0); // modulator = 5.0
        assert!(
            close > far,
            "closer palm must yield higher power: close={close}, far={far}"
        );
        // close: wanted = 1.0^1.5 * 5^1 = 5.0; result = 0 * 0.995 + 5 * 0.005 = 0.025
        assert!(
            (close - 0.025).abs() < 1e-6,
            "expected 0.025 (modulator=5 at z=190), got {close}"
        );
    }

    /// Floor boundary: power decayed exactly to the floor (0.05) is NOT zeroed
    /// (the condition is `< floor`, not `<=`).
    #[test]
    fn floor_boundary_not_zeroed() {
        // 0.1 * 0.5 = 0.05 = DOTS_HAND_POWER_FLOOR, not < floor → returned.
        let result = dots_leap_power(0.1, 0.0, 0.0);
        assert!(
            (result - 0.05).abs() < 1e-6,
            "0.1 * 0.5 = 0.05 is at floor, not below: expected 0.05, got {result}"
        );
    }

    // -----------------------------------------------------------------------
    // System-level test: update_dots_hand_attractors feeds DotsHandAttractor
    // -----------------------------------------------------------------------

    /// The `update_dots_hand_attractors` system writes `position` and `power`
    /// to a `DotsHandAttractor` component on a `TrackedHand` entity.
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    fn system_updates_position_and_power_from_palm_and_grab() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);

        // Spawn a window so `Single<&Window>` resolves.
        app.world_mut().spawn(Window {
            resolution: (1280_u32, 720_u32).into(),
            ..Default::default()
        });

        // Spawn a TrackedHand with a full grab at Leap far-plane depth.
        app.world_mut().spawn((
            TrackedHand,
            PalmPosition(Vec3::new(0.0, 195.0, 350.0)),
            GrabStrength(1.0),
            DotsHandAttractor::default(), // power starts at 0.0
        ));

        app.world_mut()
            .run_system_once(update_dots_hand_attractors)
            .expect("update_dots_hand_attractors run");

        let mut q = app.world_mut().query::<&DotsHandAttractor>();
        let attractor = q.single(app.world()).expect("one DotsHandAttractor");

        // power: grab=1.0, z=350 → one EMA step → 0.005.
        assert!(
            (attractor.power - 0.005).abs() < 1e-6,
            "expected power 0.005 after one grab frame, got {}",
            attractor.power
        );
        // position: palm at (0, 195, 350) on a 1280x720 window.
        // palm_to_world: x=0 → world_x=0; y=195 sits between Y_MIN=40 and Y_MAX=350,
        // → near center vertically. Exact value asserted via the shared palm_to_world
        // function directly.
        let expected = palm_to_world(Vec3::new(0.0, 195.0, 350.0), Vec2::new(1280.0, 720.0));
        assert!(
            (attractor.position - expected).length() < 1e-4,
            "expected position {expected:?}, got {:?}",
            attractor.position
        );
    }
}
