//! Per-frame writer for [`crate::line::compute::LineSimParams`].
//!
//! Populates the attractor array (mouse at index 0 when active), bakes the
//! pulling/inertial drag constants against the v4 fixed-dt, derives the
//! size-scaled gravity multiplier from the window width, and writes the
//! constrain-to-box bounds. The render world extracts
//! [`crate::line::compute::LineSimParams`] each frame so the compute shader
//! sees up-to-date values.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "f32 ↔ u32 casts for window-derived sizing are intentional"
)]

use bevy::prelude::*;
use wc_core::input::entity::TrackedHand;

use crate::line::compute::LineSimParams;
use crate::line::leap_attractors::LineHandAttractor;
use crate::line::particle::{Attractor, SimParams, MAX_ATTRACTORS};
use crate::line::post_process::LinePostParams;
use crate::line::settings::LineSettings;
use crate::line::systems::mouse::MouseAttractorState;

/// v4 `PARTICLE_SYSTEM_PARAMS.PULLING_DRAG_CONSTANT`. Baked via
/// `pow(_, V4_FIXED_DT)` to produce the per-frame drag the compute kernel
/// applies whenever at least one attractor has `power > 0`.
///
/// Reproduced verbatim from v4's source so drag parity is bit-identical; the
/// trailing digits exceed f32 precision and are deliberately preserved as the
/// audit trail back to v4's literal.
#[allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    reason = "v4 PARTICLE_SYSTEM_PARAMS drag constants — preserved verbatim for parity"
)]
pub const V4_PULLING_DRAG_CONSTANT: f32 = 0.93075095702;

/// v4 `PARTICLE_SYSTEM_PARAMS.INERTIAL_DRAG_CONSTANT`. Baked the same way as
/// [`V4_PULLING_DRAG_CONSTANT`] and selected when no attractor is active.
#[allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    reason = "v4 PARTICLE_SYSTEM_PARAMS drag constants — preserved verbatim for parity"
)]
pub const V4_INERTIAL_DRAG_CONSTANT: f32 = 0.53913643334;

/// v4's fixed simulation timestep: `0.016 * 2 = 0.032`. We bake drag against
/// this constant (not the render dt) so each per-frame multiplier matches v4
/// regardless of what the renderer is actually doing.
pub const V4_FIXED_DT: f32 = 0.032;

/// v4 `PARTICLE_SYSTEM_PARAMS.FADE_DURATION`. Per-particle fade-in seconds.
pub const V4_FADE_DURATION: f32 = 3.0;

/// Window geometry the param-baker needs. Bundled so the shared
/// [`bake_sim_params`] takes one window argument, and so the screensaver's
/// attract writer (which also has a `Window`) builds it the same way.
#[derive(Clone, Copy, Debug)]
pub struct WindowGeom {
    /// Window width in logical pixels.
    pub width: f32,
    /// Window height in logical pixels.
    pub height: f32,
}

impl WindowGeom {
    /// Read the geometry from a Bevy [`Window`].
    #[must_use]
    pub fn from_window(window: &Window) -> Self {
        Self {
            width: window.width(),
            height: window.height(),
        }
    }
}

/// The live-mode gravity-smear focal point, in world space (centered on the
/// origin, +y up). [`update_sim_params`] updates it each frame: while a hand is
/// grabbing it eases toward the hand centroid (smoothed); otherwise it is set to
/// the mouse cursor directly (instant). [`bake_post_base`] converts it to the
/// shader's window-pixel space for [`LinePostParams::i_mouse`].
///
/// Inserted at [`Vec2::ZERO`] (screen center) in
/// [`crate::line::systems::spawn::spawn_line`] (`OnEnter(AppState::Line)`) and
/// removed in `remove_sim_params` (`OnExit(AppState::Line)`). Deliberately a
/// `Resource`, not a `Local`, so it cannot carry a stale focal across a Line
/// re-entry.
#[derive(Resource, Debug, Clone, Copy)]
pub struct LineSmearFocal(pub Vec2);

