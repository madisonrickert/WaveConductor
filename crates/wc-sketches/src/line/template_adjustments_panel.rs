//! The "Template adjustments" custom dock section for the Line sketch.
//!
//! Registered via `register_dock_section("line", …)` so it renders in the
//! settings dock immediately below the Line settings (under the template
//! picker), scoped to the **active** template. It draws a three-column grid that
//! mirrors the main settings: an accent-coloured label when a knob differs from
//! its default, a draggable normalized-percentage slider (or the invert
//! checkbox), and a reset-to-default glyph that appears only when modified.
//! Edits write back into the registered [`LineTemplateAdjustments`] resource —
//! which persists them and arms the in-place re-seed / colour-influence drivers.
//!
//! Edits are written only when a widget actually changes, so merely opening the
//! dock does not mark the resource dirty every frame.

use std::ops::RangeInclusive;

use bevy::prelude::*;
use bevy_egui::egui;
use egui_phosphor::regular as phosphor;
use wc_core::ui::OverlayStyle;

use crate::line::settings::LineSettings;
use crate::line::template_adjustments::TemplateAdjustments;
use crate::line::template_adjustments_store::{hash_of_path, LineTemplateAdjustments};

/// Displayed-percent → stored internal value (e.g. 100% → 1.0).
#[must_use]
pub fn pct_to_internal(pct: f32) -> f32 {
    pct / 100.0
}

/// Stored internal value → displayed percent (e.g. 1.0 → 100%).
#[must_use]
pub fn internal_to_pct(internal: f32) -> f32 {
    internal * 100.0
}

/// The custom dock-section renderer (a `DockSectionFn`). Renders nothing when no
/// template is active; otherwise draws the active image's adjustment grid.
pub fn render_template_adjustments(world: &mut World, ui: &mut egui::Ui, style: &OverlayStyle) {
    let Some(spawn) = world
        .get_resource::<LineSettings>()
        .map(|s| s.spawn_template.clone())
    else {
        return;
    };
    let Some(hash) = hash_of_path(&spawn) else {
        return;
    };
    if world.get_resource::<LineTemplateAdjustments>().is_none() {
        return;
    }

    // Read the active image's current adjustments (default if unsaved), edit a
    // local copy, then write back only if something changed — so opening the
    // dock does not dirty the resource (and arm autosave/re-seed) every frame.
    let current = world.resource::<LineTemplateAdjustments>().get(&hash);
    let mut edited = current.clone();
    let mut changed = false;

    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("TEMPLATE ADJUSTMENTS")
            .size(11.0)
            .strong(),
    );
    ui.add_space(4.0);

    let d = TemplateAdjustments::default();
    // Three columns (label / widget / reset) with the same spacing as the main
    // settings grid, so the adjustments read as part of the same panel.
    egui::Grid::new("line-template-adjustments")
        .num_columns(3)
        .spacing(egui::vec2(12.0, 8.0))
        .show(ui, |ui| {
            changed |= pct_knob_row(
                ui,
                "White point",
                &mut edited.white_point,
                d.white_point,
                0.0..=100.0,
                style,
            );
            changed |= pct_knob_row(
                ui,
                "Black point",
                &mut edited.black_point,
                d.black_point,
                0.0..=100.0,
                style,
            );
            changed |= pct_knob_row(ui, "Gamma", &mut edited.gamma, d.gamma, 10.0..=400.0, style);
            changed |= invert_knob_row(ui, &mut edited.invert, d.invert, style);
            // Position and scale have independent X and Y knobs.
            changed |= pct_knob_row(
                ui,
                "Position X",
                &mut edited.position[0],
                d.position[0],
                -100.0..=100.0,
                style,
            );
            changed |= pct_knob_row(
                ui,
                "Position Y",
                &mut edited.position[1],
                d.position[1],
                -100.0..=100.0,
                style,
            );
            changed |= pct_knob_row(
                ui,
                "Scale X",
                &mut edited.scale[0],
                d.scale[0],
                10.0..=400.0,
                style,
            );
            changed |= pct_knob_row(
                ui,
                "Scale Y",
                &mut edited.scale[1],
                d.scale[1],
                10.0..=400.0,
                style,
            );
            changed |= pct_knob_row(
                ui,
                "Color influence",
                &mut edited.color_influence,
                d.color_influence,
                0.0..=100.0,
                style,
            );
        });

    if changed {
        world
            .resource_mut::<LineTemplateAdjustments>()
            .entry_mut(&hash)
            .clone_from(&edited);
    }
}

