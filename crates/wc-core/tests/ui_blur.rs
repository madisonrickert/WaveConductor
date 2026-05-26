//! RenderApp-level tests for the backdrop-blur pipeline.
//!
//! These tests exercise [`BackdropBlurTexture`] allocation in the render world.
//! They require a GPU adapter and a real render device, so they are marked
//! `#[ignore]` when the environment cannot satisfy those requirements (e.g.
//! headless CI without a GPU). Run them locally with:
//!
//! ```text
//! cargo test -p wc-core --test ui_blur -- --ignored
//! ```
//!
//! The `DefaultPlugins` path is used for simplicity; it initialises a real
//! render world including the primary window extraction that `ensure_blur_texture`
//! depends on. If a GPU adapter is not available, Bevy's `RenderPlugin` panics
//! during `App::update()`, which surfaces as a test failure rather than a
//! compile error. The `#[ignore]` attribute prevents that from blocking CI.

#![cfg(not(target_arch = "wasm32"))]

use bevy::prelude::*;
use bevy::render::RenderApp;
use wc_core::ui::blur::{BackdropBlurPlugin, BackdropBlurTexture};

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
