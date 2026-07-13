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
//! docked to the right half of the window (see [`dock::dock_rect`]) wrapped in
//! [`crate::ui::backdrop_blur_frame`] for the translucent frosted-glass look,
//! with a header tab bar ([`SettingsTab`]) that routes each settings struct to
//! a tab by storage key. As a docked tool it closes via the cog toggle (or
//! Esc), not by clicking the artwork behind it — so there is no click-outside
//! dismiss.
//!
//! ## Module layout
//!
//! This directory module splits the panel's original single-file
//! implementation into cohesive pieces, all still driven from this file's
//! [`draw_user_panel`]:
//! - [`dock`] — the tab/routing/geometry data model (which tab a storage key
//!   belongs to, the active-sketch tab label, the right-dock rectangle, and
//!   the Advanced-toggle field-visibility gate).
//! - [`fields`] — the reflection walker that turns a settings struct's
//!   `SettingDef` table into labelled Grid rows.
//! - [`provider_status`] — the hand-tracking provider status row shown under
//!   the "Tracking provider" dropdown.
//! - [`widgets`] — the typed value widgets (`Number`, `Boolean`, `Color`,
//!   `Text`, `Enum`, `RuntimeEnum`, `FilePath`, plus the unreachable-for-now
//!   `Vec2`/`Vec3` branches).
//! - `template_picker` (feature `templates`) — the template-library
//!   `ComboBox` widget, its thumbnail cache, and its two-step delete confirm.
//!   (Plain code span, not an intra-doc link: `mod template_picker` is
//!   `#[cfg(feature = "templates")]`, so a link from this ungated module doc
//!   dangles when the crate is documented without the feature — the same
//!   reason widgets.rs spells it as a code span.)

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use smallvec::SmallVec;

use dock::{KeySnapshot, SettingsDockAdvanced, SettingsDockTab, SettingsTab};

use super::custom_section::{CustomDockSections, DockSectionFn};
use super::registry::SettingsRegistry;
use crate::lifecycle::state::AppState;
use crate::sketch::SketchManifest;
use crate::ui::auto_fade::UiOpacity;
use crate::ui::buttons::SettingsPanelVisible;
use crate::ui::{backdrop_blur_frame, hairline, FrameOptions, OverlayStyle};

