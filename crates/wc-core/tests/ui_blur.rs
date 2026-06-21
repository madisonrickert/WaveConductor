//! RenderApp-level tests for the backdrop-blur pipeline.
//!
//! These tests exercise [`BackdropBlurTexture`] allocation in the render world
//! and the [`BlurNodeRunCount`] proxy that indicates whether the
//! [`backdrop_blur`] would have executed its Kawase passes.
//!
//! ## Why these tests are `#[ignore]`
//!
//! `DefaultPlugins` includes `winit`, which requires the macOS event loop to
//! be created on the main thread. Cargo's test runner spawns each test on a
//! worker thread, causing an immediate panic on macOS. On Linux CI with a
//! virtual display this would work, but the runtime code is covered by
//! `cargo check` and the unit tests in `blur::tests`. To verify manually:
//!
//! ```text
//! cargo test -p wc-core --test ui_blur -- --ignored
//! cargo run -p waveconductor   # observe no shader errors in the log
//! ```
//!
//! [`backdrop_blur`]: wc_core::ui::blur::node::backdrop_blur

#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::expect_used,
    clippy::map_unwrap_or,
    reason = "test assertions and idiomatic Option handling in test code"
)]

use bevy::prelude::*;
use bevy::render::RenderApp;
use wc_core::ui::blur::{BackdropBlurEnabled, BackdropBlurPlugin, BackdropBlurTexture};

/// Build an app that includes the full render stack and `BackdropBlurPlugin`.
///
/// Uses `DefaultPlugins` so we get the real window extraction and render
/// device that `ensure_blur_texture` depends on.
fn make_render_app() -> App {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins);
    app.add_plugins(BackdropBlurPlugin);
    app
}

/// Verify that `BackdropBlurTexture` is inserted into the render world after
/// the first `App::update()` call.
///
/// Ignored: `DefaultPlugins` includes `winit`, which requires the macOS event
/// loop to be created on the main thread. Cargo's test runner spawns each test
/// on a worker thread, causing an immediate panic on macOS. On Linux CI with a
/// virtual display this would work, but the runtime code is covered by
/// `cargo check` and the unit test in `blur::tests`. To verify manually, run
/// the app with `cargo run -p waveconductor` and confirm the texture allocates.
#[test]
#[ignore = "winit requires the main thread on macOS; verify by running the app"]
fn backdrop_blur_texture_is_allocated_after_first_frame() {
    let mut app = make_render_app();
    app.update();
    let render_app = app.sub_app(RenderApp);
    let texture = render_app.world().resource::<BackdropBlurTexture>();
    assert!(
        texture.extent.x > 0 && texture.extent.y > 0,
        "blur texture extent should be non-zero after one frame, was {:?}",
        texture.extent
    );
}

/// Verify that [`BlurNodeRunCount`] is not incremented when
/// [`BackdropBlurEnabled`] is `false`.
///
/// The run-count proxy in [`prepare_blur_run_count`] mirrors the same skip
/// condition as [`backdrop_blur`], so asserting `counter == 0`
/// confirms the node logic would be skipped without requiring a real GPU.
///
/// Ignored: same `winit` / main-thread constraint as
/// [`backdrop_blur_texture_is_allocated_after_first_frame`].
#[test]
#[ignore = "winit requires the main thread on macOS; verify by running the app"]
fn backdrop_blur_node_skips_when_disabled() {
    let mut app = make_render_app();
    // Disable blur before the first update.
    app.world_mut().resource_mut::<BackdropBlurEnabled>().0 = false;
    app.update();
    let render_app = app.sub_app(RenderApp);
    let counter = render_app
        .world()
        .get_resource::<wc_core::ui::blur::node::BlurNodeRunCount>()
        .map(|c| c.0)
        .unwrap_or(0);
    assert_eq!(
        counter, 0,
        "node must skip when BackdropBlurEnabled is false"
    );
}

/// Verify that [`BlurNodeRunCount`] is incremented when blur is enabled and
/// [`ExtractedUiOpacity`] is at the default (1.0).
///
/// The run-count proxy in [`prepare_blur_run_count`] mirrors the same run
/// condition as [`backdrop_blur`], so asserting `counter >= 1`
/// confirms the node logic would execute without requiring a real GPU.
///
/// Ignored: same `winit` / main-thread constraint as
/// [`backdrop_blur_texture_is_allocated_after_first_frame`].
///
/// [`ExtractedUiOpacity`]: wc_core::ui::blur::node::ExtractedUiOpacity
/// [`prepare_blur_run_count`]: wc_core::ui::blur::node::prepare_blur_run_count
/// [`backdrop_blur`]: wc_core::ui::blur::node::backdrop_blur
#[test]
#[ignore = "winit requires the main thread on macOS; verify by running the app"]
fn backdrop_blur_node_runs_when_enabled() {
    let mut app = make_render_app();
    // BackdropBlurEnabled defaults to true; UiOpacity defaults to 1.0.
    // The AutoFadePlugin is not loaded here (no WaveConductorUiPlugin), so
    // ExtractedUiOpacity stays at its Default of 0.0. We manually set it to
    // 1.0 in the render world after the first update seeds the resource.
    app.update();
    {
        let render_app = app.get_sub_app_mut(RenderApp).expect("render app");
        render_app
            .world_mut()
            .get_resource_or_init::<wc_core::ui::blur::node::ExtractedUiOpacity>()
            .0 = 1.0;
    }
    app.update();
    let render_app = app.sub_app(RenderApp);
    let counter = render_app
        .world()
        .get_resource::<wc_core::ui::blur::node::BlurNodeRunCount>()
        .map(|c| c.0)
        .unwrap_or(0);
    assert!(
        counter >= 1,
        "node must run at least once when enabled and opacity >= 0.01"
    );
}
