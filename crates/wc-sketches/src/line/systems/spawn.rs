//! `OnEnter(AppState::Line)` spawn system.
//!
//! Allocates the particle storage buffer with the initial particle layout
//! (horizontal-line default; heatmap-image sampler when
//! [`crate::line::settings::LineSettings::spawn_template`] is non-empty),
//! builds a flat quad mesh (`count × 6` vertices for the vertex-index-driven
//! render shader), spawns the render entity under [`LineRoot`], inserts
//! [`crate::line::compute::LineSimParams`] for the render world, and seeds
//! the CPU mirror with the same particle state for Plan 9's `ParticleStats`.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "u32 ↔ usize ↔ f32 casts for particle count and mesh vertex sizing are intentional"
)]

use std::path::Path;

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::mesh::PrimitiveTopology;
use bevy::prelude::*;
use bevy::render::storage::ShaderStorageBuffer;
use bytemuck::cast_slice;

use crate::line::compute::LineSimParams;
use crate::line::hash::{hash_to_unit, wang_hash};
use crate::line::heatmap::sample_from_heatmap;
use crate::line::material::LineMaterial;
use crate::line::particle::{Particle, SimParams};
use crate::line::settings::LineSettings;
use crate::line::sim_cpu::LineCpuMirror;

/// Marker component placed on every entity owned by the Line sketch.
///
/// `OnExit(AppState::Line)` despawns everything tagged with this marker
/// via [`wc_core::sketch::despawn_with`].
#[derive(Component)]
pub struct LineRoot;

/// Shortest attract-mode lifespan a particle can be seeded with, in seconds.
pub const ATTRACT_LIFESPAN_MIN_SECS: f32 = 20.0;

/// Longest attract-mode lifespan a particle can be seeded with, in seconds.
pub const ATTRACT_LIFESPAN_MAX_SECS: f32 = 45.0;

/// Salt XOR-ed into the index before hashing for [`attract_lifespan`], so the
/// lifespan stream is decorrelated from the [`spawn_hash01`] stream (otherwise
/// the fraction kill would preferentially cull one end of the lifespan range).
const LIFESPAN_HASH_SALT: u32 = 0x9E37_79B9;

/// Deterministic per-index hash in `0..=1`, seeded into
/// [`Particle::spawn_hash`] at spawn. The attract-mode fraction gate kills
/// particles with `spawn_hash >= attract_fraction`; hashing the index (rather
/// than comparing the index itself) makes the cull spatially uniform, because
/// the line layout assigns indices left-to-right across the window.
#[must_use]
pub fn spawn_hash01(index: u32) -> f32 {
    hash_to_unit(wang_hash(index))
}

/// Deterministic attract-mode lifespan for particle `index`, uniform in
/// [`ATTRACT_LIFESPAN_MIN_SECS`]..=[`ATTRACT_LIFESPAN_MAX_SECS`]. Seeded into
/// [`Particle::lifespan`] at spawn. Per-particle staggering means the attract
/// field self-heals continuously instead of respawning in visible waves.
#[must_use]
pub fn attract_lifespan(index: u32) -> f32 {
    let unit = hash_to_unit(wang_hash(index ^ LIFESPAN_HASH_SALT));
    ATTRACT_LIFESPAN_MIN_SECS + (ATTRACT_LIFESPAN_MAX_SECS - ATTRACT_LIFESPAN_MIN_SECS) * unit
}

/// Construct a [`Particle`] at world-space `(x, y)` with zero velocity, its
/// spawn anchor at the same point, the attract-mode lifetime/identity fields
/// seeded deterministically from `index`, and the packed `spawn_color`. Shared
/// by both spawn layouts and the in-place re-seed so the particle fields stay
/// in one place.
pub(crate) fn make_particle(index: u32, x: f32, y: f32, spawn_color: f32) -> Particle {
    Particle {
        position: [x, y],
        velocity: [0.0, 0.0],
        original_xy: [x, y],
        alpha: 0.0,
        age: 0.0,
        lifespan: attract_lifespan(index),
        spawn_hash: spawn_hash01(index),
        spawn_color,
        _pad: 0.0,
    }
}

