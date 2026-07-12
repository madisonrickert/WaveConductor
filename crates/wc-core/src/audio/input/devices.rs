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
/// only; allocates the returned names. Both host initialization and
/// enumeration failure (headless CI, no audio subsystem) degrade to an
/// empty list — the dropdown renders empty and the persisted value shows as
/// unavailable, which the settings panel already handles. Host acquisition
/// goes through [`default_host_fallible`] rather than `cpal::default_host()`,
/// which panics instead of returning a `Result` on host-init failure.
fn current_input_device_names() -> Vec<String> {
    let host = match default_host_fallible() {
        Ok(host) => host,
        Err(err) => {
            tracing::warn!(?err, "audio host initialization failed");
            return Vec::new();
        }
    };
    match host.input_devices() {
        Ok(devices) => devices.filter_map(|d| d.name().ok()).collect(),
        Err(err) => {
            tracing::warn!(?err, "audio input device enumeration failed");
            Vec::new()
        }
    }
}

/// Acquire the platform's default cpal host without panicking.
///
/// `cpal::default_host()` initializes the platform host internally as
/// `HostImpl::new().expect("the default host should always be available")`
/// — on every platform cpal 0.16 supports, host-init failure is a `panic!`,
/// not a `Result`. That's plausible on a headless machine or CI runner with
/// no audio subsystem, and contradicts this module's documented "degrades to
/// empty list, never panics" contract. This walks the fallible pair instead:
/// [`cpal::available_hosts`] to see what's actually present, then
/// [`cpal::host_from_id`] to initialize it — mirroring how the existing
/// `input_devices()` `Result` is already handled just below.
///
/// cpal 0.16 has no `default_host_id()` accessor, so the id is chosen by
/// [`pick_default_host_id`] (first available). That matches the true
/// platform default everywhere this project ships: macOS's and Linux's and
/// Windows's default `available_hosts()` (without the optional `asio`
/// feature enabled) each report exactly one entry. It would not
/// necessarily hold with `asio` enabled, where `cpal::default_host()`
/// explicitly prefers WASAPI over ASIO despite ASIO listing first — this
/// project doesn't enable that feature today.
fn default_host_fallible() -> Result<cpal::Host, cpal::HostUnavailable> {
    let available = cpal::available_hosts();
    let id = pick_default_host_id(&available).ok_or(cpal::HostUnavailable)?;
    cpal::host_from_id(id)
}

/// Pick the id to treat as "the default host" out of a list of available
/// ids, currently just the first entry (see [`default_host_fallible`] for
/// the caveat). Split out as a pure, deterministic helper so it has a unit
/// test seam: `cpal::host_from_id`'s actual initialization can't be exercised
/// deterministically in CI, but the selection logic feeding it can.
fn pick_default_host_id(available: &[cpal::HostId]) -> Option<cpal::HostId> {
    available.first().copied()
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
    fn pick_default_host_id_returns_none_when_no_hosts_available() {
        assert_eq!(pick_default_host_id(&[]), None);
    }

    #[test]
    fn pick_default_host_id_returns_the_first_available_host() {
        // Real `available_hosts()` output, not synthetic ids: the id
        // variants that exist are platform-specific (`Alsa` on Linux,
        // `Wasapi` on Windows, `CoreAudio` on macOS), so this stays
        // portable by comparing against the list's own first element rather
        // than hardcoding a variant name.
        let available = cpal::available_hosts();
        if let Some(&first) = available.first() {
            assert_eq!(pick_default_host_id(&available), Some(first));
        }
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
