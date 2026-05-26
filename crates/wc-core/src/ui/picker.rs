//! Sketch picker page rendered during [`AppState::Home`].
//!
//! Walks [`AppState::SKETCH_ORDER`] (the canonical 5-sketch order), looks
//! each variant up in the [`SketchManifest`] resource, and renders one
//! tile per cell of a 3×2 grid:
//!
//! - **Registered** sketch → [`render_active_tile`]: screenshot background
//!   via `EguiUserTextures`, Orbitron name overlay with gradient fade,
//!   sheen-on-hover sweep. Clickable; sets `NextState<AppState>` to the
//!   entry's target state.
//! - **Unregistered** sketch → [`render_placeholder_tile`]: dark fill,
//!   greyed sketch name in Orbitron, "Coming soon" subtitle. Inert.
//!
//! The grid has 6 cells; the 6th stays empty.
//!
//! ## Data flow
//!
//! `draw_sketch_picker` runs in [`bevy_egui::EguiPrimaryContextPass`] gated
//! on [`AppState::Home`]. It reads [`SketchManifest`] (optional — absent
//! before any sketch plugin registers itself), registers each entry's
//! screenshot handle with [`bevy_egui::EguiUserTextures`] to obtain an
//! [`egui::TextureId`], and on tile click sets [`NextState<AppState>`] to
//! the entry's target state.

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
///
/// Registers each active tile's screenshot handle with
/// [`bevy_egui::EguiUserTextures`] before entering the egui closure, so the
/// closure only holds snapshotted `egui::TextureId`s (no live world borrows).
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

    // Snapshot active tile metadata: (state, display_name, texture_id).
    // EguiUserTextures::add_image is called here — before the egui closure —
    // so the world borrow is released before we build the UI.
    let active_tiles: Vec<(AppState, &'static str, egui::TextureId)> = {
        let manifest = world.get_resource::<SketchManifest>();
        let entries: Vec<(AppState, &'static str, bevy::asset::AssetId<Image>)> = manifest
            .map(|m| {
                AppState::SKETCH_ORDER
                    .iter()
                    .filter_map(|&s| {
                        m.get(s).map(|e| (e.state, e.display_name, e.screenshot.id()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Release manifest borrow before accessing EguiUserTextures.
        entries
            .into_iter()
            .map(|(state, name, asset_id)| {
                let texture_id = world
                    .resource_mut::<bevy_egui::EguiUserTextures>()
                    .add_image(bevy_egui::EguiTextureHandle::Weak(asset_id));
                (state, name, texture_id)
            })
            .collect()
    };

    // Snapshot which states are active for placeholder detection.
    let active_states: Vec<AppState> = active_tiles.iter().map(|(s, _, _)| *s).collect();

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
                        let is_active = active_states.contains(&state);
                        ui.allocate_ui(tile_size, |ui| {
                            if is_active {
                                // Find the snapshotted entry for this state.
                                if let Some(&(_, name, texture_id)) =
                                    active_tiles.iter().find(|(s, _, _)| *s == state)
                                {
                                    if let Some(target) = render_active_tile(
                                        ui, &style, state, name, tile_size, texture_id,
                                    ) {
                                        clicked_state = Some(target);
                                    }
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
/// Paints the sketch screenshot as the tile background using the provided
/// [`egui::TextureId`] (registered from the manifest's `Handle<Image>` via
/// [`bevy_egui::EguiUserTextures::add_image`]). Overlays the Orbitron name
/// with a gradient fade, then animates a sheen sweep on hover.
///
/// Returns `Some(state)` when the tile is clicked.
fn render_active_tile(
    ui: &mut egui::Ui,
    style: &OverlayStyle,
    state: AppState,
    name: &str,
    tile_size: egui::Vec2,
    texture_id: egui::TextureId,
) -> Option<AppState> {
    let (rect, response) = ui.allocate_exact_size(tile_size, egui::Sense::click());

    // Paint the screenshot as the tile background.
    // UV rect covers the full texture (0,0)→(1,1); tint is WHITE (no colour
    // modification). Before the asset finishes loading egui renders a solid
    // colour placeholder determined by the context's missing-texture policy.
    ui.painter().image(
        texture_id,
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );

    paint_tile_name(ui, style, rect, name, style.text_color_bright);

    // Sheen-on-hover: animate progress [0, 1] over 0.5 s using egui's built-in
    // bool animator. Skip the mesh entirely when progress is zero to avoid
    // painting a zero-alpha strip every frame.
    let hover_t = ui
        .ctx()
        .animate_bool_with_time(response.id, response.hovered(), 0.5);
    if hover_t > 0.0 {
        paint_sheen(ui, rect, hover_t);
    }

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

/// Paint a diagonal sheen sweep across the tile, parametrized by
/// `progress ∈ (0, 1]`.
///
/// Reproduces v4's `homePage.scss:155–164` hover highlight: a vertical
/// band that sweeps from left-of-tile to right-of-tile as `progress` rises
/// from 0 to 1. The band is 60% of the tile width and uses four colour
/// stops (transparent → dim white → bright white → transparent) painted
/// as three adjacent quads via an `epaint::Mesh`.
///
/// The gradient runs vertically (same colour at top and bottom of each
/// column), giving a flat translucent strip rather than a true rotated
/// gradient — close enough to v4 and avoids shader dependencies.
fn paint_sheen(ui: &egui::Ui, rect: egui::Rect, progress: f32) {
    let painter = ui.painter();

    // The sheen band is 60% of the tile width. It starts fully off-left
    // at progress=0 and finishes fully off-right at progress=1, so the
    // visible highlight sweeps across the tile.
    let sheen_width = rect.width() * 0.6;
    let half = sheen_width * 0.5;
    let travel = rect.width() + sheen_width; // total distance band travels
    let center_x = (rect.left() - half) + travel * progress;

    let top = rect.top();
    let bottom = rect.bottom();

    // Four X positions form three quads. Colour stops match v4:
    //   transparent → rgba(255,255,255,0.13) → rgba(255,255,255,0.5) → transparent
    let xs: [f32; 4] = [
        center_x - half,
        center_x - half * 0.333,
        center_x + half * 0.333,
        center_x + half,
    ];
    let colors: [egui::Color32; 4] = [
        egui::Color32::TRANSPARENT,
        egui::Color32::from_white_alpha(33),  // ≈ 0.13 × 255
        egui::Color32::from_white_alpha(128), // ≈ 0.5 × 255
        egui::Color32::TRANSPARENT,
    ];

    // Build an 8-vertex mesh: 2 rows (top, bottom) × 4 columns = 8 vertices,
    // 3 quads × 2 triangles each = 6 triangles.
    //
    // Vertex layout: column i contributes vertices 2*i (top) and 2*i+1 (bottom).
    let mut mesh = egui::epaint::Mesh::default();
    for (&x, &color) in xs.iter().zip(colors.iter()) {
        mesh.colored_vertex(egui::pos2(x, top), color);
        mesh.colored_vertex(egui::pos2(x, bottom), color);
    }
    // Wire up the 3 quads with 6 triangles. Vertex pairs are (0,1), (2,3), (4,5), (6,7).
    for col in 0_u32..3 {
        let base = col * 2;
        // Two triangles forming the quad between column col and column col+1.
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base + 1, base + 3, base + 2);
    }

    painter.add(egui::Shape::mesh(mesh));
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
