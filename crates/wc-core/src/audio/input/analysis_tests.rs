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

// ---------------------------------------------------------------------------
// AnalysisEngine: time domain (Task 3)
// ---------------------------------------------------------------------------

/// Generate `len` samples of a sine at `freq` Hz, `amp` amplitude, 48 kHz,
/// phase-continuous from sample index 0.
fn sine(freq: f32, amp: f32, len: usize) -> Vec<f32> {
    (0..len)
        .map(|n| amp * (core::f32::consts::TAU * freq * (n as f32) / 48_000.0).sin())
        .collect()
}

/// Push `samples` into the engine in 800-sample chunks (one 60 Hz frame of
/// 48 kHz audio), calling `analyze` after each chunk. Returns the last
/// analysis output.
fn run_frames(engine: &mut AnalysisEngine, samples: &[f32]) -> crate::audio::input::AudioAnalysis {
    let mut out = crate::audio::input::AudioAnalysis::neutral();
    for chunk in samples.chunks(800) {
        for &s in chunk {
            engine.push(s);
        }
        out = engine.analyze(DT);
    }
    out
}

#[test]
fn engine_is_inactive_until_a_full_window_arrives() {
    let mut engine = AnalysisEngine::new(48_000);
    let out = engine.analyze(DT);
    assert!(!out.active, "no samples pushed yet");
    for &s in &sine(440.0, 0.5, FFT_SIZE - 1) {
        engine.push(s);
    }
    assert!(!engine.analyze(DT).active, "one short of a full window");
    engine.push(0.0);
    assert!(engine.analyze(DT).active, "a full window has arrived");
}

#[test]
fn engine_goes_inactive_after_samples_stop() {
    let mut engine = AnalysisEngine::new(48_000);
    run_frames(&mut engine, &sine(440.0, 0.5, 4_800));
    assert!(engine.analyze(DT).active);
    // One simulated second with no pushes: liveness times out.
    let mut out = engine.analyze(DT);
    for _ in 0..60 {
        out = engine.analyze(DT);
    }
    assert!(!out.active);
}

#[test]
fn post_agc_rms_converges_to_target_on_a_steady_sine() {
    let mut engine = AnalysisEngine::new(48_000);
    // 30 s of a steady 440 Hz sine at 0.5 amplitude (raw RMS ~0.354).
    let out = run_frames(&mut engine, &sine(440.0, 0.5, 48_000 * 30));
    assert!(
        (out.rms - AGC_TARGET_RMS).abs() < 0.03,
        "post-AGC rms {} should sit near target {}",
        out.rms,
        AGC_TARGET_RMS
    );
    assert!(out.peak > out.rms, "peak-hold rides above rms for a sine");
    assert!(out.peak <= 1.0);
    assert!(out.active);
}

#[test]
fn history_is_circular_and_the_window_reads_the_newest_samples() {
    let mut engine = AnalysisEngine::new(48_000);
    // Fill well past HISTORY_LEN with a loud DC value, then exactly one
    // window of a quiet DC value. The analysis window must see only the
    // quiet tail, proving the circular wrap points at the newest samples.
    for _ in 0..(HISTORY_LEN + 100) {
        engine.push(0.9);
    }
    for _ in 0..FFT_SIZE {
        engine.push(0.25);
    }
    engine.analyze(DT);
    assert!(
        (engine.last_raw_rms() - 0.25).abs() < 1.0e-3,
        "window raw RMS {} should reflect only the newest FFT_SIZE samples",
        engine.last_raw_rms()
    );
}

#[test]
fn engine_reset_returns_to_neutral_and_counts_from_zero() {
    let mut engine = AnalysisEngine::new(48_000);
    run_frames(&mut engine, &sine(440.0, 0.5, 9_600));
    assert!(engine.samples_received() > 0);
    engine.reset();
    assert_eq!(engine.samples_received(), 0);
    let out = engine.analyze(DT);
    assert!(!out.active);
    assert!(out.rms.abs() < f32::EPSILON);
}

// ---------------------------------------------------------------------------
// AnalysisEngine: spectral bands (Task 4)
// ---------------------------------------------------------------------------

/// Index of the strongest band in an analysis output.
fn dominant_band(bands: &[f32; AUDIO_BAND_COUNT]) -> usize {
    let mut best = 0;
    for (i, &b) in bands.iter().enumerate() {
        if b > bands[best] {
            best = i;
        }
    }
    best
}

/// Feed 4 s of a steady tone and assert the given band dominates decisively.
fn assert_tone_lands_in_band(freq: f32, expected_band: usize) {
    let mut engine = AnalysisEngine::new(48_000);
    let out = run_frames(&mut engine, &sine(freq, 0.25, 48_000 * 4));
    assert_eq!(
        dominant_band(&out.bands),
        expected_band,
        "tone at {freq} Hz should dominate band {expected_band}, bands: {:?}",
        out.bands
    );
    assert!(
        out.bands[expected_band] > 0.02,
        "dominant band should carry real energy, bands: {:?}",
        out.bands
    );
    for (i, &b) in out.bands.iter().enumerate() {
        if i != expected_band {
            assert!(
                out.bands[expected_band] > 5.0 * b,
                "band {expected_band} should dominate band {i} by 5x, bands: {:?}",
                out.bands
            );
        }
    }
}

#[test]
fn a_250_hz_tone_lands_in_band_2() {
    // BAND_EDGES_HZ: band 2 spans 200–400 Hz.
    assert_tone_lands_in_band(250.0, 2);
}

#[test]
fn a_3_khz_tone_lands_in_band_5() {
    // BAND_EDGES_HZ: band 5 spans 1600–3200 Hz.
    assert_tone_lands_in_band(3_000.0, 5);
}

#[test]
fn silence_produces_zero_bands() {
    let mut engine = AnalysisEngine::new(48_000);
    let out = run_frames(&mut engine, &vec![0.0; 48_000]);
    assert!(
        out.bands.iter().all(|b| b.abs() < 1.0e-3),
        "silent input must not excite bands: {:?}",
        out.bands
    );
}

#[test]
fn band_bins_are_monotonic_and_in_range_at_both_common_rates() {
    for rate in [44_100_u32, 48_000_u32] {
        let bins = band_bins(rate);
        let mut prev_hi = 1;
        for &(lo, hi, inv) in &bins {
            assert!(lo >= 1, "DC bin excluded");
            assert!(hi > lo, "every band has at least one bin");
            assert_eq!(lo, prev_hi, "bands tile the spectrum contiguously");
            assert!(hi <= SPECTRUM_LEN);
            assert!(inv > 0.0);
            prev_hi = hi;
        }
    }
}
