//! Shared camera render-profile vocabulary: the tonemapping operator a sketch
//! selects, the bloom knobs it tunes, and the helpers that write them onto the
//! main `Camera2d`. Centralised here so sketch crates pick a tonemap by name
//! without depending on `bevy::core_pipeline::tonemapping` directly, and so the
//! SDR base (Home/picker) lives in exactly one place.

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::Bloom;
use bevy::reflect::Reflect;
use serde::{Deserialize, Serialize};

/// Bloom intensity the main camera spawns with and resets to outside any sketch
/// (Home/picker). Sketches override it live via their `bloom_intensity` setting.
pub const BASE_BLOOM_INTENSITY: f32 = 0.15;

/// Bloom prefilter threshold the main camera spawns with and resets to (bloom
/// everything). Sketches override it live via their `bloom_threshold` setting.
pub const BASE_BLOOM_THRESHOLD: f32 = 0.0;

/// The camera tonemapping operator a sketch can select, mirrored from Bevy's
/// [`Tonemapping`] so it can back a `ty = Enum` setting (a `Reflect` enum with
/// unit variants). `Default` is [`Self::ReinhardLuminance`] — the chroma-
/// preserving "neon glow" baseline. Variant names are the serialized TOML
/// strings, so do not `#[serde(rename)]` them.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TonemapChoice {
    /// Luminance-only Reinhard: preserves colour ratios as values brighten.
    #[default]
    ReinhardLuminance,
    /// Hue-preserving filmic display transform; gentler highlight rolloff.
    TonyMcMapface,
    /// Sobotka `AgX`: desaturates highlights (filmic, muted).
    AgX,
    /// ACES fitted: punchy/contrasty (shifts hue toward orange in highlights).
    AcesFitted,
    /// No tonemap: linear passthrough (HDR clips at the swapchain). The SDR base.
    None,
}

impl TonemapChoice {
    /// Map to the Bevy [`Tonemapping`] component variant.
    #[must_use]
    pub fn to_bevy(self) -> Tonemapping {
        match self {
            Self::ReinhardLuminance => Tonemapping::ReinhardLuminance,
            Self::TonyMcMapface => Tonemapping::TonyMcMapface,
            Self::AgX => Tonemapping::AgX,
            Self::AcesFitted => Tonemapping::AcesFitted,
            Self::None => Tonemapping::None,
        }
    }
}

/// Write a sketch's render profile onto the main camera's tonemapping + bloom.
/// Called each frame by a sketch's apply system so dev-panel edits are live.
#[allow(
    clippy::float_cmp,
    reason = "intentional bit-exact change-gate: skip the write (and the Changed mark a deref \
              would set) only when the bloom value is byte-identical to what is already there"
)]
pub fn set_camera_render_profile(
    tonemapping: &mut Tonemapping,
    bloom: &mut Bloom,
    choice: TonemapChoice,
    bloom_intensity: f32,
    bloom_threshold: f32,
) {
    let desired = choice.to_bevy();
    if *tonemapping != desired {
        *tonemapping = desired;
    }
    if bloom.intensity != bloom_intensity {
        bloom.intensity = bloom_intensity;
    }
    if bloom.prefilter.threshold != bloom_threshold {
        bloom.prefilter.threshold = bloom_threshold;
    }
}

/// Reset the camera to the SDR base (Home/picker): no tonemap, spawn-default
/// bloom. Called on `OnExit` of every sketch.
pub fn reset_camera_render_profile(tonemapping: &mut Tonemapping, bloom: &mut Bloom) {
    if *tonemapping != Tonemapping::None {
        *tonemapping = Tonemapping::None;
    }
    if bloom.intensity != BASE_BLOOM_INTENSITY {
        bloom.intensity = BASE_BLOOM_INTENSITY;
    }
    if bloom.prefilter.threshold != BASE_BLOOM_THRESHOLD {
        bloom.prefilter.threshold = BASE_BLOOM_THRESHOLD;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::core_pipeline::tonemapping::Tonemapping;

    #[test]
    fn default_is_reinhard_luminance() {
        assert_eq!(TonemapChoice::default(), TonemapChoice::ReinhardLuminance);
    }

    #[test]
    fn every_variant_maps_to_bevy() {
        assert_eq!(
            TonemapChoice::ReinhardLuminance.to_bevy(),
            Tonemapping::ReinhardLuminance
        );
        assert_eq!(
            TonemapChoice::TonyMcMapface.to_bevy(),
            Tonemapping::TonyMcMapface
        );
        assert_eq!(TonemapChoice::AgX.to_bevy(), Tonemapping::AgX);
        assert_eq!(TonemapChoice::AcesFitted.to_bevy(), Tonemapping::AcesFitted);
        assert_eq!(TonemapChoice::None.to_bevy(), Tonemapping::None);
    }

    #[test]
    fn reset_restores_sdr_base() {
        let mut tm = Tonemapping::AgX;
        let mut bloom = Bloom {
            intensity: 0.9,
            ..Bloom::NATURAL
        };
        reset_camera_render_profile(&mut tm, &mut bloom);
        assert_eq!(tm, Tonemapping::None);
        assert!((bloom.intensity - BASE_BLOOM_INTENSITY).abs() < f32::EPSILON);
        assert!((bloom.prefilter.threshold - BASE_BLOOM_THRESHOLD).abs() < f32::EPSILON);
    }
}
