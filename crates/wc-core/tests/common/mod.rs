//! Shared fixtures for `wc-core` integration tests.
//!
//! Each integration test in `tests/*.rs` is its own crate; `tests/common/`
//! is the canonical Rust pattern for sharing helpers among them. Submodules
//! here may go unused by some integration binaries — `#[allow(dead_code)]`
//! at the module level keeps `cargo test` happy.
//!
//! `TestSketchSettings` is a small, varied settings struct that touches every
//! `SettingKind` so panel renderers and persistence can be tested in isolation
//! against a stable target. Lives in `tests/common/` (not `src/`) so it does
//! not ship in the release binary.

#![allow(
    dead_code,
    reason = "test fixtures may be referenced from only some integration tests"
)]

pub mod app;
pub mod input;
pub mod lifecycle;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// Unit-variant enum backing the `ty = Enum` setting on
/// [`TestSketchSettings`]. Serde serializes unit variants as their name
/// string, so the persisted TOML reads `blend_mode = "Add"`.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestBlendMode {
    Normal,
    Add,
    Multiply,
}

#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "test")]
pub struct TestSketchSettings {
    #[setting(default = 42_u32, min = 0_u32, max = 1000_u32, category = User, requires_restart)]
    pub widget_count: u32,
    #[setting(default = 0.5_f32, min = 0.0_f32, max = 4.0_f32, step = 0.05_f32, category = User)]
    pub tempo_hz: f32,
    #[setting(default = true, ty = Boolean, category = User)]
    pub enable_tint: bool,
    #[setting(default = [1.0_f32, 1.0, 1.0, 1.0], ty = Color, category = User)]
    pub tint_color: [f32; 4],
    #[setting(default = String::from("default"), ty = Text, category = Dev)]
    pub dev_label: String,
    // `#[serde(default = ...)]` mirrors the production pattern
    // (hand_tracking.rs): a settings file written before this field existed
    // still deserializes, with the field falling back to the same value
    // `TestSketchSettings::default()` uses.
    #[setting(default = TestBlendMode::Normal, ty = Enum, category = User)]
    #[serde(default = "default_blend_mode")]
    pub blend_mode: TestBlendMode,
}

/// Serde fallback so configs saved before `blend_mode` existed still load.
fn default_blend_mode() -> TestBlendMode {
    TestBlendMode::Normal
}
