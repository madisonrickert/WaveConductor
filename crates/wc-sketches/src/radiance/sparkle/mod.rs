//! Extremity sparkles: a constellation of small twinkling motes riding every
//! tracked dancer's high-motion extremities, tinted per body.
//!
//! ## What it does
//!
//! Every frame, a per-body tracker scores the four extremity landmarks
//! (wrists + ankles) by how *fast they oscillate*, not how fast they move: a
//! per-axis Schmitt trigger on the One-Euro-smoothed landmark velocity counts
//! direction flips into an exponentially-decaying flips-per-second score, so
//! a hand waving at 3 Hz outranks a whole body drifting across frame. Each
//! body's winning limb and its *contralateral partner* (left wrist ↔ right
//! wrist, left ankle ↔ right ankle) anchor a handful of motes — small soft
//! gaussian points that orbit the limb on slow per-mote drift paths, twinkle
//! on staggered phases, and flash a gentle four-point glint only at the crest
//! of their twinkle. Mote color is the body's identity color (the same
//! per-slot derivation the flame and rims use), so each dancer's sparkles
//! match their flame.
//!
//! The mote **budget is shared** across bodies ([`MAX_SPARKLES`] = 12 slots,
//! capped by the `sparkle_count` setting): a solo dancer gets a fuller
//! constellation (6 motes), a crowded floor spreads fewer per person (see
//! [`per_body_quota`]). Every strength change rides an eased attack/release
//! envelope ([`step_env`]) *and* the body's tracking fade, so motes bloom in
//! and dissolve out — never binary. Highs-band audio shortens the twinkle
//! period and lifts the master brightness (the mid/high lane per the spec;
//! bass never drives the sparkles).
//!
//! ## Module layout
//!
//! The pure decision half — [`LimbOscillator`], winner selection, and the
//! shared-pool assignment ([`assign_motes`] / [`per_body_quota`]) — lives in
//! [`tracker`] (re-exported here, so `radiance::sparkle::*` paths are
//! unchanged). This root owns the GPU-facing surface: the uniform, the
//! material, the mote envelopes, and the per-frame driver.
//!
//! ## Latency + hot-path posture
//!
//! Zero added pipeline latency: the system reads the same-frame
//! `BodyTrackingState` the sim baker reads and packs one small uniform (the
//! `drive_radiance_materials` cost class). All state is fixed-size arrays on
//! a `Copy` resource; nothing allocates after spawn. Priority switches are
//! hysteretic ([`SWITCH_RATIO`]/[`SWITCH_FLOOR`]) so a body's motes do not
//! flicker between two similarly-active limbs, and a mote whose assignment
//! changes fades out at its held position before re-igniting at the new one.
//! When the packed uniform contributes exactly zero light — every envelope
//! releases to ~0 within a second of the last active limb — the driver hides
//! the fullscreen quad ([`sparkle_uniform_dead`]), so the additive
//! full-window fragment pass is skipped instead of adding zeros.

pub mod tracker;

use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey};
use wc_core::input::body::{BodyTrackingState, MAX_TRACKED_BODIES};
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;

pub use tracker::{
    assign_motes, body_com_uv, candidate_eligible, per_body_quota, LimbOscillator, MoteTarget,
    CANDIDATE_LANDMARKS, FLIP_HYSTERESIS_UV_S, MIN_COM_DIST_UV, PARTNER, SCORE_TAU_S, SWITCH_FLOOR,
    SWITCH_RATIO, VISIBILITY_GATE,
};

use super::render::slot_identity_colors;
use super::settings::RadianceSettings;
use super::systems::sim_params::mask_uv_to_world;
use super::systems::spawn::RadianceRoot;

/// Fixed mote capacity (uniform array size; WGSL mirrors it). The
/// `sparkle_count` setting caps how many of these slots are ever assigned —
/// this constant is the hard budget across ALL bodies, documented per the
/// shared-budget rule.
pub const MAX_SPARKLES: usize = 12;
/// Most motes any single body may carry (a solo dancer's constellation).
pub const MAX_MOTES_PER_BODY: usize = 6;
/// Base twinkle waveform period, seconds (the highs drive shortens it).
pub const TWINKLE_PERIOD_S: f32 = 1.4;
/// HDR gain on the body identity color for mote tint (clears the tonemapper
/// knee so motes bloom).
pub const SPARKLE_HDR_GAIN: f32 = 2.0;

