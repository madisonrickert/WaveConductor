//! Debug-only file-driven audio input: `WC_AUDIO_FILE` test-track playback
//! + analysis drive.
//!
//! ## Role
//!
//! Audio-reactive sketches (Radiance is the first) are normally driven by a
//! live mic (`super::capture` / `super::analysis`). A room mic is
//! unrepeatable across runs and machines, which makes beat/onset/band
//! tuning hard to iterate on. Setting `WC_AUDIO_FILE=<path>` to a FLAC (or
//! WAV) file makes this module decode it once, loop it forever, and drive
//! the *same* [`super::analysis::AnalysisEngine`] /
//! [`super::AudioAnalysis`] pipeline the mic would — while also playing the
//! track audibly through a dedicated output stream, so a human watching the
//! sketch hears exactly what is driving it.
//!
//! ## Data flow
//!
//! ```text
//!   Startup: decode the whole file into memory (claxon), then resample
//!            once to the output device's sample rate:
//!              - a stereo interleaved buffer, for audible playback
//!              - a mono buffer, for analysis (mirrors the mic's downmix)
//!     │
//!     ▼
//!   a dedicated cpal OUTPUT stream (separate from `engine::AudioStream`,
//!   the synth graph's own stream — see "Why a separate stream" below)
//!     │  each callback: write the next block of stereo samples to
//!     │  `output` (looping), AND push the paired mono samples into a
//!     │  `capture::AudioInputRing` — the SAME ring type the mic's cpal
//!     │  input callback fills.
//!     ▼
//!   PreUpdate: `analysis::drain_and_analyze` (unmodified) drains that ring
//!   into a `analysis::AnalysisState` it owns, publishing `AudioAnalysis`
//!   exactly as it would for a live mic.
//! ```
//!
//! ## Why a separate stream
//!
//! The synth engine's own cpal output stream (`engine::AudioStream`) renders
//! `DspHost` — sketch voices, background beds, one-shots. Mixing the test
//! track into that graph would mean either a new `AudioCommand` variant (a
//! permanent addition to ship code for a debug-only tool) or fighting the
//! `DspHost`'s render loop from two threads. A second, independent
//! `cpal::Stream` to the same output device is simpler and keeps this
//! module's entire footprint behind `#[cfg(debug_assertions)]`.
//!
//! ## Why the mic is suppressed, not paused
//!
//! Both this module's output callback and the mic's input callback
//! (`capture::build_typed_stream`) would push into `capture::AudioInputRing`
//! if both ran — the ring has one producer end, so the two would race and
//! corrupt the sample sequence `AnalysisEngine` reads. Rather than teach
//! `capture::drive_capture` to pause on some new per-frame signal (which
//! would still let a request flip it back on), [`super::FileDriveActive`]
//! makes `capture::decide` refuse to ever build a mic stream while it's
//! present — see the short-circuit at the top of `capture::decide`. The
//! flag is fixed for the process's lifetime (read once, at plugin build,
//! from an env var), so this is a permanent, not a toggled, suppression:
//! the cleanest seam available in the mic device-selection/pause policy.
//!
//! `super::AudioCaptureRequest` itself is untouched by this module — a
//! sketch (Radiance) still inserts/removes/pauses it exactly as it would
//! for the mic, and `analysis::drain_and_analyze` still gates on its
//! presence. That means Idle/Screensaver's pause behavior (drain the ring,
//! reset the engine, hold `AudioAnalysis` neutral — see
//! `analysis::drain_and_analyze`) applies to file-drive audio for free: no
//! special-casing needed, because the request/pause contract was already
//! sketch-agnostic.
//!
//! One known gap: `capture::AudioInputStatus` (the mic diagnostics surface)
//! stays `Inactive` the whole time file-drive is running, because
//! `capture::drive_capture` never reaches its Build arm. A debug-only dev
//! tool's own INFO log line (see [`start_file_drive`]) is the diagnostic
//! surface here, not the mic status resource.
//!
//! ## Real-time / hot-path invariants
//!
//! The file is decoded and resampled exactly ONCE, at `Startup`
//! ([`build_file_drive`]), into owned buffers moved into the output
//! callback's closure. The callback itself ([`build_file_drive`]'s
//! `build_output_stream` closure) only reads those buffers by index and
//! pushes to the ring — no allocation, no locks beyond `rtrb`'s wait-free
//! push, matching the mic callback's discipline in `capture.rs`.
//!
//! For a 3–4 minute FLAC this is roughly 40 MB of f32 (mono + stereo,
//! post-resample) held for the process's lifetime — acceptable for a
//! debug-only tool, not something this module tries to bound further.
//!
//! ## Release safety
//!
//! `#[cfg(debug_assertions)]`-gated end to end (see `super::AudioInputPlugin`'s
//! `build`), exactly like `WC_AUDIO_INPUT_SMOKE` and the `WC_SOAK` harness:
//! this module is entirely absent from a release build.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::analysis::{AnalysisEngine, AnalysisState};
use super::capture::{AudioInputRing, RING_SAMPLE_CAPACITY};

