//! Activity-envelope coupling: [`DotsMouseAttractorState`] → [`DotsSynth`].
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
//! [`DotsMouseAttractorState::power`] stands in for all three v4 control
//! signals. The envelope rises toward 1.0 while the attractor is active
//! (`power > 0.0`) and decays toward 0.0 after release. Volume, bandpass
//! cutoff, and LFO depth all derive from this single envelope value each frame.
//!
//! ## What this writes each frame
//!
//! Three `SetDotsParam` commands per frame, pushed onto the lock-free
//! [`AudioCommandSender`] ring:
//!
//! - `"volume"` = `activity_envelope` (clamped `[0, 1]`).
//! - `"bandpass_freq"` = `200 + envelope × 1800` Hz, sweeping 200→2000 Hz
//!   with activity. Approximation of v4's `120/normVarLen × avgVel/100`
//!   (which required per-particle stats); documented as a tuning target.
//! - `"lfo_depth"` = `bandpass_freq × 0.06` (v4's exact relation, preserved).
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

use super::systems::DotsMouseAttractorState;

// ── v4 constants ──────────────────────────────────────────────────────────────

/// v4's `lfoGain.gain = bandpassFreq × 0.06` constant, preserved exactly.
///
/// LFO modulation depth tracks the bandpass cutoff so the wobble's musical
/// interval stays consistent as the cutoff sweeps. Mirrors the same constant
/// in [`wc_sketches::line::audio_coupling`].
const LFO_DEPTH_OVER_CUTOFF: f32 = 0.06;

// ── Envelope-to-frequency mapping ────────────────────────────────────────────

/// Base bandpass cutoff (Hz) when the activity envelope is at zero (silence).
///
/// This is the low-end anchor of the envelope-to-frequency sweep. At rest
/// (no attractor active) the fabric is still and the cutoff parks here.
/// Operator-tunable by ear; this constant is the primary lever.
///
/// Approximation of v4's `120 / normVarLen × avgVel / 100` formula at the
/// idle (high variance, zero velocity) end of its range.
const BANDPASS_BASE_HZ: f32 = 200.0;

/// Bandpass cutoff sweep range (Hz) across the full `[0, 1]` activity envelope.
///
/// `bandpass_freq = BANDPASS_BASE_HZ + envelope × BANDPASS_RANGE_HZ`
/// sweeps 200 Hz (silence) → 2000 Hz (full activity), covering roughly a
/// decade of cutoff travel across the vocal / upper-midrange band. The
/// 200–2000 Hz window was chosen to approximate v4's observed cutoff range
/// during press; operator tunes by ear at the next hardware checkpoint.
const BANDPASS_RANGE_HZ: f32 = 1800.0;

// ── Envelope time constants ───────────────────────────────────────────────────

/// Attack rate (s⁻¹) for the activity envelope's rising edge.
///
/// Mirrors Line's `synth_attack_ms = 115 ms` default:
/// `rate = 1000 / 115 ≈ 8.70 s⁻¹`. Per-frame lerp step is `rate × Δt`,
/// giving ~63% of target in one attack time-constant (exponential follow).
/// Operator tunes by ear — this constant is the "press snappiness" knob.
const ENVELOPE_ATTACK_RATE: f32 = 1000.0 / 115.0; // ≈ 8.70 s⁻¹

/// Release rate (s⁻¹) for the activity envelope's falling edge.
///
/// Mirrors Line's `synth_release_ms = 350 ms` default:
/// `rate = 1000 / 350 ≈ 2.86 s⁻¹`. Gives a ~350 ms exponential tail after
/// the attractor is released. Operator tunes by ear — this constant is the
/// "tail length" knob.
const ENVELOPE_RELEASE_RATE: f32 = 1000.0 / 350.0; // ≈ 2.86 s⁻¹

// ── Resource ──────────────────────────────────────────────────────────────────

