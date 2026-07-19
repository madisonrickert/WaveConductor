//! Extremity sparkles: two mirrored star-glints that ride the dancer's
//! fastest-oscillating limb, twinkling through the rainbow.
//!
//! ## What it does
//!
//! Every frame the tracker scores the four extremity landmarks (wrists +
//! ankles — the appendages farthest from the centre of mass) by how *fast
//! they oscillate*, not how fast they move: a per-axis Schmitt trigger on the
//! One-Euro-smoothed landmark velocity counts direction flips into an
//! exponentially-decaying flips-per-second score, so a hand waving at 3 Hz
//! outranks a whole body drifting across frame. The winning limb gets the
//! primary sparkle; its *contralateral partner* (left wrist ↔ right wrist,
//! left ankle ↔ right ankle) gets the mirror sparkle — the "reflection
//! across the Y-axis" is anchored to the real opposite limb's tracked
//! position, never a geometric mirror point, so a sparkle can never float in
//! empty air. If the partner is occluded the mirror fades out instead.
//!
//! The two glints twinkle on the same waveform with the mirror offset by
//! [`MIRROR_OFFSET_S`] (0.5 s), and both colors sweep the full rainbow every
//! [`RAINBOW_PERIOD_S`] (7 s) — the mirror sampling the wheel 0.5 s behind,
//! so the pair always shows two neighbouring rainbow hues.
//!
//! ## Latency + hot-path posture
//!
//! Zero added pipeline latency: the system reads the same-frame
//! primary `TrackedBody` the sim baker reads and packs one small uniform (the
//! `drive_radiance_materials` cost class). All state is fixed-size arrays on
//! a `Copy` resource; nothing allocates after spawn. Priority switches are
//! hysteretic ([`SWITCH_RATIO`]/[`SWITCH_FLOOR`]) so the sparkle does not
//! flicker between two similarly-active limbs.

use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, RenderPipelineDescriptor,
    ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey};
use wc_core::input::body::landmark_index::{
    LEFT_ANKLE, LEFT_HIP, LEFT_WRIST, RIGHT_ANKLE, RIGHT_HIP, RIGHT_WRIST,
};
use wc_core::input::body::{BodyTrackingState, TrackedBody};
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;

use super::render::rotate_hue;
use super::settings::RadianceSettings;
use super::systems::sim_params::mask_uv_to_world;
use super::systems::spawn::RadianceRoot;

/// The extremity candidates, ordered so [`PARTNER`] is a same-array index
/// map: the appendages farthest from the centre of mass, each with a
/// contralateral partner (which is why the nose is excluded — it has no
/// mirror limb to anchor the reflected sparkle to).
pub const CANDIDATE_LANDMARKS: [usize; 4] = [LEFT_WRIST, RIGHT_WRIST, LEFT_ANKLE, RIGHT_ANKLE];
/// Contralateral partner of each entry in [`CANDIDATE_LANDMARKS`]
/// (candidate-index → candidate-index).
pub const PARTNER: [usize; 4] = [1, 0, 3, 2];

/// Twinkle waveform period, seconds.
pub const TWINKLE_PERIOD_S: f32 = 1.4;
/// The mirror sparkle's animation clock lags the primary by this much
/// (pinned by the feature spec: ~0.5 s offset twinkle).
pub const MIRROR_OFFSET_S: f32 = 0.5;
/// One full rainbow sweep, seconds (pinned by the feature spec: 7 s).
pub const RAINBOW_PERIOD_S: f32 = 7.0;
/// HDR red the rainbow wheel rotates from (fully saturated; clears the
/// tonemapper knee so the glint blooms).
pub const SPARKLE_HDR: f32 = 2.2;

