//! Reflection-driven walker that turns a settings struct's `SettingDef` table
//! into labelled Grid rows.
//!
//! [`render_section_by_key`] looks up the registered settings type for a
//! storage key via the `TypeRegistry`, then [`render_user_fields_via_reflect`]
//! walks its fields through `bevy_reflect::ReflectMut`, grouping them by
//! section and dispatching each field's value widget to
//! [`super::widgets::render_widget_value`]. Per-row modified/reset handling
//! ([`field_differs_from_default`], [`render_label_cell`],
//! [`render_reset_cell`]) lives here too since it is part of the same walk.

use std::sync::Arc;

use bevy::prelude::*;
use bevy::reflect::ReflectMut;
use bevy_egui::egui;
use egui_phosphor::regular as phosphor;

use super::provider_status::{
    render_backend_status_row, render_provider_status_row, HandTrackingStatus, ProviderStatusLine,
    BACKEND_FIELD_NAME,
};
use super::widgets::render_widget_value;
use crate::settings::def::{SettingDef, SettingKind, SettingsCategory};
use crate::settings::registry::{reflect_resource_mut, SettingsRegistry};
use crate::settings::runtime_enum::{
    self, RuntimeEnumOptionsSnapshot, RuntimeEnumOptionsSnapshotEntry,
};
use crate::ui::OverlayStyle;

/// Look up the type registration matching `storage_key` and render its
/// `User`-category fields. Walks the `TypeRegistry` to find the registered
/// settings type whose `STORAGE_KEY` matches; uses reflection to
/// read/write fields without static type knowledge.
///
/// `hand_tracking_status` is the pre-snapshotted hand-tracking state, threaded
/// through to the two read-only rows under the "Tracking provider" and
/// "Inference backend" dropdowns (see [`super::provider_status`]).
pub(super) fn render_section_by_key(
    world: &mut World,
    ui: &mut egui::Ui,
    storage_key: &'static str,
    hand_tracking_status: HandTrackingStatus,
    #[cfg(feature = "templates")] template_rows: &[crate::templates::view::TemplateRow],
    #[cfg(feature = "templates")] template_dirty: &mut bool,
    advanced: bool,
    style: &OverlayStyle,
) {
    // Snapshot the entry's defs as an Arc handle so the registry resource
    // stays unborrowed while we re-enter `world` for reflection. Cloning an
    // `Arc<[SettingDef]>` is a refcount bump, not a Vec copy.
    let defs: Arc<[SettingDef]> = match world
        .get_resource::<SettingsRegistry>()
        .and_then(|r| r.entries.iter().find(|e| e.storage_key == storage_key))
    {
        Some(entry) => Arc::clone(&entry.def),
        None => return,
    };
    // Nothing to show when no field is visible at the current Advanced state
    // (e.g. a Dev-only struct while Advanced is off).
    if !defs.iter().any(|d| super::dock::field_visible(d, advanced)) {
        return;
    }

    // Walk the type registry to find the settings type by its
    // SketchSettings::STORAGE_KEY. Compare by value, not pointer identity.
    let type_id = world
        .resource::<AppTypeRegistry>()
        .read()
        .iter()
        .find_map(|reg| settings_type_id_for_key(reg, storage_key));
    let Some(type_id) = type_id else {
        ui.label("(settings type not in TypeRegistry — register via App::register_type)");
        return;
    };

    // Get a Reflect handle on the resource, plus a default instance for
    // modified-from-default detection and reset. Clone the Arc so the read
    // guard doesn't borrow `world`; build the default while the guard is alive
    // so the owned `Box` outlives the `drop` below.
    let registry = world.resource::<AppTypeRegistry>().clone();
    // A fresh default instance, available when the type registered
    // `#[reflect(Default)]`. Absent → rows degrade to no bold / no reset glyph,
    // never a hard failure. Built while the read guard is alive so the owned
    // `Box` outlives it.
    let default_instance: Option<Box<dyn Reflect>> = registry
        .read()
        .get_type_data::<bevy::reflect::std_traits::ReflectDefault>(type_id)
        .map(bevy::reflect::std_traits::ReflectDefault::default);

    // Snapshot every registered runtime-enum options source now, while
    // `world` is still a shared borrow -- once `reflect_mut` below is taken,
    // `world` is borrowed for the rest of this function and no widget below
    // can re-enter it. Mirrors the `defs` snapshot near the top of this
    // function. Deferred past the TypeRegistry bail above so a section
    // whose settings type isn't registered doesn't pay for a snapshot it
    // can't use; it cannot also be deferred past the "resource not present"
    // bail below, since that bail is discovered *by* the
    // `reflect_resource_mut` call this snapshot must precede. See
    // `crate::settings::runtime_enum` for the registration side.
    //
    // Skipped entirely for a section with no runtime-enum field, which is
    // every section today: `snapshot` deep-copies each source's option
    // Strings (see `RuntimeEnumOptionsSnapshotEntry::options`), and this runs
    // per rendered section, per frame. An empty snapshot is the correct input
    // for such a section anyway -- `options_for` on it yields an empty slice,
    // and no widget in the section asks.
    let runtime_enum_options = if defs
        .iter()
        .any(|d| matches!(d.kind, SettingKind::RuntimeEnum { .. }))
    {
        runtime_enum::snapshot(world)
    } else {
        RuntimeEnumOptionsSnapshot::new()
    };

    // Bevy 0.19 made `ReflectResource` a ZST; resources are now reflected via
    // `ReflectComponent` on their backing entity (see `reflect_resource_mut`).
    let Some(mut reflect_mut) = reflect_resource_mut(world, type_id) else {
        ui.label("(resource not present)");
        return;
    };
    // Deref `Mut<dyn Reflect>` to get `&mut dyn Reflect`.
    render_user_fields_via_reflect(
        &mut *reflect_mut,
        defs.as_ref(),
        storage_key,
        hand_tracking_status,
        &runtime_enum_options,
        #[cfg(feature = "templates")]
        template_rows,
        #[cfg(feature = "templates")]
        template_dirty,
        default_instance.as_deref(),
        advanced,
        style,
        ui,
    );
}

