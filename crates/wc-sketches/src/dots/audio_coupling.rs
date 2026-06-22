//! Activity-envelope coupling: mouse + hand attractors → [`DotsSynth`].
//!
//! ## Approach (ENVELOPE-PRIMARY)
//!
//! Dots has no per-frame GPU readback and no CPU particle mirror that carries
//! field statistics (average velocity, variance, entropy). v4's Dots synth
//! derived its filter cutoff and LFO depth from `normVarLen` and `avgVel`
//! (per-particle stats computed over the full fabric). Reproducing those in
//! v5 would require either a synchronous GPU readback (audio latency + stall)
//! or a full CPU mirror of the compute simulation (CPU budget + stale audio).
//!
//! Instead a single attack/release ACTIVITY envelope driven by
//! [`DotsMouseAttractorState::power`] and [`super::hand_attractors::DotsHandAttractor::power`]
//! stands in for all three v4 control signals. The envelope rises toward 1.0
//! while any attractor is active (mouse `power > 0.0` OR any hand power above
//! [`HAND_ACTIVITY_THRESHOLD`]) and decays toward 0.0 after all release.
//! "Loudest hand wins": the max over all tracked hands is used so a second,
//! farther hand cannot duck a near grab — mirrors Line's `update_hand_audio_drive`
//! pattern. Volume, bandpass cutoff, and LFO depth all derive from this single
//! envelope value each frame.
//!
//! ## What this writes each frame
//!
//! Three `SetDotsParam` commands per frame, pushed onto the lock-free
//! [`AudioCommandSender`] ring:
//!
//! - `"volume"` = `env × breath × synth_volume_scale`, clamped `≥ 0`.
//! - `"bandpass_freq"` = `(base + envelope × range) × breath`, sweeping from
//!   `bandpass_base_hz` to `base + bandpass_range_hz` with activity, further
//!   modulated by the breath LFO. Approximation of v4's per-particle stats;
//!   all three tuning parameters live in [`DotsSettings`].
//! - `"lfo_depth"` = `bandpass_freq × 0.06` (v4's exact relation, preserved).
//!
//! ## Breath
//!
//! A slow sine gated by the activity envelope recreates v4's "low warm pulse
//! following the in-out particle motion":
//!
//! ```text
//! breath = 1 + depth × env × sin(TAU × rate_hz × t)
//! ```
//!
//! Because the swell is scaled by `env`, at rest (`env = 0`) `breath = 1.0` —
//! the breath has no effect when the fabric is silent. Volume and cutoff both
//! receive the same breath so the two parameters stay perceptually coupled.
//!
//! ## Known gap: LFO rate
//!
//! v4's `flatRatio` (cloud aspect ratio) drove the LFO oscillator rate so the
//! LFO would breathe faster mid-press when the cloud was circular and slower
//! during the elongated post-release tail. Without particle stats this term is
//! **not synthesized** — the [`DotsSynth`]'s LFO rate is fixed at the
//! hardcoded 8.66 Hz. This is a known perceptual deviation from v4; accepted
//! as the ENVELOPE-PRIMARY tradeoff (no GPU readback, no CPU mirror).
//!
//! ## Ring-full handling
//!
//! If the audio ring is full (audio thread severely backlogged), the dropped
//! command logs at `warn` once per occurrence and the frame's `SetDotsParam`
//! is skipped. A dropped frame of a param-set is survivable — the parameter
//! holds its last value for one extra frame.
//!
//! [`DotsSynth`]: wc_core::audio::dots_synth::DotsSynth

use bevy::ecs::system::NonSendMut;
use bevy::prelude::*;

use wc_core::audio::command::AudioCommand;
use wc_core::audio::ring::AudioCommandSender;
use wc_core::input::entity::TrackedHand;

use super::hand_attractors::DotsHandAttractor;
use super::settings::DotsSettings;
use super::systems::DotsMouseAttractorState;

// ── Hand-activity threshold ───────────────────────────────────────────────────

/// Minimum [`DotsHandAttractor::power`] that counts as "active" for the audio
/// envelope.
///
/// The Dots power model floors at [`super::hand_attractors::DOTS_HAND_POWER_FLOOR`]
/// (0.05) — any power below that is hard-zeroed, so values between zero and the
/// floor never appear in practice. This threshold sits just above numerical
/// noise (`1e-2 < 0.05`) and below the floor, meaning any non-zero power
/// produced by `dots_leap_power` is treated as active. Consistent with the
/// sim-feed `1e-2` convention documented in the plan.
pub(crate) const HAND_ACTIVITY_THRESHOLD: f32 = 1e-2;