/// Schmitt-trigger hysteresis on landmark velocity, mask-UV/s: a direction
/// flip only counts when the velocity actually crosses ±this, so One-Euro
/// residual jitter around zero never reads as oscillation. (Active limbs
/// sweep ~0.1..1.0 UV/s; see `IMPULSE_FULL_SPEED`'s world-px equivalent.)
pub const FLIP_HYSTERESIS_UV_S: f32 = 0.25;
/// Decay time constant of the flips-per-second score, seconds. Long enough
/// to hold priority through a beat, short enough to hand off within ~a bar.
pub const SCORE_TAU_S: f32 = 1.2;
/// A challenger must beat the incumbent by this ratio (plus the floor) to
/// steal the sparkle — priority-switch hysteresis.
pub const SWITCH_RATIO: f32 = 1.3;
/// Absolute score floor a challenger must clear (flips/s) so a still body
/// never hands priority to noise.
pub const SWITCH_FLOOR: f32 = 0.2;
/// Minimum mask-UV distance from the centre of mass (mid-hip) for a limb to
/// sparkle: a wrist resting on the hip is not "far from the centre of mass".
pub const MIN_COM_DIST_UV: f32 = 0.12;
/// Landmark visibility gate (matches the limb-impulse gate).
pub const VISIBILITY_GATE: f32 = 0.5;

/// Strength-envelope attack rate, 1/s (fade-in on acquiring a limb).
const ENV_ATTACK_RATE: f32 = 8.0;
/// Strength-envelope release rate, 1/s (fade-out on losing it).
const ENV_RELEASE_RATE: f32 = 3.5;
/// Frame-delta cap, matching the sim baker's hitch guard.
const SPARKLE_DT_CAP: f32 = 0.05;

/// Per-frame tracker + envelope state for the sparkle pair. Inserted on
/// Radiance entry, removed on exit.
#[derive(Resource, Clone, Copy, Debug)]
pub struct RadianceSparkles {
    /// Schmitt sign state per candidate, x axis (`-1`, `0` = unarmed, `1`).
    sign_x: [i8; 4],
    /// Schmitt sign state per candidate, y axis.
    sign_y: [i8; 4],
    /// Decaying flips-per-second oscillation score per candidate.
    score: [f32; 4],
    /// Currently prioritized candidate index (into [`CANDIDATE_LANDMARKS`]).
    current: Option<usize>,
    /// Primary sparkle strength envelope, `0..1`.
    primary_env: f32,
    /// Mirror sparkle strength envelope, `0..1`.
    mirror_env: f32,
    /// Last valid primary anchor, world px (held while fading out).
    last_primary_world: Vec2,
    /// Last valid mirror anchor, world px (held while fading out).
    last_mirror_world: Vec2,
}

impl Default for RadianceSparkles {
    fn default() -> Self {
        Self {
            sign_x: [0; 4],
            sign_y: [0; 4],
            score: [0.0; 4],
            current: None,
            primary_env: 0.0,
            mirror_env: 0.0,
            last_primary_world: Vec2::ZERO,
            last_mirror_world: Vec2::ZERO,
        }
    }
}

impl RadianceSparkles {
    /// The current flips-per-second score of a candidate (test/diagnostic).
    #[must_use]
    pub fn score(&self, candidate: usize) -> f32 {
        self.score[candidate]
    }

    /// The currently prioritized candidate index (test/diagnostic).
    #[must_use]
    pub fn current(&self) -> Option<usize> {
        self.current
    }
}

/// The uniform block the star-glint shader consumes.
#[derive(ShaderType, Clone, Copy, Debug)]
pub struct RadianceSparkleUniform {
    /// Per sparkle: xy = anchor world px, z = animation clock offset s
    /// (0 primary, [`MIRROR_OFFSET_S`] mirror), w = strength (0 = off).
    pub sparkles: [Vec4; 2],
    /// Per sparkle: rgb = linear-HDR rainbow color, w unused.
    pub colors: [Vec4; 2],
    /// x = master intensity, y = elapsed seconds, z = twinkle period s,
    /// w reserved.
    pub params: Vec4,
}

impl Default for RadianceSparkleUniform {
    /// Both sparkles off, canonical period, master 0.
    fn default() -> Self {
        Self {
            sparkles: [Vec4::ZERO; 2],
            colors: [Vec4::ZERO; 2],
            params: Vec4::new(0.0, 0.0, TWINKLE_PERIOD_S, 0.0),
        }
    }
}