/// Walk `reflect` (a `&mut dyn Reflect` over the settings struct) and render
/// each visible field as a typed widget, grouped under section headers.
///
/// `advanced` controls which fields are visible: `User` fields always, `Dev`
/// fields only when the Advanced toggle is on (and then with a dimmed label, so
/// they read as the secondary layer). Fields with the same `section` name are
/// clustered together under an uppercase section header label. Fields with
/// `section == ""` are rendered first in an unlabeled group (no header).
/// Section order follows the first appearance of each section name in `defs`.
///
/// Each section uses its own `egui::Grid` with three columns: the label
/// (accent-highlighted when the field differs from its default, with a restart
/// badge when the field requires a restart), the value widget, and a
/// reset-to-default glyph (shown only when modified; an aligned spacer otherwise).
///
/// `default` is a fresh default instance of the same settings struct (from
/// `#[reflect(Default)]`), used to detect modification and to power the reset
/// glyph. `None` when the type did not register a default — rows then render
/// without the bold / reset affordances.
///
/// `storage_key` salts every egui id created below (Grids, `ComboBox`es) so
/// that two settings structs using the same section or field names don't
/// collide in egui's id-to-state map (colliding Grids share column widths;
/// colliding `ComboBox`es share popup open/close state).
#[expect(
    clippy::too_many_arguments,
    reason = "the settings render chain threads the hand-tracking status snapshot, the runtime-enum options snapshot, and (when the `templates` feature is on) the template rows + dirty flag through this fn; bundling them into a struct is a larger refactor out of scope here"
)]
fn render_user_fields_via_reflect(
    reflect: &mut dyn Reflect,
    defs: &[SettingDef],
    storage_key: &'static str,
    hand_tracking_status: HandTrackingStatus,
    runtime_enum_options: &[RuntimeEnumOptionsSnapshotEntry],
    #[cfg(feature = "templates")] template_rows: &[crate::templates::view::TemplateRow],
    #[cfg(feature = "templates")] template_dirty: &mut bool,
    default: Option<&dyn Reflect>,
    advanced: bool,
    style: &OverlayStyle,
    ui: &mut egui::Ui,
) {
    use bevy::reflect::ReflectRef;

    let default_struct = match default.map(|d| d.reflect_ref()) {
        Some(ReflectRef::Struct(s)) => Some(s),
        _ => None,
    };

    let ReflectMut::Struct(struct_mut) = reflect.reflect_mut() else {
        ui.label("(settings is not a struct)");
        return;
    };

    // Pass 1: collect section names in order of first appearance among visible
    // fields. `""` (no section) always sorts first when present.
    let mut section_order: Vec<&'static str> = Vec::new();
    for def in defs
        .iter()
        .filter(|d| super::dock::field_visible(d, advanced))
    {
        if !section_order.contains(&def.section) {
            section_order.push(def.section);
        }
    }

    // Pass 2: render each section as a labelled block with its own Grid.
    for (idx, &section_name) in section_order.iter().enumerate() {
        if idx > 0 {
            ui.add_space(8.0);
        }
        if !section_name.is_empty() {
            ui.label(
                egui::RichText::new(section_name.to_uppercase())
                    .size(11.0)
                    .strong(),
            );
            ui.add_space(4.0);
        }

        // Tuple id salt (no per-frame `format!` allocation) including the
        // settings struct's storage key — two structs may both use e.g. a
        // "Hand Tracking" section name without sharing Grid layout state.
        egui::Grid::new(("settings_form", storage_key, section_name))
            .num_columns(3)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                for def in defs.iter().filter(|d| {
                    super::dock::field_visible(d, advanced) && d.section == section_name
                }) {
                    let Some(field) = struct_mut.field_mut(def.field_name) else {
                        continue;
                    };
                    let default_field = default_struct.and_then(|s| s.field(def.field_name));
                    let modified = field_differs_from_default(field, default_field);
                    let is_dev = def.category == SettingsCategory::Dev;
                    // Column 1: label (+ restart badge), highlighted when
                    // modified, dimmed when it is an Advanced (Dev) field.
                    render_label_cell(ui, def, modified, is_dev, style);
                    // Column 2: the value widget.
                    render_widget_value(
                        field,
                        def,
                        storage_key,
                        runtime_enum_options,
                        #[cfg(feature = "templates")]
                        template_rows,
                        #[cfg(feature = "templates")]
                        template_dirty,
                        #[cfg(feature = "templates")]
                        style,
                        ui,
                    );
                    // Column 3: reset-to-default glyph, or an aligned spacer.
                    render_reset_cell(ui, field, default_field, modified, style);
                    ui.end_row();

                    render_hand_tracking_status_row(
                        ui,
                        storage_key,
                        def,
                        hand_tracking_status,
                        style,
                    );
                }
            });
    }
}

