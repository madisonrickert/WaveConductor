//! Line sketch settings.
//!
//! Curated knobs that show up in the user panel:
//!
//! - **`particle_count`** — how many particles to simulate. Restart on change
//!   (the compute pipeline rebuilds its storage buffer).
//! - **`gravity_constant`** — strength of the pull toward the pointer attractor.
//! - **`drag`** — per-frame velocity damping (0 = none, 1 = freeze).
//! - **`attractor_radius`** — soft radius around the pointer inside which the
//!   gravity well dominates.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// User-tunable parameters for the Line sketch.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "line")]
pub struct LineSettings {
    /// Number of particles to simulate. Changing this requires a sketch restart
    /// because the storage buffer is sized at spawn time.
    #[setting(default = 5000_u32, min = 100_u32, max = 50_000_u32, category = User, requires_restart)]
    pub particle_count: u32,

    /// Strength of the pull toward the pointer attractor (acceleration units).
    #[setting(default = 280.0_f32, min = 0.0_f32, max = 1000.0_f32, step = 10.0_f32, category = User)]
    pub gravity_constant: f32,

    /// Per-frame velocity damping. 0.0 = no damping, 1.0 = freeze.
    #[setting(default = 0.47_f32, min = 0.0_f32, max = 1.0_f32, step = 0.01_f32, category = User)]
    pub drag: f32,

    /// Soft attractor radius in world units.
    #[setting(default = 50.0_f32, min = 1.0_f32, max = 500.0_f32, step = 5.0_f32, category = User)]
    pub attractor_radius: f32,
}
