//! Translates [`WaveConductorAction`] presses into [`AppState`] transitions and
//! window-level effects (fullscreen toggle).

use bevy::prelude::*;
use bevy::window::WindowMode;

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
pub fn handle_navigation_actions(
    mut actions: MessageReader<'_, '_, ActionInput>,
    current: Res<'_, State<AppState>>,
    mut next: ResMut<'_, NextState<AppState>>,
    mut windows: Query<'_, '_, &mut Window>,
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
            A::SelectWaves => pressed_select = pressed_select.or(Some(AppState::Waves)),
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
        for mut window in &mut windows {
            window.mode = match window.mode {
                WindowMode::Windowed => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
                _ => WindowMode::Windowed,
            };
            tracing::info!(mode = ?window.mode, "toggle fullscreen");
        }
    }

    // ToggleVolume is handled by crate::audio::nav::handle_volume_toggle (Plan 4).
    // ToggleDevPanel is handled by crate::settings::panel_dev::handle_dev_panel_toggle (Plan 5).
}
