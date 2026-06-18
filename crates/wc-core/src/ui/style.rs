//! Egui [`Style`] configuration matched to v4's overlay SCSS.
//!
//! All constants here cite the v4 source line they derive from so future
//! re-tuning catches drift in both directions: if v4's SCSS changes, the
//! cited line points the maintainer at what to update; if these constants
//! are tweaked first, the citation makes the divergence explicit.
//!
//! Values come from:
//! - `.worktrees/v4/src/styles/overlayPanel.scss`
//! - `.worktrees/v4/src/styles/overlayButton.scss`
//! - `.worktrees/v4/src/settings/DevSettingsPanel/advancedSettingsPanel.scss`
//!
//! [`Style`]: bevy_egui::egui::Style

use bevy::prelude::*;
use bevy_egui::egui;

/// Static palette + sizing values used everywhere in the overlay surface.
///
/// Every field cites the v4 SCSS line it derives from. Insert this resource
/// with `app.init_resource::<OverlayStyle>()` or let [`OverlayStylePlugin`]
/// do it automatically at startup.
#[derive(Resource, Clone, Copy, Debug)]
pub struct OverlayStyle {
    /// Panel background tint.
    ///
    /// v4 SCSS: `rgba(0,0,0,0.8)` (alpha 204) per `overlayPanel.scss:5`.
    ///
    /// Tuned to alpha 160 (~0.63) so the frosted-glass blur is visually
    /// perceptible behind the tint. v4's CSS `backdrop-filter: blur(12px)` is
    /// clearly visible at 0.8 alpha because browser compositing lifts the
    /// apparent brightness — Bevy's `Rgba8UnormSrgb` compositing does not
    /// apply that lift, so 0.8 alpha makes the blur invisible in practice.
    /// 160 is the midpoint between v4's 204 and an aggressive 128; approved
    /// deviation from v4 for visual parity of intent.
    pub panel_fill: egui::Color32,
    /// Panel hairline border.
    /// v4 SCSS: `rgba(255,255,255,0.08)` (alpha 20/255, `overlayPanel.scss:13`).
    /// Bumped to alpha 60 (≈ 0.24) so the stroke catches light against the dark
    /// blurred backdrop — same treatment applied to `button_stroke` (20 → 76).
    /// Approved deviation from v4 SCSS; intent matches, literal value differs.
    pub panel_stroke: egui::Color32,
    /// Panel corner radius `10px` per `overlayPanel.scss:7`.
    pub panel_corner_radius: u8,
    /// Button background when not hovered, ≈ `rgba(0,0,0,0.4)` per `overlayButton.scss:9`.
    pub button_fill_inactive: egui::Color32,
    /// Button background when hovered, ≈ `rgba(0,0,0,0.6)` per `overlayButton.scss:18`.
    pub button_fill_hovered: egui::Color32,
    /// Button hairline border.
    /// v4 SCSS: `rgba(255,255,255,0.15)` (alpha 38/255, overlayButton.scss:10).
    /// Bumped to alpha 76 (≈ 0.30) so the stroke is perceptible in egui's
    /// render pipeline — egui composites on a dark background and the v4 CSS
    /// value is genuinely too faint to see at 1 px (approved deviation from v4
    /// SCSS; intent matches, literal value differs).
    pub button_stroke: egui::Color32,
    /// Button corner radius `6px` per `overlayButton.scss:11`.
    pub button_corner_radius: u8,
    /// Fine-pointer button size `32×32` per `overlayButton.scss:5–6`.
    pub button_size_fine: f32,
    /// Coarse-pointer button size `44×44` per `overlayButton.scss:23–24`.
    pub button_size_coarse: f32,
    /// Dim chrome text colour, ≈ v4 `$gray3` / `$gray4`. Palette value for
    /// callers that draw text labels; not applied to the global egui `Style`
    /// (which uses `text_color_bright` via `override_text_color`).
    pub text_color_dim: egui::Color32,
    /// Bright chrome text colour (white labels, hover state).
    pub text_color_bright: egui::Color32,
    /// Sketch-name font size in the picker tiles, derived from v4 Orbitron
    /// sizing. Palette value applied by `picker.rs` (Task 16) when rendering
    /// each sketch tile; not used by the global egui style.
    pub picker_tile_name_size: f32,

