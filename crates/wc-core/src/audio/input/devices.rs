//! Input-device enumeration for the audio-input device picker.
//!
//! Publishes [`AvailableAudioInputDevices`] and registers it as the
//! runtime-enum options source under the pinned key
//! `"audio_input_devices"` — the exact use case the
//! `crate::settings::runtime_enum` module docs anticipate. Plan C binds a
//! `SettingKind::RuntimeEnum { options_key: "audio_input_devices" }` field
//! on its sketch settings to this list; an empty string / `None` request
//! means the system default input device.
//!
//! Enumeration runs at **event frequency only** (app startup, plus a
//! refresh on the frame a `super::AudioCaptureRequest` is inserted), never
//! per frame — `cpal` enumeration can be slow and allocates the name
//! `String`s, which is fine at that cadence.

use bevy::prelude::*;
use cpal::traits::{DeviceTrait, HostTrait};

use crate::settings::RuntimeEnumOptionsSource;

use super::AudioCaptureRequest;

/// Names of every input device cpal can currently enumerate, in cpal
/// order. Pinned Radiance contract; the runtime-enum source for the
/// `"audio_input_devices"` dropdown.
#[derive(Resource, Default)]
pub struct AvailableAudioInputDevices(pub Vec<String>);

impl RuntimeEnumOptionsSource for AvailableAudioInputDevices {
    const OPTIONS_KEY: &'static str = "audio_input_devices";

    fn options(&self) -> &[String] {
        &self.0
    }
}

/// `Startup` system: seed [`AvailableAudioInputDevices`] with the devices
/// present at launch.
pub fn enumerate_input_devices(mut list: ResMut<'_, AvailableAudioInputDevices>) {
    list.0 = current_input_device_names();
    tracing::info!(count = list.0.len(), devices = ?list.0, "audio input devices enumerated");
}

/// `Update` system: re-enumerate on the frame a capture request is inserted,
/// so the settings dropdown reflects devices plugged in since startup by the
/// time Radiance's panel can show it. No-ops (one `Option` check) on every
/// other frame.
pub fn refresh_devices_on_request_added(
    request: Option<Res<'_, AudioCaptureRequest>>,
    mut list: ResMut<'_, AvailableAudioInputDevices>,
) {
    if request.is_some_and(|r| r.is_added()) {
        list.0 = current_input_device_names();
        tracing::debug!(count = list.0.len(), "audio input devices re-enumerated");
    }
}

/// Enumerate input-device names from the default cpal host. Event-frequency
/// only; allocates the returned names. Enumeration failure (headless CI, no
/// audio subsystem) degrades to an empty list — the dropdown renders empty
/// and the persisted value shows as unavailable, which the settings panel
/// already handles.
fn current_input_device_names() -> Vec<String> {
    let host = cpal::default_host();
    match host.input_devices() {
        Ok(devices) => devices.filter_map(|d| d.name().ok()).collect(),
        Err(err) => {
            tracing::warn!(?err, "audio input device enumeration failed");
            Vec::new()
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;
    use crate::settings::runtime_enum::{options_for, snapshot};
    use crate::settings::{RegisterRuntimeEnumOptionsExt, RuntimeEnumOptionsSource};

    #[test]
    fn options_key_matches_the_pinned_contract() {
        // The string is the whole cross-module contract (see the
        // runtime_enum module docs): Plan C's RadianceSettings field will
        // declare options_key = "audio_input_devices" — a mismatch here
        // degrades into an empty dropdown, so pin it with a test.
        assert_eq!(
            AvailableAudioInputDevices::OPTIONS_KEY,
            "audio_input_devices"
        );
    }

    #[test]
    fn registered_source_resolves_through_the_registry() {
        let mut app = App::new();
        app.register_runtime_enum_options::<AvailableAudioInputDevices>();
        app.insert_resource(AvailableAudioInputDevices(vec![
            "USB Interface".to_owned(),
            "Built-in Microphone".to_owned(),
        ]));
        let snap = snapshot(app.world());
        assert_eq!(
            options_for(&snap, AvailableAudioInputDevices::OPTIONS_KEY).to_vec(),
            vec!["USB Interface".to_owned(), "Built-in Microphone".to_owned()]
        );
    }

    #[test]
    fn refresh_runs_only_on_the_frame_the_request_is_added() {
        // Marker check via change detection semantics: the refresh system's
        // run condition is is_added() on the request. We can't call cpal in
        // an assertion-friendly way here, so this exercises the guard path
        // only: with no request, the device list is untouched by Update.
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.insert_resource(AvailableAudioInputDevices(vec!["Sentinel".to_owned()]));
        app.add_systems(Update, refresh_devices_on_request_added);
        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<AvailableAudioInputDevices>().0,
            vec!["Sentinel".to_owned()],
            "no request added, so the sentinel list must be untouched"
        );
    }
}
