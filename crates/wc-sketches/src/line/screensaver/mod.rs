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
//!    [`choreography::attract_frame`] (the "Wandering Pulses" composition: three
//!    slow Lissajous walkers, each briefly swelling to a gentle attraction
//!    pulse) and supplies the noise-turbulence parameters that do the
//!    continuous slow-morph drift, baking it all via the shared
//!    [`crate::line::systems::sim_params::bake_sim_params`] (Condition A1 — one
//!    baker, two writers, cannot drift).
//! 2. **`LinePostParams`** — `i_resolution` / `i_mouse` / `i_global_time` /
//!    `gamma` via the shared [`bake_post_base`], plus a `g_constant` at the
//!    settled-field breathing baseline. The smear is deliberately kept
//!    **decoupled from the walkers** (focal pinned to screen centre, no
//!    pulse-driven swell) so the pulses can no longer jolt the gravity shader.
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
use bevy::sprite_render::MeshMaterial2d;
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;
use wc_core::lifecycle::screensaver::in_screensaver;
use wc_core::lifecycle::state::AppState;
use wc_core::sketch::sketch_active;

use crate::line::compute::LineSimParams;
use crate::line::material::LineMaterial;
use crate::line::particle::{Attractor, MAX_ATTRACTORS};
use crate::line::post_process::LinePostParams;
use crate::line::settings::LineSettings;
use crate::line::systems::sim_params::{
    bake_post_base, bake_sim_params, bake_smear_tints, AttractGate, Turbulence, WindowGeom,
};
use crate::line::LineRoot;

/// Spatial frequency (radians per world unit) of the turbulence flow's base
/// octave. ~0.012 ≈ a 520-px primary swirl wavelength (the second octave is
/// half that), so the swirls are broad and slow rather than busy.
const TURBULENCE_SCALE: f32 = 0.012;

/// Plugin wiring the Line attract driver.
pub struct LineScreensaverPlugin;

impl Plugin for LineScreensaverPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            drive_line_attract.run_if(in_screensaver(AppState::Line)),
        );
        // The velocity-color strength follows the ScreensaverFade envelope,
        // which ramps *up* during Screensaver and back *down* during Active
        // (the wake transition happens in Active). The driver therefore runs
        // under BOTH gates — registered twice, with mutually-exclusive run
        // conditions, rather than once unconditionally, so Line still runs
        // zero systems in `SketchActivity::Idle` and other app states
        // (AGENTS.md "zero systems when idle"). The system is change-gated
        // internally: outside the 1.5 s fade ramps it compares one float and
        // returns.
        app.add_systems(
            Update,
            drive_attract_color.run_if(in_screensaver(AppState::Line)),
        );
        app.add_systems(
            Update,
            drive_attract_color.run_if(sketch_active(AppState::Line)),
        );
    }
}

