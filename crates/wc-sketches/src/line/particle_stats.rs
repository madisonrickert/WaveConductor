//! CPU-side approximated audio control signals for the Line sketch.
//!
//! ## Approach
//!
//! v4 computed full per-particle aggregate statistics each frame (`computeStats`
//! in `src/particles/particleStats.ts` — averageVel, varianceX/Y, entropy,
//! flatRatio, groupedUpness). Plan 7's GPU compute pipeline moved particle
//! physics off-CPU, which would have required either:
//!
//! - a per-frame GPU readback (sync stall, audio reacts to stale visuals), or
//! - a CPU mirror running the same physics twice (Plan 7–10's solution, ~50µs/
//!   frame at 12k particles).
//!
//! Plan 11 Phase F replaces both with smoothed CPU envelopes driven by
//! [`super::systems::MouseAttractorState`] state. The audio coupling doesn't
//! need exact statistics — it needs the right *perceptual shape* of the
//! control signals. `groupedUpness` rising on press, plateauing during hold,
//! decaying after release: that shape can be captured by a handful of
//! attack/release envelopes at near-zero CPU cost (~1µs/frame).
//!
//! Cymatics (v4) is the architectural reference: its audio coupling reads
//! CPU-side input scalars (`activeRadius`, `numCycles`, etc.) that drive the
//! GPU compute simulation, never reading the simulation state back. Phase F
//! brings Line in line with that pattern.
//!
//! ## Perceptual deviation
//!
//! This is a *perceptual* approximation, not mathematical parity. Values
//! within each field's expected range, dynamics within expected attack/
//! release shapes, but a side-by-side comparison against v4 will show
//! frame-by-frame numerical differences. Signed off as an approved deviation
//! in `PARITY.md`.
//!
//! ## Tuning
//!
//! The constants at the bottom of this module (`ATTACK_RATE_FAST`,
//! `GROUPED_UPNESS_PEAK`, `SPREAD_MIN`, etc.) are tunable knobs that shape
//! the musical response. If a specific downstream synth parameter sounds
//! wrong on manual sign-off, adjust the relevant constant and re-test.

use bevy::prelude::*;
use wc_core::input::entity::TrackedHand;

use super::leap_attractors::LineHandAttractor;
use super::settings::LineSettings;
use super::systems::mouse::{MouseAttractorState, MOUSE_POWER_FLOOR};

/// User-configurable envelope rates, derived from [`LineSettings`].
///
/// The four time-constants that shape the audio envelope live in
/// `LineSettings` as ms / s for human readability; this struct converts them
/// to lerp rates (`1 / time_constant`) once per frame so `step_envelope`
/// stays a pure function. Defaults reproduce the hand-tuned constants from
/// before settings exposure (Plan 11 Phase F).
#[derive(Debug, Clone, Copy)]
pub struct EnvelopeRates {
    /// Rising-edge lerp rate for `grouped_upness`. Derived from
    /// [`LineSettings::synth_attack_ms`].
    pub grouped_upness_attack: f32,
    /// Falling-edge lerp rate for `grouped_upness`. Derived from
    /// [`LineSettings::synth_release_ms`].
    pub grouped_upness_release: f32,
    /// Rising-edge lerp rate for the pad evolution envelope. Derived from
    /// [`LineSettings::synth_evolution_attack_s`].
    pub evolution_attack: f32,
    /// Falling-edge lerp rate for the pad evolution envelope. Derived from
    /// [`LineSettings::synth_evolution_release_s`].
    pub evolution_release: f32,
}

impl Default for EnvelopeRates {
    /// Defaults match the hand-tuned constants from before settings
    /// exposure. Anyone calling `step_envelope` from a test or stand-alone
    /// context gets the same envelope shape Madison signed off on.
    fn default() -> Self {
        Self {
            grouped_upness_attack: 1000.0 / 40.0,   // 40 ms → 25 Hz
            grouped_upness_release: 1000.0 / 670.0, // 670 ms → ~1.5 Hz
            evolution_attack: 1.0 / 4.0,            // 4 s → 0.25 Hz
            evolution_release: 1.0 / 6.0,           // 6 s → ~0.17 Hz
        }
    }
}

