//! OBSBOT camera control: put the app — not the camera's on-device AI — in
//! charge of framing.
//!
//! At the last live deployment the OBSBOT Tiny 2 Lite's onboard AI tracking
//! and gesture control fought WaveConductor's own `MediaPipe` tracking (both
//! systems reacted to the same person), so the camera went unused. This
//! module takes programmatic control over the vendored OBSBOT libdev SDK
//! (`vendor/libdev`, behind the `obsbot-camera-control` feature): when the
//! app starts it disables on-device AI tracking and gesture control,
//! recenters the gimbal, selects the widest FOV, and explicitly re-asserts
//! auto exposure; on clean shutdown it restores the camera's out-of-the-box
//! behavior so other software isn't surprised.
//!
//! ## Data flow
//!
//! ```text
//! SDK hotplug callback (SDK thread) ──► atomic epoch in the C++ shim
//!                                            │  polled by
//!                                            ▼
//! obsbot-control worker thread  ── std::sync::mpsc ──►  drain_worker_status
//!   (all device IO lives here;                            (PreUpdate system)
//!    blocking SDK calls never                                   │
//!    touch the Bevy schedule)                                   ▼
//!                        ▲                              Res<ObsbotControl>
//!                        └── WorkerCommand channel ◄──  manual APIs +
//!                                                       settings watcher
//! ```
//!
//! Real device IO is **Windows-only** (the deployment target; see
//! `platform/`): elsewhere [`platform::spawn_worker`](crate::input::obsbot::platform::spawn_worker) returns `None` and the
//! resource reports [`ObsbotStatus::NoDevice`](crate::input::obsbot::ObsbotStatus::NoDevice) forever, which keeps CI's
//! `--all-features` builds green on every runner without a C++ toolchain.
//!
//! The systems added here run in every [`SketchActivity`] state by design —
//! like the settings-reload listeners, they are cheap message drains
//! (`try_recv` on an empty channel; no allocation, no device IO on the Bevy
//! thread), and a camera plugged in during the screensaver must still be
//! captured.
//!
//! [`SketchActivity`]: crate::lifecycle::state::SketchActivity

pub mod platform;

use bevy::prelude::*;
use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

use crate::settings::RegisterSketchSettingsExt;

bitflags! {
    /// The take-control steps, as a bitmask of which steps **succeeded**.
    ///
    /// Bit values mirror the `OBSBOT_STEP_*` constants in
    /// `vendor/libdev/shim/obsbot_shim.h` — keep the two in sync.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ControlSteps: u32 {
        /// On-device AI tracking disabled.
        const AI_OFF = 1 << 0;
        /// On-device gesture control disabled.
        const GESTURE_OFF = 1 << 1;
        /// Gimbal recentered to the zero position.
        const GIMBAL_CENTER = 1 << 2;
        /// Widest field of view (86°) selected, digital zoom reset.
        const FOV_WIDE = 1 << 3;
        /// Auto exposure explicitly re-asserted.
        const AUTO_EXPOSURE = 1 << 4;
    }
}

/// The steps that must succeed before the camera counts as "under app
/// control": the two that stop the on-device AI from fighting our tracking.
/// FOV/gimbal/exposure failures degrade the picture but not the control
/// story, so they downgrade to log warnings rather than a `Failed` status.
pub const REQUIRED_STEPS: ControlSteps = ControlSteps::AI_OFF.union(ControlSteps::GESTURE_OFF);

/// Where the OBSBOT control worker currently stands. Published by the worker
/// thread and mirrored into [`ObsbotControl::status`] each frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ObsbotStatus {
    /// No OBSBOT device detected (or this platform has no real backend).
    #[default]
    NoDevice,
    /// A device was found; the take-control sequence is running.
    TakingControl,
    /// The camera is under app control: on-device AI and gestures are off.
    InControl {
        /// 14-character device serial number.
        sn: String,
        /// Firmware version string (e.g. `"1.2.3.4"`).
        firmware: String,
        /// Human-readable product name (see [`product_name`]).
        product: String,
    },
    /// The take-control sequence ran but a [`REQUIRED_STEPS`] step failed;
    /// `achieved` holds the steps that did succeed.
    Failed {
        /// Steps that succeeded before/despite the failure.
        achieved: ControlSteps,
    },
    /// A device is present but the operator disabled
    /// [`ObsbotSettings::take_control`]; the camera keeps its own behavior.
    ControlDisabled {
        /// Serial number of the detected-but-untouched device.
        sn: String,
    },
}