/// Smoothed attack/release activity envelope for Dots audio.
///
/// A scalar in `[0, 1]` that rises toward 1.0 while the Dots mouse attractor
/// is active (`DotsMouseAttractorState::power > 0.0`) and decays toward 0.0
/// after release. Advanced each frame by [`drive_dots_audio`] using the
/// asymmetric rates [`ENVELOPE_ATTACK_RATE`] / [`ENVELOPE_RELEASE_RATE`].
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
/// Pure function: given the current envelope value, attractor power, and frame
/// delta in seconds, returns the next envelope value clamped to `[0, 1]`.
/// Extracted from [`drive_dots_audio`] so the envelope math is unit-testable
/// without a Bevy `World`. Production code calls this through the system.
///
/// - `power > 0.0` → target 1.0, advance at [`ENVELOPE_ATTACK_RATE`].
/// - `power == 0.0` → target 0.0, decay at [`ENVELOPE_RELEASE_RATE`].
pub(crate) fn step_dots_envelope(envelope: f32, power: f32, dt: f32) -> f32 {
    let target = if power > 0.0 { 1.0_f32 } else { 0.0_f32 };
    let rate = if target > envelope {
        ENVELOPE_ATTACK_RATE
    } else {
        ENVELOPE_RELEASE_RATE
    };
    // Per-frame exponential follow: `lerp(current, target, rate × dt)`.
    // `(rate × dt).min(1.0)` prevents overshoot when the frame time is large
    // (e.g. hitching). Clamp ensures floating-point noise can't escape [0, 1].
    (envelope + (target - envelope) * (rate * dt).min(1.0)).clamp(0.0, 1.0)
}

/// Derive the `"bandpass_freq"` param value from the activity envelope.
///
/// `bandpass_freq = BANDPASS_BASE_HZ + envelope × BANDPASS_RANGE_HZ`
/// sweeps from 200 Hz (silence) to 2000 Hz (full activity).
///
/// This is an envelope approximation of v4's `120 / normVarLen × avgVel / 100`
/// formula, which required per-particle stats unavailable in v5 without a CPU
/// mirror or GPU readback. The 200–2000 Hz range is the primary tuning target
/// for the operator's hardware sign-off.
///
/// Extracted for unit-testability.
pub(crate) fn dots_bandpass_freq(envelope: f32) -> f32 {
    BANDPASS_BASE_HZ + envelope * BANDPASS_RANGE_HZ
}

