//! Runtime metadata produced by `#[derive(SketchSettings)]`.
//!
//! The derive macro emits a `Vec<SettingDef>` per settings struct. Panels
//! consume this table to render typed widgets without reflection-walking the
//! struct at every frame.

/// Top-level kind of a setting. Determines which widget the user panel uses
/// and how persistence serializes / deserializes the value.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingKind {
    /// Numeric scalar (`u32`, `i32`, `f32`, `f64`). Rendered as a slider when
    /// both `min` and `max` are present, otherwise as a `DragValue`.
    Number(NumberRange),
    /// Boolean toggle. Rendered as a checkbox.
    Boolean,
    /// RGBA color stored as `[f32; 4]`. Rendered with `egui::color_picker`.
    Color,
    /// Free-form UTF-8 string. Rendered as a single-line text edit.
    Text,
    /// Filesystem path stored as a UTF-8 `String`. Rendered as a text-edit
    /// plus a Browse… button that opens [`rfd::FileDialog`]. The `extensions`
    /// list filters the dialog; an empty slice allows any file.
    FilePath {
        /// Human-facing filter label shown above the extension list in the
        /// file dialog (e.g., "Image", "Audio sample", "Configuration").
        filter_label: &'static str,
        /// Extensions to filter the picker on (e.g., `&["png", "jpg"]`).
        /// Empty means no filter (and the label is ignored).
        extensions: &'static [&'static str],
    },
}

/// Numeric range constraints. All bounds are stored as `f64` for uniform
/// rendering; the derive macro converts from the field's native type via
/// `f64::from(...)` (so `u8`, `u16`, `u32`, `i8`, `i16`, `i32`, `f32`, `f64`
/// all work without lossy `as` casts).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NumberRange {
    /// Lower bound. `None` when the setting is unbounded below.
    pub min: Option<f64>,
    /// Upper bound. `None` when the setting is unbounded above.
    pub max: Option<f64>,
    /// Slider step. `None` lets egui choose.
    pub step: Option<f64>,
}

/// Which panel a setting is shown in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsCategory {
    /// User-facing curated control. Appears in the curated settings panel.
    User,
    /// Developer-only knob. Visible only via the Shift+D dev inspector.
    Dev,
}

/// Per-field metadata. One entry per `#[setting(...)]`-annotated struct field.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingDef {
    /// The Rust field name (`stringify!(field)`). Used as the persistence key.
    pub field_name: &'static str,
    /// Human-facing label. Defaults to `field_name` when `label = "..."` is
    /// not given in the attribute.
    pub label: &'static str,
    /// Which panel renders this field.
    pub category: SettingsCategory,
    /// Widget shape + value-space constraints.
    pub kind: SettingKind,
    /// If true, changing this field fires `SketchRestart` so the sketch can
    /// rebuild any resources it baked from the value (e.g., particle counts).
    pub requires_restart: bool,
}
