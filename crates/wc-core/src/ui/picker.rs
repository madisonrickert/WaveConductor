//! Sketch picker page rendered during [`AppState::Home`].
//!
//! Walks [`AppState::SKETCH_ORDER`] (the canonical sketch cycle order —
//! currently Line, Flame, Dots, Cymatics, Radiance; Waves is a de-routed
//! seam, see `AUDIT.md` T5), looks each variant up in the
//! [`SketchManifest`] resource, and renders one tile per cell of an
//! orientation-aware grid — 3×2 in landscape, 2×3 in portrait (see
//! `grid_dims`):
//!
//! - **Registered** sketch → `render_active_tile`: screenshot background
//!   via `EguiUserTextures` (aspect-preserving cover-crop, see
//!   `cover_crop_uv`), Orbitron name overlay with gradient fade,
//!   sheen-on-hover sweep. Clickable; begins a graceful reload into the
//!   entry's target state.
//! - **Unregistered** sketch → `render_placeholder_tile`: dark fill,
//!   greyed sketch name in Orbitron, "Coming soon" subtitle. Inert.
//!
//! The grid has 6 cells; the 6th is the credits tile, whose
//! "Open Source Licenses" link opens the full-screen credits overlay
//! (`super::credits`) — while that overlay is visible this system skips
//! drawing the grid entirely.
//!
//! ## Data flow
//!
//! `draw_sketch_picker` runs in [`bevy_egui::EguiPrimaryContextPass`] gated
//! on [`AppState::Home`]. It reads [`SketchManifest`] (optional — absent
//! before any sketch plugin registers itself), registers each entry's
//! screenshot handle with [`bevy_egui::EguiUserTextures`] to obtain an
//! [`egui::TextureId`], and on tile click calls
//! [`SketchReloadState::begin_fade_out`] with
//! [`crate::lifecycle::reload::ReloadReason::SketchSwitch`] — the same
//! graceful dip-to-black `nav::handle_navigation_actions` drives for a
//! keyboard select (see that module's doc) — instead of writing
//! [`NextState<AppState>`] directly. The `picker_not_reloading` run condition
//! already guarantees no reload is in flight whenever this system runs, so
//! the click handler does not need its own `is_idle` check.

use bevy::prelude::*;
use bevy_egui::egui;

use super::credits::CreditsVisible;
use super::style::OverlayStyle;
use super::text::{letter_spaced_label, measure_letter_spaced};
use crate::audio::state::AudioState;
use crate::lifecycle::reload::{ReloadReason, SketchReloadState};
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
/// `AppState == Home` but the screen should be fully blacked out. Shared with
/// `super::credits::draw_credits_overlay`, which hides for the same reason.
pub(super) fn picker_not_reloading(state: Option<Res<'_, SketchReloadState>>) -> bool {
    state.is_none_or(|s| s.is_idle())
}