/// Drive `LineSimParams.params` + `LinePostParams` from the choreography frame.
///
/// Builds the attractor array (the wandering pulse points) plus the noise
/// turbulence, bakes the sim params via the shared baker (A1), and writes the
/// smear uniforms with the focal pinned to centre and `g_constant` at the
/// settled breathing baseline (the smear is decoupled from the walkers so they
/// cannot jolt it). The particle count is left at the full spawned value —
/// cooldown is the present rate's job (see module docs), so this driver never
/// touches the dispatch size.
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
    // The attract gate turns on the kernel's attract-only mechanisms: the
    // fraction kill (a sparser, calmer field — survivors chosen by per-index
    // spawn hash so the thinning is spatially uniform) and the per-particle
    // lifetime respawn (the field continuously self-heals back into the spawn
    // image on staggered ~10-18 s lifespans).
    let (attractors, count) = build_attractor_array(&frame, settings.gravity_constant);
    let gate = AttractGate {
        enabled: true,
        fraction: settings.attract_particle_fraction,
    };
    // Gentle divergence-free drift — now the screensaver's primary motion (the
    // meteors that used to stir the field were cut for jolting the smear). The
    // operator-tunable `attract_turbulence` knob sets the drift speed; `time`
    // scrolls the flow so the field slowly morphs.
    let turbulence = Turbulence {
        amp: settings.attract_turbulence,
        scale: TURBULENCE_SCALE,
        time: time.elapsed_secs(),
    };
    sim.params = bake_sim_params(time.delta_secs(), geom, attractors, count, gate, turbulence);

    // --- Gravity-smear uniforms: kept DECOUPLED from the walkers. ---
    // The smear's focal point (`i_mouse`, the centre of the gravity ray-march)
    // is pinned to screen centre rather than tracking `frame.focal_world`. When
    // it followed the pulses, each cresting walker yanked the whole concentric
    // ring pattern up to ~half a frame toward that walker and snapped it back —
    // a hard jolt of the gravity shader (operator report). Pinning it leaves the
    // smear calm and centred; the walkers (if any) only gently bow the particle
    // line, and the slow noise turbulence morphs the field underneath.
    bake_post_base(
        &mut post,
        geom,
        [0.0, 0.0],
        time.elapsed_secs(),
        settings.gamma,
    );
    bake_smear_tints(&mut post, &settings);
    // The smear breathes EXACTLY like the live sketch at rest: the same
    // 5-second triangle wave the audio coupling drives, at the settled-field
    // baseline (`grouped_upness` = 0). An earlier design pinned a flat
    // 0.10x15000 baseline here, which sat at the dim end of the live breathing
    // range (peaks 0.5x15000) — the whole window visibly dimmed at attract entry
    // (operator report 2026-06-10). The 0.5 baseline matches the live rest
    // breathing, so there is no brightness step at the Active -> Screensaver
    // boundary. We deliberately do NOT add the pulse `activity` swell here: that
    // was a second way the walkers jolted the smear.
    post.g_constant = crate::line::audio_coupling::triangle_wave_approx(time.elapsed_secs() / 5.0)
        * 0.5
        * 15_000.0;
    post.i_mouse_factor = 1.0 / 15.0;
}

/// Map the screensaver fade envelope, the velocity-tint strength knob, and the
/// brightness-lift knob onto the material's `attract_color` uniform value
/// (`x` = velocity-tint strength, `y` = brightness lift `mult − 1`, rest
/// reserved). Both ride the fade so they ramp in/out with attract mode and are
/// exactly zero (provable no-op) when hidden. Pure helper so the mapping is
/// unit-testable without assets.
#[must_use]
fn attract_color_params(fade_alpha: f32, strength: f32, brightness: f32) -> Vec4 {
    let fade = fade_alpha.clamp(0.0, 1.0);
    // The shader applies `rgb *= 1.0 + y`, so the lift amount is `mult − 1`,
    // ramped by the fade. `brightness <= 1.0` (or fade 0) leaves `y == 0`,
    // a bit-exact no-op.
    let lift = fade * (brightness.max(1.0) - 1.0);
    Vec4::new(fade * strength.max(0.0), lift, 0.0, 0.0)
}

/// Drive [`LineMaterial::attract_color`] from the [`ScreensaverFade`]
/// envelope × [`LineSettings::attract_color_strength`].
///
/// Runs during both Screensaver (fade-in) and Active (fade-out after wake) —
/// see the plugin registration for the gating rationale. Mutating the
/// material asset re-prepares its bind group, so the write is change-gated on
/// the strength actually moving: in the settled states (fade at exactly zero
/// or one) this system is a single float compare per frame, no asset churn.
/// `last` is only advanced when the material was actually written, so a frame
/// where the asset isn't loaded yet retries instead of losing the value.
fn drive_attract_color(
    fade: Res<'_, ScreensaverFade>,
    settings: Res<'_, LineSettings>,
    roots: Query<'_, '_, &MeshMaterial2d<LineMaterial>, With<LineRoot>>,
    mut materials: ResMut<'_, Assets<LineMaterial>>,
    mut last: Local<'_, Vec4>,
) {
    let target = attract_color_params(
        fade.alpha(),
        settings.attract_color_strength,
        settings.attract_brightness,
    );
    // Two instances of this system exist (one per activity gate), each with
    // its own `Local`. A stale `last` in the instance that was not running
    // self-corrects: the fade envelope is continuous, so the first frame the
    // instance runs with a differing target rewrites the material and resyncs.
    // Gate on both driven channels (x = tint, y = brightness lift) moving.
    if (target.x - last.x).abs() < f32::EPSILON && (target.y - last.y).abs() < f32::EPSILON {
        return;
    }
    for handle in &roots {
        if let Some(material) = materials.get_mut(&handle.0) {
            material.attract_color = target;
            *last = target;
        }
    }
}