/// Emit the read-only Hand Tracking status row that belongs directly under the
/// field just rendered, if any (see [`super::provider_status`]).
///
/// - Under "Tracking provider": a spinner while the `MediaPipe` backend loads its
///   models and opens the camera (~1-2 s with no tracking), a red note when the
///   provider failed. No row while healthy.
/// - Under "Inference backend": the EP the sessions actually registered on, amber
///   when one model degraded to the CPU. This one *does* show while healthy — a
///   kiosk quietly running palm detection on the CPU for an eight-hour soak is
///   otherwise indistinguishable from one on the GPU, since tracking is `Active`
///   either way and the provider row above stays silent.
///
/// Called from inside the section's `egui::Grid`, so it owns the whole row: the
/// empty label cell, the widget cell, the column-3 spacer, and `end_row`.
fn render_hand_tracking_status_row(
    ui: &mut egui::Ui,
    storage_key: &'static str,
    def: &SettingDef,
    status: HandTrackingStatus,
    style: &OverlayStyle,
) {
    if storage_key != ProviderStatusLine::STORAGE_KEY {
        return;
    }
    match (def.field_name, status.provider, status.backend) {
        (ProviderStatusLine::FIELD_NAME, Some(line), _) => {
            ui.label(""); // column 1: keep the grid aligned
            render_provider_status_row(ui, line, style);
            end_status_row(ui);
        }
        (BACKEND_FIELD_NAME, _, Some(backend)) => {
            ui.label("");
            render_backend_status_row(ui, backend, style);
            end_status_row(ui);
        }
        _ => {}
    }
}

/// Close a status row: the column-3 spacer plus `end_row`. The spacer is an
/// `allocate_exact_size`, not `add_space` — the latter panics inside an
/// `egui::Grid` (see [`render_reset_cell`]).
fn end_status_row(ui: &mut egui::Ui) {
    ui.allocate_exact_size(egui::vec2(18.0, 0.0), egui::Sense::hover());
    ui.end_row();
}

/// Whether a field's current value differs from its struct default.
///
/// Conservative: an absent default (a type without `#[reflect(Default)]`) or an
/// undecidable comparison reads as *not* modified, so the row never shows a
/// spurious bold label or reset glyph.
fn field_differs_from_default(
    field: &dyn bevy::reflect::PartialReflect,
    default_field: Option<&dyn bevy::reflect::PartialReflect>,
) -> bool {
    match default_field {
        Some(df) => !field.reflect_partial_eq(df).unwrap_or(true),
        None => false,
    }
}

