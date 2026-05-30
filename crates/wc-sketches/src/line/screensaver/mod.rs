//! Line attract-mode driver (Plan 12, Seam 3).
//!
//! Drives the **real** Line pipeline from synthetic attractors while the
//! screensaver is showing, so the attract visual is unmistakably the actual
//! sketch (Approach A, spec §9). The single system here is gated on
//! `in_screensaver(AppState::Line)` so it runs only during Line's attract mode
//! and nowhere else (AGENTS.md "zero systems when idle").
//!
//! ## What it writes
//!
//! 1. **`LineSimParams.params`** — the normal `update_sim_params` writer is
//!    gated on `Active` and does not run here, so this is the param *producer*
//!    during attract. It builds the attractor array from
//!    [`choreography::attract_frame`] (dream wanderers + invitation-pulse
//!    phantom hands) and bakes it via the shared
//!    [`crate::line::systems::sim_params::bake_sim_params`] (Condition A1 — one
//!    baker, two writers, cannot drift).
//! 2. **`LinePostParams`** — `i_resolution` / `i_mouse` / `i_global_time` /
//!    `gamma` via the shared [`bake_post_base`], plus a pulse-scaled
//!    `g_constant` so the gravity smear breathes with the invitation pulse.
//!
//! ## Thermal cooldown is the present-rate's job, not this driver's ("Low-Rate Ember")
//!
//! This driver is **thermal-tier-agnostic**: the same choreography runs at every
//! tier, and cooldown is applied centrally by the framework's per-tier
//! present-rate throttle (Seam 2: Cool ≈ 30 fps, Warm ≈ 15 fps, Hot ≈ 3 fps via
//! winit `UpdateMode::Reactive`). At the Hot tier the full pipeline therefore
//! runs ~1/10th as often as Cool — a large, real GPU/CPU cooldown — while the
//! particle alpha still fades in normally, so Hot is a calm, slowly-*breathing*
//! "resting ember", never a black screen. The choreography is a function of
//! wall-clock `time.elapsed_secs()`, so the pulse still completes in its real
//! period; only the present cadence drops.
//!
//! ### Why not freeze the compute dispatch at Hot?
//!
//! An earlier design set `particle_count = 0` at Hot to dispatch zero compute
//! workgroups. That black-screens: particles are CPU-seeded with `alpha = 0`
//! (`spawn.rs`) and alpha *only* rises inside the compute shader
//! (`simulate.wgsl`), so a never-dispatched buffer stays fully transparent — a
//! dead black frame, the worst outcome for an attract loop, and exactly what was
//! observed on cold-start-into-Hot. Present-rate throttling delivers the bulk of
//! the cooldown (≈10×) without that failure mode and without the
//! freeze/cache/restore state machine. A true dispatch freeze remains a possible
//! *future* escalation — but only as a **warm-up-then-freeze** (run the dispatch
//! until alpha saturates, *then* latch it off), gated on 8-hour-soak telemetry
//! showing that 3 fps + the Leap idle-pause (Seam 4) still runs the NUC too hot.
//! Until that evidence exists, YAGNI: ship the ember.

pub mod choreography;

use bevy::prelude::*;
use wc_core::lifecycle::screensaver::in_screensaver;
use wc_core::lifecycle::state::AppState;

use crate::line::compute::LineSimParams;
use crate::line::particle::{Attractor, MAX_ATTRACTORS};
use crate::line::post_process::LinePostParams;
use crate::line::settings::LineSettings;
use crate::line::systems::sim_params::{bake_post_base, bake_sim_params, WindowGeom};

/// Plugin wiring the Line attract driver.
pub struct LineScreensaverPlugin;

impl Plugin for LineScreensaverPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            drive_line_attract.run_if(in_screensaver(AppState::Line)),
        );
    }
}

