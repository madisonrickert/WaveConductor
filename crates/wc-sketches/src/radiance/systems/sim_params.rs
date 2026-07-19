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
use wc_core::input::body::{
    BodyTrackingState, SilhouetteEdges, MAX_EDGE_POINTS, MAX_TRACKED_BODIES,
};

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
pub const EMISSION_BASE_HZ: f32 = 0.2;
/// Onset envelope exponential release time constant, seconds.
pub const ONSET_DECAY_SECS: f32 = 0.18;
/// Onset envelope clamp (spectral flux is unbounded above).
pub const ONSET_MAX: f32 = 2.0;
/// Outward burst speed at full onset envelope, world px/s. A gentle global
/// push — the drama of a hit lives in the ejecta layer (see
/// [`EJECTA_SPEED`]), so this stays small enough that the flame body swells
/// rather than detaching wholesale.
pub const BURST_SPEED: f32 = 90.0;
/// Spawn offset along the outward normal, world px.
pub const SPAWN_OFFSET: f32 = 4.0;
/// Baseline spawn speed along the outward normal, world px/s.
pub const SPAWN_SPEED: f32 = 70.0;
/// Particle lifespan range, seconds.
pub const LIFESPAN_MIN: f32 = 0.8;
/// See [`LIFESPAN_MIN`].
pub const LIFESPAN_MAX: f32 = 2.2;
/// Ejecta launch speed at neutral intensity, world px/s (the "shooting
/// particles" of an onset hit; the render shader streaks anything this fast).
pub const EJECTA_SPEED: f32 = 480.0;
/// Baseline fraction of spawns that are ejecta with **zero** onset — a few
/// stray sparks keep the flame alive-looking between hits.
pub const EJECTA_BASE_FRACTION: f32 = 0.01;
/// Extra ejecta fraction at full onset envelope (scaled by the
/// `ejecta_amount` setting).
pub const EJECTA_ONSET_FRACTION: f32 = 0.35;
/// Flame-tongue spatial frequency, radians per world px (~300 px wavelength:
/// two to three licking tongues across a standing figure).
pub const TONGUE_FREQ: f32 = 0.017;
/// Emission-share boost for an igniting body (fade rising through the low
/// range): the appearing dancer's flame catches with a visible flare while
/// the *total* budget stays constant (weights are normalized).
pub const IGNITE_BOOST: f32 = 2.5;
/// Fade ceiling below which a rising body still counts as igniting.
pub const IGNITE_FADE_CEIL: f32 = 0.7;
/// Velocity fraction remaining after one second of drag.
pub const DRAG_PER_SECOND: f32 = 0.25;
/// Curl spatial frequency, radians per world px (~785 px swirl wavelength).
pub const CURL_SCALE: f32 = 0.012;
/// Limb impulse influence radius, world px.
pub const IMPULSE_RADIUS: f32 = 140.0;
/// Limb speed (world px/s) that maps to impulse gain 1.0.
pub const IMPULSE_FULL_SPEED: f32 = 900.0;
/// Smoothing time constant for the intensity/sparkle envelopes, seconds.
pub const ENVELOPE_SMOOTH_SECS: f32 = 0.25;
/// Time constant of the slow per-aggregate running means the band drives are
/// normalized by (see [`band_drive`]). Long enough to track a song section,
/// short enough to re-adapt across a DJ transition.
pub const BAND_NORM_TAU_S: f32 = 8.0;
/// Floor on the bass running mean: silence must not normalize the noise
/// floor up into a full drive.
pub const BASS_AVG_FLOOR: f32 = 0.02;
/// Floor on the highs running mean. Far lower than the bass floor: a party
/// room mic delivers almost no absolute energy above 1.6 kHz (measured
/// p90 ≈ 0.004 on real material), so the highs lane is useful only as a
/// *relative* signal — but it still needs a floor against amplified hiss.
pub const HIGHS_AVG_FLOOR: f32 = 1.0e-3;

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
    /// Slow running mean of the bass aggregate ([`band_drive`] reference).
    pub bass_avg: f32,
    /// Slow running mean of the highs aggregate ([`band_drive`] reference).
    pub highs_avg: f32,
    /// Palette hue-rotation phase in `0..1` (wraps; 1 = one full spectrum
    /// rotation). Advanced by `hue_cycle_speed`, accelerated by bass.
    pub hue_phase: f32,
    /// Smoothed bass drive (`0..1`, the [`band_drive`]-normalized bass lane):
    /// the beat-weighted "flame swell" signal shared by the billboard-size
    /// breathing and the beat-pulse strength.
    pub bass_drive: f32,
    /// Previous frame's per-slot fade envelopes — the ignite detector
    /// compares against these to spot a body fading *in* (see
    /// [`emission_slot_weights`]).
    pub slot_fade_prev: [f32; MAX_TRACKED_BODIES],
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
    /// Normalized bass drive `0..1` (the [`band_drive`] lane the multipliers
    /// above are built from), exposed for the tongue/swell/pulse consumers.
    pub bass: f32,
}