/// `OnEnter(AppState::Line)`.
///
/// Allocates the particle storage buffer, constructs a flat quad mesh
/// (`count × 6` vertices for the vertex-index-driven render shader), spawns
/// the render entity under [`LineRoot`], inserts [`LineSimParams`] for the
/// render world to extract each frame, and seeds the CPU mirror with the
/// same particle state.
///
/// Particle layout depends on [`LineSettings::spawn_template`]:
///
/// - Empty (default) — horizontal-line layout at mid-Y with five-strand
///   sawtooth Y-jitter (the v5-line-sim baseline).
/// - Non-empty — PNG path passed to [`sample_from_heatmap`]; particle
///   density follows the image's luminance × alpha. A missing or
///   undecodable file falls back to the horizontal-line layout.
///
/// The particle count is derived from `settings.particle_density × window.width`
/// (v4 parity: `particleDensity = 10` per canvas-pixel of width yields ~12,800
/// particles at 1280px), clamped to `[100, 100_000]` so a sudden resize spike
/// does not catastrophically allocate.
#[allow(
    clippy::too_many_arguments,
    reason = "Bevy system: commands, settings, window, asset server, three asset \
              stores, plus the gated adjustments and debug-toggle params"
)]
pub fn spawn_line(
    mut commands: Commands<'_, '_>,
    settings: Res<'_, LineSettings>,
    window: Single<'_, '_, &Window>,
    asset_server: Res<'_, AssetServer>,
    mut buffers: ResMut<'_, Assets<ShaderStorageBuffer>>,
    mut materials: ResMut<'_, Assets<LineMaterial>>,
    mut meshes: ResMut<'_, Assets<Mesh>>,
    // The active image's per-image adjustments (templates feature only). Absent
    // resource / no active template ⇒ identity defaults (prior behaviour).
    #[cfg(feature = "templates")] adjustments: Option<
        Res<'_, crate::line::template_adjustments_store::LineTemplateAdjustments>,
    >,
    // Optional debug toggles (present only when a `WC_DEBUG_*` var is set, and
    // only in debug builds). Placed last so the release signature is unchanged.
    #[cfg(debug_assertions)] debug_toggles: Option<Res<'_, wc_core::debug::DebugToggles>>,
) {
    let w = window.width();
    let win_h = window.height();
    let half_w = w * 0.5;
    let half_h = win_h * 0.5;
    // TODO(plan-12+): if a sketch needs the Line camera off-center, promote
    // mid_y to a setting.
    let mid_y = 0.0_f32; // window-centered world

    // v4 particleDensity = 10 per canvas-pixel of width. Derive count from
    // density × width, clamping to a sane range (avoids massive resize spikes).
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "density × width is positive and bounded by clamp"
    )]
    let count = ((settings.particle_density * w).round() as u32).clamp(100, 100_000);

    // Build particle positions: heatmap sampler if `spawn_template` is set,
    // else the default horizontal-line layout. The heatmap sampler returns
    // window-space (top-left origin, +y down); we convert to world-space
    // (centered, +y up) during Particle construction below.
    let initial: Vec<Particle> = if settings.spawn_template.is_empty() {
        let white = crate::line::template_adjustments::pack_rgb8([255, 255, 255]);
        (0..count)
            .map(|i| {
                // Evenly space across the window width, centered on origin.
                let x = (i as f32 / count as f32) * w - half_w;
                // v4: subtle sawtooth Y-jitter `((i % 5) - 2) * 2` so particles
                // sit on five stacked horizontal strands rather than a line. No
                // template: white = no colour tint (template_color uniform 0).
                let y = mid_y + ((i % 5) as f32 - 2.0) * 2.0;
                make_particle(i, x, y, white)
            })
            .collect()
    } else {
        // Window-space positions + per-particle colours from the heatmap
        // sampler, reshaped by the active image's adjustments (identity defaults
        // when the feature is off or no entry exists — prior behaviour exactly).
        let path = Path::new(&settings.spawn_template);
        #[cfg(feature = "templates")]
        let adj = adjustments
            .as_ref()
            .and_then(|a| {
                crate::line::template_adjustments_store::hash_of_path(&settings.spawn_template)
                    .map(|h| a.get(&h))
            })
            .unwrap_or_default();
        #[cfg(not(feature = "templates"))]
        let adj = crate::line::template_adjustments::TemplateAdjustments::default();
        let sampled = sample_from_heatmap(path, w, win_h, count as usize, &adj);
        sampled
            .into_iter()
            .enumerate()
            .map(|(i, sp)| {
                // Convert window-space (top-left origin, +y down) to centered
                // world-space (+y up) — the coordinate system the rest of the
                // sketch uses.
                let x = sp.pos.x - half_w;
                let y = -(sp.pos.y - half_h);
                make_particle(
                    i as u32,
                    x,
                    y,
                    crate::line::template_adjustments::pack_rgb8(sp.color),
                )
            })
            .collect()
    };

    // Upload particle data to a GPU storage buffer.
    // `ShaderStorageBuffer::new` takes raw bytes + usage flags.
    let particle_bytes = cast_slice::<Particle, u8>(&initial);
    let particles_handle = buffers.add(ShaderStorageBuffer::new(
        particle_bytes,
        RenderAssetUsages::RENDER_WORLD,
    ));

    // Star sprite for the particle quads (ported from v4's
    // `src/materials/starMaterial/star.png`). Loaded via `AssetServer` so
    // Bevy's image loader (`ImagePlugin`, included in `DefaultPlugins`)
    // decodes the PNG into a GPU texture asynchronously; the bind group
    // becomes valid once the asset finishes loading.
    let star_texture: Handle<Image> = asset_server.load("sketches/line/star.png");

    // Debug-only: `WC_DEBUG_SOLID_PARTICLES` paints every particle a flat
    // colour for render-stage isolation. Off-sentinel (alpha 0) in normal
    // runs and always off in release (no `DebugToggles`).
    #[cfg(debug_assertions)]
    let solid_color = debug_toggles
        .as_ref()
        .and_then(|t| t.solid_particles)
        .map_or_else(LineMaterial::solid_off, |[r, g, b, a]| {
            Vec4::new(r, g, b, a)
        });
    #[cfg(not(debug_assertions))]
    let solid_color = LineMaterial::solid_off();

    let material_handle = materials.add(LineMaterial {
        particles: particles_handle.clone(),
        star_texture,
        solid_color,
        // Velocity tint off at spawn (Active-mode value); the attract driver
        // ramps it with the screensaver fade.
        attract_color: LineMaterial::attract_color_off(),
        // Colour influence off at spawn; the colour-influence driver writes the
        // active template's value each frame.
        template_color: LineMaterial::template_color_off(),
    });

    // Build a flat mesh with `count * 6` vertices (all at origin).
    // The vertex shader derives particle position + quad corner from
    // @builtin(vertex_index), so the mesh only needs to exist to trigger
    // the draw call — its vertex data is unused.
    let vertex_count = count as usize * 6;
    let positions: Vec<[f32; 3]> = vec![[0.0, 0.0, 0.0]; vertex_count];
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);

    let mesh_handle = meshes.add(mesh);

    commands.spawn((
        LineRoot,
        bevy::mesh::Mesh2d(mesh_handle),
        bevy::sprite_render::MeshMaterial2d(material_handle),
        Transform::default(),
        GlobalTransform::default(),
        Visibility::default(),
    ));

    // Seed the CPU mirror with the same particle state. The `clone()` here is a
    // one-shot allocation at sketch entry — not per-frame — so the
    // no-allocations-in-hot-paths rule still holds.
    commands.insert_resource(LineCpuMirror {
        particles: initial.clone(),
    });

    // Install LineSimParams — the render world extracts this each frame.
    commands.insert_resource(LineSimParams {
        params: SimParams::default(),
        particles_handle,
        particle_count: count,
    });

    tracing::info!(count, "spawned Line sketch");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attract_lifespan_is_deterministic_and_in_range() {
        for i in 0..10_000_u32 {
            let a = attract_lifespan(i);
            let b = attract_lifespan(i);
            assert!(a.to_bits() == b.to_bits(), "lifespan must be deterministic");
            assert!(
                (ATTRACT_LIFESPAN_MIN_SECS..=ATTRACT_LIFESPAN_MAX_SECS).contains(&a),
                "lifespan {a} out of range at index {i}"
            );
        }
    }

    #[test]
    fn attract_lifespans_are_staggered() {
        // The whole point of per-particle lifespans is to avoid synchronized
        // respawn waves: over a typical buffer the seeded values must spread
        // across (not cluster within) the range. Check the mean sits near the
        // midpoint and both tails are reached.
        let n = 10_000_u32;
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        let mut sum = 0.0_f64;
        for i in 0..n {
            let l = attract_lifespan(i);
            min = min.min(l);
            max = max.max(l);
            sum += f64::from(l);
        }
        let mean = sum / f64::from(n);
        let mid = f64::from(ATTRACT_LIFESPAN_MIN_SECS + ATTRACT_LIFESPAN_MAX_SECS) / 2.0;
        assert!(
            (mean - mid).abs() < 1.0,
            "lifespan mean {mean} far from {mid}"
        );
        assert!(
            min < ATTRACT_LIFESPAN_MIN_SECS + 2.0,
            "low tail unreached: {min}"
        );
        assert!(
            max > ATTRACT_LIFESPAN_MAX_SECS - 2.0,
            "high tail unreached: {max}"
        );
    }

    #[test]
    fn spawn_hash_is_uniform_enough_for_the_fraction_gate() {
        // The fraction gate keeps particles with spawn_hash < fraction; the
        // hash must be roughly uniform so a 0.6 fraction keeps ~60% of the
        // field, evenly across index (and therefore screen-x) order.
        let n = 10_000_u32;
        let fraction = 0.6_f32;
        let mut kept = 0_u32;
        // Also count survivors in the left and right index halves — the
        // line layout maps index to screen-x, so a skewed hash would thin
        // one side of the image more than the other.
        let mut kept_left = 0_u32;
        for i in 0..n {
            let h = spawn_hash01(i);
            assert!((0.0..=1.0).contains(&h), "hash {h} out of unit range");
            if h < fraction {
                kept += 1;
                if i < n / 2 {
                    kept_left += 1;
                }
            }
        }
        let kept_frac = f64::from(kept) / f64::from(n);
        assert!(
            (kept_frac - 0.6).abs() < 0.03,
            "kept fraction {kept_frac} should be ~0.6"
        );
        let left_share = f64::from(kept_left) / f64::from(kept);
        assert!(
            (left_share - 0.5).abs() < 0.03,
            "survivors should be index-uniform, left share = {left_share}"
        );
    }
}
