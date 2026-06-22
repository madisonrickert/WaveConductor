//! Per-frame writer for [`crate::particles::compute::ParticleSimParams`].
//!
//! Produces the Dots-specific [`crate::particles::particle::SimParams`] each
//! frame: drag constants baked against [`V4_FIXED_DT_DOTS`] (v4 Dots parity),
//! `size_scale` from canvas width (canvas-width multiplier only — gravity is
//! baked into attractor power host-side via `DOTS_GRAVITY_CONSTANT`), stationary
//! spring (`0.01`), and effectively infinite constrain bounds (`constrainToBox =
//! false` in v4). When the mouse attractor is active (`power > 0`), it is
//! written to `attractors[0]` with `power * DOTS_GRAVITY_CONSTANT` baked in.
//! Tracked-hand [`crate::dots::hand_attractors::DotsHandAttractor`] entries are
//! appended after the mouse attractor (same threshold/bake/cap as the Line path).
//!
//! The pure baker [`bake_dots_sim_params`] assembles all [`SimParams`] fields
//! from the attractor array, gate, and turbulence inputs. The live writer
//! [`update_dots_sim_params`] calls it with [`DotsAttractGate::OFF`] and
//! [`DotsTurbulence::OFF`] so the active (non-attract) path is provably
//! unchanged. The coming D6a screensaver driver (Task 2) will call
//! [`bake_dots_sim_params`] directly with live gate and turbulence values.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "f32 casts for window-derived sizing are intentional"
)]

use bevy::prelude::*;
use wc_core::input::entity::TrackedHand;

use crate::dots::hand_attractors::DotsHandAttractor;
use crate::dots::settings::DotsSettings;
use crate::dots::systems::mouse::DotsMouseAttractorState;
use crate::particles::compute::ParticleSimParams;
use crate::particles::particle::{Attractor, SimParams, MAX_ATTRACTORS};

/// v4 Dots fixed simulation timestep. Drag constants are baked against this
/// value (not the render `dt`) so each per-frame multiplier matches v4
/// regardless of actual frame rate — matching Line's `V4_FIXED_DT` convention.
pub const V4_FIXED_DT_DOTS: f32 = 0.048;

/// v4 Dots `PULLING_DRAG_CONSTANT`. Baked via `pow(_, V4_FIXED_DT_DOTS)` to
/// produce the per-frame drag the compute kernel applies when attractors are
/// active. Trailing digits are preserved verbatim from v4 for parity auditing.
#[allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    reason = "v4 Dots parity"
)]
pub const V4_DOTS_PULLING_DRAG: f32 = 0.96075095702;

/// v4 Dots `INERTIAL_DRAG_CONSTANT`. Baked via `pow(_, V4_FIXED_DT_DOTS)`.
/// Stronger damping (closer to 0 when baked) than pulling drag so free
/// particles decelerate quickly while attracted particles stay responsive.
#[allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    reason = "v4 Dots parity"
)]
pub const V4_DOTS_INERTIAL_DRAG: f32 = 0.23913643334;

/// v4 Dots `gravity_constant`. Baked into each attractor's `power` host-side
/// (Task 3 Step 2); declared here so all Dots sim-param constants live in one
/// place. Not used in D2 (no attractors yet).
pub const DOTS_GRAVITY_CONSTANT: f32 = 100.0;

/// Window geometry the param-baker needs. Bundled so [`bake_dots_sim_params`]
/// takes one window argument, and so the screensaver's attract writer (Task 2)
/// builds it the same way. Mirrors Line's [`crate::line::systems::sim_params::WindowGeom`].
#[derive(Clone, Copy, Debug)]
pub struct DotsWindowGeom {
    /// Window width in logical pixels.
    pub width: f32,
    /// Window height in logical pixels.
    pub height: f32,
}

impl DotsWindowGeom {
    /// Read the geometry from a Bevy [`Window`].
    #[must_use]
    pub fn from_window(window: &Window) -> Self {
        Self {
            width: window.width(),
            height: window.height(),
        }
    }
}