/// Render Grid column 1: the field label, accent-highlighted when the value
/// differs from its default and dimmed when it is an Advanced (`Dev`) field,
/// followed by an amber restart badge when the field requires a restart.
///
/// Highlight, not weight: only `Inter-Regular` is loaded and egui has no
/// faux-bold, so `.strong()` would not change the glyph weight — it only shifts
/// colour, which our explicit label colour already pins. A modified field is
/// therefore marked by the accent colour (the dock's signature) rather than by
/// bold. Loading an Inter bold/semibold face is the path to true weight.
///
/// Precedence: a modified field shows the accent even when it is a `Dev` field
/// (the "you changed this" signal outranks the "this is advanced" dimming).
fn render_label_cell(
    ui: &mut egui::Ui,
    def: &SettingDef,
    modified: bool,
    is_dev: bool,
    style: &OverlayStyle,
) {
    ui.horizontal(|ui| {
        let color = if modified {
            style.accent_bright
        } else if is_dev {
            style.text_faint
        } else {
            style.text_primary
        };
        ui.label(egui::RichText::new(def.label).color(color));
        if def.requires_restart {
            ui.label(
                egui::RichText::new(phosphor::ARROW_CLOCKWISE)
                    .family(egui::FontFamily::Name("phosphor".into()))
                    .size(10.0)
                    .color(style.warn_amber),
            )
            .on_hover_text("Takes effect after restart");
        }
    });
}

/// Render Grid column 3: a frameless reset-to-default glyph when the field is
/// modified, or a fixed-width spacer otherwise so the column stays aligned.
///
/// The reset writes the default back through the same reflected field handle as
/// every widget, so Bevy change detection, autosave, and restart diffing all
/// fire identically. `try_apply` cannot fail here — `default_field` is the same
/// field from a default instance of the same type.
fn render_reset_cell(
    ui: &mut egui::Ui,
    field: &mut dyn bevy::reflect::PartialReflect,
    default_field: Option<&dyn bevy::reflect::PartialReflect>,
    modified: bool,
    style: &OverlayStyle,
) {
    match (modified, default_field) {
        (true, Some(df)) => {
            let glyph = egui::RichText::new(phosphor::ARROW_COUNTER_CLOCKWISE)
                .family(egui::FontFamily::Name("phosphor".into()))
                .size(12.0)
                .color(style.text_secondary);
            if ui
                .add(egui::Button::new(glyph).frame(false))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Reset to default")
                .clicked()
            {
                if let Err(err) = field.try_apply(df) {
                    tracing::warn!(?err, "settings reset-to-default write-back failed");
                }
            }
        }
        // Keep the reset column's width stable whether or not the glyph shows.
        // `add_space` panics inside an `egui::Grid`; allocate an empty
        // fixed-width cell instead.
        _ => {
            ui.allocate_exact_size(egui::vec2(18.0, 0.0), egui::Sense::hover());
        }
    }
}

/// Look up a `TypeRegistration`'s `SketchSettings::STORAGE_KEY` and return its
/// `TypeId` if it matches.
///
/// The derive macro emits the storage key as a const associated to the
/// trait, not as type-registration metadata. We sidestep that by storing
/// the `(type_id, storage_key)` mapping at registration time via
/// [`crate::settings::registry::SettingsTypeKey`] type-data.
fn settings_type_id_for_key(
    reg: &bevy::reflect::TypeRegistration,
    storage_key: &str,
) -> Option<std::any::TypeId> {
    use crate::settings::registry::SettingsTypeKey;
    let data = reg.data::<SettingsTypeKey>()?;
    (data.0 == storage_key).then(|| reg.type_id())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Modified-from-default detection: equal reads unmodified, differing reads
    /// modified, and an absent default degrades to unmodified (no bold/reset).
    #[test]
    fn field_modified_detection() {
        use bevy::reflect::PartialReflect;
        let live: f32 = 0.5;
        let same: f32 = 0.5;
        let diff: f32 = 0.9;
        let same_ref: &dyn PartialReflect = &same;
        let diff_ref: &dyn PartialReflect = &diff;
        assert!(
            !field_differs_from_default(&live, Some(same_ref)),
            "value equal to default is not modified"
        );
        assert!(
            field_differs_from_default(&live, Some(diff_ref)),
            "value differing from default is modified"
        );
        assert!(
            !field_differs_from_default(&live, None),
            "no default available degrades to not-modified"
        );
    }

    /// Regression: the reset cell's unmodified branch must use a grid-safe
    /// allocation, not `ui.add_space`, which panics ("makes no sense in a grid
    /// layout") the moment the settings panel opens. Render the cell inside a
    /// real `egui::Grid` and assert the frame completes.
    #[test]
    fn reset_cell_unmodified_branch_is_grid_safe() {
        let ctx = egui::Context::default();
        let style = OverlayStyle::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::Grid::new("reset_cell_test")
                .num_columns(3)
                .show(ui, |ui| {
                    let mut field: f32 = 0.5;
                    ui.label("label");
                    ui.label("widget");
                    // Unmodified → the empty-cell branch (the crash path).
                    render_reset_cell(ui, &mut field, None, false, &style);
                    ui.end_row();
                });
        });
        // Reaching here without a panic is the assertion.
    }
}
