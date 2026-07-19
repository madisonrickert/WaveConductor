//! `OnEnter(AppState::Radiance)` spawn plus the `OnExit` teardown.
//!
//! Allocates the particle storage buffer (zeroed = all dead; the kernel's
//! edge-respawn births every particle), the billboard mesh (count × 6
//! vertices, data unused — the vertex shader derives everything from
//! `vertex_index`), the silhouette quad, and the sim resources; inserts the
//! Plan A/B activation requests. On exit everything is dropped, the requests
//! are removed (stopping the mic stream and the body worker), and the
//! render-world `RadianceSimParams` copy dies via the compute plugin's
//! removal companion.
//!
//! This module (and everything it spawns/consumes) is gated behind the
//! `body-tracking-mediapipe` feature: it needs `MaskTexture`/
//! `SilhouetteEdges`/`BodyTrackingRequest` (wc-core gates the whole
//! `wc_core::input::body` module behind this name) and
//! `RadianceSilhouetteMaterial`/`RadianceState` (gated the same way one
//! layer up — see `radiance::render` and `radiance::systems::sim_params`).
//! The `cargo doc` gate builds default features only, so this module must be
//! absent there — see `Cargo.toml`'s `body-tracking-mediapipe` forwarding
//! feature, and `radiance::compute::mod`/`radiance::render` for the
//! identical precedent.

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::mesh::PrimitiveTopology;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::storage::ShaderBuffer;
use bytemuck::{cast_slice, Zeroable};
use wc_core::audio::input::AudioCaptureRequest;
use wc_core::input::body::{
    BodyTrackingRequest, MaskTexture, SilhouetteEdges, MASK_SIZE, MASK_SIZE_U32, MAX_EDGE_POINTS,
    MAX_TRACKED_BODIES,
};

use crate::radiance::compute::sim_params::{
    RadianceParticle, RadianceSimParams, RadianceSimParamsGpu,
};
use crate::radiance::distance_field::RadianceDistanceField;
use crate::radiance::pulse::{RadiancePulseMaterial, RadiancePulses};
use crate::radiance::render::{
    silhouette_fill_color, RadianceMaterial, RadianceSilhouetteMaterial, QUAD_HALF_PX,
};
use crate::radiance::settings::RadianceSettings;
use crate::radiance::sparkle::{RadianceSparkleMaterial, RadianceSparkles};
use crate::radiance::systems::sim_params::RadianceState;

/// Marker component on every entity owned by the Radiance sketch;
/// `OnExit(AppState::Radiance)` despawns everything tagged with it.
#[derive(Component)]
pub struct RadianceRoot;