impl EnvelopeRates {
    /// Convert the human-facing time constants in [`LineSettings`] to lerp
    /// rates. Guards against zero / negative times (which would produce
    /// infinite or NaN rates) by clamping at 1 ms / 0.1 s minimums.
    pub fn from_settings(settings: &LineSettings) -> Self {
        Self {
            grouped_upness_attack: 1000.0 / settings.synth_attack_ms.max(1.0),
            grouped_upness_release: 1000.0 / settings.synth_release_ms.max(1.0),
            evolution_attack: 1.0 / settings.synth_evolution_attack_s.max(0.1),
            evolution_release: 1.0 / settings.synth_evolution_release_s.max(0.1),
        }
    }
}

/// Statistics over the current particle population.
///
/// All values are dimensionless or normalised. Default is all-zero; produced
/// by Bevy when the resource is first initialised and populated each frame
/// by [`update_particle_stats`] via smoothed attack/release envelopes keyed
/// on [`MouseAttractorState`]. See module rustdoc for the perceptual-
/// approximation rationale.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct ParticleStats {
    /// RMS speed across all particles, in world-pixel units per second.
    /// Approximated: snaps up on press, decays slowly on release via
    /// [`ATTACK_RATE_FAST`] / [`RELEASE_RATE_SLOW`] envelopes.
    pub average_vel: f32,
    /// 2D variance length `sqrt(varX² + varY²)` over particle positions.
    /// Approximated: high at rest (particles spread along spawn line),
    /// drops during press (cluster forms around attractor).
    pub variance_length: f32,
    /// Position-cloud aspect ratio `varianceX / varianceY`. Held at 1.0
    /// (circular cloud) for this approximation. Tune if the LFO frequency
    /// target sounds off during manual sign-off.
    pub flat_ratio: f32,
    /// `sqrt(average_vel / variance_length)`. High when particles are moving
    /// fast *and* tightly clustered — the v4 "gathering" condition that gates
    /// most of the musical reactivity.
    pub grouped_upness: f32,
    /// Entropy of particle-to-centroid distances, normalised. Approximated
    /// from `variance_length` (same scalar, different downstream formula).
    pub normalized_entropy: f32,
    /// `variance_length` divided by the v4 normalisation constant
    /// `0.28866 × width`. Approximated from `variance_length`.
    pub normalized_variance_length: f32,
    /// `average_vel` divided by canvas width. Approximated from
    /// `average_vel`.
    pub normalized_average_vel: f32,
    /// **Pad evolution envelope** — slow follow on press state (~4 s
    /// attack, ~6 s release). Ranges `[0, 1]`. Drives modulator-depth
    /// growth + filter-cutoff opening in the `LineSynth` DSP graph so the
    /// patch develops dramatically over a sustained press, the classic
    /// pad-synthesis "filter envelope" technique. Not in v4 (v4 has no
    /// equivalent — the patch character is constant per press); v5
    /// improvement.
    pub evolution: f32,
}

/// Per-frame approximated CPU audio control signals.
///
/// Reads the effective attractor power (max of [`MouseAttractorState::power`]
/// and any active [`LineHandAttractor`] scaled into the mouse-power range),
/// smooths it into envelopes shaped like v4's full per-particle reduction,
/// and writes [`ParticleStats`]. See module rustdoc for the
/// perceptual-approximation rationale.
///
/// The hand-attractor branch closes the audio loop that Plan 11.6 Phase 11.3
/// opened when it removed the pinch-stub bridge from `mouse.rs`. Without
/// this, a Leap grab fed the sim's attractor uniform via
/// [`super::leap_attractors::LineHandAttractor`] but never reached the
/// audio envelope — so visually the particles converged on the hand while
/// the synth stayed silent.
pub fn update_particle_stats(
    mouse: Res<'_, MouseAttractorState>,
    hand_attractors: Query<'_, '_, &LineHandAttractor, With<TrackedHand>>,
    time: Res<'_, Time>,
    settings: Res<'_, LineSettings>,
    mut stats: ResMut<'_, ParticleStats>,
) {
    let rates = EnvelopeRates::from_settings(&settings);
    let effective_power = effective_attractor_power(mouse.power, &hand_attractors, &settings);
    step_envelope(&mut stats, effective_power, time.delta_secs(), &rates);
}

