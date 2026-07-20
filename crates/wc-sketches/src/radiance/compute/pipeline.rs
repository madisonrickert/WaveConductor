//! Render-world compute plugin for the Radiance aura.
//!
//! # Signal / data flow
//!
//! 1. `ExtractResourcePlugin` clones [`RadianceSimParams`] (POD + one
//!    `Handle`, memcpy clone) from the main world each frame;
//!    `remove_radiance_sim_params_if_absent` mirrors removals the plugin
//!    does not propagate (the established landmine — see
//!    `particles/compute.rs`).
//! 2. [`extract_silhouette_edges`] copies the edge list generation-gated
//!    (see `edge_upload`).
//! 3. `init_radiance_pipeline` (`RenderStartup`) builds the bind-group
//!    layout, queues the compute pipeline, and allocates the persistent
//!    uniform buffer (400 B `SimParams`) and the persistent edge storage
//!    buffer (`MAX_EDGE_POINTS` × 16 B) once — never per frame.
//! 4. `prepare_radiance_bind_group` (`PrepareBindGroups`, after the edge
//!    upload) writes this frame's uniforms and builds (or reuses) the single
//!    bind group, cached in [`RadianceBindGroupCache`] keyed on the particle
//!    buffer's [`BufferId`] (bounded by construction: one slot, replaced on
//!    change, and *cleared* by the removal companion on sketch exit so the
//!    freed particle buffer's `Arc` is not pinned for the rest of the
//!    session).
//! 5. `radiance_compute` dispatches `ceil(particle_count / 64)` workgroups in
//!    the root `RenderGraph` schedule before `camera_driver`, so the buffer
//!    is current before the 2D pass draws it. Dispatch scales with the
//!    particle-count setting; no unused workgroups.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "u32/u64/usize casts for GPU buffer sizes are intentional and \
              bounds-checked (MAX_EDGE_POINTS and the 300k particle cap)"
)]

use std::borrow::Cow;
use std::num::NonZeroU64;

use bevy::core_pipeline::schedule::camera_driver;
use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResourcePlugin;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    BindGroup, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
    BindingType, Buffer, BufferBindingType, BufferDescriptor, BufferId, BufferUsages,
    CachedComputePipelineId, ComputePassDescriptor, ComputePipelineDescriptor, PipelineCache,
    ShaderStages,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderGraph, RenderQueue};
use bevy::render::storage::GpuShaderBuffer;
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderStartup, RenderSystems};
use wc_core::input::body::{EdgePoint, MAX_EDGE_POINTS};

use super::edge_upload::{extract_silhouette_edges, upload_silhouette_edges, ExtractedEdges};
use super::sim_params::{RadianceSimParams, RadianceSimParamsGpu};

/// Workgroup width; must match `@workgroup_size(64)` in
/// `assets/shaders/radiance/simulate.wgsl`.
const WORKGROUP_SIZE: u32 = 64;

/// `RadianceSimParamsGpu` byte size (400) for binding 0's `min_binding_size`.
/// The `panic!` is inside a `const`, so a zero-sized regression fails at
/// compile time.
const SIM_PARAMS_SIZE: NonZeroU64 =
    match NonZeroU64::new(std::mem::size_of::<RadianceSimParamsGpu>() as u64) {
        Some(n) => n,
        None => panic!("RadianceSimParamsGpu must be non-zero-sized"),
    };

/// Full-capacity edge buffer size in bytes (`MAX_EDGE_POINTS` × 16).
const EDGES_BUFFER_SIZE: u64 = (MAX_EDGE_POINTS * std::mem::size_of::<EdgePoint>()) as u64;

/// Registers extraction (+ removal companion), the edge upload, pipeline
/// init, per-frame prepare, and the dispatch for the Radiance aura.
///
/// `Plugin` singleton — added exactly once by `SketchesPlugin`. Inert until
/// the sketch inserts [`RadianceSimParams`] on entry, so it costs nothing on
/// other sketches.
pub struct RadianceComputePlugin;

impl Plugin for RadianceComputePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ExtractResourcePlugin::<RadianceSimParams>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app.init_resource::<ExtractedEdges>();
        render_app.init_resource::<RadianceBindGroupCache>();
        render_app.add_systems(
            ExtractSchedule,
            (
                remove_radiance_sim_params_if_absent,
                extract_silhouette_edges,
            ),
        );

        render_app
            .add_systems(RenderStartup, init_radiance_pipeline)
            .add_systems(
                Render,
                (
                    upload_silhouette_edges,
                    prepare_radiance_bind_group.run_if(resource_exists::<RadianceSimParams>),
                )
                    .chain()
                    .in_set(RenderSystems::PrepareBindGroups),
            );

        // Dispatch before camera_driver so the 2D pass reads updated
        // particles (Bevy 0.19 systems-based render graph).
        render_app.add_systems(RenderGraph, radiance_compute.before(camera_driver));
    }
}

