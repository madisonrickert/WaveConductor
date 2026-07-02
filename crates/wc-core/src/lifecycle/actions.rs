//! Keyboard action surface for `WaveConductor`.
//!
//! The [`WaveConductorAction`] enum names every abstract action the lifecycle
//! plugin exposes. Physical key bindings are defined in [`super::action_map`]
//! via [`super::action_map::default_bindings`]; future settings UI can rebind
//! by editing the `ActionBindings` resource.

/// Top-level keyboard actions used by [`crate::lifecycle::nav`] to drive
/// [`crate::lifecycle::state::AppState`] transitions and global UI toggles.
#[derive(Clone, Copy, Hash, PartialEq, Eq, Debug)]
pub enum WaveConductorAction {
    /// Cycle to the previous sketch (`z` / `←`).
    NavigatePrev,
    /// Cycle to the next sketch (`x` / `→`).
    NavigateNext,
    /// Jump directly to Line (`1`).
    SelectLine,
    /// Jump directly to the Flame sketch (2).
    SelectFlame,
    /// Jump directly to Dots (`3`).
    SelectDots,
    /// Jump directly to Cymatics (`4`).
    ///
    /// `Digit5` is intentionally unbound for the same reason as `Digit2`
    /// above (it selected the now de-routed `Waves` seam).
    SelectCymatics,
    /// Return to the home gallery (`Escape`).
    NavigateHome,
    /// Toggle global volume (`V`). Wired in Plan 4 (audio).
    ToggleVolume,
    /// Toggle the developer settings panel (`Shift+D`). Wired in Plan 5 (settings).
    ToggleDevPanel,
    /// Toggle fullscreen (`F11`). Handled by the lifecycle plugin.
    ToggleFullscreen,
    /// Skip the idle wait and show the screensaver now (`Shift+S`).
    /// Operator/testing convenience: rewinds the idle timer past both
    /// thresholds instead of waiting out the ~60 s; any later interaction
    /// wakes the sketch exactly as it does after a natural timeout. See
    /// [`crate::lifecycle::idle::skip_to_screensaver`].
    StartScreensaver,
}

impl WaveConductorAction {
    /// Every action variant, in nav-precedence order. Used by the action-input
    /// producer to iterate actions without per-frame allocation.
    pub const ALL: [WaveConductorAction; 11] = [
        WaveConductorAction::SelectLine,
        WaveConductorAction::SelectFlame,
        WaveConductorAction::SelectDots,
        WaveConductorAction::SelectCymatics,
        WaveConductorAction::NavigateHome,
        WaveConductorAction::NavigateNext,
        WaveConductorAction::NavigatePrev,
        WaveConductorAction::ToggleVolume,
        WaveConductorAction::ToggleDevPanel,
        WaveConductorAction::ToggleFullscreen,
        WaveConductorAction::StartScreensaver,
    ];
}