/// Derive the `"lfo_depth"` param value from the bandpass cutoff.
///
/// Preserves v4's exact relation: `lfoGain.gain = bandpassFreq × 0.06`.
/// At 200 Hz base the LFO depth is 12 Hz; at 2000 Hz peak the depth is
/// 120 Hz — a decade of modulation depth travel alongside the cutoff sweep.
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
/// ## LFO rate gap
///
/// See module-level docs. The LFO oscillator rate is **not** driven here; it
/// stays fixed at `DotsSynth`'s hardcoded 8.66 Hz. This is a deliberate
/// ENVELOPE-PRIMARY tradeoff.
pub fn drive_dots_audio(
    mouse: Res<'_, DotsMouseAttractorState>,
    time: Res<'_, Time>,
    audio_cmd: Option<NonSendMut<'_, AudioCommandSender>>,
    mut envelope: ResMut<'_, DotsAudioEnvelope>,
) {
    // Advance the envelope every frame — even when the audio engine is absent
    // (headless tests) — so the resource reflects the current activity state.
    envelope.0 = step_dots_envelope(envelope.0, mouse.power, time.delta_secs());

    // The audio engine is not started in headless integration tests (no cpal
    // device). Skip ring pushes cleanly when the sender is absent.
    let Some(mut audio_cmd) = audio_cmd else {
        return;
    };

    let env = envelope.0;
    let bandpass_freq = dots_bandpass_freq(env);
    let lfo_depth = dots_lfo_depth(bandpass_freq);

    // `"volume"` = activity envelope directly. The DotsSynth derives the noise
    // path gain internally as `volume × 0.05` (v4 parity).
    push_dots_audio(
        &mut audio_cmd,
        AudioCommand::SetDotsParam {
            key: "volume",
            value: env,
        },
    );
    // `"bandpass_freq"`: envelope-mapped cutoff (200→2000 Hz). Approximation
    // of v4's variance/velocity stat; tune by ear at hardware sign-off.
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

    // ── Envelope step: direction and bounds ───────────────────────────────

    /// Envelope rises from 0 toward 1 when the attractor is active.
    #[test]
    fn envelope_rises_when_power_nonzero() {
        let after = step_dots_envelope(0.0, 1.0, 1.0 / 60.0);
        assert!(
            after > 0.0,
            "envelope must rise when power > 0; got {after}"
        );
        assert!(after <= 1.0, "envelope must stay ≤ 1.0; got {after}");
    }

    /// Envelope decays from 1 toward 0 when the attractor is released.
    #[test]
    fn envelope_decays_when_power_zero() {
        let after = step_dots_envelope(1.0, 0.0, 1.0 / 60.0);
        assert!(
            after < 1.0,
            "envelope must decay when power == 0; got {after}"
        );
        assert!(after >= 0.0, "envelope must stay ≥ 0.0; got {after}");
    }

    /// A huge dt (frame hitch) must not push the envelope outside `[0, 1]`.
    #[test]
    fn envelope_stays_in_unit_interval_with_extreme_dt() {
        let at_peak = step_dots_envelope(0.5, 1.0, 100.0);
        assert!(
            (0.0..=1.0).contains(&at_peak),
            "envelope out of [0,1] on attack with dt=100: {at_peak}"
        );
        let at_floor = step_dots_envelope(0.5, 0.0, 100.0);
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
            let next = step_dots_envelope(env, 1.0, 1.0 / 60.0);
            assert!(
                next >= env,
                "envelope decreased on active frame {frame}: {env} → {next}"
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
            let next = step_dots_envelope(env, 0.0, 1.0 / 60.0);
            assert!(
                next <= env,
                "envelope increased on idle frame {frame}: {env} → {next}"
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
            dots_bandpass_freq(0.0),
            BANDPASS_BASE_HZ,
            "bandpass_freq at env=0 must equal BANDPASS_BASE_HZ ({BANDPASS_BASE_HZ} Hz)"
        );
    }

    /// At envelope = 1, `bandpass_freq` equals base + range (2000 Hz).
    #[test]
    fn bandpass_freq_at_full_envelope_equals_base_plus_range() {
        assert_eq!(
            dots_bandpass_freq(1.0),
            BANDPASS_BASE_HZ + BANDPASS_RANGE_HZ,
            "bandpass_freq at env=1 must equal BASE + RANGE"
        );
    }

    /// `lfo_depth == bandpass_freq × 0.06` at every sample point.
    #[test]
    fn lfo_depth_equals_bandpass_times_point_zero_six() {
        for env in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
            let bp = dots_bandpass_freq(env);
            let depth = dots_lfo_depth(bp);
            assert_eq!(
                depth,
                bp * LFO_DEPTH_OVER_CUTOFF,
                "lfo_depth != bandpass_freq × 0.06 at env={env} (bp={bp})"
            );
        }
    }

    /// At the base cutoff (200 Hz), LFO depth is 12 Hz (200 × 0.06).
    #[test]
    fn lfo_depth_at_base_cutoff_is_twelve_hz() {
        let depth = dots_lfo_depth(BANDPASS_BASE_HZ);
        assert!(
            (depth - 12.0).abs() < 1e-4,
            "lfo_depth at base cutoff must be 12 Hz; got {depth}"
        );
    }

    /// At the peak cutoff (2000 Hz), LFO depth is 120 Hz (2000 × 0.06).
    #[test]
    fn lfo_depth_at_peak_cutoff_is_one_hundred_twenty_hz() {
        let depth = dots_lfo_depth(BANDPASS_BASE_HZ + BANDPASS_RANGE_HZ);
        assert!(
            (depth - 120.0).abs() < 1e-3,
            "lfo_depth at peak cutoff must be 120 Hz; got {depth}"
        );
    }

    /// The volume param equals the envelope directly (no extra scaling).
    /// Asserts the clamping in `step_dots_envelope` keeps volume in `[0, 1]`.
    #[test]
    fn volume_param_equals_envelope_and_stays_in_unit_interval() {
        // volume = envelope.0 (the direct assignment in drive_dots_audio).
        // The meaningful guarantee is that step_dots_envelope always keeps the
        // value in [0, 1], so volume can never clip or go negative.
        for &(power, init) in &[(1.0_f32, 0.0_f32), (0.0, 1.0), (1.0, 1.0), (0.0, 0.0)] {
            let env = step_dots_envelope(init, power, 1.0 / 60.0);
            assert!(
                (0.0..=1.0).contains(&env),
                "volume out of [0,1]: power={power}, init={init}, env={env}"
            );
        }
    }

    // ── System-level test: envelope advances via RunSystemOnce ────────────

    /// `drive_dots_audio` advances the envelope when run without an
    /// `AudioCommandSender` (headless mode). Verifies the system is wired
    /// correctly and that the early-return on absent audio does not skip the
    /// envelope step.
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
        // No AudioCommandSender inserted — system must skip ring pushes cleanly.

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
            "drive_dots_audio must keep envelope ≤ 1.0; got {env}"
        );
    }
}
