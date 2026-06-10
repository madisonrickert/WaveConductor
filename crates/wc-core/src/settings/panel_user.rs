//! Curated user-facing settings panel.
//!
//! Walks [`super::SettingsRegistry`] each frame and, for each registered
//! settings resource, renders typed widgets directly (no collapsing header)
//! for every `SettingDef` whose `category == User`. Field values are
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
use crate::input::state::ServiceConnection;
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

    // Snapshot the hand-tracking provider's lifecycle state for the status
    // row under the "Tracking provider" dropdown (Task: surface the ~1-2 s
    // MediaPipe model-load/camera-open window instead of looking dead).
    // Taken before the egui closure borrows `world`, mirroring the dev
    // panel's registry snapshot; fails soft to `None` (no row) when the
    // registry resource is absent (tests, `Off` with an empty registry).
    let provider_status = provider_status_snapshot(world);

    // Read window width for top-right positioning; fall back to 1280.
    let window_width = {
        let mut q =
            world.query_filtered::<&bevy::window::Window, With<bevy::window::PrimaryWindow>>();
        q.single(world).map_or(1280.0, bevy::window::Window::width)
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
    // Width bumped from 320 px to 420 px so the spawn_template file-picker row
    // (file name + Browse… button) fits on one line without overflowing.
    // This is a minor intentional deviation from v4's exact panel width.
    let area_pos = egui::pos2(window_width - 16.0 - 420.0, 60.0);

    let mut panel_rect = egui::Rect::NOTHING;

    egui::Area::new(egui::Id::new("wc-settings-user-panel"))
        .order(egui::Order::Foreground)
        .fixed_pos(area_pos)
        .show(&ctx, |ui| {
            ui.set_max_width(420.0);
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
                        render_section_by_key(world, ui, key, provider_status);
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
///
/// `provider_status` is the pre-snapshotted hand-tracking provider state,
/// threaded through to the status row under the "Tracking provider"
/// dropdown (see [`render_provider_status_row`]).
fn render_section_by_key(
    world: &mut World,
    ui: &mut egui::Ui,
    storage_key: &'static str,
    provider_status: Option<ProviderStatusLine>,
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
    if defs.iter().all(|d| d.category != SettingsCategory::User) {
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
    render_user_fields_via_reflect(
        &mut *reflect_mut,
        defs.as_ref(),
        storage_key,
        provider_status,
        ui,
    );
}

/// Walk `reflect` (a `&mut dyn Reflect` over the settings struct) and render
/// each user-category field as a typed widget, grouped under section headers.
///
/// Fields with the same `section` name are clustered together under an
/// uppercase section header label. Fields with `section == ""` are rendered
/// first in an unlabeled group (no header). Section order follows the first
/// appearance of each section name in the `defs` slice.
///
/// Each section uses its own `egui::Grid` with two columns so labels are
/// left-aligned in column 1 and input widgets fill column 2. This is the
/// idiomatic egui form-layout pattern.
///
/// `storage_key` salts every egui id created below (Grids, `ComboBox`es) so
/// that two settings structs using the same section or field names don't
/// collide in egui's id-to-state map (colliding Grids share column widths;
/// colliding `ComboBox`es share popup open/close state).
fn render_user_fields_via_reflect(
    reflect: &mut dyn Reflect,
    defs: &[SettingDef],
    storage_key: &'static str,
    provider_status: Option<ProviderStatusLine>,
    ui: &mut egui::Ui,
) {
    let ReflectMut::Struct(struct_mut) = reflect.reflect_mut() else {
        ui.label("(settings is not a struct)");
        return;
    };

    // Pass 1: collect section names in order of first appearance among User
    // fields. `""` (no section) always sorts first when present.
    let mut section_order: Vec<&'static str> = Vec::new();
    for def in defs.iter().filter(|d| d.category == SettingsCategory::User) {
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
            .num_columns(2)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                for def in defs
                    .iter()
                    .filter(|d| d.category == SettingsCategory::User && d.section == section_name)
                {
                    let Some(field) = struct_mut.field_mut(def.field_name) else {
                        continue;
                    };
                    // Column 1: label, left-aligned by default in egui Grid.
                    ui.label(def.label);
                    // Column 2: widget fills remaining width automatically.
                    render_widget_value(field, def, storage_key, ui);
                    ui.end_row();

                    // Status row directly under the "Tracking provider"
                    // dropdown: the MediaPipe backend loads its models and
                    // opens the camera asynchronously (~1-2 s with no
                    // tracking), so show a spinner while it starts and a red
                    // note when it failed. No row while healthy.
                    if storage_key == ProviderStatusLine::STORAGE_KEY
                        && def.field_name == ProviderStatusLine::FIELD_NAME
                    {
                        if let Some(line) = provider_status {
                            ui.label(""); // column 1: keep the grid aligned
                            render_provider_status_row(ui, line);
                            ui.end_row();
                        }
                    }
                }
            });
    }
}

/// What the status row under the "Tracking provider" dropdown should show,
/// derived from the primary provider's [`ServiceConnection`] axis by
/// [`provider_status_line`] (the dropdown's *selected* enum value is not
/// consulted: the row reports the provider actually installed, which is what
/// the operator is waiting on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderStatusLine {
    /// Provider is between `start()` and its first verdict (`MediaPipe`:
    /// model load + camera open on the worker; Leap: service handshake).
    /// Rendered as a spinner + "starting…".
    Starting,
    /// Provider is errored / unreachable. Rendered as a short red note
    /// pointing at the dev panel, which has the full multi-axis status.
    Failed,
}

impl ProviderStatusLine {
    /// Storage key of the settings struct owning the provider dropdown
    /// ([`crate::settings::HandTrackingSettings`]).
    const STORAGE_KEY: &'static str =
        <crate::settings::HandTrackingSettings as super::SketchSettings>::STORAGE_KEY;
    /// Field name of the provider dropdown within that struct.
    const FIELD_NAME: &'static str = "provider";
}

/// Snapshot the primary hand-tracking provider's state as a status-row
/// verdict. `None` (no row) when the registry resource is absent, when it is
/// empty (provider `Off`), or when the provider is healthy.
fn provider_status_snapshot(world: &World) -> Option<ProviderStatusLine> {
    let registry = world.get_resource::<crate::input::provider::ProviderRegistry>()?;
    // An empty registry (`Off`) has no provider to wait on — `primary_id()`
    // is `None` and `primary_status()` would report a default `NotStarted`,
    // which must NOT render as an eternal spinner.
    registry.primary_id()?;
    provider_status_line(registry.primary_status().service)
}

/// Map a provider's [`ServiceConnection`] to the status row to display.
///
/// - `NotStarted` / `Connecting` → [`ProviderStatusLine::Starting`]: the
///   verdict is pending (`MediaPipe` reports `Connecting` from `start()`
///   until its worker has loaded the ort models and opened the camera).
/// - `Errored` / `ServiceMissing` / `Disconnected` →
///   [`ProviderStatusLine::Failed`]: all three mean "you will not get
///   tracking without intervention" — honest red beats a stuck spinner.
/// - `Connected` → `None`: tracking works; the panel stays quiet.
fn provider_status_line(service: ServiceConnection) -> Option<ProviderStatusLine> {
    match service {
        ServiceConnection::NotStarted | ServiceConnection::Connecting => {
            Some(ProviderStatusLine::Starting)
        }
        ServiceConnection::Errored
        | ServiceConnection::ServiceMissing
        | ServiceConnection::Disconnected => Some(ProviderStatusLine::Failed),
        ServiceConnection::Connected => None,
    }
}

/// Render the widget half (Grid column 2) of the provider status row.
///
/// Cheap and allocation-free: static strings only, one small spinner while
/// starting (egui spinners self-animate; the egui pass runs every frame).
fn render_provider_status_row(ui: &mut egui::Ui, line: ProviderStatusLine) {
    match line {
        ProviderStatusLine::Starting => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(12.0));
                ui.weak("starting…");
            });
        }
        ProviderStatusLine::Failed => {
            ui.label(
                egui::RichText::new("failed: see dev panel (Shift+D)")
                    .size(11.0)
                    .color(egui::Color32::from_rgb(0xE5, 0x6E, 0x6E)),
            );
        }
    }
}

