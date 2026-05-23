//! Translates [`WaveConductorAction`] presses into [`AppState`] transitions and
//! window-level effects (fullscreen toggle, quit).

use bevy::prelude::*;
use bevy::window::WindowMode;
use leafwing_input_manager::prelude::*;

use super::actions::WaveConductorAction;
use super::state::AppState;

/// Reads `Res<ActionState<WaveConductorAction>>` and translates `just_pressed`
/// events into navigation transitions and side effects.
///
/// Note: Bevy 0.18 renamed `EventWriter` to `MessageWriter`.
pub fn handle_navigation_actions(
    actions: Res<'_, ActionState<WaveConductorAction>>,
    current: Res<'_, State<AppState>>,
    mut next: ResMut<'_, NextState<AppState>>,
    mut windows: Query<'_, '_, &mut Window>,
    mut exit: MessageWriter<'_, AppExit>,
) {
    use WaveConductorAction as A;

    let mut transition_to: Option<AppState> = None;
    if actions.just_pressed(&A::SelectLine) {
        transition_to = Some(AppState::Line);
    } else if actions.just_pressed(&A::SelectFlame) {
        transition_to = Some(AppState::Flame);
    } else if actions.just_pressed(&A::SelectDots) {
        transition_to = Some(AppState::Dots);
    } else if actions.just_pressed(&A::SelectCymatics) {
        transition_to = Some(AppState::Cymatics);
    } else if actions.just_pressed(&A::SelectWaves) {
        transition_to = Some(AppState::Waves);
    } else if actions.just_pressed(&A::NavigateHome) {
        transition_to = Some(AppState::Home);
    } else if actions.just_pressed(&A::NavigateNext) {
        transition_to = Some(current.get().next_sketch());
    } else if actions.just_pressed(&A::NavigatePrev) {
        transition_to = Some(current.get().prev_sketch());
    }

    if let Some(target) = transition_to {
        if *current.get() != target {
            tracing::info!(?target, "navigate");
            next.set(target);
        }
    }

    if actions.just_pressed(&A::ToggleFullscreen) {
        for mut window in &mut windows {
            window.mode = match window.mode {
                WindowMode::Windowed => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
                _ => WindowMode::Windowed,
            };
            tracing::info!(mode = ?window.mode, "toggle fullscreen");
        }
    }

    if actions.just_pressed(&A::Quit) {
        tracing::info!("quit requested");
        exit.write(AppExit::Success);
    }

    // ToggleVolume is handled by crate::audio::nav::handle_volume_toggle (Plan 4).
    // ToggleDevPanel is handled by crate::settings::panel_dev::handle_dev_panel_toggle (Plan 5).
}
