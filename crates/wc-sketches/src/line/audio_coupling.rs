//! Reactivity coupling: `ParticleStats` → Line synth + gravity-smear shader.
//!
//! Direct port of the audio + shader-uniform writes in v4's
//! `src/sketches/line/index.ts::step()`. Runs each `Update` while the Line
//! sketch is `Active` (gated by the chain assembled in [`super::LinePlugin`]).
//!
//! ## What this writes
//!
//! 1. **Audio params** — four `SetLineParam` commands per frame, pushed onto
//!    the lock-free [`wc_core::audio::ring::AudioCommandSender`] ring:
//!    - `lfo_freq = bandpass_freq × 0.06` — modulation **depth** in Hz for the
//!      bandpass LFO. v4 sets the LFO oscillator's gain (depth) as
//!      `lfoGain.gain = lfoFreq × 0.06` where lfoFreq tracks bandpass freq;
//!      v5's [`wc_core::audio::line_synth::LineSynth`] routes the `lfo_freq`
//!      param key directly to the LFO depth multiplier (the oscillator rate
//!      is hardcoded at 8.66 Hz). Plan 11 Phase F tuning: passing the depth
//!      derived from current bandpass cutoff reproduces v4's audible wobble
//!      (11–67 Hz depth across the press cycle) — the previous constant 1.0
//!      from `flat_ratio` was sub-quarter-tone and inaudible.
//!    - `bandpass_freq = 222.0 / normalized_entropy` (when entropy is non-zero)
//!      — entropy-driven filter cutoff; less entropy → higher cutoff.
//!    - `noise_freq = 2000.0 * normalized_variance_length` — spreading the
//!      cloud raises the noise modulator frequency.
//!    - `volume = max(0, grouped_upness - 0.05) * 5` — synth speaks only when
//!      particles are tightly clustered *and* moving (the v4 "gathering" cue).
//!      The `-0.05` threshold creates a silence floor under low-action states.
//!
//! 2. **Shader uniforms** — overwrites two fields on [`super::post_process::LinePostParams`]
//!    (which the [`super::systems::sim_params::update_sim_params`] system also
//!    touches earlier in the same frame; ordering matters and is enforced by
//!    the `.chain()` in `LinePlugin::build`):
//!    - `g_constant = triangleWaveApprox(t/5) × (grouped_upness + 0.5) × 15000`
//!      — pulses the gravity smear at a 5-second triangle period, scaled by
//!      groupedUpness. v4 ms→s conversion: `triangleWaveApprox(now/5000)` in
//!      v4's millisecond clock = `triangle_wave_approx(t/5.0)` here.
//!    - `i_mouse_factor = (1/15) / (grouped_upness + 1)` — softens the
//!      per-step mouse-pull contribution as upness rises.
//!
//! ## Ring full?
//!
//! If the audio ring is full (audio thread severely backlogged), the dropped
//! command logs at `warn` once. We deliberately do not panic or block — the
//! audio thread may catch up next frame, and a dropped `SetLineParam` is
//! survivable (the parameter just holds its last value for one frame).

use bevy::ecs::system::NonSendMut;
use bevy::prelude::*;

use wc_core::audio::command::AudioCommand;
use wc_core::audio::ring::AudioCommandSender;

use super::particle_stats::ParticleStats;
use super::post_process::LinePostParams;

/// v4's `lfoGain.gain.setTargetAtTime(freq * 0.06, …)` constant.
///
/// The LFO modulation depth tracks the bandpass cutoff so the wobble's
/// musical interval stays consistent as the cutoff sweeps — at 222 Hz cutoff
/// the depth is ~13 Hz (≈ minor second), at 1110 Hz cutoff the depth is
/// ~67 Hz (≈ minor sixth). Pinning this to the v4 constant avoids re-deriving
/// it whenever the bandpass formula changes.
const LFO_DEPTH_OVER_CUTOFF: f32 = 0.06;

