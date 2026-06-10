//! Line attract-mode driver (Plan 11.8, Seam 3).
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
//!    [`choreography::attract_frame`] (the "Wandering Pulses" composition:
//!    three slow Lissajous walkers, each briefly swelling to a gentle
//!    attraction pulse) and bakes it via the shared
//!    [`crate::line::systems::sim_params::bake_sim_params`] (Condition A1 — one
//!    baker, two writers, cannot drift).
//! 2. **`LinePostParams`** — `i_resolution` / `i_mouse` / `i_global_time` /
//!    `gamma` via the shared [`bake_post_base`], plus an activity-scaled
//!    `g_constant` so the gravity smear swells softly with each pulse.
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
/// Builds the attractor array (the wandering pulse points), bakes the sim
/// params via the shared baker (A1), and writes the smear uniforms with an
/// activity-scaled `g_constant` so the gravity smear swells softly with each
/// pulse. The particle count is left at the full spawned value — cooldown is
/// the present rate's job (see module docs), so this driver never touches the
/// dispatch size.
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
    // The smear swells with pulse activity: a faint baseline glow over the
    // settled field (0.10 — gentled from the old design's 0.35 so the particle
    // picture, not the trail echo, dominates at rest), rising to 0.35 at a
    // pulse crest (vs the old 1.0 grab; capture-tuned — 0.60 still blanketed
    // the frame in concentric rings). Scaled into the smear shader's expected
    // magnitude range (matches the live coupling's ~15000 ceiling).
    post.g_constant = (0.10 + 0.25 * frame.activity) * 15_000.0;
    // Soften the per-step pull as activity rises, mirroring the live coupling.
    post.i_mouse_factor = (1.0 / 15.0) / (frame.activity + 1.0);
}

/// Pack the choreography frame's attractors into the GPU `[Attractor; N]` array,
/// returning `(array, live_count)`.
///
/// Each sample's power is baked with `gravity_constant` exactly as the live
/// mouse/hand writers do (A1 parity). Zero-power samples are skipped so they
/// don't consume uniform slots — in the settled field that is *all* of them
/// (zero attractors packed, putting the kernel in inertial drag); only
/// walkers inside their pulse window pack. If a nonzero ambient floor were
/// ever restored, every walker would pack every frame — see
/// [`choreography::AMBIENT_POWER`] for why it must not be. `slot` (usize)
/// and the returned count (u32) advance in lockstep, both capped at
/// `MAX_ATTRACTORS`, avoiding a numeric `as` cast in the loop (workspace
/// `as_conversions` lint).
#[must_use]
fn build_attractor_array(
    frame: &choreography::AttractFrame,
    gravity_constant: f32,
) -> ([Attractor; MAX_ATTRACTORS], u32) {
    let mut attractors = [Attractor::default(); MAX_ATTRACTORS];
    let mut slot = 0_usize;
    for sample in frame.pulses {
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
        // Settled field (t=0): zero ambient, every walker is off — no slots
        // pack, so the kernel sees zero attractors (inertial-drag mode).
        let bounds = choreography::Bounds::from_size(1280.0, 720.0);
        let settled = choreography::attract_frame(0.0, bounds);
        let (_arr, count) = build_attractor_array(&settled, 280.0);
        assert_eq!(count, 0, "settled field packs no attractors");

        // Pulse crest (walker 0's first window midpoint, t = 4.6): exactly
        // one slot packs, baked at peak power × gravity_constant.
        let crest = choreography::attract_frame(4.0 + choreography::PULSE_ON_SECS * 0.5, bounds);
        let (crest_arr, crest_count) = build_attractor_array(&crest, 280.0);
        assert_eq!(crest_count, 1, "only the cresting walker packs");
        assert!(
            (crest_arr[0].power - choreography::PULSE_PEAK_POWER * 280.0).abs() < 1.0,
            "cresting walker bakes peak power × gravity_constant"
        );
    }
}
