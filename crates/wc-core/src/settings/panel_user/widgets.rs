//! Typed value widgets for the settings panel's Grid column 2.
//!
//! [`render_widget_value`] dispatches on [`SettingKind`] to one of the
//! per-kind renderers below; each renders only the input widget (no label —
//! [`super::fields`]'s Grid already placed that in column 1). The
//! `TemplateLibrary` kind delegates to
//! `super::template_picker::render_template_library` when the `templates`
//! feature is on, and permanently falls back to [`render_file_path`]
//! otherwise.
//
// NB: the `template_picker` reference above is a plain code span, not an
// intra-doc link — `mod template_picker` is `#[cfg(feature = "templates")]`, so
// a `[...]` link fails to resolve in the default-feature `cargo doc` CI gate
// (which does not pass `--all-features`). See `super::fields` for the same.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    reason = "egui sliders use f32/i64 widget ranges; bounds-checked against SettingDef metadata"
)]

use bevy::color::ColorToComponents;
use bevy_egui::egui;

use crate::settings::def::{SettingDef, SettingKind};
#[cfg(feature = "templates")]
use crate::ui::OverlayStyle;

/// Render the widget (second Grid column) for `field` based on the metadata in `def`.
///
/// Called from inside an `egui::Grid` row after the label has already been
/// placed in column 1. Each helper renders only the input widget — no label,
/// no `ui.horizontal` wrapper. The Grid handles label/widget alignment.
///
/// `field` is `&mut dyn PartialReflect` as returned by [`bevy::reflect::structs::Struct::field_mut`].
///
/// `storage_key` is the owning settings struct's storage key, threaded through
/// to widgets that need a unique egui id (currently [`render_enum`]).
pub(super) fn render_widget_value(
    field: &mut dyn bevy::reflect::PartialReflect,
    def: &SettingDef,
    storage_key: &'static str,
    #[cfg(feature = "templates")] template_rows: &[crate::templates::view::TemplateRow],
    #[cfg(feature = "templates")] template_dirty: &mut bool,
    #[cfg(feature = "templates")] style: &OverlayStyle,
    ui: &mut egui::Ui,
) {
    match &def.kind {
        SettingKind::Number(range) => render_number(field, range, def.unit, ui),
        SettingKind::Boolean => render_bool(field, ui),
        SettingKind::Color => render_color(field, ui),
        SettingKind::Text => render_text(field, ui),
        SettingKind::TextList => render_text_list(field, ui),
        SettingKind::FilePath {
            filter_label,
            extensions,
        } => {
            render_file_path(field, filter_label, extensions, ui);
        }
        SettingKind::TemplateLibrary {
            filter_label,
            extensions,
        } => {
            #[cfg(feature = "templates")]
            super::template_picker::render_template_library(
                field,
                storage_key,
                def.field_name,
                filter_label,
                extensions,
                template_rows,
                template_dirty,
                style,
                ui,
            );
            // Permanent fallback when the `templates` feature is off.
            #[cfg(not(feature = "templates"))]
            render_file_path(field, filter_label, extensions, ui);
        }
        SettingKind::Enum { variants } => {
            render_enum(field, storage_key, def.field_name, variants, ui);
        }
    }
}

/// Render the numeric widget (slider) for a field. Called from inside a Grid row
/// where the label has already been placed in column 1. Dispatches on the
/// field's concrete Rust type (u32, f32, etc.) via `try_downcast_mut`.
///
/// `unit` is appended after the value as the slider's suffix (e.g. ` ms`); an
/// empty string renders no suffix.
fn render_number(
    field: &mut dyn bevy::reflect::PartialReflect,
    range: &crate::settings::def::NumberRange,
    unit: &str,
    ui: &mut egui::Ui,
) {
    let lo = range.min.unwrap_or(0.0);
    let hi = range.max.unwrap_or(1.0);
    let step = range.step;
    // egui renders the suffix verbatim; lead with a space so it reads "12 ms",
    // not "12ms". Empty unit → empty suffix → nothing shown.
    let suffix = if unit.is_empty() {
        String::new()
    } else {
        format!(" {unit}")
    };

    if let Some(v) = field.try_downcast_mut::<u32>() {
        let mut tmp = *v as i64;
        let mut slider = egui::Slider::new(&mut tmp, (lo as i64)..=(hi as i64)).suffix(&suffix);
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        if ui.add(slider).changed() {
            *v = tmp.max(0) as u32;
        }
    } else if let Some(v) = field.try_downcast_mut::<f32>() {
        let mut slider = egui::Slider::new(v, (lo as f32)..=(hi as f32)).suffix(&suffix);
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        ui.add(slider);
    } else if let Some(v) = field.try_downcast_mut::<f64>() {
        let mut slider = egui::Slider::new(v, lo..=hi).suffix(&suffix);
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        ui.add(slider);
    } else if let Some(v) = field.try_downcast_mut::<i32>() {
        let mut tmp = *v as i64;
        let mut slider = egui::Slider::new(&mut tmp, (lo as i64)..=(hi as i64)).suffix(&suffix);
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        if ui.add(slider).changed() {
            *v = tmp.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        }
    } else if let Some(v) = field.try_downcast_mut::<i64>() {
        let mut slider = egui::Slider::new(v, (lo as i64)..=(hi as i64)).suffix(&suffix);
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        ui.add(slider);
    } else {
        ui.label("(unsupported number type)");
    }
}