/// Strength-envelope attack rate, 1/s (bloom-in on acquiring a limb).
const ENV_ATTACK_RATE: f32 = 6.0;
/// Strength-envelope release rate, 1/s (dissolve on losing it).
const ENV_RELEASE_RATE: f32 = 3.0;
/// Envelope level below which a mote may adopt a new assignment (it faded
/// out far enough that the position jump is invisible).
const REASSIGN_ENV: f32 = 0.12;
/// Mote orbit radius range, world px (per-mote constant within it).
const DRIFT_RADIUS_MIN_PX: f32 = 9.0;
/// See [`DRIFT_RADIUS_MIN_PX`].
const DRIFT_RADIUS_SPAN_PX: f32 = 14.0;
/// Frame-delta cap, matching the sim baker's hitch guard.
const SPARKLE_DT_CAP: f32 = 0.05;

// ── Mote state + constellation resource ─────────────────────────────────────

/// One mote's continuous state.
#[derive(Clone, Copy, Debug, Default)]
struct MoteState {
    /// Strength envelope `0..1` (eased attack/release).
    env: f32,
    /// Last anchored world position (held while fading out).
    pos: Vec2,
    /// Current owner; a mote only re-anchors once its envelope has released
    /// below [`REASSIGN_ENV`], so assignment churn reads as a cross-fade.
    owner: MoteTarget,
}

/// Tracker + mote state for the whole constellation. Inserted on Radiance
/// entry, removed on exit.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct RadianceSparkles {
    /// Per-body-slot oscillation trackers.
    trackers: [LimbOscillator; MAX_TRACKED_BODIES],
    /// The mote pool (shared budget across all bodies).
    motes: [MoteState; MAX_SPARKLES],
}

impl RadianceSparkles {
    /// A slot's tracker (test/diagnostic).
    #[must_use]
    pub fn tracker(&self, slot: usize) -> &LimbOscillator {
        &self.trackers[slot]
    }

    /// Force a mote's strength envelope directly (test-only: `MoteState` is
    /// private, and driving a real envelope up requires frames of oscillating
    /// landmark input).
    #[cfg(test)]
    pub(crate) fn force_mote_env(&mut self, index: usize, env: f32) {
        self.motes[index].env = env;
    }
}

// ── Uniform + material ──────────────────────────────────────────────────────

/// The uniform block the mote shader consumes.
#[derive(ShaderType, Clone, Copy, Debug)]
pub struct RadianceSparkleUniform {
    /// Per mote: xy = anchor world px, z = twinkle phase `0..1`,
    /// w = strength (0 = off).
    pub sparkles: [Vec4; MAX_SPARKLES],
    /// Per mote: rgb = linear-HDR body tint, w = crest-glint gain.
    pub colors: [Vec4; MAX_SPARKLES],
    /// x = master intensity, y = elapsed seconds, z = twinkle period s,
    /// w reserved.
    pub params: Vec4,
}

impl Default for RadianceSparkleUniform {
    /// All motes off, canonical period, master 0.
    fn default() -> Self {
        Self {
            sparkles: [Vec4::ZERO; MAX_SPARKLES],
            colors: [Vec4::ZERO; MAX_SPARKLES],
            params: Vec4::new(0.0, 0.0, TWINKLE_PERIOD_S, 0.0),
        }
    }
}

/// Fullscreen additive material drawing the mote constellation
/// (fragment-only; the default `Material2d` vertex shader supplies world
/// position).
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone, Default)]
pub struct RadianceSparkleMaterial {
    /// The packed mote state for this frame.
    #[uniform(0)]
    pub sparkles: RadianceSparkleUniform,
}

impl Material2d for RadianceSparkleMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/radiance/sparkle.wgsl".into()
    }

    /// `Blend` routes into `Transparent2d`; [`Self::specialize`] then makes
    /// it pure additive (the `RadianceMaterial`/pulse recipe).
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }

    /// Override the color-target blend to pure additive `(One, One)` so the
    /// motes accumulate HDR light into bloom instead of alpha-occluding (the
    /// shared `render::override_additive_blend` recipe — a code span, not a
    /// link: the helper is `pub(crate)` and this doc is public).
    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        super::render::override_additive_blend(descriptor);
        Ok(())
    }
}

// ── Pure helpers ────────────────────────────────────────────────────────────

/// One strength-envelope step toward `target` (asymmetric attack/release,
/// the `step_flame_energy` shape).
#[must_use]
pub fn step_env(env: f32, target: f32, dt: f32) -> f32 {
    let rate = if target > env {
        ENV_ATTACK_RATE
    } else {
        ENV_RELEASE_RATE
    };
    (env + (target - env) * (rate * dt).min(1.0)).clamp(0.0, 1.0)
}

