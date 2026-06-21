//! Live colour-influence uniform driver.
//!
//! Writes the active template's `color_influence` (0..1) into the
//! [`LineMaterial`]`::template_color` uniform, so dragging the colour-influence
//! slider tints particles immediately with **no re-seed** (the per-particle
//! image colour is already baked into the buffer; only the blend strength
//! changes). It compares against the material's current value, so the asset is
//! marked changed (re-uploaded) only on an actual change — which also lets a
//! freshly respawned material (seeded "off") self-correct on the next frame.

#![cfg(feature = "templates")]

use bevy::prelude::*;
use bevy::sprite_render::MeshMaterial2d;

use crate::line::material::LineMaterial;
use crate::line::settings::LineSettings;
use crate::line::template_adjustments_store::LineTemplateAdjustments;
use crate::line::LineRoot;

/// The active template's colour influence (`0..1`), or `0.0` when no template is
/// active. Delegates to the allocation-free [`LineTemplateAdjustments::color_influence_for`]
/// since this runs every frame.
#[must_use]
pub fn influence_for(spawn_template: &str, store: &LineTemplateAdjustments) -> f32 {
    store.color_influence_for(spawn_template)
}

/// Sync the active colour influence into the `LineMaterial` uniform. Only writes
/// (and thus re-uploads) when it differs from the material's current value, so
/// there is no per-frame churn and a respawned material picks up the right value.
pub fn drive_color_influence(
    settings: Res<'_, LineSettings>,
    store: Res<'_, LineTemplateAdjustments>,
    roots: Query<'_, '_, &MeshMaterial2d<LineMaterial>, With<LineRoot>>,
    mut materials: ResMut<'_, Assets<LineMaterial>>,
) {
    let target = influence_for(&settings.spawn_template, &store);
    for handle in &roots {
        let differs = materials
            .get(&handle.0)
            .is_some_and(|m| (m.template_color.x - target).abs() > f32::EPSILON);
        if differs {
            if let Some(mut material) = materials.get_mut(&handle.0) {
                material.template_color = Vec4::new(target, 0.0, 0.0, 0.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line::template_adjustments::TemplateAdjustments;

    #[test]
    fn influence_zero_when_no_template() {
        assert!(influence_for("", &LineTemplateAdjustments::default()).abs() < 1e-6);
    }

    #[test]
    fn influence_reads_active_entry() {
        let mut store = LineTemplateAdjustments::default();
        store.map.insert(
            "h".into(),
            TemplateAdjustments {
                color_influence: 0.7,
                ..Default::default()
            },
        );
        assert!((influence_for("/x/h.png", &store) - 0.7).abs() < 1e-6);
    }
}