/// Background colour for the picker page, matching v4's `#10161A`.
/// Shared with the credits overlay (`super::credits`) so the two Home
/// surfaces read as one continuous page.
pub(super) const PICKER_BACKGROUND: egui::Color32 = egui::Color32::from_rgb(16, 22, 26);

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

    // The full-screen credits overlay (`super::credits`) replaces the grid
    // while it is open — skip the entire draw so the two surfaces never
    // stack interactive widgets.
    if world.get_resource::<CreditsVisible>().is_some_and(|v| v.0) {
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

    let active_tiles = snapshot_active_tiles(world);

    // Snapshot which states are active for placeholder detection.
    let active_states: Vec<AppState> = active_tiles.iter().map(|(s, _, _, _)| *s).collect();

    let mut clicked_state: Option<AppState> = None;
    let mut credits_clicked = false;

    // egui 0.34 deprecated the `Context`-based `CentralPanel::show` in favor of
    // `show_inside(ui)`, but bevy_egui only hands us a `Context` (not a `Ui`),
    // so the Context-based show is the only top-level path here (bevy_egui's own
    // examples use it). Revisit if bevy_egui exposes a root `Ui`.
    #[allow(deprecated)]
    egui::CentralPanel::default()
        .frame(egui::Frame::default().fill(PICKER_BACKGROUND))
        .show(&ctx, |ui| {
            let available = ui.available_size();
            // Orientation-aware layout: 3×2 in landscape, 2×3 in portrait so
            // tiles keep a sensible aspect instead of stretching into tall
            // slivers. `grid_dims` never returns a zero dimension, so the
            // divisions below cannot divide by zero even for a degenerate
            // (zero/NaN-sized) viewport.
            let (cols, rows) = grid_dims(available.x, available.y);
            let tile_w = available.x / f32::from(cols);
            let tile_h = available.y / f32::from(rows);
            let tile_size = egui::vec2(tile_w, tile_h);

            egui::Grid::new("sketch-picker-grid")
                .num_columns(usize::from(cols))
                .spacing(egui::vec2(0.0, 0.0))
                .show(ui, |ui| {
                    for (idx, &state) in AppState::SKETCH_ORDER.iter().enumerate() {
                        let is_active = active_states.contains(&state);
                        ui.allocate_ui(tile_size, |ui| {
                            if is_active {
                                // Find the snapshotted entry for this state.
                                if let Some(&(_, name, texture_id, tex_size)) =
                                    active_tiles.iter().find(|(s, _, _, _)| *s == state)
                                {
                                    if let Some(target) = render_active_tile(
                                        ui, &style, state, name, tile_size, texture_id, tex_size,
                                    ) {
                                        clicked_state = Some(target);
                                    }
                                }
                            } else {
                                render_placeholder_tile(ui, &style, state, tile_size);
                            }
                        });

                        if (idx + 1) % usize::from(cols) == 0 {
                            ui.end_row();
                        }
                    }
                    // 6th cell: credits tile (last cell in either orientation),
                    // matching v4's `<div class="work-grid-item credits-block">`
                    // in HomePage.tsx:45.
                    ui.allocate_ui(tile_size, |ui| {
                        if render_credits_tile(ui, &style, tile_size) {
                            credits_clicked = true;
                        }
                    });
                });
        });

    if credits_clicked {
        // Open the full-screen credits overlay (`super::credits`). The overlay
        // system is ordered after this one in the same egui pass, so it draws
        // this very frame — no one-frame grid flash.
        if let Some(mut visible) = world.get_resource_mut::<CreditsVisible>() {
            visible.0 = true;
        }
    }

    if let Some(target) = clicked_state {
        // Graceful dip-to-black instead of an instant `NextState` write —
        // same `ReloadReason::SketchSwitch` path as a keyboard sketch-select
        // (see `nav::handle_navigation_actions`'s module doc). Safe to begin
        // unconditionally: `picker_not_reloading` already gates this whole
        // system on `SketchReloadState::is_idle()`, so no reload can be in
        // flight when a click lands.
        let now = world.resource::<Time>().elapsed();
        let pre_fade_volume = world.get_resource::<AudioState>().map_or(1.0, |s| s.volume);
        if let Some(mut reload_state) = world.get_resource_mut::<SketchReloadState>() {
            reload_state.begin_fade_out(now, pre_fade_volume, target, ReloadReason::SketchSwitch);
        }
    }
}