/// Fullscreen additive material drawing the two star-glints (fragment-only;
/// the default `Material2d` vertex shader supplies world position).
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone, Default)]
pub struct RadianceSparkleMaterial {
    /// The packed sparkle state for this frame.
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
    /// glints accumulate HDR light into bloom instead of alpha-occluding.
    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(Some(target)) = fragment.targets.get_mut(0) {
                target.blend = Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                });
            }
        }
        Ok(())
    }
}

// ── Pure tracker steps ──────────────────────────────────────────────────────

/// One Schmitt-trigger step: the sign only changes when `v` crosses the
/// hysteresis band, and `0` (unarmed, the initial state) arms without
/// counting a flip.
fn schmitt_step(prev: i8, v: f32) -> i8 {
    if v > FLIP_HYSTERESIS_UV_S {
        1
    } else if v < -FLIP_HYSTERESIS_UV_S {
        -1
    } else {
        prev
    }
}

/// The centre of mass in mask UV: the mean of the visible hips, or `None`
/// when both are occluded (the distance gate then passes — with no COM
/// reference we cannot judge "far from it").
#[must_use]
pub fn body_com_uv(body: &TrackedBody) -> Option<Vec2> {
    let left = body.landmarks[LEFT_HIP];
    let right = body.landmarks[RIGHT_HIP];
    match (
        left.visibility >= VISIBILITY_GATE,
        right.visibility >= VISIBILITY_GATE,
    ) {
        (true, true) => Some((left.pos.truncate() + right.pos.truncate()) / 2.0),
        (true, false) => Some(left.pos.truncate()),
        (false, true) => Some(right.pos.truncate()),
        (false, false) => None,
    }
}

/// Whether a candidate may carry a sparkle this frame: visible, and far
/// enough from the centre of mass.
#[must_use]
pub fn candidate_eligible(body: &TrackedBody, candidate: usize, com: Option<Vec2>) -> bool {
    let landmark = body.landmarks[CANDIDATE_LANDMARKS[candidate]];
    if landmark.visibility < VISIBILITY_GATE {
        return false;
    }
    com.is_none_or(|c| landmark.pos.truncate().distance(c) >= MIN_COM_DIST_UV)
}

impl RadianceSparkles {
    /// Advance the oscillation scores by one frame: decay every score toward
    /// zero, then count hysteretic velocity-direction flips on both axes
    /// into the flipping candidate's score. A limb oscillating at `f` Hz on
    /// one axis converges to a score of `2f` (two flips per cycle); the
    /// ranking only needs relative order.
    pub fn step_scores(&mut self, body: &TrackedBody, dt: f32) {
        let decay = (-dt / SCORE_TAU_S).exp();
        for (i, &landmark) in CANDIDATE_LANDMARKS.iter().enumerate() {
            self.score[i] *= decay;
            let v = body.velocities[landmark];
            let nx = schmitt_step(self.sign_x[i], v.x);
            let ny = schmitt_step(self.sign_y[i], v.y);
            let mut flips = 0.0_f32;
            if self.sign_x[i] != 0 && nx != self.sign_x[i] {
                flips += 1.0;
            }
            if self.sign_y[i] != 0 && ny != self.sign_y[i] {
                flips += 1.0;
            }
            self.sign_x[i] = nx;
            self.sign_y[i] = ny;
            // Impulse-train EMA: converges to the flip rate in flips/s.
            self.score[i] += flips / SCORE_TAU_S;
        }
    }

    /// Re-select the prioritized candidate with switch hysteresis: the
    /// incumbent keeps the sparkle unless it becomes ineligible or a
    /// challenger beats it by [`SWITCH_RATIO`] (plus [`SWITCH_FLOOR`]).
    pub fn select(&mut self, body: &TrackedBody) {
        let com = body_com_uv(body);
        let incumbent = self.current.filter(|&c| candidate_eligible(body, c, com));
        let mut best: Option<usize> = None;
        for i in 0..CANDIDATE_LANDMARKS.len() {
            if !candidate_eligible(body, i, com) {
                continue;
            }
            if best.is_none_or(|b| self.score[i] > self.score[b]) {
                best = Some(i);
            }
        }
        self.current = match (incumbent, best) {
            (None, b) => b.filter(|&b| self.score[b] > SWITCH_FLOOR),
            (Some(inc), Some(b)) if b != inc => {
                if self.score[b] > self.score[inc] * SWITCH_RATIO + SWITCH_FLOOR {
                    Some(b)
                } else {
                    Some(inc)
                }
            }
            (Some(inc), _) => Some(inc),
        };
    }
}

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

