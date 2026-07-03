//! [`FlameMaterial`]: the additive billboard material for the Flame point
//! cloud, plus its per-frame uniform driver.
//!
//! ## In-material 3D projection (no `Camera3d`)
//!
//! The app runs exactly one window camera: the global HDR `Camera2d`. Rather
//! than add a second `Camera3d` (which would re-open the shared-MSAA-texture
//! landmine documented on the main camera and route the hand mesh through a
//! 3D pass), the orbit camera arrives as two `mat4` uniforms and the vertex
//! shader does the perspective projection + screen-space billboarding +
//! fake-DoF sizing itself, drawing into the single 2D pipeline (which keeps
//! HDR + bloom + tonemapping untouched). See `assets/shaders/flame/render.wgsl`.
//!
//! ## Additive blend
//!
//! `alpha_mode = AlphaMode2d::Blend` routes the draw into `Transparent2d`, and
//! [`FlameMaterial::specialize`] overrides the color target's blend to pure
//! additive `(One, One)` — the mechanism that reproduces v4's
//! `THREE.AdditiveBlending` inside the 2D pipeline. v4's `AdditiveBlending` is
//! `(SrcAlpha, One)` with the shader's final `pow(rgba, gamma)`; with
//! `(One, One)` the fragment multiplies its own alpha in
//! (`contribution = pow(rgb, gamma) * pow(alpha, gamma)`), which is
//! algebraically identical.

use std::f32::consts::FRAC_PI_2;

use bevy::asset::Asset;
use bevy::image::Image;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, RenderPipelineDescriptor,
    SpecializedMeshPipelineError,
};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey, MeshMaterial2d};

use crate::flame::settings::FlameSettings;
use crate::flame::systems::sim_params::FlameState;
use crate::flame::systems::spawn::FlameRoot;

/// The additive billboard material for the Flame point cloud.
///
/// Shares the [`ShaderBuffer`] node handle with [`crate::flame::compute::sim_params::FlameSimParams`]
/// (the compute pass writes it read-write; the render vertex shader reads it
/// read-only). Bind group layout is `@group(2)`; see the shader for the full
/// binding contract.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct FlameMaterial {
    /// Node storage buffer, read-only from the vertex shader. The same handle
    /// the compute pipeline writes each frame.
    #[storage(0, read_only)]
    pub nodes: Handle<ShaderBuffer>,
    /// Disc sprite sampled in the fragment shader; its alpha shapes each point
    /// into a soft round splat rather than a flat quad.
    #[texture(1)]
    #[sampler(2)]
    pub disc_texture: Handle<Image>,
    /// `view_from_world * model`. Model bakes v4's `pointCloud.rotateX(-PI/2)`.
    #[uniform(3)]
    pub view_from_model: Mat4,
    /// Perspective projection: fovy 60 deg, near 0.01, far 25 (v4 camera).
    #[uniform(4)]
    pub clip_from_view: Mat4,
    /// x: `focal_length` (camera distance), y: base point size px,
    /// z: `DoF` strength, w: point opacity.
    #[uniform(5)]
    pub render_a: Vec4,
    /// x: live node count, y: gamma, z: brightness, w: point size clamp px.
    #[uniform(6)]
    pub render_b: Vec4,
    /// xyz: fog color (linear), w: unused.
    #[uniform(7)]
    pub fog_color: Vec4,
    /// x: fog near, y: fog far, z/w: viewport width/height px.
    #[uniform(8)]
    pub fog_range: Vec4,
}

impl Material2d for FlameMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/flame/render.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/flame/render.wgsl".into()
    }

    /// `Blend` routes the draw into the `Transparent2d` pass; [`Self::specialize`]
    /// then overrides the blend factors to pure additive.
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }

    /// Override the color-target blend to pure additive `(One, One)` — v4's
    /// `THREE.AdditiveBlending` inside the 2D pipeline. Per-material-pipeline,
    /// so it does not leak into the other sketches' `ParticleMaterial` /
    /// `CymaticsMaterial` blends.
    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(Some(target)) = fragment.targets.get_mut(0) {
                target.blend = Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                });
            }
        }
        Ok(())
    }
}

/// The v4 start pose, used until the F9 orbit driver takes over: eye
/// `(0.0, 0.35, 0.7)` looking at the origin (up +Y). Returns
/// `(view_from_model, clip_from_view)`:
/// - `view_from_model = view * rotateX(-PI/2)` (bakes v4's `pointCloud.rotateX`).
/// - `clip_from_view` = 60 deg fovy perspective, near 0.01, far 25 (v4 camera).
#[must_use]
pub fn default_view_matrices(aspect: f32) -> (Mat4, Mat4) {
    let view = Mat4::look_at_rh(Vec3::new(0.0, 0.35, 0.7), Vec3::ZERO, Vec3::Y);
    let view_from_model = view * Mat4::from_rotation_x(-FRAC_PI_2);
    let clip_from_view = Mat4::perspective_rh(60.0_f32.to_radians(), aspect, 0.01, 25.0);
    (view_from_model, clip_from_view)
}

