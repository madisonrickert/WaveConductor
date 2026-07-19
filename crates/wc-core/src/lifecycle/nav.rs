//! Translates [`WaveConductorAction`] presses into [`AppState`] transitions and
//! window-level effects (fullscreen toggle).
//!
//! ## Sketch-to-sketch transitions are graceful, not instant
//!
//! A sketch-select key, `NavigateHome`, `NavigateNext`, or `NavigatePrev` no
//! longer writes `NextState<AppState>` directly. Instead it calls
//! [`SketchReloadState::begin_fade_out`] with the destination state as
//! `return_state` and [`ReloadReason::SketchSwitch`], reusing the same
//! `FadeOut → Switch → FadeIn` machine that a settings restart or a settled
//! window resize already drive (see `crate::lifecycle::reload`'s module doc).
//! `drive_reload_state` (registered alongside this system in
//! `LifecyclePlugin`, immediately after it in the same `Update` chain) owns
//! every subsequent phase transition and `NextState` write. The practical
//! effect: pressing a sketch-select key dips the screen to black, dips the
//! master volume, hops through `Home`, and fades back in on the destination —
//! rather than cutting instantly, which used to expose a one-frame flash of
//! the outgoing sketch's teardown and the incoming sketch's cold-start.
//!
//! Two edge cases, both deliberate:
//! - **Navigating to the state already active** is a no-op, exactly as before
//!   (`*current.get() != target` still gates the whole block).
//! - **Navigating while a reload is already in flight** is *ignored*, not
//!   retargeted. A second key press landing mid-fade (rapid double-tap, or a
//!   nav key racing a settings-restart/resize reload that happened to be
//!   in-flight already) is simply dropped; the in-flight reload completes on
//!   its original destination. Retargeting mid-fade would mean deciding which
//!   phase to resume from and risks a visible audio/alpha discontinuity, for
//!   a scenario (a sub-second-long window) a user is very unlikely to
//!   deliberately hit twice.
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
use super::reload::{ReloadReason, SketchReloadState};
use super::state::AppState;
use crate::audio::state::AudioState;

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
    time: Res<'_, Time>,
    mut reload_state: ResMut<'_, SketchReloadState>,
    audio_state: Option<Res<'_, AudioState>>,
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
            if reload_state.is_idle() {
                // Fall back to full volume (1.0) when the audio engine hasn't
                // started — headless tests and early startup before the cpal
                // stream is active. Mirrors `restart_on_settings_change` /
                // `reload_on_resize_settled` in `crate::sketch::lifecycle`.
                let pre_fade_volume = audio_state.as_ref().map_or(1.0, |s| s.volume);
                tracing::info!(?target, "navigate (graceful sketch switch)");
                reload_state.begin_fade_out(
                    time.elapsed(),
                    pre_fade_volume,
                    target,
                    ReloadReason::SketchSwitch,
                );
            } else {
                // A reload is already in flight — ignored, not retargeted.
                // See the module doc for why.
                tracing::debug!(?target, "navigate ignored: a reload is already in flight");
            }
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

    /// A sketch-select key must begin a graceful [`ReloadReason::SketchSwitch`]
    /// reload rather than writing `NextState<AppState>` directly — the whole
    /// point of this change (see the module doc). `AppState` must NOT have
    /// moved yet by the same frame's end: the transition only lands once
    /// `drive_reload_state` walks `FadeOut -> Switch -> FadeIn` to completion.
    #[test]
    fn selecting_a_sketch_begins_a_graceful_reload_instead_of_an_instant_transition() {
        use bevy::time::TimeUpdateStrategy;

        let mut app = test_app();
        // Freeze the clock at zero elapsed-per-tick so the FadeOut-still-mid-fade
        // assertion below is deterministic rather than depending on real
        // wall-clock time staying under SKETCH_SWITCH_FADE_DURATION (400 ms)
        // between two `app.update()` calls.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(
            std::time::Duration::ZERO,
        ));
        app.update();

        press(&mut app, WaveConductorAction::SelectLine);

        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Home,
            "AppState must not move instantly — the reload machine hasn't \
             finished its FadeOut leg yet"
        );
        let reload = app.world().resource::<SketchReloadState>();
        assert_eq!(reload.phase, crate::lifecycle::reload::ReloadPhase::FadeOut);
        assert_eq!(reload.return_state, AppState::Line);
        assert_eq!(reload.reason, ReloadReason::SketchSwitch);
    }

    /// The already-in-flight edge case documented on the module: a nav key
    /// pressed while a reload is mid-fade is ignored outright, not retargeted.
    /// Simulates "already in flight" by hand-arming `SketchReloadState` (stands
    /// in for either a rapid double-tap or a settings-restart/resize reload
    /// that happened to be running already) rather than needing a second real
    /// key press to land inside the fade window.
    #[test]
    fn navigating_while_a_reload_is_in_flight_is_ignored() {
        use bevy::time::TimeUpdateStrategy;

        let mut app = test_app();
        // Freeze the clock (see the comment in the test above) so the
        // hand-armed reload is still genuinely in-flight by the time the
        // second `press` runs, regardless of real wall-clock speed.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(
            std::time::Duration::ZERO,
        ));
        app.update();

        // Hand-arm an in-flight reload bound for Dots, as if some other
        // trigger had already begun one.
        {
            let now = app.world().resource::<Time>().elapsed();
            let mut reload = app.world_mut().resource_mut::<SketchReloadState>();
            reload.begin_fade_out(now, 1.0, AppState::Dots, ReloadReason::SettingsRestart);
        }

        press(&mut app, WaveConductorAction::SelectLine);

        let reload = app.world().resource::<SketchReloadState>();
        assert_eq!(
            reload.return_state,
            AppState::Dots,
            "a nav key pressed mid-fade must not retarget the in-flight reload"
        );
        assert_eq!(
            reload.reason,
            ReloadReason::SettingsRestart,
            "the original reload's reason must survive untouched"
        );
    }

    /// Navigating to the state that is already active is a no-op: it must not
    /// arm a reload at all. Reaches `AppState::Line` first via the graceful
    /// path (mirroring `select_line_transitions_into_line_state` in
    /// `tests/lifecycle.rs`), then presses `SelectLine` again.
    #[test]
    fn navigating_to_the_already_active_state_does_not_arm_a_reload() {
        use bevy::time::{TimeUpdateStrategy, Virtual};

        let mut app = test_app();
        app.update();

        press(&mut app, WaveConductorAction::SelectLine);
        // Settle the graceful reload: a 500 ms manual step comfortably
        // covers `SKETCH_SWITCH_FADE_DURATION` (400 ms) per leg, so three
        // updates walk FadeOut -> Switch -> FadeIn -> Idle, exactly as the
        // phase-walk tests in `lifecycle/reload.rs` do. `Time<Virtual>`'s
        // default `max_delta` (250 ms) would otherwise silently clamp the
        // 500 ms step below the 400 ms fade, stalling it forever.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_millis(500),
        ));
        app.world_mut()
            .resource_mut::<Time<Virtual>>()
            .set_max_delta(std::time::Duration::from_secs(1));
        app.update();
        app.update();
        app.update();
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Line,
            "precondition: must have settled into Line"
        );
        assert!(app.world().resource::<SketchReloadState>().is_idle());

        press(&mut app, WaveConductorAction::SelectLine);

        assert!(
            app.world().resource::<SketchReloadState>().is_idle(),
            "pressing the select key for the already-active sketch must not \
             arm a reload"
        );
    }
}
