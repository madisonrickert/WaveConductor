//! Per-frame Radiance simulation writer plus the idle freeze.
//!
//! Owns [`RadianceState`] (the smoothed audio-drive envelopes), the pure
//! mask-UV↔world mapping (CPU twin of the kernel's), the pure
//! [`audio_drive`] mapping, and the single [`bake_radiance_sim`] baker that
//! both the live writer ([`update_radiance_sim`]) and the screensaver
//! performer call — one baker, two writers, so the audio/impulse derivation
//! cannot drift between the live and attract paths (flame's Condition A1).
//!
//! Nothing here allocates: every value is stack math over `Copy` inputs, so
//! the per-frame path is heap-free per the multi-hour soak target.

use bevy::prelude::*;
use wc_core::audio::input::AudioAnalysis;
use wc_core::input::body::landmark_index::{
    LEFT_ANKLE, LEFT_HIP, LEFT_WRIST, NOSE, RIGHT_ANKLE, RIGHT_HIP, RIGHT_WRIST,
};
use wc_core::input::body::{BodyTrackingState, SilhouetteEdges, MAX_EDGE_POINTS};

use crate::radiance::compute::sim_params::{
    RadianceImpulse, RadianceSimParams, RadianceSimParamsGpu, MAX_IMPULSES,
};
use crate::radiance::settings::RadianceSettings;

/// `MediaPipe` pose landmark indices baked into impulse slots, per the pinned
/// cross-plan contract: nose, left/right wrist, left/right hip, left/right
/// ankle. Seven of the eight slots; the eighth is headroom. Sourced from
/// `wc_core::input::body::landmark_index` rather than re-declared literals so
/// the two plans cannot silently drift on index assignment.
pub const IMPULSE_LANDMARKS: [usize; 7] = [
    NOSE,
    LEFT_WRIST,
    RIGHT_WRIST,
    LEFT_HIP,
    RIGHT_HIP,
    LEFT_ANKLE,
    RIGHT_ANKLE,
];

/// Frame-time cap in seconds (matches the shared particle engine's 50 ms cap).
pub const DT_CAP: f32 = 0.05;
/// Per-dead-particle respawn attempts per second at `emission_rate == 1.0`
/// and neutral audio. The baker multiplies by the bass drive and `dt`.
pub const EMISSION_BASE_HZ: f32 = 2.5;
/// Onset envelope exponential release time constant, seconds.
pub const ONSET_DECAY_SECS: f32 = 0.18;
/// Onset envelope clamp (spectral flux is unbounded above).
pub const ONSET_MAX: f32 = 2.0;
/// Outward burst speed at full onset envelope, world px/s.
pub const BURST_SPEED: f32 = 260.0;
/// Spawn offset along the outward normal, world px.
pub const SPAWN_OFFSET: f32 = 4.0;
/// Baseline spawn speed along the outward normal, world px/s.
pub const SPAWN_SPEED: f32 = 70.0;
/// Particle lifespan range, seconds.
pub const LIFESPAN_MIN: f32 = 1.2;
/// See [`LIFESPAN_MIN`].
pub const LIFESPAN_MAX: f32 = 3.4;
/// Velocity fraction remaining after one second of drag.
pub const DRAG_PER_SECOND: f32 = 0.25;
/// Curl spatial frequency, radians per world px (~785 px swirl wavelength).
pub const CURL_SCALE: f32 = 0.008;
/// Limb impulse influence radius, world px.
pub const IMPULSE_RADIUS: f32 = 140.0;
/// Limb speed (world px/s) that maps to impulse gain 1.0.
pub const IMPULSE_FULL_SPEED: f32 = 900.0;
/// Smoothing time constant for the intensity/sparkle envelopes, seconds.
pub const ENVELOPE_SMOOTH_SECS: f32 = 0.25;

/// Smoothed audio-drive envelopes and the palette-shift accumulator; also
/// read by the material driver (Task 8). Rebuilt fresh on every sketch entry.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct RadianceState {
    /// Onset burst envelope: instant attack, exponential release.
    pub onset_env: f32,
    /// Smoothed master intensity from RMS (`~0.55..1.5`); drives the
    /// particle-material brightness.
    pub intensity: f32,
    /// Smoothed high-band energy (`0..1`); drives sparkle flicker + fill
    /// shimmer.
    pub sparkle: f32,
    /// Gradient-shift accumulator in `0..1` (wraps); bass advances it.
    pub palette_shift: f32,
}