/// Contrast-expanded, room-adaptive band drive: `value` relative to its own
/// slow running mean `reference`, mapped so sitting *at* the mean yields a
/// moderate drive and ~1.5x the mean saturates. Calibrated against real
/// party-room mic material (48 s report, 2026-07-18): the post-AGC bass
/// aggregate spans only ~0.07..0.21 absolute (a 1.1x..1.3x multiplier under
/// the old absolute mapping — visually near-static), but its *ratio* to its
/// own mean spans ~0.45..1.55, which this map stretches across the full
/// `0..1` drive. The highs aggregate is ~50x smaller in absolute terms
/// (p90 ≈ 0.004) yet has 2x ratio dynamics, so relative normalization is the
/// only mapping that makes the sparkle/turbulence lane live on a room mic.
#[must_use]
pub fn band_drive(value: f32, reference: f32) -> f32 {
    // ratio 0.7 → 0.0, ratio 1.5 → 1.0 (clamped outside).
    ((value / reference - 0.7) / 0.8).clamp(0.0, 1.0)
}

/// Map one analysis frame into drive values. Pure and allocation-free.
/// `sensitivity == 0.0` returns the exact neutral drive (all multipliers 1.0)
/// so audio coupling is provably inert at the knob's floor.
///
/// `bass_avg` / `highs_avg` are the slow running means tracked in
/// [`RadianceState`] (floored here so a fresh/silent state cannot divide by
/// ~0); see [`band_drive`] for the normalization rationale.
#[must_use]
pub fn audio_drive(
    audio: &AudioAnalysis,
    sensitivity: f32,
    bass_avg: f32,
    highs_avg: f32,
) -> AudioDrive {
    let s = sensitivity.max(0.0);
    let (bass, highs) = band_aggregates(audio);
    let bass_n = band_drive(bass, bass_avg.max(BASS_AVG_FLOOR));
    let highs_n = band_drive(highs, highs_avg.max(HIGHS_AVG_FLOOR));
    AudioDrive {
        emission_mul: 1.0 + 1.5 * bass_n * s,
        buoyancy_mul: 1.0 + 0.8 * bass_n * s,
        turbulence_mul: 1.0 + 1.6 * highs_n * s,
        sparkle: (highs_n * s).clamp(0.0, 1.0),
        // RMS lifts the floor brightness; each detected beat rides a throb on
        // top (beat_confidence snaps to 1 and decays in ~0.3 s). The 1.7x RMS
        // slope is calibrated to real material (rms p10..p90 ≈ 0.08..0.23 →
        // intensity ~0.63..0.89 before the beat term).
        intensity: 0.5 + (1.7 * audio.rms + 0.3 * audio.beat_confidence) * s,
        onset: (audio.onset * s).clamp(0.0, ONSET_MAX),
        bass: (bass_n * s).clamp(0.0, 1.0),
    }
}

/// The two band aggregates every drive consumer shares: low three bands =
/// bass body (50–400 Hz), top three = air/sparkle (1.6–12.8 kHz).
#[must_use]
pub fn band_aggregates(audio: &AudioAnalysis) -> (f32, f32) {
    let bass = (audio.bands[0] + audio.bands[1] + audio.bands[2]) / 3.0;
    let highs = (audio.bands[5] + audio.bands[6] + audio.bands[7]) / 3.0;
    (bass, highs)
}