/// Attract-mode gate for the per-particle lifetime respawn + fraction kill in
/// `simulate.wgsl`. Only the screensaver's attract writer enables it; the live
/// writer passes [`AttractGate::OFF`] so Active behavior is provably
/// unchanged (the kernel's gated branches never take).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AttractGate {
    /// `true` only while the Line screensaver drives the sim.
    pub enabled: bool,
    /// Survivor fraction `0..=1`: particles whose spawn hash lands at or above
    /// this fade out and stay dead while the gate is enabled. Ignored when
    /// `enabled` is `false`.
    pub fraction: f32,
}

impl AttractGate {
    /// The live (Active-mode) gate: both attract mechanisms off.
    pub const OFF: Self = Self {
        enabled: false,
        fraction: 1.0,
    };
}

/// Attract-mode noise-turbulence parameters for the kernel's divergence-free
/// drift force. Only the screensaver's attract writer supplies a non-zero
/// amplitude; the live writer passes [`Turbulence::OFF`] so the force is
/// provably inert during Active interaction (`turbulence_amp == 0.0` skips the
/// kernel branch entirely).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Turbulence {
    /// Drift speed (world px/s) the curl-noise flow advects positions at.
    /// `0.0` disables the turbulence.
    pub amp: f32,
    /// Spatial frequency of the flow (radians per world unit).
    pub scale: f32,
    /// Animation phase (seconds of elapsed wall-clock).
    pub time: f32,
}

impl Turbulence {
    /// The live (Active-mode) value: turbulence fully off.
    pub const OFF: Self = Self {
        amp: 0.0,
        scale: 0.0,
        time: 0.0,
    };
}

/// **Plan 11.8 Condition A1 (shared bake fn).** Build the full [`SimParams`] for a
/// frame from a baked attractor array, the frame `dt`, and the window geometry.
/// Both the live writer ([`update_sim_params`]) and the screensaver's
/// wandering-pulse writer (`crate::line::screensaver`) call this so the two
/// producers cannot drift in their drag-baking, size-scaling, fade duration, or
/// constrain-box derivation.
///
/// `attractors` carries the already-`gravity_constant`-baked powers (the caller
/// multiplies each attractor's raw power by `gravity_constant` before filling
/// the array, matching the mouse/hand attractor treatment); `attractor_count`
/// is the number of live entries. `dt` is the (uncapped) per-frame delta — the
/// 50 ms cap is applied here. `gate` switches the attract-only lifetime/
/// fraction mechanisms (live writer: [`AttractGate::OFF`]); `turbulence`
/// supplies the attract-only noise-drift force (live writer:
/// [`Turbulence::OFF`]).
#[must_use]
pub fn bake_sim_params(
    dt: f32,
    geom: WindowGeom,
    attractors: [Attractor; MAX_ATTRACTORS],
    attractor_count: u32,
    gate: AttractGate,
    turbulence: Turbulence,
) -> SimParams {
    // --- Drag baking (v4-parity, against the FIXED dt, not render dt) ----
    let pulling_drag_baked = V4_PULLING_DRAG_CONSTANT.powf(V4_FIXED_DT);
    let inertial_drag_baked = V4_INERTIAL_DRAG_CONSTANT.powf(V4_FIXED_DT);

    // --- Size scaling (matches v4 sizeScaledGravityConstant) ------------
    let w = geom.width;
    let h = geom.height;
    let size_scale = (2.0_f32.powf(w / 836.0 - 1.0)).min(1.0);

    // --- Constrain-to-box bounds (centered on origin, matching spawn) ---
    let half_w = w * 0.5;
    let half_h = h * 0.5;

    SimParams {
        dt: dt.min(0.05),
        attractor_count,
        pulling_drag_baked,
        inertial_drag_baked,
        size_scale,
        fade_duration: V4_FADE_DURATION,
        constrain_min: [-half_w, -half_h],
        constrain_max: [half_w, half_h],
        attract_gate: u32::from(gate.enabled),
        attract_fraction: gate.fraction,
        turbulence_amp: turbulence.amp,
        turbulence_scale: turbulence.scale,
        turbulence_time: turbulence.time,
        _turb_pad: 0.0,
        attractors,
    }
}

/// Center-bias weight `W₀` for the smear-focal centroid: a virtual sample
/// pinned at the world origin (screen center). Keeps the focal point defined
/// and smoothly moving when every attractor weight is zero, instead of dividing
/// by ~0 or snapping. Shared by the live writer ([`update_sim_params`]) and the
/// screensaver choreography
/// (`crate::line::screensaver::choreography::attract_frame`) so the two compute
/// the focal identically and cannot drift.
pub const FOCAL_CENTER_WEIGHT: f32 = 0.15;

