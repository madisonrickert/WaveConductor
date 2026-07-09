//! Operator-customizable screensaver / attract-mode settings (Plan 11.8, Seam 2).
//!
//! A core (not per-sketch) [`SketchSettings`](crate::settings::SketchSettings)
//! resource persisted by the normal settings layer. Carries the attract-mode
//! present-rate cap and the idle-to-attract-mode timeout.
//!
//! ## History: the instruction caption is gone
//!
//! Through 2026-06-10 this struct also carried an operator-set caption
//! (headline + subline) drawn by a lower-third overlay during attract mode.
//! Madison cut it ("get rid of the attract-mode headline") — the attract
//! visual communicates on its own, which had been the default stance (D6)
//! all along. Legacy TOML with `caption_headline` / `caption_subline` keys
//! still parses: serde ignores unknown fields (no `deny_unknown_fields`).
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
///
/// `attract_mode_timeout_secs` is read by
/// `crate::lifecycle::screensaver::sync_attract_timeout_from_settings`
/// (private), which splits it evenly into
/// [`crate::lifecycle::idle::InteractionTimer`]'s two thresholds; the other
/// two fields below are read directly by the framework's present-rate
/// throttle and OS display-sleep-inhibit systems.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "screensaver")]
pub struct ScreensaverSettings {
    /// Present-rate cap (frames per second) while the screensaver is showing,
    /// regardless of temperature — the Cool-tier wait is derived from it, and
    /// the hotter tiers' waits floor at it so heat only ever lowers the rate
    /// further. The reactive winit loop drives the whole schedule, so every
    /// skipped present also skips that frame's particle compute dispatch and
    /// smear post pass — the primary attract-mode thermal lever. The attract
    /// choreography is a pure function of wall-clock time (pulses ~1.2 s,
    /// paths spanning minutes), so it reads correctly even at low rates.
    /// Default 20: hardware-tuned (2026-06-10) — 15 read slightly steppy on
    /// the wandering-pulse look; 20 is smooth while still well under the
    /// previous fixed 30's energy.
    #[setting(
        default = 20.0_f32,
        min = 5.0,
        max = 60.0,
        step = 1.0,
        section = "Attract Mode",
        category = User,
        label = "Screensaver frame cap",
        unit = "fps"
    )]
    #[serde(default = "default_screensaver_fps")]
    pub screensaver_fps: f32,

    /// Hold an OS display-sleep assertion while the app runs, so an
    /// unattended kiosk never has its panel dimmed or slept by the OS
    /// (macOS `IOPMAssertion` / Windows `SetThreadExecutionState` / Linux
    /// D-Bus inhibitor). Default on — a gallery install idles into attract
    /// mode for hours with no input. Turn off for laptop dev sessions where
    /// normal power management is preferable.
    #[setting(
        default = true,
        ty = Boolean,
        section = "Attract Mode",
        category = User,
        label = "Keep display awake"
    )]
    #[serde(default = "default_keep_display_awake")]
    pub keep_display_awake: bool,

    /// Total time of inactivity (mouse, keyboard, touch, or hand tracking)
    /// before the screensaver's attract mode begins. Split evenly by
    /// `crate::lifecycle::screensaver::sync_attract_timeout_from_settings`
    /// (private) into [`crate::lifecycle::idle::InteractionTimer`]'s two internal
    /// stages (`Active → Idle` throttles hand-tracking inference and freezes
    /// some sketch dispatches; `Idle → Screensaver` shows the attract
    /// visual) — that split is an implementation detail, not
    /// operator-facing. Default 60 (30 s + 30 s), matching the app's
    /// long-standing hardcoded behavior before this setting existed.
    #[setting(
        default = 60.0_f32,
        min = 10.0,
        max = 600.0,
        step = 5.0,
        section = "Attract Mode",
        category = User,
        label = "Idle timeout",
        unit = "s"
    )]
    #[serde(default = "default_attract_mode_timeout_secs")]
    pub attract_mode_timeout_secs: f32,
}

/// Serde fallback so a config saved before `screensaver_fps` existed still
/// loads at the documented default.
fn default_screensaver_fps() -> f32 {
    20.0
}

/// Serde fallback: kiosk-first default, the display stays awake.
fn default_keep_display_awake() -> bool {
    true
}

/// Serde fallback so a config saved before `attract_mode_timeout_secs`
/// existed still loads at the documented default (today's hardcoded 60 s).
fn default_attract_mode_timeout_secs() -> f32 {
    60.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::trait_def::SketchSettings as _;

    #[test]
    fn storage_key_is_stable() {
        assert_eq!(ScreensaverSettings::STORAGE_KEY, "screensaver");
    }

    /// Forward-compat: legacy TOML (e.g. with the retired caption keys, or
    /// written before `screensaver_fps` existed) still parses — unknown keys
    /// are ignored, missing keys land on their defaults.
    #[test]
    #[allow(
        clippy::expect_used,
        clippy::float_cmp,
        reason = "test-only: panic on bad TOML is the intended failure mode; \
                  the serde default is an exact literal"
    )]
    fn legacy_toml_with_caption_keys_still_parses() {
        let legacy = r#"caption_headline = "hi""#;
        let parsed: ScreensaverSettings = toml::from_str(legacy).expect("legacy TOML must parse");
        assert_eq!(parsed.screensaver_fps, 20.0);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "test-only: comparing an exact literal default"
    )]
    fn attract_mode_timeout_defaults_to_60_seconds() {
        assert_eq!(
            ScreensaverSettings::default().attract_mode_timeout_secs,
            60.0
        );
    }

    /// Forward-compat: TOML persisted before `attract_mode_timeout_secs`
    /// existed (only setting `screensaver_fps`) still parses, landing the
    /// new field on its documented default.
    #[test]
    #[allow(
        clippy::expect_used,
        clippy::float_cmp,
        reason = "test-only: panic on bad TOML is the intended failure mode; \
                  the serde default is an exact literal"
    )]
    fn legacy_toml_without_attract_timeout_key_still_parses() {
        let legacy = "screensaver_fps = 25.0";
        let parsed: ScreensaverSettings = toml::from_str(legacy).expect("legacy TOML must parse");
        assert_eq!(parsed.screensaver_fps, 25.0);
        assert_eq!(parsed.attract_mode_timeout_secs, 60.0);
    }
}