/// One numeric knob row: an accent-coloured label when modified, a draggable
/// normalized-percentage slider, and a reset-to-default glyph (only when
/// modified). Shows `internal * 100` as a `%`; on change writes `pct / 100` back.
/// Returns whether the value changed.
fn pct_knob_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    default: f32,
    pct_range: RangeInclusive<f32>,
    style: &OverlayStyle,
) -> bool {
    let modified = (*value - default).abs() > f32::EPSILON;
    knob_label(ui, label, modified, style);

    let mut changed = false;
    let mut pct = internal_to_pct(*value);
    if ui
        .add(
            egui::Slider::new(&mut pct, pct_range)
                .suffix("%")
                .fixed_decimals(0),
        )
        .changed()
    {
        *value = pct_to_internal(pct);
        changed = true;
    }

    if reset_cell(ui, modified, style) {
        *value = default;
        changed = true;
    }
    ui.end_row();
    changed
}

/// The invert checkbox row, with the same modified-label + reset treatment.
fn invert_knob_row(
    ui: &mut egui::Ui,
    value: &mut bool,
    default: bool,
    style: &OverlayStyle,
) -> bool {
    let modified = *value != default;
    knob_label(ui, "Invert", modified, style);
    let mut changed = ui.checkbox(value, "").changed();
    if reset_cell(ui, modified, style) {
        *value = default;
        changed = true;
    }
    ui.end_row();
    changed
}

/// Grid column 1: the knob label, accent-coloured when it differs from default
/// (matching the main settings panel's modified-field highlight).
fn knob_label(ui: &mut egui::Ui, label: &str, modified: bool, style: &OverlayStyle) {
    let color = if modified {
        style.accent_bright
    } else {
        style.text_primary
    };
    ui.label(egui::RichText::new(label).color(color));
}

/// Grid column 3: a frameless reset-to-default glyph when modified, else an
/// aligned spacer (so the column width stays stable; `add_space` panics inside a
/// Grid). Returns true when the reset glyph is clicked.
fn reset_cell(ui: &mut egui::Ui, modified: bool, style: &OverlayStyle) -> bool {
    if !modified {
        ui.allocate_exact_size(egui::vec2(18.0, 0.0), egui::Sense::hover());
        return false;
    }
    let glyph = egui::RichText::new(phosphor::ARROW_COUNTER_CLOCKWISE)
        .family(egui::FontFamily::Name("phosphor".into()))
        .size(12.0)
        .color(style.text_secondary);
    ui.add(egui::Button::new(glyph).frame(false))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Reset to default")
        .clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_round_trips() {
        assert!((pct_to_internal(100.0) - 1.0).abs() < 1e-6);
        assert!((internal_to_pct(1.0) - 100.0).abs() < 1e-6);
        assert!((pct_to_internal(0.0) - 0.0).abs() < 1e-6);
        assert!((pct_to_internal(-50.0) - -0.5).abs() < 1e-6);
        assert!((pct_to_internal(400.0) - 4.0).abs() < 1e-6);
    }

    #[test]
    fn defaults_show_clean_percentages() {
        // The identity defaults read as a clean baseline on the sliders.
        let a = TemplateAdjustments::default();
        assert!((internal_to_pct(a.white_point) - 100.0).abs() < 1e-6);
        assert!((internal_to_pct(a.black_point) - 0.0).abs() < 1e-6);
        assert!((internal_to_pct(a.gamma) - 100.0).abs() < 1e-6);
        assert!((internal_to_pct(a.scale[0]) - 100.0).abs() < 1e-6);
        assert!((internal_to_pct(a.position[0]) - 0.0).abs() < 1e-6);
        assert!((internal_to_pct(a.color_influence) - 0.0).abs() < 1e-6);
    }
}