/// Apportion the **shared** particle budget across body slots: normalized
/// fade-weighted spawn shares (density stays constant as dancers come and
/// go — four dancers each get a quarter of the flame, not four flames).
///
/// - A slot with no edge points this frame gets zero share (nothing to
///   spawn on).
/// - An *igniting* slot (fade rising through the low range — a dancer
///   appearing) gets an [`IGNITE_BOOST`]× share so its flame catches with a
///   visible flare; the boost shifts share, never raises the total.
/// - When **no** slot carries fade (the attract phantom and the synthetic
///   writers publish mask/edges without `TrackedBody` entries), shares fall
///   back to each slot's edge-count proportion so those single-body paths
///   keep their flame.
/// - All-zero output (no edges anywhere) means "spawn nothing".
#[must_use]
pub fn emission_slot_weights(
    fades: [f32; MAX_TRACKED_BODIES],
    igniting: [bool; MAX_TRACKED_BODIES],
    counts: [usize; MAX_TRACKED_BODIES],
) -> [f32; MAX_TRACKED_BODIES] {
    let mut weights = [0.0_f32; MAX_TRACKED_BODIES];
    let mut sum = 0.0_f32;
    for i in 0..MAX_TRACKED_BODIES {
        if counts[i] == 0 {
            continue;
        }
        let boost = if igniting[i] { IGNITE_BOOST } else { 1.0 };
        weights[i] = fades[i].clamp(0.0, 1.0) * boost;
        sum += weights[i];
    }
    if sum <= f32::EPSILON {
        // Phantom/synthetic fallback: no tracked fades but edges exist.
        let total: usize = counts.iter().sum();
        if total == 0 {
            return [0.0; MAX_TRACKED_BODIES];
        }
        #[allow(
            clippy::as_conversions,
            clippy::cast_precision_loss,
            reason = "edge counts are bounded by MAX_EDGE_POINTS (2048), exact in f32"
        )]
        for (w, &c) in weights.iter_mut().zip(counts.iter()) {
            *w = c as f32 / total as f32;
        }
        return weights;
    }
    for w in &mut weights {
        *w /= sum;
    }
    weights
}

/// Fold normalized weights into the monotone CDF the kernel samples
/// (`pick_slot`: first `i` with `rand < cdf[i]`). All-zero weights stay an
/// all-zero CDF, which the kernel reads as "no live slot".
#[must_use]
pub fn weights_to_cdf(weights: [f32; MAX_TRACKED_BODIES]) -> [f32; MAX_TRACKED_BODIES] {
    let mut cdf = [0.0_f32; MAX_TRACKED_BODIES];
    let mut acc = 0.0_f32;
    for (c, w) in cdf.iter_mut().zip(weights.iter()) {
        acc += w;
        *c = acc;
    }
    cdf
}