/// Snapshot active tile metadata: `(state, display_name, texture_id, texture
/// pixel size)` for every registered sketch, in `SKETCH_ORDER`.
///
/// `EguiUserTextures::add_image` is called here — before the egui closure in
/// [`draw_sketch_picker`] — so the world borrow is released before the UI is
/// built. The pixel size feeds the cover-crop UV math ([`cover_crop_uv`]); it
/// is `Vec2::ZERO` until the async PNG load completes, which falls back to
/// the full-texture UV (the placeholder colour has no aspect to preserve).
fn snapshot_active_tiles(
    world: &mut World,
) -> Vec<(AppState, &'static str, egui::TextureId, egui::Vec2)> {
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
            let tex_size = world
                .get_resource::<Assets<Image>>()
                .and_then(|images| images.get(asset_id))
                .map_or(egui::Vec2::ZERO, |img| {
                    let s = img.size_f32();
                    egui::vec2(s.x, s.y)
                });
            (state, name, texture_id, tex_size)
        })
        .collect()
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
    tex_size: egui::Vec2,
) -> Option<AppState> {
    let (rect, response) = ui.allocate_exact_size(tile_size, egui::Sense::click());
    // Show a pointer cursor when hovering over a clickable active tile,
    // matching v4's `<a>` / `<Link>` element behaviour.
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

    // Paint the screenshot as the tile background with an aspect-preserving
    // cover-crop (CSS `object-fit: cover` semantics): the UV sub-rect trims
    // the overflowing texture axis so the image fills the tile without
    // distortion in either orientation. `tex_size` is `Vec2::ZERO` until the
    // async load completes, which yields the full-UV fallback. Tint is WHITE
    // (no colour modification). Before the asset finishes loading egui
    // renders a solid colour placeholder determined by the context's
    // missing-texture policy.
    ui.painter().image(
        texture_id,
        rect,
        cover_crop_uv(tile_size, tex_size),
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

/// Vertical gaps (logical points) between the credits-tile content rows.
/// Named so the measurement pass and the paint pass cannot drift apart.
const CREDITS_GAP_AFTER_TITLE: f32 = 8.0;
/// Gap below the "based on … by …" attribution row.
const CREDITS_GAP_AFTER_ATTRIBUTION: f32 = 12.0;
/// Gap between the two contributor hyperlinks.
const CREDITS_GAP_BETWEEN_LINKS: f32 = 4.0;
/// Gap below the contributor hyperlinks.
const CREDITS_GAP_AFTER_LINKS: f32 = 8.0;

/// Render the credits tile (last grid cell), matching v4's `credits-block`
/// div (HomePage.tsx:45–60).
///
/// Displays:
/// - `"WaveConductor"` in Orbitron Bold at ~28 pt, centred.
/// - "based on " + hyperlink "hellochar" + " by " + hyperlink "Xiaohan Zhang".
/// - Hyperlinks to [`Madison Rickert`](https://madisonrickert.com) and
///   [`Rich Trapani | LoveTech`](https://lovetech.org) each on their own line.
/// - An "Open Source Licenses" link that opens the full-screen credits
///   overlay (`super::credits`); returns `true` on the frame it is clicked.
///
/// ## Centering is measured, not estimated
///
/// Every row height (and the attribution row's width) is measured from the
/// actual laid-out galleys via [`measure_letter_spaced`] / `layout_no_wrap`,
/// then summed with the named `CREDITS_GAP_*` constants, so vertical
/// centering and the row width track font metrics and DPI instead of the
/// hardcoded 133 px / 280 px guesses this replaced. The child UI zeroes
/// egui's `item_spacing` so the explicit gaps are the only spacing in play,
/// which is what makes the measured sum exact. Reflows correctly for both
/// landscape and the narrower/taller portrait tiles.
fn render_credits_tile(ui: &mut egui::Ui, style: &OverlayStyle, tile_size: egui::Vec2) -> bool {
    let (rect, _response) = ui.allocate_exact_size(tile_size, egui::Sense::hover());
    // Fill matches PLACEHOLDER_FILL / v4's `$dark-gray1`.
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::ZERO, PLACEHOLDER_FILL);

    let metrics = credits_tile_metrics(ui, style);

    // Centre all text vertically and horizontally inside the tile.
    // `Align::Center` cross-aligns each widget horizontally (centres text
    // within the tile's width). Vertical centering prepends `add_space`
    // equal to half the remaining height above the measured content block.
    let mut child_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Center)),
    );
    // Explicit gaps only — see the doc comment on why item_spacing is zeroed.
    child_ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
    // Keep every painted glyph inside the tile even when the tile is narrower
    // than the content (extreme portrait aspect): clip to the tile bounds.
    child_ui.set_clip_rect(rect.intersect(child_ui.clip_rect()));
    child_ui.add_space(centered_top_pad(rect.height(), metrics.content_height));

    // "WaveConductor" heading — Orbitron Bold, large, with v4 letter-spacing.
    // v4 reference: homePage.scss:101 `.credits-content h2 {
    //   letter-spacing: 0.1em }` — 28 pt × 0.1 = 2.8 pt gap.
    let (title_font, title_spacing) = credits_title_font();
    letter_spaced_label(
        &mut child_ui,
        "WaveConductor",
        title_font,
        style.text_color_bright,
        title_spacing,
    );
    child_ui.add_space(CREDITS_GAP_AFTER_TITLE);

    paint_attribution_row(&mut child_ui, style, metrics.attribution_size);
    child_ui.add_space(CREDITS_GAP_AFTER_ATTRIBUTION);

    // Contributors — each a clickable hyperlink.
    child_ui.hyperlink_to(
        egui::RichText::new("Madison Rickert")
            .size(13.0)
            .color(style.text_color_dim),
        "https://madisonrickert.com",
    );
    child_ui.add_space(CREDITS_GAP_BETWEEN_LINKS);
    child_ui.hyperlink_to(
        egui::RichText::new("Rich Trapani | LoveTech")
            .size(13.0)
            .color(style.text_color_dim),
        "https://lovetech.org",
    );
    child_ui.add_space(CREDITS_GAP_AFTER_LINKS);

    // The Ultraleap attribution (Enterprise Tracking Licence §5(b)) lives on
    // the credits overlay's HAND TRACKING section (`super::credits`), not on
    // this tile — the licence requires it to appear, not to appear here.

    // Licenses footer — a real link now: opens the full-screen credits /
    // open-source-licenses overlay (`super::credits`), v5's equivalent of
    // v4's internal `/licenses` route.
    child_ui
        .add(egui::Link::new(
            egui::RichText::new("Open Source Licenses")
                .size(11.0)
                .color(egui::Color32::from_gray(90)),
        ))
        .clicked()
}