/// Pick the loudest active attractor across the mouse and all tracked hands.
///
/// Hand attractor power is in raw v4 grab-power units (peak ~12 for a fully
/// closed fist near the device); the mouse attractor power is in
/// `gravity_constant`-scaled units (peak ~`gravity_constant * MOUSE_POWER_PRESS`).
/// To compare them on equal footing, scale the hand power by
/// `settings.gravity_constant` — matching how `update_sim_params` feeds
/// hand attractors into the particle uniform.
fn effective_attractor_power(
    mouse_power: f32,
    hand_attractors: &Query<'_, '_, &LineHandAttractor, With<TrackedHand>>,
    settings: &LineSettings,
) -> f32 {
    let hand_max = hand_attractors
        .iter()
        .map(|a| a.power * settings.gravity_constant)
        .fold(0.0_f32, f32::max);
    mouse_power.max(hand_max)
}

/// Pure-function step: advance the envelope state by one frame of `dt` seconds
/// given the current attractor `power` and the configurable envelope `rates`.
/// Extracted from `update_particle_stats` so unit tests can drive the same
/// math without a Bevy `World` / `Res` / `ResMut` setup. Production code only
/// ever calls this through the system wrapper above.
pub(crate) fn step_envelope(stats: &mut ParticleStats, power: f32, dt: f32, rates: &EnvelopeRates) {
    // Normalised attractor activity in [0, 1]. Drives all downstream envelopes.
    //
    // Divide by `MOUSE_POWER_FLOOR` (not `MOUSE_POWER_PRESS`) so excitement
    // stays at 1.0 throughout the held period. `decay_mouse_attractor` brings
    // power asymptotically to the floor; with this normalisation, anything
    // power ≥ floor clamps to excitement = 1.0, matching v4 where
    // groupedUpness stays elevated during sustained press (particles keep
    // orbiting the attractor). Power == 0 (explicit release) is the only
    // way excitement reaches 0.
    let excitement = (power / MOUSE_POWER_FLOOR).clamp(0.0, 1.0);

    // average_vel: snaps up on press, decays slowly. Asymmetric attack/release
    // matches v4's behavior where particles accelerate fast (forces are strong
    // near the attractor) but slow gradually due to drag after release.
    let target_vel = excitement;
    let vel_rate = if target_vel > stats.average_vel {
        ATTACK_RATE_FAST
    } else {
        RELEASE_RATE_SLOW
    };
    stats.average_vel = lerp(stats.average_vel, target_vel, (vel_rate * dt).min(1.0));

    // grouped_upness: the load-bearing audio scalar (synth volume + shader G).
    //
    // **Perceptual curve**: target = excitement^0.6. Sub-linear so the volume
    // rises faster at low excitement and saturates near peak — matches how the
    // ear perceives loudness (Stevens' power law for sound). Addresses v4's
    // "onset feels mushy" calibration weakness; pressing now feels louder
    // sooner.
    //
    // **Asymmetric attack/release**: 50 Hz attack (~20 ms time constant) so
    // the press is *immediately* audible; 6.7 Hz release (~150 ms time
    // constant) so the tail decays cleanly without clicking. The previous
    // symmetric 3 Hz rate produced a slow ramp-up that the v4 comparison
    // capture showed lagged v4's actual envelope by ~80 ms — audibly off.
    let target_grouped = excitement.powf(GROUPED_UPNESS_CURVE_EXPONENT) * GROUPED_UPNESS_PEAK;
    let grouped_rate = if target_grouped > stats.grouped_upness {
        rates.grouped_upness_attack
    } else {
        rates.grouped_upness_release
    };
    stats.grouped_upness = lerp(
        stats.grouped_upness,
        target_grouped,
        (grouped_rate * dt).min(1.0),
    );

    // variance_length: high at rest (particles spread along the horizontal spawn
    // line), drops during press (cluster forms around attractor). Mapped so
    // 1.0 = full spread, SPREAD_MIN = tight cluster.
    //
    // Asymmetric attack/release: clumping on press happens fast (in v4 the
    // particles clump within ~0.2-0.3 s of the gravity well's activation),
    // while re-spreading after release follows the same slow inertial decay
    // as `average_vel`. Note the inverted direction: variance *decreases* on
    // press, so the "attack" branch fires when target < current.
    let target_variance = SPREAD_BASELINE - excitement * (SPREAD_BASELINE - SPREAD_MIN);
    let variance_rate = if target_variance < stats.variance_length {
        ATTACK_RATE_FAST
    } else {
        RELEASE_RATE_SLOW
    };
    stats.variance_length = lerp(
        stats.variance_length,
        target_variance,
        (variance_rate * dt).min(1.0),
    );

    // normalized_variance_length tracks variance_length directly: drives the
    // noise filter cutoff (`2000 × value`), which sweeps 200 Hz (peak press,
    // value ≈ SPREAD_MIN) → 2000 Hz (rest, value ≈ SPREAD_BASELINE). Matches
    // v4's noise lowpass floor.
    stats.normalized_variance_length = stats.variance_length;

    // normalized_entropy tracks variance_length directly. v4 reference capture
    // (2026-05-26) shows `normalizedEntropy` and `normalizedVarianceLength`
    // have near-identical means during press (0.78 vs 0.79) with floors
    // 0.40 and 0.45 respectively. Since `variance_length` floors at
    // [`SPREAD_MIN`] = 0.45, `normalized_entropy` also bottoms at 0.45 →
    // bandpass cutoff cap = 222 / 0.45 ≈ 493 Hz, close to v4's observed max
    // of 554 Hz. The [`ENTROPY_FLOOR`] guard is still applied as a safety
    // floor against future tuning that drops variance_length lower.
    stats.normalized_entropy = stats.variance_length.max(ENTROPY_FLOOR);

    // normalized_average_vel follows average_vel directly.
    stats.normalized_average_vel = stats.average_vel;

    // flat_ratio: drives the LFO oscillator rate via `lfo_rate_hz`. v4's
    // `flatRatio` is the cloud's `varianceX / varianceY` — high when the
    // cloud is spread (rest or post-release relaxation), low when tightly
    // clustered (peak press). v4 capture (2026-05-26) shows mean ≈ 3.58
    // during press and mean ≈ 6.96 during the release tail; the LFO stays
    // elevated *after* release because the cloud's stretched-then-relaxing
    // shape produces a high aspect ratio for several seconds.
    //
    // Approximate by tying `flat_ratio` directly to `variance_length`'s
    // envelope: high variance → high flat_ratio → slow-to-medium LFO; low
    // variance → low flat_ratio → faster LFO. Variance's asymmetric
    // attack/release (fast clumping on press, slow recovery after release)
    // gives flat_ratio the right post-release elevated character "for free."
    let variance_normalised =
        ((stats.variance_length - SPREAD_MIN) / (SPREAD_BASELINE - SPREAD_MIN)).clamp(0.0, 1.0);
    stats.flat_ratio =
        FLAT_RATIO_MIN + variance_normalised * (FLAT_RATIO_BASELINE - FLAT_RATIO_MIN);

    // **Evolution envelope** — slow follow on grouped_upness, normalised to
    // [0, 1]. ~4 s attack, ~6 s release. Drives modulator-depth growth +
    // filter-cutoff opening in the LineSynth DSP graph. The asymmetric rate
    // is what makes the patch *develop* over a held press: voice swells in
    // fast (via grouped_upness), but the texture/filter take seconds to
    // fully bloom (via evolution).
    let target_evolution = (stats.grouped_upness / GROUPED_UPNESS_PEAK).clamp(0.0, 1.0);
    let evolution_rate = if target_evolution > stats.evolution {
        rates.evolution_attack
    } else {
        rates.evolution_release
    };
    stats.evolution = lerp(
        stats.evolution,
        target_evolution,
        (evolution_rate * dt).min(1.0),
    );
}

