//! End-to-end test for the heatmap-image spawn path.
//!
//! Drives `spawn_line` with a real PNG path (`assets/sketches/line/star.png`)
//! and confirms particle positions follow the image's luminance × alpha
//! distribution. Also exercises the fallback path with a deliberately wrong
//! path.
//!
//! Carry-forward #63.

#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

mod common;
use common::input::tap_key;
use common::sketches_test_app;

use bevy::input::keyboard::KeyCode;
use bevy::prelude::*;
use wc_core::lifecycle::state::AppState;
use wc_sketches::line::settings::LineSettings;
use wc_sketches::line::sim_cpu::LineCpuMirror;

/// Mirror of `line_input.rs::enter_line` — three updates suffice (one fold,
/// one nav handler, one `OnEnter`). Inlined here rather than imported because
/// `line_input.rs` is a sibling integration-test binary, not a library module.
fn enter_line(app: &mut App) {
    tap_key(app, KeyCode::Digit1);
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line,
        "Digit1 keyboard nav should enter AppState::Line",
    );
}

fn app_with_template(template: &str) -> App {
    let mut app = sketches_test_app();
    {
        // LineSettings is registered by LinePlugin::build. Set the template
        // *before* entering Line so spawn_line reads the override on OnEnter.
        let mut settings = app.world_mut().resource_mut::<LineSettings>();
        settings.spawn_template = template.to_string();
    }
    app.update();
    enter_line(&mut app);
    app
}

/// star.png as an absolute path resolved at compile time. `cargo test` for
/// integration tests sets CWD to the crate root (`crates/wc-sketches/`), but
/// `image::open` (called inside `heatmap::sample_from_heatmap`) uses
/// `std::fs`, which doesn't auto-prepend `CARGO_MANIFEST_DIR` — so we build the
/// absolute path explicitly. Mirrors `LINE_BACKGROUND_PATH` in
/// `crates/waveconductor/src/main.rs`.
const STAR_PNG_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/sketches/line/star.png"
);

#[test]
fn heatmap_spawn_clusters_particles_near_bright_pixels() {
    // star.png is a 64x64 soft-diamond glow with luminance peaking at the
    // center. We expect particle X positions to cluster around the canvas
    // center (and not be uniformly distributed like the fallback layout).
    let app = app_with_template(STAR_PNG_PATH);

    let mirror = app.world().resource::<LineCpuMirror>();
    let particles = &mirror.particles;
    assert!(
        particles.len() >= 100,
        "expected ≥100 particles; got {}",
        particles.len()
    );

    // Compute X-coordinate mean and stddev. For a uniformly-spread
    // horizontal layout, X stddev ≈ canvas_width / sqrt(12) ≈ width / 3.46.
    // For a center-clustered heatmap from a soft-diamond sprite, stddev
    // should be substantially smaller.
    #[allow(
        clippy::cast_precision_loss,
        clippy::as_conversions,
        reason = "particles.len() ≤ 100k so f32 round-trip is lossless; as cast is intentional"
    )]
    let n = particles.len() as f32;
    let mean_x: f32 = particles.iter().map(|p| p.position[0]).sum::<f32>() / n;
    let var_x: f32 = particles
        .iter()
        .map(|p| (p.position[0] - mean_x).powi(2))
        .sum::<f32>()
        / n;
    let stddev_x = var_x.sqrt();

    assert!(
        mean_x.abs() < 50.0,
        "expected mean_x near 0; got {mean_x} (suggests offset bias)"
    );
    let win_w = 1280.0_f32;
    let uniform_stddev = win_w / 3.46_f32;
    assert!(
        stddev_x < uniform_stddev * 0.75,
        "stddev_x={stddev_x} suggests uniform layout (uniform≈{uniform_stddev}); \
         heatmap should cluster particles toward the center"
    );
}

#[test]
fn missing_template_falls_back_to_horizontal_layout() {
    let app = app_with_template("/this/path/does/not/exist.png");
    let mirror = app.world().resource::<LineCpuMirror>();
    let particles = &mirror.particles;
    assert!(
        !particles.is_empty(),
        "fallback must still produce particles"
    );

    // Fallback layout: Y stays near mid-Y (== 0 in window-centered world);
    // sawtooth jitter is ±4px. If we got the heatmap path or a different
    // fallback, Y would spread further.
    for p in particles {
        assert!(
            p.position[1].abs() <= 4.0 + 0.001,
            "fallback Y {} not near 0±4 (got heatmap or wrong layout?)",
            p.position[1]
        );
    }
}