/// Attract-mode gate for the per-particle lifetime respawn + fraction kill in
/// `simulate.wgsl`. Only the screensaver's attract writer (D6a Task 2) enables
/// it; the live writer passes [`DotsAttractGate::OFF`] so Active behavior is
/// provably unchanged (the kernel's gated branches never take).
///
/// Mirrors Line's [`crate::line::systems::sim_params::AttractGate`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DotsAttractGate {
    /// `true` only while the Dots screensaver drives the sim.
    pub enabled: bool,
    /// Survivor fraction `0..=1`: particles whose spawn hash lands at or above
    /// this fade out and stay dead while the gate is enabled. Ignored when
    /// `enabled` is `false`.
    pub fraction: f32,
}

impl DotsAttractGate {
    /// The live (Active-mode) gate: both attract mechanisms off.
    pub const OFF: Self = Self {
        enabled: false,
        fraction: 1.0,
    };
}

/// Attract-mode noise-turbulence parameters for the kernel's divergence-free
/// drift force. Only the screensaver's attract writer (D6a Task 2) supplies a
/// non-zero amplitude; the live writer passes [`DotsTurbulence::OFF`] so the
/// force is provably inert during Active interaction (`turbulence_amp == 0.0`
/// skips the kernel branch entirely).
///
/// Mirrors Line's [`crate::line::systems::sim_params::Turbulence`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DotsTurbulence {
    /// Drift speed (world px/s) the curl-noise flow advects positions at.
    /// `0.0` disables the turbulence.
    pub amp: f32,
    /// Spatial frequency of the flow (radians per world unit).
    pub scale: f32,
    /// Animation phase (seconds of elapsed wall-clock).
    pub time: f32,
}

impl DotsTurbulence {
    /// The live (Active-mode) value: turbulence fully off.
    pub const OFF: Self = Self {
        amp: 0.0,
        scale: 0.0,
        time: 0.0,
    };
}

/// Build the full [`SimParams`] for a Dots frame from a baked attractor array,
/// the frame `dt`, the window geometry, attract/turbulence gate inputs, and
/// the linear restoring-spring coefficient.
///
/// Both the live writer ([`update_dots_sim_params`]) and the screensaver's
/// attract driver call this so the two producers cannot drift in their
/// drag-baking, size-scaling, fade duration, or stationary-spring value.
///
/// `attractors` carries the already gravity-constant-baked powers;
/// `count` is the number of live entries. `dt` is the (uncapped) per-frame
/// delta — the 50 ms cap is applied here. `gate` switches the attract-only
/// lifetime/fraction mechanisms (live writer: [`DotsAttractGate::OFF`]);
/// `turbulence` supplies the attract-only noise-drift force (live writer:
/// [`DotsTurbulence::OFF`]). `restoring_linear` is the Hookean spring
/// coefficient; pass `0.0` during screensaver attract so the spring does not
/// fight the turbulence morph.
///
/// Per-call no-allocation guarantee: all arithmetic is on stack scalars; the
/// attractor array is passed by value (fixed-size stack copy).
#[must_use]
pub(crate) fn bake_dots_sim_params(
    dt: f32,
    geom: DotsWindowGeom,
    attractors: [Attractor; MAX_ATTRACTORS],
    count: u32,
    gate: DotsAttractGate,
    turbulence: DotsTurbulence,
    restoring_linear: f32,
) -> SimParams {
    // --- Drag baking (v4 Dots parity, against the fixed Dots dt) ----------
    let pulling_drag_baked = V4_DOTS_PULLING_DRAG.powf(V4_FIXED_DT_DOTS);
    let inertial_drag_baked = V4_DOTS_INERTIAL_DRAG.powf(V4_FIXED_DT_DOTS);

    // --- Size scaling: canvas-width multiplier ONLY -----------------------
    // v4: `min(2^(w / 836 - 1), 1)`. Gravity is baked into attractor power
    // host-side; size_scale carries only the width term, matching Line's
    // `bake_sim_params` convention so `force_mag = a.power * size_scale` is
    // uniform across sketches.
    let size_scale = (2.0_f32.powf(geom.width / 836.0 - 1.0)).min(1.0);

    SimParams {
        // dt: per-frame delta capped at 50 ms — matches Line's convention.
        dt: dt.min(0.05),
        attractor_count: count,
        pulling_drag_baked,
        inertial_drag_baked,
        size_scale,
        // v4 Dots FADE_DURATION = 3.0 seconds per-particle fade-in.
        fade_duration: 3.0,
        // v4 constrainToBox = false: effectively infinite bounds so the
        // OOB→home teleport never fires. Dots grid dots return home via the
        // stationary spring, not via a hard position reset.
        constrain_min: [-1e9, -1e9],
        constrain_max: [1e9, 1e9],
        // Attract-mode gate: 1 when the screensaver drives the sim, 0 otherwise.
        attract_gate: u32::from(gate.enabled),
        attract_fraction: gate.fraction,
        // Turbulence: non-zero only when the screensaver supplies it.
        turbulence_amp: turbulence.amp,
        turbulence_scale: turbulence.scale,
        turbulence_time: turbulence.time,
        // v4 STATIONARY_CONSTANT = 0.01. Each particle is pulled toward its
        // immutable original_xy home. Line passes 0.0 (provable no-op); Dots
        // needs 0.01 so the grid stays anchored and returns home after interaction.
        stationary_constant: 0.01,
        // Linear (Hookean) fabric-tension coefficient. Supplied by the caller:
        // the live writer passes `settings.fabric_tension`; the screensaver
        // always passes 0.0 so the spring does not fight the turbulence morph.
        restoring_linear,
        _spring_pad: [0.0; 3],
        attractors,
    }
}

