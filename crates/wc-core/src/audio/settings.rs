//! Audio-engine settings, persisted across sessions.
//!
//! One field today: the output device, stored **by name**. Empty means "follow
//! the system default". The settings panel renders it with Plan 03a's
//! runtime-enumerated dropdown, whose options come from
//! [`crate::audio::device::AvailableAudioDevices`]. A saved name that is not in
//! the live list is shown and kept, never rewritten (a sleeping HDMI TV must
//! keep its binding — see [`crate::audio::device::resolve_output_device`],
//! which takes the saved name as `&str` so that is true at the type level).
//!
//! ## Where the saved name is read
//!
//! Three places, all of them *resolvers*, none of them writers:
//! `engine::start_audio_engine` (the first build), `engine::rebuild_engine`
//! (every reconnect), and `device::drain_device_topology` (the migrate-back
//! check, which fires when the saved endpoint reappears in the watcher's list).

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// Global audio settings (not per-sketch).
///
/// Registered by [`crate::audio::AudioPlugin`] under the storage key `"audio"`,
/// which routes it to the settings dock's Display tab (see
/// `settings::panel_user::dock::tab_for_storage_key`, whose map is total).
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "audio")]
pub struct AudioSettings {
    /// Output device name. Empty = follow the system default output. When set,
    /// the engine opens the matching device at startup and after every
    /// reconnect; if the name is not currently enumerated (e.g. an HDMI TV
    /// asleep) the engine falls back to the default **and keeps this value**,
    /// so the binding is restored when the device reappears.
    ///
    /// Rendered with Plan 03a's runtime-enumerated widget: the panel resolves
    /// `options_key` against the `RuntimeEnumOptionsSource` registry, which
    /// binds it to [`crate::audio::device::AvailableAudioDevices`] under the key
    /// `"audio_output_devices"`. The key is a plain string literal — the derive
    /// never names the concrete options resource.
    #[setting(
        default = String::new(),
        ty = RuntimeEnum,
        options_key = "audio_output_devices",
        category = User,
        section = "Audio",
        label = "Audio output device"
    )]
    #[serde(default)]
    pub output_device: String,
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_empty_meaning_system_default() {
        let settings = AudioSettings::default();
        assert!(
            settings.output_device.is_empty(),
            "empty = follow the system default"
        );
    }

    #[test]
    fn output_device_persists_as_the_name_string() {
        let settings = AudioSettings {
            output_device: "LG TV (HDMI)".to_owned(),
        };
        let text = toml::to_string(&settings).expect("serialize");
        assert!(
            text.contains("output_device = \"LG TV (HDMI)\""),
            "got: {text}"
        );
    }

    #[test]
    fn a_saved_name_round_trips_and_survives_an_absent_field() {
        let settings = AudioSettings {
            output_device: "LG TV (HDMI)".to_owned(),
        };
        let text = toml::to_string(&settings).expect("serialize");
        let back: AudioSettings = toml::from_str(&text).expect("parse back");
        assert_eq!(back.output_device, "LG TV (HDMI)");
        // A config saved before this field existed loads as the default.
        let old: AudioSettings = toml::from_str("").expect("empty config loads");
        assert_eq!(old, AudioSettings::default());
    }
}
