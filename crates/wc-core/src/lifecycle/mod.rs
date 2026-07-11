//! Lifecycle subsystem: app-level navigation states, sketch activity sub-states,
//! idle detection, screensaver overlay, and the keyboard-action input map that
//! drives navigation.
//!
//! ## Data flow
//!
//! 1. User presses a key bound by [`actions::WaveConductorAction`].
//! 2. `action_map::emit_action_input` reads `ButtonInput<KeyCode>` and emits `ActionInput` messages.
//! 3. [`nav::handle_navigation_actions`] reads those `ActionInput` messages and
//!    transitions [`state::AppState`] via `NextState<AppState>`.
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
pub mod window_resize;

pub use idle::RegisterIdleVetoExt;
pub use reload::SketchReloadState;
pub use thermal::{ThermalSource, ThermalState, ThermalTier};

use bevy::prelude::*;

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
            // In-house action input (see action_map).
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
            // Plan 02: debounced window-resize settling signal (see
            // `window_resize`). Registered here so it exists even for lifecycle
            // tests that do not add a sketch plugin.
            .add_message::<window_resize::WindowResizeSettled>()
            // The two upstream window messages `debounce_window_resize` reads.
            // `WindowPlugin` registers both in the real app, and `add_message` is
            // guarded by `if !contains_resource::<Messages<T>>()`, so this is a
            // no-op there. It matters for the many headless harnesses that add
            // `LifecyclePlugin` without `WindowPlugin`: without it, the always-on
            // `debounce_window_resize` fails `MessageReader` param validation on
            // its first run and panics the whole schedule. Same reason
            // `HandTrackingFrame` is registered above.
            .add_message::<bevy::window::WindowResized>()
            .add_message::<bevy::window::WindowScaleFactorChanged>()
            // Systems
            .add_systems(
                Update,
                (
                    // The egui keyboard-capture gate now lives on the
                    // PreUpdate producer (`emit_action_input`), so nav reads
                    // only actions that already passed the gate — no redundant
                    // run_if needed here.
                    nav::handle_navigation_actions,
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

        // Plan 02: debounce `WindowResized` / `WindowScaleFactorChanged` into a
        // single `WindowResizeSettled` (250 ms after the last event). Always-on
        // message listener — it must observe resize events in every state
        // (including `Home`) and no-ops cheaply on any frame with no event, the
        // same always-on category as `reload::drive_reload_state`.
        app.add_systems(Update, window_resize::debounce_window_resize);

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
