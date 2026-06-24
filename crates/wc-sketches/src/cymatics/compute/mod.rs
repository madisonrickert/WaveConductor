//! Cymatics compute pipeline: GPU types, texture allocation, and (in C6) the
//! render-world plugin that dispatches `simulate.wgsl` each frame.
//!
//! Data flow:
//! - [`sim_params::SimParamsGpu`] / [`sim_params::IterParamsGpu`] are the
//!   `#[repr(C)]` bytemuck types that map to the WGSL uniform structs.
//! - [`sim_params::CymaticsSimParams`] is extracted into the render world by
//!   `ExtractResourcePlugin` (registered by [`pipeline::CymaticsComputePlugin`]).
//! - [`create_cymatics_textures`] allocates the two ping-pong `rgba32float`
//!   storage textures (A and B) on sketch entry. The render material samples A
//!   directly — the odd-N continuity refresh keeps A current at frame end, so
//!   there is no separate display texture.
//! - [`pipeline::CymaticsComputePlugin`] is the render-graph node that advances
//!   the wave field each frame (the `simulate.wgsl` ping-pong dispatch).

pub mod pipeline;
pub mod sim_params;

pub use pipeline::CymaticsComputePlugin;
pub use sim_params::{
    CymaticsSimParams, CymaticsTextures, IterParamsGpu, SimParamsGpu, ITER_PARAMS_STRIDE,
    MAX_ITERATIONS,
};

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};

/// Build the two ping-pong textures at `width × height`.
///
/// A and B are `rgba32float` with `STORAGE_BINDING | TEXTURE_BINDING |
/// COPY_SRC | COPY_DST`: each plays both read and write roles across iterations.
/// `TEXTURE_BINDING` is what `textureLoad` needs — both the compute read path
/// and the render material (which samples A directly) read via `textureLoad`.
/// `COPY_DST` is required on A so the compute node can copy the freshest field
/// B → A after an odd sub-step count, restoring the cross-frame ping-pong
/// invariant ("A holds the latest state at frame end") that both the next
/// frame's read-A start and this frame's render-from-A rely on. `COPY_SRC`
/// stays on both so B can be the refresh copy source. A and B share one
/// descriptor, so the usages are symmetric (the refresh only ever copies B → A;
/// A's `COPY_SRC` and B's `COPY_DST` are unused but harmless). `rgba32float`
/// (not f16) preserves the small accumulated-height integration values.
///
/// # Early `rgba32float` support note
///
/// WebGPU mandates `rgba32float` storage support via the
/// `float32-filterable` feature, which all deployment targets (Metal,
/// Vulkan, DX12) expose. If the GPU rejects the storage binding the compute
/// pipeline compile (C6) will fail at `PipelineCache` time rather than here;
/// this function only allocates CPU-side `Image` descriptors. If you observe
/// a pipeline-compilation failure on a non-standard device, check whether the
/// adapter advertises `Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` or
/// inspect `adapter.features()` for `FLOAT32_FILTERABLE`.
pub fn create_cymatics_textures(
    width: u32,
    height: u32,
    images: &mut Assets<Image>,
) -> CymaticsTextures {
    let w = width.max(1);
    let h = height.max(1);
    let extent = Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };

    // Rgba32Float pixel: 4 × f32 = 16 bytes, all zeros (quiescent sim state).
    let zero = [0u8; 16];

    // Ping-pong A and B: storage + texture + copy-src/dst so each can be the
    // compute write target one iteration and the read source the next. COPY_DST
    // lets the node restore A from B after an odd sub-step count, so A holds the
    // latest state for the next frame's read-A start AND for this frame's
    // render (the material samples A directly via textureLoad — TEXTURE_BINDING).
    let mut ping = Image::new_fill(
        extent,
        TextureDimension::D2,
        &zero,
        TextureFormat::Rgba32Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    ping.texture_descriptor.usage = TextureUsages::STORAGE_BINDING
        | TextureUsages::TEXTURE_BINDING
        | TextureUsages::COPY_SRC
        | TextureUsages::COPY_DST;

    let a = images.add(ping.clone());
    let b = images.add(ping);

    CymaticsTextures { a, b }
}