/// Measured layout metrics for the credits tile's content block.
struct CreditsTileMetrics {
    /// Total height of the content stack, rows plus `CREDITS_GAP_*` gaps.
    content_height: f32,
    /// Exact size of the "based on … by …" attribution row (galley-width sum
    /// × tallest galley), painted with zero item spacing.
    attribution_size: egui::Vec2,
}

/// The credits-tile title font (Orbitron Bold 28 pt) and its letter-spacing
/// (`0.1em` per v4's homePage.scss:101), shared by the measure and paint
/// passes so they cannot drift apart.
fn credits_title_font() -> (egui::FontId, f32) {
    (
        egui::FontId::new(28.0, egui::FontFamily::Name("orbitron".into())),
        28.0 * 0.1,
    )
}

/// Measure every credits-tile row from its actual laid-out galley and sum the
/// stack with the `CREDITS_GAP_*` constants.
///
/// The attribution row segments carry their own separating spaces, so the row
/// is painted with `item_spacing.x == 0` and its width is exactly the galley
/// sum computed here.
fn credits_tile_metrics(ui: &egui::Ui, style: &OverlayStyle) -> CreditsTileMetrics {
    let (title_font, title_spacing) = credits_title_font();
    let title_size = measure_letter_spaced(
        ui.ctx(),
        "WaveConductor",
        &title_font,
        style.text_color_bright,
        title_spacing,
    );

    let measure_line = |text: &str, size: f32| -> egui::Vec2 {
        ui.ctx().fonts_mut(|fonts| {
            let galley = fonts.layout_no_wrap(
                text.to_owned(),
                egui::FontId::proportional(size),
                style.text_color_dim,
            );
            galley.rect.size()
        })
    };

    let attribution_segments = ["based on ", "hellochar", " by ", "Xiaohan Zhang"];
    let mut attribution_width = 0.0_f32;
    let mut attribution_height = 0.0_f32;
    for segment in attribution_segments {
        let s = measure_line(segment, 12.0);
        attribution_width += s.x;
        attribution_height = attribution_height.max(s.y);
    }

    let link_madison = measure_line("Madison Rickert", 13.0);
    let link_lovetech = measure_line("Rich Trapani | LoveTech", 13.0);
    let licenses = measure_line("Open Source Licenses", 11.0);

    let content_height = title_size.y
        + CREDITS_GAP_AFTER_TITLE
        + attribution_height
        + CREDITS_GAP_AFTER_ATTRIBUTION
        + link_madison.y
        + CREDITS_GAP_BETWEEN_LINKS
        + link_lovetech.y
        + CREDITS_GAP_AFTER_LINKS
        + licenses.y;

    CreditsTileMetrics {
        content_height,
        attribution_size: egui::vec2(attribution_width, attribution_height),
    }
}