// ── v4 constants ──────────────────────────────────────────────────────────────

/// v4's `lfoGain.gain = bandpassFreq × 0.06` constant, preserved exactly.
///
/// LFO modulation depth tracks the bandpass cutoff so the wobble's musical
/// interval stays consistent as the cutoff sweeps. Mirrors the same constant
/// in [`crate::line::audio_coupling`].
const LFO_DEPTH_OVER_CUTOFF: f32 = 0.06;

// ── Resource ──────────────────────────────────────────────────────────────────

/// Smoothed attack/release activity envelope for Dots audio.
///
/// A scalar in `[0, 1]` that rises toward 1.0 while any Dots attractor is
/// active (mouse `power > 0.0` OR any `DotsHandAttractor::power >
/// HAND_ACTIVITY_THRESHOLD`) and decays toward 0.0 after all attractors
/// release. Advanced each frame by [`drive_dots_audio`] using the asymmetric
/// attack/release rates derived from [`DotsSettings::synth_attack_ms`] and
/// [`DotsSettings::synth_release_ms`].
///
/// Initialised to 0.0 (`Default`) by the plugin. Persists across `OnEnter`/
/// `OnExit` cycles — the synth is removed on exit and rebuilt on entry, so a
/// residual non-zero value on re-entry means the envelope begins from its last
/// state rather than cold-starting (perceptually acceptable and consistent with
/// how Line handles its `ParticleStats` resource).
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct DotsAudioEnvelope(pub f32);

// ── Pure helpers (unit-testable) ──────────────────────────────────────────────

/// Advance the [`DotsAudioEnvelope`] value by one frame.
///
/// Pure function: given the current envelope value, combined attractor power,
/// frame delta in seconds, and explicit attack/release rates, returns the next
/// envelope value clamped to `[0, 1]`. Extracted from [`drive_dots_audio`] so
/// the envelope math is unit-testable without a Bevy `World`. Production code
/// derives rates from [`DotsSettings::synth_attack_ms`] and
/// [`DotsSettings::synth_release_ms`] before calling here.
///
/// `active_power` is the pre-computed maximum over the mouse and all hand
/// attractor powers — see [`dots_activity_power`]. The envelope target is
/// derived from a simple threshold:
///
/// - `active_power > 0.0` → target 1.0, advance at `attack_rate` (s⁻¹).
/// - `active_power == 0.0` → target 0.0, decay at `release_rate` (s⁻¹).
pub(crate) fn step_dots_envelope(
    envelope: f32,
    active_power: f32,
    dt: f32,
    attack_rate: f32,
    release_rate: f32,
) -> f32 {
    let target = if active_power > 0.0 { 1.0_f32 } else { 0.0_f32 };
    let rate = if target > envelope {
        attack_rate
    } else {
        release_rate
    };
    // Per-frame exponential follow: `lerp(current, target, rate × dt)`.
    // `(rate × dt).min(1.0)` prevents overshoot when the frame time is large
    // (e.g. hitching). Clamp ensures floating-point noise can't escape [0, 1].
    (envelope + (target - envelope) * (rate * dt).min(1.0)).clamp(0.0, 1.0)
}

/// Derive the combined activity power from the mouse and all tracked hands.
///
/// Returns the maximum scalar "active" power across all sources so that the
/// loudest hand wins and a second, farther hand cannot duck a near grab.
/// Specifically:
///
/// - Mouse contributes its `power` directly (any `> 0.0` means mouse is active).
/// - Each hand contributes its [`DotsHandAttractor::power`] if it exceeds
///   [`HAND_ACTIVITY_THRESHOLD`] (otherwise contributes `0.0`).
/// - The result is the max over all contributions, clamped to `[0, 1]`.
///
/// Extracted for unit-testability: the system passes its query-gathered values
/// here; tests can call directly with synthetic inputs without a Bevy `World`.
pub(crate) fn dots_activity_power(mouse_power: f32, max_hand_power: f32) -> f32 {
    // Any non-zero mouse power counts as active (mirrors the pre-D5 condition).
    let mouse_active = if mouse_power > 0.0 { mouse_power } else { 0.0 };
    // Hands: threshold-gate to avoid residual near-zero noise from the EMA.
    let hand_active = if max_hand_power > HAND_ACTIVITY_THRESHOLD {
        max_hand_power
    } else {
        0.0
    };
    mouse_active.max(hand_active).clamp(0.0, 1.0)
}

