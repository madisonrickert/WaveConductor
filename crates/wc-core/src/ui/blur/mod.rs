//! Backdrop-blur render-graph node and paint-callback integration.
//!
//! ## Pipeline
//!
//! 1. Once per frame, [`node::backdrop_blur`] samples the camera's
//!    post-tonemap colour attachment, runs 3 downsample passes
//!    (1/2 → 1/4 → 1/8) and 3 upsample passes back to 1/2
//!    resolution using the dual-Kawase shaders with a 1.0× texel offset,
//!    and parks the result in [`BackdropBlurTexture`].
//! 2. Any panel that wants frosted glass wraps its content in
//!    [`super::frame::backdrop_blur_frame`], which pushes a
//!    [`callback::BackdropBlurPaintCallback`] into the egui paint list.
//!    The callback samples [`BackdropBlurTexture`] in its render method
//!    and draws a textured quad with a corner-radius SDF mask.
//! 3. egui then paints the panel's translucent tint on top of the blurred
//!    rect, completing the CSS `backdrop-filter: blur()` compositing
//!    order.

pub mod callback;
pub mod node;
pub(crate) mod slots;

use bevy::math::UVec2;
use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_resource::{
    AddressMode, Extent3d, FilterMode, MipmapFilterMode, Sampler, SamplerDescriptor, Texture,
    TextureDescriptor, TextureDimension, TextureFormat, TextureUsages, TextureView,
    TextureViewDescriptor,
};
use bevy::render::renderer::RenderDevice;
use bevy::render::view::ExtractedWindows;
use bevy::render::{Render, RenderApp, RenderSystems};

/// Master toggle for the backdrop-blur node. Lives in the main world.
///
/// Default `true`. [`crate::ui::auto_fade::sync_backdrop_blur_enabled`] runs
/// each `Update` and writes `OverlayUiSettings::backdrop_blur_enabled` into
/// this resource, so toggling the dev-panel checkbox takes effect on the next
/// rendered frame.
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
    /// render attachment for the final blur upsample pass and as the sampled
    /// texture for the composite paint callback.
    pub view: TextureView,
    /// Bilinear clamp-to-edge sampler shared across all Kawase passes and
    /// the composite callback.
    pub sampler: Sampler,
    /// Physical half-resolution at which the texture was allocated.
    /// `ensure_blur_texture` uses this to skip reallocation when unchanged.
    pub extent: UVec2,
}

/// Intermediate scratch textures for the dual-Kawase downsample/upsample chain.
///
/// Allocated alongside [`BackdropBlurTexture`] in `ensure_blur_texture`.
/// Three levels: half, quarter, and eighth of the primary viewport's physical
/// resolution.
///
/// The private `_*_tex` fields hold the [`Texture`] objects to keep the GPU
/// resources alive while the views remain valid.
#[derive(Resource)]
pub struct BackdropBlurScratch {
    /// View into the half-resolution intermediate texture.
    pub half_view: TextureView,
    /// View into the quarter-resolution intermediate texture.
    pub quarter_view: TextureView,
    /// View into the eighth-resolution intermediate texture.
    pub eighth_view: TextureView,
    /// Physical size of the half-resolution scratch texture.
    pub half_extent: UVec2,
    /// Physical size of the quarter-resolution scratch texture.
    pub quarter_extent: UVec2,
    /// Physical size of the eighth-resolution scratch texture.
    pub eighth_extent: UVec2,
    // Hold textures alive so their views remain valid.
    _half_tex: Texture,
    _quarter_tex: Texture,
    _eighth_tex: Texture,
}

/// Plugin assembly for the backdrop-blur feature.
///
/// Inserts [`BackdropBlurEnabled`] into the main world and wires the
/// [`ExtractResourcePlugin`] so the render app sees the toggle each frame.
/// Also registers `ensure_blur_texture` in the render app so the
/// half-resolution [`BackdropBlurTexture`] and [`BackdropBlurScratch`] are
/// allocated on first frame and resized on window-resize events.
pub struct BackdropBlurPlugin;

impl BackdropBlurPlugin {
    /// Wires render-sub-app systems and the render graph node.
    ///
    /// Called from [`Plugin::build`]. Returns early without error if no
    /// `RenderApp` is present (e.g. headless tests that don't load
    /// `RenderPlugin`).
    fn setup_render_app(app: &mut App) {
        // In Bevy 0.18, get_sub_app_mut returns Option<&mut SubApp>.
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.add_systems(
            Render,
            ensure_blur_texture.in_set(RenderSystems::PrepareResources),
        );
        node::setup_render_systems(render_app);
        node::setup_render_graph(render_app);
    }
}