/// Path to the track named by `WC_AUDIO_FILE`, read once at plugin build
/// and consumed by [`start_file_drive`] at `Startup`.
#[derive(Resource)]
pub(super) struct FileDrivePath(pub PathBuf);

/// Wraps the file-drive's dedicated cpal OUTPUT stream so Bevy keeps it
/// alive for the process's lifetime. A distinct type from
/// `engine::AudioStream` (the synth graph's stream) and
/// `capture::AudioInputStream` (the mic's stream) so the three never
/// collide in the non-send resource map.
struct FileDriveOutputStream {
    /// Owned stream handle. Dropping it stops playback and the analysis
    /// feed together — there is no separate teardown path because this
    /// module never tears down (see the module docs' file-drive-active
    /// lifetime note).
    #[allow(dead_code, reason = "kept alive for its Drop; never read again")]
    stream: cpal::Stream,
}

/// `Startup` system: decode `WC_AUDIO_FILE` and start the dedicated
/// playback + analysis-drive stream.
///
/// On failure, logs and leaves nothing installed — no `AnalysisState`, no
/// `AudioInputRing` — so `analysis::drain_and_analyze` sees the same
/// "inactive" shape it does with no mic present, and the rest of the app
/// runs (silently, with neutral analysis) rather than failing to start.
pub(super) fn start_file_drive(world: &mut World) {
    let path = world.resource::<FileDrivePath>().0.clone();
    match build_file_drive(&path) {
        Ok(built) => {
            tracing::info!(
                file = %path.display(),
                duration_secs = built.duration_secs,
                sample_rate = built.sample_rate,
                "WC_AUDIO_FILE: looping playback + analysis drive started",
            );
            world.insert_resource(AnalysisState(AnalysisEngine::new(built.sample_rate)));
            world.insert_non_send(built.ring);
            world.insert_non_send(built.stream);
        }
        Err(err) => {
            tracing::error!(
                ?err,
                file = %path.display(),
                "WC_AUDIO_FILE: failed to start; audio analysis stays neutral",
            );
        }
    }
}

