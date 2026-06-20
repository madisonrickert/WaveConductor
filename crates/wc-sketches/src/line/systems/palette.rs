//! Live palette-uniform driver.
//!
//! Maps the `LineSettings` palette knobs onto [`LineMaterial`]`::palette_params`
//! (`x` = mode index, `y` = strength, `z` = cycle, `w` = spread). Compares
//! against the material's current value rather than a `Local` cache, so a
//! freshly respawned material (seeded [`LineMaterial::palette_off`]) picks up the
//! correct target on the next frame without any user interaction. Dragging an
//! unrelated slider or sitting idle costs one float compare per frame with no
//! asset re-upload. The palette's time animation reads `globals.time` in the
//! shader, so cycling does NOT churn this uniform.

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
/// Compares against the material's current `palette_params` rather than a `Local`
/// cache: mutating a `LineMaterial` re-prepares its bind group, so the write
/// happens only when the packed value actually differs from what is already in the
/// asset (a settings edit or a fresh respawn). A freshly respawned material is
/// seeded [`LineMaterial::palette_off`] (`Vec4::ZERO`); when the user has an
/// enabled palette that seed differs from the live target, so the system rewrites
/// on the very next frame — no user interaction required. A frame where the asset
/// isn't loaded yet (`get` → `None`) is silently skipped and retried next frame.
pub fn drive_palette(
    settings: Res<'_, LineSettings>,
    roots: Query<'_, '_, &MeshMaterial2d<LineMaterial>, With<LineRoot>>,
    mut materials: ResMut<'_, Assets<LineMaterial>>,
) {
    let target = palette_params(
        settings.palette_mode,
        settings.palette_strength,
        settings.palette_cycle,
        settings.palette_scale,
    );
    for handle in &roots {
        // Compare against the material's CURRENT value (not a Local cache): a
        // freshly respawned material (OnEnter seeds palette_off()) differs from a
        // non-Off target and self-corrects on the next frame. Read-only `get`
        // until it differs, so no asset churn in the settled state; an
        // unloaded asset (`get` -> None) is skipped and retried next frame.
        let differs = materials
            .get(&handle.0)
            .is_some_and(|m| m.palette_params != target);
        if differs {
            if let Some(material) = materials.get_mut(&handle.0) {
                material.palette_params = target;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::prelude::Assets;

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

    /// Regression: after a material respawn (seeded `palette_off()`), `drive_palette`
    /// must rewrite `palette_params` to the live target on the next frame even though
    /// the settings have not changed. The old `Local<Option<Vec4>>` pattern held the
    /// previous target, short-circuited the loop, and silently left the fresh
    /// material at `Vec4::ZERO` — palette invisible — until the user nudged a knob.
    #[test]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test-only: panics are acceptable failures"
    )]
    fn drive_palette_self_corrects_on_material_respawn() {
        // LineSettings::default() uses palette_strength=0.8, palette_cycle=0.03,
        // palette_scale=1.0 (verified by settings.rs tests). Override palette_mode
        // to Velocity so the target is non-Off.
        let mut world = World::new();
        let settings = LineSettings {
            palette_mode: PaletteMode::Velocity,
            ..LineSettings::default()
        };
        let expected = palette_params(
            PaletteMode::Velocity,
            settings.palette_strength,
            settings.palette_cycle,
            settings.palette_scale,
        );
        world.insert_resource(settings);

        // Build a LineMaterial seeded at palette_off() — exactly what spawn_line does.
        let mut assets = Assets::<LineMaterial>::default();
        let material = LineMaterial {
            particles: Handle::default(),
            star_texture: Handle::default(),
            solid_color: LineMaterial::solid_off(),
            attract_color: LineMaterial::attract_color_off(),
            template_color: LineMaterial::template_color_off(),
            palette_params: LineMaterial::palette_off(),
        };
        let handle = assets.add(material);
        world.insert_resource(assets);

        // Spawn the root entity — same shape as the live scene after spawn_line.
        world.spawn((LineRoot, MeshMaterial2d(handle.clone())));

        // First run: fresh material (palette_off) should be rewritten to `expected`.
        world
            .run_system_once(drive_palette)
            .expect("drive_palette system run");
        assert_eq!(
            world
                .resource::<Assets<LineMaterial>>()
                .get(&handle)
                .unwrap()
                .palette_params,
            expected,
            "first run: fresh material should be written to Velocity target",
        );

        // Simulate a material respawn: reset palette_params back to palette_off(),
        // as if OnEnter(AppState::Line) re-seeded the material. Settings unchanged.
        world
            .resource_mut::<Assets<LineMaterial>>()
            .get_mut(&handle)
            .unwrap()
            .palette_params = LineMaterial::palette_off();

        // Second run: must re-sync even though settings did not change.
        // This is the assertion that FAILS under the old Local<Option<Vec4>> code.
        world
            .run_system_once(drive_palette)
            .expect("drive_palette system run (respawn)");
        assert_eq!(
            world
                .resource::<Assets<LineMaterial>>()
                .get(&handle)
                .unwrap()
                .palette_params,
            expected,
            "respawn: palette must re-sync to Velocity target, not stay at ZERO",
        );
    }
}