/// Render the widget (second Grid column) for `field` based on the metadata in `def`.
///
/// Called from inside an `egui::Grid` row after the label has already been
/// placed in column 1. Each helper renders only the input widget — no label,
/// no `ui.horizontal` wrapper. The Grid handles label/widget alignment.
///
/// `field` is `&mut dyn PartialReflect` as returned by [`Struct::field_mut`].
///
/// `storage_key` is the owning settings struct's storage key, threaded through
/// to widgets that need a unique egui id (currently [`render_enum`]).
fn render_widget_value(
    field: &mut dyn bevy::reflect::PartialReflect,
    def: &SettingDef,
    storage_key: &'static str,
    ui: &mut egui::Ui,
) {
    match &def.kind {
        SettingKind::Number(range) => render_number(field, range, ui),
        SettingKind::Boolean => render_bool(field, ui),
        SettingKind::Color => render_color(field, ui),
        SettingKind::Text => render_text(field, ui),
        SettingKind::FilePath {
            filter_label,
            extensions,
        } => {
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
fn render_number(
    field: &mut dyn bevy::reflect::PartialReflect,
    range: &super::def::NumberRange,
    ui: &mut egui::Ui,
) {
    let lo = range.min.unwrap_or(0.0);
    let hi = range.max.unwrap_or(1.0);
    let step = range.step;

    if let Some(v) = field.try_downcast_mut::<u32>() {
        let mut tmp = *v as i64;
        let mut slider = egui::Slider::new(&mut tmp, (lo as i64)..=(hi as i64));
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        if ui.add(slider).changed() {
            *v = tmp.max(0) as u32;
        }
    } else if let Some(v) = field.try_downcast_mut::<f32>() {
        let mut slider = egui::Slider::new(v, (lo as f32)..=(hi as f32));
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        ui.add(slider);
    } else if let Some(v) = field.try_downcast_mut::<f64>() {
        let mut slider = egui::Slider::new(v, lo..=hi);
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        ui.add(slider);
    } else if let Some(v) = field.try_downcast_mut::<i32>() {
        let mut tmp = *v as i64;
        let mut slider = egui::Slider::new(&mut tmp, (lo as i64)..=(hi as i64));
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        if ui.add(slider).changed() {
            *v = tmp.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        }
    } else if let Some(v) = field.try_downcast_mut::<i64>() {
        let mut slider = egui::Slider::new(v, (lo as i64)..=(hi as i64));
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
fn render_text(field: &mut dyn bevy::reflect::PartialReflect, ui: &mut egui::Ui) {
    if let Some(v) = field.try_downcast_mut::<String>() {
        ui.text_edit_singleline(v);
    } else {
        ui.label("(expected String)");
    }
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
/// Applies a payload-less [`bevy::reflect::DynamicEnum`], which is exactly
/// the variant-switch operation `Reflect`-derived enums support for unit
/// variants. Returns `true` on success. Failure (a payload variant or a name
/// the enum doesn't have) leaves the field unchanged and logs a warning that
/// names the offending field — the loud debug-build failure for such misuse
/// already lives in [`super::def::enum_variant_names`].
fn set_enum_variant(
    field: &mut dyn bevy::reflect::PartialReflect,
    field_name: &str,
    variant: &str,
) -> bool {
    use bevy::reflect::{DynamicEnum, DynamicVariant};

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

    /// The status row keys on `HandTrackingSettings`'s actual storage key and
    /// the `provider` field's actual name; if either is renamed without
    /// updating [`ProviderStatusLine`], the row silently stops rendering.
    #[test]
    fn provider_status_row_keys_match_the_settings_struct() {
        use crate::settings::SketchSettings;
        assert_eq!(ProviderStatusLine::STORAGE_KEY, "hand_tracking");
        assert!(
            crate::settings::HandTrackingSettings::settings_def()
                .iter()
                .any(|d| d.field_name == ProviderStatusLine::FIELD_NAME),
            "HandTrackingSettings has no `{}` field — update ProviderStatusLine::FIELD_NAME",
            ProviderStatusLine::FIELD_NAME
        );
    }

    /// Pre-verdict states spin, dead states warn, healthy shows nothing.
    #[test]
    fn provider_status_line_maps_every_service_state() {
        use ProviderStatusLine::{Failed, Starting};
        for (service, expected) in [
            (ServiceConnection::NotStarted, Some(Starting)),
            (ServiceConnection::Connecting, Some(Starting)),
            (ServiceConnection::Errored, Some(Failed)),
            (ServiceConnection::ServiceMissing, Some(Failed)),
            (ServiceConnection::Disconnected, Some(Failed)),
            (ServiceConnection::Connected, None),
        ] {
            assert_eq!(provider_status_line(service), expected, "{service:?}");
        }
    }

    /// An empty registry (provider `Off`) must render no status row — its
    /// default `NotStarted` primary status would otherwise read as an
    /// eternal "starting…" spinner.
    #[test]
    fn provider_status_snapshot_is_none_for_empty_or_absent_registry() {
        let mut world = World::new();
        assert_eq!(provider_status_snapshot(&world), None, "absent registry");
        world.insert_resource(crate::input::provider::ProviderRegistry::default());
        assert_eq!(provider_status_snapshot(&world), None, "empty registry");
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
        let variants = super::super::def::enum_variant_names::<Palette>();
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
