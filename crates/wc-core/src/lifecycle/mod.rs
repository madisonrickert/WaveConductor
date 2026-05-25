//! Lifecycle subsystem: app-level navigation states, sketch activity sub-states,
//! idle detection, screensaver overlay, and the keyboard-action input map that
//! drives navigation.
//!
//! ## Data flow
//!
//! 1. User presses a key bound by [`actions::WaveConductorAction`].
//! 2. `leafwing-input-manager` updates `Res<ActionState<WaveConductorAction>>`.
//! 3. [`nav::handle_navigation_actions`] reads the action state and transitions
//!    [`state::AppState`] via `NextState<AppState>`.
//! 4. Any interaction (mouse, keyboard, future hand-tracking) resets
//!    [`idle::InteractionTimer`].
//! 5. The idle system advances [`state::SketchActivity`] through Active → Idle →
//!    Screensaver as the timer crosses configured thresholds.
//!
//! Sketches (registered in `wc-sketches`) gate their update systems on
//! `in_state(SketchActivity::Active)` so they stop simulating when idle.

pub mod actions;
pub mod idle;
pub mod nav;
pub mod screensaver;
pub mod state;

pub use idle::RegisterIdleVetoExt;

use bevy::prelude::*;
use leafwing_input_manager::prelude::*;

/// Single plugin that wires every lifecycle subsystem into the Bevy [`App`].
///
/// Registered by [`crate::CorePlugin`].
pub struct LifecyclePlugin;

impl Plugin for LifecyclePlugin {
    fn build(&self, app: &mut App) {
        app
            // States machine
            .init_state::<state::AppState>()
            .add_sub_state::<state::SketchActivity>()
            // Input action mapping (leafwing)
            .add_plugins(InputManagerPlugin::<actions::WaveConductorAction>::default())
            .insert_resource(actions::default_input_map())
            .init_resource::<ActionState<actions::WaveConductorAction>>()
            // Idle / interaction tracking
            .init_resource::<idle::InteractionTimer>()
            .init_resource::<idle::IdleVetoes>()
            // Register HandTrackingFrame message so reset_on_interaction can
            // read it. If HandTrackingPlugin is also present, Bevy deduplicates
            // the registration; registering here ensures lifecycle tests that do
            // not add HandTrackingPlugin still compile and run.
            .add_message::<crate::input::state::HandTrackingFrame>()
            // Systems
            .add_systems(
                Update,
                (
                    nav::handle_navigation_actions,
                    idle::reset_on_interaction,
                    idle::advance_activity,
                )
                    .chain(),
            )
            .add_systems(OnEnter(state::SketchActivity::Screensaver), screensaver::show)
            .add_systems(OnExit(state::SketchActivity::Screensaver), screensaver::hide);
    }
}