/// Render the boolean widget (checkbox) for a field. No label — Grid column 1
/// already holds it.
fn render_bool(field: &mut dyn bevy::reflect::PartialReflect, ui: &mut egui::Ui) {
    if let Some(v) = field.try_downcast_mut::<bool>() {
        ui.checkbox(v, "");
    } else {
        ui.label("(expected bool)");
    }
}

/// Render the colour widget for a field. No label — Grid column 1 already holds it.
fn render_color(field: &mut dyn bevy::reflect::PartialReflect, ui: &mut egui::Ui) {
    if let Some(v) = field.try_downcast_mut::<[f32; 4]>() {
        ui.color_edit_button_rgba_unmultiplied(v);
    } else if let Some(v) = field.try_downcast_mut::<bevy::color::Color>() {
        let mut rgba = v.to_srgba().to_f32_array();
        if ui.color_edit_button_rgba_unmultiplied(&mut rgba).changed() {
            *v = bevy::color::Color::srgba(rgba[0], rgba[1], rgba[2], rgba[3]);
        }
    } else {
        ui.label("(expected [f32; 4] or Color)");
    }
}

/// Render the text widget for a field. No label — Grid column 1 already holds it.
///
/// Fills the grid's value column (`desired_width = INFINITY`) rather than egui's
/// narrow default so long values like the flame name are comfortably editable.
fn render_text(field: &mut dyn bevy::reflect::PartialReflect, ui: &mut egui::Ui) {
    if let Some(v) = field.try_downcast_mut::<String>() {
        ui.add(egui::TextEdit::singleline(v).desired_width(f32::INFINITY));
    } else {
        ui.label("(expected String)");
    }
}

/// Render an editable string-list widget: per-row edit + up/down/remove,
/// plus an add button. Mutates the reflected `Vec<String>` in place.
fn render_text_list(field: &mut dyn bevy::reflect::PartialReflect, ui: &mut egui::Ui) {
    let Some(list) = field.try_downcast_mut::<Vec<String>>() else {
        ui.label("(expected Vec<String>)");
        return;
    };
    ui.vertical(|ui| {
        let len = list.len();
        let mut remove: Option<usize> = None;
        let mut swap: Option<(usize, usize)> = None;
        for (i, item) in list.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(item).desired_width(140.0));
                if ui.small_button("up").clicked() && i > 0 {
                    swap = Some((i, i - 1));
                }
                if ui.small_button("dn").clicked() && i + 1 < len {
                    swap = Some((i, i + 1));
                }
                if ui.small_button("x").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some((a, b)) = swap {
            list.swap(a, b);
        }
        if let Some(i) = remove {
            list.remove(i);
        }
        if ui.button("Add entry").clicked() {
            list.push(String::new());
        }
    });
}