/// `Update` system that closes the audio-reactivity loop.
///
/// Reads [`ParticleStats`] (written earlier in the same frame by
/// [`super::particle_stats::update_particle_stats`]) and writes audio commands
/// plus shader uniforms.
///
/// Gated by `sketch_active(AppState::Line)` in [`super::LinePlugin::build`] and
/// chained after `update_sim_params`, so the `LinePostParams` writes here
/// override the placeholder `g_constant` and `i_mouse_factor` set earlier in
/// the same frame.
pub fn drive_audio_and_shader(
    stats: Res<'_, ParticleStats>,
    time: Res<'_, Time>,
    audio_cmd: Option<NonSendMut<'_, AudioCommandSender>>,
    mut post: ResMut<'_, LinePostParams>,
    settings: Res<'_, super::settings::LineSettings>,
) {
    // --- Audio modulation (matches v4 LineSketch.step()) ---
    //
    // Each `push` may fail with `Err(dropped_command)` if the audio ring is
    // full. We log once via `tracing::warn!` and continue; one dropped frame
    // of a param-set just means the audio thread sees the previous value for
    // one extra frame, which is inaudible.
    //
    // The audio engine isn't started in headless integration tests (no cpal
    // device). When `AudioCommandSender` is absent, skip the audio writes and
    // still update the shader uniforms below.
    if let Some(mut audio_cmd) = audio_cmd {
        // LFO oscillator rate. v4 sets `sourceLfoFreq.setTargetAtTime(flatRatio, …)`
        // every frame in `index.ts::step()`. Typical 1–3 Hz during sustained
        // press, slower for a roughly-circular cloud, faster during left-right
        // mouse motion. `lfo_rate_hz` drives the variable-rate sine in
        // `LineSynth`'s bandpass modulation. (The historically-named `lfo_freq`
        // key still drives LFO depth — see below.)
        push_audio(
            &mut audio_cmd,
            AudioCommand::SetLineParam {
                key: "lfo_rate_hz",
                value: stats.flat_ratio,
            },
        );
        // v4 guards bandpass against division-by-zero on `normalizedEntropy == 0`
        // (which happens when all particles share a position, e.g. first frame).
        // Skip the bandpass + lfo_depth commands together in that case — both
        // depend on the same denominator, and the audio thread keeps the last
        // values. The lfo_depth derives from bandpass_freq directly, matching
        // v4's `lfoGain.gain = bandpassFreq × 0.06`.
        if stats.normalized_entropy != 0.0 {
            let bandpass_freq = 222.0 / stats.normalized_entropy;
            push_audio(
                &mut audio_cmd,
                AudioCommand::SetLineParam {
                    key: "bandpass_freq",
                    value: bandpass_freq,
                },
            );
            push_audio(
                &mut audio_cmd,
                AudioCommand::SetLineParam {
                    key: "lfo_freq",
                    // v4 parity: LFO modulation depth tracks bandpass cutoff.
                    // The `lfo_freq` key in LineSynth is routed to LFO depth
                    // (oscillator rate is the separate `lfo_rate_hz` key above).
                    // 0.06 is v4's `lfoGain.gain = freq × 0.06` constant.
                    value: bandpass_freq * LFO_DEPTH_OVER_CUTOFF,
                },
            );
        }
        push_audio(
            &mut audio_cmd,
            AudioCommand::SetLineParam {
                key: "noise_freq",
                value: 2000.0 * stats.normalized_variance_length,
            },
        );
        push_audio(
            &mut audio_cmd,
            AudioCommand::SetLineParam {
                key: "volume",
                // Threshold + scale: silent below 0.05, scaled ×5 above.
                // Then multiplied by the user-configurable volume trim from
                // `LineSettings::synth_volume_scale` (default 1.0).
                value: (stats.grouped_upness - 0.05).max(0.0) * 5.0 * settings.synth_volume_scale,
            },
        );
        // **Pad evolution envelope** — drives the slow modulator-depth growth
        // and filter-cutoff opening inside LineSynth. Stats-side has the
        // ~4 s / ~6 s asymmetric follow; we just forward the value.
        push_audio(
            &mut audio_cmd,
            AudioCommand::SetLineParam {
                key: "evolution",
                value: stats.evolution,
            },
        );
    } // end if let Some(audio_cmd)

    // --- Shader modulation (v4 LineSketch.step()) ---
    //
    // `g_constant` pulses on a triangle wave with a 5-second period, scaled
    // by `grouped_upness + 0.5` (so a non-zero baseline pulse is always
    // present even when the particles are still) and a constant 15000 to
    // bring the value into the gravity-smear shader's expected range.
    let t = time.elapsed_secs();
    post.g_constant = triangle_wave_approx(t / 5.0) * (stats.grouped_upness + 0.5) * 15_000.0;
    // `i_mouse_factor` softens the mouse-pull contribution as upness rises:
    // 1/15 baseline, halved as groupedUpness approaches 1.
    post.i_mouse_factor = (1.0 / 15.0) / (stats.grouped_upness + 1.0);
}

