//! Hand-tracking input subsystem.
//!
//! Models the data and event flow for hand-tracking input — the only input
//! modality Bevy does not natively know about. Mouse, keyboard, and touch are
//! consumed directly via Bevy's `Res<ButtonInput<…>>`, `Res<Touches>`,
//! `Res<AccumulatedMouseMotion>`, etc.
//!
//! ## Architecture
//!
//! [`HandTrackingPlugin`] is modeled exactly on Bevy's built-in `InputPlugin`:
//! it initializes resources, registers messages, and adds systems that run in
//! `PreUpdate` under the `InputSystems` set so that downstream `Update` systems
//! see fresh state.
//!
//! ```text
//! Provider::poll → Messages<HandTrackingFrame> → fuse_hand_frames
//!                                                ↓
//!                                                sync_hand_entities (TrackedHand ECS)
//!                                                ↓
//!                                                mirror_state_resource
//!                                                ↓
//!                                                Res<HandTrackingState>
//!                                                Res<ButtonInput<HandButton>>
//!                                                ↓
//!                                                systems::detect_gestures
//!                                                ↓
//!                                                Messages<HandGestureEvent>
//! ```
//!
//! ## What sketches consume
//!
//! - [`state::HandTrackingState`] (`Res<…>`) — continuous per-hand snapshot,
//!   shape mirrors `Res<Touches>`.
//! - `Res<ButtonInput<HandButton>>` — discrete press state, idiom identical to
//!   `Res<ButtonInput<MouseButton>>`.
//! - `Messages<HandGestureEvent>` — derived discrete moments (pinch-down,
//!   pinch-up, grab-down, grab-up).
//! - `Messages<HandTrackingFrame>` — raw provider frames, for systems that
//!   want them (analytics, recording, lifecycle interaction reset).
//!
//! ## What sketches NEVER touch
//!
//! - [`provider::HandTrackingProvider`] — the strategy trait is an internal
//!   implementation detail. App startup picks one provider; sketches read
//!   the resources / messages above.

pub mod activation;
/// Webcam body tracking (BlazePose person detector + landmark/segmentation
/// worker), consumed by the Radiance sketch.
#[cfg(feature = "body-tracking-mediapipe")]
pub mod body;
pub mod button;
/// Live camera preview for the settings dock: a low-rate downscaled tap on
/// the tracking workers' camera frames plus the toggle that gates it. Gated
/// like [`capture`], whose `FrameSource` it decorates.
#[cfg(any(
    feature = "hand-tracking-mediapipe",
    feature = "body-tracking-mediapipe"
))]
pub mod camera_preview;
/// Shared webcam frame capture: the `FrameSource` trait, the platform
/// backends (AVFoundation on macOS, nokhwa elsewhere), and the test
/// `MockFrameSource`. Consumed by the MediaPipe hand provider and by the
/// body-tracking worker, so it lives beside — not inside — either.
#[cfg(any(
    feature = "hand-tracking-mediapipe",
    feature = "body-tracking-mediapipe"
))]
pub mod capture;
/// Pure engagement-scoring math ("is this a player's hand?"), shared by the
/// `MediaPipe` worker's bystander-eviction logic and Line's focal-hand pick.
pub mod engagement;
pub mod entity;
pub mod gesture;
pub mod hand;
pub mod idle_pause;
/// OBSBOT camera control (disable on-device AI/gestures, gimbal + FOV +
/// exposure) over the vendored libdev SDK. Real device IO is Windows-only;
/// other platforms compile a documented no-op facade.
#[cfg(feature = "obsbot-camera-control")]
pub mod obsbot;
/// Shared ONNX inference (tensor types + the ort backend with its CoreML
/// per-model cache), consumed by the MediaPipe hand provider and the
/// body-tracking pipeline.
#[cfg(any(
    feature = "hand-tracking-mediapipe",
    feature = "body-tracking-mediapipe"
))]
pub mod onnx;
pub mod pointer;
pub mod projection;
pub mod provider;
pub mod providers;
pub mod selection;
pub mod state;
pub mod synthetic;
pub mod systems;
pub mod wedge;

