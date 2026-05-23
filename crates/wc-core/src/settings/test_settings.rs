//! Synthetic settings struct exercised by Plan-5 integration tests.
//!
//! Plan 5 originally registered this in production `SettingsPlugin::build` so
//! the panels had something to render before any real sketches existed. Plan 6
//! Phase 0 (this commit's lineage) moved that registration into the integration
//! test harness — the file still ships in the production binary (no `#[cfg(test)]`
//! gate) but is no longer instantiated unless a test explicitly registers it.
//! The next plan that introduces a second real sketch can drop the file entirely
//! or `#[cfg(test)]`-gate the module.

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
