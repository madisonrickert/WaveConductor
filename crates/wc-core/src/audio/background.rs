//! Background-sample loading and named sample bank construction.
//!
//! Provides the asset pipeline that decodes every sketch's encoded audio
//! samples on the main thread and hands them to the audio engine as a
//! [`super::sample_bank::SampleBank`]. The looping background bed (e.g.
//! `line_background.ogg`) is just one named entry; Cymatics adds more in
//! Task C4.
//!
//! ## Types
//!
//! - [`EncodedSample`] — one named encoded sample (Ogg/Vorbis bytes).
//! - [`SampleAssets`] — Bevy resource the binary inserts before `App::run`;
//!   the engine startup system decodes every entry and builds the bank.
//! - [`build_sample_bank`] — decode + resample all entries into the engine
//!   output format and construct a [`super::sample_bank::SampleBank`].
//!
//! Decode helpers ([`decode_to_interleaved_f32`], [`resample_and_remix`]) are
//! unchanged from the original single-background path.
//!
//! ## Real-time safety
//!
//! Decoding and resampling run on the **main thread** before the cpal callback
//! starts producing samples. The audio thread only sees a finalized, immutable
//! [`super::sample_bank::SampleBank`] and reads from it via index lookups.
//! No symphonia code runs on the audio thread.
//!
//! ## Sample-rate handling
//!
//! `line_background.ogg` is 44.1 kHz stereo. macOS commonly reports a 48 kHz
//! default output stream. [`resample_and_remix`] resamples with linear
//! interpolation so the loop plays at the correct musical pitch on any output
//! rate. A higher-quality resampler is Plan 10 polish if needed.
//!
//! ## Missing-asset fallback
//!
//! If an OGG cannot be loaded or decoded, [`build_sample_bank`] logs a warning
//! and skips that entry (the engine must always start). An absent
//! `line_background` entry means the background voice contributes silence;
//! the engine never refuses to start because of a missing asset.

use std::io::Cursor;

use bevy::prelude::*;
use symphonia::core::audio::{SampleBuffer, SignalSpec};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// One encoded sample (Ogg/Vorbis bytes) the binary hands to the engine.
#[derive(Debug, Clone)]
pub struct EncodedSample {
    /// Bank entry name (e.g. `"line_background"`, `"cymatics_kick"`).
    pub name: &'static str,
    /// Encoded container bytes. Empty entries are skipped by the bank builder.
    pub bytes: Vec<u8>,
}

/// Encoded sample assets the binary inserts before `App::run`.
///
/// Replaces the former single `BackgroundSampleAsset`: the engine startup
/// system decodes every entry once and builds a
/// [`super::sample_bank::SampleBank`]. The looping background bed is just the
/// `"line_background"` entry.
#[derive(Resource, Debug, Default, Clone)]
pub struct SampleAssets {
    /// Named encoded samples.
    pub samples: Vec<EncodedSample>,
}

impl SampleAssets {
    /// True when no samples are present (engine starts silent).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// Decode + resample every entry in `assets` into the engine output format and
/// build a [`super::sample_bank::SampleBank`]. Decode/resample failures are
/// logged and skipped (the engine must always start), mirroring the former
/// single-background path.
#[must_use]
pub fn build_sample_bank(
    assets: &SampleAssets,
    channels: u16,
    sample_rate: u32,
) -> super::sample_bank::SampleBank {
    use super::sample_bank::{SampleBank, SampleData};
    let mut entries: Vec<(&'static str, SampleData)> = Vec::new();
    for asset in &assets.samples {
        if asset.bytes.is_empty() {
            continue;
        }
        match decode_to_interleaved_f32(&asset.bytes) {
            Ok(decoded) => {
                let resampled = resample_and_remix(
                    &decoded.pcm,
                    decoded.channels,
                    decoded.sample_rate,
                    channels,
                    sample_rate,
                );
                tracing::info!(
                    name = asset.name,
                    frames = resampled.len() / usize::from(channels.max(1)),
                    "decoded sample for bank"
                );
                entries.push((asset.name, SampleData::new(resampled, channels)));
            }
            Err(err) => {
                tracing::warn!(
                    name = asset.name,
                    ?err,
                    "sample decode failed; skipping bank entry"
                );
            }
        }
    }
    SampleBank::from_samples(entries)
}

/// Result of [`decode_to_interleaved_f32`]: PCM ready for the audio thread,
/// plus the source spec for downstream resampling.
#[derive(Debug)]
pub struct DecodedSample {
    /// Interleaved samples in `[L, R, L, R, ...]` layout (or mono if the
    /// source is single-channel).
    pub pcm: Vec<f32>,
    /// Channel count of `pcm` (1 = mono, 2 = stereo). Other layouts are
    /// downmixed/padded by [`resample_and_remix`] to match the output.
    pub channels: u16,
    /// Sample rate of `pcm` in Hz, as reported by the decoder.
    pub sample_rate: u32,
}

impl DecodedSample {
    /// Number of frames (sample groups across channels) in `pcm`.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.pcm.len() / usize::from(self.channels)
        }
    }
}

