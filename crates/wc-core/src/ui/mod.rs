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
//! 1. `style::OverlayStylePlugin` — egui `Style` tuned to v4.
//! 2. `blur::BackdropBlurPlugin` — render-graph node producing the
//!    half-resolution blurred texture every panel samples.
//! 3. `auto_fade::AutoFadePlugin` — `UiOpacity` driven from the existing
//!    `InteractionTimer`.
//! 4. `buttons::OverlayButtonsPlugin` — Home/Settings/Volume corner buttons.
//! 5. `picker::SketchPickerPlugin` — Home-state grid.

use bevy::prelude::*;

pub mod auto_fade;
pub mod blur;
pub mod buttons;
pub mod frame;
pub mod picker;
pub mod reload_overlay;
pub mod style;
pub mod text;

pub use blur::{BackdropBlurEnabled, BackdropBlurPlugin, BackdropBlurTexture};
pub use buttons::PointerCoarse;
pub use frame::{backdrop_blur_frame, FrameOptions};
pub use style::OverlayStyle;
pub use text::letter_spaced_label;

/// Umbrella plugin for the overlay UI surface.
pub struct WaveConductorUiPlugin;

impl Plugin for WaveConductorUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            style::OverlayStylePlugin,
            blur::BackdropBlurPlugin,
            auto_fade::AutoFadePlugin,
            buttons::OverlayButtonsPlugin,
            picker::SketchPickerPlugin,
        ));
        // Full-screen reload fade overlay: runs unconditionally (no state gate)
        // so it fires even during the one-frame Switch phase (AppState::Home).
        app.add_systems(
            bevy_egui::EguiPrimaryContextPass,
            reload_overlay::draw_reload_overlay,
        );
    }
}
