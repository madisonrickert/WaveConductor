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
use super::text::letter_spaced_label;
use crate::lifecycle::reload::SketchReloadState;
use crate::lifecycle::state::AppState;
use crate::sketch::SketchManifest;

// Phosphor icon glyph for the tile hover overlay.
// Using `egui_phosphor::regular` directly (same crate used by `buttons.rs`).
use egui_phosphor::regular as phosphor;

/// Plugin: registers [`draw_sketch_picker`] in
/// [`bevy_egui::EguiPrimaryContextPass`], gated on [`AppState::Home`] AND
/// `SketchReloadState::is_idle()` so the picker stays hidden during the
/// `FadeOut` → Switch → `FadeIn` reload round-trip.
pub struct SketchPickerPlugin;

impl Plugin for SketchPickerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            bevy_egui::EguiPrimaryContextPass,
            draw_sketch_picker
                .run_if(in_state(AppState::Home))
                .run_if(picker_not_reloading),
        );
    }
}

/// Run condition: returns `true` only when no reload transition is in progress.
///
/// Prevents the picker grid from flashing during the brief `Switch` phase where
/// `AppState == Home` but the screen should be fully blacked out.
fn picker_not_reloading(state: Option<Res<'_, SketchReloadState>>) -> bool {
    state.is_none_or(|s| s.is_idle())
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
    let Ok(mut contexts) = state_param.get_mut(world) else {
        return;
    };
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
                        m.get(s)
                            .map(|e| (e.state, e.display_name, e.screenshot.id()))
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

    // egui 0.34 deprecated the `Context`-based `CentralPanel::show` in favor of
    // `show_inside(ui)`, but bevy_egui only hands us a `Context` (not a `Ui`),
    // so the Context-based show is the only top-level path here (bevy_egui's own
    // examples use it). Revisit if bevy_egui exposes a root `Ui`.
    #[allow(deprecated)]
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
/// with a gradient fade. On hover, three additional layers appear (layer order
/// matches v4's `.work-highlight-sheen.sheen-on-hover` stacking):
///
/// 1. Faint white tint over the whole tile (0 → alpha 40 over 0.3 s).
/// 2. Diagonal sheen sweep (v4's `.sheen-on-hover:after`) with cubic ease-out.
/// 3. Centred PLAY icon (v4's `<FaPlay />`, 0 → alpha 255 over 0.3 s).
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
    // Show a pointer cursor when hovering over a clickable active tile,
    // matching v4's `<a>` / `<Link>` element behaviour.
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

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

    // Hover light tint: a faint white wash over the whole tile, fading in over
    // 0.3 s — same timeline as the play icon. Painted before the sheen sweep
    // so the layer order is: screenshot → name gradient → tint → sheen → play.
    // Matches the "bit of a light overlay" effect from v4's sheen hover.
    let hover_t =
        ui.ctx()
            .animate_bool_with_time(response.id.with("play"), response.hovered(), 0.3);
    if hover_t > 0.0 {
        #[allow(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "hover_t is in [0, 1] so product is in [0, 40]; truncation is intentional"
        )]
        let tint_alpha = (40.0 * hover_t) as u8;
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::ZERO,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, tint_alpha),
        );
    }

    // Sheen-on-hover: two separate animations matching v4's CSS split:
    //   position transition → 0.5 s with cubic ease-out (v4: `transition: 0.5s ease`)
    //   opacity transition  → 0.15 s (quick flash in/out, v4: `transition: 0.15s`)
    // Using distinct animation keys (suffixed with "pos"/"alpha") so each
    // parameter animates independently on the same widget id.
    let position_t_linear =
        ui.ctx()
            .animate_bool_with_time(response.id.with("pos"), response.hovered(), 0.5);
    // Apply cubic ease-out to match CSS `ease` (which decelerates toward the end).
    // Formula: 1 - (1 - t)³  produces fast start, slow finish — the same feel
    // as CSS `transition-timing-function: ease`.
    let position_t = 1.0 - (1.0 - position_t_linear).powi(3);

    let opacity_t =
        ui.ctx()
            .animate_bool_with_time(response.id.with("alpha"), response.hovered(), 0.15);
    if opacity_t > 0.0 {
        paint_sheen(ui, rect, position_t, opacity_t);
    }

    // Play icon overlay: a centred Phosphor PLAY glyph, fading in over 0.3 s
    // on hover. Matches v4's `<FaPlay />` inside `.work-highlight-sheen` (opacity
    // 0 → 1, transition 0.3 s). Uses `hover_t` computed above (same id key "play").
    // Uses the named "phosphor" font family so the PUA codepoint routes directly
    // to the Phosphor font regardless of Inter's PUA overlap.
    if hover_t > 0.0 {
        #[allow(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "hover_t is in [0, 1] so product is in [0, 255]; truncation is intentional"
        )]
        let play_alpha = (255.0 * hover_t) as u8;
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            phosphor::PLAY,
            egui::FontId::new(100.0, egui::FontFamily::Name("phosphor".into())),
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, play_alpha),
        );
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
/// (HomePage.tsx:45–60). Paint-only — no sketch-launch click handler.
///
/// Displays:
/// - `"WaveConductor"` in Orbitron Bold at ~28 pt, centred.
/// - "based on " + hyperlink "hellochar" + " by " + hyperlink "Xiaohan Zhang".
/// - Hyperlinks to [`Madison Rickert`](https://madisonrickert.com) and
///   [`Rich Trapani | LoveTech`](https://lovetech.org) each on their own line.
/// - Plain text "Open Source Licenses" (v4 linked to an internal `/licenses`
///   route; v5 has no in-app licenses page yet — TODO: wire to an in-app modal).
fn render_credits_tile(ui: &mut egui::Ui, style: &OverlayStyle, tile_size: egui::Vec2) {
    let (rect, _response) = ui.allocate_exact_size(tile_size, egui::Sense::hover());
    // Fill matches PLACEHOLDER_FILL / v4's `$dark-gray1`.
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::ZERO, PLACEHOLDER_FILL);

    // Centre all text vertically and horizontally inside the tile.
    // `Align::Center` cross-aligns each widget horizontally (centres text
    // within the tile's width). Vertical centering is achieved by prepending
    // `add_space` equal to half the remaining height above the content block.
    //
    // Estimated content height for the credits block:
    //   28 px  WaveConductor heading
    //    8 px  spacing
    //   12 px  attribution row
    //   12 px  spacing
    //   13 px  Madison Rickert hyperlink
    //   13 px  Rich Trapani hyperlink
    //    8 px  spacing
    //   12 px  Ultraleap attribution
    //   16 px  spacing
    //   11 px  Open Source Licenses label
    //   ──────
    //  133 px  total
    #[allow(
        clippy::items_after_statements,
        reason = "constant is local to this function and belongs beside its usage context"
    )]
    const CREDITS_CONTENT_HEIGHT: f32 = 133.0;
    let mut child_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Center)),
    );
    // Clamp to zero so that a very short tile never produces negative space.
    let top_pad = ((tile_size.y - CREDITS_CONTENT_HEIGHT) * 0.5).max(0.0);
    child_ui.add_space(top_pad);

    // "WaveConductor" heading — Orbitron Bold, large, with v4 letter-spacing.
    // v4 reference: homePage.scss:101 `.credits-content h2 {
    //   letter-spacing: 0.1em }` — 28 pt × 0.1 = 2.8 pt gap.
    letter_spaced_label(
        &mut child_ui,
        "WaveConductor",
        egui::FontId::new(28.0, egui::FontFamily::Name("orbitron".into())),
        style.text_color_bright,
        28.0 * 0.1,
    );
    child_ui.add_space(8.0);

    // Attribution line: "based on hellochar by Xiaohan Zhang" with the two
    // names as clickable hyperlinks matching v4's HomePage.tsx:52–53.
    //
    // egui's `horizontal` always expands to fill the parent's available width,
    // so wrapping it in `top_down(Center)` only centers within the allocation
    // — the row content still left-aligns inside the full-width horizontal.
    // The correct idiom for "horizontally center a row of widgets" is:
    // 1. Put the parent in `top_down(Align::Center)` (already done via child_ui).
    // 2. Use `allocate_ui_with_layout` with a *fixed* width matching the row
    //    content so the child UI has no spare width to push content left.
    //    The `top_down(Center)` parent then centers that fixed-width child.
    //
    // 280 px ≈ "based on " + "hellochar" + " by " + "Xiaohan Zhang" in Inter 12 pt.
    // Nudge if the rendered row clips or has visible gap on a different DPI.
    child_ui.allocate_ui_with_layout(
        egui::vec2(280.0, 20.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(
                egui::RichText::new("based on ")
                    .size(12.0)
                    .color(style.text_color_dim),
            );
            ui.hyperlink_to(
                egui::RichText::new("hellochar")
                    .size(12.0)
                    .color(style.text_color_dim),
                "https://github.com/hellochar/hellochar.com",
            );
            ui.label(
                egui::RichText::new(" by ")
                    .size(12.0)
                    .color(style.text_color_dim),
            );
            ui.hyperlink_to(
                egui::RichText::new("Xiaohan Zhang")
                    .size(12.0)
                    .color(style.text_color_dim),
                "https://github.com/hellochar",
            );
        },
    );
    child_ui.add_space(12.0);

    // Contributors — each a clickable hyperlink.
    child_ui.hyperlink_to(
        egui::RichText::new("Madison Rickert")
            .size(13.0)
            .color(style.text_color_dim),
        "https://madisonrickert.com",
    );
    child_ui.hyperlink_to(
        egui::RichText::new("Rich Trapani | LoveTech")
            .size(13.0)
            .color(style.text_color_dim),
        "https://lovetech.org",
    );
    child_ui.add_space(8.0);

    // Ultraleap attribution (vendor/leapc/ATTRIBUTION.md short form).
    // Required by the Ultraleap Enterprise Tracking Licence §5(b).
    child_ui.label(
        egui::RichText::new("Hand tracking by Ultraleap.")
            .size(12.0)
            .color(style.text_color_dim),
    );
    child_ui.add_space(16.0);

    // Licenses footer — plain text for now; v4 links to an internal /licenses
    // route that v5 does not yet implement.
    // TODO: wire to an in-app open-source-licenses modal (future plan).
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
///   animated over 0.5 s with cubic ease-out applied by the caller (v4:
///   `transition: 0.5s ease`). The caller should pass an already-eased value.
/// - `opacity_t ∈ (0, 1]` — scales every vertex alpha, animated over 0.15 s
///   (quick flash in/out, v4 CSS `transition: 0.15s`). The caller already
///   skips the call when `opacity_t == 0` so this function always receives a
///   positive value.
///
/// Reproduces v4's `homePage.scss:155–164` hover highlight: a vertical
/// band with a **30° clockwise rotation** (matching v4's
/// `transform: rotate(30deg)` on `.work-highlight-sheen`) that sweeps
/// diagonally from left-of-tile to right-of-tile. The band is 60% of
/// the tile width and uses four colour stops
/// (transparent → dim white → bright white peak at strip centre → transparent).
/// The bright peak is centred in the strip (not at 92% as in v4's original
/// CSS) so it remains visible for more of the sweep, and its alpha is boosted
/// to ~0.7 so it reads clearly over dark thumbnails.
///
/// The vertical extent is extended 1.5× beyond the tile top/bottom so that
/// after the 30° rotation the angled strip still covers the full tile.
fn paint_sheen(ui: &egui::Ui, rect: egui::Rect, position_t: f32, opacity_t: f32) {
    let painter = ui.painter();

    // The sheen band is 60% of the tile width. It starts fully off-left
    // at position_t=0 and finishes fully off-right at position_t=1, so the
    // visible highlight sweeps across the tile.
    let sheen_width = rect.width() * 0.6;
    let half = sheen_width * 0.5;
    let travel = rect.width() + sheen_width; // total distance band travels
    let center_x = (rect.left() - half) + travel * position_t;
    let center_y = rect.center().y;

    // Extend the strip's vertical extent by 1.5× the tile height above and
    // below the tile centre. After the 30° rotation this guarantees the
    // diagonal covers the full tile even at the tile's corners.
    let v_extend = rect.height() * 1.5;
    let top = center_y - v_extend;
    let bottom = center_y + v_extend;

    // Four X positions form three quads. Colour stops adapted from v4's gradient:
    //   transparent → dim white → BRIGHT peak (centred in strip) → transparent
    //
    // v4 placed the bright peak at 92% of the strip width (near the right edge),
    // which caused it to pass through too quickly. Centring the bright stop at
    // x=0 of the strip makes the peak visible for more of the sweep duration.
    // Peak alpha boosted from 0.5 (128) → ~0.7 (180) so the highlight reads
    // clearly over dark screenshot thumbnails.
    let xs: [f32; 4] = [
        center_x - half,       // left edge: transparent
        center_x - half * 0.3, // dim entry
        center_x + half * 0.0, // bright peak at strip centre
        center_x + half,       // right edge: transparent
    ];
    let base_colors: [egui::Color32; 4] = [
        egui::Color32::TRANSPARENT,
        egui::Color32::from_white_alpha(33), // ≈ 0.13 × 255 (dim shoulder)
        egui::Color32::from_white_alpha(180), // ≈ 0.7 × 255 (bright peak, up from 0.5)
        egui::Color32::TRANSPARENT,
    ];
    let colors: [egui::Color32; 4] = base_colors.map(|c| scale_sheen_alpha(c, opacity_t));

    // Rotation: v4's `.work-highlight-sheen { transform: rotate(30deg) }`.
    // Rotate each mesh vertex 30° clockwise around the tile centre.
    let tile_center = rect.center();
    let angle: f32 = 30.0_f32.to_radians();
    let cos = angle.cos();
    let sin = angle.sin();
    let rotate = |p: egui::Pos2| -> egui::Pos2 {
        let dx = p.x - tile_center.x;
        let dy = p.y - tile_center.y;
        egui::pos2(
            tile_center.x + dx * cos - dy * sin,
            tile_center.y + dx * sin + dy * cos,
        )
    };

    // Build an 8-vertex mesh: 2 rows (top, bottom) × 4 columns = 8 vertices,
    // 3 quads × 2 triangles each = 6 triangles.
    //
    // Vertex layout: column i contributes vertices 2*i (top) and 2*i+1 (bottom).
    let mut mesh = egui::epaint::Mesh::default();
    for (&x, &color) in xs.iter().zip(colors.iter()) {
        mesh.colored_vertex(rotate(egui::pos2(x, top)), color);
        mesh.colored_vertex(rotate(egui::pos2(x, bottom)), color);
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
fn paint_tile_name(
    ui: &egui::Ui,
    style: &OverlayStyle,
    rect: egui::Rect,
    name: &str,
    color: egui::Color32,
) {
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
