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
//! ## Chrome and layout
//!
//! The panel is gated on [`crate::ui::buttons::SettingsPanelVisible`] (default
//! `false`) so it does not auto-open at startup. It draws as an `egui::Area`
//! docked to the right half of the window (see [`dock_rect`]) wrapped in
//! [`crate::ui::backdrop_blur_frame`] for the translucent frosted-glass look,
//! with a header tab bar ([`SettingsTab`]) that routes each settings struct to
//! a tab by storage key. As a docked tool it closes via the cog toggle (or
//! Esc), not by clicking the artwork behind it — so there is no click-outside
//! dismiss.

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
use egui_phosphor::regular as phosphor;
use smallvec::SmallVec;

use super::def::{SettingDef, SettingKind, SettingsCategory};
use super::registry::SettingsRegistry;
use crate::input::state::ServiceConnection;
use crate::lifecycle::state::AppState;
use crate::ui::auto_fade::UiOpacity;
use crate::ui::buttons::SettingsPanelVisible;
use crate::ui::{backdrop_blur_frame, FrameOptions, OverlayStyle};

/// Inline stack snapshot of registered settings storage keys. Sized for the
/// expected case of ≤8 settings types per app; spills to the heap above that.
type KeySnapshot = SmallVec<[&'static str; 8]>;

/// One tab of the consolidated settings dock.
///
/// Each registered settings struct is routed to a tab by its storage key (see
/// [`tab_for_storage_key`]); the dock renders only the sections whose struct
/// maps to the active tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SettingsTab {
    /// Active sketch (Line): particles, visual, spawn, audio.
    #[default]
    Line,
    /// Hand-tracking provider, Leap, and feel.
    HandTracking,
    /// Interface (overlay) and attract-mode/screensaver display.
    Display,
}

impl SettingsTab {
    /// All tabs in left-to-right display order, with their header labels.
    const ORDER: [(SettingsTab, &'static str); 3] = [
        (SettingsTab::Line, "LINE"),
        (SettingsTab::HandTracking, "HAND TRACKING"),
        (SettingsTab::Display, "DISPLAY"),
    ];
}

/// The dock's currently selected tab. Persists across frames so the operator's
/// tab choice survives panel close/reopen.
#[derive(Resource, Default)]
struct SettingsDockTab(SettingsTab);

/// Route a settings struct (identified by its storage key) to its dock tab.
///
/// The map is intentionally total: any key not explicitly placed — including
/// the overlay (`auto_fade`) and any future settings struct — falls to
/// [`SettingsTab::Display`], so a newly registered struct is always reachable
/// rather than silently hidden.
fn tab_for_storage_key(key: &str) -> SettingsTab {
    match key {
        "line" => SettingsTab::Line,
        "hand_tracking" => SettingsTab::HandTracking,
        // "screensaver", overlay/auto_fade, and anything new.
        _ => SettingsTab::Display,
    }
}

/// Geometry of the right-docked settings panel for a window of `window_w` ×
/// `window_h` egui points, returned as `(x, y, width, height)`.
///
/// The dock occupies the right half as a zone, capped to a readable 640 px and
/// floored at 420 px so it never collapses narrower than the file-picker rows
/// need; it is inset 16 px from the right and bottom edges and sits 60 px from
/// the top (below the Home/Settings/Volume button strip). Below ~888 px window
/// width the floor wins and the dock may cross the midline — the operator-on-a-
/// laptop case, accepted rather than special-cased.
fn dock_rect(window_w: f32, window_h: f32) -> (f32, f32, f32, f32) {
    let width = ((window_w * 0.5) - 24.0).clamp(420.0, 640.0);
    let x = window_w - 16.0 - width;
    let y = 60.0;
    let height = (window_h - 60.0 - 16.0).max(0.0);
    (x, y, width, height)
}

/// Plugin assembly hook called by [`super::SettingsPlugin::build`].
///
/// The draw system runs in [`bevy_egui::EguiPrimaryContextPass`] and is gated
/// on [`SettingsPanelVisible`] so the panel only renders when the settings cog
/// has been clicked. The dock is a docked tool: it closes via the cog toggle
/// (or Esc), not by clicking the artwork behind it — so there is no
/// click-outside dismiss (the operator must be able to click the sketch to test
/// a gesture with the panel open).
pub(super) fn add_systems(app: &mut App) {
    app.init_resource::<SettingsDockTab>();
    app.add_systems(
        bevy_egui::EguiPrimaryContextPass,
        draw_user_panel.run_if(settings_panel_visible),
    );
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

/// Exclusive system that draws the consolidated, right-docked settings panel.
///
/// Uses `egui::Area` docked to the right half (see [`dock_rect`]) and wraps
/// content in [`backdrop_blur_frame`] for the translucent frosted-glass look.
/// A header tab bar ([`SettingsTab`]) selects which settings structs render;
/// the selection persists in [`SettingsDockTab`]. Only runs when
/// [`SettingsPanelVisible`] is `true` (gated by the `settings_panel_visible`
/// run condition in [`add_systems`]).
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

    // Read window size for the right-dock geometry; fall back to 1280×720.
    let (window_width, window_height) = {
        let mut q =
            world.query_filtered::<&bevy::window::Window, With<bevy::window::PrimaryWindow>>();
        q.single(world)
            .map_or((1280.0, 720.0), |w| (w.width(), w.height()))
    };
    let (dock_x, dock_y, dock_w, dock_h) = dock_rect(window_width, window_height);

    // Tab selection: read the persisted choice, mutate it from the tab bar this
    // frame, write it back after the Area closure releases the world borrow.
    let mut selected_tab = world
        .get_resource::<SettingsDockTab>()
        .map_or(SettingsTab::default(), |t| t.0);

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

    egui::Area::new(egui::Id::new("wc-settings-dock"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(dock_x, dock_y))
        .show(&ctx, |ui| {
            // Pin the Area to the exact dock rect so `backdrop_blur_frame`'s
            // `available_size()` allocation fills the dock (not just its
            // content's natural size).
            ui.set_min_size(egui::vec2(dock_w, dock_h));
            ui.set_max_size(egui::vec2(dock_w, dock_h));
            backdrop_blur_frame(
                ui,
                &style,
                FrameOptions {
                    corner_radius: style.panel_corner_radius,
                    padding: egui::vec2(20.0, 16.0),
                    opacity_mul,
                },
                |ui| {
                    // Scoped dock visuals: the accent drives selection fills and
                    // the slider's trailing fill, leaving the rest of the overlay
                    // chrome on its existing palette.
                    let v = ui.visuals_mut();
                    v.selection.bg_fill = style.accent_weak;
                    v.selection.stroke = egui::Stroke::new(1.0, style.accent);
                    v.slider_trailing_fill = true;

                    // Header: the tab bar is the title (the old "SETTINGS" label
                    // is retired). Fixed, outside the scroll body.
                    draw_dock_tabs(ui, &mut selected_tab, &style);
                    ui.add_space(4.0);
                    hairline(ui, &style);
                    ui.add_space(8.0);

                    // Body: only the structs routed to the active tab, scrolling
                    // within the fixed dock height.
                    egui::ScrollArea::vertical()
                        .id_salt("wc-dock-scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for key in &keys {
                                if tab_for_storage_key(key) == selected_tab {
                                    render_section_by_key(world, ui, key, provider_status, &style);
                                }
                            }
                        });
                },
            );
        });

