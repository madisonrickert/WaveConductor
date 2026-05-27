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
                    // 6th cell: credits tile (bottom-right), matching v4's
                    // `<div class="work-grid-item credits-block">` in HomePage.tsx:45.
                    ui.allocate_ui(tile_size, |ui| {
                        render_credits_tile(ui, &style, tile_size);
                    });
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

    // Sheen-on-hover: two separate animations matching v4's CSS split:
    //   position transition → 0.5 s (slow diagonal sweep, CSS `transition: 0.5s`)
    //   opacity transition  → 0.15 s (quick flash in/out, CSS `transition: 0.15s`)
    // Using distinct animation keys (suffixed with "pos"/"alpha") so each
    // parameter animates independently on the same widget id.
    let position_t = ui
        .ctx()
        .animate_bool_with_time(response.id.with("pos"), response.hovered(), 0.5);
    let opacity_t = ui
        .ctx()
        .animate_bool_with_time(response.id.with("alpha"), response.hovered(), 0.15);
    if opacity_t > 0.0 {
        paint_sheen(ui, rect, position_t, opacity_t);
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

/// Render the bottom-right credits tile, matching v4's `credits-block` div
/// (HomePage.tsx:45–60). Paint-only — no click handler.
///
/// Displays:
/// - `"WaveConductor"` in Orbitron Bold at ~28 pt, centred.
/// - "based on hellochar by Xiaohan Zhang" in dim text at ~12 pt.
/// - "Madison Rickert" and `"Rich Trapani | LoveTech"` each on their own line.
/// - "Open Source Licenses" in dimmer text at the bottom.
///
/// Links from v4 are omitted for now (egui hyperlinks are non-trivial and this
/// is intentionally a lean parity tile). Deferred to a future polish task.
fn render_credits_tile(ui: &mut egui::Ui, style: &OverlayStyle, tile_size: egui::Vec2) {
    let (rect, _response) = ui.allocate_exact_size(tile_size, egui::Sense::hover());
    // Fill matches PLACEHOLDER_FILL / v4's `$dark-gray1`.
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::ZERO, PLACEHOLDER_FILL);

    // Centre all text vertically and horizontally inside the tile.
    let mut child_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Center)),
    );
    // Push spacing so the block sits roughly in the vertical centre.
    child_ui.add_space(tile_size.y * 0.28);

    // "WaveConductor" heading — Orbitron Bold, large.
    child_ui.label(
        egui::RichText::new("WaveConductor")
            .size(28.0)
            .family(egui::FontFamily::Name("orbitron".into()))
            .color(style.text_color_bright),
    );
    child_ui.add_space(8.0);

    // Attribution line.
    child_ui.label(
        egui::RichText::new("based on hellochar by Xiaohan Zhang")
            .size(12.0)
            .color(style.text_color_dim),
    );
    child_ui.add_space(12.0);

    // Contributors.
    child_ui.label(
        egui::RichText::new("Madison Rickert")
            .size(13.0)
            .color(style.text_color_dim),
    );
    child_ui.label(
        egui::RichText::new("Rich Trapani | LoveTech")
            .size(13.0)
            .color(style.text_color_dim),
    );
    child_ui.add_space(16.0);

    // Licenses footer — dimmer than contributor text.
    child_ui.label(
        egui::RichText::new("Open Source Licenses")
            .size(11.0)
            .color(egui::Color32::from_gray(90)),
    );
}

/// Paint a diagonal sheen sweep across the tile.
///
/// Two independent animation parameters match v4's split CSS transitions:
/// - `position_t ∈ [0, 1]` — controls where the band sits horizontally,
///   animated over 0.5 s (slow sweep, v4 CSS `transition: 0.5s`).
/// - `opacity_t ∈ (0, 1]` — scales every vertex alpha, animated over 0.15 s
///   (quick flash in/out, v4 CSS `transition: 0.15s`). The caller already
///   skips the call when `opacity_t == 0` so this function always receives a
///   positive value.
///
/// Reproduces v4's `homePage.scss:155–164` hover highlight: a vertical
/// band that sweeps from left-of-tile to right-of-tile as `position_t` rises
/// from 0 to 1. The band is 60% of the tile width and uses four colour
/// stops (transparent → dim white → bright white → transparent) painted
/// as three adjacent quads via an `epaint::Mesh`.
///
/// The gradient runs vertically (same colour at top and bottom of each
/// column), giving a flat translucent strip rather than a true rotated
/// gradient — close enough to v4 and avoids shader dependencies.
fn paint_sheen(ui: &egui::Ui, rect: egui::Rect, position_t: f32, opacity_t: f32) {
    let painter = ui.painter();

    // The sheen band is 60% of the tile width. It starts fully off-left
    // at position_t=0 and finishes fully off-right at position_t=1, so the
    // visible highlight sweeps across the tile.
    let sheen_width = rect.width() * 0.6;
    let half = sheen_width * 0.5;
    let travel = rect.width() + sheen_width; // total distance band travels
    let center_x = (rect.left() - half) + travel * position_t;

    let top = rect.top();
    let bottom = rect.bottom();

    // Four X positions form three quads. Colour stops match v4:
    //   transparent → rgba(255,255,255,0.13) → rgba(255,255,255,0.5) → transparent
    // Each stop is alpha-scaled by `opacity_t` so the whole sheen fades in/out
    // independently of the sweep position.
    let xs: [f32; 4] = [
        center_x - half,
        center_x - half * 0.333,
        center_x + half * 0.333,
        center_x + half,
    ];
    let base_colors: [egui::Color32; 4] = [
        egui::Color32::TRANSPARENT,
        egui::Color32::from_white_alpha(33),  // ≈ 0.13 × 255
        egui::Color32::from_white_alpha(128), // ≈ 0.5 × 255
        egui::Color32::TRANSPARENT,
    ];
    let colors: [egui::Color32; 4] = base_colors.map(|c| scale_sheen_alpha(c, opacity_t));

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

/// Scale the alpha channel of a sheen colour vertex by `mul ∈ [0, 1]`.
///
/// Used to apply the fast-opacity animation independently from the slow-position
/// sweep. The `as u8` cast is safe: `f32::from(u8) * mul.clamp(0, 1)` lies in
/// `[0.0, 255.0]`; truncation is the intended rounding mode.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "product is in [0.0, 255.0] by construction; truncation is intentional"
)]
fn scale_sheen_alpha(c: egui::Color32, mul: f32) -> egui::Color32 {
    let a = (f32::from(c.a()) * mul.clamp(0.0, 1.0)) as u8;
    egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
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