/// `Update` — gated by `sketch_active(AppState::Dots)`.
///
/// Collects the live attractors (mouse + tracked hands), then delegates
/// full [`SimParams`] assembly to the shared [`bake_dots_sim_params`] baker with
/// [`DotsAttractGate::OFF`] and [`DotsTurbulence::OFF`], so the attract-only
/// lifetime respawn, fraction kill, and noise turbulence never run during live
/// interaction. The output is written to [`ParticleSimParams`] for the render
/// world to extract each frame.
///
/// Per-frame no-allocation guarantee: all arithmetic is on stack scalars; the
/// attractor array is a zero-initialized stack array written in place.
pub fn update_dots_sim_params(
    time: Res<'_, Time>,
    window: Single<'_, '_, &Window>,
    mouse: Res<'_, DotsMouseAttractorState>,
    dots_hands: Query<'_, '_, &DotsHandAttractor, With<TrackedHand>>,
    settings: Res<'_, DotsSettings>,
    mut sim: ResMut<'_, ParticleSimParams>,
) {
    // --- Mouse attractor at index 0 ----------------------------------------
    // When the pointer is active (`power > 0`), bake `settings.gravity_constant`
    // into the attractor's power host-side so the WGSL kernel treats power
    // uniformly across attractor sources (`force_mag = a.power * size_scale`).
    // This mirrors Line's `update_sim_params` convention exactly.
    let mut attractors = [Attractor::default(); MAX_ATTRACTORS];
    let mut attractor_count = 0_u32;
    if mouse.power > 0.0 {
        attractors[0] = Attractor {
            position: mouse.position,
            // Bake gravity_constant into power (host-side), matching the WGSL
            // comment "mouse.power * gravity_constant is already baked into
            // attractor.power host-side". The kernel sees the combined value.
            power: mouse.power * settings.gravity_constant,
            // Unbounded pull (v4 parity: no current Dots attractor localizes its
            // radius; the grid feels a constant-magnitude pull toward the cursor).
            radius: 0.0,
        };
        attractor_count = 1;
    }

    // --- Hand attractors: append after the mouse ---------------------------
    // Skip very-low-power entries to avoid wasting uniform slots on
    // fully-decayed hands. `slot` tracks the usize index in parallel with
    // `attractor_count` (u32) to avoid a `usize::try_from` / `expect` in the
    // hot path. Both advance in lockstep and are capped at MAX_ATTRACTORS (=8),
    // which fits in both types. Mirrors Line's `update_sim_params` loop exactly
    // (same threshold, same gravity bake, same cap).
    //
    // Hand raw power from close full-grab is ~500–2500 vs. the mouse's ~200;
    // `settings.hand_power_scale` (default 0.3) brings the hand bake down
    // toward the mouse feel before the shared gravity_constant multiplier.
    let mut slot = attractor_count as usize;
    for hand_attractor in &dots_hands {
        if hand_attractor.power.abs() <= 1e-2 {
            continue;
        }
        if slot >= MAX_ATTRACTORS {
            break;
        }
        attractors[slot] = Attractor {
            position: hand_attractor.position.to_array(),
            // Bake gravity_constant and hand_power_scale into power, matching
            // the mouse attractor's gravity bake but with the per-source scale
            // to compensate for the hand's higher raw power range.
            power: hand_attractor.power * settings.gravity_constant * settings.hand_power_scale,
            // Unbounded pull (v4 parity).
            radius: 0.0,
        };
        attractor_count += 1;
        slot += 1;
    }

    // --- Bake via the shared baker -----------------------------------------
    // `DotsAttractGate::OFF` + `DotsTurbulence::OFF`: the attract-only lifetime
    // respawn, fraction kill, and noise turbulence never run during live
    // interaction. `settings.fabric_tension` is threaded through as the linear
    // restoring-spring coefficient so the fabric returns crisply after input.
    let geom = DotsWindowGeom::from_window(&window);
    sim.params = bake_dots_sim_params(
        time.delta_secs(),
        geom,
        attractors,
        attractor_count,
        DotsAttractGate::OFF,
        DotsTurbulence::OFF,
        settings.fabric_tension,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use std::time::Duration;

    use crate::dots::settings::DotsSettings;

    /// Helper: insert the resources `update_dots_sim_params` requires.
    fn setup_world(mouse_power: f32, mouse_pos: [f32; 2]) -> World {
        let mut world = World::new();
        world.insert_resource(ParticleSimParams {
            params: SimParams::default(),
            particles_handle: Handle::default(),
            particle_count: 0,
        });
        world.insert_resource(DotsMouseAttractorState {
            power: mouse_power,
            position: mouse_pos,
        });
        // DotsSettings required by update_dots_sim_params (Task 6).
        world.insert_resource(DotsSettings::default());
        let mut time = Time::<()>::default();
        time.advance_by(Duration::from_millis(16));
        world.insert_resource(time);
        // Window::default() gives 1280 × 720.
        world.spawn(Window::default());
        world
    }

    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    #[allow(
        clippy::float_cmp,
        reason = "turbulence_amp is written as literal 0.0 — bit-exact zero comparison is correct"
    )]
    fn update_dots_sim_params_writes_expected_values_no_attractor() {
        let mut world = setup_world(0.0, [0.0, 0.0]);

        world
            .run_system_once(update_dots_sim_params)
            .expect("update_dots_sim_params run");

        let sim = world.resource::<ParticleSimParams>();
        let params = &sim.params;

        assert!(
            (params.stationary_constant - 0.01).abs() < 1e-6,
            "stationary_constant must be 0.01, got {}",
            params.stationary_constant
        );
        assert!(
            (params.fade_duration - 3.0).abs() < 1e-6,
            "fade_duration must be 3.0, got {}",
            params.fade_duration
        );
        assert_eq!(
            params.attractor_count, 0,
            "inactive mouse (power=0) must produce attractor_count=0"
        );
        assert!(
            params.constrain_max[0] >= 1e8,
            "constrain_max must be huge (no OOB reset), got {}",
            params.constrain_max[0]
        );
        assert!(
            params.constrain_min[0] <= -1e8,
            "constrain_min must be huge-negative (no OOB reset), got {}",
            params.constrain_min[0]
        );
        assert_eq!(params.attract_gate, 0, "attract gate must be off in D2");
        assert_eq!(params.turbulence_amp, 0.0, "turbulence must be off in D2");
    }

    /// With an active mouse attractor (power=1.0, position=[5, 5]), the system
    /// must set `attractor_count=1`, bake `power * DOTS_GRAVITY_CONSTANT` into
    /// `attractors[0].power`, and copy the position verbatim.
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    fn active_mouse_attractor_bakes_gravity_into_power() {
        let mut world = setup_world(1.0, [5.0, 5.0]);

        world
            .run_system_once(update_dots_sim_params)
            .expect("update_dots_sim_params run");

        let sim = world.resource::<ParticleSimParams>();
        let params = &sim.params;

        assert_eq!(
            params.attractor_count, 1,
            "active mouse (power=1.0) must produce attractor_count=1"
        );

        // Power baked host-side: 1.0 * DOTS_GRAVITY_CONSTANT = 100.0.
        assert!(
            (params.attractors[0].power - 1.0 * DOTS_GRAVITY_CONSTANT).abs() < 1e-5,
            "attractors[0].power must be 1.0 * DOTS_GRAVITY_CONSTANT = {}, got {}",
            1.0 * DOTS_GRAVITY_CONSTANT,
            params.attractors[0].power
        );

        #[allow(
            clippy::float_cmp,
            reason = "position is copied verbatim from the mouse state integer inputs — bit-exact equality is correct"
        )]
        {
            assert_eq!(
                params.attractors[0].position,
                [5.0, 5.0],
                "attractors[0].position must match the mouse state position"
            );
        }
    }

    #[test]
    fn drag_baking_produces_valid_multipliers() {
        // Baked drag must be in (0, 1): 1.0 means no drag (wrong), 0.0 means
        // instant full stop (wrong). Any physical drag is strictly in between.
        let pulling = V4_DOTS_PULLING_DRAG.powf(V4_FIXED_DT_DOTS);
        let inertial = V4_DOTS_INERTIAL_DRAG.powf(V4_FIXED_DT_DOTS);

        assert!(
            pulling > 0.0 && pulling < 1.0,
            "pulling drag baked = {pulling} must be in (0, 1)"
        );
        assert!(
            inertial > 0.0 && inertial < 1.0,
            "inertial drag baked = {inertial} must be in (0, 1)"
        );
        // Inertial drag is stronger (closer to 0) than pulling drag.
        assert!(
            inertial < pulling,
            "inertial drag {inertial} must be stronger than pulling drag {pulling}"
        );
    }

    #[test]
    fn size_scale_is_at_most_one() {
        // size_scale = min(2^(w/836 - 1), 1) ≤ 1 for all positive widths.
        // It reaches 1.0 at w = 836 px and is capped there for wider windows.
        for w in [400.0_f32, 836.0, 1280.0, 1920.0, 3840.0] {
            let scale = (2.0_f32.powf(w / 836.0 - 1.0)).min(1.0);
            assert!(
                scale > 0.0 && scale <= 1.0,
                "size_scale {scale} out of (0, 1] at width {w}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Hand attractor integration tests (D5 Task 1)
    // -----------------------------------------------------------------------

    use crate::dots::hand_attractors::DotsHandAttractor;
    use wc_core::input::entity::TrackedHand;

    /// Helper: insert the resources + spawn a `TrackedHand` with a
    /// `DotsHandAttractor` at the given power and position.
    fn setup_world_with_hand(mouse_power: f32, hand_power: f32, hand_pos: Vec2) -> World {
        let mut world = setup_world(mouse_power, [0.0, 0.0]);
        world.spawn((
            TrackedHand,
            DotsHandAttractor {
                power: hand_power,
                position: hand_pos,
            },
        ));
        world
    }

    /// Inactive mouse + one hand with power=0.5: the hand is appended to slot 0
    /// with `power * gravity_constant * hand_power_scale` baked in (Task 6).
    /// Default `gravity_constant=100`, `hand_power_scale=0.3` → 0.5 * 100 * 0.3 = 15.
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    fn hand_attractor_appended_after_inactive_mouse() {
        let hand_pos = Vec2::new(100.0, 50.0);
        let mut world = setup_world_with_hand(0.0, 0.5, hand_pos);

        world
            .run_system_once(update_dots_sim_params)
            .expect("update_dots_sim_params run");

        let sim = world.resource::<ParticleSimParams>();
        let params = &sim.params;

        assert_eq!(
            params.attractor_count, 1,
            "one hand attractor must produce attractor_count=1"
        );
        // With default gravity_constant=100 and hand_power_scale=0.3:
        // baked power = 0.5 * 100.0 * 0.3 = 15.0.
        // Verify the hand_power_scale is applied (result is NOT 0.5 * 100 = 50).
        let expected = 0.5_f32 * 100.0 * 0.3;
        assert!(
            (params.attractors[0].power - expected).abs() < 1e-5,
            "hand power baked with hand_power_scale=0.3: expected {expected}, got {}",
            params.attractors[0].power
        );
        #[allow(
            clippy::float_cmp,
            reason = "position is copied verbatim from the hand attractor (integer-valued Vec2) — bit-exact equality is correct"
        )]
        {
            assert_eq!(
                params.attractors[0].position,
                hand_pos.to_array(),
                "hand position copied verbatim to attractor slot"
            );
        }
    }

    /// Active mouse at slot 0 + one hand: mouse goes to slot 0, hand to slot 1;
    /// `attractor_count = 2`. Mouse uses `gravity_constant` only; hand also
    /// applies `hand_power_scale` (Task 6).
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    fn mouse_at_slot_0_hand_at_slot_1() {
        let mut world = setup_world_with_hand(1.0, 0.5, Vec2::new(200.0, 100.0));

        world
            .run_system_once(update_dots_sim_params)
            .expect("update_dots_sim_params run");

        let sim = world.resource::<ParticleSimParams>();
        let params = &sim.params;

        assert_eq!(
            params.attractor_count, 2,
            "mouse + 1 hand must yield attractor_count=2"
        );
        // Mouse at slot 0: baked power = 1.0 * gravity_constant(100) = 100.
        // No hand_power_scale on mouse; mouse and hand use different multipliers.
        let mouse_expected = 1.0_f32 * 100.0;
        assert!(
            (params.attractors[0].power - mouse_expected).abs() < 1e-5,
            "mouse baked power at slot 0: expected {mouse_expected}, got {}",
            params.attractors[0].power
        );
        // Hand at slot 1: baked power = 0.5 * gravity_constant(100) * hand_power_scale(0.3) = 15.
        // Verify scale is applied: result is 15, NOT 0.5 * 100 = 50.
        let hand_expected = 0.5_f32 * 100.0 * 0.3;
        assert!(
            (params.attractors[1].power - hand_expected).abs() < 1e-5,
            "hand baked power at slot 1 with hand_power_scale=0.3: expected {hand_expected}, got {}",
            params.attractors[1].power
        );
    }

    /// Near-zero hand (`power = 0.005`, below the 1e-2 threshold) is skipped.
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    fn near_zero_hand_is_skipped() {
        let mut world = setup_world_with_hand(0.0, 0.005, Vec2::new(0.0, 0.0));

        world
            .run_system_once(update_dots_sim_params)
            .expect("update_dots_sim_params run");

        let sim = world.resource::<ParticleSimParams>();
        assert_eq!(
            sim.params.attractor_count, 0,
            "hand with power=0.005 (below 1e-2 threshold) must be skipped"
        );
    }

    /// Spawning more than `MAX_ATTRACTORS` (8) active hands must clamp
    /// `attractor_count` to exactly `MAX_ATTRACTORS` with no panic or
    /// out-of-bounds write. Exercises the `if slot >= MAX_ATTRACTORS { break; }`
    /// guard in the hand-append loop.
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    fn hand_attractor_count_clamped_at_max_attractors() {
        let mut world = setup_world(0.0, [0.0, 0.0]);
        // Spawn MAX_ATTRACTORS + 2 hands (10 total) all above the power threshold.
        for i in 0..=(MAX_ATTRACTORS + 1) {
            world.spawn((
                TrackedHand,
                DotsHandAttractor {
                    power: 0.5,
                    position: Vec2::new(i as f32 * 10.0, 0.0),
                },
            ));
        }

        world
            .run_system_once(update_dots_sim_params)
            .expect("update_dots_sim_params run");

        let sim = world.resource::<ParticleSimParams>();
        assert_eq!(
            sim.params.attractor_count, MAX_ATTRACTORS as u32,
            "attractor_count must be clamped to MAX_ATTRACTORS={MAX_ATTRACTORS}, got {}",
            sim.params.attractor_count
        );
    }

    // -----------------------------------------------------------------------
    // Baker unit tests (D6a Task 1)
    // -----------------------------------------------------------------------

    /// `bake_dots_sim_params` with gate enabled and turbulence non-zero must
    /// set `attract_gate = 1`, pass `attract_fraction` through verbatim, and
    /// set `turbulence_amp/scale/time` from the turbulence input. All fields
    /// shared between the live and attract paths must be identical.
    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "the shared baker must produce bit-identical shared fields for both callers"
    )]
    fn bake_dots_sim_params_bakes_the_attract_gate() {
        let geom = DotsWindowGeom {
            width: 1280.0,
            height: 720.0,
        };
        let attractors = [Attractor::default(); MAX_ATTRACTORS];

        // Live caller: gate off — both attract mechanisms disabled, turbulence off.
        // restoring_linear = 0.0 (used only to satisfy the new signature; the
        // live writer passes settings.fabric_tension in production).
        let live = bake_dots_sim_params(
            0.016,
            geom,
            attractors,
            0,
            DotsAttractGate::OFF,
            DotsTurbulence::OFF,
            0.0,
        );
        assert_eq!(live.attract_gate, 0, "live bake must leave the gate off");
        assert_eq!(
            live.turbulence_amp, 0.0,
            "live bake must leave turbulence off"
        );

        // Attract caller: gate on, fraction and turbulence passed through verbatim.
        // The screensaver always passes restoring_linear = 0.0 so the spring
        // does not fight the turbulence morph.
        let gate = DotsAttractGate {
            enabled: true,
            fraction: 0.6,
        };
        let turb = DotsTurbulence {
            amp: 1.0,
            scale: 0.012,
            time: 3.5,
        };
        let attract = bake_dots_sim_params(0.016, geom, attractors, 0, gate, turb, 0.0);
        assert_eq!(attract.attract_gate, 1, "attract bake must set the gate");
        assert!(
            (attract.attract_fraction - 0.6).abs() < 1e-6,
            "attract_fraction must be gate.fraction"
        );
        assert!(
            (attract.turbulence_amp - 1.0).abs() < 1e-6,
            "turbulence_amp must be turb.amp"
        );
        assert!(
            (attract.turbulence_scale - 0.012).abs() < 1e-6,
            "turbulence_scale must be turb.scale"
        );
        assert!(
            (attract.turbulence_time - 3.5).abs() < 1e-6,
            "turbulence_time must be turb.time"
        );

        // Everything the two callers share is identical — the gate/turbulence
        // inputs are the ONLY difference between live and attract baking.
        assert!(
            (live.pulling_drag_baked - attract.pulling_drag_baked).abs() < 1e-9,
            "pulling_drag_baked must be caller-independent"
        );
        assert!(
            (live.size_scale - attract.size_scale).abs() < 1e-9,
            "size_scale must be caller-independent"
        );
        assert_eq!(live.constrain_min, attract.constrain_min);
        assert_eq!(live.constrain_max, attract.constrain_max);
        assert!(
            (live.stationary_constant - attract.stationary_constant).abs() < 1e-9,
            "stationary_constant must be caller-independent"
        );
        assert!(
            (live.restoring_linear - attract.restoring_linear).abs() < 1e-9,
            "restoring_linear must be caller-independent when both pass 0.0"
        );
    }

    // -----------------------------------------------------------------------
    // Fabric tension / gravity / hand-power-scale threading tests (Task 6)
    // -----------------------------------------------------------------------

    /// With `fabric_tension = 2.0` in `DotsSettings`, `update_dots_sim_params`
    /// must write `restoring_linear = 2.0` to `SimParams`. Confirms the setting
    /// is threaded live from settings → baker → GPU struct.
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    fn fabric_tension_setting_writes_restoring_linear() {
        let mut world = setup_world(0.0, [0.0, 0.0]);
        // Override fabric_tension to a non-default value so the assertion
        // is meaningful (default is 1.0, not 0.0).
        {
            let mut settings = world.resource_mut::<DotsSettings>();
            settings.fabric_tension = 2.0;
        }

        world
            .run_system_once(update_dots_sim_params)
            .expect("update_dots_sim_params run");

        let params = &world.resource::<ParticleSimParams>().params;
        assert!(
            (params.restoring_linear - 2.0).abs() < 1e-6,
            "restoring_linear must equal fabric_tension=2.0, got {}",
            params.restoring_linear
        );
    }

    /// The screensaver baker always receives `restoring_linear = 0.0` so the
    /// Hookean spring does not fight the turbulence morph during attract mode.
    /// Tested via a direct baker call (the screensaver system is in its own
    /// module; see `screensaver.rs` tests for the full system assertion).
    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "restoring_linear is written as literal 0.0 — bit-exact comparison is correct"
    )]
    fn screensaver_baker_receives_zero_restoring_linear() {
        let geom = DotsWindowGeom {
            width: 1280.0,
            height: 720.0,
        };
        let attractors = [Attractor::default(); MAX_ATTRACTORS];
        let gate = DotsAttractGate {
            enabled: true,
            fraction: 0.6,
        };
        let turb = DotsTurbulence {
            amp: 6.0,
            scale: 0.012, // DOTS_TURBULENCE_SCALE from screensaver.rs
            time: 0.0,
        };
        // Screensaver always passes 0.0 for restoring_linear.
        let attract = bake_dots_sim_params(0.016, geom, attractors, 0, gate, turb, 0.0);
        assert_eq!(
            attract.restoring_linear, 0.0,
            "screensaver bake must produce restoring_linear=0.0"
        );
    }
}