/// Render the enum widget (`ComboBox`) for a field. No label — Grid column 1
/// already holds it.
///
/// `variants` is the `&'static` name list from [`SettingKind::Enum`] (derived
/// from the enum's reflection info by the macro), so the current variant is
/// matched against it without allocating. Selection writes back through
/// [`set_enum_variant`], which goes through the same reflected field handle as
/// every other widget — Bevy change detection, autosave, and restart diffing
/// all fire identically.
///
/// `(storage_key, field_name)` salts the `ComboBox` id so two enum settings in
/// one panel don't share popup state — field name alone is not enough, since
/// two settings structs may each declare a same-named enum field.
fn render_enum(
    field: &mut dyn bevy::reflect::PartialReflect,
    storage_key: &'static str,
    field_name: &'static str,
    variants: &[&'static str],
    ui: &mut egui::Ui,
) {
    use bevy::reflect::ReflectRef;

    // Resolve the current variant to its entry in the static `variants` list
    // so the ComboBox works on `&'static str` (no per-frame String clones).
    let current: Option<&'static str> = match field.reflect_ref() {
        ReflectRef::Enum(enum_ref) => variants
            .iter()
            .copied()
            .find(|v| *v == enum_ref.variant_name()),
        _ => None,
    };
    let Some(current) = current else {
        // Either the field is not an enum (macro misuse — already
        // debug_assert-ed in `enum_variant_names`) or the live variant is
        // missing from the metadata list (unreachable when the list comes
        // from the same enum's reflection info).
        ui.label("(expected unit-variant enum)");
        return;
    };

    let mut selected = current;
    egui::ComboBox::from_id_salt(("wc-setting-enum", storage_key, field_name))
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for &variant in variants {
                ui.selectable_value(&mut selected, variant, variant);
            }
        });
    if selected != current {
        set_enum_variant(field, field_name, selected);
    }
}

/// Write `variant` (a unit-variant name) into a reflected enum field.
///
/// Applies a payload-less [`bevy::reflect::enums::DynamicEnum`], which is exactly
/// the variant-switch operation `Reflect`-derived enums support for unit
/// variants. Returns `true` on success. Failure (a payload variant or a name
/// the enum doesn't have) leaves the field unchanged and logs a warning that
/// names the offending field — the loud debug-build failure for such misuse
/// already lives in [`crate::settings::def::enum_variant_names`].
fn set_enum_variant(
    field: &mut dyn bevy::reflect::PartialReflect,
    field_name: &str,
    variant: &str,
) -> bool {
    use bevy::reflect::enums::{DynamicEnum, DynamicVariant};

    let dynamic = DynamicEnum::new(variant, DynamicVariant::Unit);
    match field.try_apply(&dynamic) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(?err, field_name, variant, "enum setting write-back failed");
            false
        }
    }
}

/// Render the filesystem-path widget (`[file name][Browse…]`) for a field.
///
/// No label — Grid column 1 already holds it. The field stores the full path,
/// but the widget shows only the selected file's *name* (read-only); the path
/// is set entirely through the Browse… button — typing absolute paths is poor
/// kiosk UX. On Browse, opens [`rfd::FileDialog`] filtered to `extensions`; the
/// selected path replaces the field value, and an empty field reads as
/// `(none)`. Native only — the wasm build shows the file name without a picker
/// (web is out of scope; see `Cargo.toml`).
fn render_file_path(
    field: &mut dyn bevy::reflect::PartialReflect,
    #[cfg_attr(target_arch = "wasm32", allow(unused_variables))] filter_label: &str,
    #[cfg_attr(target_arch = "wasm32", allow(unused_variables))] extensions: &[&str],
    ui: &mut egui::Ui,
) {
    let Some(v) = field.try_downcast_mut::<String>() else {
        ui.label("(expected String for file path)");
        return;
    };
    ui.horizontal(|ui| {
        // Display the file name only (or "(none)"), not the full editable path.
        let file_name = if v.is_empty() {
            "(none)".to_owned()
        } else {
            std::path::Path::new(v.as_str())
                .file_name()
                .map_or_else(|| v.clone(), |name| name.to_string_lossy().into_owned())
        };
        ui.label(file_name);
        #[cfg(not(target_arch = "wasm32"))]
        if ui.button("Browse…").clicked() {
            let mut dlg = rfd::FileDialog::new();
            if !extensions.is_empty() {
                dlg = dlg.add_filter(filter_label, extensions);
            }
            if let Some(path) = dlg.pick_file() {
                *v = path.to_string_lossy().into_owned();
            }
        }
    });
}