/// The rainbow wheel: a fully-saturated HDR color at `elapsed` seconds into
/// the [`RAINBOW_PERIOD_S`] sweep (rotating pure HDR red keeps saturation
/// and brightness constant around the whole wheel).
#[must_use]
pub fn rainbow_color(elapsed: f32) -> Vec4 {
    let phase = (elapsed / RAINBOW_PERIOD_S).rem_euclid(1.0);
    rotate_hue(Vec4::new(SPARKLE_HDR, 0.0, 0.0, 1.0), phase)
}

// ── Per-frame system ────────────────────────────────────────────────────────

/// `Update` (gated `in_state(AppState::Radiance)`, like the pulse driver):
/// advance the oscillation tracker, pick the priority limb, anchor the
/// mirrored pair, and pack the glint uniform.
#[allow(
    clippy::too_many_arguments,
    reason = "Bevy system — each param is a distinct ECS resource/query the driver packs"
)]
pub fn update_radiance_sparkles(
    time: Res<'_, Time>,
    window: Single<'_, '_, &Window>,
    settings: Res<'_, RadianceSettings>,
    fade: Res<'_, ScreensaverFade>,
    body: Option<Res<'_, BodyTrackingState>>,
    mut sparkles: ResMut<'_, RadianceSparkles>,
    quads: Query<
        '_,
        '_,
        &bevy::sprite_render::MeshMaterial2d<RadianceSparkleMaterial>,
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

    // Multi-body migration: the sparkles ride the PRIMARY (featured) body;
    // a later radiance overhaul may fan the pair out per tracked body.
    let tracked = body
        .as_deref()
        .and_then(BodyTrackingState::primary)
        .filter(|b| b.present);
    let mut primary_target = 0.0;
    let mut mirror_target = 0.0;
    if let Some(body) = tracked {
        sparkles.step_scores(body, dt);
        sparkles.select(body);
        if let Some(current) = sparkles.current {
            primary_target = 1.0;
            let anchor = body.landmarks[CANDIDATE_LANDMARKS[current]];
            sparkles.last_primary_world =
                mask_uv_to_world(anchor.pos.truncate(), scale, settings.mirror);
            // The "Y-axis reflection" anchors to the real contralateral
            // limb, tracking it live — never a geometric mirror point.
            let partner = body.landmarks[CANDIDATE_LANDMARKS[PARTNER[current]]];
            if partner.visibility >= VISIBILITY_GATE {
                mirror_target = 1.0;
                sparkles.last_mirror_world =
                    mask_uv_to_world(partner.pos.truncate(), scale, settings.mirror);
            }
        }
    } else {
        sparkles.current = None;
    }
    if settings.sparkle_intensity <= 0.0 {
        primary_target = 0.0;
        mirror_target = 0.0;
    }
    sparkles.primary_env = step_env(sparkles.primary_env, primary_target, dt);
    sparkles.mirror_env = step_env(sparkles.mirror_env, mirror_target, dt);

    let master = settings.sparkle_intensity * (1.0 - fade.alpha());
    let uniform = RadianceSparkleUniform {
        sparkles: [
            Vec4::new(
                sparkles.last_primary_world.x,
                sparkles.last_primary_world.y,
                0.0,
                sparkles.primary_env,
            ),
            Vec4::new(
                sparkles.last_mirror_world.x,
                sparkles.last_mirror_world.y,
                MIRROR_OFFSET_S,
                sparkles.mirror_env,
            ),
        ],
        colors: [
            rainbow_color(elapsed),
            rainbow_color(elapsed - MIRROR_OFFSET_S),
        ],
        params: Vec4::new(master.max(0.0), elapsed, TWINKLE_PERIOD_S, 0.0),
    };
    for handle in &quads {
        if let Some(mut material) = materials.get_mut(&handle.0) {
            material.sparkles = uniform;
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "tests derive small positive step counts / times from literals, exact in f32"
)]
mod tests {
    use super::*;
    use wc_core::input::body::{BodyLandmark, BODY_LANDMARK_COUNT};

    /// A visible primary body (slot 0, fully faded in) with all landmarks at
    /// UV (0.5, 0.5) and no motion.
    fn fixture_body() -> TrackedBody {
        let mut landmarks = [BodyLandmark::default(); BODY_LANDMARK_COUNT];
        for lm in &mut landmarks {
            lm.visibility = 1.0;
            lm.pos = Vec3::new(0.5, 0.5, 0.0);
        }
        // Spread the extremities away from the mid-hip COM so the distance
        // gate passes.
        landmarks[LEFT_WRIST].pos = Vec3::new(0.2, 0.3, 0.0);
        landmarks[RIGHT_WRIST].pos = Vec3::new(0.8, 0.3, 0.0);
        landmarks[LEFT_ANKLE].pos = Vec3::new(0.4, 0.9, 0.0);
        landmarks[RIGHT_ANKLE].pos = Vec3::new(0.6, 0.9, 0.0);
        TrackedBody {
            slot: 0,
            present: true,
            fade: 1.0,
            confidence: 0.9,
            landmarks,
            timestamp: std::time::Duration::from_millis(33),
            crop_fraction: 1.0,
            size: 0.2,
            ..TrackedBody::default()
        }
    }

    /// Drive `body`'s landmark `idx` with a square-wave x velocity at `hz`
    /// for `seconds`, stepping the tracker at 60 fps.
    fn oscillate(
        state: &mut RadianceSparkles,
        body: &mut TrackedBody,
        idx: usize,
        hz: f32,
        seconds: f32,
    ) {
        let dt = 1.0 / 60.0;
        let steps = (seconds * 60.0) as usize;
        for step in 0..steps {
            let t = step as f32 * dt;
            let sign = if (t * hz).fract() < 0.5 { 1.0 } else { -1.0 };
            body.velocities[idx] = Vec3::new(sign * 0.6, 0.0, 0.0);
            state.step_scores(body, dt);
            body.velocities[idx] = Vec3::ZERO;
        }
    }

    /// A fast-waving wrist out-scores a slowly-waving ankle, and selection
    /// picks it.
    #[test]
    fn fastest_oscillator_wins_priority() {
        let mut state = RadianceSparkles::default();
        let mut body = fixture_body();
        let dt = 1.0 / 60.0;
        let steps = (3.0 * 60.0) as usize;
        // Drive both limbs simultaneously: left wrist at 3 Hz, left ankle at 1 Hz.
        for step in 0..steps {
            let t = step as f32 * dt;
            let wrist_sign = if (t * 3.0).fract() < 0.5 { 1.0 } else { -1.0 };
            let ankle_sign = if (t * 1.0).fract() < 0.5 { 1.0 } else { -1.0 };
            body.velocities[LEFT_WRIST] = Vec3::new(wrist_sign * 0.6, 0.0, 0.0);
            body.velocities[LEFT_ANKLE] = Vec3::new(ankle_sign * 0.6, 0.0, 0.0);
            state.step_scores(&body, dt);
        }
        assert!(
            state.score(0) > state.score(2) * 2.0,
            "3 Hz wrist ({}) must far out-score 1 Hz ankle ({})",
            state.score(0),
            state.score(2)
        );
        state.select(&body);
        assert_eq!(state.current(), Some(0), "left wrist takes priority");
    }

    /// Sub-hysteresis jitter around zero never accumulates score.
    #[test]
    fn jitter_below_hysteresis_scores_nothing() {
        let mut state = RadianceSparkles::default();
        let mut body = fixture_body();
        let dt = 1.0 / 60.0;
        for step in 0..600 {
            let sign = if step % 2 == 0 { 1.0 } else { -1.0 };
            body.velocities[LEFT_WRIST] = Vec3::new(sign * 0.1, sign * -0.1, 0.0);
            state.step_scores(&body, dt);
        }
        assert!(
            state.score(0).abs() < f32::EPSILON,
            "jitter must not read as oscillation: {}",
            state.score(0)
        );
    }

    /// The incumbent keeps priority against a marginally-better challenger
    /// (switch hysteresis), but loses to a decisively better one.
    #[test]
    fn priority_switch_is_hysteretic() {
        let mut state = RadianceSparkles::default();
        let mut body = fixture_body();
        oscillate(&mut state, &mut body, LEFT_WRIST, 2.0, 2.0);
        state.select(&body);
        assert_eq!(state.current(), Some(0));
        // Nudge the right wrist just above the left's decayed score: within
        // the ratio band, the incumbent holds.
        state.score[1] = state.score[0] * 1.1;
        state.select(&body);
        assert_eq!(
            state.current(),
            Some(0),
            "marginal challenger must not steal"
        );
        state.score[1] = state.score[0] * SWITCH_RATIO + SWITCH_FLOOR + 0.1;
        state.select(&body);
        assert_eq!(state.current(), Some(1), "decisive challenger takes over");
    }

    /// An occluded incumbent hands off; with every candidate occluded the
    /// sparkle goes away entirely.
    #[test]
    fn occlusion_releases_priority() {
        let mut state = RadianceSparkles::default();
        let mut body = fixture_body();
        oscillate(&mut state, &mut body, LEFT_WRIST, 2.0, 2.0);
        state.select(&body);
        assert_eq!(state.current(), Some(0));
        body.landmarks[LEFT_WRIST].visibility = 0.0;
        state.select(&body);
        assert_ne!(
            state.current(),
            Some(0),
            "occluded limb cannot hold priority"
        );
        for &lm in &CANDIDATE_LANDMARKS {
            body.landmarks[lm].visibility = 0.0;
        }
        state.select(&body);
        assert_eq!(state.current(), None, "no visible extremity, no sparkle");
    }

    /// A wrist resting on the hip (inside the COM ring) is ineligible.
    #[test]
    fn extremity_near_com_is_ineligible() {
        let body = {
            let mut b = fixture_body();
            b.landmarks[LEFT_WRIST].pos = b.landmarks[LEFT_HIP].pos;
            b
        };
        let com = body_com_uv(&body);
        assert!(com.is_some(), "hips visible -> COM exists");
        assert!(
            !candidate_eligible(&body, 0, com),
            "wrist on the hip is not far from the centre of mass"
        );
        assert!(candidate_eligible(&body, 1, com), "raised wrist is");
    }

    /// The partner map is a left↔right involution over the candidate set.
    #[test]
    fn partner_map_is_contralateral_involution() {
        for (i, &p) in PARTNER.iter().enumerate() {
            assert_eq!(PARTNER[p], i, "partner of partner is self");
            assert_ne!(p, i);
        }
        assert_eq!(CANDIDATE_LANDMARKS[PARTNER[0]], RIGHT_WRIST);
        assert_eq!(CANDIDATE_LANDMARKS[PARTNER[2]], RIGHT_ANKLE);
    }

    /// The rainbow sweep returns to its start color after exactly one
    /// period and keeps constant HDR value + full saturation on the way.
    #[test]
    fn rainbow_cycles_in_seven_seconds() {
        let start = rainbow_color(0.0);
        let looped = rainbow_color(RAINBOW_PERIOD_S);
        assert!(
            (start - looped).abs().max_element() < 1e-4,
            "{start} vs {looped}"
        );
        for i in 0..14_u16 {
            let c = rainbow_color(f32::from(i) * 0.5);
            let value = c.x.max(c.y).max(c.z);
            let low = c.x.min(c.y).min(c.z);
            assert!(
                (value - SPARKLE_HDR).abs() < 1e-4,
                "HDR value constant: {c}"
            );
            assert!(low.abs() < 1e-4, "full saturation: {c}");
        }
        // Halfway round the wheel from red lands on cyan (hue 180°).
        let half = rainbow_color(RAINBOW_PERIOD_S / 2.0);
        assert!(
            half.x.abs() < 1e-3
                && (half.y - SPARKLE_HDR).abs() < 1e-3
                && (half.z - SPARKLE_HDR).abs() < 1e-3,
            "{half}"
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
}
