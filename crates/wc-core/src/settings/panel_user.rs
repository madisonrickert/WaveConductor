//! Curated user-facing settings panel.
//!
//! Iterates [`super::SettingsRegistry`] and, for each registered settings
//! resource, draws an `egui` collapsing header containing typed widgets
//! for every field with `category = User`. `Dev`-category fields are
//! invisible here — the Shift+D inspector renders them instead.
//!
//! ## Implementation notes
//!
//! - Uses an exclusive `world: &mut World` system because we need to read
//!   the registry's `Vec<RegisteredSettings>` and then mutate each
//!   resource it points at; this can't be expressed with normal system
//!   params.
//! - The panel is registered behind an `egui::Window` keyed by a stable
//!   id so egui persists position / collapsed state across frames.

#![allow(
    clippy::as_conversions,
    reason = "panel renderer converts between u32/i64/f32 for egui widgets; bounds-checked above"
)]
#![allow(
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "numeric type conversions for egui slider bounds and widget values; values are range-limited before conversion"
)]
#![allow(
    clippy::expect_used,
    reason = "checked_downcast_mut panics only when TypeId was not verified by the caller, which is a programmer error"
)]

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use super::def::{SettingDef, SettingKind, SettingsCategory};
use super::registry::SettingsRegistry;
use super::test_settings::TestSketchSettings;
use super::trait_def::SketchSettings;

/// Plugin assembly hook called by [`super::SettingsPlugin::build`].
pub(super) fn add_systems(app: &mut App) {
    app.add_systems(Update, draw_user_panel);
}

/// Exclusive system that draws the user panel.
fn draw_user_panel(world: &mut World) {
    let registry = world
        .get_resource::<SettingsRegistry>()
        .cloned()
        .unwrap_or_default();
    if registry.entries.is_empty() {
        return;
    }

    // Guard: EguiPlugin must be initialized. In test harnesses that use
    // MinimalPlugins without EguiPlugin the resource won't exist and
    // SystemState::new would panic when initializing EguiContexts.
    if !world.contains_resource::<bevy_egui::EguiUserTextures>() {
        return;
    }

    // Pull the egui context out via SystemState, then apply it back before
    // calling any further world mutations.
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
        .default_open(false)
        .show(&ctx, |ui| {
            for entry in &registry.entries {
                ui.collapsing(entry.storage_key, |ui| {
                    // For Plan 5 we only ship one typed renderer per known
                    // settings struct. Real sketches in Plan 6+ will each
                    // add a renderer here (or we'll switch to a fully
                    // reflection-driven walker once we hit two real
                    // sketches and have something to factor against).
                    if entry.storage_key == TestSketchSettings::STORAGE_KEY {
                        render_user_fields::<TestSketchSettings>(world, ui, &entry.def);
                    } else {
                        ui.label(
                            "(no typed renderer; open the dev panel with \
                             Shift+D for full inspection)",
                        );
                    }
                });
            }
        });
}

/// Render every `category = User` field of `S` against `ui`.
fn render_user_fields<S: SketchSettings>(
    world: &mut World,
    ui: &mut egui::Ui,
    defs: &[SettingDef],
) {
    // We avoid `bevy_reflect::ReflectMut` plumbing for the typed renderer.
    // Instead, switch on the field name. For Plan 5 this is the single
    // synthetic struct, so the cost is tiny and the code is explicit.
    let mut value = world.resource::<S>().clone();
    let mut dirty = false;

    if std::any::TypeId::of::<S>() == std::any::TypeId::of::<TestSketchSettings>() {
        // Safe cast: same TypeId.
        let typed: &mut TestSketchSettings = checked_downcast_mut(&mut value);
        for def in defs {
            if def.category != SettingsCategory::User {
                continue;
            }
            match def.field_name {
                "widget_count" => {
                    if let SettingKind::Number(range) = &def.kind {
                        let mut tmp = typed.widget_count as i64;
                        let lo = range.min.unwrap_or(0.0) as i64;
                        let hi = range.max.unwrap_or(1000.0) as i64;
                        if ui
                            .add(egui::Slider::new(&mut tmp, lo..=hi).text(def.label))
                            .changed()
                        {
                            typed.widget_count = tmp.max(0) as u32;
                            dirty = true;
                        }
                    }
                }
                "tempo_hz" => {
                    if let SettingKind::Number(range) = &def.kind {
                        let lo = range.min.unwrap_or(0.0) as f32;
                        let hi = range.max.unwrap_or(1.0) as f32;
                        if ui
                            .add(egui::Slider::new(&mut typed.tempo_hz, lo..=hi).text(def.label))
                            .changed()
                        {
                            dirty = true;
                        }
                    }
                }
                "enable_tint" => {
                    if ui.checkbox(&mut typed.enable_tint, def.label).changed() {
                        dirty = true;
                    }
                }
                "tint_color" => {
                    ui.horizontal(|ui| {
                        ui.label(def.label);
                        // `color_edit_button_rgba_unmultiplied` takes `&mut [f32; 4]`
                        // and writes the new RGBA value back in place — no separate
                        // conversion needed.
                        if ui
                            .color_edit_button_rgba_unmultiplied(&mut typed.tint_color)
                            .changed()
                        {
                            dirty = true;
                        }
                    });
                }
                _ => {}
            }
        }
    }

    if dirty {
        *world.resource_mut::<S>() = value;
    }
}

/// Reinterpret `&mut S` as `&mut T` once the caller has verified
/// `TypeId::of::<S>() == TypeId::of::<T>()`. Uses `Any::downcast_mut`
/// (safe, runtime-checked) so the workspace `unsafe_code = "deny"` lint
/// stays clean. Kept in one place so the contract is auditable.
fn checked_downcast_mut<S: SketchSettings, T: 'static>(value: &mut S) -> &mut T {
    let any: &mut dyn std::any::Any = value;
    any.downcast_mut::<T>()
        .expect("caller verified TypeId match")
}