/// Cached compute pipeline state. Initialised once in `RenderStartup`.
#[derive(Resource)]
pub struct RadiancePipeline {
    /// Retained so the prepare system can fetch the [`BindGroupLayout`] from
    /// the [`PipelineCache`] without storing it twice.
    bind_group_layout_descriptor: BindGroupLayoutDescriptor,
    /// Handle into Bevy's [`PipelineCache`].
    pipeline_id: CachedComputePipelineId,
    /// Persistent `UNIFORM | COPY_DST` buffer for the 400-byte sim params;
    /// refilled each frame via `write_buffer` (no realloc).
    sim_params_buffer: Buffer,
    /// Persistent `STORAGE | COPY_DST` buffer of `MAX_EDGE_POINTS` edge
    /// points; refilled generation-gated by `edge_upload` (stable
    /// `BufferId`, so it never churns the bind-group cache).
    pub edges_buffer: Buffer,
}

/// Per-frame bind group + dispatch size, consumed by `radiance_compute`.
/// Removed by `remove_radiance_sim_params_if_absent` (private, so a code
/// span) on sketch exit — the held [`BindGroup`] owns an `Arc` reference to
/// the particle buffer, so letting it linger would pin the freed buffer's
/// VRAM for the session.
#[derive(Resource)]
pub struct RadianceComputeBindGroup {
    /// sim uniform (0), particle storage rw (1), edge storage ro (2).
    bind_group: BindGroup,
    /// `ceil(particle_count / WORKGROUP_SIZE)`.
    dispatch_size: u32,
}

/// One-slot bind-group cache keyed on the particle buffer's [`BufferId`].
///
/// A render-world `Resource` (not a system `Local`) deliberately: the prepare
/// system stops running once its `run_if(resource_exists::<RadianceSimParams>)`
/// gate goes false on sketch exit, so a `Local` slot could never release the
/// old bind group — pinning the freed particle buffer's `Arc`. As a resource,
/// `remove_radiance_sim_params_if_absent` (private, so a code span) clears
/// it on the same exit seam.
#[derive(Resource, Default)]
pub struct RadianceBindGroupCache(Option<(BufferId, BindGroup)>);

/// Initialises [`RadiancePipeline`] in the render-world startup schedule.
fn init_radiance_pipeline(
    mut commands: Commands<'_, '_>,
    asset_server: Res<'_, AssetServer>,
    pipeline_cache: Res<'_, PipelineCache>,
    render_device: Res<'_, RenderDevice>,
) {
    let bind_group_layout_descriptor = BindGroupLayoutDescriptor::new(
        "radiance_compute_bgl",
        &[
            // binding 0 — SimParams uniform.
            BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::COMPUTE,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: Some(SIM_PARAMS_SIZE),
                },
                count: None,
            },
            // binding 1 — Particle storage, read_write.
            BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::COMPUTE,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // binding 2 — EdgePoint storage, read-only.
            BindGroupLayoutEntry {
                binding: 2,
                visibility: ShaderStages::COMPUTE,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    );

    let shader = asset_server.load::<bevy::shader::Shader>("shaders/radiance/simulate.wgsl");

    let pipeline_id = pipeline_cache.queue_compute_pipeline(ComputePipelineDescriptor {
        label: Some(Cow::from("radiance_compute_pipeline")),
        layout: vec![bind_group_layout_descriptor.clone()],
        shader,
        entry_point: Some(Cow::from("main")),
        ..default()
    });

    // Both persistent buffers allocated once; refilled via write_buffer.
    let sim_params_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("radiance_sim_params_uniform"),
        size: std::mem::size_of::<RadianceSimParamsGpu>() as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let edges_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("radiance_silhouette_edges"),
        size: EDGES_BUFFER_SIZE,
        usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    commands.insert_resource(RadiancePipeline {
        bind_group_layout_descriptor,
        pipeline_id,
        sim_params_buffer,
        edges_buffer,
    });
}