mod dock;
mod fields;
mod provider_status;
#[cfg(feature = "templates")]
mod template_picker;
mod widgets;

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
    app.init_resource::<SettingsDockAdvanced>();
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
/// Uses `egui::Area` docked to the right half (see [`dock::dock_rect`]) and wraps
/// content in [`backdrop_blur_frame`] for the translucent frosted-glass look.
/// A header tab bar ([`SettingsTab`]) selects which settings structs render;
/// the selection persists in [`SettingsDockTab`]. Only runs when
/// [`SettingsPanelVisible`] is `true` (gated by the `settings_panel_visible`
/// run condition in [`add_systems`]).
#[allow(
    clippy::too_many_lines,
    reason = "settings dock UI is one cohesive panel; splitting harms readability"
)]
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
    let provider_status = provider_status::provider_status_snapshot(world);

    // Read the active AppState + the sketch manifest to determine which
    // sketch's settings show in the Sketch tab and what label it carries.
    // Both the active-sketch binding and the full sketch-key set (for tab
    // routing) come from the manifest — the single registry every sketch must
    // populate — so no per-sketch `match` arm lives here. Snapshotted before
    // the egui context borrow so the resources stay free for the reflection
    // pass inside the closure. `sketch_keys` is a handful of `&'static str`;
    // `active_label` is uppercased once per frame only while the panel is open.
    let app_state = *world.resource::<State<AppState>>().get();
    let (sketch_keys, active_binding): (SmallVec<[&'static str; 8]>, Option<(&str, &str)>) =
        match world.get_resource::<SketchManifest>() {
            Some(m) => (
                m.sketch_settings_keys().collect(),
                m.settings_binding(app_state),
            ),
            None => (SmallVec::new(), None),
        };
    let active_key = active_binding.map(|(key, _)| key);
    let active_label = active_binding.map_or_else(String::new, |(_, name)| name.to_uppercase());

    // Set true inside the closure when an import/delete changed the store, so the
    // in-memory library resource is reloaded after the closure releases `world`.
    #[cfg(feature = "templates")]
    let mut template_dirty = false;

    // Dock geometry is derived from egui's content rect below, after the egui
    // context is cloned (see the note there) — not from Bevy's `Window`.

    // Tab + Advanced state: read the persisted values, mutate them from the
    // header this frame, write them back after the Area closure releases world.
    let mut selected_tab = world
        .get_resource::<SettingsDockTab>()
        .map_or(SettingsTab::default(), |t| t.0);
    let mut advanced = world
        .get_resource::<SettingsDockAdvanced>()
        .is_some_and(|a| a.0);

    let mut state: bevy::ecs::system::SystemState<EguiContexts<'_, '_>> =
        bevy::ecs::system::SystemState::new(world);
    let Ok(mut contexts) = state.get_mut(world) else {
        return;
    };
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    // `EguiContext` is an `Arc<Mutex<…>>` internally, so `.clone()` is a
    // refcount bump. Cloning here lets us release the `EguiContexts` SystemParam
    // borrow before the `show` closure re-enters `World` to render each section.
    let ctx = ctx.clone();
    state.apply(world);

    // Right-dock geometry from egui's own content rect (points), NOT Bevy's
    // `Window` (logical pixels). The two agree in steady state but diverge for
    // the one frame after a scale-factor change: `bevy_egui`'s screen rect still
    // reflects the previous frame's `pixels_per_point` (a one-frame upstream
    // lag), while `Window` already reports the new size. Mixing the two placed
    // the dock using new-size pixels inside egui's stale-size layout, so it
    // overflowed the right edge for a frame and then snapped in — exactly the
    // reported "panel loads oversized then corrects itself". Feeding `dock_rect`
    // the same units egui lays out in keeps the dock anchored inside whatever
    // egui currently believes the screen to be. `dock_rect` stays pure; only its
    // input source changed, so its unit tests stand. `content_rect()` is the
    // non-deprecated call (see `ui::reload_overlay`); `screen_rect()` is
    // deprecated in egui 0.34 and would fail `-D warnings`.
    let content = ctx.content_rect();
    let (screen_w, screen_h) = if content.width() > 1.0 && content.height() > 1.0 {
        (content.width(), content.height())
    } else {
        // Belt-and-braces against a degenerate rect; unreachable in practice,
        // because `bevy_egui` never publishes one (`update_ui_screen_rect` skips
        // any viewport under 1 point). It is deliberately NOT a guard for egui's
        // *un-initialized* rect: that default is 10000x10000, which passes the
        // check above rather than landing here. Guarding it is unnecessary — the
        // panel cannot draw before a real rect exists, since `settings_panel_visible`
        // requires the operator to have entered a sketch and toggled the cog, long
        // after `update_ui_screen_rect` runs in `PreUpdate`.
        (1280.0, 720.0)
    };
    let (dock_x, dock_y, dock_w, dock_h) = dock::dock_rect(screen_w, screen_h);

    // Snapshot the template library into display rows (lazily decoding each
    // thumbnail into a cached egui texture) before the dock closure borrows
    // `world`. After the ctx clone so the loader can upload textures.
    #[cfg(feature = "templates")]
    let template_rows = template_picker::template_library_rows(world, &ctx);

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

                    // Header: tab bar (the title) on the left, Advanced toggle on
                    // the right. Fixed, outside the scroll body. The active-sketch
                    // label (e.g. "GRAVITY", "FABRIC") comes from the manifest so
                    // the Sketch tab reads the live name, not a static placeholder.
                    draw_dock_header(ui, &mut selected_tab, &mut advanced, &active_label, &style);
                    ui.add_space(4.0);
                    hairline(ui, &style);
                    ui.add_space(8.0);

                    // Body: only the structs routed to the active tab, scrolling
                    // within the fixed dock height. Advanced reveals Dev rows.
                    egui::ScrollArea::vertical()
                        .id_salt("wc-dock-scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for key in &keys {
                                let tab = dock::tab_for_storage_key(key, &sketch_keys);
                                if tab != selected_tab {
                                    continue;
                                }
                                // On the Sketch tab only the *running* sketch's
                                // settings render; every sketch's settings
                                // struct is always registered, so without this
                                // gate all of them would appear. `active_key` is
                                // `None` only when no sketch is active (Home) —
                                // then nothing renders here, never a stale tab.
                                if tab == SettingsTab::Sketch && active_key != Some(*key) {
                                    continue;
                                }
                                fields::render_section_by_key(
                                    world,
                                    ui,
                                    key,
                                    provider_status,
                                    #[cfg(feature = "templates")]
                                    &template_rows,
                                    #[cfg(feature = "templates")]
                                    &mut template_dirty,
                                    advanced,
                                    &style,
                                );
                                render_custom_sections(world, ui, key, &style);
                            }
                        });
                },
            );
        });

    if let Some(mut tab) = world.get_resource_mut::<SettingsDockTab>() {
        tab.0 = selected_tab;
    }
    if let Some(mut adv) = world.get_resource_mut::<SettingsDockAdvanced>() {
        adv.0 = advanced;
    }

    // An import/delete inside the dock changed the store on disk; reload the
    // in-memory library so the dropdown reflects it next frame.
    #[cfg(feature = "templates")]
    if template_dirty {
        if let Some(mut lib) =
            world.get_resource_mut::<crate::templates::resource::TemplateLibrary>()
        {
            lib.reload();
        }
    }
}