/// Commands the Bevy side sends to the worker thread. The manual-control
/// variants exist for future use (choreographed gimbal moves, operator
/// panels); no UI issues them today.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WorkerCommand {
    /// Enable/disable app control at runtime (mirrors the settings toggle).
    /// `false` releases control and restores the camera's own behavior.
    SetTakeControl(bool),
    /// Move the gimbal to an absolute angle (degrees; pitch −90..90,
    /// yaw −180..180 — the device ignores out-of-range values).
    SetGimbalAngle {
        /// Pitch angle in degrees.
        pitch: f32,
        /// Yaw angle in degrees.
        yaw: f32,
    },
    /// Rotate the gimbal at a constant speed (pitch −90..90, pan −180..180);
    /// `0, 0` stops.
    SetGimbalSpeed {
        /// Pitch speed.
        pitch: f64,
        /// Pan speed.
        pan: f64,
    },
    /// Stop any gimbal motion.
    GimbalStop,
    /// Absolute digital zoom, normalized `1.0..=2.0`.
    SetZoom(f32),
    /// Select a field-of-view preset.
    SetFov(FovPreset),
    /// Release control (if held) and exit the worker thread.
    Shutdown,
}

/// Field-of-view presets, mirroring the SDK's `Device::FovType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FovPreset {
    /// 86° — widest view, the factory default and the take-control choice.
    Wide86,
    /// 78° — medium view.
    Medium78,
    /// 65° — narrow view.
    Narrow65,
}

impl FovPreset {
    /// Raw `Device::FovType` value passed across the shim
    /// (`OBSBOT_FOV_*` in `obsbot_shim.h`).
    #[must_use]
    pub fn raw(self) -> i32 {
        match self {
            FovPreset::Wide86 => 0,
            FovPreset::Medium78 => 1,
            FovPreset::Narrow65 => 2,
        }
    }
}

/// Global OBSBOT camera-control settings (not per-sketch), persisted across
/// sessions under the `obsbot` storage key.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "obsbot")]
pub struct ObsbotSettings {
    /// Whether the app should take control of a detected OBSBOT camera
    /// (disable its on-device AI tracking + gesture control, recenter the
    /// gimbal, widest FOV, auto exposure). Applies live: turning it off
    /// releases control and restores the camera's own behavior.
    #[setting(
        default = true,
        ty = Boolean,
        category = User,
        section = "Camera",
        label = "Take control of OBSBOT camera (disable its on-device AI)"
    )]
    #[serde(default = "default_take_control")]
    pub take_control: bool,
}

/// Serde fallback so a settings file saved before this field existed loads
/// with control enabled (the deployment default).
fn default_take_control() -> bool {
    true
}

/// Bevy resource exposing the OBSBOT control status and the manual-control
/// command surface. Inserted by [`ObsbotControlPlugin`].
///
/// Dropping this resource (when the Bevy `App` drops at exit) shuts the
/// worker down, which releases control — re-enabling the camera's AI and
/// gestures — before the process ends. A hard kill skips that; see
/// `docs/runbooks/obsbot.md` for the manual recovery path.
#[derive(Resource, Default)]
pub struct ObsbotControl {
    /// Latest status published by the worker.
    pub status: ObsbotStatus,
    /// Worker handle; `None` until `Startup` (or forever on platforms
    /// without a real backend).
    worker: Option<platform::WorkerHandle>,
}

impl ObsbotControl {
    /// Send a command to the worker thread. Returns `false` when no worker
    /// is running (non-Windows facade, or the worker exited).
    pub fn send_command(&self, cmd: WorkerCommand) -> bool {
        self.worker.as_ref().is_some_and(|w| w.send(cmd))
    }

    /// Move the gimbal to an absolute angle (degrees). Valid only while
    /// [`ObsbotStatus::InControl`]; the device ignores out-of-range values.
    pub fn set_gimbal_angle(&self, pitch_deg: f32, yaw_deg: f32) -> bool {
        self.send_command(WorkerCommand::SetGimbalAngle {
            pitch: pitch_deg,
            yaw: yaw_deg,
        })
    }

    /// Rotate the gimbal at a constant speed; `0, 0` stops.
    pub fn set_gimbal_speed(&self, pitch: f64, pan: f64) -> bool {
        self.send_command(WorkerCommand::SetGimbalSpeed { pitch, pan })
    }

    /// Stop any gimbal motion.
    pub fn gimbal_stop(&self) -> bool {
        self.send_command(WorkerCommand::GimbalStop)
    }

    /// Set absolute digital zoom (normalized `1.0..=2.0`).
    pub fn set_zoom(&self, ratio: f32) -> bool {
        self.send_command(WorkerCommand::SetZoom(ratio))
    }