/// The neutral [`AudioAnalysis`] used when the resource is absent (headless
/// tests, feature-less harnesses) — the same values Plan A publishes when the
/// stream is inactive. Delegates to `AudioAnalysis::neutral()`; kept as a
/// named free function so this module's own public surface (and the tests
/// below) can spell the neutral case without reaching into Plan A's type.
#[must_use]
pub fn neutral_audio() -> AudioAnalysis {
    AudioAnalysis::neutral()
}

/// Mask-UV (0..1, y down) → world px (origin center, y up), with the mirror
/// flip. CPU twin of the kernel's `mask_uv_to_world` — the two must stay
/// term-for-term identical (world = ((u − 0.5)·sx, (0.5 − v)·sy)).
#[must_use]
pub fn mask_uv_to_world(uv: Vec2, scale: Vec2, mirror: bool) -> Vec2 {
    let u = if mirror { 1.0 - uv.x } else { uv.x };
    Vec2::new((u - 0.5) * scale.x, (0.5 - uv.y) * scale.y)
}

/// Mask-UV direction → world direction (mirror sign on x, y flip). NOT
/// normalized — impulse velocities keep their magnitude (UV/s × scale =
/// px/s); the kernel normalizes separately where it needs a unit normal.
#[must_use]
pub fn mask_dir_to_world(dir: Vec2, scale: Vec2, mirror: bool) -> Vec2 {
    let sx = if mirror { -scale.x } else { scale.x };
    Vec2::new(dir.x * sx, -dir.y * scale.y)
}

/// The audio→simulation coupling, as pure multipliers/values over one
/// [`AudioAnalysis`] frame (spec: bass→emission+buoyancy, highs→turbulence+
/// sparkle, onset→radial burst, slow RMS→master intensity).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioDrive {
    /// Multiplier on the emission pressure (bass).
    pub emission_mul: f32,
    /// Multiplier on buoyancy (bass pulse).
    pub buoyancy_mul: f32,
    /// Multiplier on curl flow strength (highs).
    pub turbulence_mul: f32,
    /// Sparkle target `0..1` (highs).
    pub sparkle: f32,
    /// Master intensity target (RMS-lifted brightness).
    pub intensity: f32,
    /// Raw onset strength this frame, sensitivity-scaled and clamped.
    pub onset: f32,
}

/// Map one analysis frame into drive values. Pure and allocation-free.
/// `sensitivity == 0.0` returns the exact neutral drive (all multipliers 1.0)
/// so audio coupling is provably inert at the knob's floor.
#[must_use]
pub fn audio_drive(audio: &AudioAnalysis, sensitivity: f32) -> AudioDrive {
    let s = sensitivity.max(0.0);
    // Low three bands = bass body; top three = air/sparkle.
    let bass = (audio.bands[0] + audio.bands[1] + audio.bands[2]) / 3.0;
    let highs = (audio.bands[5] + audio.bands[6] + audio.bands[7]) / 3.0;
    AudioDrive {
        emission_mul: 1.0 + 1.5 * bass * s,
        buoyancy_mul: 1.0 + 0.8 * bass * s,
        turbulence_mul: 1.0 + 1.2 * highs * s,
        sparkle: (highs * s).clamp(0.0, 1.0),
        intensity: 0.55 + 0.9 * audio.rms * s,
        onset: (audio.onset * s).clamp(0.0, ONSET_MAX),
    }
}