    // ── Settings/debug dock palette (2026-06 UI consult) ────────────────
    //
    // These drive the consolidated tabbed settings dock and the diagnostics
    // window. They are additive: the existing button/picker/panel chrome above
    // is unchanged, and `apply_overlay_style` does not yet consume them (the
    // global `override_text_color` removal the consult calls for is a separate,
    // capture-verified step). The dock applies these via scoped `Visuals`
    // overrides so the rest of the overlay keeps its current look until the
    // redesign lands wholesale.
    /// Dock accent: a desaturated cyan-steel chosen to sit opposite the Line
    /// artwork's warm white/amber particles on the wheel while staying well off
    /// the saturated blue/red chromatic fringe — so the chrome reads as a
    /// separate, quieter material rather than part of the piece.
    pub accent: egui::Color32,
    /// Brighter accent for hover/active (tab underline, selected toggle).
    pub accent_bright: egui::Color32,
    /// Translucent accent for fills (selection background, slider trailing fill).
    pub accent_weak: egui::Color32,
    /// Primary dock text (labels, values): near-white `gray(235)`.
    pub text_primary: egui::Color32,
    /// Secondary dock text (section headers, status words): `gray(160)`.
    pub text_secondary: egui::Color32,
    /// Faint dock text (footer note, disabled): `gray(120)`.
    pub text_faint: egui::Color32,
    /// In-panel hairline (section rules, footer divider): `white_alpha(18)`,
    /// much quieter than the outer `panel_stroke`.
    pub hairline: egui::Color32,
    /// Dock background tint with the backdrop blur on: `black_alpha(178)`
    /// (~0.70), slightly heavier than `panel_fill` since the dock is a
    /// long-form reading surface.
    pub dock_fill: egui::Color32,
    /// Dock background tint with the backdrop blur off: `black_alpha(216)`
    /// (~0.85) — without the Kawase pass the tint alone must carry legibility
    /// when bright particles pass underneath.
    pub dock_fill_no_blur: egui::Color32,
    /// Status amber (restart-pending badge, "starting…" dot): `F39C12`.
    pub warn_amber: egui::Color32,
    /// Status red (error notes): `E56E6E`, softened from the LED red so it
    /// does not glare against the glass.
    pub error_red: egui::Color32,
    /// Status green (connected dot, ready): `2ECC71`.
    pub ok_green: egui::Color32,
}

impl Default for OverlayStyle {
    fn default() -> Self {
        Self {
            // Alpha 160 (~0.63): tuned down from v4's 204 (0.8) so the
            // frosted-glass blur is visible behind the tint. See field doc.
            panel_fill: egui::Color32::from_black_alpha(160),
            // Brightened from v4's literal 20 (0.08 alpha) to 60 (≈ 0.24) so the
            // border is visible against the dark blurred backdrop — same approach
            // taken for button_stroke (38 → 76). Approved deviation from v4 SCSS.
            panel_stroke: egui::Color32::from_white_alpha(60),
            panel_corner_radius: 10,
            button_fill_inactive: egui::Color32::from_black_alpha(102),
            button_fill_hovered: egui::Color32::from_black_alpha(153),
            // note: brighter than v4 SCSS (alpha 38) to be visible against egui's render.
            // v4 SCSS rgba(255,255,255,0.15) = alpha 38; bumped to 76 (≈ 0.30) here.
            button_stroke: egui::Color32::from_white_alpha(76),
            button_corner_radius: 6,
            button_size_fine: 32.0,
            button_size_coarse: 44.0,
            text_color_dim: egui::Color32::from_gray(140),
            text_color_bright: egui::Color32::WHITE,
            picker_tile_name_size: 40.0,

            // Dock palette — exact values from the UI consult.
            accent: egui::Color32::from_rgb(0x7A, 0xB8, 0xC8),
            accent_bright: egui::Color32::from_rgb(0x9E, 0xD2, 0xDE),
            accent_weak: egui::Color32::from_rgba_unmultiplied(0x7A, 0xB8, 0xC8, 64),
            text_primary: egui::Color32::from_gray(235),
            text_secondary: egui::Color32::from_gray(160),
            text_faint: egui::Color32::from_gray(120),
            hairline: egui::Color32::from_white_alpha(18),
            dock_fill: egui::Color32::from_black_alpha(178),
            dock_fill_no_blur: egui::Color32::from_black_alpha(216),
            warn_amber: egui::Color32::from_rgb(0xF3, 0x9C, 0x12),
            error_red: egui::Color32::from_rgb(0xE5, 0x6E, 0x6E),
            ok_green: egui::Color32::from_rgb(0x2E, 0xCC, 0x71),
        }
    }
}

