//! Cymatics compute pipeline: GPU types, texture allocation, and (in C6) the
//! render-world plugin that dispatches `simulate.wgsl` each frame.
//!
//! Data flow:
//! - [`sim_params::SimParamsGpu`] / [`sim_params::IterParamsGpu`] are the
//!   `#[repr(C)]` bytemuck types that map to the WGSL uniform structs.
//! - [`sim_params::CymaticsSimParams`] is extracted into the render world by
//!   `ExtractResourcePlugin` (registered in C6's plugin).
//! - [`create_cymatics_textures`] allocates the two ping-pong `rgba32float`
//!   storage textures (A and B) and the stable display texture on sketch entry.

pub mod sim_params;

pub use sim_params::{
    CymaticsSimParams, CymaticsTextures, IterParamsGpu, SimParamsGpu, ITER_PARAMS_STRIDE,
    MAX_ITERATIONS,
};

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};

/// Build the ping-pong + display textures at `width × height`.
///
/// A and B are `rgba32float` with `STORAGE_BINDING | TEXTURE_BINDING |
/// COPY_SRC`: each plays both read and write roles across iterations, and the
/// final output is the blit source. The display texture is
/// `TEXTURE_BINDING | COPY_DST` — sampled by the material, written by the
/// post-iteration blit. `rgba32float` (not f16) preserves the small
/// accumulated-height integration values.
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

    // Ping-pong A and B: storage + texture + copy-src so each can be the
    // compute write target one iteration and the read source the next, and the
    // last one written is the blit source for the display copy.
    let mut ping = Image::new_fill(
        extent,
        TextureDimension::D2,
        &zero,
        TextureFormat::Rgba32Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    ping.texture_descriptor.usage =
        TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC;

    // Display: receives the final blit (COPY_DST) and is sampled by the
    // material (TEXTURE_BINDING). No storage — it is never a compute target.
    let mut display = Image::new_fill(
        extent,
        TextureDimension::D2,
        &zero,
        TextureFormat::Rgba32Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    display.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;

    let a = images.add(ping.clone());
    let b = images.add(ping);
    let display = images.add(display);

    CymaticsTextures { a, b, display }
}