/// Draw the dock's header: the tab row on the left (mutating `selected` on
/// click) and the Advanced toggle on the right (mutating `advanced`).
///
/// Each [`SettingsTab`] is a frameless selectable label (the pill background is
/// suppressed in a scope so tabs read as plain text) with a 2 px accent
/// underline beneath the active tab; the caller's hairline below reads as the
/// tab bar's baseline. The Advanced toggle reuses the same lit/underlined
/// treatment — it reads as a fourth, modal tab that reveals a layer rather than
/// switching pages.
///
/// `active_sketch_label` is the live label for [`SettingsTab::Sketch`]
/// (e.g. `"GRAVITY"` or `"FABRIC"`), the active sketch's manifest display name
/// uppercased by the caller (see [`SketchManifest::settings_binding`]). It
/// overrides the static placeholder stored in [`SettingsTab::ORDER`] for that
/// entry.
fn draw_dock_header(
    ui: &mut egui::Ui,
    selected: &mut SettingsTab,
    advanced: &mut bool,
    active_sketch_label: &str,
    style: &OverlayStyle,
) {
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
                // The Sketch tab's label comes from the live AppState so it
                // reads "LINE" in Line and "FABRIC" in Dots; the placeholder
                // in ORDER is never shown directly.
                let display_label = if tab == SettingsTab::Sketch {
                    active_sketch_label
                } else {
                    label
                };
                let is_sel = *selected == tab;
                let color = if is_sel {
                    style.text_primary
                } else {
                    style.text_secondary
                };
                let text = egui::RichText::new(display_label).size(12.5).color(color);
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

            // Advanced toggle, right-aligned in the same header row.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let color = if *advanced {
                    style.accent_bright
                } else {
                    style.text_faint
                };
                let text = egui::RichText::new("ADVANCED").size(11.0).color(color);
                let resp = ui.selectable_label(*advanced, text);
                if resp.clicked() {
                    *advanced = !*advanced;
                }
                if *advanced {
                    ui.painter().hline(
                        resp.rect.x_range(),
                        resp.rect.bottom() + 3.0,
                        egui::Stroke::new(2.0, style.accent),
                    );
                }
            });
        });
    });
}

/// Render any sketch-contributed custom dock sections registered after `key`,
/// immediately below that key's reflected section (render-only; see
/// [`super::custom_section::CustomDockSections`]). The fn pointers are snapshotted
/// (they are `Copy`) so the [`CustomDockSections`] borrow is released before each
/// section re-enters `world`.
fn render_custom_sections(world: &mut World, ui: &mut egui::Ui, key: &str, style: &OverlayStyle) {
    let custom: SmallVec<[DockSectionFn; 2]> = world
        .get_resource::<CustomDockSections>()
        .map(|c| c.for_key(key).collect())
        .unwrap_or_default();
    for render in custom {
        render(world, ui, style);
    }
}
