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
//! Provider::poll → Messages<HandTrackingFrame> → systems::update_hand_tracking_state
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

pub mod button;
pub mod entity;
pub mod gesture;
pub mod hand;
pub mod pointer;
pub mod projection;
pub mod provider;
pub mod providers;
pub mod state;
pub mod systems;

use bevy::input::InputSystems;
use bevy::prelude::*;

use self::button::HandButton;
use self::gesture::HandGestureEvent;
use self::pointer::{pointer_merge_system, PointerState};
use self::provider::ProviderRegistry;
use self::state::{HandTrackingFrame, HandTrackingState};

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
            // Messages
            .add_message::<HandTrackingFrame>()
            .add_message::<HandGestureEvent>()
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
                    systems::update_hand_tracking_state,
                    systems::detect_gestures,
                    pointer_merge_system,
                )
                    .chain()
                    .in_set(InputSystems),
            );
    }
}
