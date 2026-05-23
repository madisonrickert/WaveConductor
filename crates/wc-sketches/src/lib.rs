//! `WaveConductor` sketches.
//!
//! The [`SketchesPlugin`] umbrella registers every concrete sketch plugin.
//! Each sketch lives in its own module and follows the pattern documented in
//! [`wc_core::sketch`].

pub mod line;

use bevy::prelude::*;

/// Umbrella plugin that registers every concrete sketch.
pub struct SketchesPlugin;

impl Plugin for SketchesPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(line::LinePlugin);
    }
}
