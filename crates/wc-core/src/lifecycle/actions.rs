//! Keyboard action mapping driven by `leafwing-input-manager`.
//!
//! The [`WaveConductorAction`] enum is the abstract action surface that the
//! lifecycle plugin consumes. The physical keys are bound here via
//! [`default_input_map`]; future settings UI can rebind by editing the
//! `InputMap` resource.

use bevy::prelude::*;
use leafwing_input_manager::prelude::*;

/// Top-level keyboard actions used by [`crate::lifecycle::nav`] to drive
/// [`crate::lifecycle::state::AppState`] transitions and global UI toggles.
#[derive(Actionlike, Reflect, Clone, Copy, Hash, PartialEq, Eq, Debug)]
#[reflect(Hash, PartialEq)]
pub enum WaveConductorAction {
    /// Cycle to the previous sketch (`z` / `←`).
    NavigatePrev,
    /// Cycle to the next sketch (`x` / `→`).
    NavigateNext,
    /// Jump directly to sketch 1 — Line (`1`).
    SelectLine,
    /// Jump to sketch 2 — Flame (`2`).
    SelectFlame,
    /// Jump to sketch 3 — Dots (`3`).
    SelectDots,
    /// Jump to sketch 4 — Cymatics (`4`).
    SelectCymatics,
    /// Jump to sketch 5 — Waves (`5`).
    SelectWaves,
    /// Return to the home gallery (`Escape`).
    NavigateHome,
    /// Toggle global volume (`V`). Wired in Plan 4 (audio).
    ToggleVolume,
    /// Toggle the developer settings panel (`Shift+D`). Wired in Plan 5 (settings).
    ToggleDevPanel,
    /// Toggle fullscreen (`F11`). Handled by the lifecycle plugin.
    ToggleFullscreen,
    /// Quit the application (`Ctrl+Q`). macOS `Cmd+Q` is handled by the OS and never reaches the app.
    Quit,
}

/// Build the default `InputMap<WaveConductorAction>` matching v4's hotkey table.
///
/// Returned as a `Resource` so the lifecycle plugin can register it and a future
/// settings panel can mutate it.
#[must_use]
pub fn default_input_map() -> InputMap<WaveConductorAction> {
    use WaveConductorAction as A;

    let mut map = InputMap::default();

    // Sketch selection (number row keys)
    map.insert(A::SelectLine, KeyCode::Digit1);
    map.insert(A::SelectFlame, KeyCode::Digit2);
    map.insert(A::SelectDots, KeyCode::Digit3);
    map.insert(A::SelectCymatics, KeyCode::Digit4);
    map.insert(A::SelectWaves, KeyCode::Digit5);

    // Sequential navigation
    map.insert(A::NavigatePrev, KeyCode::KeyZ);
    map.insert(A::NavigatePrev, KeyCode::ArrowLeft);
    map.insert(A::NavigateNext, KeyCode::KeyX);
    map.insert(A::NavigateNext, KeyCode::ArrowRight);

    // Global toggles
    map.insert(A::NavigateHome, KeyCode::Escape);
    map.insert(A::ToggleVolume, KeyCode::KeyV);
    map.insert(A::ToggleFullscreen, KeyCode::F11);

    // Modifier combos — leafwing 0.20 uses ButtonlikeChord::modified instead of
    // insert_modified (which was removed in the 0.16→0.20 API bump).
    map.insert(
        A::ToggleDevPanel,
        ButtonlikeChord::modified(ModifierKey::Shift, KeyCode::KeyD),
    );

    // Platform-specific quit: Ctrl+Q on all platforms (Cmd+Q is handled by macOS
    // natively; the binding here fires on Ctrl+Q / Cmd+Q via ModifierKey::Control).
    map.insert(
        A::Quit,
        ButtonlikeChord::modified(ModifierKey::Control, KeyCode::KeyQ),
    );

    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_input_map_contains_all_actions() {
        let map = default_input_map();
        // Every variant should have at least one binding.
        for action in [
            WaveConductorAction::NavigatePrev,
            WaveConductorAction::NavigateNext,
            WaveConductorAction::SelectLine,
            WaveConductorAction::SelectFlame,
            WaveConductorAction::SelectDots,
            WaveConductorAction::SelectCymatics,
            WaveConductorAction::SelectWaves,
            WaveConductorAction::NavigateHome,
            WaveConductorAction::ToggleVolume,
            WaveConductorAction::ToggleDevPanel,
            WaveConductorAction::ToggleFullscreen,
            WaveConductorAction::Quit,
        ] {
            assert!(
                map.get_buttonlike(&action).is_some(),
                "no binding for {action:?}",
            );
        }
    }
}
