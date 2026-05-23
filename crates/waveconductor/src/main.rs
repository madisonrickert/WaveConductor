//! `WaveConductor` v5 binary entry point.
//!
//! Constructs the Bevy [`App`], registers core plugins, and runs the event loop.
//! In Plan 1 this opens an empty Bevy window to prove the workspace links and
//! runs end-to-end. Subsystem registration (audio, input, settings) lands in
//! Plan 2; sketch plugins land in Plans 3 and 4.

use bevy::prelude::*;
use wc_core::CorePlugin;
use wc_sketches::SketchesPlugin;

fn main() {
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
            CorePlugin,
            SketchesPlugin,
        ))
        .add_systems(Startup, log_startup)
        .run();
}

/// One-shot logger that confirms the app booted. Removed once Plan 2 wires in
/// proper logging configuration.
fn log_startup() {
    tracing::info!("WaveConductor v5 starting (Plan 1 scaffold)");
}
