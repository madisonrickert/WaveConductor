//! Translates [`WaveConductorAction`] presses into [`AppState`] transitions and
//! window-level effects (fullscreen toggle).
//!
//! The fullscreen toggle only flips `DisplaySettings::start_fullscreen`
//! (`crate::settings::DisplaySettings`); it does not write `Window` directly.
//! `crate::lifecycle::display::apply_display_mode` is the sole writer of
//! `Window::mode` and `CursorOptions::visible`, ordered to run immediately
//! after this system each frame.

use bevy::prelude::*;

use super::action_map::{ActionInput, ActionPhase};
use super::actions::WaveConductorAction;
use super::state::AppState;

/// Reads `MessageReader<ActionInput>` and translates `Pressed` edges into
/// navigation transitions and window-level effects (fullscreen toggle).
///
/// Drains all of this frame's `Pressed` edges first, then applies a single
/// transition by fixed precedence (sketch-select, Home, Next, Prev) so two
/// select keys landing the same frame resolve deterministically — matching the
/// previous else-if ordering.
pub(crate) fn handle_navigation_actions(
    mut actions: MessageReader<'_, '_, ActionInput>,
    current: Res<'_, State<AppState>>,
    mut next: ResMut<'_, NextState<AppState>>,
    mut display_settings: ResMut<'_, crate::settings::DisplaySettings>,
) {
    use WaveConductorAction as A;

    let mut pressed_select: Option<AppState> = None;
    let mut home = false;
    let mut go_next = false;
    let mut go_prev = false;
    let mut fullscreen = false;

    for input in actions.read() {
        if input.phase != ActionPhase::Pressed {
            continue;
        }
        match input.action {
            A::SelectLine => pressed_select = pressed_select.or(Some(AppState::Line)),
            A::SelectFlame => pressed_select = pressed_select.or(Some(AppState::Flame)),
            A::SelectDots => pressed_select = pressed_select.or(Some(AppState::Dots)),
            A::SelectCymatics => pressed_select = pressed_select.or(Some(AppState::Cymatics)),
            A::NavigateHome => home = true,
            A::NavigateNext => go_next = true,
            A::NavigatePrev => go_prev = true,
            A::ToggleFullscreen => fullscreen = true,
            // ToggleVolume → audio::nav; ToggleDevPanel → settings::panel_dev;
            // StartScreensaver → idle::skip_to_screensaver.
            _ => {}
        }
    }

    let transition_to = pressed_select
        .or_else(|| home.then_some(AppState::Home))
        .or_else(|| go_next.then(|| current.get().next_sketch()))
        .or_else(|| go_prev.then(|| current.get().prev_sketch()));

    if let Some(target) = transition_to {
        if *current.get() != target {
            tracing::info!(?target, "navigate");
            next.set(target);
        }
    }

    if fullscreen {
        // Only the flag flips here. `crate::lifecycle::display::apply_display_mode`
        // is the single writer of `Window::mode` / `CursorOptions::visible`;
        // it is ordered `.after(handle_navigation_actions)` in `Update`, so
        // this toggle takes effect the same frame, and it re-derives the
        // target from DisplaySettings plus the live monitor set rather than
        // this handler guessing at a MonitorSelection directly.
        display_settings.start_fullscreen = !display_settings.start_fullscreen;
        tracing::info!(
            start_fullscreen = display_settings.start_fullscreen,
            "toggle fullscreen"
        );
    }

    // ToggleVolume is handled by crate::audio::nav::handle_volume_toggle (Plan 4).
    // ToggleDevPanel is handled by crate::settings::panel_dev::handle_dev_panel_toggle (Plan 5).
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;

    use super::*;
    use crate::lifecycle::LifecyclePlugin;
    use crate::settings::DisplaySettings;

    /// Build the minimal headless app needed to exercise
    /// `handle_navigation_actions` end-to-end through `LifecyclePlugin`
    /// (which wires up `DisplayPlugin` and, with it, `DisplaySettings`).
    ///
    /// Mirrors `crates/wc-core/tests/common/app.rs::lifecycle_test_app`
    /// (`MinimalPlugins` + `InputPlugin` + `StatesPlugin` + `LifecyclePlugin`,
    /// plus a bare `Window` entity). That helper lives in the external
    /// `tests/` integration-test crate and cannot see `DisplaySettings` —
    /// it is `pub(crate)` by design (see the module doc on
    /// `crate::lifecycle::display::apply_display_mode` for why: an
    /// unresolvable saved monitor name must never be silently rewritten, and
    /// keeping the type out of the public surface is part of enforcing that).
    /// This copy lives beside the code it tests so the assertions below can
    /// read the resource directly.
    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::input::InputPlugin);
        app.add_plugins(StatesPlugin);
        app.add_plugins(LifecyclePlugin);
        app.world_mut().spawn(Window::default());
        app
    }

    /// Regression test for the dead-F11 bug this task fixes.
    ///
    /// Before this change, `handle_navigation_actions` wrote `Window::mode`
    /// directly, and `apply_display_mode` — ordered right after it in
    /// `Update` — reverted that write from the unchanged `DisplaySettings`
    /// on the very same frame: F11 pressed and released with no observable
    /// effect (confirmed empirically against the pre-fix code). Nothing in
    /// the test suite exercised F11 before this test, which is exactly why
    /// that regression shipped invisibly.
    ///
    /// Injects `ActionInput { action: ToggleFullscreen, phase: Pressed }`
    /// directly into the message queue (bypassing physical key simulation),
    /// matching the `direct_action_input_rewinds_timer` pattern in
    /// `crates/wc-core/tests/lifecycle.rs`, and asserts
    /// `DisplaySettings::start_fullscreen` flips on the very next `update()`
    /// — the observable effect `apply_display_mode` then re-derives
    /// `Window::mode` from — then flips back on a second press.
    #[test]
    fn toggle_fullscreen_flips_display_settings_start_fullscreen() {
        let mut app = test_app();
        app.update();

        let initial = app.world().resource::<DisplaySettings>().start_fullscreen;

        app.world_mut().write_message(ActionInput {
            action: WaveConductorAction::ToggleFullscreen,
            phase: ActionPhase::Pressed,
        });
        app.update();

        let after_first = app.world().resource::<DisplaySettings>().start_fullscreen;
        assert_ne!(
            after_first, initial,
            "F11 (ToggleFullscreen) must flip DisplaySettings::start_fullscreen"
        );

        app.world_mut().write_message(ActionInput {
            action: WaveConductorAction::ToggleFullscreen,
            phase: ActionPhase::Pressed,
        });
        app.update();

        let after_second = app.world().resource::<DisplaySettings>().start_fullscreen;
        assert_eq!(
            after_second, initial,
            "a second F11 press must flip DisplaySettings::start_fullscreen back"
        );
    }
}