    /// Select a field-of-view preset.
    pub fn set_fov(&self, fov: FovPreset) -> bool {
        self.send_command(WorkerCommand::SetFov(fov))
    }
}

/// Wires OBSBOT camera control into the Bevy [`App`].
///
/// Signal flow (see the module docs for the thread diagram): `Startup` spawns
/// the platform worker (device discovery + take-control run there, off the
/// Bevy thread); each `PreUpdate` the status drain mirrors worker updates
/// into [`ObsbotControl`] and the settings watcher forwards
/// [`ObsbotSettings::take_control`] changes to the worker. Worker shutdown —
/// which restores the camera to its own behavior — happens when the resource
/// drops with the `App`.
pub struct ObsbotControlPlugin;

impl Plugin for ObsbotControlPlugin {
    fn build(&self, app: &mut App) {
        app.register_sketch_settings::<ObsbotSettings>()
            .init_resource::<ObsbotControl>()
            .add_systems(Startup, start_worker)
            .add_systems(PreUpdate, (drain_worker_status, apply_take_control_setting));
    }
}

/// `Startup`: spawn the platform worker thread (real on Windows, `None`
/// elsewhere) with the persisted take-control preference.
fn start_worker(mut ctl: ResMut<'_, ObsbotControl>, settings: Res<'_, ObsbotSettings>) {
    ctl.worker = platform::spawn_worker(settings.take_control);
    if ctl.worker.is_none() {
        info!("OBSBOT camera control: no backend on this platform (no-op facade)");
    }
}

/// `PreUpdate`: drain worker status updates into the resource. Cheap no-op
/// when the channel is empty (a `try_recv` per frame; no allocation).
fn drain_worker_status(mut ctl: ResMut<'_, ObsbotControl>) {
    let mut latest = None;
    if let Some(worker) = ctl.worker.as_ref() {
        while let Some(status) = worker.try_recv_status() {
            latest = Some(status);
        }
    }
    if let Some(status) = latest {
        if status != ctl.status {
            // The operator's at-a-gig confirmation line (per-step detail is
            // logged by the worker as each step runs).
            info!("OBSBOT control status: {status:?}");
        }
        ctl.status = status;
    }
}

/// `PreUpdate`: forward runtime changes of the take-control toggle to the
/// worker. Skips the insertion tick — the startup value is passed to
/// [`platform::spawn_worker`] directly.
fn apply_take_control_setting(settings: Res<'_, ObsbotSettings>, ctl: Res<'_, ObsbotControl>) {
    if settings.is_changed() && !settings.is_added() {
        ctl.send_command(WorkerCommand::SetTakeControl(settings.take_control));
    }
}

/// Decide the status a finished take-control run maps to: `InControl` when
/// every [`REQUIRED_STEPS`] bit is present, `Failed` (carrying the achieved
/// bits) otherwise. Pure so the policy is unit-testable without hardware.
#[must_use]
pub fn take_control_outcome(
    achieved: ControlSteps,
    sn: &str,
    firmware: &str,
    product: &str,
) -> ObsbotStatus {
    if achieved.contains(REQUIRED_STEPS) {
        ObsbotStatus::InControl {
            sn: sn.to_owned(),
            firmware: firmware.to_owned(),
            product: product.to_owned(),
        }
    } else {
        ObsbotStatus::Failed { achieved }
    }
}

