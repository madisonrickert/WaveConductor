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
//!
//! ## Task 18: v4 chrome
//!
//! The panel is now gated on [`crate::ui::buttons::SettingsPanelVisible`]
//! (default `false`) so it no longer auto-opens at startup. The `egui::Window`
//! is replaced by `egui::Area` + [`crate::ui::backdrop_blur_frame`] for the
//! translucent frosted-glass look. A click-outside dismiss system runs each
//! `Update` frame to match v4's `mousedown`-outside behaviour.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    reason = "egui sliders use f32/i64 widget ranges; bounds-checked against SettingDef metadata"
)]

use std::sync::Arc;

use bevy::prelude::*;
use bevy::reflect::ReflectMut;
use bevy_egui::{egui, EguiContexts};
use smallvec::SmallVec;

use super::def::{SettingDef, SettingKind, SettingsCategory};
use super::registry::SettingsRegistry;
use crate::lifecycle::state::AppState;
use crate::ui::auto_fade::UiOpacity;
use crate::ui::buttons::{LastSettingsPanelRect, SettingsPanelVisible};
use crate::ui::{backdrop_blur_frame, FrameOptions, OverlayStyle};

/// Inline stack snapshot of registered settings storage keys. Sized for the
/// expected case of ≤8 settings types per app; spills to the heap above that.
type KeySnapshot = SmallVec<[&'static str; 8]>;

/// Plugin assembly hook called by [`super::SettingsPlugin::build`].
///
/// The draw system runs in [`bevy_egui::EguiPrimaryContextPass`] and is gated
/// on [`SettingsPanelVisible`] so the panel only renders when the settings cog
/// has been clicked. A second system runs each [`Update`] frame to dismiss the
/// panel when the user clicks outside its bounds.
pub(super) fn add_systems(app: &mut App) {
    app.add_systems(
        bevy_egui::EguiPrimaryContextPass,
        draw_user_panel.run_if(settings_panel_visible),
    );
    app.add_systems(Update, dismiss_on_click_outside);
}

/// Run condition: returns `true` when the settings panel should be visible.
///
/// Also gates on `AppState != Home` — the settings panel is sketch chrome and
/// must not appear over the picker page, matching v4's behaviour where the cog
/// button itself is hidden on Home (Fix 2). Without this guard, a panel opened
/// while in a sketch would persist visually through the Home transition.
fn settings_panel_visible(
    visible: Res<'_, SettingsPanelVisible>,
    state: Res<'_, State<AppState>>,
) -> bool {
    visible.0 && **state != AppState::Home
}

/// Exclusive system that draws the user settings panel with v4 chrome.
///
/// Uses `egui::Area` for fixed top-right positioning (under the cog) and
/// wraps content in [`backdrop_blur_frame`] for the translucent frosted-glass
/// look. Only runs when [`SettingsPanelVisible`] is `true` (gated by the
/// `settings_panel_visible` run condition in [`add_systems`]).
///
/// After drawing, updates [`LastSettingsPanelRect`] so that
/// [`dismiss_on_click_outside`] knows the panel's bounds for the next frame.
fn draw_user_panel(world: &mut World) {
    // Skip when no egui context is up (e.g., MinimalPlugins test harness).
    if !world.contains_resource::<bevy_egui::EguiUserTextures>() {
        return;
    }
    // Snapshot the storage keys we need to iterate so the registry resource
    // stays unborrowed while we mutate per-type resources. SmallVec keeps the
    // common ≤8-types case on the stack.
    let keys: KeySnapshot = world
        .get_resource::<SettingsRegistry>()
        .map(|r| r.entries.iter().map(|e| e.storage_key).collect())
        .unwrap_or_default();
    if keys.is_empty() {
        return;
    }

    // Read style and opacity before entering the egui context borrow.
    let style = *world.resource::<OverlayStyle>();
    let opacity_mul = world.resource::<UiOpacity>().current;

    // Read window width for top-right positioning; fall back to 1280.
    let window_width = {
        let mut q = world.query_filtered::<&bevy::window::Window, With<bevy::window::PrimaryWindow>>();
        q.single(world).map(bevy::window::Window::width).unwrap_or(1280.0)
    };

    let mut state: bevy::ecs::system::SystemState<EguiContexts<'_, '_>> =
        bevy::ecs::system::SystemState::new(world);
    let mut contexts = state.get_mut(world);
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    // `EguiContext` is an `Arc<Mutex<…>>` internally, so `.clone()` is a
    // refcount bump. Cloning here lets us release the `EguiContexts` SystemParam
    // borrow before the `show` closure re-enters `World` to render each section.
    let ctx = ctx.clone();
    state.apply(world);

    // Position: top-right, 16 px inset, 60 px from the top (below the cog).
    // Width fixed at 320 px to match v4's overlay panel width.
    let area_pos = egui::pos2(window_width - 16.0 - 320.0, 60.0);

    let mut panel_rect = egui::Rect::NOTHING;

    egui::Area::new(egui::Id::new("wc-settings-user-panel"))
        .order(egui::Order::Foreground)
        .fixed_pos(area_pos)
        .show(&ctx, |ui| {
            ui.set_max_width(320.0);
            let resp = backdrop_blur_frame(
                ui,
                &style,
                FrameOptions {
                    corner_radius: style.panel_corner_radius,
                    padding: egui::vec2(20.0, 16.0),
                    opacity_mul,
                },
                |ui| {
                    // Title row: "SETTINGS" in dim chrome text.
                    // Note: egui has no built-in letter-spacing; default spacing
                    // is an approved deviation per Plan 11.5 Task 18.
                    ui.label(
                        egui::RichText::new("SETTINGS")
                            .color(style.text_color_dim)
                            .size(13.0),
                    );
                    ui.separator();
                    for key in &keys {
                        render_section_by_key(world, ui, key);
                    }
                },
            );
            // Capture the panel rect for click-outside detection. Written here
            // (inside the Area closure) so it reflects the drawn frame's actual
            // bounds, not a stale value from a previous frame.
            panel_rect = resp.rect;
        });

    world.resource_mut::<LastSettingsPanelRect>().0 = panel_rect;
}

/// Dismiss the settings panel when the user clicks outside its bounds.
///
/// Mirrors v4's `mousedown` outside handler. Only triggers on
/// `MouseButton::Left` just-pressed events. If `EguiPointerCaptured` is true,
/// the click was consumed by egui (e.g., hitting the cog to toggle the panel),
/// so this handler skips — the cog's own toggle logic already handled it.
fn dismiss_on_click_outside(
    mut visible: ResMut<'_, SettingsPanelVisible>,
    last_rect: Res<'_, LastSettingsPanelRect>,
    egui_captured: Res<'_, crate::settings::EguiPointerCaptured>,
    mouse: Res<'_, ButtonInput<MouseButton>>,
    windows: Query<'_, '_, &bevy::window::Window, With<bevy::window::PrimaryWindow>>,
) {
    if !visible.0 {
        return;
    }
    // Only fire on the frame the left button is first pressed.
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    // If egui captured the pointer this frame, the click landed inside egui
    // (which includes the panel itself and the cog button). Defer to the cog's
    // own toggle — don't double-dismiss.
    if egui_captured.0 {
        return;
    }
    let Some(window) = windows.iter().next() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let cursor_egui = egui::pos2(cursor.x, cursor.y);
    if !last_rect.0.contains(cursor_egui) {
        visible.0 = false;
    }
}

/// Look up the type registration matching `storage_key` and render its
/// `User`-category fields. Walks the `TypeRegistry` to find the registered
/// settings type whose `STORAGE_KEY` matches; uses reflection to
/// read/write fields without static type knowledge.
fn render_section_by_key(world: &mut World, ui: &mut egui::Ui, storage_key: &'static str) {
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
        render_user_fields_via_reflect(&mut *reflect_mut, defs.as_ref(), ui);
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
        SettingKind::FilePath {
            filter_label,
            extensions,
        } => {
            render_file_path(field, def.label, filter_label, extensions, ui);
        }
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
    if let Some(v) = field.try_downcast_mut::<i32>() {
        let mut tmp = *v as i64;
        let mut slider = egui::Slider::new(&mut tmp, (lo as i64)..=(hi as i64)).text(label);
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        if ui.add(slider).changed() {
            *v = tmp.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        }
        return;
    }
    if let Some(v) = field.try_downcast_mut::<i64>() {
        let mut slider = egui::Slider::new(v, (lo as i64)..=(hi as i64)).text(label);
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
        return;
    }
    if let Some(v) = field.try_downcast_mut::<bevy::color::Color>() {
        let mut rgba = v.to_srgba().to_f32_array();
        ui.horizontal(|ui| {
            ui.label(label);
            if ui.color_edit_button_rgba_unmultiplied(&mut rgba).changed() {
                *v = bevy::color::Color::srgba(rgba[0], rgba[1], rgba[2], rgba[3]);
            }
        });
        return;
    }
    ui.label(format!("(expected [f32; 4] or Color for {label})"));
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

/// Render a filesystem-path field as a text edit plus a Browse… button.
/// On Browse, opens [`rfd::FileDialog`] filtered to `extensions`; the
/// selected path replaces the field value. Available only on native
/// platforms — the wasm build renders a text-edit only (no picker).
fn render_file_path(
    field: &mut dyn bevy::reflect::PartialReflect,
    label: &str,
    #[cfg_attr(target_arch = "wasm32", allow(unused_variables))] filter_label: &str,
    #[cfg_attr(target_arch = "wasm32", allow(unused_variables))] extensions: &[&str],
    ui: &mut egui::Ui,
) {
    let Some(v) = field.try_downcast_mut::<String>() else {
        ui.label(format!("(expected String for {label})"));
        return;
    };
    ui.horizontal(|ui| {
        ui.label(label);
        ui.text_edit_singleline(v);
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
#[allow(
    dead_code,
    reason = "preemptive support; reachable once `kind = Vec2` lands in the derive macro"
)]
fn render_vec2(field: &mut dyn bevy::reflect::PartialReflect, label: &str, ui: &mut egui::Ui) {
    if let Some(v) = field.try_downcast_mut::<bevy::math::Vec2>() {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.add(egui::DragValue::new(&mut v.x).prefix("x: "));
            ui.add(egui::DragValue::new(&mut v.y).prefix("y: "));
        });
    } else {
        ui.label(format!("(expected Vec2 for {label})"));
    }
}

/// Reflection branch for `Vec3` fields. Not yet reachable through the
/// `#[setting(...)]` attribute (no `SettingKind` variant); added eagerly so
/// the panel is ready when the next sketch needs it. The derive macro will
/// gain a `kind = Vec3` parser when that sketch lands.
#[allow(
    dead_code,
    reason = "preemptive support; reachable once `kind = Vec3` lands in the derive macro"
)]
fn render_vec3(field: &mut dyn bevy::reflect::PartialReflect, label: &str, ui: &mut egui::Ui) {
    if let Some(v) = field.try_downcast_mut::<bevy::math::Vec3>() {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.add(egui::DragValue::new(&mut v.x).prefix("x: "));
            ui.add(egui::DragValue::new(&mut v.y).prefix("y: "));
            ui.add(egui::DragValue::new(&mut v.z).prefix("z: "));
        });
    } else {
        ui.label(format!("(expected Vec3 for {label})"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::panic,
        reason = "test assertion — panic on wrong variant is intentional"
    )]
    fn file_path_kind_dispatches() {
        let def = SettingDef {
            field_name: "path",
            label: "Path",
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
}
