//! Sketch picker page rendered during [`AppState::Home`].
//!
//! Walks [`AppState::SKETCH_ORDER`] (the canonical 5-sketch order), looks
//! each variant up in the [`SketchManifest`] resource, and renders one
//! tile per cell of a 3×2 grid:
//!
//! - **Registered** sketch → [`render_active_tile`]: solid dark-blue
//!   placeholder background (Task 17 swaps in the screenshot via
//!   `EguiUserTextures`), Orbitron name overlay with gradient fade.
//!   Clickable; sets `NextState<AppState>` to the entry's target state.
//! - **Unregistered** sketch → [`render_placeholder_tile`]: dark fill,
//!   greyed sketch name in Orbitron, "Coming soon" subtitle. Inert.
//!
//! The grid has 6 cells; the 6th stays empty.
//!
//! ## Data flow
//!
//! `draw_sketch_picker` runs in [`bevy_egui::EguiPrimaryContextPass`] gated
//! on [`AppState::Home`]. It reads [`SketchManifest`] (optional — absent
//! before any sketch plugin registers itself) and, on tile click, sets
//! [`NextState<AppState>`] to the entry's target state.

use bevy::prelude::*;
use bevy_egui::egui;

use super::style::OverlayStyle;
use crate::lifecycle::state::AppState;
use crate::sketch::SketchManifest;

/// Plugin: registers [`draw_sketch_picker`] in
/// [`bevy_egui::EguiPrimaryContextPass`], gated on [`AppState::Home`].
pub struct SketchPickerPlugin;

impl Plugin for SketchPickerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            bevy_egui::EguiPrimaryContextPass,
            draw_sketch_picker.run_if(in_state(AppState::Home)),
        );
    }
}

/// Background colour for the picker page, matching v4's `#10161A`.
const PICKER_BACKGROUND: egui::Color32 = egui::Color32::from_rgb(16, 22, 26);

/// Background colour for placeholder ("Coming soon") tiles.
const PLACEHOLDER_FILL: egui::Color32 = egui::Color32::from_rgb(20, 26, 32);

/// Draw the 3×2 sketch-picker grid over the entire viewport.
///
/// Runs as an exclusive-world system so it can clone the egui context and
/// then re-read resources without borrow-checker conflicts (same pattern as
/// `draw_home_button` in `buttons.rs`). Skips silently when the egui plugin
/// is absent (e.g., `MinimalPlugins` test harness).
pub fn draw_sketch_picker(world: &mut World) {
    // Skip when EguiPlugin is absent (MinimalPlugins tests).
    if !world.contains_resource::<bevy_egui::EguiUserTextures>() {
        return;
    }

    // Acquire and clone the egui context so we can release the SystemState
    // borrow before entering the CentralPanel closure.
    let mut state_param: bevy::ecs::system::SystemState<bevy_egui::EguiContexts<'_, '_>> =
        bevy::ecs::system::SystemState::new(world);
    let mut contexts = state_param.get_mut(world);
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let ctx = ctx.clone();
    state_param.apply(world);

    let style = *world.resource::<OverlayStyle>();

    // Snapshot which states are registered so we can look them up inside the
    // closure without holding a borrow on the manifest resource.
    let registered: Vec<AppState> = world
        .get_resource::<SketchManifest>()
        .map(|m| {
            AppState::SKETCH_ORDER
                .iter()
                .filter(|&&s| m.get(s).is_some())
                .copied()
                .collect()
        })
        .unwrap_or_default();

    // Also snapshot the display names for registered entries.
    let display_names: Vec<(&'static str, AppState)> = world
        .get_resource::<SketchManifest>()
        .map(|m| {
            AppState::SKETCH_ORDER
                .iter()
                .filter_map(|&s| m.get(s).map(|e| (e.display_name, e.state)))
                .collect()
        })
        .unwrap_or_default();

    let mut clicked_state: Option<AppState> = None;

    egui::CentralPanel::default()
        .frame(egui::Frame::default().fill(PICKER_BACKGROUND))
        .show(&ctx, |ui| {
            let available = ui.available_size();
            let tile_w = available.x / 3.0;
            let tile_h = available.y / 2.0;
            let tile_size = egui::vec2(tile_w, tile_h);

            egui::Grid::new("sketch-picker-grid")
                .num_columns(3)
                .spacing(egui::vec2(0.0, 0.0))
                .show(ui, |ui| {
                    for (idx, &state) in AppState::SKETCH_ORDER.iter().enumerate() {
                        let is_active = registered.contains(&state);
                        ui.allocate_ui(tile_size, |ui| {
                            if is_active {
                                // Find the display name for this state.
                                let name = display_names
                                    .iter()
                                    .find(|(_, s)| *s == state)
                                    .map_or("?", |(n, _)| *n);
                                if let Some(target) =
                                    render_active_tile(ui, &style, state, name, tile_size)
                                {
                                    clicked_state = Some(target);
                                }
                            } else {
                                render_placeholder_tile(ui, &style, state, tile_size);
                            }
                        });

                        if (idx + 1) % 3 == 0 {
                            ui.end_row();
                        }
                    }
                    // 6th cell: empty spacer so the grid stays 3×2.
                    ui.allocate_ui(tile_size, |_ui| {});
                });
        });

    if let Some(target) = clicked_state {
        if let Some(mut next) = world.get_resource_mut::<NextState<AppState>>() {
            next.set(target);
        }
    }
}

/// Render a registered sketch tile.
///
/// For now paints a solid dark-blue placeholder rect (Task 17 will swap in
/// the screenshot via `EguiUserTextures`). Paints the sketch name in Orbitron
/// at the bottom-left with a gradient fade up from the tile floor.
///
/// Returns `Some(state)` when the tile is clicked.
fn render_active_tile(
    ui: &mut egui::Ui,
    style: &OverlayStyle,
    state: AppState,
    name: &str,
    tile_size: egui::Vec2,
) -> Option<AppState> {
    let (rect, response) = ui.allocate_exact_size(tile_size, egui::Sense::click());

    // TODO Task 17: paint screenshot via EguiUserTextures::add_image lookup.
    // Dark-blue placeholder distinguishes active tiles from "Coming soon" ones.
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::ZERO, egui::Color32::from_rgb(8, 30, 50));

    paint_tile_name(ui, style, rect, name, style.text_color_bright);

    if response.clicked() {
        Some(state)
    } else {
        None
    }
}