/// Why [`build_file_drive`] failed. Event-frequency (once, at `Startup`);
/// formatting allocates, which is fine off the audio thread.
#[derive(Debug, thiserror::Error)]
pub(super) enum FileDriveError {
    /// claxon could not open or decode the FLAC container.
    #[error("flac decode: {0}")]
    Flac(#[from] claxon::Error),
    /// The decoded track had no samples, or reported zero channels.
    #[error("decoded track is empty")]
    EmptyTrack,
    /// No default output device to play through.
    #[error("no default output device available")]
    NoDefaultOutputDevice,
    #[error("cpal default output config error: {0}")]
    DefaultConfig(#[from] cpal::DefaultStreamConfigError),
    #[error("cpal output stream build error: {0}")]
    BuildStream(#[from] cpal::BuildStreamError),
    #[error("cpal output stream play error: {0}")]
    PlayStream(#[from] cpal::PlayStreamError),
}

/// Everything a successful [`build_file_drive`] hands back to
/// [`start_file_drive`].
struct BuiltFileDrive {
    stream: FileDriveOutputStream,
    ring: AudioInputRing,
    sample_rate: u32,
    duration_secs: f32,
}

/// Decode `path`, resample to the default output device's rate, and start
/// a dedicated cpal output stream that plays the track and feeds a fresh
/// [`AudioInputRing`] in lockstep. All decode/resample allocation happens
/// here, once; the returned stream's callback never allocates again.
fn build_file_drive(path: &Path) -> Result<BuiltFileDrive, FileDriveError> {
    let raw = decode_flac(path)?;
    if raw.samples.is_empty() || raw.channels == 0 {
        return Err(FileDriveError::EmptyTrack);
    }

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or(FileDriveError::NoDefaultOutputDevice)?;
    let supported = device.default_output_config()?;
    let out_rate = supported.sample_rate().0;
    let out_channels = usize::from(supported.channels());
    let config: cpal::StreamConfig = supported.into();

    // Fix channel counts up front (to_stereo / downmix_to_mono), then
    // resample each fixed-channel buffer independently. Two independent
    // resamples can round to frame counts that differ by at most one frame
    // — irrelevant here since each loops on its own frame count.
    let raw_channels = usize::from(raw.channels);
    let stereo_native = to_stereo(&raw.samples, raw_channels);
    let mono_native = downmix_to_mono(&raw.samples, raw_channels);
    let stereo = resample_linear(&stereo_native, 2, raw.sample_rate, out_rate);
    let mono = resample_linear(&mono_native, 1, raw.sample_rate, out_rate);

    let stereo_frames = stereo.len() / 2;
    let mono_frames = mono.len();
    if stereo_frames == 0 || mono_frames == 0 {
        return Err(FileDriveError::EmptyTrack);
    }
    let duration_secs = frame_duration_secs(stereo_frames, out_rate);

    let (mut producer, consumer) = rtrb::RingBuffer::<f32>::new(RING_SAMPLE_CAPACITY);

    // Frame cursors into `stereo`/`mono`, owned by the callback closure.
    // Advanced with `wrap_index` (pure, unit-tested below) so playback and
    // the analysis feed loop seamlessly at end-of-track without a
    // conditional branch in the hot loop.
    let mut play_pos = 0_usize;
    let mut mono_pos = 0_usize;

    let stream = device.build_output_stream(
        &config,
        move |output: &mut [f32], _info: &cpal::OutputCallbackInfo| {
            if out_channels == 0 {
                return;
            }
            for frame in output.chunks_exact_mut(out_channels) {
                let base = play_pos * 2;
                let l = stereo[base];
                let r = stereo[base + 1];
                play_pos = wrap_index(play_pos, stereo_frames);

                if out_channels >= 2 {
                    frame[0] = l;
                    frame[1] = r;
                    for slot in &mut frame[2..] {
                        *slot = 0.0;
                    }
                } else {
                    frame[0] = 0.5 * (l + r);
                }

                // Feed the same ring type the mic's input callback fills —
                // see the module docs' "Why the mic is suppressed" section.
                let _ = producer.push(mono[mono_pos]);
                mono_pos = wrap_index(mono_pos, mono_frames);
            }
        },
        move |_err| {
            // OS audio thread: no alloc, no lock, no log — the same
            // discipline as capture.rs's and engine.rs's error callbacks.
            // Nothing observes this today: a dead file-drive stream simply
            // goes silent and analysis holds its last value, which is an
            // acceptable failure mode for a debug-only tool (no rebuild
            // path exists to observe the flag for).
        },
        None,
    )?;
    stream.play()?;

    Ok(BuiltFileDrive {
        stream: FileDriveOutputStream { stream },
        ring: AudioInputRing::new(consumer),
        sample_rate: out_rate,
        duration_secs,
    })
}

/// Raw decode result: interleaved PCM at the file's native rate/channels,
/// before any remix or resample.
struct RawTrack {
    /// Interleaved samples, `channels` per frame, normalized to `[-1, 1)`.
    samples: Vec<f32>,
    channels: u16,
    sample_rate: u32,
}

/// Decode a FLAC file fully into memory via `claxon`. The entire stream is
/// consumed into one `Vec<f32>` — for a several-minute track this is tens
/// of MB, acceptable for a debug-only tool (see the module docs).
fn decode_flac(path: &Path) -> Result<RawTrack, FileDriveError> {
    let mut reader = claxon::FlacReader::open(path)?;
    let info = reader.streaminfo();
    let channels = u16::try_from(info.channels).unwrap_or(2);
    let sample_rate = info.sample_rate;
    let scale = full_scale_recip(info.bits_per_sample);

    let expected = info
        .samples
        .and_then(|n| n.checked_mul(u64::from(channels)))
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0);
    let mut samples = Vec::with_capacity(expected);
    for sample in reader.samples() {
        samples.push(pcm_to_f32(sample?, scale));
    }

    Ok(RawTrack {
        samples,
        channels,
        sample_rate,
    })
}

/// Reciprocal of the full-scale magnitude for a `bits`-deep signed PCM
/// sample (e.g. 16 → 1/32768), used to normalize FLAC's raw integer
/// samples into `[-1, 1)`. FLAC bounds `bits_per_sample` to 4..=32; the
/// `.clamp` keeps the shift amount well-defined even against a malformed
/// header reporting something outside that range.
#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "init-time (once per file) unit conversion; bits is clamped to \
              a tiny, well-defined domain (4..=32) before the cast, exactly \
              the pattern background.rs's decode scale factors use"
)]
fn full_scale_recip(bits: u32) -> f32 {
    let bits = bits.clamp(4, 32);
    1.0 / (1_i64 << (bits - 1)) as f32
}

/// One raw FLAC PCM sample (`i32`) to `[-1, 1)` `f32`. No lossless
/// `From<i32> for f32` exists, so this is an explicit, documented `as`
/// cast — the same pattern `capture.rs` uses for its i16/u16 → f32
/// conversions in the mic callback.
#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "raw FLAC PCM sample -> f32; see doc comment"
)]
fn pcm_to_f32(sample: i32, full_scale_recip: f32) -> f32 {
    sample as f32 * full_scale_recip
}

