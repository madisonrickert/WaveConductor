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

/// How long the chosen device name must stop changing before a change is acted
/// on.
///
/// The runtime-enum widget's free-text half writes back **per keystroke** (see
/// `SettingKind::RuntimeEnum`'s docs): typing `"Living Room TV"` walks the field
/// through `"L"`, `"Li"`, `"Liv"`, … A consumer that opened a device per observed
/// value would try to open each of those prefixes — tearing down and blocking-
/// reopening a cpal stream on the main thread, per keystroke. So nothing is acted
/// on until the value has been still for this long. A dropdown pick (the normal
/// path) writes the whole name at once and simply waits out the window.
#[cfg(not(target_arch = "wasm32"))]
const OUTPUT_DEVICE_SETTLE: std::time::Duration = std::time::Duration::from_millis(500);

/// Debounce state for [`apply_output_device_change`], held as that system's
/// `Local`. Pure — the clock is passed in — so the whole rule is unit-testable
/// without an `App`, an audio device, or a real half-second.
///
/// It is *seeded* on its first poll rather than starting empty: the value it
/// first sees is the persisted one that `engine::start_audio_engine` has already
/// opened, so treating it as a change would rebuild the stream a half-second into
/// every session.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default)]
pub struct OutputDeviceDebounce {
    /// Whether the persisted value has been adopted as the baseline (see above).
    seeded: bool,
    /// The value observed on the previous poll. A change restarts the timer.
    last_seen: String,
    /// The last value actually acted on. Guards against re-firing when a value
    /// settles back to where it already was (type a letter, delete it).
    applied: String,
    /// Monotonic time at which the current value becomes settled, or `None` when
    /// nothing is pending.
    settle_at: Option<f64>,
}

#[cfg(not(target_arch = "wasm32"))]
impl OutputDeviceDebounce {
    /// Observe `current` at monotonic time `now`; `true` exactly once, on the
    /// frame the value has been unchanged for [`OUTPUT_DEVICE_SETTLE`] **and**
    /// differs from the last value acted on.
    ///
    /// Allocation-free in steady state: the common case is `last_seen == current`
    /// with no timer armed, which compares two strings and returns. The `String`
    /// buffers are refilled with `clear` + `push_str` (capacity is kept), and only
    /// on an actual edit.
    fn poll(&mut self, current: &str, now: f64) -> bool {
        if !self.seeded {
            self.seeded = true;
            Self::replace(&mut self.last_seen, current);
            Self::replace(&mut self.applied, current);
            return false;
        }
        if self.last_seen != current {
            Self::replace(&mut self.last_seen, current);
            self.settle_at = Some(now + OUTPUT_DEVICE_SETTLE.as_secs_f64());
            return false;
        }
        match self.settle_at {
            Some(at) if now >= at => {
                self.settle_at = None;
                if self.applied == current {
                    return false;
                }
                Self::replace(&mut self.applied, current);
                true
            }
            _ => false,
        }
    }

    /// Refill an owned buffer in place, keeping its capacity.
    fn replace(buffer: &mut String, value: &str) {
        buffer.clear();
        buffer.push_str(value);
    }
}