/// Center-biased, weight-weighted centroid of `(weight, world_pos)` samples:
/// `Σ wᵢ·posᵢ / (Σ wᵢ + center_weight)`. The extra `center_weight` term is a
/// virtual sample at the origin, so the result is always defined and relaxes to
/// `[0, 0]` (screen center, world origin) as the sample weights fall to zero —
/// no divide-by-zero and no pop when the last sample releases.
///
/// Pure and allocation-free; the caller supplies a stack slice.
#[must_use]
pub fn weighted_focal(samples: &[(f32, [f32; 2])], center_weight: f32) -> [f32; 2] {
    let mut weighted = [0.0_f32, 0.0_f32];
    let mut weight_sum = 0.0_f32;
    for &(w, pos) in samples {
        weighted[0] += w * pos[0];
        weighted[1] += w * pos[1];
        weight_sum += w;
    }
    let denom = weight_sum + center_weight;
    // Degenerate guard: with no center bias and no (or net-negative) weights,
    // fall back to screen center rather than dividing by ~0.
    if denom <= 0.0 {
        return [0.0, 0.0];
    }
    [weighted[0] / denom, weighted[1] / denom]
}

/// Frame-rate-independent exponential ease of `current` toward `target` over
/// time constant `tau` seconds: `current + (target − current)·(1 − e^(−dt/τ))`.
///
/// `dt` is capped at 50 ms (matching the sim's `dt.min(0.05)`) so a long pause
/// can't teleport the focal in one frame. `tau <= 0` snaps instantly (α = 1) —
/// the un-smoothed / "off" setting. The discrete form composes exactly, so N
/// small steps land on the same point as one big step for a constant target
/// (the frame-rate-independence guarantee). Pure; operates on `[f32; 2]` so it
/// has no Bevy dependency.
#[must_use]
pub fn ease_focal(current: [f32; 2], target: [f32; 2], dt: f32, tau: f32) -> [f32; 2] {
    let dt = dt.min(0.05);
    let alpha = if tau <= 0.0 {
        1.0
    } else {
        1.0 - (-dt / tau).exp()
    };
    [
        current[0] + (target[0] - current[0]) * alpha,
        current[1] + (target[1] - current[1]) * alpha,
    ]
}

/// **Plan 11.8 Condition A1 (shared post-process base).** Set the geometry- and
/// time-derived fields of [`LinePostParams`] both writers share: `i_resolution`,
/// `i_mouse` (focal point in window-pixel space), `i_global_time`, and `gamma`.
/// `g_constant` / `i_mouse_factor` are left for the caller (live:
/// `audio_coupling`; attract: the choreography) so this fn owns only the
/// truly-shared derivation.
///
/// `focal_world` is the world-space focal point (mouse for the live writer, the
/// pulse-weighted centroid for the attract writer); it is converted to the
/// shader's window-pixel space (top-left origin, +y down) for `i_mouse`.
pub fn bake_post_base(
    post: &mut LinePostParams,
    geom: WindowGeom,
    focal_world: [f32; 2],
    elapsed_secs: f32,
    gamma: f32,
) {
    let w = geom.width;
    let h = geom.height;
    post.i_resolution = [w, h];
    post.i_mouse = [focal_world[0] + w * 0.5, h - (focal_world[1] + h * 0.5)];
    post.i_global_time = elapsed_secs;
    post.gamma = gamma;
}

/// Write the configured smear fringe end-tints into [`LinePostParams`] from
/// `LineSettings`: `end = color.rgb × gain` (HDR — the dominant channel boosts
/// past 1 for the additive glow). Shared by the live (`update_sim_params`) and
/// screensaver writers so the two cannot drift. `w` is padding (0).
pub fn bake_smear_tints(post: &mut LinePostParams, settings: &LineSettings) {
    let gain = settings.smear_chroma_gain.max(0.0);
    let o = settings.smear_outgoing_color;
    let i = settings.smear_incoming_color;
    post.smear_outgoing_tint = [o[0] * gain, o[1] * gain, o[2] * gain, 0.0];
    post.smear_incoming_tint = [i[0] * gain, i[1] * gain, i[2] * gain, 0.0];
}

