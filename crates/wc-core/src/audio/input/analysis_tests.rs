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

// ---------------------------------------------------------------------------
// AnalysisEngine: onset + beat (Task 5)
// ---------------------------------------------------------------------------

/// One 60 Hz frame (800 samples at 48 kHz) that is silent except for a
/// broadband click: the first 100 samples at 0.8.
fn click_frame() -> Vec<f32> {
    let mut frame = vec![0.0_f32; 800];
    for s in frame.iter_mut().take(100) {
        *s = 0.8;
    }
    frame
}

/// One silent 60 Hz frame.
fn silent_frame() -> Vec<f32> {
    vec![0.0_f32; 800]
}

/// Push one frame of samples and analyze it.
fn step(engine: &mut AnalysisEngine, frame: &[f32]) -> crate::audio::input::AudioAnalysis {
    for &s in frame {
        engine.push(s);
    }
    engine.analyze(DT)
}

#[test]
fn silence_produces_zero_onset_and_no_beats() {
    let mut engine = AnalysisEngine::new(48_000);
    let mut out = engine.analyze(DT);
    for _ in 0..120 {
        out = step(&mut engine, &silent_frame());
    }
    assert!(
        out.onset.abs() < f32::EPSILON,
        "flux of silence is exactly 0"
    );
    assert!(out.beat_confidence < 1.0e-3);
    assert_eq!(engine.beat_count(), 0);
}

#[test]
fn a_click_train_produces_debounced_beats_half_second_apart() {
    let mut engine = AnalysisEngine::new(48_000);
    // Settle on silence first so the click is a clean onset.
    for _ in 0..60 {
        step(&mut engine, &silent_frame());
    }
    // Two clicks 0.5 s apart (frames 0 and 30): both register as beats.
    let mut max_onset = 0.0_f32;
    for i in 0..60 {
        let out = if i == 0 || i == 30 {
            step(&mut engine, &click_frame())
        } else {
            step(&mut engine, &silent_frame())
        };
        max_onset = max_onset.max(out.onset);
        if i == 0 || i == 30 {
            assert!(
                (out.beat_confidence - 1.0).abs() < f32::EPSILON,
                "click frame {i} snaps beat confidence to 1.0 (got {})",
                out.beat_confidence
            );
        }
    }
    assert_eq!(engine.beat_count(), 2);
    assert!(
        max_onset > BEAT_ONSET_THRESHOLD,
        "click onsets must clear the beat threshold (max {max_onset})"
    );
}

#[test]
fn beats_within_the_minimum_interval_are_debounced() {
    let mut engine = AnalysisEngine::new(48_000);
    for _ in 0..60 {
        step(&mut engine, &silent_frame());
    }
    // Clicks at frames 0 and 3 — 0.05 s apart, inside MIN_BEAT_INTERVAL_S.
    for i in 0..10 {
        if i == 0 || i == 3 {
            step(&mut engine, &click_frame());
        } else {
            step(&mut engine, &silent_frame());
        }
    }
    assert_eq!(
        engine.beat_count(),
        1,
        "the second click is inside the debounce window"
    );
}

#[test]
fn beat_confidence_decays_between_beats() {
    let mut engine = AnalysisEngine::new(48_000);
    for _ in 0..60 {
        step(&mut engine, &silent_frame());
    }
    let at_beat = step(&mut engine, &click_frame());
    assert!((at_beat.beat_confidence - 1.0).abs() < f32::EPSILON);
    let mut later = at_beat;
    for _ in 0..30 {
        later = step(&mut engine, &silent_frame());
    }
    assert!(
        later.beat_confidence < 0.3,
        "confidence should have decayed well below 1 after 0.5 s (got {})",
        later.beat_confidence
    );
}

// ---------------------------------------------------------------------------
// drain_and_analyze system (Task 6)
// ---------------------------------------------------------------------------

// `bevy::prelude::*` is already in scope here via `use super::*;` above,
// which pulls in `analysis.rs`'s own (private but visible-to-descendants)
// bevy prelude import — a second explicit import would be unused.
use crate::audio::input::capture::{AudioInputRing, RING_SAMPLE_CAPACITY};
use crate::audio::input::AudioCaptureRequest;

/// Headless drain-test app: ring + engine + request wired by hand, only the
/// drain system registered. NO capture driver and NO real cpal stream — see
/// the plan's execution notes (a request + driver would open a live mic).
fn drain_test_app(request: AudioCaptureRequest) -> (App, rtrb::Producer<f32>) {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.init_resource::<crate::audio::input::AudioAnalysis>();
    app.insert_resource(request);
    app.insert_resource(AnalysisState(AnalysisEngine::new(48_000)));
    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(RING_SAMPLE_CAPACITY);
    app.world_mut()
        .insert_non_send(AudioInputRing::new(consumer));
    app.add_systems(PreUpdate, drain_and_analyze);
    (app, producer)
}