/// One baker, two writers (live + screensaver) — flame's Condition A1.
///
/// Advances the [`RadianceState`] envelopes (onset attack/release, smoothed
/// intensity/sparkle/bass, palette shift), then writes every field of the
/// kernel uniform: audio-scaled emission/buoyancy/turbulence, the
/// beat-weighted swell, the onset ejecta lane, the per-slot edge ranges +
/// fade-weighted emission CDF (multi-body budget apportioning), the
/// mask-UV→world transform for the current window + mirror setting, and up
/// to [`MAX_IMPULSES`] limb impulse slots fanned across every present body.
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
    bodies: Option<&BodyTrackingState>,
    slot_counts: [usize; MAX_TRACKED_BODIES],
    window_size: Vec2,
    dt: f32,
    elapsed: f32,
    state: &mut RadianceState,
    out: &mut RadianceSimParamsGpu,
) {
    let dt = dt.min(DT_CAP);
    let sensitivity = settings.audio_sensitivity.max(0.0);
    // Advance the slow per-aggregate running means the band drives are
    // normalized by (room-adaptive contrast expansion — see `band_drive`).
    let (bass_raw, highs_raw) = band_aggregates(audio);
    let kn = 1.0 - (-dt / BAND_NORM_TAU_S).exp();
    state.bass_avg += (bass_raw - state.bass_avg) * kn;
    state.highs_avg += (highs_raw - state.highs_avg) * kn;
    let drive = audio_drive(audio, sensitivity, state.bass_avg, state.highs_avg);

    // Onset envelope: instant attack to the incoming strength, exponential
    // release — so one drum hit reads as one burst, not a sustained gale.
    let released = state.onset_env * (-dt / ONSET_DECAY_SECS).exp();
    state.onset_env = released.max(drive.onset);
    // Smoothed intensity/sparkle/bass (one-pole toward the drive targets).
    let k = 1.0 - (-dt / ENVELOPE_SMOOTH_SECS).exp();
    state.intensity += (drive.intensity - state.intensity) * k;
    state.sparkle += (drive.sparkle - state.sparkle) * k;
    state.bass_drive += (drive.bass - state.bass_drive) * k;
    // Palette drifts slowly, faster under bass (audio-shifted gradient).
    state.palette_shift =
        (state.palette_shift + dt * (0.02 + 0.10 * (drive.emission_mul - 1.0))).fract();
    // Hue rotation phase: the psychedelic full-spectrum drift. Base rate from
    // the setting, accelerated up to ~2.8x by the bass drive so heavy
    // sections push the whole palette around the wheel. The mid/high lane
    // adds a subtle shimmer-rate term (spec: highs drive color shimmer,
    // never the big pulses).
    state.hue_phase = (state.hue_phase
        + dt * settings.hue_cycle_speed
            * (1.0 + 0.6 * (drive.emission_mul - 1.0) + 0.4 * state.sparkle))
        .fract();

    // Beat swell: the debounced beat lane pumps emission + buoyancy so the
    // whole flame visibly SWELLS on the beat (bass-weighted per the spec —
    // this multiplies the bass-derived drive, it does not replace it).
    let beat_swell = 1.0 + 0.5 * audio.beat_confidence * sensitivity.min(1.5);

    out.dt = dt;
    out.time = elapsed;
    // Monotonic per-bake counter salting the kernel's respawn hash. Wraps
    // freely (the hash tolerates it) and, unlike the old `u32(time * 60.0)`
    // salt, never aliases when `elapsed` is pinned or two bakes fall in the
    // same 1/60 s bucket.
    out.frame = out.frame.wrapping_add(1);
    out.emission_prob =
        (settings.emission_rate * drive.emission_mul * beat_swell * EMISSION_BASE_HZ * dt)
            .clamp(0.0, 1.0);
    out.spawn_offset = SPAWN_OFFSET;
    out.spawn_speed = SPAWN_SPEED * (0.6 + 0.4 * state.intensity);
    out.burst_speed = state.onset_env * BURST_SPEED;
    out.buoyancy = settings.buoyancy * drive.buoyancy_mul * beat_swell;
    out.flow_strength = settings.flow_strength * drive.turbulence_mul;
    out.curl_scale = CURL_SCALE;
    out.curl_octaves = settings.curl_octaves.clamp(1, 3);
    out.drag_baked = DRAG_PER_SECOND.powf(dt);
    out.lifespan_min = LIFESPAN_MIN;
    out.lifespan_max = LIFESPAN_MAX;
    out.mirror = u32::from(settings.mirror);

    // Ejecta lane: onsets convert a fraction of spawns into fast shooting
    // sparks (the kernel rolls per spawn; render streaks them by velocity).
    out.ejecta_prob = (settings.ejecta_amount
        * (EJECTA_BASE_FRACTION + EJECTA_ONSET_FRACTION * (state.onset_env / ONSET_MAX)))
        .clamp(0.0, 1.0);
    out.ejecta_speed = EJECTA_SPEED * (0.8 + 0.4 * state.intensity);
    // Flame tongues: buoyancy noise amplitude breathes with the bass drive
    // (the tongue multiplier can dip briefly ~zero at full strength + full
    // bass — a transient local downdraft reads as organic flicker).
    out.tongue_amp = settings.tongue_strength * (0.55 + 0.5 * state.bass_drive);
    out.tongue_freq = TONGUE_FREQ;

    // Per-slot edge ranges: `SilhouetteEdges` concatenates slots ascending,
    // so starts are the prefix sums; counts clamp so `start + count` stays
    // inside the uploaded MAX_EDGE_POINTS prefix.
    let mut start = 0_usize;
    let mut fades = [0.0_f32; MAX_TRACKED_BODIES];
    let mut igniting = [false; MAX_TRACKED_BODIES];
    let mut clamped_counts = [0_usize; MAX_TRACKED_BODIES];
    for i in 0..MAX_TRACKED_BODIES {
        let clamped = slot_counts[i].min(MAX_EDGE_POINTS.saturating_sub(start));
        out.slot_start[i] = start as u32;
        out.slot_count[i] = clamped as u32;
        clamped_counts[i] = clamped;
        start += clamped;
    }
    out.edge_count = start as u32;

    // Per-slot fades + the ignite detector (fade rising through the low
    // range = a dancer appearing; their flame catches with a flare).
    if let Some(bodies) = bodies {
        for body in bodies.iter_bodies() {
            if body.slot < MAX_TRACKED_BODIES {
                let fade = body.fade.clamp(0.0, 1.0);
                fades[body.slot] = fade;
                igniting[body.slot] =
                    fade > state.slot_fade_prev[body.slot] + 1e-4 && fade < IGNITE_FADE_CEIL;
            }
        }
    }
    out.slot_cdf = weights_to_cdf(emission_slot_weights(fades, igniting, clamped_counts));
    state.slot_fade_prev = fades;

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
    bake_impulses(bodies, settings.mirror, out);
    // particle_count is owned by spawn (buffer size); the baker leaves it.
}

