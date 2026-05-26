//! Line sketch settings.
//!
//! Curated knobs that show up in the user panel. v4 exposes two: particle
//! density and the gravity constant. Plan 7 mirrors that. Drag and attractor
//! radius existed as v5-only knobs during Plan 6 (the inverse-linear gravity
//! era); Plan 7 baked drag into [`crate::line::particle::SimParams`] from
//! fixed v4 constants and made the force constant-magnitude (no radius
//! needed), so both fields are dropped.
//!
//! ## Serde forward-compatibility
//!
//! Existing user TOML written by earlier v5 builds with `drag` /
//! `attractor_radius` keys still deserializes cleanly: serde's default is to
//! ignore unknown fields, and we intentionally do **not** set
//! `#[serde(deny_unknown_fields)]`. A future maintainer adding that attribute
//! would break upgrades from v5-line; leave it off so dropped knobs don't
//! invalidate persisted user settings.
//!
//! - **`particle_density`** — particles per canvas-pixel of width. v4 uses 10
//!   (so a 1280px window has ~12,800 particles). Restart on change (the
//!   compute pipeline rebuilds its storage buffer).
//! - **`gravity_constant`** — strength of the pull toward attractors (v4
//!   `GRAVITY_CONSTANT`, default 280).
//! - **`gamma`** — per-channel gamma curve on the post-process pass.
//! - **`spawn_template`** — optional PNG path whose luminance × alpha weights
//!   the particle spawn density (empty = horizontal-line layout).

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

    /// Path to a PNG file whose luminance × alpha drives particle spawn density.
    /// Empty string = use the default horizontal-line layout. Relative paths
    /// resolve against the process current directory; absolute paths are
    /// honored as-is. v4 default = "" (no template). Restart on change so
    /// `spawn_line` re-runs with the new sampler.
    #[setting(default = String::new(), ty = Text, category = User, requires_restart)]
    pub spawn_template: String,
}
