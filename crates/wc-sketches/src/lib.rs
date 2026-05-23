//! # wc-sketches
//!
//! Bundle plugin that registers every sketch in `WaveConductor` v5. Sketches
//! themselves arrive in Plan 3 (Line) and Plan 4 (Flame, Dots, Cymatics, Waves).
//! In Plan 1 this is an empty placeholder so the workspace builds end-to-end.

#![warn(missing_docs)]

use bevy::prelude::*;

/// Single plugin that bundles every sketch.
///
/// Registered once by the binary crate. Each sketch is a sub-plugin added inside
/// `build()` as it lands.
pub struct SketchesPlugin;

impl Plugin for SketchesPlugin {
    fn build(&self, _app: &mut App) {
        // Plan 3 onward will add per-sketch sub-plugins here.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sketches_plugin_builds_without_panicking() {
        let mut app = App::new();
        app.add_plugins(SketchesPlugin);
    }
}
