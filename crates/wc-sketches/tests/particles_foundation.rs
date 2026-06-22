//! Smoke coverage for the shared particle foundation: the compute plugin builds
//! into an app, and the relocated public API is reachable at its new path.

use bevy::prelude::*;
use wc_sketches::particles::compute::ParticleComputePlugin;
use wc_sketches::particles::particle::{SimParams, MAX_ATTRACTORS};

#[test]
fn sim_params_layout_is_16_byte_aligned() {
    // Mirrors the in-module const asserts; guards the stationary_constant rename
    // from drifting the GPU layout.
    assert_eq!(std::mem::size_of::<SimParams>() % 16, 0);
    const { assert!(MAX_ATTRACTORS >= 1) };
}

#[test]
fn particle_compute_plugin_builds() {
    // ParticleComputePlugin no-ops cleanly without a RenderApp (it early-returns
    // when get_sub_app_mut(RenderApp) is None under MinimalPlugins), so adding it
    // must not panic.
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(ParticleComputePlugin);
    app.update();
}