/// One baker, two writers (live + screensaver) — flame's Condition A1.
///
/// Advances the [`RadianceState`] envelopes (onset attack/release, smoothed
/// intensity/sparkle, palette shift), then writes every field of the kernel
/// uniform: audio-scaled emission/buoyancy/turbulence, the onset burst, the
/// mask-UV→world transform for the current window + mirror setting, and up to
/// [`MAX_IMPULSES`] limb impulse slots from the smoothed landmark velocities.
#[allow(
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "edge/particle counts are bounded (MAX_EDGE_POINTS / the 300k \
              particle slider); usize -> u32 is exact in range"
)]
#[allow(
    clippy::too_many_arguments,
    reason = "a pure baker's parameters are its data dependencies; the ninth \
              (elapsed, alongside dt) is what lets the screensaver performer \
              (Task 12) drive the same function on its own virtual clock \
              instead of duplicating the kernel-uniform write"
)]
pub fn bake_radiance_sim(
    settings: &RadianceSettings,
    audio: &AudioAnalysis,
    body: Option<&BodyTrackingState>,
    edge_count: usize,
    window_size: Vec2,
    dt: f32,
    elapsed: f32,
    state: &mut RadianceState,
    out: &mut RadianceSimParamsGpu,
) {
    let dt = dt.min(DT_CAP);
    let drive = audio_drive(audio, settings.audio_sensitivity);

    // Onset envelope: instant attack to the incoming strength, exponential
    // release — so one drum hit reads as one burst, not a sustained gale.
    let released = state.onset_env * (-dt / ONSET_DECAY_SECS).exp();
    state.onset_env = released.max(drive.onset);
    // Smoothed intensity/sparkle (one-pole toward the drive targets).
    let k = 1.0 - (-dt / ENVELOPE_SMOOTH_SECS).exp();
    state.intensity += (drive.intensity - state.intensity) * k;
    state.sparkle += (drive.sparkle - state.sparkle) * k;
    // Palette drifts slowly, faster under bass (audio-shifted gradient).
    state.palette_shift =
        (state.palette_shift + dt * (0.02 + 0.10 * (drive.emission_mul - 1.0))).fract();

    out.dt = dt;
    out.time = elapsed;
    // Monotonic per-bake counter salting the kernel's respawn hash. Wraps
    // freely (the hash tolerates it) and, unlike the old `u32(time * 60.0)`
    // salt, never aliases when `elapsed` is pinned or two bakes fall in the
    // same 1/60 s bucket.
    out.frame = out.frame.wrapping_add(1);
    out.emission_prob =
        (settings.emission_rate * drive.emission_mul * EMISSION_BASE_HZ * dt).clamp(0.0, 1.0);
    out.edge_count = edge_count.min(MAX_EDGE_POINTS) as u32;
    out.spawn_offset = SPAWN_OFFSET;
    out.spawn_speed = SPAWN_SPEED * (0.6 + 0.4 * state.intensity);
    out.burst_speed = state.onset_env * BURST_SPEED;
    out.buoyancy = settings.buoyancy * drive.buoyancy_mul;
    out.flow_strength = settings.flow_strength * drive.turbulence_mul;
    out.curl_scale = CURL_SCALE;
    out.curl_octaves = settings.curl_octaves.clamp(1, 3);
    out.drag_baked = DRAG_PER_SECOND.powf(dt);
    out.lifespan_min = LIFESPAN_MIN;
    out.lifespan_max = LIFESPAN_MAX;
    out.mirror = u32::from(settings.mirror);
    // Mask → world scale. The mask is square; the `fit_to_height` setting maps
    // it to a centred height×height square so the dancer keeps its proportions
    // on non-square displays (portrait installs — a 9:16 screen otherwise
    // stretches the dancer ~1.8x tall). The default stretches the square to fill
    // the whole window rect (the v1 look, tuned for 16:9). Every consumer (fill,
    // rim, edges, limb impulses) reads `uv_to_world`, so this one value keeps
    // them consistent.
    let h = window_size.y.max(1.0);
    out.uv_to_world = if settings.fit_to_height {
        [h, h]
    } else {
        [window_size.x.max(1.0), h]
    };

    // Limb impulses from the smoothed landmark velocities.
    let scale = Vec2::new(out.uv_to_world[0], out.uv_to_world[1]);
    let mut n = 0usize;
    if let Some(body) = body {
        if body.present {
            for &lm in &IMPULSE_LANDMARKS {
                if n >= MAX_IMPULSES {
                    break;
                }
                let landmark = body.landmarks[lm];
                if landmark.visibility < 0.5 {
                    continue;
                }
                let vel = mask_dir_to_world(
                    Vec2::new(body.velocities[lm].x, body.velocities[lm].y),
                    scale,
                    settings.mirror,
                );
                let gain = (vel.length() / IMPULSE_FULL_SPEED).clamp(0.0, 1.0);
                if gain < 0.05 {
                    continue; // resting limbs shed nothing
                }
                let pos = mask_uv_to_world(
                    Vec2::new(landmark.pos.x, landmark.pos.y),
                    scale,
                    settings.mirror,
                );
                out.impulses[n] = RadianceImpulse {
                    position: pos.into(),
                    velocity: vel.into(),
                    radius: IMPULSE_RADIUS,
                    gain,
                    _pad: [0.0; 2],
                };
                n += 1;
            }
        }
    }
    // Zero stale slots past the live count so a limb dropping out of frame
    // cannot leave a ghost impulse.
    for slot in out.impulses.iter_mut().skip(n) {
        *slot = RadianceImpulse::default();
    }
    out.impulse_count = n as u32;
    // particle_count is owned by spawn (buffer size); the baker leaves it.
}