/// Errors emitted by [`decode_to_interleaved_f32`]. All variants are treated
/// by the engine as "fall back to silence"; the engine never refuses to
/// start because of a decode failure.
#[derive(Debug, thiserror::Error)]
pub enum BackgroundDecodeError {
    /// Symphonia could not probe a container or codec for the bytes.
    #[error("symphonia: {0}")]
    Symphonia(#[from] SymphoniaError),
    /// The container had no usable audio track.
    #[error("no decodable audio track in container")]
    NoTrack,
    /// The decoded `SignalSpec` was missing a sample rate. (Vorbis always
    /// carries one; this is defensive for other codecs.)
    #[error("track is missing a sample rate")]
    MissingSampleRate,
}

/// Decode an Ogg/Vorbis (or any symphonia-supported) byte slice into
/// interleaved `f32` PCM. Returns the source channel count and sample rate
/// alongside the buffer so the caller can adapt to the cpal output format.
///
/// The function consumes the entire stream into a `Vec<f32>`; for typical
/// background loops (under a minute, stereo, 44.1 kHz) this is ~10 MB and
/// fits comfortably in RAM. Larger samples should use a streaming approach.
pub fn decode_to_interleaved_f32(bytes: &[u8]) -> Result<DecodedSample, BackgroundDecodeError> {
    // Wrap the slice in a Cursor so symphonia's MediaSourceStream has a
    // `Read + Seek` it can probe. `Cursor<Vec<u8>>` is `Send + 'static`
    // which the MediaSourceStream constructor requires.
    let cursor = Cursor::new(bytes.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());

    let probed = symphonia::default::get_probe().format(
        Hint::new().with_extension("ogg"),
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or(BackgroundDecodeError::NoTrack)?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();
    let sample_rate = codec_params
        .sample_rate
        .ok_or(BackgroundDecodeError::MissingSampleRate)?;

    let mut codec =
        symphonia::default::get_codecs().make(&codec_params, &DecoderOptions::default())?;

    let mut pcm: Vec<f32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut spec_channels: u16 = 0;

    loop {
        // `next_packet` returns `IoError(UnexpectedEof)` at clean EOF and
        // `ResetRequired` on container resync. Both end our decode loop.
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(err) => return Err(err.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match codec.decode(&packet) {
            Ok(d) => d,
            // Skip malformed packets rather than abort the whole loop;
            // a single bad packet shouldn't kill the ambient bed.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(err) => return Err(err.into()),
        };

        // First-packet path: size and allocate the SampleBuffer to match
        // the decoded SignalSpec. `decoded.capacity()` is the per-packet
        // frame capacity (e.g. 1024); `u64::try_from` is safe on every
        // architecture we support.
        let spec: SignalSpec = *decoded.spec();
        if sample_buf.is_none() {
            let capacity = u64::try_from(decoded.capacity()).unwrap_or(0);
            sample_buf = Some(SampleBuffer::<f32>::new(capacity, spec));
            // `Channels::count` returns `usize`; clamp to u16 to fit
            // cpal's channel-count contract. Real-world OGGs are mono or
            // stereo, so this clamp is defensive.
            spec_channels = u16::try_from(spec.channels.count()).unwrap_or(2);
        }

        // The block above guarantees `sample_buf` is `Some`; we re-borrow
        // mutably here. `if let` keeps clippy's `single_match_else` happy
        // and avoids `unwrap`/`expect` per the workspace lint policy.
        if let Some(buf) = sample_buf.as_mut() {
            buf.copy_interleaved_ref(decoded);
            pcm.extend_from_slice(buf.samples());
        }
    }

    Ok(DecodedSample {
        pcm,
        channels: spec_channels.max(1),
        sample_rate,
    })
}

/// Resample interleaved PCM from `(src_rate, src_channels)` to
/// `(dst_rate, dst_channels)` via per-channel linear interpolation.
///
/// Channel remixing is minimal:
/// - Mono → stereo: duplicate L into R.
/// - Stereo → mono: average L+R.
/// - Channel counts above 2 are downmixed by taking the first two channels.
///
/// Returns an interleaved `Vec<f32>` sized to `dst_channels × dst_frames`.
/// An empty input produces an empty output.
///
/// ### Numeric-cast safety
///
/// Two `as` casts are deliberate and rationalized inline:
/// 1. `usize → f64` for `src_frames` and the loop index `i`. Background
///    samples are minute-scale at most (~3 million stereo frames at 44.1
///    kHz × 60 s), well under f64's 2^53 exact-integer range.
/// 2. `f64 → usize` for the destination frame count and per-frame source
///    index. The source values are `>= 0` (`floor` after multiplying by a
///    positive ratio) and bounded above by `src_frames`, so the cast is
///    well-defined.
/// 3. `f64 → f32` for the fractional part of the interpolation position.
///    The value is in `[0, 1)` and survives f32 truncation cleanly.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "documented above; resample math is in a narrow numeric domain"
)]
pub fn resample_and_remix(
    src_pcm: &[f32],
    src_channels: u16,
    src_rate: u32,
    dst_channels: u16,
    dst_rate: u32,
) -> Vec<f32> {
    if src_pcm.is_empty() || src_channels == 0 || dst_channels == 0 {
        return Vec::new();
    }

    let src_channels_usize = usize::from(src_channels);
    let dst_channels_usize = usize::from(dst_channels);
    let src_frames = src_pcm.len() / src_channels_usize;
    if src_frames == 0 {
        return Vec::new();
    }

    // Frame ratio: how many destination frames per source frame. Linear
    // interpolation samples src_frames * (dst_rate/src_rate) destination
    // frames. We round to the nearest frame count to avoid drift over long
    // loops.
    let ratio = f64::from(dst_rate) / f64::from(src_rate);
    let dst_frames_f = (src_frames as f64) * ratio;
    let dst_frames = dst_frames_f.round() as usize;
    if dst_frames == 0 {
        return Vec::new();
    }

    let mut out = vec![0.0_f32; dst_frames * dst_channels_usize];

    // Helper: read a source frame's L/R-equivalent into `(l, r)`. This is
    // where mono↔stereo remixing happens. We only consider channels 0 and 1
    // because the audio output is stereo; multi-channel sources downmix to
    // their first two channels.
    let frame_lr = |frame_idx: usize| -> (f32, f32) {
        let base = frame_idx * src_channels_usize;
        if src_channels == 1 {
            let s = src_pcm[base];
            (s, s)
        } else {
            let l = src_pcm[base];
            let r = src_pcm[base + 1];
            (l, r)
        }
    };

    // Per-frame linear interpolation. For destination frame `i`, the
    // corresponding source frame is `i / ratio`. We split into integer +
    // fractional parts and lerp between two adjacent source frames.
    let inv_ratio = 1.0 / ratio;
    for i in 0..dst_frames {
        let src_pos = (i as f64) * inv_ratio;
        let src_pos_floor = src_pos.floor();
        let src_idx = src_pos_floor as usize;
        let frac = (src_pos - src_pos_floor) as f32;
        let src_idx_next = (src_idx + 1).min(src_frames - 1);

        let (l0, r0) = frame_lr(src_idx);
        let (l1, r1) = frame_lr(src_idx_next);
        let l = l0 + (l1 - l0) * frac;
        let r = r0 + (r1 - r0) * frac;

        let out_base = i * dst_channels_usize;
        if dst_channels == 1 {
            // Stereo → mono via simple average.
            out[out_base] = 0.5 * (l + r);
        } else {
            out[out_base] = l;
            out[out_base + 1] = r;
            // Pad any extra channels with silence; surround setups will
            // hear the bed in the front L/R only.
            for slot in &mut out[out_base + 2..out_base + dst_channels_usize] {
                *slot = 0.0;
            }
        }
    }

    out
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "EPSILON comparisons are appropriate for test assertions on clean f32 values"
)]
mod tests {
    use super::*;

