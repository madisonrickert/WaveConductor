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

/// `Update` — gated by `sketch_active(AppState::Line)`.
///
/// Populates the attractor array (mouse at index 0 when active), bakes the
/// pulling/inertial drag constants against the v4 fixed-dt, derives the
/// size-scaled gravity multiplier from the window width, and writes the
/// constrain-to-box bounds.
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
            _pad: 0.0,
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
            _pad: 0.0,
        };
        attractor_count += 1;
        slot += 1;
    }

    // --- Drag baking ----------------------------------------------------
    let pulling_drag_baked = V4_PULLING_DRAG_CONSTANT.powf(V4_FIXED_DT);
    let inertial_drag_baked = V4_INERTIAL_DRAG_CONSTANT.powf(V4_FIXED_DT);

    // --- Size scaling (matches v4 sizeScaledGravityConstant) ------------
    let w = window.width();
    let size_scale = (2.0_f32.powf(w / 836.0 - 1.0)).min(1.0);

    // --- Constrain-to-box bounds (centered on origin, matching spawn) ---
    let h = window.height();
    let half_w = w * 0.5;
    let half_h = h * 0.5;
    let constrain_min = [-half_w, -half_h];
    let constrain_max = [half_w, half_h];

    sim.params = SimParams {
        dt: time.delta_secs().min(0.05),
        attractor_count,
        pulling_drag_baked,
        inertial_drag_baked,
        size_scale,
        fade_duration: 3.0, // v4 PARTICLE_SYSTEM_PARAMS.FADE_DURATION
        constrain_min,
        constrain_max,
        _pad: [0.0; 2],
        attractors,
    };

    // --- Gravity-smear post-process uniforms ---------------------------
    //
    // The post-process shader works in window-pixel space (matches v4's
    // `gl_FragCoord.xy` reference). Particles live in world space centred at
    // the origin (+y up) — convert the mouse position back to window-pixel
    // coords (top-left origin, +y down) for `iMouse`.
    post.i_resolution = [w, h];
    post.i_mouse = [
        mouse.position[0] + w * 0.5,
        h - (mouse.position[1] + h * 0.5),
    ];
    // Placeholder defaults for `i_mouse_factor` and `g_constant` — the
    // gated `Update` chain runs `audio_coupling::drive_audio_and_shader`
    // immediately after this system and overrides both fields with the
    // ParticleStats-driven values. The defaults here only become visible if
    // the coupling system is disabled (it never is during normal Line play),
    // but writing sane defaults keeps the resource self-consistent if the
    // chain ever re-orders.
    post.i_mouse_factor = 1.0 / 15.0;
    post.i_global_time = time.elapsed_secs();
    post.g_constant = 5000.0;
    post.gamma = settings.gamma;
}
