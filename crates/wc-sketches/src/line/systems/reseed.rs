//! In-place re-seed of the particle buffer when the active template's
//! position-shaping adjustments change.
//!
//! The six position knobs (white/black point, gamma, invert, position, scale)
//! reshape *where* particles spawn, so a change must re-run the sampler and
//! re-upload the particle storage buffer. Unlike a `requires_restart` setting
//! (which triggers the full fade → Home → fade reload), this re-seeds **in
//! place** via `Assets::get_mut`, so particles redistribute live without a state
//! round-trip. Colour influence is excluded — it is a live render uniform
//! (`drive_color_influence`), so changing it never re-seeds.
//!
//! Changes are debounced so a slider drag coalesces into one re-seed when it
//! settles; the sampler's fixed seed keeps the re-distribution continuous.

#![cfg(feature = "templates")]
#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "particle_count u32↔usize and index usize↔u32 casts are bounded"
)]
#![allow(
    clippy::float_cmp,
    reason = "exact inequality is intended: any bit difference is a real edit to re-seed on"
)]

use std::path::Path;
use std::time::Duration;

use bevy::prelude::*;
use bevy::render::storage::ShaderStorageBuffer;
use bevy::sprite_render::MeshMaterial2d;
use bytemuck::cast_slice;

use crate::line::compute::LineSimParams;
use crate::line::heatmap::sample_from_heatmap;
use crate::line::material::LineMaterial;
use crate::line::particle::Particle;
use crate::line::settings::LineSettings;
use crate::line::systems::spawn::make_particle;
use crate::line::template_adjustments::{pack_rgb8, TemplateAdjustments};
use crate::line::template_adjustments_store::{hash_of_path_str, LineTemplateAdjustments};
use crate::line::LineRoot;

/// Quiescence window before a re-seed fires (so a slider drag coalesces).
const RESEED_DEBOUNCE: Duration = Duration::from_millis(200);

/// Per-system state for the re-seed debounce: the last-seeded position-shaping
/// snapshot and the pending-fire timestamp.
#[derive(Default)]
pub struct ReseedState {
    last: Option<TemplateAdjustments>,
    debounce: Option<Duration>,
}

/// Whether any *position-shaping* field differs (everything except
/// `color_influence`, which is a live uniform and must not trigger a re-seed).
#[must_use]
pub fn position_fields_changed(a: &TemplateAdjustments, b: &TemplateAdjustments) -> bool {
    a.white_point != b.white_point
        || a.black_point != b.black_point
        || a.invert != b.invert
        || a.gamma != b.gamma
        || a.position != b.position
        || a.scale != b.scale
}

/// Debounced in-place re-seed when the active template's position knobs change.
#[allow(
    clippy::too_many_arguments,
    reason = "Bevy system: adjustments, settings, sim, window, buffers, materials, roots, time, state"
)]
pub fn reseed_on_adjustments_change(
    adjustments: Res<'_, LineTemplateAdjustments>,
    settings: Res<'_, LineSettings>,
    sim: Option<Res<'_, LineSimParams>>,
    window: Single<'_, '_, &Window>,
    mut buffers: ResMut<'_, Assets<ShaderStorageBuffer>>,
    // The render buffer is *recreated* on re-upload (Bevy's ShaderStorageBuffer
    // prepare does `create_buffer_with_data`), which invalidates the material's
    // cached bind group; touch the material so it rebinds to the new buffer.
    roots: Query<'_, '_, &MeshMaterial2d<LineMaterial>, With<LineRoot>>,
    mut materials: ResMut<'_, Assets<LineMaterial>>,
    time: Res<'_, Time>,
    mut state: Local<'_, ReseedState>,
) {
    // No buffer yet (not spawned), or no active template: nothing to re-seed.
    let Some(sim) = sim else {
        return;
    };
    // Borrowed stem (no per-frame allocation, per the no-hot-path-allocation
    // rule); `get` takes `&str` and returns a stack-only clone.
    let Some(hash) = hash_of_path_str(&settings.spawn_template) else {
        state.last = None;
        state.debounce = None;
        return;
    };
    let adj = adjustments.get(hash);

    match state.last.clone() {
        // First observation: snapshot the spawn state without re-seeding (spawn
        // already used these values).
        None => state.last = Some(adj.clone()),
        Some(prev) => {
            if position_fields_changed(&prev, &adj) {
                state.debounce = Some(time.elapsed());
                state.last = Some(adj.clone());
            }
        }
    }

    // Fire once the debounce window has elapsed with no further change.
    let Some(stamp) = state.debounce else {
        return;
    };
    if time.elapsed().saturating_sub(stamp) < RESEED_DEBOUNCE {
        return;
    }
    state.debounce = None;

    let w = window.width();
    let win_h = window.height();
    let half_w = w * 0.5;
    let half_h = win_h * 0.5;
    let count = sim.particle_count as usize;
    let sampled = sample_from_heatmap(Path::new(&settings.spawn_template), w, win_h, count, &adj);
    let particles: Vec<Particle> = sampled
        .into_iter()
        .enumerate()
        .map(|(i, sp)| {
            let x = sp.pos.x - half_w;
            let y = -(sp.pos.y - half_h);
            let mut p = make_particle(i as u32, x, y, pack_rgb8(sp.color));
            // Immediately visible (vs the spawn's fade-in-from-0): a live preview
            // should show the new layout at once, not flash invisible each tweak.
            p.alpha = 1.0;
            p
        })
        .collect();

    // Re-upload: setting the asset's bytes via `get_mut` marks it changed, so the
    // render world re-extracts and re-creates the GPU buffer.
    if buffers.get_mut(&sim.particles_handle).is_some_and(|buf| {
        buf.data = Some(cast_slice::<Particle, u8>(&particles).to_vec());
        true
    }) {
        // The buffer was recreated (new GpuBuffer), so the material's cached bind
        // group now points at the freed buffer. Touch the material to force a
        // bind-group rebind — without this the render stays frozen on the stale
        // buffer until something else marks the material changed.
        for handle in &roots {
            let _ = materials.get_mut(&handle.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_color_influence_change_is_not_a_position_change() {
        let a = TemplateAdjustments::default();
        let b = TemplateAdjustments {
            color_influence: 0.9,
            ..Default::default()
        };
        assert!(!position_fields_changed(&a, &b));
    }

    #[test]
    fn position_shaping_changes_are_detected() {
        let a = TemplateAdjustments::default();
        for b in [
            TemplateAdjustments {
                gamma: 2.0,
                ..Default::default()
            },
            TemplateAdjustments {
                invert: true,
                ..Default::default()
            },
            TemplateAdjustments {
                position: [0.1, 0.0],
                ..Default::default()
            },
            TemplateAdjustments {
                scale: [2.0, 1.0],
                ..Default::default()
            },
        ] {
            assert!(position_fields_changed(&a, &b));
        }
    }
}
