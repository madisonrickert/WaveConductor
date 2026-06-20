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

pub mod action_map;
pub mod actions;
pub mod idle;
pub mod nav;
pub mod reload;
pub mod screensaver;
pub mod state;
pub mod thermal;

pub use idle::RegisterIdleVetoExt;
pub use reload::SketchReloadState;
pub use thermal::{ThermalSource, ThermalState, ThermalTier};

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
            // In-house action input (replaces leafwing; see action_map). Runs
            // alongside leafwing during migration.
            .add_message::<action_map::ActionInput>()
            .insert_resource(action_map::default_bindings())
            .add_systems(
                PreUpdate,
                action_map::emit_action_input
                    .run_if(crate::settings::input_capture::egui_not_capturing_keyboard)
                    .after(bevy::input::InputSystems),
            )
            // Idle / interaction tracking
            .init_resource::<idle::InteractionTimer>()
            .init_resource::<idle::IdleVetoes>()
            // Sketch reload fade-overlay state (cross-sketch; not Line-specific).
            .init_resource::<reload::SketchReloadState>()
            // Register HandTrackingFrame message so reset_on_interaction can
            // read it. If HandTrackingPlugin is also present, Bevy deduplicates
            // the registration; registering here ensures lifecycle tests that do
            // not add HandTrackingPlugin still compile and run.
            .add_message::<crate::input::state::HandTrackingFrame>()
            // Systems
            .add_systems(
                Update,
                (
                    // Hotkeys must not fire while an egui text field has
                    // keyboard focus (typing "2" into a dev-panel field would
                    // otherwise switch sketches). The condition fails open
                    // when the capture resource is absent (harnesses without
                    // SettingsPlugin/EguiPlugin keep their hotkeys).
                    nav::handle_navigation_actions
                        .run_if(crate::settings::input_capture::egui_not_capturing_keyboard),
                    idle::reset_on_interaction,
                    // Shift+S screensaver skip: MUST sit between
                    // reset_on_interaction (whose keyboard marks it overrides
                    // while armed) and advance_activity (which consumes the
                    // rewound timer the same frame). Deliberately NOT behind
                    // the egui run_if — it handles keyboard capture itself,
                    // because a skipped frame would freeze its armed state
                    // (see the system docs).
                    idle::skip_to_screensaver,
                    idle::advance_activity,
                    reload::drive_reload_state,
                )
                    .chain(),
            );

        // Adaptive thermal signal (Plan 11.8, Seam 1). Spawns the background
        // temperature sampler and maintains `Res<ThermalState>`. Built before
        // the screensaver framework so the latter can read the tier.
        app.add_plugins(thermal::ThermalMonitorPlugin);

        // Screensaver / attract-mode framework (Plan 11.8, Seam 2). Owns the
        // `in_screensaver` run-condition, the `ScreensaverSettings` resource,
        // the instruction overlay, and the per-tier present-rate throttle.
        app.add_plugins(screensaver::ScreensaverPlugin);
    }
}