/// Pack the choreography frame's wandering-pulse attractors into the GPU
/// `[Attractor; N]` array, returning `(array, live_count)`.
///
/// Each sample's power is baked with `gravity_constant` exactly as the live
/// mouse/hand writers do (A1 parity). Zero-power samples are skipped so they
/// don't consume uniform slots — in the settled field that is *all* of them
/// (zero attractors packed, putting the kernel in inertial drag); only walkers
/// inside their pulse window pack. The worst case is `PULSE_COUNT = 3`
/// concurrent samples, within `MAX_ATTRACTORS` (8); the cap below guards the
/// invariant if the count grows. If a nonzero ambient floor were ever restored,
/// every walker would pack every frame — see [`choreography::AMBIENT_POWER`] for
/// why it must not be. The pulses keep the v4-parity unbounded constant-magnitude
/// pull (`radius = 0`). `slot` (usize) and the returned count (u32) advance in
/// lockstep, both capped at `MAX_ATTRACTORS`, avoiding a numeric `as` cast in the
/// loop (workspace `as_conversions` lint).
#[must_use]
fn build_attractor_array(
    frame: &choreography::AttractFrame,
    gravity_constant: f32,
) -> ([Attractor; MAX_ATTRACTORS], u32) {
    let mut attractors = [Attractor::default(); MAX_ATTRACTORS];
    let mut slot = 0_usize;
    for sample in &frame.pulses {
        if slot >= MAX_ATTRACTORS || sample.power <= 0.0 {
            continue;
        }
        attractors[slot] = Attractor {
            position: sample.position,
            power: sample.power * gravity_constant,
            // Pulses use the v4-parity unbounded constant-magnitude pull.
            radius: 0.0,
        };
        slot += 1;
    }
    (attractors, u32::try_from(slot).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attract_color_params_scales_and_clamps() {
        // Hidden (Active steady state): fade 0 → both channels exactly zero, so
        // the shader tint AND the brightness lift are provably inert.
        assert_eq!(attract_color_params(0.0, 0.35, 2.2), Vec4::ZERO);
        // Fully shown: x = the tint knob; y = brightness lift (mult − 1).
        let full = attract_color_params(1.0, 0.35, 2.2);
        assert!((full.x - 0.35).abs() < 1e-6);
        assert!((full.y - 1.2).abs() < 1e-6, "lift should be brightness − 1");
        assert_eq!((full.z, full.w), (0.0, 0.0));
        // Mid-fade: both channels linear in the envelope.
        let mid = attract_color_params(0.5, 0.35, 2.2);
        assert!((mid.x - 0.175).abs() < 1e-6);
        assert!((mid.y - 0.6).abs() < 1e-6);
        // brightness 1.0 (lift off) → y is exactly zero even fully shown.
        assert!(attract_color_params(1.0, 0.35, 1.0).y.abs() < 1e-6);
        // brightness below 1.0 clamps to the no-op (never darkens).
        assert!(attract_color_params(1.0, 0.35, 0.5).y.abs() < 1e-6);
        // Out-of-range inputs clamp instead of inverting the tint/lift.
        assert_eq!(attract_color_params(-1.0, 0.35, 2.2), Vec4::ZERO);
        assert_eq!(attract_color_params(0.5, -2.0, 1.0), Vec4::ZERO);
    }

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
        // The packed pulse uses the unbounded pull (radius 0).
        assert!(
            crest_arr[0].radius.abs() < f32::EPSILON,
            "pulses pack with radius 0"
        );
    }
}