    if let Some(mut tab) = world.get_resource_mut::<SettingsDockTab>() {
        tab.0 = selected_tab;
    }
}

/// Draw the dock's header tab row, mutating `selected` on click.
///
/// Renders each [`SettingsTab`] as a frameless selectable label (the pill
/// background is suppressed in a scope so the tabs read as plain text) with a
/// 2 px accent underline beneath the active tab. The hairline drawn below the
/// row by the caller reads as the tab bar's baseline.
fn draw_dock_tabs(ui: &mut egui::Ui, selected: &mut SettingsTab, style: &OverlayStyle) {
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        // Suppress the selectable-label pill so a tab is text + underline only.
        v.selection.bg_fill = egui::Color32::TRANSPARENT;
        v.widgets.hovered.weak_bg_fill = egui::Color32::TRANSPARENT;
        v.widgets.active.weak_bg_fill = egui::Color32::TRANSPARENT;
        v.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
        ui.spacing_mut().item_spacing.x = 18.0;

        ui.horizontal(|ui| {
            for (tab, label) in SettingsTab::ORDER {
                let is_sel = *selected == tab;
                let color = if is_sel {
                    style.text_primary
                } else {
                    style.text_secondary
                };
                let text = egui::RichText::new(label).size(12.5).color(color);
                let resp = ui.selectable_label(is_sel, text);
                if resp.clicked() {
                    *selected = tab;
                }
                if is_sel {
                    ui.painter().hline(
                        resp.rect.x_range(),
                        resp.rect.bottom() + 3.0,
                        egui::Stroke::new(2.0, style.accent),
                    );
                }
            }
        });
    });
}