    #[test]
    fn build_sample_bank_decodes_named_entries() {
        // Encode-free smoke: an empty assets set yields an empty bank.
        let assets = SampleAssets::default();
        let bank = build_sample_bank(&assets, 2, 48_000);
        assert!(bank.index_of("anything").is_none());
    }

    #[test]
    fn decoded_sample_frame_count_handles_stereo() {
        let s = DecodedSample {
            pcm: vec![0.0; 100],
            channels: 2,
            sample_rate: 48_000,
        };
        assert_eq!(s.frame_count(), 50);
    }

    #[test]
    fn decoded_sample_frame_count_handles_empty() {
        let s = DecodedSample {
            pcm: Vec::new(),
            channels: 2,
            sample_rate: 48_000,
        };
        assert_eq!(s.frame_count(), 0);
    }

    #[test]
    fn empty_input_resamples_to_empty() {
        let out = resample_and_remix(&[], 2, 44_100, 2, 48_000);
        assert!(out.is_empty());
    }

    #[test]
    fn identity_resample_is_lossless_for_dc_signal() {
        // A constant signal sampled at any rate should still be the same
        // constant after linear-interp resampling.
        let src: Vec<f32> = std::iter::repeat_n([0.5_f32, 0.5_f32], 100)
            .flatten()
            .collect();
        let out = resample_and_remix(&src, 2, 48_000, 2, 48_000);
        assert_eq!(out.len(), src.len());
        for s in &out {
            assert!((s - 0.5).abs() < 1e-5, "DC drift: got {s}");
        }
    }

