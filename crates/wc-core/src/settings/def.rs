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
    /// Like [`SettingKind::FilePath`], but the field is backed by the managed
    /// image **template library** (`crate::templates`): the widget is a picker
    /// of previously-imported templates with thumbnails plus an import button,
    /// and the stored value is the absolute path to the managed blob. The
    /// `filter_label` / `extensions` configure the import file dialog exactly as
    /// for `FilePath`. Falls back to a plain file picker when the `templates`
    /// feature is off.
    TemplateLibrary {
        /// File-dialog filter label for the Import action (e.g. "Image").
        filter_label: &'static str,
        /// Extensions the import dialog accepts (e.g. `&["png", "jpg"]`).
        extensions: &'static [&'static str],
    },
    /// A unit-variant Rust enum. Rendered as an `egui::ComboBox` listing each
    /// variant by name. The derive macro fills `variants` from the field
    /// type's [`bevy::reflect::TypeInfo`] (see [`enum_variant_names`]), so the
    /// list never drifts from the enum definition. Enums with payload
    /// variants (tuple or struct) are **not** supported — see the
    /// [`enum_variant_names`] docs.
    Enum {
        /// Variant names in declaration order, as reported by reflection.
        /// These are the Rust identifiers (e.g., `"MediaPipe"`), which also
        /// match serde's default unit-variant serialization, so the same
        /// string appears in the persisted TOML.
        variants: &'static [&'static str],
    },
}

/// Returns the variant names of a reflected enum type, in declaration order.
///
/// Used by the `#[derive(SketchSettings)]` expansion for `ty = Enum` fields,
/// so the variant list shown in the settings panel is derived from the enum
/// definition itself rather than repeated as literals in the attribute.
///
/// ## Unit variants only
///
/// Enum settings must consist solely of unit variants (no tuple or struct
/// payloads): the `ComboBox` writes a selection back through reflection as a
/// payload-less [`bevy::reflect::enums::DynamicEnum`], which cannot construct a
/// payload variant. A proc macro cannot see the enum's definition (only the
/// field's type name), so this cannot be a compile error; instead this
/// function fails loudly in debug builds — the `debug_assert!`s below fire
/// the first time `settings_def()` runs (i.e., at settings registration) when
/// `T` is not an enum or has a non-unit variant. In release builds the names
/// are still returned and selecting an unsupported variant is rejected at
/// write-back time (logged, value unchanged).
#[must_use]
pub fn enum_variant_names<T: bevy::reflect::Typed>() -> &'static [&'static str] {
    use bevy::reflect::enums::VariantInfo;
    use bevy::reflect::TypeInfo;
    match T::type_info() {
        TypeInfo::Enum(info) => {
            debug_assert!(
                info.iter().all(|v| matches!(v, VariantInfo::Unit(_))),
                "`ty = Enum` setting on `{}`: only unit variants are supported \
                 (a ComboBox selection cannot construct a payload variant)",
                core::any::type_name::<T>(),
            );
            info.variant_names()
        }
        other => {
            debug_assert!(
                false,
                "`ty = Enum` setting requires an enum field, got `{}` ({:?} kind)",
                core::any::type_name::<T>(),
                other.kind(),
            );
            &[]
        }
    }
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
    /// Optional unit suffix shown after a numeric value (e.g. `"ms"`, `"Hz"`,
    /// `"mm"`). Empty string (`""`) means no unit. Rendered as the slider's
    /// suffix; ignored for non-numeric kinds.
    pub unit: &'static str,
    /// Optional section group name shown as a header above a cluster of
    /// related fields. Empty string (`""`) means no header — the field
    /// renders in an unlabeled group at the start of the panel.
    pub section: &'static str,
    /// Which panel renders this field.
    pub category: SettingsCategory,
    /// Widget shape + value-space constraints.
    pub kind: SettingKind,
    /// If true, changing this field fires `SketchRestart` so the sketch can
    /// rebuild any resources it baked from the value (e.g., particle counts).
    pub requires_restart: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::reflect::Reflect;

    #[derive(Reflect, Clone, PartialEq, Debug)]
    enum Mode {
        First,
        Second,
        Third,
    }

    #[test]
    fn enum_variant_names_lists_unit_variants_in_order() {
        assert_eq!(enum_variant_names::<Mode>(), &["First", "Second", "Third"]);
    }

    #[derive(Reflect, Clone, PartialEq, Debug)]
    enum WithPayload {
        Plain,
        Carrying(u32),
    }

    /// The unit-variants-only contract fails loudly in debug builds; see the
    /// `enum_variant_names` docs for why it cannot be a compile error.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "only unit variants are supported")]
    fn enum_variant_names_rejects_payload_variants_in_debug() {
        let _ = enum_variant_names::<WithPayload>();
    }

    /// Same loud-failure contract when the field is not an enum at all.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "requires an enum field")]
    fn enum_variant_names_rejects_non_enum_types_in_debug() {
        let _ = enum_variant_names::<bool>();
    }
}