/// `Update` (gated `sketch_active(AppState::Radiance)`): the live writer.
/// Gathers the current analysis/body/edges resources (all optional — the
/// sketch degrades to motion-only or emission-only gracefully) and bakes.
pub fn update_radiance_sim(
    time: Res<'_, Time>,
    window: Single<'_, '_, &Window>,
    settings: Res<'_, RadianceSettings>,
    audio: Option<Res<'_, AudioAnalysis>>,
    body: Option<Res<'_, BodyTrackingState>>,
    edges: Option<Res<'_, SilhouetteEdges>>,
    mut state: ResMut<'_, RadianceState>,
    mut sim: ResMut<'_, RadianceSimParams>,
) {
    let audio_frame = audio.map_or_else(neutral_audio, |a| *a);
    let edge_count = edges.map_or(0, |e| e.points.len());
    let window_size = Vec2::new(window.width(), window.height());
    bake_radiance_sim(
        &settings,
        &audio_frame,
        body.as_deref(),
        edge_count,
        window_size,
        time.delta_secs(),
        time.elapsed_secs(),
        &mut state,
        &mut sim.params,
    );
}

/// `OnEnter(SketchActivity::Idle)` (gated `in_state(AppState::Radiance)`):
/// zero emission and the burst so the aura fades out over one lifespan while
/// the throttled last frames hold — flame's freeze idiom, adapted to a
/// particle field that must die out rather than stop mid-air.
pub fn freeze_radiance_emission(mut sim: ResMut<'_, RadianceSimParams>) {
    sim.params.emission_prob = 0.0;
    sim.params.burst_speed = 0.0;
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use wc_core::input::body::{BodyLandmark, BODY_LANDMARK_COUNT};

    fn fixture_audio(bands: [f32; 8], rms: f32, onset: f32) -> AudioAnalysis {
        AudioAnalysis {
            rms,
            gain: 1.0,
            bands,
            onset,
            beat_confidence: 0.0,
            peak: 0.0,
            active: true,
        }
    }

    fn fixture_body(wrist_vel: Vec3) -> BodyTrackingState {
        let mut landmarks = [BodyLandmark::default(); BODY_LANDMARK_COUNT];
        for lm in &mut landmarks {
            lm.visibility = 1.0;
            lm.pos = Vec3::new(0.5, 0.5, 0.0);
        }
        // Right wrist (16) moving.
        landmarks[16].pos = Vec3::new(0.7, 0.4, 0.0);
        let mut velocities = [Vec3::ZERO; BODY_LANDMARK_COUNT];
        velocities[16] = wrist_vel;
        BodyTrackingState {
            present: true,
            confidence: 0.9,
            landmarks,
            world_landmarks: [Vec3::ZERO; BODY_LANDMARK_COUNT],
            velocities,
            timestamp: std::time::Duration::from_millis(33),
        }
    }

    fn bake(
        settings: &RadianceSettings,
        audio: &AudioAnalysis,
        body: Option<&BodyTrackingState>,
        edge_count: usize,
    ) -> (RadianceState, RadianceSimParamsGpu) {
        let mut state = RadianceState::default();
        let mut out = RadianceSimParamsGpu::default();
        bake_radiance_sim(
            settings,
            audio,
            body,
            edge_count,
            Vec2::new(1920.0, 1080.0),
            1.0 / 60.0,
            10.0,
            &mut state,
            &mut out,
        );
        (state, out)
    }

    /// Mirror on: UV x flips around center; y flips down→up. Golden points.
    #[test]
    fn mask_uv_to_world_maps_and_mirrors() {
        let scale = Vec2::new(1920.0, 1080.0);
        // Center maps to origin either way.
        assert_eq!(
            mask_uv_to_world(Vec2::new(0.5, 0.5), scale, false),
            Vec2::ZERO
        );
        assert_eq!(
            mask_uv_to_world(Vec2::new(0.5, 0.5), scale, true),
            Vec2::ZERO
        );
        // UV (0,0) is the top-left of the mask -> left edge, top of screen.
        let tl = mask_uv_to_world(Vec2::new(0.0, 0.0), scale, false);
        assert_eq!(tl, Vec2::new(-960.0, 540.0));
        // Mirrored, the same UV lands on the RIGHT edge.
        let tl_m = mask_uv_to_world(Vec2::new(0.0, 0.0), scale, true);
        assert_eq!(tl_m, Vec2::new(960.0, 540.0));
        // Directions: mask +y (down) maps to world -y; mirror negates x.
        let d = mask_dir_to_world(Vec2::new(1.0, 1.0), scale, false);
        assert!(d.x > 0.0 && d.y < 0.0);
        let d_m = mask_dir_to_world(Vec2::new(1.0, 1.0), scale, true);
        assert!(d_m.x < 0.0 && d_m.y < 0.0);
    }

    /// Sensitivity 0 (or silent input) is the exact neutral drive: every
    /// multiplier 1.0, no burst — audio coupling provably inert.
    #[test]
    fn audio_drive_neutral_at_zero_sensitivity() {
        let loud = fixture_audio([1.0; 8], 1.0, 1.0);
        let d = audio_drive(&loud, 0.0);
        assert!((d.emission_mul - 1.0).abs() < f32::EPSILON);
        assert!((d.buoyancy_mul - 1.0).abs() < f32::EPSILON);
        assert!((d.turbulence_mul - 1.0).abs() < f32::EPSILON);
        assert!(d.sparkle.abs() < f32::EPSILON);
        assert!(d.onset.abs() < f32::EPSILON);
    }

    /// Bass raises emission + buoyancy; highs raise turbulence + sparkle.
    #[test]
    fn audio_drive_routes_bands_per_spec() {
        let bassy = fixture_audio([0.9, 0.9, 0.9, 0.0, 0.0, 0.0, 0.0, 0.0], 0.3, 0.0);
        let airy = fixture_audio([0.0, 0.0, 0.0, 0.0, 0.0, 0.9, 0.9, 0.9], 0.3, 0.0);
        let db = audio_drive(&bassy, 1.0);
        let da = audio_drive(&airy, 1.0);
        assert!(db.emission_mul > 1.5 && db.buoyancy_mul > 1.2);
        assert!(
            (db.turbulence_mul - 1.0).abs() < 1e-6,
            "bass must not stir turbulence"
        );
        assert!(da.turbulence_mul > 1.5 && da.sparkle > 0.5);
        assert!(
            (da.emission_mul - 1.0).abs() < 1e-6,
            "highs must not pump emission"
        );
    }

    /// The baker scales emission with the bass drive vs the neutral bake.
    #[test]
    fn bake_bass_raises_emission_prob() {
        let settings = RadianceSettings::default();
        let quiet = neutral_audio();
        let bassy = fixture_audio([0.9, 0.9, 0.9, 0.0, 0.0, 0.0, 0.0, 0.0], 0.4, 0.0);
        let (_, base) = bake(&settings, &quiet, None, 500);
        let (_, driven) = bake(&settings, &bassy, None, 500);
        assert!(driven.emission_prob > base.emission_prob);
        assert!(driven.buoyancy > base.buoyancy);
        // Expected neutral value: rate * 1.0 * EMISSION_BASE_HZ * dt.
        let expect = 0.5 * EMISSION_BASE_HZ / 60.0;
        assert!((base.emission_prob - expect).abs() < 1e-6);
    }

    /// Onset attacks instantly and releases exponentially across frames.
    #[test]
    fn onset_envelope_attacks_then_decays() {
        let settings = RadianceSettings::default();
        let mut state = RadianceState::default();
        let mut out = RadianceSimParamsGpu::default();
        let hit = fixture_audio([0.0; 8], 0.2, 1.5);
        let silence = neutral_audio();
        let win = Vec2::new(1920.0, 1080.0);
        bake_radiance_sim(
            &settings,
            &hit,
            None,
            100,
            win,
            1.0 / 60.0,
            0.0,
            &mut state,
            &mut out,
        );
        let peak = out.burst_speed;
        assert!(peak > 0.0, "onset must produce a burst");
        for _ in 0..30 {
            bake_radiance_sim(
                &settings,
                &silence,
                None,
                100,
                win,
                1.0 / 60.0,
                0.0,
                &mut state,
                &mut out,
            );
        }
        assert!(
            out.burst_speed < peak * 0.1,
            "burst must decay: {} vs peak {peak}",
            out.burst_speed
        );
    }

    /// A fast right wrist produces exactly one impulse slot with a mirrored
    /// world position and a bounded gain; slots past it are zeroed.
    #[test]
    fn bake_bakes_wrist_impulse_with_mirror_mapping() {
        let settings = RadianceSettings::default(); // mirror = true
        let body = fixture_body(Vec3::new(0.8, 0.0, 0.0)); // fast +u sweep
        let (_, out) = bake(&settings, &neutral_audio(), Some(&body), 500);
        assert_eq!(out.impulse_count, 1, "one moving limb -> one slot");
        let imp = out.impulses[0];
        // Wrist at UV (0.7, 0.4), mirrored. `fit_to_height` (the default) maps
        // the square mask by the window height (1080), so world x =
        // (1-0.7-0.5)*1080 = -216; world y = (0.5-0.4)*1080 = 108.
        assert!(
            (imp.position[0] - -216.0).abs() < 1e-3,
            "{:?}",
            imp.position
        );
        assert!((imp.position[1] - 108.0).abs() < 1e-3, "{:?}", imp.position);
        // Mirrored +u velocity points -x in world.
        assert!(imp.velocity[0] < 0.0);
        assert!(imp.gain > 0.0 && imp.gain <= 1.0);
        assert!(
            (out.impulses[1].gain).abs() < f32::EPSILON,
            "stale slots zeroed"
        );
    }

    /// Absent body / present-but-still body bakes zero impulses.
    #[test]
    fn bake_no_body_means_no_impulses() {
        let settings = RadianceSettings::default();
        let (_, out) = bake(&settings, &neutral_audio(), None, 500);
        assert_eq!(out.impulse_count, 0);
        let still = fixture_body(Vec3::ZERO);
        let (_, out) = bake(&settings, &neutral_audio(), Some(&still), 500);
        assert_eq!(out.impulse_count, 0, "resting limbs shed nothing");
    }

    /// Edge count clamps to the contract capacity.
    #[test]
    fn bake_clamps_edge_count() {
        let settings = RadianceSettings::default();
        let (_, out) = bake(&settings, &neutral_audio(), None, MAX_EDGE_POINTS * 4);
        assert_eq!(
            out.edge_count,
            u32::try_from(MAX_EDGE_POINTS).expect("fits")
        );
    }

    /// The per-bake frame counter advances every call (it salts the kernel's
    /// respawn hash), even when `elapsed` is pinned — the exact case the old
    /// `u32(time * 60.0)` salt aliased on.
    #[test]
    fn frame_counter_increments_each_bake_even_with_pinned_time() {
        let settings = RadianceSettings::default();
        let mut state = RadianceState::default();
        let mut out = RadianceSimParamsGpu::default();
        assert_eq!(out.frame, 0, "zeroed default");
        for expected in 1..=5 {
            bake_radiance_sim(
                &settings,
                &neutral_audio(),
                None,
                100,
                Vec2::new(1920.0, 1080.0),
                1.0 / 60.0,
                7.0, // pinned elapsed: the old time-based salt would not advance
                &mut state,
                &mut out,
            );
            assert_eq!(out.frame, expected, "frame must advance per bake");
        }
    }

    /// The freeze hook zeroes emission and burst, nothing else.
    #[test]
    fn freeze_zeroes_emission() {
        let mut world = World::new();
        let settings = RadianceSettings::default();
        let (_, params) = bake(&settings, &neutral_audio(), None, 500);
        world.insert_resource(RadianceSimParams {
            params,
            particles: Handle::default(),
            particle_count: 1000,
        });
        bevy::ecs::system::RunSystemOnce::run_system_once(&mut world, freeze_radiance_emission)
            .expect("freeze runs");
        let sim = world.resource::<RadianceSimParams>();
        assert!(sim.params.emission_prob.abs() < f32::EPSILON);
        assert!(sim.params.burst_speed.abs() < f32::EPSILON);
        assert!(
            sim.params.flow_strength > 0.0,
            "flow untouched (fade-out drifts)"
        );
    }
}