/// Build an interleaved stereo (2-channel) buffer from an interleaved
/// `channels`-channel source: mono duplicates L into R; already-stereo
/// passes through unchanged; more than 2 channels keeps only the first two
/// (extra surround/height channels are unused by a debug playback tool).
/// Pure, init-time-only (allocates the output `Vec`).
fn to_stereo(src: &[f32], channels: usize) -> Vec<f32> {
    if channels == 0 || src.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity((src.len() / channels) * 2);
    for frame in src.chunks_exact(channels) {
        if channels == 1 {
            out.push(frame[0]);
            out.push(frame[0]);
        } else {
            out.push(frame[0]);
            out.push(frame[1]);
        }
    }
    out
}

/// Downmix an interleaved `channels`-channel source to mono by averaging
/// every channel in each frame — the same signal `AnalysisEngine` expects
/// (mirrors the mic path's `capture::push_mono`). Pure, init-time-only.
fn downmix_to_mono(src: &[f32], channels: usize) -> Vec<f32> {
    if channels == 0 || src.is_empty() {
        return Vec::new();
    }
    let inv = 1.0 / channels_f32(channels);
    src.chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() * inv)
        .collect()
}

/// `channels` as `f32`, losslessly. Real channel counts here are always
/// tiny (FLAC caps at 8), far under `u16`'s range, so routing through
/// `f32::from(u16)` avoids an `as` cast entirely rather than merely
/// documenting one away.
fn channels_f32(channels: usize) -> f32 {
    f32::from(u16::try_from(channels).unwrap_or(u16::MAX))
}

/// Resample interleaved `channels`-channel PCM from `src_rate` to
/// `dst_rate` via per-channel linear interpolation, preserving channel
/// count. Unlike `background::resample_and_remix`, this never remixes
/// channels — `to_stereo`/`downmix_to_mono` already fixed the channel
/// count before resampling — so it stays generic over `channels`. Pure,
/// init-time-only. Empty input, zero channels, or a zero rate resample to
/// empty; an identity rate is a plain copy.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "resample math is in a narrow numeric domain (frame counts \
              bounded by a several-minute track at audio sample rates, well \
              under f64's 2^53 exact-integer range); same rationale as \
              background::resample_and_remix"
)]
fn resample_linear(src: &[f32], channels: usize, src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if src.is_empty() || channels == 0 || src_rate == 0 || dst_rate == 0 {
        return Vec::new();
    }
    let src_frames = src.len() / channels;
    if src_frames == 0 {
        return Vec::new();
    }
    if src_rate == dst_rate {
        return src.to_vec();
    }

    let ratio = f64::from(dst_rate) / f64::from(src_rate);
    let dst_frames = ((src_frames as f64) * ratio).round() as usize;
    if dst_frames == 0 {
        return Vec::new();
    }

    let mut out = vec![0.0_f32; dst_frames * channels];
    let inv_ratio = 1.0 / ratio;
    for i in 0..dst_frames {
        let src_pos = (i as f64) * inv_ratio;
        let src_pos_floor = src_pos.floor();
        let src_idx = (src_pos_floor as usize).min(src_frames - 1);
        let src_idx_next = (src_idx + 1).min(src_frames - 1);
        let frac = (src_pos - src_pos_floor) as f32;

        let base0 = src_idx * channels;
        let base1 = src_idx_next * channels;
        let out_base = i * channels;
        for c in 0..channels {
            let a = src[base0 + c];
            let b = src[base1 + c];
            out[out_base + c] = a + (b - a) * frac;
        }
    }
    out
}

