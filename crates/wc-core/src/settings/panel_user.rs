//! Curated user-facing settings panel.
//!
//! Walks [`super::SettingsRegistry`] each frame and, for each registered
//! settings resource, renders an `egui::CollapsingHeader` containing typed
//! widgets for every `SettingDef` whose `category == User`. Field values are
//! read and written through `bevy_reflect::ReflectMut` so this panel works
//! for any settings type without per-struct dispatch code.
//!
//! ## Why reflection
//!
//! Plan 5 shipped a typed-match-ladder version that only knew how to render
//! `TestSketchSettings`. Plan 6 (Line) makes that approach untenable: every
//! sketch would add another monomorphized renderer with another match
//! ladder. Reflection drives a single walker that consumes the metadata
//! table the derive macro already emits.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    reason = "egui sliders use f32/i64 widget ranges; bounds-checked against SettingDef metadata"
)]

use bevy::prelude::*;
use bevy::reflect::ReflectMut;
use bevy_egui::{egui, EguiContexts};

use super::def::{SettingDef, SettingKind, SettingsCategory};
use super::registry::SettingsRegistry;

/// Plugin assembly hook called by [`super::SettingsPlugin::build`].
///
/// Scheduled inside `bevy_egui::EguiPrimaryContextPass`. See [`super::panel_dev`]
/// for the same scheduling rationale.
pub(super) fn add_systems(app: &mut App) {
    app.add_systems(bevy_egui::EguiPrimaryContextPass, draw_user_panel);
}

/// Exclusive system that draws the user panel.
fn draw_user_panel(world: &mut World) {
    // Skip when no egui context is up (e.g., MinimalPlugins test harness).
    if !world.contains_resource::<bevy_egui::EguiUserTextures>() {
        return;
    }
    // Snapshot the storage keys we need to iterate so the registry resource
    // stays unborrowed while we mutate per-type resources.
    let keys: Vec<&'static str> = world
        .get_resource::<SettingsRegistry>()
        .map(|r| r.entries.iter().map(|e| e.storage_key).collect())
        .unwrap_or_default();
    if keys.is_empty() {
        return;
    }

    let mut state: bevy::ecs::system::SystemState<EguiContexts<'_, '_>> =
        bevy::ecs::system::SystemState::new(world);
    let mut contexts = state.get_mut(world);
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let ctx = ctx.clone();
    state.apply(world);

    egui::Window::new("Settings")
        .id(egui::Id::new("wc-settings-user-panel"))
        .default_open(true)
        .show(&ctx, |ui| {
            for key in keys {
                render_section_by_key(world, ui, key);
            }
        });
}

/// Look up the type registration matching `storage_key` and render its
/// `User`-category fields. Walks the `TypeRegistry` to find the registered
/// settings type whose `SETTINGS_STORAGE_KEY` matches; uses reflection to
/// read/write fields without static type knowledge.
fn render_section_by_key(world: &mut World, ui: &mut egui::Ui, storage_key: &'static str) {
    // Find the entry's defs (still a borrow of the resource; copy out).
    let defs: Vec<SettingDef> = world
        .get_resource::<SettingsRegistry>()
        .and_then(|r| r.entries.iter().find(|e| e.storage_key == storage_key))
        .map(|e| e.def.clone())
        .unwrap_or_default();
    if defs.iter().all(|d| d.category != SettingsCategory::User) {
        return;
    }

    ui.collapsing(storage_key, |ui| {
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

        // Get a Reflect handle on the resource.
        // Clone the Arc so the read guard doesn't borrow `world`.
        let registry = world.resource::<AppTypeRegistry>().clone();
        let registry_read = registry.read();
        let Some(type_data) =
            registry_read.get_type_data::<bevy::ecs::reflect::ReflectResource>(type_id)
        else {
            ui.label("(no ReflectResource on settings type)");
            return;
        };
        // `&mut World` implements `Into<FilteredResourcesMut>`, so this is
        // safe to call without any unsafe code.
        let reflect_result = type_data.reflect_mut(world);
        drop(registry_read);
        let Ok(mut reflect_mut) = reflect_result else {
            ui.label("(resource not present)");
            return;
        };
        // Deref `Mut<dyn Reflect>` to get `&mut dyn Reflect`.
        render_user_fields_via_reflect(&mut *reflect_mut, &defs, ui);
    });
}