/// Human-readable product name for a raw SDK `ObsbotProductType` value
/// (`vendor/libdev/include/dev/dev.hpp`, `enum ObsbotProductType`).
#[must_use]
pub fn product_name(raw: i32) -> &'static str {
    match raw {
        0 => "Tiny",
        1 => "Tiny 4K",
        2 => "Tiny 2",
        3 => "Tiny 2 Lite",
        4 => "Tail Air",
        5 => "Meet",
        6 => "Meet 4K",
        7 => "Me",
        8 => "UVC-to-HDMI Box",
        9 => "NDI Box",
        10 => "Meet 2",
        11 => "Tail 2",
        12 => "Tiny SE",
        13 => "Meet SE",
        16 => "Tail 2S",
        18 => "Tiny 3",
        19 => "Tiny 3 Lite",
        _ => "Unknown OBSBOT",
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;

    /// All five steps succeeding is in control.
    #[test]
    fn outcome_full_success_is_in_control() {
        let status = take_control_outcome(
            ControlSteps::all(),
            "SN12345678901X",
            "1.2.3.4",
            "Tiny 2 Lite",
        );
        assert_eq!(
            status,
            ObsbotStatus::InControl {
                sn: "SN12345678901X".to_owned(),
                firmware: "1.2.3.4".to_owned(),
                product: "Tiny 2 Lite".to_owned(),
            }
        );
    }

    /// The AI/gesture bits are what "control" means; cosmetic steps (FOV,
    /// exposure, recenter) failing must not demote the status to Failed.
    #[test]
    fn outcome_required_steps_alone_is_in_control() {
        let status = take_control_outcome(REQUIRED_STEPS, "sn", "fw", "p");
        assert!(matches!(status, ObsbotStatus::InControl { .. }));
    }

    /// A run where the AI stayed on is a failure no matter what else worked —
    /// the camera would still fight the app's tracking.
    #[test]
    fn outcome_missing_ai_off_is_failed() {
        let achieved = ControlSteps::all().difference(ControlSteps::AI_OFF);
        let status = take_control_outcome(achieved, "sn", "fw", "p");
        assert_eq!(status, ObsbotStatus::Failed { achieved });
    }

    /// Gestures left on likewise fail the run (a visitor's raised palm would
    /// still zoom the camera).
    #[test]
    fn outcome_missing_gesture_off_is_failed() {
        let achieved = ControlSteps::AI_OFF | ControlSteps::FOV_WIDE;
        let status = take_control_outcome(achieved, "sn", "fw", "p");
        assert_eq!(status, ObsbotStatus::Failed { achieved });
    }

    /// The Rust bit values must mirror `OBSBOT_STEP_*` in
    /// `vendor/libdev/shim/obsbot_shim.h` — this pins them so a reorder on
    /// either side fails a test instead of silently mislabeling steps.
    #[test]
    fn control_steps_mirror_shim_constants() {
        assert_eq!(ControlSteps::AI_OFF.bits(), 1);
        assert_eq!(ControlSteps::GESTURE_OFF.bits(), 1 << 1);
        assert_eq!(ControlSteps::GIMBAL_CENTER.bits(), 1 << 2);
        assert_eq!(ControlSteps::FOV_WIDE.bits(), 1 << 3);
        assert_eq!(ControlSteps::AUTO_EXPOSURE.bits(), 1 << 4);
        // Unknown future bits from a newer shim must not panic the decoder.
        assert_eq!(
            ControlSteps::from_bits_truncate(0xFFFF_FFFF),
            ControlSteps::all()
        );
    }

    /// FOV raw values must mirror `Device::FovType` / `OBSBOT_FOV_*`.
    #[test]
    fn fov_presets_mirror_sdk_values() {
        assert_eq!(FovPreset::Wide86.raw(), 0);
        assert_eq!(FovPreset::Medium78.raw(), 1);
        assert_eq!(FovPreset::Narrow65.raw(), 2);
    }

    /// The deployment target maps to its marketing name.
    #[test]
    fn product_name_knows_the_deployed_camera() {
        assert_eq!(product_name(3), "Tiny 2 Lite");
        assert_eq!(product_name(0), "Tiny");
        assert_eq!(product_name(-1), "Unknown OBSBOT");
        assert_eq!(product_name(17), "Unknown OBSBOT"); // gap in the SDK enum
    }

    /// Take-control defaults ON — the whole point of the integration — and a
    /// pre-feature settings file must load that default, not error or land
    /// on `false`.
    #[test]
    fn take_control_defaults_on() {
        assert!(ObsbotSettings::default().take_control);
        let parsed: ObsbotSettings = toml::from_str("").expect("empty settings file loads");
        assert!(parsed.take_control);
    }

    /// Round-trip the toggle through the persisted TOML form.
    #[test]
    fn settings_round_trip_through_toml() {
        let settings = ObsbotSettings {
            take_control: false,
        };
        let text = toml::to_string(&settings).expect("serialize");
        let back: ObsbotSettings = toml::from_str(&text).expect("parse back");
        assert!(!back.take_control);
    }

    /// With no worker (the non-Windows facade, or pre-Startup), commands are
    /// swallowed and report `false` rather than panicking.
    #[test]
    fn commands_without_worker_report_false() {
        let ctl = ObsbotControl::default();
        assert_eq!(ctl.status, ObsbotStatus::NoDevice);
        assert!(!ctl.set_gimbal_angle(0.0, 0.0));
        assert!(!ctl.set_gimbal_speed(0.0, 0.0));
        assert!(!ctl.gimbal_stop());
        assert!(!ctl.set_zoom(1.0));
        assert!(!ctl.set_fov(FovPreset::Wide86));
    }

    /// The facade contract on platforms without a real backend: no worker,
    /// ever — the status stays `NoDevice` and nothing links or loads.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn facade_is_noop_off_windows() {
        assert!(platform::spawn_worker(true).is_none());
    }
}
