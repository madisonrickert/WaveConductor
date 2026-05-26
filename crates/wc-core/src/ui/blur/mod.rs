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

use bevy::math::UVec2;
use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_resource::{
    AddressMode, Extent3d, FilterMode, Sampler, SamplerDescriptor, Texture, TextureDescriptor,
    TextureDimension, TextureFormat, TextureUsages, TextureView, TextureViewDescriptor,
};
use bevy::render::renderer::RenderDevice;
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::window::PrimaryWindow;

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

/// Half-resolution blurred frame texture sampled by every overlay panel.
///
/// Lives in the [`RenderApp`]; allocated lazily on the first frame and
/// reallocated whenever the primary window's physical resolution changes.
/// [`texture`](BackdropBlurTexture::texture) is held to keep the GPU resource
/// alive while [`view`](BackdropBlurTexture::view) is sampled by draw calls.
#[derive(Resource)]
pub struct BackdropBlurTexture {
    /// Backing GPU texture. Kept alive so the view remains valid.
    pub texture: Texture,
    /// View into [`texture`](BackdropBlurTexture::texture); bound as the
    /// render attachment for blur passes and as the sampled texture for
    /// the composite paint callback.
    pub view: TextureView,
    /// Bilinear clamp-to-edge sampler for the blur texture.
    pub sampler: Sampler,
    /// Physical half-resolution at which the texture was allocated.
    /// `ensure_blur_texture` uses this to skip reallocation when unchanged.
    pub extent: UVec2,
}

/// Plugin assembly for the backdrop-blur feature.
///
/// Inserts [`BackdropBlurEnabled`] into the main world and wires the
/// [`ExtractResourcePlugin`] so the render app sees the toggle each frame.
/// Also registers [`ensure_blur_texture`] in the render app so the
/// half-resolution [`BackdropBlurTexture`] is allocated on first frame and
/// resized on window-resize events.
pub struct BackdropBlurPlugin;

impl BackdropBlurPlugin {
    /// Wires render-sub-app systems. Called from [`Plugin::build`].
    ///
    /// Returns early without error if no `RenderApp` is present (e.g. in
    /// headless tests that don't load `RenderPlugin`).
    fn setup_render_app(app: &mut App) {
        // In Bevy 0.18, get_sub_app_mut returns Option<&mut SubApp>.
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.add_systems(
            Render,
            ensure_blur_texture.in_set(RenderSystems::PrepareResources),
        );
        // Task 9 will initialize BackdropBlurPipeline here.
    }
}

impl Plugin for BackdropBlurPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BackdropBlurEnabled>();
        app.add_plugins(ExtractResourcePlugin::<BackdropBlurEnabled>::default());
        Self::setup_render_app(app);
    }
}

/// Allocate or reallocate the half-resolution blur texture in the render world.
///
/// Reads the primary window's physical size, halves each dimension (minimum
/// 1 px), and skips reallocation when the existing [`BackdropBlurTexture`]
/// already matches. On (re)allocation, creates a new `Rgba8UnormSrgb` texture
/// with `RENDER_ATTACHMENT | TEXTURE_BINDING` usages and a bilinear
/// clamp-to-edge sampler, then inserts it as a resource.
///
/// Runs in [`RenderSystems::PrepareResources`] so it is ready before any
/// bind-group build pass.
///
/// # Window in the render world
///
/// `bevy_window` extracts the primary `Window` component into the render app
/// via its own extraction system, so the query here runs against the render
/// world's copy, which reflects the logical state from the previous main-world
/// update. The physical dimensions are therefore one frame behind a resize
/// event — acceptable for a blur texture.
pub(super) fn ensure_blur_texture(
    mut commands: Commands<'_, '_>,
    device: Res<'_, RenderDevice>,
    existing: Option<Res<'_, BackdropBlurTexture>>,
    windows: Query<'_, '_, &Window, With<PrimaryWindow>>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let physical = UVec2::new(window.physical_width(), window.physical_height());
    // Guard against zero-sized windows during startup or minimization.
    if physical.x == 0 || physical.y == 0 {
        return;
    }
    let half = UVec2::new((physical.x / 2).max(1), (physical.y / 2).max(1));

    if let Some(tex) = existing.as_deref() {
        if tex.extent == half {
            return;
        }
    }

    let descriptor = TextureDescriptor {
        label: Some("backdrop_blur_texture"),
        size: Extent3d {
            width: half.x,
            height: half.y,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8UnormSrgb,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    };

    let texture = device.create_texture(&descriptor);
    let view = texture.create_view(&TextureViewDescriptor::default());
    let sampler = device.create_sampler(&SamplerDescriptor {
        label: Some("backdrop_blur_sampler"),
        address_mode_u: AddressMode::ClampToEdge,
        address_mode_v: AddressMode::ClampToEdge,
        address_mode_w: AddressMode::ClampToEdge,
        mag_filter: FilterMode::Linear,
        min_filter: FilterMode::Linear,
        mipmap_filter: FilterMode::Nearest,
        ..default()
    });

    commands.insert_resource(BackdropBlurTexture {
        texture,
        view,
        sampler,
        extent: half,
    });
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