/// `Update` — gated by `sketch_active(AppState::Line)`.
///
/// Collects the live attractors (mouse + tracked hands), bakes them via the
/// shared [`bake_sim_params`] (Condition A1), and updates the
/// [`LineSmearFocal`]: while a hand is actively grabbing, the focal eases toward
/// the center-biased hand centroid (smoothed, so the gravity smear follows the
/// hand without snapping, relaxing to center as the grab releases); otherwise
/// the mouse cursor drives the focal directly and instantly (the established
/// behavior — the smear tracks the cursor whether or not the button is held).
/// Then bakes the post-process base via [`bake_post_base`] with that focal, and
/// writes placeholder `g_constant` / `i_mouse_factor` that `audio_coupling`
/// overrides later in the same frame.
pub fn update_sim_params(
    time: Res<'_, Time>,
    settings: Res<'_, LineSettings>,
    window: Single<'_, '_, &Window>,
    mouse: Res<'_, MouseAttractorState>,
    line_hands: Query<'_, '_, &LineHandAttractor, With<TrackedHand>>,
    mut sim: ResMut<'_, LineSimParams>,
    mut post: ResMut<'_, LinePostParams>,
    mut focal: ResMut<'_, LineSmearFocal>,
) {
    // --- Attractor list + smear-focal samples ---------------------------
    let mut attractors = [Attractor::default(); MAX_ATTRACTORS];
    let mut attractor_count = 0_u32;

    // Hand-only smear-focal samples (raw hand power pre-`gravity_constant`,
    // world position). The mouse does NOT feed this centroid — the mouse cursor
    // drives the focal directly and instantly below (the established behavior).
    // Fixed-size stack buffer (every attractor slot can be a hand when no mouse
    // is active) — no heap in this per-frame hot path. `focal_count` tracks the
    // live hand entries.
    let mut focal_samples = [(0.0_f32, [0.0_f32, 0.0_f32]); MAX_ATTRACTORS];
    let mut focal_count = 0_usize;

    if mouse.power > 0.0 {
        attractors[0] = Attractor {
            position: mouse.position,
            // Bake `gravity_constant` into the attractor's `power` so the
            // WGSL kernel can treat power uniformly across attractor sources.
            power: mouse.power * settings.gravity_constant,
            // Unbounded pull (v4 parity): no current attractor localizes its radius.
            radius: 0.0,
        };
        attractor_count = 1;
    }

    // Append LineHandAttractor entries after the mouse attractor.
    // Skip very-low-power entries to avoid wasting uniform slots on
    // fully-decayed hands.
    //
    // `slot` tracks the usize index in parallel with `attractor_count` (u32)
    // to avoid a `usize::try_from` / `expect` in the hot path. Both advance
    // in lockstep and are capped at MAX_ATTRACTORS (= 8), which fits in both.
    let mut slot = attractor_count as usize;
    for hand_attractor in &line_hands {
        if hand_attractor.power.abs() <= 1e-2 {
            continue;
        }
        if slot >= MAX_ATTRACTORS {
            break;
        }
        attractors[slot] = Attractor {
            position: hand_attractor.position.to_array(),
            // Bake gravity_constant into power, matching the mouse
            // attractor's treatment.
            power: hand_attractor.power * settings.gravity_constant,
            // Unbounded pull (v4 parity): no current attractor localizes its radius.
            radius: 0.0,
        };
        attractor_count += 1;
        slot += 1;
        // Focal sample uses the RAW hand power (pre-`gravity_constant`), so the
        // center-bias weight stays decoupled from the gravity_constant knob.
        focal_samples[focal_count] = (hand_attractor.power, hand_attractor.position.to_array());
        focal_count += 1;
    }

    // --- Bake via the shared baker (Condition A1) -------------------------
    // `AttractGate::OFF` + `Turbulence::OFF`: the attract-only lifetime respawn,
    // fraction kill, and noise turbulence never run during live interaction.
    let geom = WindowGeom::from_window(&window);
    sim.params = bake_sim_params(
        time.delta_secs(),
        geom,
        attractors,
        attractor_count,
        AttractGate::OFF,
        Turbulence::OFF,
    );

    // --- Smear focal: hands ease toward it, the mouse drives it directly ---
    //
    // Active (grabbing) hands: the focal eases toward their center-biased
    // centroid with a frame-rate-independent exponential filter (τ =
    // `smear_focal_smoothing`), so a moving or jittery hand can't snap the
    // concentric rings; the centroid relaxes smoothly to screen center as the
    // grab releases (no jolt). No active hand: the mouse cursor drives the focal
    // directly and instantly — the established behavior, tracking the cursor
    // whether or not the button is held. (So the smoothing knob governs the hand
    // follow only; the mouse stays instant, as it always was.)
    if focal_count > 0 {
        let target = weighted_focal(&focal_samples[..focal_count], FOCAL_CENTER_WEIGHT);
        focal.0 = Vec2::from(ease_focal(
            focal.0.to_array(),
            target,
            time.delta_secs(),
            settings.smear_focal_smoothing,
        ));
    } else {
        focal.0 = Vec2::from(mouse.position);
    }

    // --- Gravity-smear post-process uniforms ---------------------------
    //
    // The post-process shader works in window-pixel space (matches v4's
    // `gl_FragCoord.xy` reference). Particles live in world space centred at
    // the origin (+y up) — `bake_post_base` converts the eased smear focal back
    // to window-pixel coords (top-left origin, +y down) for `iMouse`.
    bake_post_base(
        &mut post,
        geom,
        focal.0.to_array(),
        time.elapsed_secs(),
        settings.gamma,
    );
    bake_smear_tints(&mut post, &settings);
    // Placeholder defaults for `i_mouse_factor` and `g_constant` — the
    // gated `Update` chain runs `audio_coupling::drive_audio_and_shader`
    // immediately after this system and overrides both fields with the
    // ParticleStats-driven values. The defaults here only become visible if
    // the coupling system is disabled (it never is during normal Line play),
    // but writing sane defaults keeps the resource self-consistent if the
    // chain ever re-orders.
    post.i_mouse_factor = 1.0 / 15.0;
    post.g_constant = 5000.0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use std::time::Duration;

    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    #[allow(
        clippy::float_cmp,
        reason = "the mouse focal is copied verbatim (instant, no ease) — equality is exact"
    )]
    fn update_sim_params_focal_tracks_mouse_cursor_instantly() {
        // No hands and the mouse button NOT held (power 0): the cursor still
        // drives the focal directly and instantly — the established behavior,
        // not gated on an active pull.
        let mut world = World::new();
        world.insert_resource(LineSettings::default());
        world.insert_resource(MouseAttractorState {
            power: 0.0,
            position: [200.0, 100.0],
        });
        world.insert_resource(LineSimParams {
            params: SimParams::default(),
            particles_handle: Handle::default(),
            particle_count: 0,
        });
        world.insert_resource(LinePostParams::default());
        world.insert_resource(LineSmearFocal(Vec2::ZERO));
        let mut time = Time::<()>::default();
        time.advance_by(Duration::from_millis(16));
        world.insert_resource(time);
        world.spawn(Window::default()); // 1280x720 default resolution

        world
            .run_system_once(update_sim_params)
            .expect("update_sim_params run");

        // Focal lands exactly on the cursor (instant copy, no ease, no bias).
        let focal = world.resource::<LineSmearFocal>().0;
        assert_eq!(focal, Vec2::new(200.0, 100.0));

        // It reaches the smear uniform: i_mouse is the focal in window-pixel
        // space (top-left origin, +y down) for a 1280x720 window:
        // [200 + 640, 720 - (100 + 360)] = [840, 260].
        let post = world.resource::<LinePostParams>();
        assert_eq!(post.i_mouse, [840.0, 260.0]);
    }

    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    fn update_sim_params_focal_eases_toward_grabbing_hand() {
        // A grabbing hand (power > 1e-2) drives the focal via the smoothed,
        // center-biased centroid — NOT the mouse. The mouse cursor sits at the
        // origin, so a non-origin focal proves the hand path was taken and the
        // ease moved the focal only partway in one 16 ms step (τ = 0.25).
        let mut world = World::new();
        world.insert_resource(LineSettings::default()); // smear_focal_smoothing = 0.25
        world.insert_resource(MouseAttractorState {
            power: 0.0,
            position: [0.0, 0.0],
        });
        world.insert_resource(LineSimParams {
            params: SimParams::default(),
            particles_handle: Handle::default(),
            particle_count: 0,
        });
        world.insert_resource(LinePostParams::default());
        world.insert_resource(LineSmearFocal(Vec2::ZERO));
        let mut time = Time::<()>::default();
        time.advance_by(Duration::from_millis(16));
        world.insert_resource(time);
        world.spawn(Window::default());
        // One grabbing hand at (200, 100), power 1.0.
        world.spawn((
            TrackedHand,
            LineHandAttractor {
                power: 1.0,
                position: Vec2::new(200.0, 100.0),
            },
        ));

        world
            .run_system_once(update_sim_params)
            .expect("update_sim_params run");

        // Center-biased hand target ≈ [200, 100] / 1.15 ≈ [173.9, 87.0]; one
        // 16 ms ease at τ = 0.25 (α ≈ 0.063) moves the focal partway there —
        // strictly between the origin and the target, and clearly off-origin
        // (so the hand, not the idle mouse at [0,0], drove it).
        let focal = world.resource::<LineSmearFocal>().0;
        assert!(
            focal.x > 1.0 && focal.x < 173.9,
            "x eased toward hand: {}",
            focal.x
        );
        assert!(
            focal.y > 0.5 && focal.y < 87.0,
            "y eased toward hand: {}",
            focal.y
        );
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "test inputs are integer-valued and exactly representable; color × gain is bit-exact"
    )]
    fn bake_smear_tints_scales_color_by_gain() {
        let mut post = LinePostParams::default();
        let settings = LineSettings {
            smear_chroma_gain: 2.0,
            smear_outgoing_color: [0.5, 0.25, 1.0, 1.0],
            smear_incoming_color: [1.0, 0.25, 0.5, 1.0],
            ..LineSettings::default()
        };
        bake_smear_tints(&mut post, &settings);
        assert_eq!(post.smear_outgoing_tint, [1.0, 0.5, 2.0, 0.0]);
        assert_eq!(post.smear_incoming_tint, [2.0, 0.5, 1.0, 0.0]);
    }

    #[test]
    fn bake_smear_tints_default_reproduces_legacy_endtints() {
        // Legacy gravity.wgsl compounded outgoing (0.96,1,1.042) and incoming
        // (1.042,1,0.96) over 11 steps -> end-tints ~ (0.638,1,1.567) / (1.567,1,0.638).
        let mut post = LinePostParams::default();
        bake_smear_tints(&mut post, &LineSettings::default());
        let approx = |a: f32, b: f32| (a - b).abs() < 1e-2;
        assert!(
            approx(post.smear_outgoing_tint[0], 0.638)
                && approx(post.smear_outgoing_tint[2], 1.567),
            "outgoing end-tint should reproduce the legacy blue-shifted trail"
        );
        assert!(
            approx(post.smear_incoming_tint[0], 1.567)
                && approx(post.smear_incoming_tint[2], 0.638),
            "incoming end-tint should reproduce the legacy orange-shifted trail"
        );
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "the shared baker must produce bit-identical shared fields for both writers"
    )]
    fn bake_sim_params_bakes_the_attract_gate() {
        let geom = WindowGeom {
            width: 1280.0,
            height: 720.0,
        };
        let attractors = [Attractor::default(); MAX_ATTRACTORS];

        // Live writer: gate off — both attract mechanisms disabled, turbulence off.
        let live = bake_sim_params(
            0.016,
            geom,
            attractors,
            0,
            AttractGate::OFF,
            Turbulence::OFF,
        );
        assert_eq!(live.attract_gate, 0, "live bake must leave the gate off");
        assert_eq!(
            live.turbulence_amp, 0.0,
            "live bake must leave turbulence off"
        );

        // Attract writer: gate on, fraction passed through verbatim, turbulence on.
        let gate = AttractGate {
            enabled: true,
            fraction: 0.6,
        };
        let turb = Turbulence {
            amp: 12.0,
            scale: 0.012,
            time: 3.5,
        };
        let attract = bake_sim_params(0.016, geom, attractors, 0, gate, turb);
        assert_eq!(attract.attract_gate, 1);
        assert!((attract.attract_fraction - 0.6).abs() < 1e-6);
        assert!((attract.turbulence_amp - 12.0).abs() < 1e-6);
        assert!((attract.turbulence_scale - 0.012).abs() < 1e-6);
        assert!((attract.turbulence_time - 3.5).abs() < 1e-6);

        // Everything the two writers share is identical — the gate is the
        // ONLY difference between live and attract baking (Condition A1).
        assert!((live.pulling_drag_baked - attract.pulling_drag_baked).abs() < 1e-9);
        assert!((live.size_scale - attract.size_scale).abs() < 1e-9);
        assert_eq!(live.constrain_min, attract.constrain_min);
        assert_eq!(live.constrain_max, attract.constrain_max);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "empty input: numerator is exactly 0.0, result is 0.0/center_weight = 0.0 — bit-exact"
    )]
    fn weighted_focal_empty_is_center() {
        assert_eq!(weighted_focal(&[], FOCAL_CENTER_WEIGHT), [0.0, 0.0]);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "zero-weight sample: numerator stays exactly 0.0, result is 0.0/center_weight = 0.0 — bit-exact"
    )]
    fn weighted_focal_zero_weight_is_center() {
        assert_eq!(
            weighted_focal(&[(0.0, [100.0, 50.0])], FOCAL_CENTER_WEIGHT),
            [0.0, 0.0]
        );
    }

    #[test]
    fn weighted_focal_single_sample_sits_on_it_as_center_weight_vanishes() {
        // With no center bias a lone sample sits exactly on its position.
        let f = weighted_focal(&[(10.0, [100.0, 50.0])], 0.0);
        assert!((f[0] - 100.0).abs() < 1e-4);
        assert!((f[1] - 50.0).abs() < 1e-4);
        // With the center bias it sits slightly center-ward (power 10 >> W0).
        let biased = weighted_focal(&[(10.0, [100.0, 50.0])], FOCAL_CENTER_WEIGHT);
        assert!(biased[0] > 98.0 && biased[0] < 100.0);
    }

    #[test]
    fn weighted_focal_is_biased_toward_center() {
        // Two equal-weight samples at x = 100 and x = 200: the unbiased midpoint
        // is 150; the center bias pulls the focal below it (toward 0).
        let f = weighted_focal(
            &[(1.0, [100.0, 0.0]), (1.0, [200.0, 0.0])],
            FOCAL_CENTER_WEIGHT,
        );
        assert!(
            f[0] > 0.0 && f[0] < 150.0,
            "expected center-biased midpoint, got {}",
            f[0]
        );
    }

    #[test]
    fn ease_focal_moves_toward_target() {
        let f = ease_focal([0.0, 0.0], [100.0, 0.0], 0.016, 0.25);
        assert!(
            f[0] > 0.0 && f[0] < 100.0,
            "should ease partway, got {}",
            f[0]
        );
    }

    #[test]
    fn ease_focal_is_framerate_independent() {
        // One step of dt equals two steps of dt/2 for a constant target — the
        // discrete exponential form composes exactly.
        let target = [100.0, 40.0];
        let one = ease_focal([0.0, 0.0], target, 0.02, 0.3);
        let half = ease_focal([0.0, 0.0], target, 0.01, 0.3);
        let two = ease_focal(half, target, 0.01, 0.3);
        assert!((one[0] - two[0]).abs() < 1e-5, "{} vs {}", one[0], two[0]);
        assert!((one[1] - two[1]).abs() < 1e-5, "{} vs {}", one[1], two[1]);
    }

    #[test]
    fn ease_focal_converges_to_center() {
        let mut f = [300.0, 150.0];
        for _ in 0..200 {
            f = ease_focal(f, [0.0, 0.0], 0.016, 0.25);
        }
        assert!(
            f[0].abs() < 0.5 && f[1].abs() < 0.5,
            "should converge to center, got {f:?}"
        );
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "tau<=0 snaps to the target exactly")]
    fn ease_focal_zero_tau_snaps() {
        assert_eq!(
            ease_focal([0.0, 0.0], [100.0, 50.0], 0.016, 0.0),
            [100.0, 50.0]
        );
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "dt cap makes the two calls bit-identical")]
    fn ease_focal_caps_dt() {
        let huge = ease_focal([0.0, 0.0], [100.0, 0.0], 10.0, 0.25);
        let capped = ease_focal([0.0, 0.0], [100.0, 0.0], 0.05, 0.25);
        assert_eq!(huge, capped);
    }
}
