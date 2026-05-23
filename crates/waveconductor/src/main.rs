//! `WaveConductor` v5 binary entry point.
//!
//! Constructs the Bevy [`App`], registers core plugins, and runs the event loop.
//! In Plan 2 this opens a window and exercises the lifecycle plugin (state
//! machine + leafwing keyboard actions). Sketch plugins land in Plan 6 onward.

use bevy::prelude::*;
use tracing_subscriber::EnvFilter;
use wc_core::CorePlugin;
use wc_sketches::SketchesPlugin;

fn main() {
    init_tracing();
    App::new()
        .add_plugins((
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: "WaveConductor".into(),
                    resolution: (1280_u32, 720_u32).into(),
                    ..default()
                }),
                ..default()
            }),
            bevy_egui::EguiPlugin::default(),
            CorePlugin,
            SketchesPlugin,
        ))
        .run();
}

/// Initialize the global tracing subscriber.
///
/// Honors `RUST_LOG` (e.g. `RUST_LOG=info,wc_core=debug`). When unset, defaults
/// to `info` for the application crates so users can see navigation and idle
/// state transitions in the terminal during manual testing.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,waveconductor=info,wc_core=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