    #[test]
    fn upsample_doubles_frame_count() {
        // 4 stereo frames at 22050 → 8 stereo frames at 44100.
        let src: Vec<f32> = vec![0.0, 0.0, 1.0, 1.0, 0.0, 0.0, -1.0, -1.0];
        let out = resample_and_remix(&src, 2, 22_050, 2, 44_100);
        assert_eq!(out.len(), 16);
    }

    #[test]
    fn mono_upmixes_to_stereo() {
        let src: Vec<f32> = vec![0.25, 0.5, 0.75, 1.0];
        let out = resample_and_remix(&src, 1, 48_000, 2, 48_000);
        // Four mono frames in → four stereo frames out (8 samples).
        assert_eq!(out.len(), 8);
        // L and R should match the mono source for each frame.
        for i in 0..4 {
            assert!((out[i * 2] - src[i]).abs() < 1e-5);
            assert!((out[i * 2 + 1] - src[i]).abs() < 1e-5);
        }
    }

    #[test]
    fn stereo_downmixes_to_mono_by_average() {
        let src: Vec<f32> = vec![1.0, -1.0, 0.5, 0.5];
        let out = resample_and_remix(&src, 2, 48_000, 1, 48_000);
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.0).abs() < 1e-5);
        assert!((out[1] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn decode_garbage_bytes_returns_error_not_panic() {
        // A handful of zero bytes is not a valid Ogg container; the decoder
        // should reject it via a Symphonia error, not a panic.
        let bytes = vec![0_u8; 64];
        let err = decode_to_interleaved_f32(&bytes);
        assert!(err.is_err(), "garbage bytes must not decode");
    }
}