/// Render an unregistered sketch tile. Inert — no click handler.
///
/// Shows a dark fill, the sketch's debug name in dim Orbitron, and a
/// "Coming soon" subtitle below.
fn render_placeholder_tile(
    ui: &mut egui::Ui,
    style: &OverlayStyle,
    state: AppState,
    tile_size: egui::Vec2,
) {
    let (rect, _response) = ui.allocate_exact_size(tile_size, egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::ZERO, PLACEHOLDER_FILL);

    let name = format!("{state:?}");
    paint_tile_name(ui, style, rect, &name, style.text_color_dim);

    // "Coming soon" subtitle positioned just below the sketch name.
    let subtitle_pos = egui::pos2(rect.left() + 24.0, rect.bottom() - 24.0);
    ui.painter().text(
        subtitle_pos,
        egui::Align2::LEFT_BOTTOM,
        "Coming soon",
        egui::FontId::new(14.0, egui::FontFamily::Proportional),
        style.text_color_dim,
    );
}

/// Paint the Orbitron sketch name at the bottom-left with a gradient
/// fade up the tile (matching v4's `.work-highlight-name` rule).
///
/// Paints two layers back-to-front:
/// 1. A vertical gradient quad from transparent at `rect.bottom - 30%` to
///    `Color32::from_black_alpha(165)` at `rect.bottom`, softening the contrast
///    between the tile content and the name.
/// 2. The name text in Orbitron at `(rect.left + 24, rect.bottom - 48)`.
fn paint_tile_name(ui: &egui::Ui, style: &OverlayStyle, rect: egui::Rect, name: &str, color: egui::Color32) {
    let painter = ui.painter();

    // Gradient: transparent → black-alpha at the lower 30% of the tile.
    let gradient_top = rect.bottom() - rect.height() * 0.3;
    let gradient_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left(), gradient_top),
        egui::pos2(rect.right(), rect.bottom()),
    );
    let mut mesh = egui::epaint::Mesh::default();
    let top_alpha = egui::Color32::TRANSPARENT;
    let bottom_alpha = egui::Color32::from_black_alpha(165);
    // Four corners of the gradient quad, top-left → top-right → bottom-left → bottom-right.
    mesh.colored_vertex(gradient_rect.left_top(), top_alpha);
    mesh.colored_vertex(gradient_rect.right_top(), top_alpha);
    mesh.colored_vertex(gradient_rect.left_bottom(), bottom_alpha);
    mesh.colored_vertex(gradient_rect.right_bottom(), bottom_alpha);
    // Two triangles covering the quad.
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(egui::Shape::mesh(mesh));

    // Name in Orbitron at the bottom-left.
    painter.text(
        egui::pos2(rect.left() + 24.0, rect.bottom() - 48.0),
        egui::Align2::LEFT_BOTTOM,
        name,
        egui::FontId::new(
            style.picker_tile_name_size,
            egui::FontFamily::Name("orbitron".into()),
        ),
        color,
    );
}