use bevy::input::InputSystems;
use bevy::prelude::*;

use self::button::HandButton;
use self::gesture::HandGestureEvent;
use self::pointer::{pointer_merge_system, PointerState};
use self::provider::ProviderRegistry;
use self::state::{HandTrackingFrame, HandTrackingState};
use crate::settings::RegisterSketchSettingsExt;

/// Single plugin that wires the hand-tracking subsystem into the Bevy [`App`].
///
/// Models Bevy's built-in `InputPlugin`. Registered by [`crate::CorePlugin`].
pub struct HandTrackingPlugin;

impl Plugin for HandTrackingPlugin {
    fn build(&self, app: &mut App) {
        app
            // Resources — populated by systems below
            .init_resource::<HandTrackingState>()
            .init_resource::<ButtonInput<HandButton>>()
            .init_resource::<PointerState>()
            // The provider registry is populated by the binary at startup.
            .init_resource::<ProviderRegistry>()
            // Coarse activation cue for the settings panel; the binary's
            // provider systems publish it (defaults to `Inactive` so the panel
            // always has a value, including feature-off / headless builds).
            .init_resource::<self::activation::HandTrackingActivation>()
            // Global hand-tracking settings (e.g. Leap background policy).
            // Registered here so the setting follows the input subsystem's
            // lifecycle rather than being coupled to SettingsPlugin.
            .register_sketch_settings::<crate::settings::HandTrackingSettings>()
            // Messages
            .add_message::<HandTrackingFrame>()
            .add_message::<state::FusedHandFrame>()
            .add_message::<HandGestureEvent>()
            .add_message::<systems::LeapWedgeChanged>()
            // `pointer_merge_system` reads `CursorMoved` (Plan 8 Phase 0
            // closed the test-fidelity gap by wiring it into the mouse-source
            // path). In production `WindowPlugin` registers this message;
            // re-register defensively so harnesses that bring this plugin in
            // without `WindowPlugin` (the wc-core integration tests) don't
            // trip Bevy's "message not initialized" runtime validator.
            // `add_message` is idempotent when the message is already registered.
            .add_message::<bevy::window::CursorMoved>()
            // Reflection registrations for tracked hand entities and components.
            .register_type::<entity::TrackedHand>()
            .register_type::<entity::HandId>()
            .register_type::<entity::PalmPosition>()
            .register_type::<entity::PalmVelocity>()
            .register_type::<entity::PinchStrength>()
            .register_type::<entity::GrabStrength>()
            // PreUpdate systems, chained, under the same InputSystems set Bevy
            // uses for its own input systems. This means downstream Update
            // systems can use `.after(InputSystems)` to see fresh state.
            .add_systems(
                PreUpdate,
                (
                    systems::poll_all_providers,
                    // runs after poll so it reads this tick's fresh provider status
                    systems::surface_leap_wedge,
                    systems::fuse_hand_frames,
                    systems::sync_hand_entities,
                    systems::mirror_state_resource,
                    systems::detect_gestures,
                    pointer_merge_system,
                )
                    .chain()
                    .in_set(InputSystems),
            );

        #[cfg(feature = "hand-tracking-gestures")]
        {
            use crate::lifecycle::screensaver::ScreensaverActive;
            use crate::lifecycle::state::SketchActivity;

            // Defensive (independent of the duty cycle): ensure the tracking
            // service is un-paused whenever a visitor returns, even if a prior
            // run or an aborted duty cycle left it paused.
            app.add_systems(
                OnEnter(SketchActivity::Active),
                self::providers::leap_native::resume_leap_on_active,
            );

            // Experimental deep-idle duty cycle — OFF BY DEFAULT. It pauses the
            // Leap service during the screensaver to shed CPU/heat, briefly
            // un-pausing to sample for a returning hand. Under GPU contention it
            // can wedge the Ultraleap service, and on macOS a wedge needs a
            // manual USB replug to clear (see
            // docs/superpowers/specs/2026-06-03-leap-service-recovery-design.md).
            // Opt in with `WC_LEAP_DUTY_CYCLE=1` only to reproduce or measure it.
            if duty_cycle_enabled(std::env::var(DUTY_CYCLE_ENV).ok().as_deref()) {
                app.init_resource::<self::idle_pause::LeapIdlePause>();
                app.add_systems(
                    OnEnter(SketchActivity::Screensaver),
                    self::providers::leap_native::enter_leap_idle_pause,
                );
                app.add_systems(
                    Update,
                    self::providers::leap_native::drive_leap_idle_pause
                        .run_if(resource_exists::<ScreensaverActive>),
                );
            }

            // Propagate runtime leap_background changes to the live connection.
            app.add_systems(
                PreUpdate,
                self::providers::leap_native::apply_leap_background_setting
                    .after(systems::poll_all_providers)
                    .in_set(InputSystems),
            );
        }

        // Propagate runtime hand-tuning changes (grab deadzone, smoothing) to the
        // live MediaPipe provider, and mirror SketchActivity into its idle
        // inference throttle (Idle/Screensaver → 4 Hz cap; an unconditional
        // per-frame atomic store, so a provider rebuilt by the runtime selector
        // picks the current activity state up on the next frame). The mirror
        // reads the State<SketchActivity> applied last frame — one frame of
        // lag, absorbed by the throttle's documented one-frame wake race.
        // Separate feature from the Leap gestures block.
        #[cfg(feature = "hand-tracking-mediapipe")]
        app.add_systems(
            PreUpdate,
            (
                self::providers::mediapipe::apply_mediapipe_tuning_settings,
                self::providers::mediapipe::apply_mediapipe_idle_throttle,
            )
                .after(systems::poll_all_providers)
                .in_set(InputSystems),
        );
    }
}