#[test]
fn drain_empties_a_completely_full_ring_in_one_frame() {
    let (mut app, mut producer) = drain_test_app(AudioCaptureRequest {
        device_name: None,
        paused: false,
    });
    // Buffer pressure: fill the ring to capacity, then overflow it — the
    // overflow push is refused (dropped by the callback in production),
    // never a panic or a block.
    for _ in 0..RING_SAMPLE_CAPACITY {
        producer.push(0.25).expect("fits within capacity");
    }
    assert!(producer.push(0.5).is_err(), "full ring refuses the push");
    app.update();
    let received = app.world().resource::<AnalysisState>().0.samples_received();
    assert_eq!(
        received,
        u64::try_from(RING_SAMPLE_CAPACITY).expect("capacity fits u64"),
        "one frame drains the entire backlog"
    );
    assert!(
        app.world()
            .resource::<crate::audio::input::AudioAnalysis>()
            .active
    );
    assert_eq!(
        producer.slots(),
        RING_SAMPLE_CAPACITY,
        "ring fully drained: every slot free again"
    );
}

#[test]
fn paused_request_discards_samples_and_holds_neutral() {
    let (mut app, mut producer) = drain_test_app(AudioCaptureRequest {
        device_name: None,
        paused: true,
    });
    for _ in 0..4_096 {
        producer.push(0.5).expect("fits within capacity");
    }
    app.update();
    assert_eq!(
        *app.world().resource::<crate::audio::input::AudioAnalysis>(),
        crate::audio::input::AudioAnalysis::neutral()
    );
    assert_eq!(
        producer.slots(),
        RING_SAMPLE_CAPACITY,
        "paused drain discards in-flight samples so resume starts fresh"
    );
    assert_eq!(
        app.world().resource::<AnalysisState>().0.samples_received(),
        0,
        "discarded samples are never analyzed"
    );
}

#[test]
fn missing_capture_resources_hold_neutral() {
    // The plugin's steady state outside Radiance: no request, no ring, no
    // engine. The system must no-op to neutral, never panic.
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.init_resource::<crate::audio::input::AudioAnalysis>();
    app.add_systems(PreUpdate, drain_and_analyze);
    app.update();
    app.update();
    assert_eq!(
        *app.world().resource::<crate::audio::input::AudioAnalysis>(),
        crate::audio::input::AudioAnalysis::neutral()
    );
}

#[test]
fn pausing_after_activity_resets_the_engine_exactly_once() {
    // The genuine transition path: capture ACTIVE with non-neutral
    // analysis, then the request flips to paused. The one-shot reset
    // guarded by `analysis != neutral()` (analysis.rs) must fire exactly
    // once on that transition frame, not on every subsequent paused frame.
    let (mut app, mut producer) = drain_test_app(AudioCaptureRequest {
        device_name: None,
        paused: false,
    });
    // Drive several loud, active frames so analysis is genuinely non-neutral
    // (not just technically unequal by one field) before the pause.
    for _ in 0..3 {
        for _ in 0..RING_SAMPLE_CAPACITY {
            producer.push(0.5).expect("fits within capacity");
        }
        app.update();
    }
    let active_analysis = *app.world().resource::<crate::audio::input::AudioAnalysis>();
    assert!(active_analysis.active, "loud frames must report active");
    assert_ne!(
        active_analysis,
        crate::audio::input::AudioAnalysis::neutral(),
        "loud active frames must produce non-neutral analysis before pausing"
    );
    assert!(
        app.world().resource::<AnalysisState>().0.samples_received() > 0,
        "engine should have accumulated samples while active"
    );

    // Flip to paused: the very next frame must fire the one-shot reset.
    app.world_mut().resource_mut::<AudioCaptureRequest>().paused = true;
    app.update();
    assert_eq!(
        *app.world().resource::<crate::audio::input::AudioAnalysis>(),
        crate::audio::input::AudioAnalysis::neutral(),
        "the pause transition frame must publish neutral analysis"
    );
    assert_eq!(
        app.world().resource::<AnalysisState>().0.samples_received(),
        0,
        "reset() must have fired on the transition frame, zeroing the engine"
    );

    // Prove the guard does not re-fire on every subsequent paused frame:
    // push a sentinel directly into the engine (bypassing the ring, which a
    // paused frame only discards from) so an unwanted reset() on the next
    // frame is observable — the discarded-ring-samples path alone can't
    // distinguish "reset fired again" from "reset stayed off", since both
    // leave samples_received at 0.
    for _ in 0..500 {
        app.world_mut().resource_mut::<AnalysisState>().0.push(0.5);
    }
    for _ in 0..1_000 {
        producer.push(0.5).expect("fits within capacity");
    }
    app.update();
    assert_eq!(
        app.world().resource::<AnalysisState>().0.samples_received(),
        500,
        "reset() must not re-fire once analysis is already neutral, or the sentinel would be wiped"
    );
    assert_eq!(
        *app.world().resource::<crate::audio::input::AudioAnalysis>(),
        crate::audio::input::AudioAnalysis::neutral(),
        "analysis stays neutral while paused"
    );
    assert_eq!(
        producer.slots(),
        RING_SAMPLE_CAPACITY,
        "paused drain still discards newly queued samples"
    );
}