/// Walk `reflect` (a `&mut dyn Reflect` over the settings struct) and render
/// each user-category field as a typed widget.
fn render_user_fields_via_reflect(
    reflect: &mut dyn Reflect,
    defs: &[SettingDef],
    ui: &mut egui::Ui,
) {
    let ReflectMut::Struct(struct_mut) = reflect.reflect_mut() else {
        ui.label("(settings is not a struct)");
        return;
    };

    for def in defs.iter().filter(|d| d.category == SettingsCategory::User) {
        let Some(field) = struct_mut.field_mut(def.field_name) else {
            continue;
        };
        render_widget(field, def, ui);
    }
}

/// Render one widget into `field` based on the metadata in `def`.
///
/// `field` is `&mut dyn PartialReflect` as returned by [`Struct::field_mut`].
fn render_widget(
    field: &mut dyn bevy::reflect::PartialReflect,
    def: &SettingDef,
    ui: &mut egui::Ui,
) {
    match &def.kind {
        SettingKind::Number(range) => render_number(field, def.label, range, ui),
        SettingKind::Boolean => render_bool(field, def.label, ui),
        SettingKind::Color => render_color(field, def.label, ui),
        SettingKind::Text => render_text(field, def.label, ui),
    }
}

/// Render a numeric field. Dispatches on the field's concrete Rust type
/// (u32, f32, etc.) via `try_downcast_mut`.
fn render_number(
    field: &mut dyn bevy::reflect::PartialReflect,
    label: &str,
    range: &super::def::NumberRange,
    ui: &mut egui::Ui,
) {
    let lo = range.min.unwrap_or(0.0);
    let hi = range.max.unwrap_or(1.0);
    let step = range.step;

    if let Some(v) = field.try_downcast_mut::<u32>() {
        let mut tmp = *v as i64;
        let mut slider = egui::Slider::new(&mut tmp, (lo as i64)..=(hi as i64)).text(label);
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        if ui.add(slider).changed() {
            *v = tmp.max(0) as u32;
        }
        return;
    }
    if let Some(v) = field.try_downcast_mut::<f32>() {
        let mut slider = egui::Slider::new(v, (lo as f32)..=(hi as f32)).text(label);
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        ui.add(slider);
        return;
    }
    if let Some(v) = field.try_downcast_mut::<f64>() {
        let mut slider = egui::Slider::new(v, lo..=hi).text(label);
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        ui.add(slider);
        return;
    }
    ui.label(format!("(unsupported number type for {label})"));
}

fn render_bool(field: &mut dyn bevy::reflect::PartialReflect, label: &str, ui: &mut egui::Ui) {
    if let Some(v) = field.try_downcast_mut::<bool>() {
        ui.checkbox(v, label);
    } else {
        ui.label(format!("(expected bool for {label})"));
    }
}

fn render_color(field: &mut dyn bevy::reflect::PartialReflect, label: &str, ui: &mut egui::Ui) {
    if let Some(v) = field.try_downcast_mut::<[f32; 4]>() {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.color_edit_button_rgba_unmultiplied(v);
        });
    } else {
        ui.label(format!("(expected [f32; 4] for {label})"));
    }
}

fn render_text(field: &mut dyn bevy::reflect::PartialReflect, label: &str, ui: &mut egui::Ui) {
    if let Some(v) = field.try_downcast_mut::<String>() {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.text_edit_singleline(v);
        });
    } else {
        ui.label(format!("(expected String for {label})"));
    }
}

/// Look up a `TypeRegistration`'s `SketchSettings::STORAGE_KEY` and return its
/// `TypeId` if it matches.
///
/// The derive macro emits the storage key as a const associated to the
/// trait, not as type-registration metadata. We sidestep that by storing
/// the `(type_id, storage_key)` mapping at registration time via
/// [`super::registry::SettingsTypeKey`] type-data.
fn settings_type_id_for_key(
    reg: &bevy::reflect::TypeRegistration,
    storage_key: &str,
) -> Option<std::any::TypeId> {
    use super::registry::SettingsTypeKey;
    let data = reg.data::<SettingsTypeKey>()?;
    (data.0 == storage_key).then(|| reg.type_id())
}
