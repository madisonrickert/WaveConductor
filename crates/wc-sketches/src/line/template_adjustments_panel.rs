//! The "Template adjustments" custom dock section for the Line sketch.
//!
//! Registered via `register_dock_section("line", …)` so it renders in the
//! settings dock immediately below the Line settings (under the template
//! picker), scoped to the **active** template. It draws draggable
//! normalized-percentage sliders for the per-image knobs and writes edits back
//! into the registered [`LineTemplateAdjustments`] resource — which is what
//! persists them and arms the in-place re-seed / colour-influence drivers.
//!
//! Edits are written only when a widget actually changes, so merely opening the
//! dock does not mark the resource dirty every frame.

use std::ops::RangeInclusive;

use bevy::prelude::*;
use bevy_egui::egui;
use wc_core::ui::OverlayStyle;

use crate::line::settings::LineSettings;
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
/// template is active; otherwise draws the active image's adjustment sliders.
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

    changed |= pct_slider(
        ui,
        "White point",
        &mut edited.white_point,
        0.0..=100.0,
        style,
    );
    changed |= pct_slider(
        ui,
        "Black point",
        &mut edited.black_point,
        0.0..=100.0,
        style,
    );
    changed |= pct_slider(ui, "Gamma", &mut edited.gamma, 10.0..=400.0, style);
    changed |= ui.checkbox(&mut edited.invert, "Invert").changed();
    // Position and scale have independent X and Y sliders.
    changed |= pct_slider(
        ui,
        "Position X",
        &mut edited.position[0],
        -100.0..=100.0,
        style,
    );
    changed |= pct_slider(
        ui,
        "Position Y",
        &mut edited.position[1],
        -100.0..=100.0,
        style,
    );
    changed |= pct_slider(ui, "Scale X", &mut edited.scale[0], 10.0..=400.0, style);
    changed |= pct_slider(ui, "Scale Y", &mut edited.scale[1], 10.0..=400.0, style);
    changed |= pct_slider(
        ui,
        "Color influence",
        &mut edited.color_influence,
        0.0..=100.0,
        style,
    );

    if changed {
        world
            .resource_mut::<LineTemplateAdjustments>()
            .entry_mut(&hash)
            .clone_from(&edited);
    }
}

/// A draggable normalized-percentage slider over a stored-internal `f32`. Shows
/// `internal * 100` as a `%` value across `pct_range`; on change, writes the new
/// percent back as `internal = pct / 100`. Returns whether it changed.
fn pct_slider(
    ui: &mut egui::Ui,
    label: &str,
    internal: &mut f32,
    pct_range: RangeInclusive<f32>,
    style: &OverlayStyle,
) -> bool {
    let mut pct = internal_to_pct(*internal);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).color(style.text_primary));
        if ui
            .add(
                egui::Slider::new(&mut pct, pct_range)
                    .suffix("%")
                    .fixed_decimals(0),
            )
            .changed()
        {
            *internal = pct_to_internal(pct);
            changed = true;
        }
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line::template_adjustments::TemplateAdjustments;

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
