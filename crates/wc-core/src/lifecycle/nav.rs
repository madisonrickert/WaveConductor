//! Translates [`WaveConductorAction`] presses into [`AppState`] transitions and
//! window-level effects (fullscreen toggle).
//!
//! The fullscreen toggle flips the **session-only** `FullscreenOverride`
//! (`crate::settings::FullscreenOverride`); it writes neither `Window` nor the
//! persisted `DisplaySettings`. `crate::lifecycle::display::apply_display_mode`
//! is the sole writer of `Window::mode` and `CursorOptions::visible`, ordered to
//! run immediately after this system each frame.
//!
//! Why the override and not the setting: this app runs unattended on a TV.
//! `start_fullscreen` is autosaved, so writing it from F11 would mean a
//! passer-by's keypress drops the kiosk to a small window on a black screen —
//! *and that survives a reboot*. As a session override it dies with the process,
//! so a power cycle always restores the configured state. The settings panel's
//! checkbox still writes and persists `start_fullscreen`: a deliberate operator
//! choice is not a stray keypress.

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
    display_settings: Res<'_, crate::settings::DisplaySettings>,
    mut fullscreen_override: ResMut<'_, crate::settings::FullscreenOverride>,
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
        // Only the session override flips here — never the persisted
        // `DisplaySettings` (see the module doc: a stray F11 must not outlive a
        // power cycle). `crate::lifecycle::display::apply_display_mode` is the
        // single writer of `Window::mode` / `CursorOptions::visible`; it is
        // ordered `.after(handle_navigation_actions)` in `Update`, so this
        // toggle takes effect the same frame, and it re-derives the target from
        // the override + DisplaySettings + the live monitor set rather than this
        // handler guessing at a MonitorSelection directly.
        //
        // Toggling relative to `effective_fullscreen` (not to the persisted
        // flag) is what makes repeated presses alternate: after the first press
        // the override, not the setting, is what is on screen.
        let toggled = !fullscreen_override.effective_fullscreen(&display_settings);
        fullscreen_override.0 = Some(toggled);
        tracing::info!(
            fullscreen = toggled,
            persisted_start_fullscreen = display_settings.start_fullscreen,
            "toggle fullscreen (session override; not persisted)"
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
    use crate::settings::{DisplaySettings, FullscreenOverride};

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

    /// Inject one `ActionInput { action, phase: Pressed }` and run a frame.
    ///
    /// Bypasses physical key simulation, matching the
    /// `direct_action_input_rewinds_timer` pattern in
    /// `crates/wc-core/tests/lifecycle.rs`.
    fn press(app: &mut App, action: WaveConductorAction) {
        app.world_mut().write_message(ActionInput {
            action,
            phase: ActionPhase::Pressed,
        });
        app.update();
    }

    /// The fullscreen state actually on screen: the session override if set,
    /// else the persisted setting. Exactly what `apply_display_mode` derives
    /// `Window::mode` from.
    fn effective_fullscreen(app: &App) -> bool {
        app.world()
            .resource::<FullscreenOverride>()
            .effective_fullscreen(app.world().resource::<DisplaySettings>())
    }

    /// Regression test for the dead-F11 bug (F11 used to write `Window::mode`
    /// directly, and `apply_display_mode` — ordered right after it in `Update` —
    /// reverted that write from the unchanged `DisplaySettings` on the very same
    /// frame: F11 pressed and released with no observable effect). Nothing in the
    /// test suite exercised F11 before this test, which is why that regression
    /// shipped invisibly.
    ///
    /// It now also pins the *second* F11 decision: the toggle flips the
    /// session-only `FullscreenOverride`, and the effective mode — what
    /// `apply_display_mode` re-derives `Window::mode` from — alternates on each
    /// press. Asserting on `effective_fullscreen` rather than on the raw override
    /// keeps this a real behavioural test: a regression that made F11 write
    /// `Some(x)` unconditionally, or toggle against the stale persisted flag
    /// instead of what is on screen, fails here.
    #[test]
    fn toggle_fullscreen_flips_the_effective_fullscreen_state() {
        let mut app = test_app();
        app.update();

        let initial = effective_fullscreen(&app);

        press(&mut app, WaveConductorAction::ToggleFullscreen);
        assert_ne!(
            effective_fullscreen(&app),
            initial,
            "F11 (ToggleFullscreen) must flip the effective fullscreen state"
        );

        press(&mut app, WaveConductorAction::ToggleFullscreen);
        assert_eq!(
            effective_fullscreen(&app),
            initial,
            "a second F11 press must flip the effective fullscreen state back"
        );
    }

    /// Madison's decision: F11 is an *ephemeral, session-only* override. A
    /// passer-by pressing F11 at the installation must not leave the kiosk
    /// windowed after a reboot — so F11 must never touch the autosaved
    /// `DisplaySettings`. (`start_fullscreen` is autosave-registered; any write
    /// to it hits disk 0.5 s later.)
    #[test]
    fn toggle_fullscreen_never_writes_the_persisted_display_settings() {
        let mut app = test_app();
        app.update();

        let persisted = app.world().resource::<DisplaySettings>().clone();
        assert_eq!(
            *app.world().resource::<FullscreenOverride>(),
            FullscreenOverride(None),
            "a fresh app must boot with no override — the persisted value decides"
        );

        press(&mut app, WaveConductorAction::ToggleFullscreen);

        assert_eq!(
            *app.world().resource::<DisplaySettings>(),
            persisted,
            "F11 must not mutate the persisted DisplaySettings (it would autosave)"
        );
        assert_eq!(
            *app.world().resource::<FullscreenOverride>(),
            FullscreenOverride(Some(!persisted.start_fullscreen)),
            "F11 must write the session-only override instead"
        );

        // And a second press stays entirely within the override.
        press(&mut app, WaveConductorAction::ToggleFullscreen);
        assert_eq!(*app.world().resource::<DisplaySettings>(), persisted);
        assert_eq!(
            *app.world().resource::<FullscreenOverride>(),
            FullscreenOverride(Some(persisted.start_fullscreen)),
        );
    }
}