/// Uploads this frame's uniforms and builds (or reuses) the compute bind
/// group.
///
/// ## Bind-group caching (always-on compute hot path)
///
/// The sim uniform and edge buffers are pipeline-owned and live for the
/// process; the particle storage buffer is recreated per sketch entry, so
/// [`RadianceBindGroupCache`] keys on its [`BufferId`] and replaces its
/// single slot on change (dropping the old bind group releases the freed
/// buffer's reference). On sketch exit — when this system's `run_if` gate
/// stops it running — the removal companion clears the cache, so the last
/// buffer is never retained across re-entry (bounded by construction).
fn prepare_radiance_bind_group(
    mut commands: Commands<'_, '_>,
    render_device: Res<'_, RenderDevice>,
    render_queue: Res<'_, RenderQueue>,
    pipeline_cache: Res<'_, PipelineCache>,
    sim: Res<'_, RadianceSimParams>,
    buffers: Res<'_, RenderAssets<GpuShaderBuffer>>,
    pipeline: Option<Res<'_, RadiancePipeline>>,
    mut cached: ResMut<'_, RadianceBindGroupCache>,
) {
    let Some(pipeline) = pipeline else {
        return;
    };
    let Some(particle_buffer) = buffers.get(&sim.particles) else {
        return;
    };

    // Staged copy — no allocation after init.
    render_queue.0.write_buffer(
        &pipeline.sim_params_buffer,
        0,
        bytemuck::bytes_of(&sim.params),
    );

    let buffer_id = particle_buffer.buffer.id();
    let bind_group = match &cached.0 {
        Some((id, bg)) if *id == buffer_id => bg.clone(),
        _ => {
            let layout: BindGroupLayout =
                pipeline_cache.get_bind_group_layout(&pipeline.bind_group_layout_descriptor);
            let bg = render_device.create_bind_group(
                "radiance_compute_bind_group",
                &layout,
                &[
                    BindGroupEntry {
                        binding: 0,
                        resource: pipeline.sim_params_buffer.as_entire_binding(),
                    },
                    BindGroupEntry {
                        binding: 1,
                        resource: particle_buffer.buffer.as_entire_binding(),
                    },
                    BindGroupEntry {
                        binding: 2,
                        resource: pipeline.edges_buffer.as_entire_binding(),
                    },
                ],
            );
            cached.0 = Some((buffer_id, bg.clone()));
            bg
        }
    };

    // A paused sim (Idle, field deterministically all-dead — see
    // `systems::sim_params::update_radiance_pause`) dispatches nothing.
    let dispatch_size = if sim.paused {
        0
    } else {
        sim.particle_count.div_ceil(WORKGROUP_SIZE)
    };
    commands.insert_resource(RadianceComputeBindGroup {
        bind_group,
        dispatch_size,
    });
}

/// Render system dispatching the aura kernel each frame.
///
/// Gates directly on [`RadianceSimParams`] (mirroring `particle_compute`).
/// [`remove_radiance_sim_params_if_absent`] removes both that resource and
/// the [`RadianceComputeBindGroup`] on `OnExit`; the `Option` guards here
/// keep the dispatch a no-op for the one extract cycle before those removals
/// land (and while the pipeline is still compiling).
fn radiance_compute(
    bind_group: Option<Res<'_, RadianceComputeBindGroup>>,
    pipeline_res: Option<Res<'_, RadiancePipeline>>,
    sim: Option<Res<'_, RadianceSimParams>>,
    pipeline_cache: Res<'_, PipelineCache>,
    mut render_context: RenderContext<'_, '_>,
) {
    if sim.is_none() {
        return;
    }
    let Some(bg) = bind_group else {
        return;
    };
    let Some(pipeline_res) = pipeline_res else {
        return;
    };
    let Some(compute_pipeline) = pipeline_cache.get_compute_pipeline(pipeline_res.pipeline_id)
    else {
        return;
    };
    // Paused (Idle all-dead field): skip the pass entirely rather than
    // encoding a zero-workgroup dispatch.
    if bg.dispatch_size == 0 {
        return;
    }

    let mut pass = render_context
        .command_encoder()
        .begin_compute_pass(&ComputePassDescriptor {
            label: Some("radiance_compute_pass"),
            timestamp_writes: None,
        });
    pass.set_pipeline(compute_pipeline);
    pass.set_bind_group(0, &bg.bind_group, &[]);
    pass.dispatch_workgroups(bg.dispatch_size, 1, 1);
}

