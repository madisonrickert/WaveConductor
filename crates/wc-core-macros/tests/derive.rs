//! End-to-end tests for `#[derive(SketchSettings)]`.

#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]
#![allow(
    clippy::panic,
    reason = "panic! in else-branches is idiomatic in tests for asserting variant kinds"
)]
#![allow(
    clippy::float_cmp,
    reason = "comparing f32 arrays by exact equality is intentional in derive tests checking constant defaults"
)]

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core::settings::{SettingKind, SettingsCategory, SketchSettings};
use wc_core_macros::SketchSettings;

#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "derive_test")]
struct Fixture {
    #[setting(default = 7_u32, min = 1_u32, max = 10_u32, category = User, requires_restart)]
    count: u32,
    #[setting(default = 0.25_f32, min = 0.0_f32, max = 1.0_f32, step = 0.05_f32, category = Dev)]
    smoothing: f32,
    #[setting(default = false, ty = Boolean, category = User)]
    flag: bool,
    #[setting(default = [0.5_f32, 0.5, 0.5, 1.0], ty = Color, category = User)]
    color: [f32; 4],
    #[setting(default = String::from("hi"), ty = Text, label = "Greeting", category = Dev)]
    greeting: String,
}

#[test]
fn default_impl_uses_field_defaults() {
    let f = Fixture::default();
    assert_eq!(f.count, 7);
    assert!((f.smoothing - 0.25).abs() < f32::EPSILON);
    assert!(!f.flag);
    assert_eq!(f.color, [0.5, 0.5, 0.5, 1.0]);
    assert_eq!(f.greeting, "hi");
}

#[test]
fn storage_key_matches_attribute() {
    assert_eq!(Fixture::STORAGE_KEY, "derive_test");
}

#[test]
fn settings_def_lists_every_field_in_order() {
    let defs = Fixture::settings_def();
    let names: Vec<&str> = defs.iter().map(|d| d.field_name).collect();
    assert_eq!(names, ["count", "smoothing", "flag", "color", "greeting"]);
}

#[test]
fn category_attribute_maps_correctly() {
    let defs = Fixture::settings_def();
    assert_eq!(defs[0].category, SettingsCategory::User);
    assert_eq!(defs[1].category, SettingsCategory::Dev);
    assert_eq!(defs[2].category, SettingsCategory::User);
    assert_eq!(defs[3].category, SettingsCategory::User);
    assert_eq!(defs[4].category, SettingsCategory::Dev);
}

#[test]
fn requires_restart_flag_propagates() {
    let defs = Fixture::settings_def();
    assert!(defs[0].requires_restart);
    assert!(!defs[1].requires_restart);
}

#[test]
fn label_falls_back_to_field_name_when_unset() {
    let defs = Fixture::settings_def();
    assert_eq!(defs[0].label, "count");
    // Explicit override.
    assert_eq!(defs[4].label, "Greeting");
}

#[test]
fn number_range_carries_bounds_and_step() {
    let defs = Fixture::settings_def();
    let SettingKind::Number(range) = &defs[1].kind else {
        panic!("expected Number kind for smoothing");
    };
    assert_eq!(range.min, Some(0.0));
    assert_eq!(range.max, Some(1.0));
    assert!(range.step.is_some());
    assert!((range.step.expect("step set") - 0.05).abs() < 1e-6);
}

#[test]
fn kind_attribute_overrides_default_number_kind() {
    let defs = Fixture::settings_def();
    assert!(matches!(defs[2].kind, SettingKind::Boolean));
    assert!(matches!(defs[3].kind, SettingKind::Color));
    assert!(matches!(defs[4].kind, SettingKind::Text));
}