/// Deterministic per-mote hash in `0..1` (golden-ratio stride: motes get
/// well-spread constants without stored state).
#[must_use]
#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "mote index < 12, exact in f32"
)]
pub fn mote_hash(index: usize, salt: f32) -> f32 {
    (index as f32 * 0.618_034 + salt).fract()
}

/// The mote's slow orbital drift offset around its limb anchor at time `t`
/// (world px). Each mote gets its own radius, angular rate, and starting
/// phase, so the constellation breathes instead of moving as a rigid body.
#[must_use]
pub fn mote_drift(index: usize, t: f32) -> Vec2 {
    let radius = DRIFT_RADIUS_MIN_PX + DRIFT_RADIUS_SPAN_PX * mote_hash(index, 0.13);
    let rate = 0.35 + 0.5 * mote_hash(index, 0.47);
    let angle = std::f32::consts::TAU * (mote_hash(index, 0.79) + t * rate * 0.1);
    Vec2::new(angle.cos(), angle.sin()) * radius
}

/// Strength below which the sparkle shader skips a mote (`s.w <= 0.001` in
/// `sparkle.wgsl`); the dead predicate mirrors it exactly.
pub const MOTE_MIN_STRENGTH: f32 = 0.001;

/// True when the packed uniform contributes exactly zero light: the master
/// lane is zero (sparkle setting off / full screensaver dim) or every mote's
/// strength is at-or-below the shader's own skip threshold. The shader
/// multiplies its whole output by `params.x` and `continue`s sub-threshold
/// motes, so hiding the quad on this predicate is output-identical.
#[must_use]
pub fn sparkle_uniform_dead(uniform: &RadianceSparkleUniform) -> bool {
    uniform.params.x <= 0.0
        || uniform
            .sparkles
            .iter()
            .all(|mote| mote.w <= MOTE_MIN_STRENGTH)
}

// ── Per-frame system ────────────────────────────────────────────────────────

/// One frame's tracker outcome per slot, produced by [`advance_trackers`].
#[derive(Clone, Copy, Debug, Default)]
struct TrackerFrame {
    /// Each present body's winning candidate index.
    winners: [Option<usize>; MAX_TRACKED_BODIES],
    /// Whether the winner's contralateral partner is visible.
    partner_ok: [bool; MAX_TRACKED_BODIES],
    /// Each occupied slot's fade envelope.
    slot_fade: [f32; MAX_TRACKED_BODIES],
}

/// Advance each present body's oscillation tracker one frame and reset the
/// trackers of emptied slots (a returning dancer starts clean).
fn advance_trackers(
    trackers: &mut [LimbOscillator; MAX_TRACKED_BODIES],
    body: Option<&BodyTrackingState>,
    dt: f32,
) -> TrackerFrame {
    let mut frame = TrackerFrame::default();
    let mut occupied = [false; MAX_TRACKED_BODIES];
    if let Some(bodies) = body {
        for tracked in bodies.iter_bodies() {
            let slot = tracked.slot;
            if slot >= MAX_TRACKED_BODIES {
                continue;
            }
            occupied[slot] = true;
            frame.slot_fade[slot] = tracked.fade.clamp(0.0, 1.0);
            if !tracked.present {
                continue; // fading out: motes release via fade, tracker holds
            }
            let tracker = &mut trackers[slot];
            tracker.step_scores(tracked, dt);
            tracker.select(tracked);
            frame.winners[slot] = tracker.current();
            if let Some(current) = tracker.current() {
                let partner = tracked.landmarks[CANDIDATE_LANDMARKS[PARTNER[current]]];
                frame.partner_ok[slot] = partner.visibility >= VISIBILITY_GATE;
            }
        }
    }
    for (tracker, occupied) in trackers.iter_mut().zip(occupied) {
        if !occupied {
            tracker.reset();
        }
    }
    frame
}