/// Fan the limb impulses across EVERY present body in slot order until the
/// eight [`MAX_IMPULSES`] slots fill (one dancer uses at most seven, so a
/// duo always gets at least one slot). Stale slots past the live count are
/// zeroed so a limb dropping out of frame cannot leave a ghost impulse.
#[allow(
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "impulse count <= MAX_IMPULSES (8); usize -> u32 is exact"
)]
fn bake_impulses(bodies: Option<&BodyTrackingState>, mirror: bool, out: &mut RadianceSimParamsGpu) {
    let scale = Vec2::new(out.uv_to_world[0], out.uv_to_world[1]);
    let mut n = 0usize;
    if let Some(bodies) = bodies {
        'bodies: for body in bodies.iter_bodies() {
            if !body.present {
                continue;
            }
            for &lm in &IMPULSE_LANDMARKS {
                if n >= MAX_IMPULSES {
                    break 'bodies;
                }
                let landmark = body.landmarks[lm];
                if landmark.visibility < 0.5 {
                    continue;
                }
                let vel = mask_dir_to_world(
                    Vec2::new(body.velocities[lm].x, body.velocities[lm].y),
                    scale,
                    mirror,
                );
                let gain = (vel.length() / IMPULSE_FULL_SPEED).clamp(0.0, 1.0);
                if gain < 0.05 {
                    continue; // resting limbs shed nothing
                }
                let pos =
                    mask_uv_to_world(Vec2::new(landmark.pos.x, landmark.pos.y), scale, mirror);
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
    for slot in out.impulses.iter_mut().skip(n) {
        *slot = RadianceImpulse::default();
    }
    out.impulse_count = n as u32;
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
    let slot_counts = edges.map_or([0; MAX_TRACKED_BODIES], |e| e.slot_counts);
    let window_size = Vec2::new(window.width(), window.height());
    bake_radiance_sim(
        &settings,
        &audio_frame,
        body.as_deref(),
        slot_counts,
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
    sim.params.ejecta_prob = 0.0;
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use wc_core::input::body::{BodyLandmark, TrackedBody, BODY_LANDMARK_COUNT};

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

    fn fixture_body(wrist_vel: Vec3) -> TrackedBody {
        let mut landmarks = [BodyLandmark::default(); BODY_LANDMARK_COUNT];
        for lm in &mut landmarks {
            lm.visibility = 1.0;
            lm.pos = Vec3::new(0.5, 0.5, 0.0);
        }
        // Right wrist (16) moving.
        landmarks[16].pos = Vec3::new(0.7, 0.4, 0.0);
        let mut velocities = [Vec3::ZERO; BODY_LANDMARK_COUNT];
        velocities[16] = wrist_vel;
        TrackedBody {
            slot: 0,
            present: true,
            fade: 1.0,
            confidence: 0.9,
            landmarks,
            velocities,
            timestamp: std::time::Duration::from_millis(33),
            crop_fraction: 1.0,
            size: 0.2,
            ..TrackedBody::default()
        }
    }

    /// Wrap a single body (in its own slot) into the tracking-state shape
    /// the baker consumes.
    fn tracking_state(body: TrackedBody) -> BodyTrackingState {
        let mut state = BodyTrackingState::default();
        let slot = body.slot.min(MAX_TRACKED_BODIES - 1);
        state.primary = body.present.then_some(slot);
        state.bodies[slot] = Some(body);
        state
    }

    fn bake(
        settings: &RadianceSettings,
        audio: &AudioAnalysis,
        bodies: Option<&BodyTrackingState>,
        edge_count: usize,
    ) -> (RadianceState, RadianceSimParamsGpu) {
        let mut state = RadianceState::default();
        let mut out = RadianceSimParamsGpu::default();
        bake_radiance_sim(
            settings,
            audio,
            bodies,
            [edge_count, 0, 0, 0],
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
        let d = audio_drive(&loud, 0.0, 0.5, 0.5);
        assert!((d.emission_mul - 1.0).abs() < f32::EPSILON);
        assert!((d.buoyancy_mul - 1.0).abs() < f32::EPSILON);
        assert!((d.turbulence_mul - 1.0).abs() < f32::EPSILON);
        assert!(d.sparkle.abs() < f32::EPSILON);
        assert!(d.onset.abs() < f32::EPSILON);
    }

    /// Bass raises emission + buoyancy; highs raise turbulence + sparkle.
    /// References at half the aggregate: ratio 2 saturates both drives.
    #[test]
    fn audio_drive_routes_bands_per_spec() {
        let bassy = fixture_audio([0.9, 0.9, 0.9, 0.0, 0.0, 0.0, 0.0, 0.0], 0.3, 0.0);
        let airy = fixture_audio([0.0, 0.0, 0.0, 0.0, 0.0, 0.9, 0.9, 0.9], 0.3, 0.0);
        let db = audio_drive(&bassy, 1.0, 0.45, 0.45);
        let da = audio_drive(&airy, 1.0, 0.45, 0.45);
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

    /// The relative normalization is the point: a compressed room-mic bass
    /// wiggle (0.15 mean, ±0.06 swing — the measured party-room shape) maps
    /// to a wide drive range instead of the near-static absolute mapping.
    #[test]
    fn band_drive_expands_compressed_room_mic_dynamics() {
        let avg = 0.15;
        let quiet = band_drive(0.09, avg); // p10-ish trough
        let mid = band_drive(0.15, avg); // sitting at the mean
        let peak = band_drive(0.22, avg); // p95-ish hit
        assert!(quiet.abs() < f32::EPSILON, "trough must drop to 0: {quiet}");
        assert!(
            (0.2..=0.6).contains(&mid),
            "at-mean must be moderate: {mid}"
        );
        assert!(peak > 0.9, "hits must approach full drive: {peak}");
    }

    /// Beats throb the intensity target on top of the RMS floor.
    #[test]
    fn audio_drive_intensity_throbs_on_beats() {
        let base = fixture_audio([0.1; 8], 0.16, 0.0);
        let mut on_beat = base;
        on_beat.beat_confidence = 1.0;
        let di = audio_drive(&base, 1.0, 0.1, 0.1).intensity;
        let db = audio_drive(&on_beat, 1.0, 0.1, 0.1).intensity;
        assert!((db - di - 0.3).abs() < 1e-6, "beat adds 0.3: {di} -> {db}");
    }

    /// The baker's running means adapt toward the aggregates, so a sustained
    /// level stops reading as a hit: the emission drive relaxes over time.
    #[test]
    fn bake_normalization_adapts_to_sustained_level() {
        let settings = RadianceSettings::default();
        let mut state = RadianceState::default();
        let mut out = RadianceSimParamsGpu::default();
        let sustained = fixture_audio([0.3; 8], 0.2, 0.0);
        let win = Vec2::new(1920.0, 1080.0);
        let mut first = 0.0;
        // 40 simulated seconds: five BAND_NORM_TAU_S constants, so the mean
        // has fully converged onto the sustained aggregate.
        for i in 0..2400 {
            bake_radiance_sim(
                &settings,
                &sustained,
                None,
                [100, 0, 0, 0],
                win,
                1.0 / 60.0,
                0.0,
                &mut state,
                &mut out,
            );
            if i == 0 {
                first = out.emission_prob;
            }
        }
        assert!(
            out.emission_prob < first,
            "sustained level must relax: first {first}, settled {}",
            out.emission_prob
        );
        assert!(
            (state.bass_avg - 0.3).abs() < 0.02,
            "bass mean converges to the aggregate: {}",
            state.bass_avg
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
            [100, 0, 0, 0],
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
                [100, 0, 0, 0],
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
        let body = tracking_state(fixture_body(Vec3::new(0.8, 0.0, 0.0))); // fast +u sweep
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
        let still = tracking_state(fixture_body(Vec3::ZERO));
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

    /// Fade-weighted apportioning: shares are normalized (constant total
    /// density), zero-edge slots get nothing, and an igniting slot's share
    /// is boosted at its sibling's expense — never the total's.
    #[test]
    fn emission_weights_apportion_by_fade() {
        // Two full-fade bodies with edges split the budget evenly.
        let w = emission_slot_weights([1.0, 1.0, 0.0, 0.0], [false; 4], [300, 300, 0, 0]);
        assert!((w[0] - 0.5).abs() < 1e-6 && (w[1] - 0.5).abs() < 1e-6);
        // A half-faded second body takes a third of the budget.
        let w = emission_slot_weights([1.0, 0.5, 0.0, 0.0], [false; 4], [300, 300, 0, 0]);
        assert!((w[0] - 2.0 / 3.0).abs() < 1e-6 && (w[1] - 1.0 / 3.0).abs() < 1e-6);
        // A slot with fade but no edges spawns nothing.
        let w = emission_slot_weights([1.0, 1.0, 0.0, 0.0], [false; 4], [300, 0, 0, 0]);
        assert!((w[0] - 1.0).abs() < 1e-6 && w[1].abs() < f32::EPSILON);
        // Ignite boost shifts share toward the appearing body; sum stays 1.
        let w = emission_slot_weights(
            [1.0, 0.3, 0.0, 0.0],
            [false, true, false, false],
            [300, 300, 0, 0],
        );
        let boosted = 0.3 * IGNITE_BOOST;
        assert!((w[1] - boosted / (1.0 + boosted)).abs() < 1e-6, "{w:?}");
        assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-6);
    }

    /// Phantom fallback: no fades at all but edges present → edge-count
    /// shares; no edges anywhere → all-zero (spawn nothing).
    #[test]
    #[allow(clippy::float_cmp, reason = "exact zero sentinel comparison")]
    fn emission_weights_phantom_fallback() {
        let w = emission_slot_weights([0.0; 4], [false; 4], [400, 100, 0, 0]);
        assert!((w[0] - 0.8).abs() < 1e-6 && (w[1] - 0.2).abs() < 1e-6);
        let w = emission_slot_weights([0.0; 4], [false; 4], [0; 4]);
        assert_eq!(w, [0.0; 4]);
    }

    /// The CDF is the running sum; all-zero weights stay all-zero (the
    /// kernel's "no live slot" sentinel).
    #[test]
    #[allow(clippy::float_cmp, reason = "exact zero sentinel comparison")]
    fn weights_fold_to_monotone_cdf() {
        let cdf = weights_to_cdf([0.25, 0.25, 0.0, 0.5]);
        assert!((cdf[0] - 0.25).abs() < 1e-6);
        assert!((cdf[1] - 0.5).abs() < 1e-6);
        assert!((cdf[2] - 0.5).abs() < 1e-6);
        assert!((cdf[3] - 1.0).abs() < 1e-6);
        assert_eq!(weights_to_cdf([0.0; 4]), [0.0; 4]);
    }

    /// The baker writes the per-slot ranges and a fade-weighted CDF, clamped
    /// into the uploaded edge prefix.
    #[test]
    #[allow(clippy::float_cmp, reason = "fades pass through the baker unmodified")]
    fn bake_packs_slot_ranges_and_cdf() {
        let settings = RadianceSettings::default();
        let mut state = RadianceState {
            // Pre-seed the previous fades so neither body reads as *rising*
            // (igniting) — this test checks the steady-state shares; the
            // ignite boost has its own test in `emission_weights_apportion_by_fade`.
            slot_fade_prev: [1.0, 0.5, 0.0, 0.0],
            ..RadianceState::default()
        };
        let mut out = RadianceSimParamsGpu::default();
        let mut bodies = BodyTrackingState::default();
        bodies.bodies[0] = Some(TrackedBody {
            slot: 0,
            present: true,
            fade: 1.0,
            ..TrackedBody::default()
        });
        bodies.bodies[1] = Some(TrackedBody {
            slot: 1,
            present: true,
            fade: 0.5,
            ..TrackedBody::default()
        });
        bake_radiance_sim(
            &settings,
            &neutral_audio(),
            Some(&bodies),
            [200, 300, 0, 0],
            Vec2::new(1920.0, 1080.0),
            1.0 / 60.0,
            0.0,
            &mut state,
            &mut out,
        );
        assert_eq!(out.slot_start, [0, 200, 500, 500]);
        assert_eq!(out.slot_count, [200, 300, 0, 0]);
        assert_eq!(out.edge_count, 500);
        // Fades 1.0 / 0.5 → shares 2/3, 1/3 → CDF [2/3, 1, 1, 1].
        assert!(
            (out.slot_cdf[0] - 2.0 / 3.0).abs() < 1e-5,
            "{:?}",
            out.slot_cdf
        );
        assert!((out.slot_cdf[3] - 1.0).abs() < 1e-5);
        assert_eq!(state.slot_fade_prev, [1.0, 0.5, 0.0, 0.0]);
    }

    /// A beat pumps emission + buoyancy over the identical no-beat frame
    /// (the "flame swells on the beat" lane).
    #[test]
    fn bake_beat_swells_emission_and_buoyancy() {
        let settings = RadianceSettings::default();
        let base = fixture_audio([0.2; 8], 0.2, 0.0);
        let mut on_beat = base;
        on_beat.beat_confidence = 1.0;
        let (_, quiet) = bake(&settings, &base, None, 500);
        let (_, thump) = bake(&settings, &on_beat, None, 500);
        assert!(thump.emission_prob > quiet.emission_prob * 1.3);
        assert!(thump.buoyancy > quiet.buoyancy * 1.3);
    }

    /// Onsets raise the ejecta fraction; silence keeps the stray-spark floor;
    /// `ejecta_amount = 0` disables the lane entirely.
    #[test]
    fn bake_onset_drives_ejecta() {
        let settings = RadianceSettings::default();
        let (_, calm) = bake(&settings, &neutral_audio(), None, 500);
        let expect_floor = settings.ejecta_amount * EJECTA_BASE_FRACTION;
        assert!((calm.ejecta_prob - expect_floor).abs() < 1e-6);
        let hit = fixture_audio([0.3; 8], 0.3, 2.0);
        let (_, driven) = bake(&settings, &hit, None, 500);
        assert!(
            driven.ejecta_prob > calm.ejecta_prob * 4.0,
            "{}",
            driven.ejecta_prob
        );
        assert!(driven.ejecta_speed > 0.0);
        let mut off = settings.clone();
        off.ejecta_amount = 0.0;
        let (_, none) = bake(&off, &hit, None, 500);
        assert!(none.ejecta_prob.abs() < f32::EPSILON);
    }

    /// Tongue amplitude follows the setting and breathes with bass; zero
    /// setting pins it off (uniform buoyancy).
    #[test]
    fn bake_bakes_tongue_noise() {
        let settings = RadianceSettings::default();
        let (_, out) = bake(&settings, &neutral_audio(), None, 500);
        assert!(out.tongue_amp > 0.0 && (out.tongue_freq - TONGUE_FREQ).abs() < f32::EPSILON);
        let mut flat = settings.clone();
        flat.tongue_strength = 0.0;
        let (_, out) = bake(&flat, &neutral_audio(), None, 500);
        assert!(out.tongue_amp.abs() < f32::EPSILON);
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
                [100, 0, 0, 0],
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
        assert!(sim.params.ejecta_prob.abs() < f32::EPSILON);
        assert!(
            sim.params.flow_strength > 0.0,
            "flow untouched (fade-out drifts)"
        );
    }
}
