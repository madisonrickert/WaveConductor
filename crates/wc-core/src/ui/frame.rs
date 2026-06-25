//! Shared frame helper for translucent overlay panels.
//!
//! Wraps any panel content in three painter layers — back-to-front:
//! the [`super::blur::callback::BackdropBlurPaintCallback`] (a textured quad
//! sampling the blurred backdrop), a translucent tint rect using
//! [`super::style::OverlayStyle::panel_fill`], and the caller-supplied content
//! drawn inside the padded inner rect.
//!
//! The blur callback is skipped when [`super::blur::BackdropBlurEnabled`] is
//! `false`, when [`super::auto_fade::UiOpacity::current`] is below 1%, or when
//! [`super::blur::BackdropBlurTexture`] hasn't been allocated yet. In all skip
//! cases the helper still draws the tint + content so the panel remains visible.

use bevy_egui::egui;
use bevy_egui::render::EguiBevyPaintCallback;

use super::blur::callback::BackdropBlurPaintCallback;
use super::style::OverlayStyle;

const BACKDROP_BLUR_OPACITY_THRESHOLD: f32 = 0.01;

/// Frame configuration passed to [`backdrop_blur_frame`].
#[derive(Clone, Copy)]
pub struct FrameOptions {
    /// Corner radius for the panel background in egui points, stored as `u8`
    /// to match [`OverlayStyle`]'s integer palette fields and egui's
    /// [`egui::CornerRadius::same`] constructor.
    pub corner_radius: u8,
    /// Inner padding — the content closure receives a [`egui::Ui`] shrunk by
    /// this amount on all sides.
    pub padding: egui::Vec2,
    /// Multiplier applied to the panel's fill and stroke alpha channels.
    /// Pass `UiOpacity::current` so the panel fades with the rest of the chrome.
    pub opacity_mul: f32,
}

impl FrameOptions {
    /// Defaults that match v4 panel chrome: 10 px radius, 20×16 padding,
    /// fully opaque.
    ///
    /// Callers that drive opacity from [`super::auto_fade::UiOpacity`] should
    /// override `opacity_mul` before passing this to [`backdrop_blur_frame`].
    pub fn panel(style: &OverlayStyle) -> Self {
        Self {
            corner_radius: style.panel_corner_radius,
            padding: egui::Vec2::new(20.0, 16.0),
            opacity_mul: 1.0,
        }
    }
}

/// Allocate a rect, paint the chrome (blur callback + tint + stroke), and run
/// `content` inside the padded inner rect.
///
/// Back-to-front paint order:
/// 1. [`BackdropBlurPaintCallback`] — composites the blurred backdrop behind
///    the panel; silently a no-op when the texture or pipeline is not yet ready.
/// 2. Translucent tint rect (alpha scaled by `options.opacity_mul`).
/// 3. Caller-supplied `content`, drawn in a child [`egui::Ui`] clipped to the
///    inner rect (`outer_rect` shrunk by `options.padding` on all sides).
///
/// Returns the [`egui::Response`] for the outer allocation so callers can
/// detect hover / clicks on the panel background if needed.
pub fn backdrop_blur_frame(
    ui: &mut egui::Ui,
    style: &OverlayStyle,
    options: FrameOptions,
    content: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let desired = ui.available_size();
    let (outer_rect, response) = ui.allocate_exact_size(desired, egui::Sense::hover());

    let painter = ui.painter();

    // 1. Blur callback. Match the render node's opacity gate: once the chrome
    // is effectively invisible the blur texture is no longer refreshed, so
    // continuing to composite it would paint a stale frosted rectangle.
    if should_paint_backdrop_blur(options.opacity_mul) {
        let callback = EguiBevyPaintCallback::new_paint_callback(
            outer_rect,
            BackdropBlurPaintCallback {
                // BackdropBlurPaintCallback stores corner_radius as f32 for
                // shader uniform upload (physical-pixel conversion happens there).
                corner_radius: f32::from(options.corner_radius),
                rect: outer_rect,
            },
        );
        painter.add(callback);
    }

    // 2. Translucent tint with stroke, both alpha-multiplied by opacity_mul.
    let fill = scale_alpha(style.panel_fill, options.opacity_mul);
    let stroke_color = scale_alpha(style.panel_stroke, options.opacity_mul);
    painter.add(egui::Shape::Rect(egui::epaint::RectShape::new(
        outer_rect,
        egui::CornerRadius::same(options.corner_radius),
        fill,
        egui::Stroke::new(1.0, stroke_color),
        egui::epaint::StrokeKind::Inside,
    )));

    // 3. Content inside the padded inner rect.
    let inner_rect = outer_rect.shrink2(options.padding);
    let mut content_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner_rect)
            .layout(*ui.layout()),
    );
    content(&mut content_ui);

    response
}

/// Draw a full-width 1px in-panel hairline rule using
/// [`OverlayStyle::hairline`], allocating its own row.
///
/// The quiet in-panel divider for the settings/debug docks — much fainter than
/// the outer panel stroke and than egui's default `ui.separator()`, which is
/// too bright against the frosted glass.
pub fn hairline(ui: &mut egui::Ui, style: &OverlayStyle) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 1.0), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, style.hairline),
    );
}

/// Multiply the alpha channel of `color` by `mul`, clamped to [0.0, 1.0].
///
/// The RGB channels are left untouched; only the alpha is scaled. This is the
/// correct operation for pre-multiplied-alpha-free Color32 values (egui's
/// default storage format).
///
/// The `as u8` cast is safe because the operand is `f32::from(u8) * f32` where
/// `mul` is clamped to `[0.0, 1.0]`, so the product lies in `[0.0, 255.0]`
/// and truncation is the intended rounding behaviour.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "product is in [0.0, 255.0] by construction; truncation is intentional"
)]
fn scale_alpha(color: egui::Color32, mul: f32) -> egui::Color32 {
    let a = (f32::from(color.a()) * mul.clamp(0.0, 1.0)) as u8;
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

/// Whether a frosted-glass blur callback should be submitted for this opacity.
///
/// Kept in sync with the blur render node's `ExtractedUiOpacity < 0.01` skip
/// condition so faded chrome cannot keep compositing a stale blur texture.
#[must_use]
pub(crate) fn should_paint_backdrop_blur(opacity_mul: f32) -> bool {
    opacity_mul >= BACKDROP_BLUR_OPACITY_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_alpha_at_full_opacity_is_unchanged() {
        let c = egui::Color32::from_rgba_unmultiplied(20, 40, 60, 200);
        assert_eq!(scale_alpha(c, 1.0).a(), 200);
    }

    #[test]
    fn scale_alpha_at_half_opacity_halves_alpha() {
        let c = egui::Color32::from_rgba_unmultiplied(20, 40, 60, 200);
        assert_eq!(scale_alpha(c, 0.5).a(), 100);
    }

    #[test]
    fn scale_alpha_at_zero_opacity_is_invisible() {
        let c = egui::Color32::from_rgba_unmultiplied(20, 40, 60, 200);
        assert_eq!(scale_alpha(c, 0.0).a(), 0);
    }

    #[test]
    fn blur_callback_is_skipped_when_opacity_matches_render_node_skip() {
        assert!(!should_paint_backdrop_blur(0.0));
        assert!(!should_paint_backdrop_blur(0.009));
        assert!(should_paint_backdrop_blur(0.01));
    }
}