/// Derive the `"bandpass_freq"` param value from the activity envelope.
///
/// `bandpass_freq = base + envelope × range`, sweeping from `base` Hz at
/// silence to `base + range` Hz at full activity. Both `base` and `range`
/// come from [`DotsSettings`] so they are operator-tunable at runtime.
///
/// This is an envelope approximation of v4's `120 / normVarLen × avgVel / 100`
/// formula, which required per-particle stats unavailable in v5 without a CPU
/// mirror or GPU readback. The base and range are the primary tuning targets
/// for the operator's hardware sign-off.
///
/// Extracted for unit-testability.
pub(crate) fn dots_bandpass_freq(envelope: f32, base: f32, range: f32) -> f32 {
    base + envelope * range
}

/// Derive the `"lfo_depth"` param value from the bandpass cutoff.
///
/// Preserves v4's exact relation: `lfoGain.gain = bandpassFreq × 0.06`.
/// The LFO modulation depth tracks the bandpass cutoff so the wobble's musical
/// interval stays consistent as the cutoff sweeps. Mirrors the same constant
/// in [`crate::line::audio_coupling`].
///
/// Extracted for unit-testability.
pub(crate) fn dots_lfo_depth(bandpass_freq: f32) -> f32 {
    bandpass_freq * LFO_DEPTH_OVER_CUTOFF
}

// ── System ────────────────────────────────────────────────────────────────────

/// `Update` system: advance the activity envelope and push `SetDotsParam`
/// commands to the audio ring.
///
/// Runs each frame while `sketch_active(AppState::Dots)`, gated by the
/// `.run_if(sketch_active(AppState::Dots))` added in [`super::DotsPlugin::build`].
///
/// The envelope is advanced **before** the `audio_cmd` early-return so tests
/// without an `AudioCommandSender` can still observe the envelope state via
/// `Res<DotsAudioEnvelope>`.
///
/// The envelope target is "any active attractor": it rises while the mouse is
/// active OR any [`DotsHandAttractor`] power exceeds [`HAND_ACTIVITY_THRESHOLD`].
/// The loudest hand wins (max over all hands) so a second, farther hand cannot
/// duck a near grab — mirrors Line's `update_hand_audio_drive` pattern.
///
/// ## LFO rate gap
///
/// See module-level docs. The LFO oscillator rate is **not** driven here; it
/// stays fixed at `DotsSynth`'s hardcoded 8.66 Hz. This is a deliberate
/// ENVELOPE-PRIMARY tradeoff.
pub fn drive_dots_audio(
    mouse: Res<'_, DotsMouseAttractorState>,
    hands: Query<'_, '_, &DotsHandAttractor, With<TrackedHand>>,
    time: Res<'_, Time>,
    settings: Res<'_, DotsSettings>,
    audio_cmd: Option<NonSendMut<'_, AudioCommandSender>>,
    mut envelope: ResMut<'_, DotsAudioEnvelope>,
) {
    // Loudest hand wins: fold max over all tracked hands without allocating.
    let max_hand_power = hands.iter().fold(0.0_f32, |acc, h| acc.max(h.power));

    // Combine mouse and hand activity into a single scalar; clamp to [0, 1].
    let active = dots_activity_power(mouse.power, max_hand_power);

    // Derive envelope rates from settings (ms → s⁻¹).
    let attack_rate = 1000.0 / settings.synth_attack_ms;
    let release_rate = 1000.0 / settings.synth_release_ms;

    // Advance the envelope every frame — even when the audio engine is absent
    // (headless tests) — so the resource reflects the current activity state.
    envelope.0 = step_dots_envelope(
        envelope.0,
        active,
        time.delta_secs(),
        attack_rate,
        release_rate,
    );

    // The audio engine is not started in headless integration tests (no cpal
    // device). Skip ring pushes cleanly when the sender is absent.
    let Some(mut audio_cmd) = audio_cmd else {
        return;
    };

    let env = envelope.0;

    // Modeled in-out swell: a slow sine, scaled by the activity envelope so it
    // is silent at rest and swells in with the press. Recreates v4's "low warm
    // pulse following the in-out particle motion" without GPU field stats.
    let t = time.elapsed_secs();
    let breath = 1.0
        + settings.breath_depth
            * env
            * (core::f32::consts::TAU * settings.breath_rate_hz * t).sin();

    // Apply breath and volume trim; clamp to avoid any negative volume.
    let volume = (env * breath * settings.synth_volume_scale).max(0.0);
    // Apply breath to the cutoff so volume and filter swell together.
    let bandpass_freq =
        dots_bandpass_freq(env, settings.bandpass_base_hz, settings.bandpass_range_hz) * breath;
    // v4's `lfoGain.gain = bandpassFreq × 0.06`, preserved exactly.
    let lfo_depth = dots_lfo_depth(bandpass_freq);

    // `"volume"` = envelope × breath × volume trim. DotsSynth derives noise
    // path gain internally as `volume × 0.05` (v4 parity).
    push_dots_audio(
        &mut audio_cmd,
        AudioCommand::SetDotsParam {
            key: "volume",
            value: volume,
        },
    );
    // `"bandpass_freq"`: settings-driven warm cutoff with breath modulation.
    // Tune `bandpass_base_hz` / `bandpass_range_hz` by ear at hardware sign-off.
    push_dots_audio(
        &mut audio_cmd,
        AudioCommand::SetDotsParam {
            key: "bandpass_freq",
            value: bandpass_freq,
        },
    );
    // `"lfo_depth"`: v4's `lfoGain.gain = freq × 0.06`, preserved exactly.
    push_dots_audio(
        &mut audio_cmd,
        AudioCommand::SetDotsParam {
            key: "lfo_depth",
            value: lfo_depth,
        },
    );
}

