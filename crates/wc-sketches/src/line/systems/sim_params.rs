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
/// shared [`bake_sim_params`] / [`bake_post_base`] (Condition A1), and writes
/// placeholder `g_constant` / `i_mouse_factor` that `audio_coupling` overrides
/// later in the same frame.
pub fn update_sim_params(
    time: Res<'_, Time>,
    settings: Res<'_, LineSettings>,
    window: Single<'_, '_, &Window>,
    mouse: Res<'_, MouseAttractorState>,
    line_hands: Query<'_, '_, &LineHandAttractor, With<TrackedHand>>,
    mut sim: ResMut<'_, LineSimParams>,
    mut post: ResMut<'_, LinePostParams>,
) {
    // --- Attractor list -------------------------------------------------
    let mut attractors = [Attractor::default(); MAX_ATTRACTORS];
    let mut attractor_count = 0_u32;
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

    // --- Gravity-smear post-process uniforms ---------------------------
    //
    // The post-process shader works in window-pixel space (matches v4's
    // `gl_FragCoord.xy` reference). Particles live in world space centred at
    // the origin (+y up) — `bake_post_base` converts the mouse position back
    // to window-pixel coords (top-left origin, +y down) for `iMouse`.
    bake_post_base(
        &mut post,
        geom,
        mouse.position,
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
}
