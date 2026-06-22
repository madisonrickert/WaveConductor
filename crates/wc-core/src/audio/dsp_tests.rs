//! Unit tests for [`super::DspHost`].
//!
//! Lives in a sibling file (linked from `dsp.rs` via
//! `#[path = ...] mod tests;`) so the production module stays under the
//! AGENTS.md ~300-line guideline. `super::*` still resolves to the `dsp`
//! module — `#[path]` only redirects the source file, not the logical
//! module path.

#![allow(
    clippy::float_cmp,
    reason = "EPSILON comparisons are appropriate for test assertions on clean f32 values"
)]

use super::*;

#[test]
fn default_host_renders_silence() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    let mut buffer = vec![1.0_f32; 256];
    host.render(&mut buffer);
    assert!(buffer.iter().all(|s| s.abs() < f32::EPSILON));
}

#[test]
fn set_master_volume_clamps_range() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    host.apply(AudioCommand::SetMasterVolume(1.5));
    assert!((host.volume() - 1.0).abs() < f32::EPSILON);
    host.apply(AudioCommand::SetMasterVolume(-0.2));
    assert!(host.volume().abs() < f32::EPSILON);
    host.apply(AudioCommand::SetMasterVolume(0.5));
    assert!((host.volume() - 0.5).abs() < f32::EPSILON);
}

#[test]
fn set_muted_updates_state() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    assert!(!host.muted());
    host.apply(AudioCommand::SetMuted(true));
    assert!(host.muted());
    host.apply(AudioCommand::SetMuted(false));
    assert!(!host.muted());
}

#[test]
fn muted_render_outputs_zero_even_when_volume_high() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    host.apply(AudioCommand::SetMasterVolume(1.0));
    host.apply(AudioCommand::SetMuted(true));
    // Activate the synth and crank its internal volume up too: muted
    // should still force silence.
    host.apply(AudioCommand::AddLineSynth);
    host.apply(AudioCommand::SetLineParam {
        key: "volume",
        value: 1.0,
    });
    host.apply(AudioCommand::SetLineParam {
        key: "bandpass_freq",
        value: 320.0,
    });
    let mut buffer = vec![0.5_f32; 64];
    host.render(&mut buffer);
    assert!(buffer.iter().all(|s| s.abs() < f32::EPSILON));
}

#[test]
fn add_line_synth_activates() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    host.apply(AudioCommand::AddLineSynth);
    assert!(host.line_synth_active());
    // Crank volume so the smoothed source-gain ramps in.
    host.apply(AudioCommand::SetLineParam {
        key: "volume",
        value: 1.0,
    });
    host.apply(AudioCommand::SetLineParam {
        key: "bandpass_freq",
        value: 320.0,
    });
    host.apply(AudioCommand::SetLineParam {
        key: "noise_freq",
        value: 800.0,
    });
    // Render enough samples for the `follow(0.016)` smoothers to ramp.
    // 48k * 0.05 = 2400 samples ≈ 50 ms, well past the 16 ms time
    // constant.
    let mut buffer = vec![0.0_f32; 2400 * 2];
    host.render(&mut buffer);
    let max_abs = buffer.iter().fold(0.0_f32, |a, b| a.max(b.abs()));
    assert!(
        max_abs > 0.0001,
        "expected audible output after AddLineSynth + volume ramp, max_abs = {max_abs}"
    );
}

#[test]
fn remove_line_synth_silences() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    host.apply(AudioCommand::AddLineSynth);
    host.apply(AudioCommand::SetLineParam {
        key: "volume",
        value: 1.0,
    });
    // Warm up the synth so smoothers are at steady-state.
    let mut warm = vec![0.0_f32; 2400 * 2];
    host.render(&mut warm);
    host.apply(AudioCommand::RemoveLineSynth);
    assert!(!host.line_synth_active());
    let mut buffer = vec![1.0_f32; 256];
    host.render(&mut buffer);
    assert!(
        buffer.iter().all(|s| s.abs() < f32::EPSILON),
        "expected silence after RemoveLineSynth"
    );
}

#[test]
fn unknown_param_key_drops_gracefully() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    host.apply(AudioCommand::AddLineSynth);
    // Apply an unknown key; should not panic. Then render to confirm
    // the host is still operational.
    host.apply(AudioCommand::SetLineParam {
        key: "no_such_key",
        value: 1.0,
    });
    let mut buffer = vec![0.0_f32; 128];
    host.render(&mut buffer);
}

#[test]
fn set_line_param_with_no_synth_does_not_panic() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    // No synth active; SetLineParam should warn-and-drop, never panic.
    host.apply(AudioCommand::SetLineParam {
        key: "volume",
        value: 1.0,
    });
    assert!(!host.line_synth_active());
}

#[test]
fn add_line_synth_is_idempotent() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    host.apply(AudioCommand::AddLineSynth);
    // Set a param so we can detect whether the synth was replaced (a
    // replacement would reset bandpass_freq to its default).
    host.apply(AudioCommand::SetLineParam {
        key: "bandpass_freq",
        value: 4242.0,
    });
    // Second add should be a no-op; the param value survives.
    host.apply(AudioCommand::AddLineSynth);
    // We can't directly read the Shared from outside LineSynth, so the
    // best we can assert is `line_synth_active` remains true.
    assert!(host.line_synth_active());
}

