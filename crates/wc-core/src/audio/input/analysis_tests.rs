//! Unit tests for [`super::Agc`] and [`super::AnalysisEngine`].
//!
//! Lives in a sibling file (linked from `analysis.rs` via
//! `#[path = ...] mod tests;`) so the production module stays under the
//! AGENTS.md ~300-line guideline — the same idiom as `audio/dsp_tests.rs`.
//! Everything here runs headlessly: the engine is a pure struct fed
//! synthesized samples; no audio device is ever opened.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "expect and panic are appropriate in test code"
)]
#![allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "tests synthesize waveforms from small integer sample indices, exact in f32"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "seconds / DT is a small positive step count derived from test-literal durations"
)]

use super::*;

/// Simulated frame period: 60 Hz drain, matching the render loop.
const DT: f32 = 1.0 / 60.0;

/// Drive the AGC with a constant raw RMS for `seconds` of simulated time and
/// return the final gain.
fn run_agc(agc: &mut Agc, raw_rms: f32, seconds: f32) -> f32 {
    let steps = (seconds / DT) as usize;
    let mut gain = agc.gain();
    for _ in 0..steps {
        gain = agc.process(raw_rms, DT);
    }
    gain
}

#[test]
fn agc_converges_on_a_quiet_step_input() {
    // Room mic scenario: a steady signal 10x below target. After 30 s the
    // release-side AGC must have brought the post-gain level to target.
    let mut agc = Agc::new();
    // Settle at a loud level first so the quiet step exercises release.
    run_agc(&mut agc, AGC_TARGET_RMS, 5.0);
    let gain = run_agc(&mut agc, 0.025, 30.0);
    let post = 0.025 * gain;
    assert!(
        (post - AGC_TARGET_RMS).abs() < 0.02,
        "post-AGC level {post} should be within 0.02 of target {AGC_TARGET_RMS}"
    );
}

#[test]
fn agc_converges_on_a_loud_step_input() {
    // Loud step: attack side is fast — within 2 s the post-gain level is at
    // target.
    let mut agc = Agc::new();
    run_agc(&mut agc, 0.025, 30.0);
    let gain = run_agc(&mut agc, 0.5, 2.0);
    let post = 0.5 * gain;
    assert!(
        (post - AGC_TARGET_RMS).abs() < 0.02,
        "post-AGC level {post} should be within 0.02 of target {AGC_TARGET_RMS}"
    );
}

#[test]
fn agc_attack_is_faster_than_release() {
    // The same 2 s that converges the loud step must leave the quiet step
    // still far from target: gain rises slowly (release), falls fast (attack).
    let mut loud = Agc::new();
    run_agc(&mut loud, 0.025, 30.0);
    let post_loud = 0.5 * run_agc(&mut loud, 0.5, 2.0);

    let mut quiet = Agc::new();
    run_agc(&mut quiet, 0.5, 5.0);
    let post_quiet = 0.025 * run_agc(&mut quiet, 0.025, 2.0);

    assert!((post_loud - AGC_TARGET_RMS).abs() < 0.02);
    assert!(
        (post_quiet - AGC_TARGET_RMS).abs() > 0.05,
        "release must still be converging after 2 s (got {post_quiet})"
    );
}

#[test]
fn agc_gain_is_clamped_and_silence_does_not_blow_up() {
    let mut agc = Agc::new();
    let gain = run_agc(&mut agc, 0.0, 60.0);
    assert!(gain <= AGC_MAX_GAIN);
    let mut agc = Agc::new();
    let gain = run_agc(&mut agc, 10.0, 60.0);
    assert!(gain >= AGC_MIN_GAIN);
}

#[test]
fn agc_reset_restores_unity_gain() {
    let mut agc = Agc::new();
    run_agc(&mut agc, 0.01, 30.0);
    assert!(agc.gain() > 1.0);
    agc.reset();
    assert!((agc.gain() - 1.0).abs() < f32::EPSILON);
}