/// `Update` system: act on an operator's device selection **without waiting for
/// the next restart**, by routing it through the existing reconnect machinery.
///
/// A settled change asks [`crate::audio::supervisor::AudioSupervisor::request_now`]
/// for an immediate attempt; `supervise_audio` (ordered after this) then rebuilds
/// the stream on the same frame, and `rebuild_engine` re-resolves the device from
/// the very [`AudioSettings`] value that just changed. There is no second,
/// eager device-open path to keep in step with the recovery one.
///
/// ## It never acts on a partial name
///
/// Two independent guards, because the widget's free-text half writes per
/// keystroke:
///
/// 1. **Debounce** ([`OutputDeviceDebounce`]): the value must be still for
///    `OUTPUT_DEVICE_SETTLE` (500 ms). A prefix typed on the way to a full name
///    is never settled.
/// 2. **Actionability**: a non-empty name that is not in the live
///    [`crate::audio::device::AvailableAudioDevices`] list is *not* acted on at
///    all — and no prefix of a real device name is itself a device name. This is
///    also the correct behaviour for the case that matters: a name that matches
///    nothing is a device that is merely **away** (a sleeping HDMI TV), so the
///    name is kept, the current stream is left alone, and the watcher's
///    migrate-back opens it the moment it reappears. The saved name is never
///    rewritten or cleared — this system only ever *reads* it.
///
/// The empty string is always actionable: it is the explicit "follow the system
/// default" sentinel, and re-resolving it re-opens the host default.
///
/// ## Per-frame cost
///
/// Two string comparisons and a return. No allocation, no enumeration (the device
/// list is the watcher's, already built), no cpal call. The blocking rebuild
/// happens in `supervise_audio`, only on the frame a change actually settles.
#[cfg(not(target_arch = "wasm32"))]
pub fn apply_output_device_change(
    settings: Res<'_, AudioSettings>,
    available: Res<'_, crate::audio::device::AvailableAudioDevices>,
    bound: Res<'_, crate::audio::device::BoundOutputDevice>,
    mut supervisor: ResMut<'_, crate::audio::supervisor::AudioSupervisor>,
    time: Res<'_, Time<Real>>,
    mut debounce: Local<'_, OutputDeviceDebounce>,
) {
    let now = time.elapsed_secs_f64();
    if !debounce.poll(&settings.output_device, now) {
        return;
    }

    let chosen = settings.output_device.as_str();
    if !chosen.is_empty() {
        if !available.0.iter().any(|name| name == chosen) {
            // Guard 2: the chosen endpoint is not currently enumerated. Keep the
            // name (this system never writes it), keep the stream we have, and let
            // the watcher migrate back when the device reappears.
            tracing::info!(
                device = %chosen,
                "chosen audio output device is not currently available; keeping the saved name \
                 and staying on the current endpoint until it reappears"
            );
            return;
        }
        if bound.0.as_deref() == Some(chosen) {
            // Already playing there (e.g. the operator explicitly picked the
            // device the host default happened to be). Nothing to rebuild.
            return;
        }
    }

    tracing::info!(
        device = %chosen,
        "audio output device changed; requesting an immediate stream rebuild"
    );
    supervisor.request_now(now);
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

    /// The derive macro's `options_key` (on `output_device`) and the options
    /// source's `OPTIONS_KEY` (on `AvailableAudioDevices`) live in different files
    /// and are tied together by nothing but a string literal; 03a resolves an
    /// unknown key to an empty option list, so a drift between them degrades the
    /// dropdown *silently* — and an empty dropdown with the saved value marked
    /// "(unavailable)" is byte-for-byte what a correctly-wired but sleeping TV
    /// looks like. Pin them together. (`crate::audio::tests::the_output_device_fields_options_key_resolves_against_a_registered_source`
    /// covers the other half: that the source is actually *registered*.)
    ///
    /// `unreachable!` (not `panic!`) on the two structural invariants: the derive
    /// always emits a def for every field, and `output_device` is declared
    /// `ty = RuntimeEnum`, so neither `else` arm is reachable unless the struct
    /// above changed out from under this test.
    #[test]
    fn output_device_options_key_matches_its_options_source() {
        use crate::audio::device::AvailableAudioDevices;
        use crate::settings::{RuntimeEnumOptionsSource, SettingKind, SketchSettings};

        let Some(def) = AudioSettings::settings_def()
            .into_iter()
            .find(|d| d.field_name == "output_device")
        else {
            unreachable!("the derive macro always emits a def for `output_device`");
        };
        let SettingKind::RuntimeEnum { options_key } = def.kind else {
            unreachable!("`output_device` is declared `ty = RuntimeEnum`");
        };
        assert_eq!(options_key, AvailableAudioDevices::OPTIONS_KEY);
    }

    /// The device picker must never be marked `requires_restart`: the widget's
    /// free-text half writes per keystroke, and the restart diff is a *value*
    /// diff, so it would fire one `SketchRestart` per typed character. The change
    /// is applied live instead, through the debounced
    /// [`apply_output_device_change`] → supervisor path.
    #[test]
    fn output_device_does_not_require_a_restart() {
        use crate::settings::SketchSettings;

        let Some(def) = AudioSettings::settings_def()
            .into_iter()
            .find(|d| d.field_name == "output_device")
        else {
            unreachable!("the derive macro always emits a def for `output_device`");
        };
        assert!(
            !def.requires_restart,
            "a per-keystroke field must not fire a restart per keystroke"
        );
    }

    /// The debounce rule, which is what keeps a half-typed device name from ever
    /// reaching a cpal open.
    #[cfg(not(target_arch = "wasm32"))]
    mod debounce {
        use super::*;

        /// One frame past the settle window.
        fn settled(at: f64) -> f64 {
            at + OUTPUT_DEVICE_SETTLE.as_secs_f64() + 0.001
        }

        /// The persisted value is the baseline: `start_audio_engine` has already
        /// opened it. Seeing it on the first frames must not look like a change,
        /// or every session would rebuild its stream half a second in.
        #[test]
        fn the_persisted_value_is_adopted_without_firing() {
            let mut debounce = OutputDeviceDebounce::default();
            assert!(!debounce.poll("LG TV (HDMI)", 0.0));
            assert!(!debounce.poll("LG TV (HDMI)", 10.0));
            assert!(!debounce.poll("LG TV (HDMI)", 10_000.0));
        }

        /// A settled edit fires exactly once, then stays quiet.
        #[test]
        fn a_settled_change_fires_once() {
            let mut debounce = OutputDeviceDebounce::default();
            assert!(!debounce.poll("", 0.0));

            assert!(!debounce.poll("LG TV (HDMI)", 1.0), "not settled yet");
            assert!(
                !debounce.poll("LG TV (HDMI)", 1.1),
                "still inside the settle window"
            );
            assert!(debounce.poll("LG TV (HDMI)", settled(1.0)));
            // And never again for the same value.
            assert!(!debounce.poll("LG TV (HDMI)", settled(1.0) + 1.0));
            assert!(!debounce.poll("LG TV (HDMI)", 10_000.0));
        }

        /// The defect this exists to prevent: the free-text half of 03a's widget
        /// writes back per keystroke, so a naive consumer would try to open the
        /// devices `"L"`, `"Li"`, `"Liv"`, … Only the name the operator stopped
        /// typing ever settles.
        #[test]
        fn typing_a_name_never_settles_on_a_prefix() {
            let mut debounce = OutputDeviceDebounce::default();
            assert!(!debounce.poll("", 0.0));

            // ~100 ms per keystroke — nowhere near the settle window.
            let mut now = 1.0_f64;
            for prefix in ["L", "LG", "LG ", "LG T", "LG TV"] {
                assert!(
                    !debounce.poll(prefix, now),
                    "a prefix must never settle: {prefix}"
                );
                now += 0.1;
            }
            // The operator stops typing. Only now does the full name settle.
            assert!(!debounce.poll("LG TV", now));
            assert!(debounce.poll("LG TV", settled(now)));
        }

        /// Type a letter, think better of it, delete it. The value is back where it
        /// started, so there is nothing to act on — even though it *changed* twice.
        #[test]
        fn a_change_reverted_before_it_settles_does_not_fire() {
            let mut debounce = OutputDeviceDebounce::default();
            assert!(!debounce.poll("LG TV", 0.0));

            assert!(!debounce.poll("LG TVx", 1.0));
            assert!(!debounce.poll("LG TV", 1.1));
            assert!(!debounce.poll("LG TV", settled(1.1)));
        }

        /// Clearing the field is a real choice — "follow the system default" — and
        /// must settle like any other value.
        #[test]
        fn clearing_the_field_settles_as_the_system_default() {
            let mut debounce = OutputDeviceDebounce::default();
            assert!(!debounce.poll("LG TV (HDMI)", 0.0));

            assert!(!debounce.poll("", 1.0));
            assert!(debounce.poll("", settled(1.0)));
        }
    }
}