#[test]
fn remove_line_synth_is_idempotent() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    // Remove with nothing active should be a no-op.
    host.apply(AudioCommand::RemoveLineSynth);
    assert!(!host.line_synth_active());
    host.apply(AudioCommand::AddLineSynth);
    host.apply(AudioCommand::RemoveLineSynth);
    host.apply(AudioCommand::RemoveLineSynth);
    assert!(!host.line_synth_active());
}

// ----- Phase B: background sample mixing -----

/// Build a deterministic stereo PCM buffer of N frames where L = +0.5
/// and R = -0.5 on every frame. Lets us verify channel order and
/// background mixing without depending on the OGG decoder.
fn synthetic_stereo_pcm(frames: usize) -> Vec<f32> {
    let mut pcm = Vec::with_capacity(frames * 2);
    for _ in 0..frames {
        pcm.push(0.5);
        pcm.push(-0.5);
    }
    pcm
}

#[test]
fn empty_background_falls_back_to_synth_only() {
    // With an empty buffer the render path takes the no-background
    // branch: synth-only output, identical to Phase A behavior.
    let mut host = DspHost::new(48_000, 2, Vec::new());
    assert!(!host.has_background());
    let mut buffer = vec![1.0_f32; 64];
    host.render(&mut buffer);
    assert!(buffer.iter().all(|s| s.abs() < f32::EPSILON));
}

#[test]
fn background_renders_when_synth_inactive() {
    // Even with no synth, the background should mix into the output
    // at the default volume of 1.0.
    let pcm = synthetic_stereo_pcm(64);
    let mut host = DspHost::new(48_000, 2, pcm);
    assert!(host.has_background());
    let mut buffer = vec![0.0_f32; 128]; // 64 stereo frames
    host.render(&mut buffer);
    // L channel = +0.5, R channel = -0.5 (master gain = 1.0).
    for frame in buffer.chunks(2) {
        assert!((frame[0] - 0.5).abs() < f32::EPSILON);
        assert!((frame[1] + 0.5).abs() < f32::EPSILON);
    }
}

#[test]
fn background_volume_scales_output() {
    let pcm = synthetic_stereo_pcm(32);
    let mut host = DspHost::new(48_000, 2, pcm);
    host.apply(AudioCommand::SetLineParam {
        key: "background_volume",
        value: 0.5,
    });
    assert!((host.background_volume() - 0.5).abs() < f32::EPSILON);
    let mut buffer = vec![0.0_f32; 64];
    host.render(&mut buffer);
    // L = 0.5 * 0.5 = 0.25, R = -0.5 * 0.5 = -0.25.
    for frame in buffer.chunks(2) {
        assert!((frame[0] - 0.25).abs() < f32::EPSILON);
        assert!((frame[1] + 0.25).abs() < f32::EPSILON);
    }
}

#[test]
fn background_volume_clamps_negative_to_zero() {
    let mut host = DspHost::new(48_000, 2, synthetic_stereo_pcm(8));
    host.apply(AudioCommand::SetLineParam {
        key: "background_volume",
        value: -0.5,
    });
    assert!(host.background_volume() >= 0.0);
    // Negative volume would invert phase; the clamp guarantees zero.
    let mut buffer = vec![1.0_f32; 16];
    host.render(&mut buffer);
    assert!(buffer.iter().all(|s| s.abs() < f32::EPSILON));
}

#[test]
fn background_playhead_wraps_at_buffer_end() {
    // 4-frame buffer: first three frames carry +0.5/-0.5, last frame
    // carries +1.0/+1.0 (a marker we can detect on the wrap).
    let mut pcm = synthetic_stereo_pcm(3);
    pcm.push(1.0);
    pcm.push(1.0);
    let mut host = DspHost::new(48_000, 2, pcm);
    // Render 10 frames (= 2.5 loops). After 4 frames we should be back
    // at the start of the buffer.
    let mut buffer = vec![0.0_f32; 20];
    host.render(&mut buffer);
    // Frames 0..3 are the +0.5/-0.5 pattern.
    for frame in buffer[0..6].chunks(2) {
        assert!((frame[0] - 0.5).abs() < f32::EPSILON);
        assert!((frame[1] + 0.5).abs() < f32::EPSILON);
    }
    // Frame 3 is the +1.0/+1.0 marker.
    assert!((buffer[6] - 1.0).abs() < f32::EPSILON);
    assert!((buffer[7] - 1.0).abs() < f32::EPSILON);
    // Frame 4 wraps: back to the start (+0.5, -0.5).
    assert!((buffer[8] - 0.5).abs() < f32::EPSILON);
    assert!((buffer[9] + 0.5).abs() < f32::EPSILON);
    // Frame 7 is the marker again.
    assert!((buffer[14] - 1.0).abs() < f32::EPSILON);
    assert!((buffer[15] - 1.0).abs() < f32::EPSILON);
}