/// Push an [`AudioCommand`] onto the ring, logging at `warn` if the ring is
/// full and the command is dropped. Non-fatal: the parameter just holds its
/// last value for one frame.
fn push_audio(sender: &mut AudioCommandSender, command: AudioCommand) {
    if let Err(_dropped) = sender.push(command) {
        tracing::warn!("audio command ring full; dropping Line param update");
    }
}

/// Approximate normalised triangle wave using the first three odd harmonics
/// of the Fourier series.
///
/// Port of v4's `src/math.ts::triangleWaveApprox`. The series:
///
/// ```text
///   triangle(t) ≈ (8/π²) · [sin(t) − (1/9)·sin(3t) + (1/25)·sin(5t)]
/// ```
///
/// Three harmonics give a smooth, audible triangle without the high-order
/// ringing of an exact triangle wave — perceptually identical for a 5-second
/// visual modulation. The `8/π²` prefactor normalises peak amplitude to ±1.
fn triangle_wave_approx(t: f32) -> f32 {
    use std::f32::consts::PI;
    // 8/π² ≈ 0.8106 — Fourier prefactor for the normalised triangle series.
    (8.0 / (PI * PI)) * (t.sin() - (1.0 / 9.0) * (3.0 * t).sin() + (1.0 / 25.0) * (5.0 * t).sin())
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic float arithmetic is the test subject"
)]
mod tests {
    use super::*;

    #[test]
    fn triangle_wave_zero_at_origin() {
        assert!(triangle_wave_approx(0.0).abs() < 1e-6);
    }

    #[test]
    fn triangle_wave_peaks_within_amplitude_envelope() {
        // The three-harmonic approximation peaks slightly above ±1 (Gibbs
        // overshoot is small here). Bound at ±1.05 to catch wild regressions.
        for i in 0..1000 {
            #[allow(
                clippy::cast_precision_loss,
                clippy::as_conversions,
                reason = "test loop counter"
            )]
            let t = i as f32 * 0.01;
            let y = triangle_wave_approx(t);
            assert!(
                y.abs() < 1.05,
                "triangle_wave_approx({t}) = {y} exceeded ±1.05",
            );
        }
    }

    #[test]
    fn triangle_wave_period_two_pi() {
        // First harmonic is sin(t); samples 2π apart should match closely.
        // (The 3rd and 5th harmonics also share this period, so the sum does.)
        for i in 0..100 {
            #[allow(
                clippy::cast_precision_loss,
                clippy::as_conversions,
                reason = "test loop counter"
            )]
            let t = i as f32 * 0.1;
            let a = triangle_wave_approx(t);
            let b = triangle_wave_approx(t + 2.0 * std::f32::consts::PI);
            assert!((a - b).abs() < 1e-5, "period mismatch at t={t}: {a} vs {b}");
        }
    }
}