/// Ensure the Plan B mask + edge resources exist (init-if-absent).
///
/// With the body-tracking plugin present these already exist and this is a
/// no-op; in headless tests, feature-reduced harnesses, and the synthetic
/// capture path this creates the same shapes so the silhouette material, the
/// phantom, and the edge upload always have a target. Runs first in the
/// `OnEnter` chain.
pub fn ensure_body_surfaces(
    mask: Option<Res<'_, MaskTexture>>,
    edges: Option<Res<'_, SilhouetteEdges>>,
    mut images: ResMut<'_, Assets<Image>>,
    mut commands: Commands<'_, '_>,
) {
    if mask.is_none() {
        // Rgba8Unorm per the pinned multi-body channel convention (channel i
        // = body slot i); matches wc-core's init_mask_texture.
        let image = Image::new_fill(
            Extent3d {
                width: u32::try_from(MASK_SIZE).unwrap_or(256),
                height: u32::try_from(MASK_SIZE).unwrap_or(256),
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[0u8, 0, 0, 0],
            TextureFormat::Rgba8Unorm,
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        );
        commands.insert_resource(MaskTexture(images.add(image)));
    }
    if edges.is_none() {
        commands.insert_resource(SilhouetteEdges {
            points: Vec::with_capacity(MAX_EDGE_POINTS),
            slot_counts: [0; MAX_TRACKED_BODIES],
            generation: 0,
        });
    }
}

/// `OnEnter(AppState::Radiance)`: allocate the buffers, spawn the two draw
/// entities, insert the sim resources.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "particle_count is bounded by the 10k..300k settings slider, exact as f32"
)]
#[allow(
    clippy::too_many_arguments,
    reason = "a Bevy spawn system's parameters are its data dependencies; the \
              beat-pulse + sparkle quads add their material Assets and the \
              distance-field image store"
)]
pub fn spawn_radiance(
    settings: Res<'_, RadianceSettings>,
    mask: Res<'_, MaskTexture>,
    mut buffers: ResMut<'_, Assets<ShaderBuffer>>,
    mut meshes: ResMut<'_, Assets<Mesh>>,
    mut images: ResMut<'_, Assets<Image>>,
    mut particle_materials: ResMut<'_, Assets<RadianceMaterial>>,
    mut silhouette_materials: ResMut<'_, Assets<RadianceSilhouetteMaterial>>,
    mut pulse_materials: ResMut<'_, Assets<RadiancePulseMaterial>>,
    mut sparkle_materials: ResMut<'_, Assets<RadianceSparkleMaterial>>,
    window: Single<'_, '_, &Window>,
    mut commands: Commands<'_, '_>,
) {
    let count = settings.particle_count.clamp(1_000.0, 300_000.0) as u32;
    let capacity = count as usize;

    // Zeroed = all dead; the kernel births every particle at the edge list.
    // RENDER_WORLD-only: the CPU never rewrites it after this seed.
    let particles = vec![RadianceParticle::zeroed(); capacity];
    let particles_handle = buffers.add(ShaderBuffer::new(
        cast_slice::<RadianceParticle, u8>(&particles),
        RenderAssetUsages::RENDER_WORLD,
    ));

    // Billboard mesh: count × 6 origin vertices; only the draw count matters
    // (the flame/particles idiom).
    let positions: Vec<[f32; 3]> = vec![[0.0, 0.0, 0.0]; capacity * 6];
    let mut billboard_mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    billboard_mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    let billboard_mesh_handle = meshes.add(billboard_mesh);

    let w = window.width().max(1.0);
    let h = window.height().max(1.0);

    // One-frame placeholder uniforms; drive_radiance_materials overwrites
    // every lane next Update.
    let particle_material = particle_materials.add(placeholder_particle_material(
        &settings,
        particles_handle.clone(),
    ));
    let silhouette_material =
        silhouette_materials.add(placeholder_silhouette_material(&settings, mask.0.clone()));

    // Silhouette quad under (z 0.0) the billboards (z 1.0) in Transparent2d's
    // z-sort.
    commands.spawn((
        RadianceRoot,
        bevy::mesh::Mesh2d(meshes.add(Mesh::from(Rectangle::new(w, h)))),
        bevy::sprite_render::MeshMaterial2d(silhouette_material),
        Transform::from_xyz(0.0, 0.0, 0.0),
        GlobalTransform::default(),
        Visibility::default(),
    ));
    commands.spawn((
        RadianceRoot,
        bevy::mesh::Mesh2d(billboard_mesh_handle),
        bevy::sprite_render::MeshMaterial2d(particle_material),
        Transform::from_xyz(0.0, 0.0, 1.0),
        GlobalTransform::default(),
        Visibility::default(),
    ));
    // Silhouette distance field: R8Unorm 256², seeded saturated (255 = no
    // body anywhere) so waves are invisible until the first real body frame
    // computes the field. MAIN_WORLD (CPU chamfer writes) + RENDER_WORLD
    // (shader samples); Bevy re-uploads on mutation.
    let distance_image = images.add(Image::new_fill(
        Extent3d {
            width: MASK_SIZE_U32,
            height: MASK_SIZE_U32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[255u8],
        TextureFormat::R8Unorm,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    ));

    // Beat-pulse wave quad over the billboards (z 2.0, additive — light
    // washes over silhouette and aura alike). Spawns with all slots dead;
    // `pulse::update_radiance_pulses` packs the live uniform every frame.
    commands.spawn((
        RadianceRoot,
        bevy::mesh::Mesh2d(meshes.add(Mesh::from(Rectangle::new(w, h)))),
        bevy::sprite_render::MeshMaterial2d(pulse_materials.add(RadiancePulseMaterial {
            distance_field: distance_image.clone(),
            pulses: crate::radiance::pulse::RadiancePulseUniform::default(),
        })),
        Transform::from_xyz(0.0, 0.0, 2.0),
        GlobalTransform::default(),
        Visibility::default(),
    ));
    // Extremity-sparkle quad above the waves (z 3.0, additive). Both
    // sparkles spawn off; `sparkle::update_radiance_sparkles` drives them.
    commands.spawn((
        RadianceRoot,
        bevy::mesh::Mesh2d(meshes.add(Mesh::from(Rectangle::new(w, h)))),
        bevy::sprite_render::MeshMaterial2d(
            sparkle_materials.add(RadianceSparkleMaterial::default()),
        ),
        Transform::from_xyz(0.0, 0.0, 3.0),
        GlobalTransform::default(),
        Visibility::default(),
    ));

    // Zeroed params (emission 0, no edges) until the first bake next Update —
    // EXCEPT `particle_count`, which the baker deliberately never touches
    // (it is buffer topology, owned here): the kernel guards with
    // `min(arrayLength, params.particle_count)`, so leaving it zeroed makes
    // every invocation early-return and the aura never simulates. (That was
    // a real shipped bug: the uniform lane stayed 0 forever and the flame
    // layer was silently dead.)
    let mut params = RadianceSimParamsGpu::zeroed();
    params.particle_count = count;
    commands.insert_resource(RadianceSimParams {
        params,
        particles: particles_handle,
        particle_count: count,
    });
    commands.insert_resource(RadianceState::default());
    commands.insert_resource(RadiancePulses::default());
    commands.insert_resource(RadianceSparkles::default());
    commands.insert_resource(RadianceDistanceField::new(distance_image));
}

/// The first-frame particle material: base palette identity at phase 0 so
/// the first rendered frame is already on-identity before the driver's
/// first pack.
fn placeholder_particle_material(
    settings: &RadianceSettings,
    particles: Handle<ShaderBuffer>,
) -> RadianceMaterial {
    let slot_colors = crate::radiance::render::slot_identity_colors(
        settings.palette,
        0.0,
        settings.hue_spread,
        0.0,
    );
    RadianceMaterial {
        particles,
        aura: crate::radiance::render::RadianceAuraUniform {
            params: Vec4::new(0.55, QUAD_HALF_PX, 0.0, 0.0),
            slot_colors,
        },
    }
}

/// The first-frame silhouette material: slot 0 faded in (the
/// synthetic/phantom writers' slot) until the driver retargets the fade
/// vector at the live tracking state.
fn placeholder_silhouette_material(
    settings: &RadianceSettings,
    mask: Handle<Image>,
) -> RadianceSilhouetteMaterial {
    let slot_colors = crate::radiance::render::slot_identity_colors(
        settings.palette,
        0.0,
        settings.hue_spread,
        0.0,
    );
    RadianceSilhouetteMaterial {
        mask,
        fill_params: Vec4::new(
            settings.silhouette_fill,
            settings.rim_glow,
            settings.mask_threshold,
            f32::from(u8::from(settings.mirror)),
        ),
        effect_params: Vec4::ZERO,
        fill_color: silhouette_fill_color(),
        slots: crate::radiance::render::RadianceSilhouetteSlots {
            rim_colors: slot_colors,
            fades: Vec4::new(1.0, 0.0, 0.0, 0.0),
        },
    }
}

/// `OnEnter(AppState::Radiance)` (chained after `spawn_radiance`): start the
/// mic capture + body tracking via the Plan A/B activation contracts.
///
/// Under `WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_BODY` (debug builds only) this
/// early-returns without inserting either request: the capture dancer
/// (`systems::debug::drive_synthetic_body`) writes `BodyTrackingState`/
/// `AudioAnalysis` directly every frame, so a capture run under this toggle
/// must never open the mic or the camera. `activity::pause_tracking_requests`/
/// `resume_tracking_requests` already treat both requests as optional for
/// exactly this reason.
pub fn insert_tracking_requests(
    settings: Res<'_, RadianceSettings>,
    mut commands: Commands<'_, '_>,
    // Optional debug toggles (present only when a `WC_DEBUG_*` var is set, and
    // only in debug builds). Placed last so the release signature is unchanged.
    #[cfg(debug_assertions)] toggles: Option<Res<'_, wc_core::debug::DebugToggles>>,
) {
    #[cfg(debug_assertions)]
    if toggles.is_some_and(|t| t.force_radiance_synthetic_body) {
        return;
    }

    let device = settings.audio_input_device.trim();
    commands.insert_resource(AudioCaptureRequest {
        device_name: if device.is_empty() {
            None
        } else {
            Some(device.to_owned())
        },
        paused: false,
    });
    commands.insert_resource(BodyTrackingRequest {
        idle_throttle: false,
        mask_ema: settings.mask_ema,
        one_euro_min_cutoff: settings.one_euro_min_cutoff,
        one_euro_beta: settings.one_euro_beta,
    });
}

/// `OnExit(AppState::Radiance)`: drop the sim resources (releasing the
/// particle buffer's VRAM via its sole handle) and stop capture/tracking by
/// removing the activation requests (their contract: remove to stop).
pub fn remove_radiance_resources(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<RadianceSimParams>();
    commands.remove_resource::<RadianceState>();
    commands.remove_resource::<RadiancePulses>();
    commands.remove_resource::<RadianceSparkles>();
    commands.remove_resource::<RadianceDistanceField>();
    commands.remove_resource::<AudioCaptureRequest>();
    commands.remove_resource::<BodyTrackingRequest>();
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<ShaderBuffer>();
        app.init_asset::<Mesh>();
        app.init_asset::<Image>();
        app.init_asset::<RadianceMaterial>();
        app.init_asset::<RadianceSilhouetteMaterial>();
        app.init_asset::<RadiancePulseMaterial>();
        app.init_asset::<RadianceSparkleMaterial>();
        app.world_mut().spawn(Window::default());
        app.insert_resource(RadianceSettings::default());
        app
    }

    /// `ensure_body_surfaces` creates the mask + edges when absent and
    /// leaves an existing pair untouched.
    #[test]
    fn ensure_body_surfaces_is_init_if_absent() {
        let mut app = test_app();
        app.world_mut()
            .run_system_once(ensure_body_surfaces)
            .expect("runs");
        let first = app.world().resource::<MaskTexture>().0.clone();
        assert!(app.world().get_resource::<SilhouetteEdges>().is_some());
        app.world_mut()
            .run_system_once(ensure_body_surfaces)
            .expect("runs again");
        assert_eq!(
            app.world().resource::<MaskTexture>().0,
            first,
            "existing mask must not be replaced"
        );
    }

    /// Spawn sizes the buffer + mesh from the setting, inserts the sim
    /// resources zeroed, and teardown drops them plus both requests.
    #[test]
    fn spawn_sizes_buffers_and_teardown_drops_resources() {
        let mut app = test_app();
        app.world_mut()
            .resource_mut::<RadianceSettings>()
            .particle_count = 12_000.0;
        app.world_mut()
            .run_system_once(ensure_body_surfaces)
            .expect("surfaces");
        app.world_mut()
            .run_system_once(spawn_radiance)
            .expect("spawn runs");

        let sim = app.world().resource::<RadianceSimParams>();
        assert_eq!(sim.particle_count, 12_000);
        assert_eq!(
            sim.params.particle_count, 12_000,
            "the GPU uniform lane must carry the buffer length too — the \
             kernel guards with min(arrayLength, params.particle_count), so a \
             zeroed lane silently kills the whole simulation"
        );
        assert!(
            sim.params.emission_prob.abs() < f32::EPSILON,
            "zeroed until first bake"
        );
        let handle = sim.particles.clone();
        let buffers = app.world().resource::<Assets<ShaderBuffer>>();
        let buffer = buffers.get(&handle).expect("particle buffer present");
        let data = buffer.data.as_ref().expect("cpu seed present");
        assert_eq!(data.len(), 12_000 * 32, "32-byte particles at full count");
        assert!(data.iter().all(|&b| b == 0), "zeroed = all dead");

        // Four draw entities (silhouette + billboards + pulse quad +
        // sparkle quad) under the marker.
        let mut roots = app
            .world_mut()
            .query_filtered::<Entity, With<RadianceRoot>>();
        assert_eq!(roots.iter(app.world()).count(), 4);
        assert!(
            app.world().get_resource::<RadiancePulses>().is_some(),
            "pulse state inserted at spawn"
        );
        assert!(
            app.world().get_resource::<RadianceSparkles>().is_some(),
            "sparkle state inserted at spawn"
        );
        assert!(
            app.world()
                .get_resource::<RadianceDistanceField>()
                .is_some(),
            "distance field inserted at spawn"
        );

        app.world_mut()
            .run_system_once(insert_tracking_requests)
            .expect("requests");
        assert!(app.world().get_resource::<AudioCaptureRequest>().is_some());
        assert!(app.world().get_resource::<BodyTrackingRequest>().is_some());

        app.world_mut()
            .run_system_once(remove_radiance_resources)
            .expect("teardown");
        assert!(app.world().get_resource::<RadianceSimParams>().is_none());
        assert!(app.world().get_resource::<RadianceState>().is_none());
        assert!(app.world().get_resource::<RadiancePulses>().is_none());
        assert!(app.world().get_resource::<RadianceSparkles>().is_none());
        assert!(app
            .world()
            .get_resource::<RadianceDistanceField>()
            .is_none());
        assert!(app.world().get_resource::<AudioCaptureRequest>().is_none());
        assert!(app.world().get_resource::<BodyTrackingRequest>().is_none());
    }

    /// The device name maps empty → system default (None), trimmed → Some.
    #[test]
    fn request_maps_device_name() {
        let mut app = test_app();
        app.world_mut()
            .resource_mut::<RadianceSettings>()
            .audio_input_device = "  USB Interface  ".to_owned();
        app.world_mut()
            .run_system_once(insert_tracking_requests)
            .expect("requests");
        let req = app.world().resource::<AudioCaptureRequest>();
        assert_eq!(req.device_name.as_deref(), Some("USB Interface"));
        assert!(!req.paused);
    }

    /// The body request carries the three Dev tuning fields straight from
    /// settings. They are live end to end:
    /// `wc_core::input::body::systems::start_worker` seeds the worker's
    /// `BodyLiveTuning` from `mask_ema` and the `BodySmoother` from
    /// `one_euro_min_cutoff` / `one_euro_beta` on start.
    #[test]
    fn request_carries_tuning_fields_from_settings() {
        let mut app = test_app();
        {
            let mut settings = app.world_mut().resource_mut::<RadianceSettings>();
            settings.mask_ema = 0.42;
            settings.one_euro_min_cutoff = 2.5;
            settings.one_euro_beta = 0.11;
        }
        app.world_mut()
            .run_system_once(insert_tracking_requests)
            .expect("requests");
        let req = app.world().resource::<BodyTrackingRequest>();
        assert!((req.mask_ema - 0.42).abs() < f32::EPSILON);
        assert!((req.one_euro_min_cutoff - 2.5).abs() < f32::EPSILON);
        assert!((req.one_euro_beta - 0.11).abs() < f32::EPSILON);
    }

    /// `WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_BODY` suppresses BOTH activation
    /// requests — a capture run under the toggle must never open the mic or
    /// the camera. The synthetic dancer writes the tracking/analysis
    /// resources directly instead (Task 13).
    #[test]
    #[cfg(debug_assertions)]
    fn synthetic_body_toggle_suppresses_both_requests() {
        let mut app = test_app();
        app.insert_resource(wc_core::debug::DebugToggles {
            force_g: None,
            disable_smear: false,
            disable_explode: false,
            disable_heatmap_refine: false,
            disable_bloom: false,
            disable_bone_composite: false,
            disable_bone_camera: false,
            solid_particles: None,
            force_screensaver: false,
            force_tier: None,
            force_cymatics_interaction: false,
            force_flame_warp: false,
            force_flame_camera_pose: false,
            force_radiance_synthetic_body: true,
            force_radiance_synthetic_duo: false,
        });
        app.world_mut()
            .run_system_once(insert_tracking_requests)
            .expect("requests (early-out)");
        assert!(
            app.world().get_resource::<AudioCaptureRequest>().is_none(),
            "toggle must suppress the mic request"
        );
        assert!(
            app.world().get_resource::<BodyTrackingRequest>().is_none(),
            "toggle must suppress the camera/body request"
        );
    }
}