/// Draw a full-width in-panel hairline rule using the dock palette.
fn hairline(ui: &mut egui::Ui, style: &OverlayStyle) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 1.0), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, style.hairline),
    );
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

    // Get a Reflect handle on the resource, plus a default instance for
    // modified-from-default detection and reset. Clone the Arc so the read
    // guard doesn't borrow `world`; build the default while the guard is alive
    // so the owned `Box` outlives the `drop` below.
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry_read = registry.read();
    let Some(type_data) =
        registry_read.get_type_data::<bevy::ecs::reflect::ReflectResource>(type_id)
    else {
        ui.label("(no ReflectResource on settings type)");
        return;
    };
    // A fresh default instance, available when the type registered
    // `#[reflect(Default)]`. Absent → rows degrade to no bold / no reset glyph,
    // never a hard failure.
    let default_instance: Option<Box<dyn Reflect>> = registry_read
        .get_type_data::<bevy::reflect::std_traits::ReflectDefault>(type_id)
        .map(bevy::reflect::std_traits::ReflectDefault::default);
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
        default_instance.as_deref(),
        style,
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
/// Each section uses its own `egui::Grid` with three columns: the label (bold
/// when the field differs from its default, with a restart badge when the field
/// requires a restart), the value widget, and a reset-to-default glyph (shown
/// only when modified; an aligned spacer otherwise).
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
fn render_user_fields_via_reflect(
    reflect: &mut dyn Reflect,
    defs: &[SettingDef],
    storage_key: &'static str,
    provider_status: Option<ProviderStatusLine>,
    default: Option<&dyn Reflect>,
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
            .num_columns(3)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                for def in defs
                    .iter()
                    .filter(|d| d.category == SettingsCategory::User && d.section == section_name)
                {
                    let Some(field) = struct_mut.field_mut(def.field_name) else {
                        continue;
                    };
                    let default_field = default_struct.and_then(|s| s.field(def.field_name));
                    let modified = field_differs_from_default(field, default_field);
                    // Column 1: label (+ restart badge), bold when modified.
                    render_label_cell(ui, def, modified, style);
                    // Column 2: the value widget.
                    render_widget_value(field, def, storage_key, ui);
                    // Column 3: reset-to-default glyph, or an aligned spacer.
                    render_reset_cell(ui, field, default_field, modified, style);
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
                            // Column 3 spacer — allocate, not `add_space` (which
                            // panics inside a Grid).
                            ui.allocate_exact_size(egui::vec2(18.0, 0.0), egui::Sense::hover());
                            ui.end_row();
                        }
                    }
                }
            });
    }
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

/// Render Grid column 1: the field label, bold when modified, followed by an
/// amber restart badge when the field requires a restart to take effect.
fn render_label_cell(ui: &mut egui::Ui, def: &SettingDef, modified: bool, style: &OverlayStyle) {
    ui.horizontal(|ui| {
        let mut label = egui::RichText::new(def.label).color(style.text_primary);
        if modified {
            label = label.strong();
        }
        ui.label(label);
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

    /// Every settings struct lands in a tab, and the map is total: unknown
    /// keys (a future struct, the overlay) fall to Display rather than vanish.
    #[test]
    fn tab_routing_is_total() {
        assert_eq!(tab_for_storage_key("line"), SettingsTab::Line);
        assert_eq!(
            tab_for_storage_key("hand_tracking"),
            SettingsTab::HandTracking
        );
        assert_eq!(tab_for_storage_key("screensaver"), SettingsTab::Display);
        assert_eq!(tab_for_storage_key("overlay"), SettingsTab::Display);
        assert_eq!(
            tab_for_storage_key("some_future_sketch"),
            SettingsTab::Display,
            "unrecognized keys must route to Display, never disappear"
        );
    }

    /// Dock geometry: right-anchored, capped at 640, floored at 420, inset
    /// 16/16/60 from right/bottom/top.
    #[test]
    #[allow(clippy::float_cmp, reason = "exact arithmetic on integer-valued f32")]
    fn dock_rect_anchors_right_and_clamps_width() {
        // 1080p: half is 936, capped to 640; x = 1920 - 16 - 640.
        let (x, y, w, h) = dock_rect(1920.0, 1080.0);
        assert_eq!(w, 640.0);
        assert_eq!(x, 1920.0 - 16.0 - 640.0);
        assert_eq!(y, 60.0);
        assert_eq!(h, 1080.0 - 76.0);

        // Narrow window: half-24 floors at 420 and the dock may cross center.
        let (xn, _, wn, _) = dock_rect(800.0, 600.0);
        assert_eq!(wn, 420.0, "width floors at 420");
        assert_eq!(xn, 800.0 - 16.0 - 420.0);

        // Mid width that lands inside the band: 1200*0.5-24 = 576.
        let (_, _, wm, _) = dock_rect(1200.0, 800.0);
        assert_eq!(wm, 576.0);

        // Degenerate short window cannot produce a negative height.
        let (_, _, _, hz) = dock_rect(1920.0, 40.0);
        assert!(hz >= 0.0, "height is floored at 0");
    }

    /// Modified-from-default detection: equal reads unmodified, differing reads
    /// modified, and an absent default degrades to unmodified (no bold/reset).
    #[test]
    fn field_modified_detection() {
        use bevy::reflect::PartialReflect;
        let live: f32 = 0.5;
        let same: f32 = 0.5;
        let diff: f32 = 0.9;
        assert!(
            !field_differs_from_default(&live, Some(&same as &dyn PartialReflect)),
            "value equal to default is not modified"
        );
        assert!(
            field_differs_from_default(&live, Some(&diff as &dyn PartialReflect)),
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
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
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
        });
        // Reaching here without a panic is the assertion.
    }

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
