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
/// to widgets that need a unique egui id (currently [`render_enum`] and
/// [`render_runtime_enum`]).
///
/// `runtime_enum_options` is the whole-panel runtime-enum options snapshot,
/// forwarded to [`render_runtime_enum`] for `SettingKind::RuntimeEnum`
/// fields; every other kind ignores it.
pub(super) fn render_widget_value(
    field: &mut dyn bevy::reflect::PartialReflect,
    def: &SettingDef,
    storage_key: &'static str,
    runtime_enum_options: &[crate::settings::runtime_enum::RuntimeEnumOptionsSnapshotEntry],
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
        SettingKind::RuntimeEnum { options_key } => {
            render_runtime_enum(
                field,
                storage_key,
                def.field_name,
                options_key,
                runtime_enum_options,
                ui,
            );
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

/// Render the runtime-enumerated dropdown (`ComboBox`) for a field whose
/// candidate list is supplied by a registered
/// `crate::settings::RuntimeEnumOptionsSource` at render time, rather than
/// known at compile time (contrast [`render_enum`]). No label — Grid column 1
/// already holds it.
///
/// `runtime_enum_options` is the whole-panel snapshot taken once per rendered
/// section in `super::fields::render_section_by_key`, before the reflected field borrow
/// it needs `world` for makes `world` unavailable down here; `options_key`
/// selects this field's entry out of it via
/// `crate::settings::runtime_enum::options_for`.
///
/// The persisted value is never silently replaced:
/// `classify_runtime_enum_selection` decides whether it is in the live
/// list, and when it is not (source hasn't enumerated yet, device is
/// asleep/unplugged, or the name was typed by hand), it is still shown in the
/// `ComboBox` — marked "(unavailable)" — and stays selected. A `TextEdit`
/// alongside the `ComboBox` is the free-text escape hatch for exactly that
/// case: it lets the operator retype or correct the value directly instead of
/// waiting on enumeration. It is rendered **only** in that case — see
/// [`shows_free_text_escape_hatch`].
fn render_runtime_enum(
    field: &mut dyn bevy::reflect::PartialReflect,
    storage_key: &'static str,
    field_name: &'static str,
    options_key: &'static str,
    runtime_enum_options: &[crate::settings::runtime_enum::RuntimeEnumOptionsSnapshotEntry],
    ui: &mut egui::Ui,
) {
    let Some(current) = field.try_downcast_mut::<String>() else {
        ui.label("(expected String)");
        return;
    };
    let options = crate::settings::runtime_enum::options_for(runtime_enum_options, options_key);
    let selection = classify_runtime_enum_selection(current.as_str(), options);
    let mut selected = current.clone();

    ui.horizontal(|ui| {
        let combo_label = runtime_enum_combo_label(selection, current.as_str());
        egui::ComboBox::from_id_salt(("wc-setting-runtime-enum", storage_key, field_name))
            .selected_text(combo_label)
            .show_ui(ui, |ui| {
                if selection == RuntimeEnumSelection::Unavailable {
                    let label = runtime_enum_combo_label(selection, current.as_str());
                    ui.selectable_value(&mut selected, current.clone(), label);
                }
                for opt in options {
                    ui.selectable_value(&mut selected, opt.clone(), opt.as_str());
                }
            });
        // Free-text escape hatch — only when the dropdown cannot do the job.
        if shows_free_text_escape_hatch(selection) {
            ui.add(egui::TextEdit::singleline(&mut selected).desired_width(120.0));
        }
    });

    if selected != *current {
        *current = selected;
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

/// Whether a runtime-enumerated field's persisted value is currently present
/// in its live option list. Pure and UI-free so the "never silently rewrite
/// an unresolved name" contract (an HDMI TV that is merely asleep must not
/// lose its saved binding — see `AGENTS.md`) is unit-tested directly, without
/// an egui context or a GPU.
///
/// Consumed by [`render_runtime_enum`], which turns each case into the
/// dropdown's selected-text and option list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeEnumSelection {
    /// No value persisted yet (`current` is empty).
    Empty,
    /// `current` matches one of the live options.
    Known,
    /// `current` is non-empty but does not appear in the live option list.
    /// Never treated as "reset me" — the caller must keep showing it and
    /// keep it selectable/editable.
    Unavailable,
}

/// Classify `current` against `options`. See [`RuntimeEnumSelection`].
fn classify_runtime_enum_selection(current: &str, options: &[String]) -> RuntimeEnumSelection {
    if current.is_empty() {
        RuntimeEnumSelection::Empty
    } else if options.iter().any(|o| o == current) {
        RuntimeEnumSelection::Known
    } else {
        RuntimeEnumSelection::Unavailable
    }
}

/// Whether [`render_runtime_enum`] should render its free-text `TextEdit`
/// alongside the `ComboBox`.
///
/// Only when the dropdown cannot express the operator's intent: nothing is
/// selected yet ([`RuntimeEnumSelection::Empty`]) or the persisted value is not
/// in the live list ([`RuntimeEnumSelection::Unavailable`] — a monitor that is
/// asleep, an audio device that is unplugged, a name typed ahead of enumeration).
/// That is the only case the escape hatch exists to serve.
///
/// On a [`RuntimeEnumSelection::Known`] value the box is not merely redundant,
/// it is actively harmful. The widget writes the field back on **every
/// keystroke**, and `lifecycle::display::apply_display_mode` re-derives
/// `Window::mode` from `DisplaySettings::monitor` every frame. So typing `LG TV`
/// into it walks the value through `"L"`, `"LG"`, … — each an unresolvable name
/// that falls back to `MonitorSelection::Current` — and the OS window physically
/// hops to the current monitor and back on the final keystroke. Real
/// `set_fullscreen` calls, not just component churn.
///
/// Pure and UI-free so it is unit-tested directly: whether a `TextEdit` was added
/// to an `egui::Ui` is not observable headlessly, but this decision is.
fn shows_free_text_escape_hatch(selection: RuntimeEnumSelection) -> bool {
    match selection {
        RuntimeEnumSelection::Empty | RuntimeEnumSelection::Unavailable => true,
        RuntimeEnumSelection::Known => false,
    }
}

/// The `ComboBox`'s collapsed label for a runtime-enumerated field. Pure and
/// UI-free so the "(unavailable)" marker — the visible half of the
/// never-silently-drop contract — is unit-tested without an egui context.
///
/// [`render_runtime_enum`] calls this for both the collapsed `selected_text`
/// and the `Unavailable` entry's popup label, so the two can never drift out
/// of agreement with each other.
fn runtime_enum_combo_label(selection: RuntimeEnumSelection, current: &str) -> String {
    match selection {
        RuntimeEnumSelection::Empty => "(none)".to_owned(),
        RuntimeEnumSelection::Known => current.to_owned(),
        RuntimeEnumSelection::Unavailable => format!("{current} (unavailable)"),
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

    #[test]
    fn classify_runtime_enum_selection_cases() {
        let opts = vec!["Speakers".to_owned(), "HDMI TV".to_owned()];
        assert_eq!(
            classify_runtime_enum_selection("", &opts),
            RuntimeEnumSelection::Empty
        );
        assert_eq!(
            classify_runtime_enum_selection("HDMI TV", &opts),
            RuntimeEnumSelection::Known
        );
        assert_eq!(
            classify_runtime_enum_selection("Living Room TV", &opts),
            RuntimeEnumSelection::Unavailable,
            "a persisted name absent from the live list must classify as \
             Unavailable, never silently dropped"
        );
    }

    #[test]
    fn classify_runtime_enum_selection_with_empty_live_list_is_unavailable_not_empty() {
        // No source registered yet, or enumeration hasn't run: the live
        // list is empty, but a persisted (non-empty) value must still
        // classify as Unavailable, not Empty -- Empty means "nothing
        // persisted," a different UI state (no "(unavailable)" marker, no
        // reset-cell affordance driven off it).
        assert_eq!(
            classify_runtime_enum_selection("Living Room TV", &[]),
            RuntimeEnumSelection::Unavailable
        );
    }

    /// Pins Fix 3: the free-text box appears only where it is needed. A `Known`
    /// selection must render no `TextEdit` — the per-keystroke write-back plus
    /// `apply_display_mode`'s every-frame re-derive makes typing into it hop the
    /// OS window between monitors. An `Unavailable` (or not-yet-set) value still
    /// gets the escape hatch, which is the case it exists for.
    ///
    /// Pinned on the pure decision rather than on the rendered `egui::Ui`:
    /// whether a `TextEdit` was added is not observable from egui's headless
    /// output (no widget inventory in `FullOutput`), so testing the render would
    /// mean asserting nothing.
    #[test]
    fn the_free_text_escape_hatch_is_shown_only_when_the_dropdown_cannot_serve() {
        assert!(
            !shows_free_text_escape_hatch(RuntimeEnumSelection::Known),
            "a healthy dropdown selection must not also render a free-text box"
        );
        assert!(
            shows_free_text_escape_hatch(RuntimeEnumSelection::Unavailable),
            "an unavailable value must stay hand-editable"
        );
        assert!(
            shows_free_text_escape_hatch(RuntimeEnumSelection::Empty),
            "an unset value must be typeable before the source enumerates"
        );
    }

    #[test]
    fn runtime_enum_combo_label_empty_reads_none() {
        assert_eq!(
            runtime_enum_combo_label(RuntimeEnumSelection::Empty, ""),
            "(none)"
        );
    }

    #[test]
    fn runtime_enum_combo_label_known_reads_the_bare_value() {
        assert_eq!(
            runtime_enum_combo_label(RuntimeEnumSelection::Known, "Speakers"),
            "Speakers"
        );
    }

    #[test]
    fn runtime_enum_combo_label_unavailable_carries_the_marker() {
        // The visible half of the never-silently-drop contract: a persisted
        // name absent from the live list must still read in the ComboBox,
        // flagged so the operator knows it isn't currently selectable from
        // the live list.
        assert_eq!(
            runtime_enum_combo_label(RuntimeEnumSelection::Unavailable, "Living Room TV"),
            "Living Room TV (unavailable)"
        );
    }

    #[test]
    fn render_runtime_enum_keeps_a_known_value_unmutated() {
        // Mirrors `render_runtime_enum_keeps_persisted_value_when_absent_from_live_list`
        // below, but for the `Known` case: a value present in the live list
        // must also survive a render unmutated.
        let ctx = egui::Context::default();
        let mut value = String::from("Speakers");
        let snapshot = [
            crate::settings::runtime_enum::RuntimeEnumOptionsSnapshotEntry {
                options_key: "audio_output_devices",
                options: std::sync::Arc::from(["Speakers".to_owned()]),
            },
        ];
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let field: &mut dyn bevy::reflect::PartialReflect = &mut value;
            render_runtime_enum(
                field,
                "audio",
                "output_device",
                "audio_output_devices",
                &snapshot,
                ui,
            );
        });
        assert_eq!(
            value, "Speakers",
            "a value present in the live list must survive the render unmutated"
        );
    }

    #[test]
    fn render_runtime_enum_keeps_persisted_value_when_absent_from_live_list() {
        let ctx = egui::Context::default();
        let mut value = String::from("Living Room TV");
        let snapshot = [
            crate::settings::runtime_enum::RuntimeEnumOptionsSnapshotEntry {
                options_key: "audio_output_devices",
                options: std::sync::Arc::from(["Speakers".to_owned()]),
            },
        ];
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let field: &mut dyn bevy::reflect::PartialReflect = &mut value;
            render_runtime_enum(
                field,
                "audio",
                "output_device",
                "audio_output_devices",
                &snapshot,
                ui,
            );
        });
        assert_eq!(
            value, "Living Room TV",
            "a persisted name absent from the live list must survive the render, not reset"
        );
    }

    #[test]
    fn render_runtime_enum_on_a_non_string_field_labels_the_mismatch_and_does_not_panic() {
        let ctx = egui::Context::default();
        let mut value: u32 = 7;
        let snapshot: [crate::settings::runtime_enum::RuntimeEnumOptionsSnapshotEntry; 0] = [];
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let field: &mut dyn bevy::reflect::PartialReflect = &mut value;
            render_runtime_enum(field, "s", "f", "k", &snapshot, ui);
        });
        // Reaching here without a panic is the assertion (mirrors the
        // existing `(unsupported number type)` / `(expected bool)` degrade
        // pattern the other render_* helpers use on a type mismatch).
    }
}