impl Plugin for BackdropBlurPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BackdropBlurEnabled>();
        app.add_plugins(ExtractResourcePlugin::<BackdropBlurEnabled>::default());
        Self::setup_render_app(app);
    }

    /// Initialise render-app resources that depend on `PipelineCache` and
    /// `AssetServer` being fully set up. Called after all `build` methods
    /// complete, matching the pattern used by
    /// `LinePostProcessPlugin`.
    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.init_resource::<node::BackdropBlurPipeline>();
        render_app.init_resource::<callback::CompositePipeline>();
    }
}

/// Allocate or reallocate the half-resolution blur texture and scratch textures
/// in the render world.
///
/// Reads the primary window's physical size from [`ExtractedWindows`], halves
/// each dimension (minimum 1 px), and skips reallocation when the existing
/// [`BackdropBlurTexture`] already matches. On (re)allocation, creates:
/// - [`BackdropBlurTexture`] at 1/2 resolution (final blur output).
/// - [`BackdropBlurScratch`] with textures at 1/2, 1/4, and 1/8 resolution
///   (intermediate Kawase chain stages).
///
/// All textures use `Rgba16Float` format with `RENDER_ATTACHMENT |
/// TEXTURE_BINDING` usages — matching the camera's HDR view target, which
/// is unconditionally `Rgba16Float` while internal-HDR rendering is on.
/// The blur node samples *from* the view target into these scratch
/// textures and the composite pipeline writes *back* into the view target;
/// any format mismatch on either side produces a wgpu validation error.
/// The shared sampler on [`BackdropBlurTexture`] is bilinear clamp-to-edge.
///
/// Runs in [`RenderSystems::PrepareResources`] so resources are ready before
/// any bind-group creation.
///
/// # Window in the render world
///
/// In Bevy 0.18 the render app does **not** mirror `Window` + `PrimaryWindow`
/// as ECS components. Instead, `bevy_render`'s `WindowRenderPlugin` extracts
/// window state into the [`ExtractedWindows`] resource each frame. The primary
/// window's entity is in `ExtractedWindows::primary`; its physical dimensions
/// are in `ExtractedWindow::physical_width` / `physical_height`. These values
/// reflect the previous main-world frame — one frame behind a resize, which
/// is acceptable for a blur texture.
pub(super) fn ensure_blur_texture(
    mut commands: Commands<'_, '_>,
    device: Res<'_, RenderDevice>,
    existing: Option<Res<'_, BackdropBlurTexture>>,
    extracted_windows: Res<'_, ExtractedWindows>,
) {
    let Some(primary_entity) = extracted_windows.primary else {
        return;
    };
    let Some(window) = extracted_windows.windows.get(&primary_entity) else {
        return;
    };
    let physical = UVec2::new(window.physical_width, window.physical_height);
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

    // Helper: create one scratch texture at the given dimensions.
    //
    // `Rgba16Float` is mandatory while internal-HDR rendering is on — the
    // camera's view target is `Rgba16Float` (see `spawn_camera` in the
    // binary crate), and the Kawase chain samples from that target via
    // `ViewTarget::post_process_write().source`. wgpu refuses to sample
    // from an `Rgba16Float` source with a pipeline that declares an sRGB
    // 8-bit target attachment, so the scratch textures and the composite
    // pipeline's `ColorTargetState` must all match the view target's HDR
    // format. If we ever add an SDR build, this becomes a runtime read
    // from `ViewTarget::main_texture_format()` instead of a constant.
    let make_scratch = |dim: UVec2, label: &'static str| -> (Texture, TextureView) {
        let tex = device.create_texture(&TextureDescriptor {
            label: Some(label),
            size: Extent3d {
                width: dim.x.max(1),
                height: dim.y.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba16Float,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&TextureViewDescriptor::default());
        (tex, view)
    };

    let quarter = UVec2::new((physical.x / 4).max(1), (physical.y / 4).max(1));
    let eighth = UVec2::new((physical.x / 8).max(1), (physical.y / 8).max(1));

    // Scratch textures (intermediate Kawase chain stages).
    let (half_tex, half_view) = make_scratch(half, "backdrop_blur_scratch_half");
    let (quarter_tex, quarter_view) = make_scratch(quarter, "backdrop_blur_scratch_quarter");
    let (eighth_tex, eighth_view) = make_scratch(eighth, "backdrop_blur_scratch_eighth");

    commands.insert_resource(BackdropBlurScratch {
        half_view,
        quarter_view,
        eighth_view,
        half_extent: half,
        quarter_extent: quarter,
        eighth_extent: eighth,
        _half_tex: half_tex,
        _quarter_tex: quarter_tex,
        _eighth_tex: eighth_tex,
    });

    // Final output texture. Format matches the scratch chain and the
    // camera's HDR view target — see the `make_scratch` comment above for
    // why this is unconditional `Rgba16Float`.
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
        format: TextureFormat::Rgba16Float,
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
        mipmap_filter: MipmapFilterMode::Nearest,
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
