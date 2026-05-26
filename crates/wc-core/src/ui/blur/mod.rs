//! Backdrop-blur render-graph node and paint-callback integration.
//!
//! ## Pipeline
//!
//! 1. Once per frame, [`node::BackdropBlurNode`] samples the camera's
//!    post-tonemap colour attachment, runs 3 downsample passes
//!    (1/2 → 1/4 → 1/8) and 3 upsample passes back to 1/2 resolution
//!    using the dual-Kawase shaders, and parks the result in
//!    [`BackdropBlurTexture`].
//! 2. Any panel that wants frosted glass wraps its content in
//!    [`super::frame::backdrop_blur_frame`], which pushes a
//!    [`callback::BackdropBlurPaintCallback`] into the egui paint list.
//!    The callback samples [`BackdropBlurTexture`] in its render method
//!    and draws a textured quad with a corner-radius SDF mask.
//! 3. egui then paints the panel's translucent tint on top of the blurred
//!    rect, completing the CSS `backdrop-filter: blur()` compositing
//!    order.

use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};

/// Master toggle for the backdrop-blur node. Lives in the main world.
///
/// Default `true`. The dev panel's `OverlayUiSettings::backdrop_blur_enabled`
/// mirror flips this each frame.
#[derive(Resource, Debug, Clone, Copy, ExtractResource)]
pub struct BackdropBlurEnabled(pub bool);

impl Default for BackdropBlurEnabled {
    fn default() -> Self {
        Self(true)
    }
}

/// Plugin assembly for the backdrop-blur feature.
///
/// Inserts [`BackdropBlurEnabled`] into the main world and wires the
/// [`ExtractResourcePlugin`] so the render app sees the toggle each frame.
/// The render-graph node, texture allocation, and paint callback are added
/// in subsequent tasks (8, 10, 11).
pub struct BackdropBlurPlugin;

impl Plugin for BackdropBlurPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BackdropBlurEnabled>();
        app.add_plugins(ExtractResourcePlugin::<BackdropBlurEnabled>::default());
        // node::add_to_render_app and callback::add_to_render_app land in
        // Tasks 8 / 10 / 11.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_resource_default_is_true() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<BackdropBlurEnabled>();
        assert!(app.world().resource::<BackdropBlurEnabled>().0);
    }
}