/// `Update` (gated `in_state(AppState::Radiance)`, like the pulse driver):
/// advance every present body's oscillation tracker, assign the shared mote
/// pool, ease every envelope, and pack the constellation uniform. Hides the
/// fullscreen quad while the uniform is fully dead — every envelope releases
/// to ~0 within a second of the last active limb (see
/// [`sparkle_uniform_dead`]) — so the additive full-window pass is skipped
/// instead of adding zeros.
#[allow(
    clippy::too_many_arguments,
    reason = "Bevy system — each param is a distinct ECS resource/query the driver packs"
)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "mote/slot indices are bounded (12 / 4), exact in f32"
)]
pub fn update_radiance_sparkles(
    time: Res<'_, Time>,
    window: Single<'_, '_, &Window>,
    settings: Res<'_, RadianceSettings>,
    fade: Res<'_, ScreensaverFade>,
    state: Res<'_, super::systems::sim_params::RadianceState>,
    body: Option<Res<'_, BodyTrackingState>>,
    mut sparkles: ResMut<'_, RadianceSparkles>,
    mut quads: Query<
        '_,
        '_,
        (
            &bevy::sprite_render::MeshMaterial2d<RadianceSparkleMaterial>,
            &mut Visibility,
        ),
        With<RadianceRoot>,
    >,
    mut materials: ResMut<'_, Assets<RadianceSparkleMaterial>>,
) {
    let dt = time.delta_secs().min(SPARKLE_DT_CAP);
    let elapsed = time.elapsed_secs();

    // Same mask→world scale the sim baker and pulse driver use.
    let h = window.height().max(1.0);
    let scale = if settings.fit_to_height {
        Vec2::new(h, h)
    } else {
        Vec2::new(window.width().max(1.0), h)
    };

    let frame = advance_trackers(&mut sparkles.trackers, body.as_deref(), dt);
    let TrackerFrame {
        winners,
        partner_ok,
        slot_fade,
    } = frame;

    // Assign the shared pool (budget = the setting, hard-capped at 12).
    let budget = if settings.sparkle_intensity > 0.0 {
        settings.sparkle_count.min(MAX_SPARKLES as u32) as usize
    } else {
        0
    };
    let desired = assign_motes(winners, partner_ok, budget);

    // Per-mote envelopes + anchors.
    let mut uniform = RadianceSparkleUniform {
        params: Vec4::new(
            // Master: the setting, dimmed by the screensaver fade, lifted by
            // the highs drive (mid/high lane per the spec).
            (settings.sparkle_intensity * (1.0 - fade.alpha()) * (0.6 + 0.5 * state.sparkle))
                .max(0.0),
            elapsed,
            // Highs speed the twinkle up (period shrinks toward ~40%).
            TWINKLE_PERIOD_S / (1.0 + 1.3 * state.sparkle),
            0.0,
        ),
        ..RadianceSparkleUniform::default()
    };
    let slot_colors = slot_identity_colors(
        settings.palette,
        state.hue_phase,
        settings.hue_spread,
        fade.alpha(),
    );
    // Split-borrow the resource so the mote loop can read the trackers.
    let RadianceSparkles { trackers, motes } = &mut *sparkles;
    for (i, mote) in motes.iter_mut().enumerate() {
        let want = desired[i];
        if mote.owner != want && mote.env > REASSIGN_ENV {
            // Cross-fade: release at the held position before re-anchoring.
            mote.env = step_env(mote.env, 0.0, dt);
        } else {
            if mote.owner != want {
                mote.owner = want;
            }
            let mut target = 0.0;
            if let (Some((slot, candidate)), Some(bodies)) = (mote.owner, body.as_deref()) {
                if let Some(tracked) = bodies.bodies.get(slot).and_then(Option::as_ref) {
                    let landmark = tracked.landmarks[CANDIDATE_LANDMARKS[candidate]];
                    if landmark.visibility >= VISIBILITY_GATE {
                        mote.pos =
                            mask_uv_to_world(landmark.pos.truncate(), scale, settings.mirror)
                                + mote_drift(i, elapsed);
                        // Strength follows limb activity (score ramp) and the
                        // body's tracking fade — nothing pops.
                        let activity =
                            (trackers[slot].score(candidate.min(3)) / 1.5).clamp(0.3, 1.0);
                        target = activity * slot_fade[slot];
                    }
                }
            }
            mote.env = step_env(mote.env, target, dt);
        }
        if let Some((slot, _)) = mote.owner {
            let color = slot_colors[slot.min(3)] * SPARKLE_HDR_GAIN;
            // Crest-glint gain follows the envelope so the cross only shows
            // on established, active motes.
            uniform.colors[i] = Vec4::new(color.x, color.y, color.z, mote.env);
        }
        uniform.sparkles[i] = Vec4::new(mote.pos.x, mote.pos.y, mote_hash(i, 0.31), mote.env);
    }

    // Fully-dead uniform → hide the quad (skip the full-window additive
    // pass); assign only on change so visibility change detection stays quiet.
    let desired = if sparkle_uniform_dead(&uniform) {
        Visibility::Hidden
    } else {
        Visibility::Visible
    };
    for (handle, mut visibility) in &mut quads {
        if let Some(mut material) = materials.get_mut(&handle.0) {
            material.sparkles = uniform;
        }
        if *visibility != desired {
            *visibility = desired;
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// The uniform's WGSL layout size: `SparkleUniform` in `sparkle.wgsl` is
    /// `sparkles: array<vec4, 12>` (192 B) + `colors: array<vec4, 12>`
    /// (192 B) + `params: vec4` (16 B) = 400 B. Struct parity with the
    /// hand-written WGSL is by convention, so this locks the Rust side's
    /// size against silent field drift.
    #[test]
    fn sparkle_uniform_size_matches_wgsl() {
        assert_eq!(
            <RadianceSparkleUniform as bevy::render::render_resource::ShaderType>::min_size().get(),
            400,
            "RadianceSparkleUniform must stay (12 + 12 + 1) vec4s"
        );
    }

    /// The dead predicate mirrors the shader: zero master or all motes at or
    /// under the shader's own skip threshold is dead; one live mote under a
    /// live master is not.
    #[test]
    fn dead_predicate_matches_shader_contribution() {
        // Default: master 0 AND all motes off.
        assert!(sparkle_uniform_dead(&RadianceSparkleUniform::default()));
        // Live master, all motes at the shader skip threshold → dead.
        let mut uniform = RadianceSparkleUniform {
            params: Vec4::new(0.8, 0.0, TWINKLE_PERIOD_S, 0.0),
            ..RadianceSparkleUniform::default()
        };
        uniform.sparkles[3].w = MOTE_MIN_STRENGTH;
        assert!(
            sparkle_uniform_dead(&uniform),
            "at-threshold motes are skipped by the shader"
        );
        // One mote above the threshold → alive.
        uniform.sparkles[3].w = 0.01;
        assert!(!sparkle_uniform_dead(&uniform));
        // Zero master kills it regardless.
        uniform.params.x = 0.0;
        assert!(sparkle_uniform_dead(&uniform));
    }

    /// The driver hides the quad while every envelope is released and shows
    /// it again once a mote carries strength.
    #[test]
    fn driver_flips_quad_visibility_on_dead_uniform() {
        use crate::radiance::settings::RadianceSettings;
        use crate::radiance::systems::sim_params::RadianceState;
        use bevy::asset::AssetPlugin;
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<RadianceSparkleMaterial>();
        app.world_mut().spawn(Window::default());
        app.insert_resource(RadianceSettings::default());
        app.insert_resource(RadianceState::default());
        app.insert_resource(ScreensaverFade::default());
        app.insert_resource(RadianceSparkles::default());
        let material = app
            .world_mut()
            .resource_mut::<Assets<RadianceSparkleMaterial>>()
            .add(RadianceSparkleMaterial::default());
        let quad = app
            .world_mut()
            .spawn((
                RadianceRoot,
                bevy::sprite_render::MeshMaterial2d(material),
                Visibility::default(),
            ))
            .id();

        // All envelopes at zero (nobody has sparkled) → hidden.
        app.world_mut()
            .run_system_once(update_radiance_sparkles)
            .expect("driver runs");
        assert_eq!(
            *app.world().entity(quad).get::<Visibility>().expect("vis"),
            Visibility::Hidden,
            "released constellation must hide the quad"
        );

        // A mote holding strength → visible again.
        app.world_mut()
            .resource_mut::<RadianceSparkles>()
            .force_mote_env(0, 1.0);
        app.world_mut()
            .run_system_once(update_radiance_sparkles)
            .expect("driver runs again");
        assert_eq!(
            *app.world().entity(quad).get::<Visibility>().expect("vis"),
            Visibility::Visible,
            "a live mote must re-show the quad"
        );
    }

    /// Envelope eases both ways and clamps.
    #[test]
    fn env_attacks_and_releases() {
        let up = step_env(0.0, 1.0, 1.0 / 60.0);
        assert!(up > 0.0 && up < 1.0);
        let down = step_env(1.0, 0.0, 1.0 / 60.0);
        assert!(down < 1.0 && down > 0.0);
        assert!((0.0..=1.0).contains(&step_env(0.5, 1.0, 100.0)));
        assert!((0.0..=1.0).contains(&step_env(0.5, 0.0, 100.0)));
    }

    /// Drift orbits are bounded and per-mote distinct.
    #[test]
    fn drift_is_bounded_and_distinct() {
        for i in 0..MAX_SPARKLES {
            let d = mote_drift(i, 3.7);
            let r = d.length();
            assert!(
                (DRIFT_RADIUS_MIN_PX - 0.01..=DRIFT_RADIUS_MIN_PX + DRIFT_RADIUS_SPAN_PX + 0.01)
                    .contains(&r),
                "mote {i} radius {r}"
            );
        }
        assert_ne!(mote_drift(0, 1.0), mote_drift(1, 1.0));
        // The orbit actually moves over time.
        assert_ne!(mote_drift(0, 1.0), mote_drift(0, 4.0));
    }
}
