//! Synthetic settings struct exercised by Plan-5 integration tests and
//! used to populate the user panel before any real sketches exist.
//!
//! This file ships in the production binary because the dev panel and user
//! panel both want at least one example struct to render against before
//! Plan 6+ ships real sketches. After the first real sketch lands, this
//! file becomes test-only — gate it on `#[cfg(test)]` then.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// A small, varied settings struct. Touches every `SettingKind` so panel
/// renderers and persistence can be tested in isolation.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "test")]
pub struct TestSketchSettings {
    /// Number of synthetic widgets.
    #[setting(default = 42_u32, min = 0_u32, max = 1000_u32, category = User, requires_restart)]
    pub widget_count: u32,
    /// Animation tempo in Hz.
    #[setting(default = 0.5_f32, min = 0.0_f32, max = 4.0_f32, step = 0.05_f32, category = User)]
    pub tempo_hz: f32,
    /// Whether the overlay tint is enabled.
    #[setting(default = true, ty = Boolean, category = User)]
    pub enable_tint: bool,
    /// Foreground color.
    #[setting(default = [1.0_f32, 1.0, 1.0, 1.0], ty = Color, category = User)]
    pub tint_color: [f32; 4],
    /// Developer-only override label.
    #[setting(default = String::from("default"), ty = Text, category = Dev)]
    pub dev_label: String,
}