/// Duration in seconds of `frames` frames at `sample_rate` Hz, for the
/// startup log line. Frame counts here are seconds-to-minutes scale at
/// audio sample rates, far under `f32`'s 24-bit exact-integer range.
#[allow(
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "human-readable duration for a log line; see doc comment"
)]
fn frame_duration_secs(frames: usize, sample_rate: u32) -> f32 {
    if sample_rate == 0 {
        return 0.0;
    }
    frames as f32 / sample_rate as f32
}

/// Advance a looping frame cursor by one frame, wrapping at `len`. `len ==
/// 0` returns `0` rather than dividing by zero — a guard against a
/// construction bug (an empty decoded track never reaches this: see
/// `build_file_drive`'s `EmptyTrack` checks), not an expected runtime case.
fn wrap_index(pos: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (pos + 1) % len
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::float_cmp,
    reason = "test assertions; expect_used/float_cmp are appropriate on \
              deterministic test values"
)]
mod tests {
    use super::*;

    // --- wrap_index(): loop wraparound index math ---

    #[test]
    fn wrap_index_cycles_through_a_small_buffer() {
        // len = 3: 0 -> 1 -> 2 -> 0 -> 1 -> ...
        let len = 3;
        let mut pos = 0;
        let seen: Vec<usize> = (0..5)
            .map(|_| {
                pos = wrap_index(pos, len);
                pos
            })
            .collect();
        assert_eq!(seen, vec![1, 2, 0, 1, 2]);
    }

    #[test]
    fn wrap_index_wraps_at_the_last_frame() {
        assert_eq!(wrap_index(2, 3), 0);
        assert_eq!(wrap_index(0, 1), 0);
    }

    #[test]
    fn wrap_index_zero_length_never_panics() {
        assert_eq!(wrap_index(0, 0), 0);
        assert_eq!(wrap_index(41, 0), 0);
    }

    // --- to_stereo() / downmix_to_mono() ---

    #[test]
    fn to_stereo_duplicates_mono() {
        let src = [0.25_f32, 0.5, -1.0];
        let out = to_stereo(&src, 1);
        assert_eq!(out, vec![0.25, 0.25, 0.5, 0.5, -1.0, -1.0]);
    }

    #[test]
    fn to_stereo_passes_through_stereo() {
        let src = [1.0_f32, -1.0, 0.5, 0.5];
        assert_eq!(to_stereo(&src, 2), src.to_vec());
    }

    #[test]
    fn to_stereo_drops_extra_channels() {
        // 4 channels -> keep the first two.
        let src = [1.0_f32, 2.0, 3.0, 4.0];
        assert_eq!(to_stereo(&src, 4), vec![1.0, 2.0]);
    }

    #[test]
    fn downmix_to_mono_averages_stereo() {
        let src = [1.0_f32, -1.0, 0.5, 0.5];
        let out = downmix_to_mono(&src, 2);
        assert_eq!(out, vec![0.0, 0.5]);
    }

    #[test]
    fn downmix_to_mono_passes_through_mono() {
        let src = [0.25_f32, -0.75];
        assert_eq!(downmix_to_mono(&src, 1), src.to_vec());
    }

    #[test]
    fn downmix_and_stereo_of_empty_input_are_empty() {
        assert!(to_stereo(&[], 2).is_empty());
        assert!(downmix_to_mono(&[], 2).is_empty());
        assert!(to_stereo(&[1.0], 0).is_empty());
        assert!(downmix_to_mono(&[1.0], 0).is_empty());
    }

    // --- resample_linear(): correctness on a known ramp ---