/// Removes the render-world Radiance compute resources when the main-world
/// [`RadianceSimParams`] is absent (`ExtractResourcePlugin` does not
/// propagate removals — the established landmine; mirrors
/// `remove_particle_sim_params_if_absent`).
///
/// Besides the extracted [`RadianceSimParams`] copy, this also drops the
/// per-frame [`RadianceComputeBindGroup`] and clears the
/// [`RadianceBindGroupCache`] slot: both hold a [`BindGroup`] whose `Arc`
/// references pin the sketch's freed particle buffer (3.84 MB at the default
/// count) in VRAM, and neither is entity-owned nor re-run once the prepare
/// system's `run_if` gate goes false — without this, the buffer would be
/// retained for the rest of the session (AGENTS.md GPU-release mechanism 2/3).
fn remove_radiance_sim_params_if_absent(
    mut commands: Commands<'_, '_>,
    main_resource: Extract<'_, '_, Option<Res<'_, RadianceSimParams>>>,
    render_resource: Option<Res<'_, RadianceSimParams>>,
    bind_group: Option<Res<'_, RadianceComputeBindGroup>>,
    mut cache: ResMut<'_, RadianceBindGroupCache>,
) {
    if main_resource.is_some() {
        return;
    }
    if render_resource.is_some() {
        commands.remove_resource::<RadianceSimParams>();
    }
    if bind_group.is_some() {
        commands.remove_resource::<RadianceComputeBindGroup>();
    }
    if cache.0.is_some() {
        cache.0 = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build-smoke: the plugin adds cleanly under `MinimalPlugins` (no
    /// `RenderApp`) without panicking — `build` early-returns, mirroring
    /// `flame_compute_plugin_builds`.
    #[test]
    fn radiance_compute_plugin_builds() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(RadianceComputePlugin);
        app.update();
    }

    /// Binding 0's `min_binding_size` is the exact 400-byte layout, and the
    /// edge buffer holds the full contract capacity.
    #[test]
    fn buffer_size_constants_match_contracts() {
        assert_eq!(SIM_PARAMS_SIZE.get(), 400);
        assert_eq!(
            EDGES_BUFFER_SIZE,
            (MAX_EDGE_POINTS as u64) * 16,
            "EdgePoint stride is 16 bytes by the pinned contract"
        );
    }

    /// Dispatch math rounds up so the last partial workgroup still launches.
    #[test]
    fn dispatch_workgroups_round_up() {
        assert_eq!(63_u32.div_ceil(WORKGROUP_SIZE), 1);
        assert_eq!(64_u32.div_ceil(WORKGROUP_SIZE), 1);
        assert_eq!(65_u32.div_ceil(WORKGROUP_SIZE), 2);
    }

    /// The removal companion clears every render-world compute resource when
    /// the main-world source is absent, and leaves them alone while it is
    /// present — this is the seam that releases the particle buffer's VRAM
    /// on sketch exit.
    ///
    /// [`RadianceComputeBindGroup`] and a populated
    /// [`RadianceBindGroupCache`] slot hold wgpu handles that cannot be
    /// constructed headless, so this test exercises the extracted-params
    /// removal and verifies the system runs cleanly with the (empty) cache;
    /// the bind-group/cache clears share the same `main_resource.is_none()`
    /// branch asserted here.
    #[test]
    #[allow(clippy::expect_used, reason = "test assertions")]
    fn removal_companion_clears_render_world_on_exit() {
        use bevy::ecs::system::RunSystemOnce;
        use bevy::render::MainWorld;

        let render_params = || RadianceSimParams {
            params: super::super::sim_params::RadianceSimParamsGpu::default(),
            particles: Handle::default(),
            particle_count: 1_000,
            paused: false,
            frozen_secs: 0.0,
        };

        // Main-world source absent: the render copy must be removed.
        let mut render_world = World::new();
        render_world.insert_resource(MainWorld::default());
        render_world.insert_resource(render_params());
        render_world.init_resource::<RadianceBindGroupCache>();
        render_world
            .run_system_once(remove_radiance_sim_params_if_absent)
            .expect("companion runs");
        assert!(
            render_world.get_resource::<RadianceSimParams>().is_none(),
            "extracted params must be dropped once the main source is gone"
        );
        assert!(
            render_world
                .resource::<RadianceBindGroupCache>()
                .0
                .is_none(),
            "cache slot stays empty after the clear branch"
        );

        // Main-world source present: everything is left in place.
        let mut render_world = World::new();
        let mut main_world = MainWorld::default();
        main_world.insert_resource(render_params());
        render_world.insert_resource(main_world);
        render_world.insert_resource(render_params());
        render_world.init_resource::<RadianceBindGroupCache>();
        render_world
            .run_system_once(remove_radiance_sim_params_if_absent)
            .expect("companion runs");
        assert!(
            render_world.get_resource::<RadianceSimParams>().is_some(),
            "live sketch must keep its extracted params"
        );
    }
}