#[test]
fn muted_zeros_background_too() {
    let mut host = DspHost::new(48_000, 2, synthetic_stereo_pcm(16));
    host.apply(AudioCommand::SetMuted(true));
    let mut buffer = vec![0.0_f32; 32];
    host.render(&mut buffer);
    assert!(buffer.iter().all(|s| s.abs() < f32::EPSILON));
}

#[test]
fn background_clamps_when_synth_and_background_peak_together() {
    // Stereo PCM where every sample is +1.0 (background already at
    // the ceiling). Activate the synth and crank its volume so the
    // sum would exceed +1.0 without the clamp; assert that output
    // never exceeds the ceiling.
    let pcm = vec![1.0_f32; 32]; // 16 stereo frames, both channels +1.0.
    let mut host = DspHost::new(48_000, 2, pcm);
    host.apply(AudioCommand::AddLineSynth);
    host.apply(AudioCommand::SetLineParam {
        key: "volume",
        value: 1.0,
    });
    let mut buffer = vec![0.0_f32; 32];
    host.render(&mut buffer);
    for s in &buffer {
        assert!(s.abs() <= 1.0, "post-mix sample {s} escaped the clamp");
    }
}

#[test]
fn background_volume_key_works_with_no_active_synth() {
    // Regression: background_volume must NOT route through LineSynth,
    // so it has to apply even when the synth is not active.
    let mut host = DspHost::new(48_000, 2, synthetic_stereo_pcm(4));
    assert!(!host.line_synth_active());
    host.apply(AudioCommand::SetLineParam {
        key: "background_volume",
        value: 0.0,
    });
    let mut buffer = vec![1.0_f32; 8];
    host.render(&mut buffer);
    assert!(buffer.iter().all(|s| s.abs() < f32::EPSILON));
}

// ----- Dots synth dispatch tests -----

#[test]
fn add_dots_synth_activates() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    assert!(!host.dots_synth_active());
    host.apply(AudioCommand::AddDotsSynth);
    assert!(host.dots_synth_active());
}

#[test]
fn add_dots_synth_is_idempotent() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    host.apply(AudioCommand::AddDotsSynth);
    // Set a param so we can detect whether the synth was replaced
    // (a replacement resets params to defaults).
    host.apply(AudioCommand::SetDotsParam {
        key: "bandpass_freq",
        value: 4242.0,
    });
    // Second add must be a no-op; the slot must still be active.
    host.apply(AudioCommand::AddDotsSynth);
    assert!(host.dots_synth_active());
}

#[test]
fn remove_dots_synth_is_idempotent() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    // Remove with nothing active should be a no-op.
    host.apply(AudioCommand::RemoveDotsSynth);
    assert!(!host.dots_synth_active());
    host.apply(AudioCommand::AddDotsSynth);
    host.apply(AudioCommand::RemoveDotsSynth);
    host.apply(AudioCommand::RemoveDotsSynth);
    assert!(!host.dots_synth_active());
}

#[test]
fn add_dots_then_set_param_then_remove_sequence() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    host.apply(AudioCommand::AddDotsSynth);
    assert!(host.dots_synth_active());
    // SetDotsParam must not panic with an active synth.
    host.apply(AudioCommand::SetDotsParam {
        key: "volume",
        value: 0.5,
    });
    host.apply(AudioCommand::SetDotsParam {
        key: "bandpass_freq",
        value: 300.0,
    });
    host.apply(AudioCommand::SetDotsParam {
        key: "lfo_depth",
        value: 18.0,
    });
    // Render a buffer to exercise the render path.
    let mut buf = vec![0.0_f32; 256];
    host.render(&mut buf);
    host.apply(AudioCommand::RemoveDotsSynth);
    assert!(!host.dots_synth_active());
    // After removal the render path must produce silence.
    let mut silent = vec![1.0_f32; 64];
    host.render(&mut silent);
    assert!(
        silent.iter().all(|s| s.abs() < f32::EPSILON),
        "expected silence after RemoveDotsSynth"
    );
}

#[test]
fn set_dots_param_with_no_synth_does_not_panic() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    // No synth active; SetDotsParam should warn-and-drop, never panic.
    host.apply(AudioCommand::SetDotsParam {
        key: "volume",
        value: 1.0,
    });
    assert!(!host.dots_synth_active());
}

#[test]
fn dots_synth_produces_audio_after_volume_set() {
    let mut host = DspHost::new(48_000, 2, Vec::new());
    host.apply(AudioCommand::AddDotsSynth);
    host.apply(AudioCommand::SetDotsParam {
        key: "volume",
        value: 1.0,
    });
    host.apply(AudioCommand::SetDotsParam {
        key: "bandpass_freq",
        value: 300.0,
    });
    // Render enough samples for the follow(0.016) smoothers to ramp up.
    // 48 000 × 0.05 s = 2 400 samples; use stereo (× 2).
    let mut buffer = vec![0.0_f32; 2_400 * 2];
    host.render(&mut buffer);
    let max_abs = buffer.iter().fold(0.0_f32, |a, b| a.max(b.abs()));
    assert!(
        max_abs > 1e-4,
        "expected audible output after AddDotsSynth + volume ramp, max_abs = {max_abs}"
    );
}
