//! Level-ordered tree layout for the GPU IFS.
//!
//! The node buffer is laid out level by level (root at slot 0), and
//! **branch-major within each level**: all branch-0 children of a level come
//! first, then all branch-1 children, and so on. Branch-major ordering keeps
//! neighboring compute threads on the same branch, so the variation `switch`
//! in `simulate.wgsl` stays warp-coherent. Within a level of `parent_count`
//! parents, in-level index `local` maps to:
//!
//! ```text
//! branch = local / parent_count
//! parent = parent_start + (local % parent_count)
//! ```

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "v4 parity: depth = floor(ln(target)/ln(b)) narrows f64 -> u32 \
              (bounded <= 16), and the complexity ramp narrows u32 <-> f32 \
              after an explicit clamp/round; both are documented inline"
)]
#![allow(
    clippy::doc_markdown,
    reason = "the layout docs name v4 identifiers (branch_count, target_points, \
              MAX_LEVELS) as prose alongside the branch-major index formula"
)]

/// Node-buffer capacity. v4 `MAX_POINTS`; the deepest reachable tree
/// (2 branches, depth 16) totals 131,071 nodes.
pub const MAX_POINTS: u32 = 200_000;

/// Upper bound on levels for the fixed-size dynamic-offset uniform array.
/// Deepest reachable tree is 17 levels (b = 2); headroom for point-budget
/// experiments.
pub const MAX_LEVELS: usize = 24;

/// One tree level's span in the node buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelSpan {
    /// First node slot of this level.
    pub start: u32,
    /// Node count in this level (`branch_count * parent_count`).
    pub count: u32,
    /// First node slot of the parent level.
    pub parent_start: u32,
    /// Node count of the parent level.
    pub parent_count: u32,
}

/// Complete layout for one (branch_count, target_points) pair. Rebuilt on
/// name change; never on the per-frame path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelLayout {
    /// Level 0 is the root (count 1). Levels 1..=depth are dispatched.
    pub levels: Vec<LevelSpan>,
    /// Total node count across all levels.
    pub total: u32,
}

/// v4 `computeDepth`: `floor(ln(target)/ln(b))`. Callers guarantee `b >= 2`
/// ([`super::branches::normalize_name`] makes 1 branch unreachable).
#[must_use]
pub fn compute_depth(branch_count: u32, target_points: f64) -> u32 {
    debug_assert!(branch_count >= 2, "1-branch fractals are unreachable");
    let depth = (target_points.ln() / f64::from(branch_count).ln()).floor();
    // Depth is tiny (<= 16 for target 100k); the cast is exact.
    depth as u32
}

impl LevelLayout {
    /// Build the layout for `branch_count` branches at the given point target.
    #[must_use]
    pub fn build(branch_count: u32, target_points: f64) -> Self {
        let depth = compute_depth(branch_count, target_points);
        let mut levels = Vec::with_capacity(usize::try_from(depth + 1).unwrap_or(MAX_LEVELS));
        let mut start = 0_u32;
        let mut count = 1_u32;
        let mut parent_start = 0_u32;
        let mut parent_count = 0_u32;
        for level in 0..=depth {
            levels.push(LevelSpan {
                start,
                count,
                parent_start,
                parent_count,
            });
            parent_start = start;
            parent_count = count;
            start += count;
            if level < depth {
                count *= branch_count;
            }
        }
        Self {
            levels,
            total: start,
        }
    }

    /// Node count visible at `complexity` in [0, 1]: 0 shows only the root,
    /// 1 shows everything, and intermediate values cut smoothly (mid-level)
    /// so the screensaver ember ramp has no visible level "pops".
    #[must_use]
    pub fn live_count_for_complexity(&self, complexity: f32) -> u32 {
        let c = complexity.clamp(0.0, 1.0);
        let span = (self.total - 1) as f32;
        // 1 + c * (total - 1), rounded — exact at both endpoints.
        1 + (c * span).round() as u32
    }

    /// Number of leading levels (including the never-dispatched root level 0)
    /// that intersect the live prefix `[0, live)`. The compute pass dispatches
    /// levels `1..n`; deeper levels hold only invisible nodes and are skipped.
    #[must_use]
    pub fn dispatch_levels_for_live(&self, live: u32) -> u32 {
        let mut n = 0_u32;
        for level in &self.levels {
            if level.start < live {
                n += 1;
            } else {
                break;
            }
        }
        n
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
#[allow(
    clippy::erasing_op,
    clippy::identity_op,
    reason = "the branch-major index probes spell out `0 / parent_count` and \
              `local % parent_count` literally to document the mapping"
)]
mod tests {
    use super::*;