/// Linear interpolation. `t` is clamped to [0, 1] by the caller before this is
/// reached, so no clamp inside.
fn lerp(current: f32, target: f32, t: f32) -> f32 {
    current + (target - current) * t
}

// Tuning constants — initial values from architectural review. Adjust during
// manual sign-off if specific musical effects need shaping.

/// Attack rate for `average_vel` rising. Higher = snappier press response.
const ATTACK_RATE_FAST: f32 = 8.0;

/// Release rate for `average_vel` and `variance_length`. Lower = longer audio
/// tail after release. v4's particle drag (`PULLING_DRAG=0.93075`) takes ~1s
/// for velocity to halve; this rate matches that decay shape.
const RELEASE_RATE_SLOW: f32 = 1.5;

/// Exponent of the perceptual loudness curve applied to `excitement` before
/// it scales `grouped_upness`. Sub-linear (0.6 < 1.0): low excitement maps to
/// proportionally higher `grouped_upness`, matching the ear's logarithmic
/// loudness response (Stevens' power law). Pressing feels louder sooner; full
/// excitement still hits the same peak ([`GROUPED_UPNESS_PEAK`]).
///
/// Not user-tunable — the curve shape is a perceptual fixed law, not a
/// flavor knob.
const GROUPED_UPNESS_CURVE_EXPONENT: f32 = 0.6;

