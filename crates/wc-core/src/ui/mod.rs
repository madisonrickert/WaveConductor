//! Overlay UI chrome.
//!
//! Owns every system that draws on top of the active sketch: floating
//! buttons, settings panels, the sketch picker, the auto-fade behaviour, and
//! the backdrop-blur render pass that frosted-glass panels sample.
//!
//! ## Composition
//!
//! [`WaveConductorUiPlugin`] composes five sub-plugins. They are added in
//! dependency order so that downstream plugins can rely on upstream
//! resources existing during `Startup`:
//!
//! 1. [`style::OverlayStylePlugin`] — egui [`Style`] tuned to v4.
//! 2. [`blur::BackdropBlurPlugin`] — render-graph node producing the
//!    half-resolution blurred texture every panel samples.
//! 3. [`auto_fade::AutoFadePlugin`] — `UiOpacity` driven from the existing
//!    `InteractionTimer`.
//! 4. [`buttons::OverlayButtonsPlugin`] — Home/Settings/Volume corner buttons.
//! 5. [`picker::SketchPickerPlugin`] — Home-state grid.
//!
//! [`egui::Style`]: bevy_egui::egui::Style

use bevy::prelude::*;

/// Umbrella plugin for the overlay UI surface.
pub struct WaveConductorUiPlugin;

impl Plugin for WaveConductorUiPlugin {
    fn build(&self, _app: &mut App) {
        // Sub-plugins land in subsequent tasks.
    }
}
