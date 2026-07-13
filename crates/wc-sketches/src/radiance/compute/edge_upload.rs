//! Silhouette edge list → GPU storage buffer, keyed on generation.
//!
//! `SilhouetteEdges` (main world, Plan B) is refilled in place on each body
//! frame and bumps `generation`. Extracting the `Vec` every render frame
//! would clone ~32 KB per frame (a hot-path allocation) and re-uploading it
//! through a `ShaderBuffer` asset would recreate the GPU buffer — churning
//! the bind-group cache's `BufferId` key ~30 times a second. Instead:
//!
//! 1. [`extract_silhouette_edges`] (`ExtractSchedule`) copies the points into
//!    a render-world scratch (`ExtractedEdges`, capacity `MAX_EDGE_POINTS`,
//!    refilled with `clear()` — zero steady-state allocation) ONLY when
//!    `generation` changed.
//! 2. [`upload_silhouette_edges`] (`RenderSystems::PrepareBindGroups`, before
//!    the bind-group prepare) `write_buffer`s the scratch into the persistent
//!    `edges_buffer` on [`super::pipeline::RadiancePipeline`] — a staged
//!    copy, no allocation, stable `BufferId`.
//!
//! The kernel indexes `% edge_count` into the full-capacity buffer, so a
//! frame where the count shrinks can never read past the live prefix's
//! allocation.

use bevy::prelude::*;
use bevy::render::renderer::RenderQueue;
use bevy::render::Extract;
use wc_core::input::body::{EdgePoint, SilhouetteEdges, MAX_EDGE_POINTS};

use super::pipeline::RadiancePipeline;

/// Render-world scratch copy of the newest silhouette edge list.
#[derive(Resource)]
pub struct ExtractedEdges {
    /// Generation of the copy currently held (and, once uploaded, of the GPU
    /// buffer). `u64::MAX` = "never copied" sentinel, so the first real
    /// generation (whatever Plan B starts at) always triggers a copy.
    pub generation: u64,
    /// Point scratch; capacity `MAX_EDGE_POINTS`, refilled with `clear()`.
    pub points: Vec<EdgePoint>,
    /// A fresh copy is waiting for [`upload_silhouette_edges`].
    pub dirty: bool,
}

impl Default for ExtractedEdges {
    fn default() -> Self {
        Self {
            generation: u64::MAX,
            points: Vec::with_capacity(MAX_EDGE_POINTS),
            dirty: false,
        }
    }
}

/// `ExtractSchedule`: copy the main-world edge list when (and only when) its
/// generation changed. No-ops in one compare in the steady state between
/// body frames.
pub fn extract_silhouette_edges(
    main: Extract<'_, '_, Option<Res<'_, SilhouetteEdges>>>,
    mut extracted: ResMut<'_, ExtractedEdges>,
) {
    let Some(src) = main.as_ref() else {
        return;
    };
    if src.generation == extracted.generation {
        return;
    }
    extracted.points.clear();
    // The contract caps the source at MAX_EDGE_POINTS; truncate defensively
    // so the scratch (and the fixed GPU buffer) can never overflow.
    let take = src.points.len().min(MAX_EDGE_POINTS);
    extracted.points.extend_from_slice(&src.points[..take]);
    extracted.generation = src.generation;
    extracted.dirty = true;
}

/// `Render` (`PrepareBindGroups`, ordered before the bind-group prepare):
/// stage the fresh copy into the persistent edge buffer.
pub fn upload_silhouette_edges(
    pipeline: Option<Res<'_, RadiancePipeline>>,
    render_queue: Res<'_, RenderQueue>,
    mut extracted: ResMut<'_, ExtractedEdges>,
) {
    let Some(pipeline) = pipeline else {
        return;
    };
    if !extracted.dirty {
        return;
    }
    if !extracted.points.is_empty() {
        render_queue.0.write_buffer(
            &pipeline.edges_buffer,
            0,
            bytemuck::cast_slice(&extracted.points),
        );
    }
    extracted.dirty = false;
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// The scratch starts at the never-copied sentinel with full capacity and
    /// nothing pending — so the first real generation always copies, and the
    /// steady state never allocates.
    #[test]
    fn extracted_edges_default_is_clean_sentinel() {
        let e = ExtractedEdges::default();
        assert_eq!(e.generation, u64::MAX);
        assert!(e.points.is_empty());
        assert!(e.points.capacity() >= MAX_EDGE_POINTS);
        assert!(!e.dirty);
    }
}