/// Peak `grouped_upness` value at sustained-press excitement = 1.0. Tuned
/// against the v4 reference capture (2026-05-26): v4's `groupedUpness`
/// during sustained press swings 0.55–2.59 with brief excursions higher,
/// mean ≈ 1.4, peak ≈ 2.5. The downstream synth volume formula is
/// `max(grouped_upness - 0.05, 0) * 5`, so this cap of 2.5 produces a
/// volume peak of `12.25` — matching v4's observed peak (12.68). Earlier
/// values of 0.7 and 0.9 capped volume far below v4's expressive range.
///
/// **Approved perceptual deviation:** v4's `groupedUpness` swings widely
/// *within* a single press because it tracks actual particle dynamics
/// (clustering, cursor motion). Our excitement-driven envelope produces
/// only the smooth attack/release shape, not the within-press jitter.
/// Documented in `PARITY.md`.
const GROUPED_UPNESS_PEAK: f32 = 2.5;

/// `variance_length` at rest (no attractor activity). Particles spread across
/// the canvas; normalised to 1.0 as the "fully spread" baseline.
const SPREAD_BASELINE: f32 = 1.0;

/// `variance_length` at sustained-press peak excitement = 1.0. v4 reference
/// capture (2026-05-26) shows `normalizedVarianceLength` floors at ~0.45
/// during tight press, not the ~0.1 Opus-extrapolation suggested. The
/// downstream noise filter cutoff is `2000 * normalized_variance_length`,
/// so this floor of 0.45 produces a cutoff of 900 Hz at peak clustering —
/// matches v4's noise floor (901 Hz min during press).
const SPREAD_MIN: f32 = 0.45;

/// Lower clamp for `normalized_entropy`. The bandpass cutoff formula is
/// `222 / normalized_entropy`. v4 reference capture (2026-05-26) shows
/// `normalizedEntropy` min ≈ 0.40 during press (bandpass max ≈ 555 Hz).
/// This floor of 0.4 caps the v5 bandpass at 555 Hz, matching v4. Earlier
/// floor of 0.2 capped at 1110 Hz — twice v4's actual peak, audibly bright.
const ENTROPY_FLOOR: f32 = 0.4;

/// `flat_ratio` at rest and during the post-release tail. v4 reference
/// capture (2026-05-26) shows `flatRatio` mean ≈ 6.96 *after release* (the
/// elongated cloud relaxes slowly), versus mean ≈ 3.58 *during press*. The
/// LFO rate stays elevated on the audio tail. We approximate by tying
/// `flat_ratio` to `variance_length`: high variance (spread cloud, rest or
/// recovering) → high `flat_ratio` → slow-to-medium LFO breathing; low
/// variance (clustered, peak press) → low `flat_ratio` → faster LFO.
const FLAT_RATIO_BASELINE: f32 = 7.0;

/// `flat_ratio` at peak press. v4 reference capture shows `flatRatio` min
/// during press ≈ 1.0 (centered cluster). Setting min slightly above v4 (3.0)
/// keeps the LFO in audible-breathing territory at peak press; below ~1 Hz
/// becomes a single-cycle-per-second sweep rather than perceived LFO.
const FLAT_RATIO_MIN: f32 = 3.0;

#[cfg(test)]
#[path = "particle_stats_tests.rs"]
mod tests;
