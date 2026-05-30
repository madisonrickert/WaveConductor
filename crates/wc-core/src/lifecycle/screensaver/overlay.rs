//! Instruction-caption overlay for attract mode (Plan 12, Seam 2).
//!
//! Renders an operator-set lower-third caption (headline + optional subline)
//! while the screensaver is active. The caption is pure opt-in: it draws only
//! when [`ScreensaverSettings::has_caption`] is true (D6 — off by default), and
//! only while `SketchActivity::Screensaver`. It fades in/out with the
//! screensaver so it appears alongside the attract visual rather than snapping.
//!
//! ## egui usage
//!
//! Mirrors [`crate::ui::reload_overlay`]: an `egui::Area` painted over the
//! screen (no frame/title/interaction). Registered into
//! [`bevy_egui::EguiPrimaryContextPass`], which is inert in `MinimalPlugins`
//! test harnesses without `EguiPlugin`, so this is headless-safe.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use super::fade::ScreensaverFade;
use super::settings::ScreensaverSettings;

/// Fraction of the screen height at which the caption baseline sits, measured
/// from the top. `0.82` places it in the lower third without crowding the very
/// bottom edge (projector overscan safety).
const CAPTION_BASELINE_FRAC: f32 = 0.82;

/// Headline font size in logical points.
const HEADLINE_PT: f32 = 34.0;

/// Subline font size in logical points.
const SUBLINE_PT: f32 = 22.0;

/// Vertical gap between the headline baseline and the subline, in points.
const LINE_GAP_PT: f32 = 12.0;

/// Draw the attract-mode instruction caption.
///
/// No-op unless: a caption is configured, the screensaver fade is non-zero, and
/// the egui context is available. Painted with the fade alpha so it shares the
/// attract visual's appearance/disappearance.
///
/// `fade` carries the 0..1 envelope (1 = fully shown); see [`ScreensaverFade`].
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "alpha 0..1 → u8 0..=255 for egui Color32; value is clamped first"
)]
pub fn draw_caption_overlay(
    mut contexts: EguiContexts<'_, '_>,
    settings: Res<'_, ScreensaverSettings>,
    fade: Res<'_, ScreensaverFade>,
) {
    let alpha = fade.alpha();
    if alpha <= 0.0 || !settings.has_caption() {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    // `content_rect()` is the logical-pixel window rect (bevy_egui 0.39 split
    // the deprecated `screen_rect()` into viewport_rect / content_rect).
    let screen = ctx.content_rect();
    let a = (alpha.clamp(0.0, 1.0) * 255.0) as u8;
    let text_color = egui::Color32::from_rgba_unmultiplied(235, 235, 235, a);
    // A soft drop shadow keeps the caption legible over a bright attract frame.
    let shadow = egui::Color32::from_rgba_unmultiplied(0, 0, 0, a / 2);

    let center_x = screen.center().x;
    let baseline_y = screen.top() + screen.height() * CAPTION_BASELINE_FRAC;

    egui::Area::new(egui::Id::new("screensaver_caption_overlay"))
        .fixed_pos(egui::pos2(0.0, 0.0))
        .interactable(false)
        .show(ctx, |ui| {
            let painter = ui.painter();
            let headline = settings.caption_headline.trim();
            let subline = settings.caption_subline.trim();

            let mut y = baseline_y;
            if !headline.is_empty() {
                paint_centered_line(
                    painter,
                    egui::pos2(center_x, y),
                    headline,
                    HEADLINE_PT,
                    text_color,
                    shadow,
                );
                y += HEADLINE_PT + LINE_GAP_PT;
            }
            if !subline.is_empty() {
                paint_centered_line(
                    painter,
                    egui::pos2(center_x, y),
                    subline,
                    SUBLINE_PT,
                    text_color,
                    shadow,
                );
            }
        });
}

/// Paint one horizontally-centred text line with a 1px drop shadow.
fn paint_centered_line(
    painter: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    size_pt: f32,
    color: egui::Color32,
    shadow: egui::Color32,
) {
    let font = egui::FontId::proportional(size_pt);
    let anchor = egui::Align2::CENTER_TOP;
    // Shadow first, offset by one logical pixel down-right.
    painter.text(
        pos + egui::vec2(1.0, 1.0),
        anchor,
        text,
        font.clone(),
        shadow,
    );
    painter.text(pos, anchor, text, font, color);
}
