//! Line sketch settings.
//!
//! Curated knobs that show up in the user panel. v4 exposes two: particle
//! density and the gravity constant. Plan 7 mirrors that. Drag and attractor
//! radius existed as v5-only knobs during Plan 6 (the inverse-linear gravity
//! era); Plan 7 baked drag into [`crate::line::particle::SimParams`] from
//! fixed v4 constants and made the force constant-magnitude (no radius
//! needed), so both fields are dropped.
//!
//! - **`particle_density`** — particles per canvas-pixel of width. v4 uses 10
//!   (so a 1280px window has ~12,800 particles). Restart on change (the
//!   compute pipeline rebuilds its storage buffer).
//! - **`gravity_constant`** — strength of the pull toward attractors (v4
//!   `GRAVITY_CONSTANT`, default 280).

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// User-tunable parameters for the Line sketch.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "line")]
pub struct LineSettings {
    /// Particles per canvas-pixel of width. Restart on change.
    #[setting(
        default = 10.0_f32,
        min = 0.1_f32,
        max = 30.0_f32,
        step = 0.5_f32,
        category = User,
        requires_restart
    )]
    pub particle_density: f32,

    /// Strength of the pull toward the pointer attractor. v4 default = 280.
    #[setting(default = 280.0_f32, min = 0.0_f32, max = 1000.0_f32, step = 10.0_f32, category = User)]
    pub gravity_constant: f32,

    /// Per-channel gamma curve applied as the final step of the gravity-smear
    /// post-process. v4 default = 1.0.
    #[setting(default = 1.0_f32, min = 0.1_f32, max = 4.0_f32, step = 0.1_f32, category = User)]
    pub gamma: f32,
}