/// Reflection branch for `Vec2` fields. Not yet reachable through the
/// `#[setting(...)]` attribute (no `SettingKind` variant); added eagerly so
/// the panel is ready when the next sketch needs it. The derive macro will
/// gain a `kind = Vec2` parser when that sketch lands.
///
/// Follows the Grid column-2 convention: no label here — Grid column 1 holds it.
#[allow(
    dead_code,
    reason = "preemptive support; reachable once `kind = Vec2` lands in the derive macro"
)]
fn render_vec2(field: &mut dyn bevy::reflect::PartialReflect, ui: &mut egui::Ui) {
    if let Some(v) = field.try_downcast_mut::<bevy::math::Vec2>() {
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut v.x).prefix("x: "));
            ui.add(egui::DragValue::new(&mut v.y).prefix("y: "));
        });
    } else {
        ui.label("(expected Vec2)");
    }
}

/// Reflection branch for `Vec3` fields. Not yet reachable through the
/// `#[setting(...)]` attribute (no `SettingKind` variant); added eagerly so
/// the panel is ready when the next sketch needs it. The derive macro will
/// gain a `kind = Vec3` parser when that sketch lands.
///
/// Follows the Grid column-2 convention: no label here — Grid column 1 holds it.
#[allow(
    dead_code,
    reason = "preemptive support; reachable once `kind = Vec3` lands in the derive macro"
)]
fn render_vec3(field: &mut dyn bevy::reflect::PartialReflect, ui: &mut egui::Ui) {
    if let Some(v) = field.try_downcast_mut::<bevy::math::Vec3>() {
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut v.x).prefix("x: "));
            ui.add(egui::DragValue::new(&mut v.y).prefix("y: "));
            ui.add(egui::DragValue::new(&mut v.z).prefix("z: "));
        });
    } else {
        ui.label("(expected Vec3)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::def::SettingsCategory;
    use bevy::reflect::Reflect;

    #[test]
    #[allow(
        clippy::panic,
        reason = "test assertion — panic on wrong variant is intentional"
    )]
    fn file_path_kind_dispatches() {
        let def = SettingDef {
            field_name: "path",
            label: "Path",
            unit: "",
            section: "",
            category: SettingsCategory::User,
            kind: SettingKind::FilePath {
                filter_label: "Image",
                extensions: &["png"],
            },
            requires_restart: false,
        };
        match def.kind {
            SettingKind::FilePath {
                filter_label,
                extensions,
            } => {
                assert_eq!(filter_label, "Image");
                assert_eq!(extensions, &["png"]);
            }
            _ => panic!("expected FilePath kind"),
        }
    }

    /// Unit-variant fixture for the enum write-back tests.
    #[derive(Reflect, Clone, Copy, Debug, PartialEq, Eq)]
    enum Palette {
        Warm,
        Cool,
        Mono,
    }

    #[test]
    fn set_enum_variant_switches_unit_variant() {
        let mut value = Palette::Warm;
        let field: &mut dyn bevy::reflect::PartialReflect = &mut value;
        assert!(set_enum_variant(field, "palette", "Mono"));
        assert_eq!(value, Palette::Mono);
    }

    #[test]
    fn set_enum_variant_rejects_unknown_name_and_leaves_value() {
        let mut value = Palette::Cool;
        let field: &mut dyn bevy::reflect::PartialReflect = &mut value;
        assert!(!set_enum_variant(field, "palette", "Sepia"));
        assert_eq!(value, Palette::Cool, "failed write-back must not mutate");
    }

    /// Pins the contract the `ComboBox` relies on: every name in the
    /// reflection-derived variant list (what the macro bakes into
    /// `SettingKind::Enum { variants }`) is applicable through
    /// `set_enum_variant`. If the metadata list and the write-back path ever
    /// disagree about an enum's definition, a dropdown selection would
    /// silently no-op; this catches that drift.
    ///
    /// (Replaces a former `enum_kind_dispatches` test that only matched a
    /// locally-constructed `SettingDef` against itself.)
    #[test]
    fn every_listed_variant_is_writable() {
        let variants = crate::settings::def::enum_variant_names::<Palette>();
        assert_eq!(variants, &["Warm", "Cool", "Mono"]);
        let mut value = Palette::Warm;
        for &name in variants {
            let field: &mut dyn bevy::reflect::PartialReflect = &mut value;
            assert!(
                set_enum_variant(field, "palette", name),
                "variant `{name}` from the metadata list failed to apply"
            );
        }
        assert_eq!(value, Palette::Mono, "last applied variant should stick");
    }
}