/// Push an [`AudioCommand`] onto the Dots audio ring.
///
/// Logs at `warn` if the ring is full and the command is dropped. Non-fatal:
/// the parameter holds its last value for one frame.
fn push_dots_audio(sender: &mut AudioCommandSender, command: AudioCommand) {
    if let Err(_dropped) = sender.push(command) {
        tracing::warn!("audio command ring full; dropping Dots param update");
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic float arithmetic is the test subject"
)]
#[allow(
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected None/Err is the correct behaviour"
)]
mod tests {
    use super::*;

    // Rates matching DotsSettings defaults: attack = 1000/115 s⁻¹, release = 1000/350 s⁻¹.
    const TEST_ATTACK_RATE: f32 = 1000.0 / 115.0;
    const TEST_RELEASE_RATE: f32 = 1000.0 / 350.0;
    // Bandpass defaults used in parameterized tests.
    const TEST_BASE_HZ: f32 = 110.0;
    const TEST_RANGE_HZ: f32 = 280.0;

    // ── Envelope step: direction and bounds ───────────────────────────────

    /// Envelope rises from 0 toward 1 when the attractor is active.
    #[test]
    fn envelope_rises_when_power_nonzero() {
        let after = step_dots_envelope(0.0, 1.0, 1.0 / 60.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
        assert!(
            after > 0.0,
            "envelope must rise when power > 0; got {after}"
        );
        assert!(after <= 1.0, "envelope must stay <= 1.0; got {after}");
    }

    /// Envelope decays from 1 toward 0 when the attractor is released.
    #[test]
    fn envelope_decays_when_power_zero() {
        let after = step_dots_envelope(1.0, 0.0, 1.0 / 60.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
        assert!(
            after < 1.0,
            "envelope must decay when power == 0; got {after}"
        );
        assert!(after >= 0.0, "envelope must stay >= 0.0; got {after}");
    }

    /// A huge dt (frame hitch) must not push the envelope outside `[0, 1]`.
    #[test]
    fn envelope_stays_in_unit_interval_with_extreme_dt() {
        let at_peak = step_dots_envelope(0.5, 1.0, 100.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
        assert!(
            (0.0..=1.0).contains(&at_peak),
            "envelope out of [0,1] on attack with dt=100: {at_peak}"
        );
        let at_floor = step_dots_envelope(0.5, 0.0, 100.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
        assert!(
            (0.0..=1.0).contains(&at_floor),
            "envelope out of [0,1] on release with dt=100: {at_floor}"
        );
    }

    /// Envelope must rise monotonically across 20 frames while power > 0.
    #[test]
    fn envelope_rises_monotonically_across_frames() {
        let mut env = 0.0_f32;
        for frame in 0..20 {
            let next =
                step_dots_envelope(env, 1.0, 1.0 / 60.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
            assert!(
                next >= env,
                "envelope decreased on active frame {frame}: {env} -> {next}"
            );
            env = next;
        }
        assert!(
            env > 0.0,
            "envelope must have risen above 0 after 20 active frames"
        );
    }

    /// Envelope must decay monotonically across 20 frames while power == 0.
    #[test]
    fn envelope_decays_monotonically_across_frames() {
        let mut env = 1.0_f32;
        for frame in 0..20 {
            let next =
                step_dots_envelope(env, 0.0, 1.0 / 60.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
            assert!(
                next <= env,
                "envelope increased on idle frame {frame}: {env} -> {next}"
            );
            env = next;
        }
        assert!(
            env < 1.0,
            "envelope must have decayed below 1 after 20 idle frames"
        );
    }

    // ── Param derivations ─────────────────────────────────────────────────

    /// At envelope = 0, `bandpass_freq` equals the base cutoff.
    #[test]
    fn bandpass_freq_at_zero_envelope_equals_base() {
        assert_eq!(
            dots_bandpass_freq(0.0, TEST_BASE_HZ, TEST_RANGE_HZ),
            TEST_BASE_HZ,
            "bandpass_freq at env=0 must equal base ({TEST_BASE_HZ} Hz)"
        );
    }

    /// At envelope = 1, `bandpass_freq` equals base + range.
    #[test]
    fn bandpass_freq_at_full_envelope_equals_base_plus_range() {
        assert_eq!(
            dots_bandpass_freq(1.0, TEST_BASE_HZ, TEST_RANGE_HZ),
            TEST_BASE_HZ + TEST_RANGE_HZ,
            "bandpass_freq at env=1 must equal BASE ({TEST_BASE_HZ}) + RANGE ({TEST_RANGE_HZ})"
        );
    }

    /// `lfo_depth == bandpass_freq * 0.06` at every sample point.
    #[test]
    fn lfo_depth_equals_bandpass_times_point_zero_six() {
        for env in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
            let bp = dots_bandpass_freq(env, TEST_BASE_HZ, TEST_RANGE_HZ);
            let depth = dots_lfo_depth(bp);
            assert_eq!(
                depth,
                bp * LFO_DEPTH_OVER_CUTOFF,
                "lfo_depth != bandpass_freq * 0.06 at env={env} (bp={bp})"
            );
        }
    }

    /// At the default base cutoff (110 Hz), LFO depth is 6.6 Hz (110 * 0.06).
    #[test]
    fn lfo_depth_at_base_cutoff_is_six_point_six_hz() {
        let depth = dots_lfo_depth(TEST_BASE_HZ);
        assert!(
            (depth - 6.6).abs() < 1e-4,
            "lfo_depth at base cutoff must be 6.6 Hz; got {depth}"
        );
    }

    /// At the default peak cutoff (110 + 280 = 390 Hz), LFO depth is 23.4 Hz.
    #[test]
    fn lfo_depth_at_peak_cutoff_is_twenty_three_point_four_hz() {
        let depth = dots_lfo_depth(TEST_BASE_HZ + TEST_RANGE_HZ);
        assert!(
            (depth - 23.4).abs() < 1e-3,
            "lfo_depth at peak cutoff must be 23.4 Hz; got {depth}"
        );
    }

    /// The envelope stays in `[0, 1]` across the four boundary cases, ensuring
    /// the downstream volume clamp can't see negative input from the envelope.
    #[test]
    fn volume_param_equals_envelope_and_stays_in_unit_interval() {
        // The meaningful guarantee is that step_dots_envelope always keeps the
        // value in [0, 1], so volume can never clip or go negative.
        for &(power, init) in &[(1.0_f32, 0.0_f32), (0.0, 1.0), (1.0, 1.0), (0.0, 0.0)] {
            let env =
                step_dots_envelope(init, power, 1.0 / 60.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
            assert!(
                (0.0..=1.0).contains(&env),
                "volume out of [0,1]: power={power}, init={init}, env={env}"
            );
        }
    }

    // ── Breath modulation ─────────────────────────────────────────────────

    /// With envelope = 0, breath must be exactly 1.0 for any time value.
    /// Because `breath = 1 + depth * env * sin(...)` and env = 0, the sine
    /// term drops out entirely — rest stays unmodulated and silent.
    #[test]
    fn breath_at_zero_envelope_is_unity() {
        let depth = 0.3_f32;
        let rate_hz = 0.7_f32;
        for t in [0.0_f32, 0.1, 0.5, 1.0, 3.7, 100.0] {
            let breath = 1.0 + depth * 0.0 * (core::f32::consts::TAU * rate_hz * t).sin();
            assert_eq!(
                breath, 1.0,
                "breath must be 1.0 when env=0 (t={t}); got {breath}"
            );
        }
    }

    /// With envelope = 1 and depth = 0.3, breath stays within [0.7, 1.3]
    /// across a time sweep. This confirms the breath swell is bounded and
    /// never makes volume or cutoff negative.
    #[test]
    fn breath_modulates_within_bounds() {
        let depth = 0.3_f32;
        let rate_hz = 0.7_f32;
        let env = 1.0_f32;
        // Sample 1000 time points covering several full cycles.
        // Use u16 (max 65535 fits exactly in f32's 23-bit mantissa).
        for i in 0..1000_u16 {
            let t = f32::from(i) * 0.01;
            let breath = 1.0 + depth * env * (core::f32::consts::TAU * rate_hz * t).sin();
            assert!(
                breath >= 1.0 - depth,
                "breath below lower bound at t={t}: {breath} < {}",
                1.0 - depth
            );
            assert!(
                breath <= 1.0 + depth,
                "breath above upper bound at t={t}: {breath} > {}",
                1.0 + depth
            );
        }
    }

    // ── dots_activity_power: combined mouse + hand scalar ─────────────────

    /// Mouse active with no hands: activity power equals mouse power (clamped).
    #[test]
    fn activity_power_mouse_only() {
        let p = dots_activity_power(1.0, 0.0);
        assert!((p - 1.0).abs() < 1e-6, "mouse-only must give 1.0; got {p}");
    }

    /// Hand above threshold with mouse inactive: activity power equals hand
    /// power, confirming a hand alone can drive the envelope.
    #[test]
    fn activity_power_hand_only_above_threshold() {
        // 0.5 > HAND_ACTIVITY_THRESHOLD (0.01) -> hand contribution 0.5
        let p = dots_activity_power(0.0, 0.5);
        assert!(
            (p - 0.5).abs() < 1e-6,
            "hand-only above threshold must give 0.5; got {p}"
        );
    }

    /// Hand below threshold with mouse inactive: activity power is 0.0.
    #[test]
    fn activity_power_hand_below_threshold_is_zero() {
        // 0.005 < HAND_ACTIVITY_THRESHOLD (0.01) -> no hand contribution
        let p = dots_activity_power(0.0, 0.005);
        assert!(
            p == 0.0,
            "hand below threshold with inactive mouse must give 0.0; got {p}"
        );
    }

    /// Both inactive: activity power is 0.0 (envelope should decay).
    #[test]
    fn activity_power_both_inactive_is_zero() {
        let p = dots_activity_power(0.0, 0.0);
        assert!(p == 0.0, "both inactive must give 0.0; got {p}");
    }

    /// Loudest hand wins: when two hand powers are supplied (caller takes max
    /// before calling this function), the result tracks the max, not the sum
    /// or the min.
    #[test]
    fn activity_power_loudest_hand_wins() {
        // Simulate two hands: near grab (power 0.8) and far grab (power 0.2).
        // The caller folds max before passing in, so max_hand_power = 0.8.
        let max_hand = 0.8_f32.max(0.2_f32);
        let p = dots_activity_power(0.0, max_hand);
        // Must equal the louder hand (0.8), not the sum (1.0) or min (0.2).
        assert!(
            (p - 0.8).abs() < 1e-6,
            "loudest hand (0.8) must win over quieter hand (0.2); got {p}"
        );
    }

    /// Mouse + hand: the result is the max, not the sum.
    #[test]
    fn activity_power_mouse_and_hand_takes_max() {
        let p = dots_activity_power(0.6, 0.4);
        assert!(
            (p - 0.6).abs() < 1e-6,
            "mouse (0.6) > hand (0.4): expected max 0.6, got {p}"
        );
        let p2 = dots_activity_power(0.3, 0.7);
        assert!(
            (p2 - 0.7).abs() < 1e-6,
            "hand (0.7) > mouse (0.3): expected max 0.7, got {p2}"
        );
    }

    /// Activity power is always clamped to `[0, 1]`.
    #[test]
    fn activity_power_clamps_to_unit_interval() {
        // Supra-unity inputs (provider over-range) must not escape [0, 1].
        let p = dots_activity_power(2.0, 3.0);
        assert!(
            (0.0..=1.0).contains(&p),
            "activity power must clamp to [0,1]; got {p}"
        );
    }

    // ── Hand activity drives the envelope (pure helper chain) ─────────────

    /// Mouse inactive but a hand above threshold: envelope rises across frames.
    ///
    /// This is the primary D5 Task 2 contract: a hand grab with no mouse activity
    /// must cause the audio envelope to advance toward 1.0.
    #[test]
    fn envelope_rises_with_hand_active_mouse_inactive() {
        // A hand power of 0.5 is well above HAND_ACTIVITY_THRESHOLD (0.01).
        let active = dots_activity_power(0.0, 0.5);
        assert!(
            active > 0.0,
            "hand above threshold must give non-zero active"
        );

        let mut env = 0.0_f32;
        for frame in 0..20 {
            let next =
                step_dots_envelope(env, active, 1.0 / 60.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
            assert!(
                next >= env,
                "envelope must not decrease with hand active (frame {frame}): {env} -> {next}"
            );
            env = next;
        }
        assert!(
            env > 0.0,
            "envelope must have risen with only hand active after 20 frames; got {env}"
        );
    }

    /// Both mouse and hand inactive: envelope decays from a raised state.
    #[test]
    fn envelope_decays_when_both_inactive() {
        let active = dots_activity_power(0.0, 0.0);
        assert!(active == 0.0, "both inactive must give 0.0 active");

        let mut env = 1.0_f32;
        for frame in 0..20 {
            let next =
                step_dots_envelope(env, active, 1.0 / 60.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
            assert!(
                next <= env,
                "envelope must not rise when both inactive (frame {frame}): {env} -> {next}"
            );
            env = next;
        }
        assert!(
            env < 1.0,
            "envelope must have decayed when both inactive after 20 frames; got {env}"
        );
    }

    /// "Loudest hand wins" end-to-end: with two hands, the envelope target
    /// tracks the max power hand, not the sum and not the quieter hand.
    ///
    /// Asserts the max-fold pattern used in `drive_dots_audio` (callers fold
    /// `f32::max` before calling `dots_activity_power`) produces a result
    /// between `0.0` (quieter hand alone, below threshold-gate from env side)
    /// and the louder hand's contribution.
    #[test]
    fn loudest_hand_wins_end_to_end() {
        let hand_near = 0.8_f32; // near, strong grab
        let hand_far = 0.1_f32; // far, weak grab

        // The system folds max before calling dots_activity_power.
        let max_power = hand_near.max(hand_far); // = 0.8
        let active = dots_activity_power(0.0, max_power);

        // Result must equal the near hand contribution, not the sum (which
        // would be clamped to 1.0 and lose the magnitude signal) and not the
        // min (which would misrepresent the dominant hand).
        assert!(
            (active - 0.8).abs() < 1e-6,
            "loudest hand wins: max-folded activity must be 0.8, got {active}"
        );

        // Confirm the envelope rises from this target.
        let env_after =
            step_dots_envelope(0.0, active, 1.0 / 60.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
        assert!(
            env_after > 0.0,
            "envelope must rise from loudest-hand activity; got {env_after}"
        );
    }

    // ── System-level test: envelope advances via RunSystemOnce ────────────

    /// `drive_dots_audio` advances the envelope when run without an
    /// `AudioCommandSender` (headless mode). Verifies the system is wired
    /// correctly and that the early-return on absent audio does not skip the
    /// envelope step.
    ///
    /// No `DotsHandAttractor` entities are spawned — the query iterates zero
    /// rows, contributing `max_hand_power = 0.0`. The mouse alone drives the
    /// envelope here.
    #[test]
    fn drive_dots_audio_advances_envelope_without_audio_sender() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = World::new();
        // Active attractor (power = 1.0).
        world.insert_resource(DotsMouseAttractorState {
            power: 1.0,
            position: [0.0, 0.0],
        });
        // Envelope starts at 0.
        world.insert_resource(DotsAudioEnvelope(0.0));
        // Time with a non-trivial delta: use Bevy's Time resource directly.
        // `Time::default()` has delta_secs() = 0 until advanced; we manually
        // set a realistic frame time.
        let mut time = Time::<()>::default();
        time.advance_by(std::time::Duration::from_millis(16)); // ~60 Hz
        world.insert_resource(time);
        // Settings required now that drive_dots_audio reads Res<DotsSettings>.
        world.insert_resource(DotsSettings::default());
        // No AudioCommandSender inserted — system must skip ring pushes cleanly.
        // No TrackedHand entities — hand query iterates zero rows.

        world
            .run_system_once(drive_dots_audio)
            .expect("drive_dots_audio must run without error");

        let env = world.resource::<DotsAudioEnvelope>().0;
        assert!(
            env > 0.0,
            "drive_dots_audio must raise envelope when power > 0 and audio absent; got {env}"
        );
        assert!(
            env <= 1.0,
            "drive_dots_audio must keep envelope <= 1.0; got {env}"
        );
    }

    /// `drive_dots_audio` raises the envelope when a `DotsHandAttractor` entity
    /// with power above the threshold is present and the mouse is inactive.
    ///
    /// Spawns a synthetic `TrackedHand + DotsHandAttractor` entity (mirroring
    /// Task 1's system test setup in `hand_attractors::tests`) and confirms the
    /// system reads the component and advances the envelope.
    #[test]
    fn drive_dots_audio_raises_envelope_from_hand_alone() {
        use bevy::ecs::system::RunSystemOnce;
        use wc_core::input::entity::TrackedHand;

        let mut world = World::new();
        // Mouse inactive.
        world.insert_resource(DotsMouseAttractorState {
            power: 0.0,
            position: [0.0, 0.0],
        });
        // Envelope starts at 0.
        world.insert_resource(DotsAudioEnvelope(0.0));
        let mut time = Time::<()>::default();
        time.advance_by(std::time::Duration::from_millis(16));
        world.insert_resource(time);
        // Settings required now that drive_dots_audio reads Res<DotsSettings>.
        world.insert_resource(DotsSettings::default());

        // Spawn a TrackedHand with a DotsHandAttractor whose power is well
        // above HAND_ACTIVITY_THRESHOLD (0.01). The value 0.5 simulates a
        // hand mid-grab that has been running for several frames (the EMA
        // climbs from 0.005 on the first frame; 0.5 is a reachable steady
        // state after sustained grabbing).
        world.spawn((
            TrackedHand,
            bevy::prelude::Transform::default(),
            bevy::prelude::Visibility::default(),
            DotsHandAttractor {
                power: 0.5,
                position: bevy::prelude::Vec2::ZERO,
            },
        ));

        world
            .run_system_once(drive_dots_audio)
            .expect("drive_dots_audio must run without error");

        let env = world.resource::<DotsAudioEnvelope>().0;
        assert!(
            env > 0.0,
            "drive_dots_audio must raise envelope from hand alone (mouse inactive); got {env}"
        );
        assert!(env <= 1.0, "envelope must stay <= 1.0; got {env}");
    }

    /// `drive_dots_audio` leaves the envelope decaying when both the mouse is
    /// inactive and all hand powers are below `HAND_ACTIVITY_THRESHOLD`.
    #[test]
    fn drive_dots_audio_decays_envelope_when_both_inactive() {
        use bevy::ecs::system::RunSystemOnce;
        use wc_core::input::entity::TrackedHand;

        let mut world = World::new();
        world.insert_resource(DotsMouseAttractorState {
            power: 0.0,
            position: [0.0, 0.0],
        });
        // Envelope starts raised at 1.0.
        world.insert_resource(DotsAudioEnvelope(1.0));
        let mut time = Time::<()>::default();
        time.advance_by(std::time::Duration::from_millis(16));
        world.insert_resource(time);
        // Settings required now that drive_dots_audio reads Res<DotsSettings>.
        world.insert_resource(DotsSettings::default());

        // Spawn a hand whose power is zeroed (below threshold after floor snap).
        world.spawn((
            TrackedHand,
            bevy::prelude::Transform::default(),
            bevy::prelude::Visibility::default(),
            DotsHandAttractor {
                power: 0.0,
                position: bevy::prelude::Vec2::ZERO,
            },
        ));

        world
            .run_system_once(drive_dots_audio)
            .expect("drive_dots_audio must run without error");

        let env = world.resource::<DotsAudioEnvelope>().0;
        assert!(
            env < 1.0,
            "envelope must decay when both mouse and hands are inactive; got {env}"
        );
        assert!(env >= 0.0, "envelope must stay >= 0.0; got {env}");
    }
}
