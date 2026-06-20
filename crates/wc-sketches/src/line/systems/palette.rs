//! Live palette-uniform driver.
//!
//! Maps the `LineSettings` palette knobs onto [`LineMaterial`]`::palette_params`
//! (`x` = mode index, `y` = strength, `z` = cycle, `w` = spread). Change-gated:
//! the uniform is written only when the packed value differs from the material's
//! current value, so dragging an unrelated slider or sitting idle costs one
//! float compare per frame with no asset re-upload. The palette's time animation
//! reads `globals.time` in the shader, so cycling does NOT churn this uniform.

use bevy::prelude::*;
use bevy::sprite_render::MeshMaterial2d;

use crate::line::material::LineMaterial;
use crate::line::settings::{LineSettings, PaletteMode};
use crate::line::LineRoot;

/// Pack the palette settings into the `palette_params` uniform value
/// (`x` = mode index, `y` = strength, `z` = cycle, `w` = spread). `Off` returns
/// [`LineMaterial::palette_off`] (`Vec4::ZERO`) regardless of the other knobs, so
/// the shader's uniform-mode branch is skipped and color is the pre-palette path.
/// Strength is clamped to `0..=1`; cycle and spread are clamped non-negative so a
/// stray value can never invert the crossfade or run the phase backward.
#[must_use]
pub fn palette_params(mode: PaletteMode, strength: f32, cycle: f32, scale: f32) -> Vec4 {
    if mode == PaletteMode::Off {
        return LineMaterial::palette_off();
    }
    Vec4::new(
        mode.index(),
        strength.clamp(0.0, 1.0),
        cycle.max(0.0),
        scale.max(0.0),
    )
}

/// Drive [`LineMaterial::palette_params`] from the `LineSettings` palette knobs.
///
/// Runs while Line is active and while its screensaver shows (registered under
/// both gates in [`crate::line::LinePlugin`], like the attract-color driver) so
/// the palette applies live and in attract while keeping zero systems when idle.
/// Change-gated: mutating a `LineMaterial` re-prepares its bind group, so the
/// write happens only when the packed value actually moves (a settings edit).
/// `last` advances only on an actual write, so a frame where the material asset
/// isn't loaded yet retries instead of dropping the value.
pub fn drive_palette(
    settings: Res<'_, LineSettings>,
    roots: Query<'_, '_, &MeshMaterial2d<LineMaterial>, With<LineRoot>>,
    mut materials: ResMut<'_, Assets<LineMaterial>>,
    mut last: Local<'_, Option<Vec4>>,
) {
    let target = palette_params(
        settings.palette_mode,
        settings.palette_strength,
        settings.palette_cycle,
        settings.palette_scale,
    );
    if *last == Some(target) {
        return;
    }
    for handle in &roots {
        if let Some(material) = materials.get_mut(&handle.0) {
            material.palette_params = target;
            *last = Some(target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_mode_is_zero_regardless_of_knobs() {
        assert_eq!(palette_params(PaletteMode::Off, 0.8, 0.03, 1.0), Vec4::ZERO);
    }

    #[test]
    fn velocity_mode_packs_channels() {
        let p = palette_params(PaletteMode::Velocity, 0.8, 0.03, 1.5);
        assert!((p.x - 1.0).abs() < 1e-6, "mode index 1 for Velocity");
        assert!((p.y - 0.8).abs() < 1e-6);
        assert!((p.z - 0.03).abs() < 1e-6);
        assert!((p.w - 1.5).abs() < 1e-6);
    }

    #[test]
    fn scatter_mode_index_is_two() {
        let p = palette_params(PaletteMode::Scatter, 1.0, 0.0, 1.0);
        assert!((p.x - 2.0).abs() < 1e-6, "mode index 2 for Scatter");
    }

    #[test]
    fn out_of_range_inputs_clamp() {
        let p = palette_params(PaletteMode::Velocity, 5.0, -1.0, -2.0);
        assert!((p.y - 1.0).abs() < 1e-6, "strength clamps to 1");
        assert!(p.z.abs() < 1e-6, "cycle clamps to 0");
        assert!(p.w.abs() < 1e-6, "spread clamps to 0");
    }
}