    #[test]
    fn resample_identity_rate_is_a_plain_copy() {
        let src = vec![0.1_f32, 0.2, 0.3, 0.4];
        let out = resample_linear(&src, 2, 48_000, 48_000);
        assert_eq!(out, src);
    }

    #[test]
    fn resample_upsamples_a_linear_ramp_exactly() {
        // Mono ramp 0.0, 1.0, 2.0, 3.0 at rate 1 -> rate 2: linear
        // interpolation of a linear ramp reproduces the ramp's own
        // function at every interior fractional position, so each of those
        // output samples equals its ideal continuous-ramp value (up to
        // float rounding). The final destination frame lands PAST the last
        // source frame (src_pos 3.5 > 3), where the resampler clamps to
        // the last frame rather than extrapolating — so it holds 3.0.
        let src = vec![0.0_f32, 1.0, 2.0, 3.0];
        let out = resample_linear(&src, 1, 1, 2);
        // 4 src frames * (2/1) = 8 dst frames: 0.0, 0.5, ..., 3.0, then
        // the clamped 3.0.
        assert_eq!(out.len(), 8);
        for (i, &v) in out.iter().enumerate().take(7) {
            #[allow(
                clippy::as_conversions,
                clippy::cast_precision_loss,
                reason = "test-only exact index cast; i < 8 is exact in f32"
            )]
            let expected = i as f32 * 0.5;
            assert!(
                (v - expected).abs() < 1e-5,
                "index {i}: got {v}, want {expected}"
            );
        }
        assert!(
            (out[7] - 3.0).abs() < 1e-5,
            "the past-the-end frame clamps to the last source frame"
        );
    }

    #[test]
    fn resample_downsamples_frame_count() {
        // 8 mono frames at 44100 -> ~4 frames at 22050.
        let src: Vec<f32> = (0_i16..8).map(f32::from).collect();
        let out = resample_linear(&src, 1, 44_100, 22_050);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn resample_empty_input_is_empty() {
        assert!(resample_linear(&[], 2, 44_100, 48_000).is_empty());
        assert!(resample_linear(&[1.0], 0, 44_100, 48_000).is_empty());
        assert!(resample_linear(&[1.0], 1, 0, 48_000).is_empty());
        assert!(resample_linear(&[1.0], 1, 44_100, 0).is_empty());
    }

    #[test]
    fn resample_last_frame_does_not_index_out_of_bounds() {
        // A ratio that lands the final destination frame's "next" source
        // index past the end must clamp, not panic.
        let src = vec![0.0_f32, 1.0, 2.0];
        let out = resample_linear(&src, 1, 3, 5);
        assert!(!out.is_empty());
    }

    // --- pcm_to_f32() / full_scale_recip() ---

    #[test]
    fn full_scale_recip_matches_known_bit_depths() {
        assert!((full_scale_recip(16) - 1.0 / 32_768.0).abs() < 1e-9);
        assert!((full_scale_recip(8) - 1.0 / 128.0).abs() < 1e-6);
    }

    #[test]
    fn pcm_to_f32_normalizes_16_bit_extremes() {
        let scale = full_scale_recip(16);
        assert!((pcm_to_f32(i32::from(i16::MAX), scale) - 0.999_97).abs() < 1e-3);
        assert!((pcm_to_f32(i32::from(i16::MIN), scale) - (-1.0)).abs() < 1e-6);
        assert!((pcm_to_f32(0, scale) - 0.0).abs() < f32::EPSILON);
    }

    // --- decode_flac(): against the vendored fixture ---

    #[test]
    fn decode_flac_reads_the_vendored_fixture() {
        // tests/fixtures/audio/dance_robot_activate.flac — see
        // tests/fixtures/audio/README.md for provenance/license (CC0 1.0).
        // Not a hot-path call: this runs once, in a #[test].
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/audio/dance_robot_activate.flac");
        let track = decode_flac(&path).expect("vendored fixture must decode");
        assert!(track.channels >= 1);
        assert!(track.sample_rate > 0);

        let frames = track.samples.len() / usize::from(track.channels);
        let duration = frame_duration_secs(frames, track.sample_rate);
        assert!(
            duration > 100.0,
            "expected a multi-minute track, got {duration}s"
        );

        // Non-silence: some sample must clear a small noise-floor threshold.
        assert!(
            track.samples.iter().any(|&s| s.abs() > 0.05),
            "decoded track reads as silence"
        );
    }
}