    /// Depth matches v4's `computeDepth` (floor(ln(100000)/ln(b))) for every
    /// reachable branch count, and totals stay under the buffer capacity.
    /// Cross-check values from the golden generator.
    #[test]
    fn depth_and_totals_match_v4() {
        let cases: [(u32, u32, u32); 5] = [
            // (branches, depth, total nodes incl. root)
            (2, 16, 131_071),
            (3, 10, 88_573),
            (4, 8, 87_381),
            (5, 7, 97_656),
            (8, 5, 37_449),
        ];
        for (b, depth, total) in cases {
            assert_eq!(compute_depth(b, 100_000.0), depth, "depth for b={b}");
            let layout = LevelLayout::build(b, 100_000.0);
            assert_eq!(layout.total, total, "total for b={b}");
            assert!(layout.total <= MAX_POINTS);
            assert_eq!(
                layout.levels.len(),
                usize::try_from(depth + 1).expect("fits")
            );
        }
    }

    /// Level 0 is the root (1 node at slot 0); each level L has
    /// count = b * parent_count, contiguous starts, and parent spans pointing
    /// at the previous level.
    #[test]
    fn level_spans_are_contiguous_and_parented() {
        let layout = LevelLayout::build(3, 100_000.0);
        assert_eq!(layout.levels[0].start, 0);
        assert_eq!(layout.levels[0].count, 1);
        let mut expected_start = 1;
        for l in 1..layout.levels.len() {
            let level = &layout.levels[l];
            let parent = &layout.levels[l - 1];
            assert_eq!(level.start, expected_start, "level {l} start");
            assert_eq!(level.count, parent.count * 3, "level {l} count");
            assert_eq!(level.parent_start, parent.start);
            assert_eq!(level.parent_count, parent.count);
            expected_start += level.count;
        }
        assert_eq!(expected_start, layout.total);
    }

    /// Branch-major indexing: for level L with parent_count P, in-level index
    /// `local` maps to branch `local / P` and parent offset `local % P`. The
    /// whole family of a branch is contiguous (warp-coherent variation switch).
    #[test]
    fn branch_major_indexing() {
        let layout = LevelLayout::build(4, 100_000.0);
        let l2 = &layout.levels[2]; // 16 nodes, parents are the 4 level-1 nodes
        assert_eq!(l2.parent_count, 4);
        // local 0..4 are branch 0 children of parents 0..4; local 4..8 branch 1.
        assert_eq!(0 / l2.parent_count, 0);
        assert_eq!(5 / l2.parent_count, 1);
        assert_eq!(5 % l2.parent_count, 1);
        assert_eq!(15 / l2.parent_count, 3);
    }

    /// Complexity 1.0 -> all nodes; 0.0 -> just the root; monotonic between;
    /// smooth (can cut mid-level).
    #[test]
    fn live_count_for_complexity_is_monotonic_and_smooth() {
        let layout = LevelLayout::build(5, 100_000.0);
        assert_eq!(layout.live_count_for_complexity(1.0), layout.total);
        assert_eq!(layout.live_count_for_complexity(0.0), 1);
        let half = layout.live_count_for_complexity(0.5);
        assert!(half > 1 && half < layout.total);
        let mut prev = 0;
        for i in 0..=20 {
            let c = i as f32 / 20.0;
            let live = layout.live_count_for_complexity(c);
            assert!(live >= prev, "monotonic at {c}");
            prev = live;
        }
        // Smooth: neighboring complexities differ by less than a whole level.
        let a = layout.live_count_for_complexity(0.50);
        let b = layout.live_count_for_complexity(0.51);
        let biggest_level = layout.levels.last().expect("levels").count;
        assert!(b - a < biggest_level, "sub-level granularity");
    }

    /// dispatch_levels_for_live returns the number of levels (including the
    /// root level 0, which is never dispatched) whose nodes intersect
    /// [0, live): dispatching that prefix updates every visible node.
    #[test]
    fn dispatch_levels_covers_live_prefix() {
        let layout = LevelLayout::build(5, 100_000.0);
        // live = total -> all levels.
        assert_eq!(
            layout.dispatch_levels_for_live(layout.total),
            u32::try_from(layout.levels.len()).expect("fits")
        );
        // live = 1 (root only) -> 1 (no child level needs dispatch).
        assert_eq!(layout.dispatch_levels_for_live(1), 1);
        // live cutting into level 2 -> 3 levels (0, 1, 2).
        let into_l2 = layout.levels[2].start + 1;
        assert_eq!(layout.dispatch_levels_for_live(into_l2), 3);
    }

    /// MAX_LEVELS accommodates the deepest reachable tree (b=2 -> 17 levels).
    #[test]
    fn max_levels_headroom() {
        let layout = LevelLayout::build(2, 100_000.0);
        assert!(layout.levels.len() <= MAX_LEVELS);
        assert_eq!(layout.levels.len(), 17);
    }
}
