//! Operator-customizable screensaver / attract-mode settings (Plan 11.8, Seam 2).
//!
//! A core (not per-sketch) [`SketchSettings`](crate::settings::SketchSettings)
//! resource persisted by the normal settings layer. Today it carries only the
//! optional instruction caption, which is **empty by default** (D6: "by default
//! I just want to communicate visually"); the overlay renders nothing until an
//! operator sets copy.
//!
//! ## Why caption text is operator-set, not hardcoded
//!
//! The sensor usually lives inside a head sculpture, occasionally something else
//! (once a pie). The instruction ("wave your hands over the head") must read
//! correctly for whatever the vessel is at a given install, so it is an operator
//! string, not a constant.
//!
//! ## Serde forward-compatibility
//!
//! Each field carries `#[serde(default)]` so a legacy persisted TOML written
//! before a field existed still deserializes the siblings cleanly — the same
//! pattern documented on `LineSettings`. When adding a field mid-cycle, keep the
//! `#[serde(default)]` attribute.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// Operator-customizable attract-mode parameters.
///
/// Lives as a Bevy `Resource`; the overlay reads it with `Res<ScreensaverSettings>`.
/// Registered with the settings system via `register_sketch_settings` so it
/// appears in the User panel and round-trips through persistence.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "screensaver")]
pub struct ScreensaverSettings {
    /// Instruction headline shown in the attract-mode lower third. Empty
    /// (default) hides the caption entirely — the attract visual communicates
    /// on its own. Example operator value: "Wave your hands over the head".
    #[setting(
        default = String::new(),
        ty = Text,
        section = "Attract Mode",
        category = User
    )]
    #[serde(default)]
    pub caption_headline: String,

    /// Optional secondary line beneath the headline. Empty (default) hides it.
    /// Example: "to conduct the waves".
    #[setting(
        default = String::new(),
        ty = Text,
        section = "Attract Mode",
        category = User
    )]
    #[serde(default)]
    pub caption_subline: String,

    /// Present-rate cap (frames per second) while the screensaver is showing,
    /// regardless of temperature — the Cool-tier wait is derived from it, and
    /// the hotter tiers' waits floor at it so heat only ever lowers the rate
    /// further. The reactive winit loop drives the whole schedule, so every
    /// skipped present also skips that frame's particle compute dispatch and
    /// smear post pass — the primary attract-mode thermal lever. The attract
    /// choreography is a pure function of wall-clock time (pulses ~1.2 s,
    /// paths spanning minutes), so it reads correctly even at low rates.
    /// Default 15: visually adequate for the gentle wandering-pulse look and
    /// roughly half the present/compute energy of the previous fixed 30.
    #[setting(
        default = 15.0_f32,
        min = 5.0,
        max = 60.0,
        step = 1.0,
        section = "Attract Mode",
        category = User,
        label = "Screensaver FPS cap"
    )]
    #[serde(default = "default_screensaver_fps")]
    pub screensaver_fps: f32,
}

/// Serde fallback so a config saved before `screensaver_fps` existed still
/// loads at the documented default.
fn default_screensaver_fps() -> f32 {
    15.0
}

impl ScreensaverSettings {
    /// True when there is any caption copy to render. The overlay early-returns
    /// when this is false so an unconfigured install draws nothing (D6).
    #[must_use]
    pub fn has_caption(&self) -> bool {
        !self.caption_headline.trim().is_empty() || !self.caption_subline.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::trait_def::SketchSettings as _;

    #[test]
    fn default_has_no_caption() {
        let s = ScreensaverSettings::default();
        assert!(!s.has_caption(), "default must render no caption");
        assert!(s.caption_headline.is_empty());
        assert!(s.caption_subline.is_empty());
    }

    #[test]
    fn has_caption_detects_either_line() {
        let headline_only = ScreensaverSettings {
            caption_headline: "Wave your hands".to_string(),
            ..ScreensaverSettings::default()
        };
        assert!(headline_only.has_caption());
        let subline_only = ScreensaverSettings {
            caption_subline: "over the head".to_string(),
            ..ScreensaverSettings::default()
        };
        assert!(subline_only.has_caption());
    }

    #[test]
    fn whitespace_only_caption_is_blank() {
        let s = ScreensaverSettings {
            caption_headline: "   ".to_string(),
            caption_subline: "\t".to_string(),
            ..ScreensaverSettings::default()
        };
        assert!(!s.has_caption(), "whitespace-only copy must not render");
    }

    #[test]
    fn storage_key_is_stable() {
        assert_eq!(ScreensaverSettings::STORAGE_KEY, "screensaver");
    }

    /// Missing-field forward-compat: legacy TOML with only one key still parses.
    #[test]
    #[allow(
        clippy::expect_used,
        clippy::float_cmp,
        reason = "test-only: panic on bad TOML is the intended failure mode; \
                  the serde default is an exact literal"
    )]
    fn missing_field_preserves_sibling() {
        let legacy = r#"caption_headline = "hi""#;
        let parsed: ScreensaverSettings = toml::from_str(legacy).expect("legacy TOML must parse");
        assert_eq!(parsed.caption_headline, "hi");
        assert!(parsed.caption_subline.is_empty());
        // A config saved before the FPS cap existed lands on the default.
        assert_eq!(parsed.screensaver_fps, 15.0);
    }
}