/// Drive `LineSimParams.params` + `LinePostParams` from the choreography frame.
///
/// Builds the attractor array (dream wanderers + phantom hands), bakes the sim
/// params via the shared baker (A1), and writes the smear uniforms with a
/// pulse-scaled `g_constant` so the gravity smear breathes with the pulse. The
/// particle count is left at the full spawned value — cooldown is the present
/// rate's job (see module docs), so this driver never touches the dispatch size.
fn drive_line_attract(
    time: Res<'_, Time>,
    settings: Res<'_, LineSettings>,
    window: Single<'_, '_, &Window>,
    mut sim: ResMut<'_, LineSimParams>,
    mut post: ResMut<'_, LinePostParams>,
) {
    let geom = WindowGeom::from_window(&window);
    let bounds = choreography::Bounds::from_size(geom.width, geom.height);
    let frame = choreography::attract_frame(time.elapsed_secs(), bounds);

    // --- Bake the sim params via the shared baker (Condition A1). ---------
    let (attractors, count) = build_attractor_array(&frame, settings.gravity_constant);
    sim.params = bake_sim_params(time.delta_secs(), geom, attractors, count);

    // --- Gravity-smear uniforms: shared base + pulse-scaled g_constant. ---
    bake_post_base(
        &mut post,
        geom,
        frame.focal_world,
        time.elapsed_secs(),
        settings.gamma,
    );
    // The smear breathes with the invitation pulse: a calm baseline glow in the
    // resting dream (0.35), swelling as the hands grab. Scaled into the smear
    // shader's expected magnitude range (matches the live coupling's ~15000
    // ceiling).
    post.g_constant = (0.35 + 0.65 * frame.pulse) * 15_000.0;
    // Soften the per-step pull as the pulse rises, mirroring the live coupling.
    post.i_mouse_factor = (1.0 / 15.0) / (frame.pulse + 1.0);
}

/// Pack the choreography frame's attractors into the GPU `[Attractor; N]` array,
/// returning `(array, live_count)`.
///
/// Order: dream wanderers first, then phantom hands. Each sample's power is baked
/// with `gravity_constant` exactly as the live mouse/hand writers do (A1 parity).
/// Zero-power samples (inactive hands in the resting dream) are skipped so they
/// don't consume uniform slots. `slot` (usize) and the returned count (u32)
/// advance in lockstep, both capped at `MAX_ATTRACTORS`, avoiding a numeric `as`
/// cast in the loop (workspace `as_conversions` lint).
#[must_use]
fn build_attractor_array(
    frame: &choreography::AttractFrame,
    gravity_constant: f32,
) -> ([Attractor; MAX_ATTRACTORS], u32) {
    let mut attractors = [Attractor::default(); MAX_ATTRACTORS];
    let mut slot = 0_usize;
    for sample in frame.dreamers.into_iter().chain(frame.hands) {
        if slot >= MAX_ATTRACTORS || sample.power <= 0.0 {
            continue;
        }
        attractors[slot] = Attractor {
            position: sample.position,
            power: sample.power * gravity_constant,
            _pad: 0.0,
        };
        slot += 1;
    }
    (attractors, u32::try_from(slot).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_attractor_array_packs_active_samples_and_bakes_gravity() {
        // Resting dream (t=0): 2 dream wanderers active, hands at zero power.
        let bounds = choreography::Bounds::from_size(1280.0, 720.0);
        let dream = choreography::attract_frame(0.0, bounds);
        let (arr, count) = build_attractor_array(&dream, 280.0);
        assert_eq!(count, 2, "only the two dreamers are active in the dream");
        // Power baked with gravity_constant (dreamer raw power × 280).
        assert!(arr[0].power > 0.0);

        // Peak grab: dreamers + 2 phantom hands all active.
        let grab = choreography::attract_frame(choreography::PULSE_PERIOD_SECS * 0.5, bounds);
        let (_arr, grab_count) = build_attractor_array(&grab, 280.0);
        assert_eq!(grab_count, 4, "dreamers + two phantom hands at the grab");
    }
}