/// Flame's scene background `#10101f` in linear space (the fog target color).
/// `w` is unused (the fog factor is carried separately).
#[must_use]
pub fn flame_fog_color() -> Vec4 {
    let c = Color::srgb_u8(0x10, 0x10, 0x1f).to_linear();
    Vec4::new(c.red, c.green, c.blue, 0.0)
}

/// `Update` (gated `in_state(AppState::Flame)`, so it also runs during Idle /
/// Screensaver like `drive_dots_master_brightness`): pack [`FlameSettings`] +
/// [`FlameState`] into every material uniform each frame.
///
/// The camera never stops autorotating, so change-gating buys nothing here;
/// the per-frame cost is an 8-uniform bind-group re-prepare, the same class as
/// Cymatics' per-frame render params. Until F9 lands, the view/projection
/// matrices come from [`default_view_matrices`] and `render_a.x` (focal) is the
/// v4 start-pose camera distance; F9 swaps in the live `FlameCamera` matrices.
#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "live node count is bounded by MAX_POINTS (200k), exact as f32"
)]
pub fn drive_flame_material(
    settings: Res<'_, FlameSettings>,
    state: Res<'_, FlameState>,
    window: Single<'_, '_, &Window>,
    roots: Query<'_, '_, &MeshMaterial2d<FlameMaterial>, With<FlameRoot>>,
    mut materials: ResMut<'_, Assets<FlameMaterial>>,
) {
    let w = window.width().max(1.0);
    let h = window.height().max(1.0);
    let aspect = w / h;
    let (view_from_model, clip_from_view) = default_view_matrices(aspect);

    // focal = camera distance; the v4 start pose sits ~0.7826 units from the
    // origin (F9 replaces this with the live orbit distance).
    let focal = Vec3::new(0.0, 0.35, 0.7).length();
    let render_a = Vec4::new(
        focal,
        settings.base_point_size,
        settings.dof_strength,
        settings.point_opacity,
    );
    let live = state.layout.live_count_for_complexity(state.complexity) as f32;
    let render_b = Vec4::new(
        live,
        settings.gamma,
        settings.master_brightness,
        settings.point_size_clamp,
    );
    let fog_color = flame_fog_color();
    let fog_range = Vec4::new(settings.fog_near, settings.fog_far, w, h);

    for handle in &roots {
        if let Some(mut material) = materials.get_mut(&handle.0) {
            material.view_from_model = view_from_model;
            material.clip_from_view = clip_from_view;
            material.render_a = render_a;
            material.render_b = render_b;
            material.fog_color = fog_color;
            material.fog_range = fog_range;
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// The default pose matches v4: camera at (0, 0.35, 0.7), distance
    /// ~0.7826; the origin projects in front of the camera at that distance.
    #[test]
    fn default_pose_matches_v4_camera() {
        let (view_model, clip_from_view) = default_view_matrices(16.0 / 9.0);
        let origin_view = view_model * Vec4::new(0.0, 0.0, 0.0, 1.0);
        let dist = -origin_view.z;
        assert!((dist - (0.35_f32 * 0.35 + 0.7 * 0.7).sqrt()).abs() < 1e-5);
        let clip = clip_from_view * origin_view;
        assert!(clip.w > 0.0, "origin is in front of the near plane");
    }

    /// The model rotation is v4's rotateX(-PI/2): model-space +Z maps to
    /// world/view -Y-ish (a point above the fractal plane tips toward the
    /// camera's down axis, not away).
    #[test]
    fn model_bakes_negative_x_quarter_turn() {
        let (view_model, _) = default_view_matrices(1.0);
        let up_model = view_model * Vec4::new(0.0, 0.0, 1.0, 1.0);
        let origin = view_model * Vec4::new(0.0, 0.0, 0.0, 1.0);
        // rotateX(-PI/2) sends +Z to +Y in world; +Y is up on screen, so the
        // transformed point sits higher (greater view-space y) than the origin.
        assert!(up_model.y > origin.y);
    }

    /// Fog color is v4's #10101f in linear space.
    #[test]
    fn fog_color_is_linear_10101f() {
        let fog = flame_fog_color();
        let expect = Color::srgb_u8(0x10, 0x10, 0x1f).to_linear();
        assert!((fog.x - expect.red).abs() < 1e-6);
        assert!((fog.y - expect.green).abs() < 1e-6);
        assert!((fog.z - expect.blue).abs() < 1e-6);
    }
}
