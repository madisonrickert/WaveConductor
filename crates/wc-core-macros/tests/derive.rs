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

/// Unit-variant enum exercising `ty = Enum`. Variant names are surfaced to
/// the settings panel via `bevy_reflect` enum info, so no list is repeated
/// in the `#[setting(...)]` attribute.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
enum Quality {
    Low,
    Medium,
    High,
}

#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "derive_test")]
struct Fixture {
    #[setting(default = 7_u32, min = 1_u32, max = 10_u32, unit = "ms", category = User, requires_restart)]
    count: u32,
    #[setting(default = 0.25_f32, min = 0.0_f32, max = 1.0_f32, step = 0.05_f32, category = Dev)]
    smoothing: f32,
    #[setting(default = false, ty = Boolean, category = User)]
    flag: bool,
    #[setting(default = [0.5_f32, 0.5, 0.5, 1.0], ty = Color, category = User)]
    color: [f32; 4],
    #[setting(default = String::from("hi"), ty = Text, label = "Greeting", category = Dev)]
    greeting: String,
    #[setting(default = Quality::Medium, ty = Enum, category = User)]
    quality: Quality,
}

#[test]
fn default_impl_uses_field_defaults() {
    let f = Fixture::default();
    assert_eq!(f.count, 7);
    assert!((f.smoothing - 0.25).abs() < f32::EPSILON);
    assert!(!f.flag);
    assert_eq!(f.color, [0.5, 0.5, 0.5, 1.0]);
    assert_eq!(f.greeting, "hi");
    assert_eq!(f.quality, Quality::Medium);
}

#[test]
fn storage_key_matches_attribute() {
    assert_eq!(Fixture::STORAGE_KEY, "derive_test");
}

#[test]
fn settings_def_lists_every_field_in_order() {
    let defs = Fixture::settings_def();
    let names: Vec<&str> = defs.iter().map(|d| d.field_name).collect();
    assert_eq!(
        names,
        ["count", "smoothing", "flag", "color", "greeting", "quality"]
    );
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
fn label_title_cases_field_name_when_unset() {
    // When no explicit `label = "..."` is given, the macro title-cases the
    // field name: `count` → `"Count"`, `my_field` → `"My Field"`.
    let defs = Fixture::settings_def();
    assert_eq!(defs[0].label, "Count");
    // Explicit `label = "Greeting"` overrides the title-case default.
    assert_eq!(defs[4].label, "Greeting");
}

#[test]
fn unit_attribute_propagates_and_defaults_empty() {
    let defs = Fixture::settings_def();
    // Explicit `unit = "ms"` on the first field.
    assert_eq!(defs[0].unit, "ms");
    // Fields without a `unit` attribute serialise to the empty string.
    assert_eq!(defs[1].unit, "");
    assert_eq!(defs[4].unit, "");
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

#[test]
fn enum_kind_carries_variant_names_from_reflect() {
    let defs = Fixture::settings_def();
    let SettingKind::Enum { variants } = &defs[5].kind else {
        panic!("expected Enum kind for quality");
    };
    // Variant names come from `bevy_reflect` enum info, in declaration order.
    assert_eq!(*variants, ["Low", "Medium", "High"]);
}

/// Second unit-variant enum for the two-enum-fields fixture below.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
enum Theme {
    Dark,
    Light,
}

/// Two `ty = Enum` fields of different enum types in one struct: each
/// `SettingKind::Enum` must carry its own field type's variant list (the
/// expansion turbofishes the *field's* type into `enum_variant_names`, not
/// a struct-wide one).
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "derive_test_two_enums")]
struct TwoEnumFixture {
    #[setting(default = Quality::Low, ty = Enum, category = User)]
    quality: Quality,
    #[setting(default = Theme::Dark, ty = Enum, category = User)]
    theme: Theme,
}

#[test]
fn two_enum_fields_carry_independent_variant_lists() {
    let defs = TwoEnumFixture::settings_def();
    let SettingKind::Enum { variants: quality } = &defs[0].kind else {
        panic!("expected Enum kind for quality");
    };
    let SettingKind::Enum { variants: theme } = &defs[1].kind else {
        panic!("expected Enum kind for theme");
    };
    assert_eq!(*quality, ["Low", "Medium", "High"]);
    assert_eq!(*theme, ["Dark", "Light"]);
}

/// Fixture exercising `ty = TemplateLibrary`: same `filter_label`/`extensions`
/// plumbing as `FilePath`, distinct `SettingKind` variant.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "derive_test_template")]
struct TemplateFixture {
    #[setting(
        default = String::new(),
        ty = TemplateLibrary,
        filter_label = "Image",
        extensions = ["png", "jpg"],
        category = User
    )]
    #[serde(default)]
    template_field: String,
}

#[test]
fn template_library_kind_carries_filter_and_extensions() {
    let defs = TemplateFixture::settings_def();
    let SettingKind::TemplateLibrary {
        filter_label,
        extensions,
    } = &defs[0].kind
    else {
        panic!("expected TemplateLibrary kind for template_field");
    };
    assert_eq!(*filter_label, "Image");
    assert_eq!(*extensions, ["png", "jpg"]);
}