/// Plugin: inserts [`OverlayStyle`] and applies the egui `Style` /
/// `FontDefinitions` on the first `Update` frame where the egui context is
/// ready. In Bevy 0.18 + `bevy_egui` 0.39, the egui primary context is created
/// lazily (typically not until the first rendered frame), so `PostStartup` is
/// too early — `ctx_mut()` returns `Err` and the font definitions are never
/// installed, causing a panic when the picker tries to use `FontFamily::Name("orbitron")`.
pub struct OverlayStylePlugin;

impl Plugin for OverlayStylePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OverlayStyle>();
        app.add_systems(Update, apply_overlay_style);
    }
}

/// Configure the egui context: load Inter / Fira Code / Orbitron fonts and
/// apply the dark visuals derived from [`OverlayStyle`].
///
/// Runs every `Update` tick but only does work on the first frame where
/// `ctx_mut()` succeeds. The `applied` local guard prevents redundant
/// `set_fonts` / `set_visuals` calls on subsequent frames.
pub(super) fn apply_overlay_style(
    mut contexts: bevy_egui::EguiContexts<'_, '_>,
    style: Res<'_, OverlayStyle>,
    mut applied: Local<'_, bool>,
) {
    if *applied {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    // Fonts — embed as compile-time statics so no asset-server round-trip is
    // needed before the first frame.
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "inter".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../../../assets/fonts/Inter-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        "fira_code".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../../../assets/fonts/FiraCode-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        "orbitron".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../../../assets/fonts/Orbitron-Bold.ttf"
        ))),
    );
    // Prepend Inter as the first Proportional family (egui's built-in falls back
    // after all entries in the list, so prepending gives Inter priority).
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "inter".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "fira_code".to_owned());
    // Named family for sketch titles — callers reference it as
    // `FontFamily::Name("orbitron".into())`. Inter is appended as a fallback so the
    // family has a replacement glyph (see the phosphor family below for why).
    fonts.families.insert(
        egui::FontFamily::Name("orbitron".into()),
        vec!["orbitron".to_owned(), "inter".to_owned()],
    );
    // Register Phosphor icon font so PUA glyphs (HOUSE, GEAR, SPEAKER_HIGH, etc.)
    // resolve correctly.
    //
    // `add_to_fonts` inserts phosphor data and places "phosphor" at position 1
    // in the Proportional family. However Inter-Regular.ttf maps several PUA
    // codepoints (U+E1E2–E2C7) that clash with Phosphor icons including HOUSE
    // and GEAR — egui picks the first font in the list that covers a codepoint,
    // so Inter wins and renders the wrong glyph.
    //
    // Fix: ALSO register "phosphor" as a standalone named family so callers that
    // need icon glyphs (overlay buttons, picker play icon) can reference it
    // explicitly via `FontFamily::Name("phosphor".into())`. The Proportional
    // fallback chain entry added by `add_to_fonts` is kept for any future
    // non-icon callers that happen to use PUA codepoints outside the Inter clash
    // range, but critical icon sites use the named family directly.
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    // Phosphor first (so PUA icon codepoints resolve to the icon font), Inter
    // appended as a fallback. epaint derives each family's replacement glyph from
    // '◻' then '?' when the family is first used; the icon font has neither, so a
    // phosphor-only family logs "Failed to find replacement characters '◻' or '?'.
    // Will use empty glyph." the first time it renders (e.g. the Home overlay
    // buttons). Inter is consulted only when Phosphor lacks a codepoint, so it
    // supplies '?' for the replacement glyph while real icons still win.
    fonts.families.insert(
        egui::FontFamily::Name("phosphor".into()),
        vec!["phosphor".to_owned(), "inter".to_owned()],
    );
    ctx.set_fonts(fonts);

    // Visuals — start from dark, override key fields to match v4's SCSS.
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = style.panel_fill;
    visuals.window_fill = style.panel_fill;
    visuals.window_stroke = egui::Stroke::new(1.0, style.panel_stroke);
    visuals.window_corner_radius = egui::CornerRadius::same(style.panel_corner_radius);
    visuals.widgets.inactive.weak_bg_fill = style.button_fill_inactive;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, style.button_stroke);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(style.button_corner_radius);
    visuals.widgets.hovered.weak_bg_fill = style.button_fill_hovered;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, style.button_stroke);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(style.button_corner_radius);
    visuals.widgets.active.weak_bg_fill = style.button_fill_hovered;
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(style.button_corner_radius);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, style.button_stroke);
    visuals.override_text_color = Some(style.text_color_bright);

    ctx.set_visuals(visuals);
    *applied = true;
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "comparing exact integer-representable constants set by assignment, not by arithmetic"
)]
mod tests {
    use super::*;