/// Paint the "based on hellochar by Xiaohan Zhang" attribution row, with the
/// two names as clickable hyperlinks matching v4's HomePage.tsx:52–53.
///
/// egui's `horizontal` always expands to fill the parent's available width,
/// so wrapping it in `top_down(Center)` only centers within the allocation —
/// the row content still left-aligns inside the full-width horizontal. The
/// correct idiom for "horizontally center a row of widgets" is:
/// 1. Put the parent in `top_down(Align::Center)` (the caller's child UI).
/// 2. Use `allocate_ui_with_layout` with a *fixed* size matching the measured
///    row content ([`credits_tile_metrics`]) so the child UI has no spare
///    width to push content left. The `top_down(Center)` parent then centers
///    that fixed-width child.
fn paint_attribution_row(ui: &mut egui::Ui, style: &OverlayStyle, row_size: egui::Vec2) {
    ui.allocate_ui_with_layout(
        row_size,
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
/// Because of that deliberate over-extension the mesh is painted through a
/// clip rect intersected with the tile bounds — v4 relied on the container's
/// CSS `overflow: hidden` for the same containment, and the original egui
/// port dropped it, letting the band bleed into neighbouring tiles.
fn paint_sheen(ui: &egui::Ui, rect: egui::Rect, position_t: f32, opacity_t: f32) {
    // `with_clip_rect` intersects with the painter's existing clip, so this
    // can only shrink the paintable region, never escape a parent clip.
    let painter = ui.painter().with_clip_rect(rect);

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
    // Clip to the tile: the gradient quad is derived from `rect` and cannot
    // spill, but a long name at 40 pt can overflow the right edge of a narrow
    // (portrait) tile — clip keeps it from bleeding into the neighbour.
    let painter = ui.painter().with_clip_rect(rect);

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

/// Grid dimensions `(columns, rows)` for the picker, from the viewport size.
///
/// Landscape (or square/degenerate) viewports use the classic 3×2; portrait
/// viewports (`height > width`) flip to 2×3 so tiles keep a sensible aspect
/// instead of stretching into tall slivers. Both grids hold exactly 6 cells
/// (5 sketches + the credits tile).
///
/// NaN or non-positive inputs fail the `height > width` comparison and fall
/// through to the landscape default, and neither returned dimension is ever
/// zero — callers can divide by them unconditionally.
fn grid_dims(width: f32, height: f32) -> (u16, u16) {
    if height > width {
        (2, 3)
    } else {
        (3, 2)
    }
}

/// UV sub-rect that covers `tile` with `tex` without distortion — CSS
/// `object-fit: cover` semantics.
///
/// Whichever texture axis overflows the tile's aspect is cropped equally from
/// both sides; the other axis spans the full `0..1` range. Returns the full
/// UV rect when either size is degenerate (zero, negative, or NaN — e.g. a
/// screenshot still loading reports `Vec2::ZERO`), painting the texture
/// stretched exactly as before, which for the not-yet-loaded placeholder
/// colour is invisible.
fn cover_crop_uv(tile: egui::Vec2, tex: egui::Vec2) -> egui::Rect {
    let full = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    // NaN fails every `>` comparison, so degenerate inputs land here too.
    if !(tile.x > 0.0 && tile.y > 0.0 && tex.x > 0.0 && tex.y > 0.0) {
        return full;
    }
    let tile_aspect = tile.x / tile.y;
    let tex_aspect = tex.x / tex.y;
    if tex_aspect > tile_aspect {
        // Texture is wider than the tile: crop left/right.
        // Fraction of the texture width that fills the tile at cover scale.
        let u_span = tile_aspect / tex_aspect;
        let u_min = (1.0 - u_span) * 0.5;
        egui::Rect::from_min_max(egui::pos2(u_min, 0.0), egui::pos2(u_min + u_span, 1.0))
    } else {
        // Texture is taller than the tile: crop top/bottom.
        let v_span = tex_aspect / tile_aspect;
        let v_min = (1.0 - v_span) * 0.5;
        egui::Rect::from_min_max(egui::pos2(0.0, v_min), egui::pos2(1.0, v_min + v_span))
    }
}

/// Top padding that vertically centres a `content_height`-tall block inside a
/// `container_height`-tall region, clamped to zero so a container shorter
/// than its content pins the block to the top instead of producing negative
/// space.
fn centered_top_pad(container_height: f32, content_height: f32) -> f32 {
    ((container_height - content_height) * 0.5).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_is_3x2_in_landscape_and_square() {
        assert_eq!(grid_dims(1920.0, 1080.0), (3, 2));
        // Square counts as landscape (strict `>` comparison).
        assert_eq!(grid_dims(1000.0, 1000.0), (3, 2));
    }

    #[test]
    fn grid_is_2x3_in_portrait() {
        assert_eq!(grid_dims(1080.0, 1920.0), (2, 3));
    }

    #[test]
    fn grid_dims_degenerate_sizes_fall_back_to_landscape() {
        assert_eq!(grid_dims(0.0, 0.0), (3, 2));
        assert_eq!(grid_dims(f32::NAN, f32::NAN), (3, 2));
        assert_eq!(grid_dims(f32::NAN, 100.0), (3, 2));
        // Both dimensions are nonzero in every case — division is always safe.
        let (c, r) = grid_dims(-5.0, -10.0);
        assert!(c > 0 && r > 0);
    }

    /// Matching aspect: the full texture is used, no crop.
    #[test]
    fn cover_crop_uv_matching_aspect_is_full_rect() {
        let uv = cover_crop_uv(egui::vec2(640.0, 360.0), egui::vec2(1920.0, 1080.0));
        assert!((uv.min.x - 0.0).abs() < 1e-6 && (uv.max.x - 1.0).abs() < 1e-6);
        assert!((uv.min.y - 0.0).abs() < 1e-6 && (uv.max.y - 1.0).abs() < 1e-6);
    }

    /// A wide (16:9) screenshot in a square tile crops left/right equally:
    /// the used width fraction is (1:1)/(16:9) = 9/16, centred.
    #[test]
    fn cover_crop_uv_wide_texture_in_square_tile_crops_sides() {
        let uv = cover_crop_uv(egui::vec2(500.0, 500.0), egui::vec2(1920.0, 1080.0));
        let expected_span = 9.0 / 16.0;
        let expected_min = (1.0 - expected_span) * 0.5;
        assert!(
            (uv.width() - expected_span).abs() < 1e-6,
            "span {}",
            uv.width()
        );
        assert!((uv.min.x - expected_min).abs() < 1e-6, "min {}", uv.min.x);
        // Vertical axis spans the full texture.
        assert!((uv.min.y - 0.0).abs() < 1e-6 && (uv.max.y - 1.0).abs() < 1e-6);
    }

    /// A landscape screenshot in a taller-than-wide (portrait) tile crops
    /// top/bottom — the horizontal axis spans the full texture.
    #[test]
    fn cover_crop_uv_wide_texture_in_portrait_tile_crops_top_bottom() {
        // Portrait tile 540×640 (aspect 0.84), 16:9 texture (aspect 1.78).
        let uv = cover_crop_uv(egui::vec2(540.0, 640.0), egui::vec2(1920.0, 1080.0));
        // Texture is wider than the tile → sides cropped, not top/bottom.
        assert!((uv.min.y - 0.0).abs() < 1e-6 && (uv.max.y - 1.0).abs() < 1e-6);
        let expected_span = (540.0 / 640.0) / (1920.0 / 1080.0);
        assert!((uv.width() - expected_span).abs() < 1e-6);
        // Crop is centred.
        assert!((uv.min.x - (1.0 - uv.width()) * 0.5).abs() < 1e-6);
    }

    /// A tall texture in a wide tile crops top/bottom, centred.
    #[test]
    fn cover_crop_uv_tall_texture_in_wide_tile_crops_top_bottom() {
        let uv = cover_crop_uv(egui::vec2(640.0, 360.0), egui::vec2(1080.0, 1920.0));
        assert!((uv.min.x - 0.0).abs() < 1e-6 && (uv.max.x - 1.0).abs() < 1e-6);
        let expected_span = (1080.0 / 1920.0) / (640.0 / 360.0);
        assert!((uv.height() - expected_span).abs() < 1e-6);
        assert!((uv.min.y - (1.0 - uv.height()) * 0.5).abs() < 1e-6);
    }

    /// Zero / NaN texture or tile sizes must not divide by zero — they fall
    /// back to the full UV rect (stretched paint, matching the pre-crop
    /// behaviour for the not-yet-loaded placeholder).
    #[test]
    fn cover_crop_uv_degenerate_sizes_fall_back_to_full_rect() {
        let full = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        assert_eq!(
            cover_crop_uv(egui::vec2(0.0, 0.0), egui::vec2(1920.0, 1080.0)),
            full
        );
        assert_eq!(
            cover_crop_uv(egui::vec2(640.0, 360.0), egui::Vec2::ZERO),
            full
        );
        assert_eq!(
            cover_crop_uv(egui::vec2(f32::NAN, 360.0), egui::vec2(1920.0, 1080.0)),
            full
        );
        assert_eq!(
            cover_crop_uv(egui::vec2(640.0, 360.0), egui::vec2(-64.0, 64.0)),
            full
        );
    }

    #[test]
    fn centered_top_pad_centers_and_clamps() {
        // Plenty of room: half the leftover space above.
        assert!((centered_top_pad(500.0, 133.0) - 183.5).abs() < 1e-6);
        // Content taller than the container: clamp to zero, never negative.
        assert!((centered_top_pad(100.0, 133.0) - 0.0).abs() < f32::EPSILON);
    }
}
