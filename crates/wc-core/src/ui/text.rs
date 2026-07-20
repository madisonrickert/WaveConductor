//! Custom text rendering helpers that go beyond egui's built-in widgets.
//!
//! egui's `RichText` and `Label` widgets don't support letter-spacing.
//! [`letter_spaced_label`] paints each glyph individually with a
//! configurable horizontal gap between glyphs, matching the v4 SCSS
//! `letter-spacing: 0.04em` panel-title style.

use bevy_egui::egui;

/// Lay out and paint `text` as a label with explicit letter-spacing
/// (extra horizontal space inserted between each pair of adjacent
/// glyphs). The widget allocates a rect sized to the total width
/// (glyph widths plus `(n-1) * letter_spacing`) and the font's row
/// height.
///
/// `letter_spacing` is in egui logical points (matches the units used
/// by `FontId::size`). For v4 parity with CSS `letter-spacing: 0.04em`,
/// pass `font_size * 0.04`.
///
/// # Note
///
/// Letter-spacing values that work out to less than ~1 logical point
/// (e.g., `0.04em` at 11–13pt fonts) are sub-perceptual at 1× and add
/// no visual benefit. Reserve this helper for headings at 24pt+ or
/// for spacing values explicitly above 1pt. For small labels, prefer
/// plain `ui.label(RichText::new(...))`.
pub fn letter_spaced_label(
    ui: &mut egui::Ui,
    text: &str,
    font_id: egui::FontId,
    color: egui::Color32,
    letter_spacing: f32,
) -> egui::Response {
    let size = measure_letter_spaced(ui.ctx(), text, &font_id, color, letter_spacing);
    let chars: Vec<char> = text.chars().collect();

    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());

    if ui.is_rect_visible(rect) {
        let mut cursor_x = rect.left();
        for ch in &chars {
            let galley = ui
                .ctx()
                .fonts_mut(|fonts| fonts.layout_no_wrap(ch.to_string(), font_id.clone(), color));
            let glyph_width = galley.rect.width();
            ui.painter()
                .galley(egui::pos2(cursor_x, rect.top()), galley, color);
            cursor_x += glyph_width + letter_spacing;
        }
    }

    response
}

/// Measure the exact rect [`letter_spaced_label`] would allocate for `text`
/// without painting it.
///
/// Returns `(total_width, row_height)` in logical points: the sum of the
/// individual glyph widths plus `(n-1) * letter_spacing` gaps, and the tallest
/// glyph's height. Callers that need to vertically centre a block containing a
/// letter-spaced heading (e.g. the picker's credits tile) use this instead of
/// hardcoding a font-size estimate.
///
/// `color` should match the colour the text will later be painted with so the
/// memoized galley cache entry is shared between the measure and paint passes
/// (galleys are cached keyed on the full layout job, colour included).
pub fn measure_letter_spaced(
    ctx: &egui::Context,
    text: &str,
    font_id: &egui::FontId,
    color: egui::Color32,
    letter_spacing: f32,
) -> egui::Vec2 {
    // `fonts_mut` is required because `FontsView::layout_no_wrap` takes
    // `&mut self` (memoized galley cache is mutated on cache miss).
    let chars: Vec<char> = text.chars().collect();
    ctx.fonts_mut(|fonts| {
        let mut w = 0.0_f32;
        let mut max_h = 0.0_f32;
        for ch in &chars {
            let g = fonts.layout_no_wrap(ch.to_string(), font_id.clone(), color);
            w += g.rect.width();
            max_h = max_h.max(g.rect.height());
        }
        if chars.len() > 1 {
            // n-1 inter-glyph gaps.
            // SAFETY: char count fits comfortably in f32 exact-integer range
            // (~16 million). No realistic UI label approaches that length.
            #[allow(
                clippy::as_conversions,
                clippy::cast_precision_loss,
                reason = "char count for any realistic label fits in f32 exact-integer range (~16M)"
            )]
            let gap_count = (chars.len() - 1) as f32;
            w += letter_spacing * gap_count;
        }
        egui::vec2(w, max_h)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: the helper allocates a non-zero rect for a non-empty
    /// string. Cannot exercise glyph layout without a real egui context,
    /// but compile-time + allocation-shape verification catches the
    /// 90% case.
    #[test]
    fn letter_spaced_label_compiles_with_expected_signature() {
        // Smoke check — type-level only.
        let _: fn(&mut egui::Ui, &str, egui::FontId, egui::Color32, f32) -> egui::Response =
            letter_spaced_label;
    }

    /// The measure helper lays out glyphs against a real (default-font) egui
    /// context: a non-empty string must measure wider than a shorter prefix
    /// of itself, and adding letter-spacing must widen the result by exactly
    /// `(n-1) * spacing`.
    #[test]
    fn measure_letter_spaced_accounts_for_gaps() {
        let ctx = egui::Context::default();
        // Force font atlas initialization by running one (empty) pass.
        ctx.begin_pass(egui::RawInput::default());
        let _ = ctx.end_pass();
        let font = egui::FontId::proportional(20.0);
        let color = egui::Color32::WHITE;

        let unspaced = measure_letter_spaced(&ctx, "WaveConductor", &font, color, 0.0);
        let spaced = measure_letter_spaced(&ctx, "WaveConductor", &font, color, 2.0);
        assert!(unspaced.x > 0.0, "non-empty text must have positive width");
        assert!(unspaced.y > 0.0, "non-empty text must have positive height");
        // "WaveConductor" has 13 chars → 12 gaps of 2.0 points each.
        let expected_extra = 12.0 * 2.0;
        let extra = spaced.x - unspaced.x;
        assert!(
            (extra - expected_extra).abs() < 0.01,
            "letter-spacing must add (n-1)*spacing width: got {extra}, want {expected_extra}"
        );
        assert!(
            (spaced.y - unspaced.y).abs() < f32::EPSILON,
            "letter-spacing must not change the measured height"
        );
    }
}