    #[test]
    fn overlay_style_defaults_match_v4_scss() {
        // These assertions intentionally hardcode the v4 SCSS values; if a
        // future re-tune of v4's stylesheet drifts, this test catches it.
        let style = OverlayStyle::default();
        // overlayPanel.scss:5 rgba(0,0,0,0.8) → 204/255 alpha in v4.
        // WaveConductor uses 160 (~0.63 alpha) so the frosted-glass blur
        // is visible — approved deviation from v4 (see OverlayStyle::panel_fill doc).
        assert_eq!(style.panel_fill, egui::Color32::from_black_alpha(160));
        // overlayPanel.scss:13 rgba(255,255,255,0.08) → ~20/255 in v4.
        // WaveConductor uses 60 (≈ 0.24 alpha) so the border catches light against
        // the dark blurred backdrop — approved deviation from v4 (see field doc).
        assert_eq!(style.panel_stroke, egui::Color32::from_white_alpha(60));
        // overlayPanel.scss:7 border-radius 10px
        assert_eq!(style.panel_corner_radius, 10);
        // overlayButton.scss:9 rgba(0,0,0,0.4) → ~102/255
        assert_eq!(
            style.button_fill_inactive,
            egui::Color32::from_black_alpha(102)
        );
        // overlayButton.scss:18 rgba(0,0,0,0.6) → ~153/255
        assert_eq!(
            style.button_fill_hovered,
            egui::Color32::from_black_alpha(153)
        );
        // overlayButton.scss:5–6 width/height 32px
        assert_eq!(style.button_size_fine, 32.0);
        // overlayButton.scss:23–24 @media (pointer: coarse) → 44px
        assert_eq!(style.button_size_coarse, 44.0);
        // button_stroke: intentional deviation from v4's alpha 38 → bumped to 76
        // so the border is perceptible in egui's render pipeline (see style.rs doc).
        assert_eq!(style.button_stroke, egui::Color32::from_white_alpha(76));
    }

    /// Pin the dock palette to the UI consult's exact values — these drive the
    /// settings/debug dock and a drift here would silently re-tint the chrome.
    #[test]
    fn overlay_style_dock_palette_matches_the_consult() {
        let style = OverlayStyle::default();
        assert_eq!(style.accent, egui::Color32::from_rgb(0x7A, 0xB8, 0xC8));
        assert_eq!(
            style.accent_bright,
            egui::Color32::from_rgb(0x9E, 0xD2, 0xDE)
        );
        assert_eq!(
            style.accent_weak,
            egui::Color32::from_rgba_unmultiplied(0x7A, 0xB8, 0xC8, 64)
        );
        assert_eq!(style.text_primary, egui::Color32::from_gray(235));
        assert_eq!(style.text_secondary, egui::Color32::from_gray(160));
        assert_eq!(style.text_faint, egui::Color32::from_gray(120));
        assert_eq!(style.hairline, egui::Color32::from_white_alpha(18));
        assert_eq!(style.dock_fill, egui::Color32::from_black_alpha(178));
        assert_eq!(
            style.dock_fill_no_blur,
            egui::Color32::from_black_alpha(216)
        );
        assert_eq!(style.warn_amber, egui::Color32::from_rgb(0xF3, 0x9C, 0x12));
        assert_eq!(style.error_red, egui::Color32::from_rgb(0xE5, 0x6E, 0x6E));
        assert_eq!(style.ok_green, egui::Color32::from_rgb(0x2E, 0xCC, 0x71));
    }
}