/// Env var that opts into the experimental deep-idle Leap duty cycle. Unset — or
/// any value other than `1`/`true` — leaves the duty cycle **off**.
#[cfg(feature = "hand-tracking-gestures")]
const DUTY_CYCLE_ENV: &str = "WC_LEAP_DUTY_CYCLE";

/// Whether the experimental deep-idle Leap duty cycle is enabled, given the
/// value of [`DUTY_CYCLE_ENV`] (pass `std::env::var(DUTY_CYCLE_ENV).ok().as_deref()`).
///
/// **Off by default.** The duty cycle pauses/resumes the Ultraleap service ~2×/s
/// during the screensaver to shed CPU/heat, but under GPU contention that can
/// wedge the service, and on macOS a wedge needs a manual USB replug to clear
/// (see `docs/superpowers/specs/2026-06-03-leap-service-recovery-design.md`).
/// Opt in with `WC_LEAP_DUTY_CYCLE=1` (or `true`) only to reproduce or measure it.
#[cfg(feature = "hand-tracking-gestures")]
fn duty_cycle_enabled(var: Option<&str>) -> bool {
    matches!(var, Some("1" | "true"))
}

#[cfg(all(test, feature = "hand-tracking-gestures"))]
mod tests {
    use super::duty_cycle_enabled;

    #[test]
    fn duty_cycle_is_off_unless_explicitly_opted_in() {
        // Off by default and for any unrecognized value (footgun guard).
        assert!(!duty_cycle_enabled(None));
        assert!(!duty_cycle_enabled(Some("")));
        assert!(!duty_cycle_enabled(Some("0")));
        assert!(!duty_cycle_enabled(Some("false")));
        assert!(!duty_cycle_enabled(Some("yes")));
        // On only for the two documented opt-in values.
        assert!(duty_cycle_enabled(Some("1")));
        assert!(duty_cycle_enabled(Some("true")));
    }
}
