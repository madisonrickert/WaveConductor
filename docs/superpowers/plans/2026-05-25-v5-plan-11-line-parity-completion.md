# Plan 11: Line Parity Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the four parity gaps surfaced by Plan 10's hands-on run on 2026-05-25, sign `crates/wc-sketches/src/line/PARITY.md`, and tag `v5-line-parity`.

**Architecture:** Five small, independent surgeries on the existing Line sketch. (1) Swap the rotationally-symmetric `Annulus` ring mesh for a low-segment `RegularPolygon` so the per-frame `(10 - idx) / 20 * power` rotation is visibly perceivable. (2) Extend `update_mouse_attractor` to read `Res<Touches>` and `Res<HandTrackingState>` for press, gated by an `update_mouse_attractor` rewrite that consolidates the three input sources behind one `just_pressed_any()` helper. (3) Add a `FilePath` variant to the settings derive macro and panel renderer, plumb `rfd::FileDialog` through, and add per-field `#[serde(default)]` so future field additions don't silently zero sibling values. (4) Add a small integration test that exercises the heatmap-spawn path end-to-end with a real PNG. (5) Operator-driven side-by-side capture against v4, then a tagged release.

**Tech Stack:** `rfd = "0.15"` (new — native file picker on macOS NSOpenPanel / Windows IFileDialog / GTK on Linux). No other new workspace deps.

**Reference v4 source:** `src/particles/attractor.ts` — confirmed `RingGeometry(15, 18, 32)` with a 0.8 rad X-axis tilt on the group. The 2D Bevy port can't replicate the 3D tilt, so the polygon-segments trick is what makes Z-rotation legible in 2D. The roadmap (and PARITY.md verdict §1) call this out.

**Branch:** `rewrite/bevy`. Pre-flight: HEAD at `c77abf5` (`gitignore: Claude Code and skills tooling state`) or later — verify `git log -1 --oneline` before starting Phase 0.

---

## Scope check

Plan 11 is the last plan in the Line stack. Plans 12+ port Flame, Dots, Cymatics, and Waves.

Six phases, six commits. Phase F pushes the tag.

**In scope:**
- Visible ring rotation (Annulus → low-segment polygon).
- Touch press (verify; already partially wired in `mouse.rs:51-52`) + hand-tracking pinch gesture for synthetic press.
- `rfd`-based file picker for `spawn_template`.
- Per-field `#[serde(default)]` on `LineSettings` (and the other settings struct that ships today, `TestSketchSettings` if still present).
- Heatmap-spawn end-to-end smoke test with a real bundled PNG.
- Manual side-by-side parity capture and `PARITY.md` sign-off.

**Out of scope (deferred to Plan 12+):**
- Hand-tracking *provider implementation*. `HandTrackingState` has no writer yet — Plan 11 wires the gesture-detection consumer side and tests it with synthetic frames. The real Leap / MediaPipe provider lands later. The `wc-core` feature flag `hand-tracking-gestures` (added in Phase B) keeps the consumer path behind a flag so production behavior is unchanged until a provider exists.
- Plan 8 known-deferred items (post-process gating outside `AppState::Line`, per-frame uniform-buffer reuse — carry-forwards #53, #54). Fold into the next render-graph plan.

## File map

**Modified:**
- `crates/wc-sketches/src/line/attractor_visuals.rs` — swap `Annulus::new(...)` for a stroked low-segment ring mesh built from two `RegularPolygon`s (outer minus inner, custom triangle-list).
- `crates/wc-sketches/src/line/systems/mouse.rs` — add `HandTrackingState` pinch-gesture press, refactor `just_pressed` evaluation.
- `crates/wc-sketches/src/line/settings.rs` — add `#[serde(default)]` to every field; switch `spawn_template`'s `#[setting(ty = Text, ...)]` to a new `ty = FilePath`.
- `crates/wc-core-macros/src/lib.rs` — add `FilePath` variant + `extensions = [...]` attribute to the `#[setting(...)]` grammar.
- `crates/wc-core/src/settings/def.rs` — add `SettingKind::FilePath { extensions: &'static [&'static str] }` variant.
- `crates/wc-core/src/settings/panel_user.rs` — render `FilePath` widget (text-edit + Browse… button → `rfd::FileDialog`).
- `crates/wc-core/src/settings/panel_dev.rs` — same FilePath rendering for the dev panel (symmetry).
- `crates/wc-core/Cargo.toml` — add `rfd = "0.15"` dependency, gated on `not(target_arch = "wasm32")` (rfd has a wasm backend but it returns a JS promise — out of scope for Plan 11).
- `Cargo.toml` (workspace) — declare `rfd = "0.15"` once.
- `crates/wc-sketches/src/line/PARITY.md` — flip Verdict from PENDING → PASS after the capture; record v4/v5 commit hashes.

**Created:**
- `crates/wc-sketches/tests/line_heatmap_e2e.rs` — drives `spawn_line` with a real `assets/sketches/line/star.png` path; asserts particles cluster near the bright center pixels.

**Untouched but verified:**
- `crates/wc-core/src/input/state.rs` — `Hand.pinch_strength` already exists.
- `crates/wc-core/src/input/pointer.rs` — already routes hand/touch position into `PointerState.primary` correctly. No change.

---

# Phase 0 — Drain carry-forwards 30–63

`docs/superpowers/next-plan-carry-forwards.md` has items 30–63 still open at Plan 11 start. Triage them in two passes:

- **Absorb now** — fits Plan 11's scope, low cost, related to Line.
- **Push to Plan 12+** — sketch-specific (Flame/Dots/Cymatics/Waves), pre-release distribution work, or upstream Bevy noise we can't fix.

### Task 1: Triage the open items

- [ ] **Step 1: Read the file and categorize each open item (30–63)**

Open `docs/superpowers/next-plan-carry-forwards.md`. For each numbered item still in the file, write next to it in your worklog one of:

- `absorb-phase-0` — Plan 11 Phase 0 will land it.
- `phase-A/B/C/D/E` — folds naturally into a later phase of Plan 11.
- `defer-12+` — sketch-specific or distribution work; leave in the file.

Expected categorization (the implementer should verify; treat the list below as the recommended split, not a binding decree):

- **Absorb in Phase 0:**
  - #30 (`_held` dead code in `mouse.rs:53-54`) — gets touched anyway in Phase B; resolve there.
  - #31 (weak directional assertion in `one_attractor_pulls_particle`).
  - #32 (`EXPECTED_POST_DECAY_POWER` brittleness).
  - #33 (`step_one` rustdoc hot-path note).
  - #36 (stale "drag moves to Dev" claim in Plan 7 commit msg — patch the plan doc).
  - #37 (visual verification of horizontal-line spawn — moot now Plan 8/9/10 shipped; mark resolved).
  - #38 (`mid_y = 0.0_f32` could become a setting — leave the comment, no code change).
  - #39 (`arm_idle_timeline` duplication — hoist to `tests/common/`).
  - #40 (group veto tests adjacent in `line_lifecycle.rs`).
  - #41 (`use ... RegisterIdleVetoExt;` hoist).
  - #45 (`pointer_merge_system` test-fidelity) — touched in Phase B test; resolve there.
  - #51 (`SIM_PARAMS_SIZE` cast comment).
  - #52 (cursor-moved-reader newest-wins comment).
  - #58, #59 (Bevy/winit shutdown noise; not actionable from our code — close as won't-fix with a note).
- **Push to Phase A:** #56 (Annulus rotation invisibility — *is* Phase A).
- **Push to Phase B:** #60 (touch + hand-tracking activation — *is* Phase B), #61 (hand-tracking provider stub).
- **Push to Phase C:** #57 (LineSettings serde defaults — *is* Phase C), #62 (`spawn_template` file picker — *is* Phase C).
- **Push to Phase D:** #63 (heatmap-spawn E2E — *is* Phase D).
- **Defer to Plan 12+:** #44 (`line_idle_veto` visibility — not currently needed), #47 (`seed_pointer` hoist — moot once #45 closes), #48 (`#[path]` fragility — reminder), #49 (`enter_line()` update count — already trimmed in Plan 10), #53, #54 (post-process gating + uniform-buffer reuse — render-graph plan), #55 (visual gravity-smear verification — Madison verified during Plan 10 manual run).

### Task 2: Apply the Phase 0 picks

For each `absorb-phase-0` item, surgically apply the listed change:

- [ ] **Step 1: #30 — delete `_held` if Phase B doesn't need it; otherwise add `#[allow(unused_variables, reason = "Plan 11 hand-tracking will read this")]`**

(Phase B's rewrite of `update_mouse_attractor` will land first; if `_held` is gone after Phase B, this item closes naturally and you can mark it done without a code change in Phase 0. Triage decision: leave #30 to Phase B.)

- [ ] **Step 2: #31 — tighten `one_attractor_pulls_particle` directional assertion**

**File:** `crates/wc-sketches/src/line/sim_cpu.rs:127-145`

Replace the `velocity[0] > 0.0` check with a numeric comparison to the expected acceleration. The expected x-velocity after one frame at the test's parameters is:

```rust
// (1 attractor at +X, particle at origin, attractor power=P, gravity-scaled G_SCALE).
// Force on x-axis: P * G_SCALE (from particle::SimParams::compute_attraction
// at unit distance — see particle.rs for the actual formula).
// Velocity after 1 step ≈ force * dt - particle * drag_term.
// Use a generous tolerance (±10%) so floating-point order doesn't fail the test.
let expected = power * gravity_scale * dt;
assert!(
    (velocity[0] - expected).abs() < expected * 0.10,
    "x-velocity {} not within 10% of expected {}",
    velocity[0], expected
);
```

Look at the existing test for the exact constants in scope; the snippet above describes the *shape*, not literal values. Run the test before and after to confirm it still passes; the goal is "regression detector" not "value oracle."

- [ ] **Step 3: #32 — `EXPECTED_POST_DECAY_POWER` brittleness**

**File:** `crates/wc-sketches/tests/line_lifecycle.rs:230-256`

Promote the hard-coded 9.2 (or whatever value is currently in the test) to a `const` derived from the production constants:

```rust
// Production constants the test depends on.
use wc_sketches::line::systems::mouse::{
    MOUSE_POWER_DECAY, MOUSE_POWER_FLOOR, MOUSE_POWER_PRESS,
};

/// After one decay step from `MOUSE_POWER_PRESS`:
///   floor + (PRESS - floor) * DECAY = 2.0 + 8.0 * 0.9 = 9.2
const EXPECTED_POST_DECAY_POWER: f32 =
    MOUSE_POWER_FLOOR + (MOUSE_POWER_PRESS - MOUSE_POWER_FLOOR) * MOUSE_POWER_DECAY;
```

If the constants are `pub(crate)` only, hoist them to `pub` first (single-line attribute change in `mouse.rs`). They already document v4 parity values, so making them `pub` doesn't leak implementation detail.

- [ ] **Step 4: #33 — `step_one` rustdoc note**

**File:** `crates/wc-sketches/src/line/sim_cpu.rs:39`

Prepend to the existing rustdoc:

```rust
/// Pure function, allocation-free; called once per particle per frame from
/// [`step_cpu_mirror`]. Hot path — do not introduce branches or allocations.
/// (existing rustdoc continues...)
```

- [ ] **Step 5: #36 — fix stale "drag moves to Dev" claim**

**File:** `docs/superpowers/plans/2026-05-23-v5-plan-6-line.md` (or wherever Plan 7 Task 26 lives — search for "drag moves to Dev")

Find the line and patch it to reflect what actually shipped: drag was *removed entirely* from `LineSettings` because Plan 7 baked v4's constants into `SimParams` directly. The commit message is immutable; the plan doc is not.

- [ ] **Step 6: #37 — close the "horizontal-line spawn visual verification" carry-forward**

This was confirmed visually in Plan 10's manual run. No code change. Just remove the item from `next-plan-carry-forwards.md` (no need to track a closed item).

- [ ] **Step 7: #38 — `mid_y` setting note**

No code change. Add a single-line `// TODO(plan-12+): if a sketch needs the Line camera off-center, promote mid_y to a setting.` comment to `crates/wc-sketches/src/line/systems/spawn.rs:74`.

- [ ] **Step 8: #39 — hoist `arm_idle_timeline` to `tests/common/`**

Two test files duplicate this helper:
- `crates/wc-sketches/tests/line_lifecycle.rs:183-193,324-334`
- `crates/wc-core/tests/lifecycle_idle_veto.rs:44-60`

Pick one as canonical, move it to `crates/wc-core/tests/common/lifecycle.rs` (create if absent), and rewrite the call sites to use it. Verify both test crates still compile (the `#[path]` import pattern is already in use — see `crates/wc-sketches/tests/common/mod.rs` for the existing wc-core test helpers reuse).

- [ ] **Step 9: #40 — group veto tests in `line_lifecycle.rs`**

Move `idle_veto_keeps_line_active_during_attractor_decay` to sit immediately after `update_sim_params_does_not_run_when_idle` in the file. No semantic change, just reordering.

- [ ] **Step 10: #41 — hoist `use ... RegisterIdleVetoExt;` to top of `line/mod.rs`**

It currently lives buried inside `LinePlugin::build` at `crates/wc-sketches/src/line/mod.rs:42`. Move to the top `use` block.

- [ ] **Step 11: #51 — `SIM_PARAMS_SIZE` cast comment**

**File:** `crates/wc-sketches/src/line/compute.rs` (the `SIM_PARAMS_SIZE` const)

Add to the existing const:

```rust
#[allow(
    clippy::cast_possible_truncation,
    reason = "size_of::<SimParams>() fits in u64 on all supported targets; \
              u64::try_from(usize) isn't const-stable in 1.89"
)]
const SIM_PARAMS_SIZE: NonZeroU64 = match NonZeroU64::new(...) { ... };
```

Look at the existing site for exact form. The goal is no behavior change — just narrow the allow with a reason.

- [ ] **Step 12: #52 — newest-wins cursor reader comment**

**File:** `crates/wc-core/src/input/pointer.rs:68`

Currently:
```rust
let cursor_msg_position: Option<Vec2> = cursor_moved_reader.read().last().map(|c| c.position);
```

Add an explicit comment immediately above:
```rust
// Newest-wins: `last()` discards intermediate positions intentionally — we
// want the pointer's *current* location, not its motion path. Sketches that
// need motion deltas should read `CursorMoved` events directly instead of
// going through PointerState.
```

(The existing comment further down covers this but is below the call; the new comment puts it adjacent to the line.)

- [ ] **Step 13: #58, #59 — close-as-won't-fix**

These are upstream Bevy/winit noise. In `next-plan-carry-forwards.md`, replace each item with a one-line resolution:
```
58. RESOLVED 2026-05-25 (Plan 11 Phase 0): Bevy 0.18 shutdown noise; upstream issue; not actionable. Re-evaluate at Bevy 0.19+ point bump.
59. RESOLVED 2026-05-25 (Plan 11 Phase 0): bevy_pbr Metal info warning (bevy issue #18149); not actionable from our code.
```

Don't delete — leaving a resolution line documents that we triaged and chose inaction.

- [ ] **Step 14: Verify**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All must pass before committing.

- [ ] **Step 15: Commit Phase 0**

```bash
git add docs/superpowers/next-plan-carry-forwards.md \
        docs/superpowers/plans/2026-05-23-v5-plan-6-line.md \
        crates/wc-sketches/src/line/sim_cpu.rs \
        crates/wc-sketches/tests/line_lifecycle.rs \
        crates/wc-sketches/src/line/systems/mouse.rs \
        crates/wc-sketches/src/line/systems/spawn.rs \
        crates/wc-sketches/src/line/mod.rs \
        crates/wc-sketches/src/line/compute.rs \
        crates/wc-core/src/input/pointer.rs \
        crates/wc-core/tests/common/

git commit -m "$(cat <<'EOF'
Plan 11 Phase 0: drain carry-forwards 30-63

Absorbs the small/related items from next-plan-carry-forwards.md that
sit naturally in Plan 11's scope. Sketch-touching items (rings, touch,
file picker, heatmap E2E) move into their own phases; provider/render-
graph/distribution items defer to Plan 12+.

Items landed: #31, #32, #33, #36, #37, #38, #39, #40, #41, #51, #52,
plus close-as-won't-fix resolutions for #58 and #59.

Items now owned by later phases of Plan 11: #30 (Phase B), #45 (Phase B),
#56 (Phase A), #57 (Phase C), #60 (Phase B), #61 (Phase B), #62 (Phase C),
#63 (Phase D).

Items deferred to Plan 12+: #44, #47, #48, #49, #53, #54, #55.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase A — Visible attractor ring rotation

v4's rings (`RingGeometry(15, 18, 32)` smooth annulus, 0.8 rad X-axis tilt + Y-axis rotation in 3D) are visibly spinning because the tilt makes Y-rotation produce elliptical foreshortening. The 2D Bevy port can't tilt, so a circular `Annulus` looks identical at any rotation. Fix: replace the annulus with a 6-segment polygonal ring built from two `RegularPolygon`s differenced by the WGSL renderer — *or* by constructing a custom `Mesh::TriangleList` with 6 outer + 6 inner vertices.

The custom mesh path is simpler and avoids changing the render pipeline. Build it once at module init (the mesh is identical for all rings; per-ring `Transform` scales it).

### Task 3: Build the polygonal ring mesh helper

**File:** `crates/wc-sketches/src/line/attractor_visuals.rs`

- [ ] **Step 1: Write the failing test**

Add a `#[cfg(test)] mod tests` block at the bottom of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::PrimitiveTopology;

    #[test]
    fn polygonal_ring_has_2n_vertices_and_2n_triangles() {
        let n: u32 = 6;
        let mesh = build_polygonal_ring_mesh(RING_INNER_RADIUS, RING_OUTER_RADIUS, n);
        assert_eq!(mesh.primitive_topology(), PrimitiveTopology::TriangleList);
        // 2n vertices (one inner + one outer per segment).
        let positions = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .expect("position attribute");
        if let bevy::mesh::VertexAttributeValues::Float32x3(pos) = positions {
            assert_eq!(pos.len(), (2 * n) as usize);
        } else {
            panic!("position attribute must be Float32x3");
        }
        // 2n triangles → 6n indices (3 per triangle).
        let indices = mesh.indices().expect("indexed mesh");
        assert_eq!(indices.len(), (6 * n) as usize);
    }

    #[test]
    fn polygonal_ring_first_outer_vertex_is_on_outer_radius() {
        let mesh = build_polygonal_ring_mesh(15.0, 18.0, 6);
        let positions = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .expect("position attribute");
        if let bevy::mesh::VertexAttributeValues::Float32x3(pos) = positions {
            // Convention used by build_polygonal_ring_mesh:
            // vertex 0 = inner radius at angle 0; vertex 1 = outer at angle 0.
            let inner = pos[0];
            let outer = pos[1];
            let inner_len = (inner[0] * inner[0] + inner[1] * inner[1]).sqrt();
            let outer_len = (outer[0] * outer[0] + outer[1] * outer[1]).sqrt();
            assert!((inner_len - 15.0).abs() < 1e-4);
            assert!((outer_len - 18.0).abs() < 1e-4);
        }
    }
}
```

- [ ] **Step 2: Run test, watch it fail**

```bash
cargo test -p wc-sketches --lib line::attractor_visuals::tests
```

Expected: FAIL with `cannot find function 'build_polygonal_ring_mesh'`.

- [ ] **Step 3: Implement `build_polygonal_ring_mesh`**

Add this function to `attractor_visuals.rs`, between the constants and `spawn_attractor_visual`:

```rust
/// Number of segments around each ring. Six is the smallest count that still
/// reads as a "ring" (a circle) at typical viewing distances but is angular
/// enough that the per-frame rotation is visibly perceivable. v4 uses 32 with
/// a 3D tilt; we use 6 to compensate for the lack of 3D in this 2D port.
///
/// Carry-forward #56 (PARITY.md verdict §1) is the source-of-record for the
/// rotation-visibility motivation.
const RING_SEGMENTS: u32 = 6;

/// Build a flat polygonal ring mesh as an indexed triangle list.
///
/// Vertices alternate inner / outer around the ring at evenly-spaced angles
/// (`segments` segments → `2 × segments` vertices). The triangle list links
/// each pair `(inner_i, outer_i, outer_{i+1})` and `(inner_i, outer_{i+1},
/// inner_{i+1})` so the ring is two strips of triangles closing on itself.
///
/// Built once at sketch entry; all 10 rings of an attractor visual share this
/// mesh handle and use per-entity `Transform::scale` to size themselves.
///
/// Returns a `Mesh` with `Float32x3` positions and indexed topology. No
/// normals or UVs — the ring material is a flat `ColorMaterial`.
fn build_polygonal_ring_mesh(inner_radius: f32, outer_radius: f32, segments: u32) -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{Indices, PrimitiveTopology};

    let n = segments;
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity((2 * n) as usize);
    for i in 0..n {
        #[allow(
            clippy::as_conversions,
            clippy::cast_precision_loss,
            reason = "i ∈ 0..segments (≤ 16 in practice); u32→f32 round-trip is lossless"
        )]
        let angle = (i as f32) / (n as f32) * std::f32::consts::TAU;
        let (s, c) = angle.sin_cos();
        // Convention used by tests: even index i → inner, odd index → outer.
        positions.push([c * inner_radius, s * inner_radius, 0.0]);
        positions.push([c * outer_radius, s * outer_radius, 0.0]);
    }

    let mut indices: Vec<u32> = Vec::with_capacity((6 * n) as usize);
    for i in 0..n {
        let inner_i = 2 * i;
        let outer_i = 2 * i + 1;
        let inner_next = 2 * ((i + 1) % n);
        let outer_next = 2 * ((i + 1) % n) + 1;
        // Two triangles per segment forming a quad slice of the ring.
        indices.extend_from_slice(&[inner_i, outer_i, outer_next]);
        indices.extend_from_slice(&[inner_i, outer_next, inner_next]);
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}
```

- [ ] **Step 4: Run the tests; watch them pass**

```bash
cargo test -p wc-sketches --lib line::attractor_visuals::tests
```

Expected: PASS.

### Task 4: Wire the polygonal mesh into `spawn_attractor_visual`

**File:** `crates/wc-sketches/src/line/attractor_visuals.rs:113-118`

- [ ] **Step 1: Replace the Annulus construction**

Find:
```rust
let mesh_handle = meshes.add(Mesh::from(Annulus::new(
    RING_INNER_RADIUS,
    RING_OUTER_RADIUS,
)));
```

Replace with:
```rust
let mesh_handle = meshes.add(build_polygonal_ring_mesh(
    RING_INNER_RADIUS,
    RING_OUTER_RADIUS,
    RING_SEGMENTS,
));
```

- [ ] **Step 2: Remove the now-unused `Annulus` import**

`use bevy::math::primitives::Annulus;` at line 34 is now unused. Delete it.

- [ ] **Step 3: Update the module-level rustdoc**

`attractor_visuals.rs` lines 1-31 currently describe Annulus geometry. Update the "Geometry" section:

```
//! ## Geometry
//!
//! - Polygonal ring with [`RING_SEGMENTS`] = 6 segments. v4 uses 32-segment
//!   smooth annuli but tilts the parent group 0.8 rad on X so Y-rotation is
//!   visibly elliptical. This 2D port can't tilt, so a 6-segment polygon
//!   gives the rotation a legible corner to spin around — see Plan 11 § A.
//! - Inner radius: 15 world units.
//! - Outer radius: 18 world units.
//! - Per-ring scale: `1 + (i / 10)^2 * 2` (outer rings progressively larger).
//! - Group scale: `sqrt(power) / 5`.
//! - Per-ring rotation speed: `(10 - i) / 20 * power` rad/s (inner rings
//!   spin faster).
//! - Z position: `-1.0` so the rings sit just behind the particles.
//! - Color: v4 `#C5E2CC` ≈ `Color::srgb(0.77, 0.886, 0.8)`.
```

- [ ] **Step 4: Build and test**

```bash
cargo build -p wc-sketches
cargo test -p wc-sketches
```

Both must pass.

- [ ] **Step 5: Commit Phase A**

```bash
git add crates/wc-sketches/src/line/attractor_visuals.rs

git commit -m "$(cat <<'EOF'
Plan 11 Phase A: visible attractor ring rotation

Swap rotationally-symmetric `Annulus` for a 6-segment polygonal ring so
the per-frame `(10 - idx) / 20 * power` Z-rotation is perceivable. v4's
32-segment smooth annuli rely on a 0.8 rad X-axis tilt (group is in 3D
space) to make Y-rotation visibly elliptical; the 2D Bevy port can't
tilt, so we substitute angular features for that lost 3D shading cue.

Closes carry-forward #56.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase B — Touch + hand-tracking attractor activation

`update_mouse_attractor` currently activates on `mouse_buttons.just_pressed(Left) || touches.iter_just_pressed().next().is_some()`. The touch path was added in Plan 10 Phase 0 (commit `0bda435`) but never got an integration test, so it's untested end-to-end. The hand-tracking path is absent entirely — `Hand.pinch_strength` exists but nothing reads it.

This phase: (1) add `HandTrackingState` pinch-gesture press, (2) add an integration test confirming touch *and* hand-tracking activate the attractor end-to-end, (3) gate the hand-tracking consumer behind a `hand-tracking-gestures` feature flag (since no provider writes `HandTrackingState` yet — turning the flag on in tests, off in production-default, keeps behavior identical until Plan 12+ lands the provider).

### Task 5: Add the `hand-tracking-gestures` feature

**File:** `crates/wc-core/Cargo.toml`

- [ ] **Step 1: Locate the `[features]` table**

If it doesn't exist, add it under the package metadata block:

```toml
[features]
default = []
# Enables HandTrackingState consumers (sketch gesture detection). Disabled by
# default until a provider lands — turning this on with no provider means the
# consumer reads an always-empty `HandTrackingState` and is a no-op.
hand-tracking-gestures = []
```

If it already exists, append the `hand-tracking-gestures = []` line.

- [ ] **Step 2: Propagate the feature to wc-sketches**

**File:** `crates/wc-sketches/Cargo.toml`

Add a `[features]` section:

```toml
[features]
default = []
hand-tracking-gestures = ["wc-core/hand-tracking-gestures"]
```

- [ ] **Step 3: Promote `HandTrackingState::ingest` from `pub(crate)` to `pub`**

**File:** `crates/wc-core/src/input/state.rs:75`

Current:
```rust
/// Replace the state with the contents of a frame. Called only by the
/// `update_hand_tracking_state` system; not part of the public API.
pub(crate) fn ingest(&mut self, frame: &HandTrackingFrame) {
```

Replace with:
```rust
/// Replace the state with the contents of a frame.
///
/// Production write path is [`crate::input::systems::update_hand_tracking_state`]
/// (driven from `Messages<HandTrackingFrame>`). Promoted to `pub` in Plan 11
/// so integration tests can synthesize hand frames without a fake provider
/// — see `crates/wc-sketches/tests/line_input.rs::hand_pinch_activates_mouse_attractor`.
pub fn ingest(&mut self, frame: &HandTrackingFrame) {
```

No behavior change; just visibility.

- [ ] **Step 4: Verify both crates still build with default features**

```bash
cargo build -p wc-core
cargo build -p wc-sketches
```

Both must succeed.

### Task 6: Define the pinch-gesture threshold + edge detection

**File:** `crates/wc-sketches/src/line/systems/mouse.rs`

- [ ] **Step 1: Add the pinch state resource**

Below the existing `MOUSE_POWER_DECAY_EPSILON` constant:

```rust
/// Pinch strength at which a hand counts as "pressed" (analogous to a finger
/// touching the screen). Leap Motion's `pinch_strength` ranges `[0, 1]`; this
/// threshold gives a comfortable "pinched" pose without false-triggering on
/// half-closed hands.
#[cfg(feature = "hand-tracking-gestures")]
pub const PINCH_PRESS_THRESHOLD: f32 = 0.85;

/// Tracks last-frame pinch state per chirality so we can detect press *edges*
/// (transition from below-threshold to above), not just "is currently
/// pinched." Without edge detection, holding a pinched fist would re-trigger
/// `MOUSE_POWER_PRESS` every frame (the bug Plan 7 Phase C fixed for mouse).
#[cfg(feature = "hand-tracking-gestures")]
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct LastPinchState {
    /// Was the left hand above [`PINCH_PRESS_THRESHOLD`] last frame?
    pub left_pinched: bool,
    /// Was the right hand above [`PINCH_PRESS_THRESHOLD`] last frame?
    pub right_pinched: bool,
}
```

- [ ] **Step 2: Rewrite `update_mouse_attractor` to consume hand-tracking**

Replace the body of `update_mouse_attractor` (the entire `pub fn update_mouse_attractor`) with:

```rust
pub fn update_mouse_attractor(
    pointer: Res<'_, PointerState>,
    mouse_buttons: Res<'_, bevy::input::ButtonInput<bevy::input::mouse::MouseButton>>,
    touches: Res<'_, bevy::input::touch::Touches>,
    #[cfg(feature = "hand-tracking-gestures")] hands: Res<
        '_,
        wc_core::input::state::HandTrackingState,
    >,
    #[cfg(feature = "hand-tracking-gestures")] mut last_pinch: ResMut<'_, LastPinchState>,
    window: Single<'_, '_, &Window>,
    mut state: ResMut<'_, MouseAttractorState>,
) {
    // Per-source just-pressed edges. Any one of them transitions the
    // attractor into the pressed state. v4's `pointerdown` event fires for
    // mouse + touch + (in spirit) tracked-hand pinch alike.
    let mouse_just_pressed = mouse_buttons
        .just_pressed(bevy::input::mouse::MouseButton::Left);
    let touch_just_pressed = touches.iter_just_pressed().next().is_some();

    #[cfg(feature = "hand-tracking-gestures")]
    let hand_just_pressed = {
        let right_now = hands
            .right()
            .is_some_and(|h| h.pinch_strength >= PINCH_PRESS_THRESHOLD);
        let left_now = hands
            .left()
            .is_some_and(|h| h.pinch_strength >= PINCH_PRESS_THRESHOLD);
        let right_edge = right_now && !last_pinch.right_pinched;
        let left_edge = left_now && !last_pinch.left_pinched;
        // Write-back for next-frame edge detection.
        last_pinch.right_pinched = right_now;
        last_pinch.left_pinched = left_now;
        right_edge || left_edge
    };
    #[cfg(not(feature = "hand-tracking-gestures"))]
    let hand_just_pressed = false;

    let just_pressed = mouse_just_pressed || touch_just_pressed || hand_just_pressed;

    if let Some(cursor_window) = pointer.primary {
        let w = window.width();
        let h = window.height();
        let wx = cursor_window.x - w * 0.5;
        let wy = -(cursor_window.y - h * 0.5);
        state.position = [wx, wy];

        if just_pressed {
            state.power = MOUSE_POWER_PRESS;
        }
    }
}
```

- [ ] **Step 3: Delete the now-orphaned `_held` block**

Lines that previously read:
```rust
let _held = mouse_buttons.pressed(bevy::input::mouse::MouseButton::Left)
    || touches.iter().next().is_some();
```
(and the comment above them) are gone after the rewrite. This closes carry-forward #30.

- [ ] **Step 4: Register `LastPinchState` in the LinePlugin**

**File:** `crates/wc-sketches/src/line/mod.rs:79` (right after `init_resource::<MouseAttractorState>()`)

```rust
#[cfg(feature = "hand-tracking-gestures")]
app.init_resource::<systems::mouse::LastPinchState>();
```

- [ ] **Step 5: Update the `update_mouse_attractor` rustdoc**

Replace the existing rustdoc above `pub fn update_mouse_attractor` with:

```rust
/// Tracks pointer-button-equivalent transitions across mouse, touch, and
/// (under the `hand-tracking-gestures` feature) tracked-hand pinch, and
/// updates [`MouseAttractorState`].
///
/// Matches v4's `pointerdown`/`pointerup`: only the rising edge of "any
/// source pressed" sets `power = MOUSE_POWER_PRESS`. Held with a stationary
/// input decays to floor; positional updates just move the attractor.
///
/// Hand-tracking gesture (feature-gated, since `HandTrackingState` has no
/// writer until Plan 12+ lands a provider): pinch strength ≥
/// [`PINCH_PRESS_THRESHOLD`] = 0.85 counts as pressed. [`LastPinchState`]
/// tracks per-chirality edges so a held pinch doesn't re-trigger every frame.
pub fn update_mouse_attractor(
```

- [ ] **Step 6: Build with both feature configurations**

```bash
cargo build -p wc-sketches
cargo build -p wc-sketches --features hand-tracking-gestures
```

Both must succeed.

### Task 7: Integration test — touch activates the attractor

**File:** `crates/wc-sketches/tests/line_input.rs`

- [ ] **Step 1: Add a touch-activation test**

Append to the existing test file:

```rust
#[test]
fn touch_press_activates_mouse_attractor() {
    use wc_sketches::line::systems::mouse::MOUSE_POWER_PRESS;
    use common::input::{move_pointer, touch_start};

    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    // Move cursor to window center first so PointerState.primary is Some;
    // the attractor only updates position when a pointer is available.
    move_pointer(&mut app, 640.0, 360.0, Vec2::ZERO);
    app.update();

    let pre = app
        .world()
        .resource::<MouseAttractorState>()
        .power;
    #[allow(
        clippy::float_cmp,
        reason = "bit-for-bit baseline check: default power must be exactly 0.0"
    )]
    {
        assert_eq!(pre, 0.0, "attractor inactive before any input");
    }

    // Synthetic touch press at window center. `touch_start` writes a
    // TouchInput message; Bevy's touch processor folds it into `Touches`
    // before `update_mouse_attractor` runs.
    touch_start(&mut app, 1, 640.0, 360.0);
    app.update();

    let post = app
        .world()
        .resource::<MouseAttractorState>()
        .power;
    assert!(
        (post - MOUSE_POWER_PRESS).abs() < 1e-3,
        "expected power={MOUSE_POWER_PRESS} after touch_start; got {post}"
    );
}
```

- [ ] **Step 2: Run the test and confirm it passes**

```bash
cargo test -p wc-sketches --test line_input touch_press_activates_mouse_attractor
```

Expected: PASS. (Touch press was wired in Plan 10 Phase 0; this test makes that wiring observable.)

### Task 8: Integration test — hand-pinch activates the attractor (feature-gated)

**File:** `crates/wc-sketches/tests/line_input.rs`

- [ ] **Step 1: Write the failing test**

Append:

```rust
#[cfg(feature = "hand-tracking-gestures")]
#[test]
fn hand_pinch_activates_mouse_attractor() {
    use wc_core::input::hand::{Chirality, Hand, LandmarkIndex, LANDMARK_COUNT};
    use wc_core::input::state::{HandTrackingFrame, HandTrackingState};
    use wc_sketches::line::systems::mouse::{PINCH_PRESS_THRESHOLD, MOUSE_POWER_PRESS};
    use bevy::math::Vec3;
    use std::time::Duration;

    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    // Construct a right hand at NDC (0, 0) with pinch above threshold.
    // `pointer_merge_system` projects NDC → window-space center.
    let mut landmarks = [Vec3::ZERO; LANDMARK_COUNT];
    // Index-finger tip at NDC origin → window center.
    landmarks[LandmarkIndex::IndexTip as usize] = Vec3::ZERO;
    let hand = Hand {
        id: 1,
        chirality: Chirality::Right,
        palm_position: Vec3::ZERO,
        palm_normal: Vec3::Y,
        palm_velocity: Vec3::ZERO,
        pinch_strength: PINCH_PRESS_THRESHOLD + 0.05,
        grab_strength: 0.0,
        landmarks,
    };

    let frame = HandTrackingFrame {
        hands: smallvec::smallvec![hand],
        timestamp: Duration::from_millis(0),
    };
    // `HandTrackingState::ingest` is `pub(crate)` in production. Plan 11
    // promotes it to `pub` to make tests like this writable without a
    // test-only ingest helper — see the matching `state.rs` change in Task 5
    // Step 2 (the `pub` visibility bump on `ingest`).
    app.world_mut()
        .resource_mut::<HandTrackingState>()
        .ingest(&frame);

    // First update: pinch is above threshold and last_pinch.right_pinched
    // was false → rising edge → power = PRESS.
    app.update();

    let post = app
        .world()
        .resource::<MouseAttractorState>()
        .power;
    assert!(
        (post - MOUSE_POWER_PRESS).abs() < 1e-3,
        "expected power={MOUSE_POWER_PRESS} after hand pinch edge; got {post}"
    );
}
```

(`HandTrackingState::ingest` is currently `pub(crate)`. Task 5 Step 2 promotes it to `pub` — add `pub fn ingest(...)` in place of `pub(crate) fn ingest(...)` at `crates/wc-core/src/input/state.rs:75`. The rustdoc comment about "not part of the public API" gets updated to "callable by tests; production write path is `update_hand_tracking_state`.")

- [ ] **Step 2: Run with the feature flag enabled**

```bash
cargo test -p wc-sketches --test line_input --features hand-tracking-gestures hand_pinch_activates_mouse_attractor
```

Expected: PASS.

- [ ] **Step 3: Run without the feature; confirm test is excluded**

```bash
cargo test -p wc-sketches --test line_input hand_pinch_activates_mouse_attractor
```

Expected: no matching test (the `#[cfg(...)]` excludes it). The default build path stays clean.

### Task 9: Confirm carry-forward #45 is already resolved

The `line_input.rs` header note (line 5) says Plan 8 Phase 0 wired `pointer_merge_system` into `sketches_test_app` so synthetic events flow end-to-end without the `seed_pointer` resource-poke shortcut. That means carry-forward #45 has already shipped (under a different commit than the carry-forward expected).

- [ ] **Step 1: Verify the fix is live**

```bash
grep -n "pointer_merge_system\|seed_pointer" crates/wc-sketches/tests/common/mod.rs \
                                              crates/wc-core/tests/common/mod.rs \
                                              crates/wc-core/tests/common/*.rs
```

Expected: `pointer_merge_system` is registered in `sketches_test_app`; no `seed_pointer` helper remains.

- [ ] **Step 2: Mark #45 resolved in the carry-forwards doc**

In `docs/superpowers/next-plan-carry-forwards.md`, replace item #45's body with:

```
45. RESOLVED 2026-05-25 (Plan 11 Phase B audit): Plan 8 Phase 0 already wired
    `pointer_merge_system` into `sketches_test_app`. `seed_pointer` is gone;
    synthetic CursorMoved events flow end-to-end. The fix shipped under a
    different commit than this carry-forward originally expected.
```

(Same close-as-already-done treatment as #58/#59. Keep the line for triage history.)

- [ ] **Step 3: No code change**

### Task 10: Commit Phase B

- [ ] **Step 1: Verify everything passes**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p wc-sketches --all-targets --features hand-tracking-gestures -- -D warnings
cargo test --workspace
cargo test -p wc-sketches --features hand-tracking-gestures
```

All must pass.

- [ ] **Step 2: Commit**

```bash
git add crates/wc-core/Cargo.toml \
        crates/wc-core/src/input/state.rs \
        crates/wc-sketches/Cargo.toml \
        crates/wc-sketches/src/line/systems/mouse.rs \
        crates/wc-sketches/src/line/mod.rs \
        crates/wc-sketches/tests/line_input.rs

git commit -m "$(cat <<'EOF'
Plan 11 Phase B: touch + hand-tracking attractor activation

`update_mouse_attractor` now consumes mouse, touch, and (under the new
`hand-tracking-gestures` feature) tracked-hand pinch. All three reach
the attractor's `just_pressed` rising edge via the same path, matching
v4's `pointerdown` semantics across input modalities.

Hand-tracking lives behind a feature flag because `HandTrackingState`
has no provider writer yet (carry-forward #61 — provider lands in Plan
12+). With the flag off, the consumer is compiled out; with it on, the
new `LastPinchState` resource tracks per-chirality edges so a held
pinch doesn't re-trigger every frame.

New integration tests confirm touch press and hand pinch both transition
`MouseAttractorState.power` from 0 → MOUSE_POWER_PRESS end-to-end.

Closes carry-forwards #30, #45 (audit), #60, #61.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase C — `rfd` file picker + per-field serde defaults

Two related settings concerns: (1) `spawn_template` is rendered as a free-text input (`ty = Text`), so kiosk operators have to type absolute paths — a poor UX. (2) `LineSettings` lacks `#[serde(default)]` per field, so when a future field is added to the struct (as `gamma` was in Plan 8), legacy persisted TOML without the new key silently fails the whole-section deserialize and reverts every sibling to default.

### Task 11: Add `rfd` as a workspace dependency

**File:** `Cargo.toml` (workspace)

- [ ] **Step 1: Add the dep**

In `[workspace.dependencies]`, near the GUI deps:

```toml
# Native file picker (Plan 11 Phase C): cross-platform Browse… dialog for
# the Line sketch's `spawn_template` setting. Targets macOS NSOpenPanel,
# Windows IFileDialog, GTK on Linux. Web build is out of scope (rfd has a
# wasm backend but it returns a promise; would require a different code
# path).
rfd = "0.15"
```

- [ ] **Step 2: Add to `wc-core` Cargo.toml**

**File:** `crates/wc-core/Cargo.toml`

In `[dependencies]`:

```toml
# Native file dialog for `FilePath` settings (Plan 11 Phase C).
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
rfd = { workspace = true }
```

(If there's already a `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` block, append to it.)

- [ ] **Step 3: Build**

```bash
cargo build -p wc-core
```

Expected: PASS.

### Task 12: Add `SettingKind::FilePath` to the runtime

**File:** `crates/wc-core/src/settings/def.rs`

- [ ] **Step 1: Add the variant**

In the `SettingKind` enum, after `Text`:

```rust
/// Filesystem path stored as a UTF-8 `String`. Rendered as a text-edit
/// plus a Browse… button that opens [`rfd::FileDialog`]. The `extensions`
/// list filters the dialog; an empty slice allows any file.
FilePath {
    /// Extensions to filter the picker on (e.g., `&["png", "jpg"]`).
    /// Empty means no filter.
    extensions: &'static [&'static str],
},
```

- [ ] **Step 2: Build wc-core**

```bash
cargo build -p wc-core
```

Expected: a compile error in `panel_user.rs` and possibly `panel_dev.rs` for the non-exhaustive match in `render_widget`. That's the lever pulling Task 13 next.

### Task 13: Render the `FilePath` widget

**File:** `crates/wc-core/src/settings/panel_user.rs`

- [ ] **Step 1: Write the failing test (panel widget unit test)**

Append to the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test for the FilePath widget: confirms the dispatch table
    /// recognises the variant. Rendering itself requires an egui context, so
    /// we don't exercise the visual side here.
    #[test]
    fn file_path_kind_dispatches() {
        let def = SettingDef {
            field_name: "path",
            label: "Path",
            category: SettingsCategory::User,
            kind: SettingKind::FilePath {
                extensions: &["png"],
            },
            requires_restart: false,
        };
        // We can't test rendering without an egui context; the assertion is
        // that the kind variant has the expected extensions slice.
        match def.kind {
            SettingKind::FilePath { extensions } => {
                assert_eq!(extensions, &["png"]);
            }
            _ => panic!("expected FilePath kind"),
        }
    }
}
```

- [ ] **Step 2: Run it; confirm it compiles after the rendering branch is added**

```bash
cargo test -p wc-core --lib settings::panel_user::tests::file_path_kind_dispatches
```

Expected initially: compile error in `render_widget`. Fix it in Step 3.

- [ ] **Step 3: Add the dispatch branch**

In `render_widget` (around line 170 in `panel_user.rs`), update the `match &def.kind`:

```rust
match &def.kind {
    SettingKind::Number(range) => render_number(field, def.label, range, ui),
    SettingKind::Boolean => render_bool(field, def.label, ui),
    SettingKind::Color => render_color(field, def.label, ui),
    SettingKind::Text => render_text(field, def.label, ui),
    SettingKind::FilePath { extensions } => {
        render_file_path(field, def.label, extensions, ui)
    }
}
```

- [ ] **Step 4: Implement `render_file_path`**

Append to `panel_user.rs`, after `render_text`:

```rust
/// Render a filesystem-path field as a text edit plus a Browse… button.
/// On Browse, opens [`rfd::FileDialog`] filtered to `extensions`; the
/// selected path replaces the field value. Available only on native
/// platforms — the wasm build renders a text-edit only (no picker).
fn render_file_path(
    field: &mut dyn bevy::reflect::PartialReflect,
    label: &str,
    #[cfg_attr(target_arch = "wasm32", allow(unused_variables))] extensions: &[&str],
    ui: &mut egui::Ui,
) {
    let Some(v) = field.try_downcast_mut::<String>() else {
        ui.label(format!("(expected String for {label})"));
        return;
    };
    ui.horizontal(|ui| {
        ui.label(label);
        ui.text_edit_singleline(v);
        #[cfg(not(target_arch = "wasm32"))]
        if ui.button("Browse…").clicked() {
            let mut dlg = rfd::FileDialog::new();
            if !extensions.is_empty() {
                dlg = dlg.add_filter("Image", extensions);
            }
            if let Some(path) = dlg.pick_file() {
                *v = path.to_string_lossy().into_owned();
            }
        }
    });
}
```

- [ ] **Step 5: Mirror the change in `panel_dev.rs`**

**File:** `crates/wc-core/src/settings/panel_dev.rs`

Add the same `FilePath` arm to its `render_widget` (the dev panel renders all categories, not just `User`, so the variant is reachable there too). Copy `render_file_path` to that file *or* hoist both panels' helpers into a shared `panel_widgets.rs` if that fits a cleaner refactor. For Plan 11's scope: copy the function inline is fine — DRY can wait for Plan 12+ when both panels grow more widgets.

- [ ] **Step 6: Run tests and clippy**

```bash
cargo test -p wc-core
cargo clippy --workspace --all-targets -- -D warnings
```

All must pass.

### Task 14: Add `ty = FilePath` and `extensions = [...]` to the derive macro

**File:** `crates/wc-core-macros/src/lib.rs`

- [ ] **Step 1: Extend the `Kind` enum**

Find the `enum Kind` (around line 79):

```rust
#[derive(Clone, Copy)]
enum Kind {
    Number,
    Boolean,
    Color,
    Text,
    FilePath,
}
```

- [ ] **Step 2: Add `extensions` to `FieldInfo`**

In `struct FieldInfo` (around line 86):

```rust
struct FieldInfo {
    ident: Ident,
    default: Option<Expr>,
    label: Option<String>,
    category: Category,
    requires_restart: bool,
    kind: Kind,
    min: Option<Expr>,
    max: Option<Expr>,
    step: Option<Expr>,
    /// File extensions for `Kind::FilePath`. None for other kinds.
    extensions: Option<Vec<String>>,
}
```

Update the default initializer in `parse_fields` (the `let mut info = FieldInfo { ... }` block):

```rust
let mut info = FieldInfo {
    ident,
    default: None,
    label: None,
    category: Category::Dev,
    requires_restart: false,
    kind: Kind::Number,
    min: None,
    max: None,
    step: None,
    extensions: None,
};
```

- [ ] **Step 3: Extend the `ty` parser**

In the `attr.parse_nested_meta` block that handles `ty`:

```rust
} else if meta.path.is_ident("ty") {
    let ident: Ident = meta.value()?.parse()?;
    info.kind = match ident.to_string().as_str() {
        "Number" => Kind::Number,
        "Boolean" => Kind::Boolean,
        "Color" => Kind::Color,
        "Text" => Kind::Text,
        "FilePath" => Kind::FilePath,
        other => {
            return Err(meta.error(format!(
                "unknown ty `{other}` (expected `Number`, `Boolean`, `Color`, `Text`, or `FilePath`)"
            )))
        }
    };
}
```

- [ ] **Step 4: Parse the `extensions = [...]` attribute**

Add after the `step` branch in the same `parse_nested_meta`:

```rust
} else if meta.path.is_ident("extensions") {
    // `extensions = ["png", "jpg"]` — array of string literals.
    let value = meta.value()?;
    let arr: syn::ExprArray = value.parse()?;
    let mut exts: Vec<String> = Vec::with_capacity(arr.elems.len());
    for elem in arr.elems {
        if let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) = elem
        {
            exts.push(s.value());
        } else {
            return Err(syn::Error::new_spanned(
                elem,
                "`extensions` must be an array of string literals",
            ));
        }
    }
    info.extensions = Some(exts);
}
```

- [ ] **Step 5: Emit the `FilePath` kind**

In `emit_trait_impl`'s `kind_tokens` match (around line 244):

```rust
Kind::FilePath => {
    let exts = f.extensions.as_deref().unwrap_or(&[]);
    let ext_lits = exts.iter().map(|s| s.as_str());
    quote! {
        ::wc_core::settings::SettingKind::FilePath {
            extensions: &[ #( #ext_lits, )* ],
        }
    }
}
```

- [ ] **Step 6: Build the macro and a downstream consumer**

```bash
cargo build -p wc-core-macros
cargo build -p wc-sketches
```

Both must succeed.

### Task 15: Switch `spawn_template` to `FilePath`

**File:** `crates/wc-sketches/src/line/settings.rs`

- [ ] **Step 1: Update the attribute**

Find:

```rust
#[setting(default = String::new(), ty = Text, category = User, requires_restart)]
pub spawn_template: String,
```

Replace with:

```rust
#[setting(default = String::new(), ty = FilePath, extensions = ["png", "jpg", "jpeg", "webp"], category = User, requires_restart)]
pub spawn_template: String,
```

- [ ] **Step 2: Build, test**

```bash
cargo build -p wc-sketches
cargo test -p wc-sketches
```

Both must pass.

### Task 16: Add per-field `#[serde(default)]` to `LineSettings`

This solves the silent-data-loss when a future field is added (the failing-section behavior described in carry-forward #57).

**File:** `crates/wc-sketches/src/line/settings.rs`

- [ ] **Step 1: Write the failing test**

Append to `settings.rs` at the bottom (before `EOF`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Confirms that legacy persisted TOML missing one field still
    /// deserializes the other fields cleanly. Without per-field
    /// `#[serde(default)]`, missing-field would fail the whole section
    /// and revert every sibling to default — Plan 8's `gamma` addition
    /// would have done exactly that to existing user files.
    #[test]
    fn missing_field_preserves_sibling_values() {
        // TOML written by a hypothetical v5-line build (pre-`gamma`).
        let legacy = r#"
            particle_density = 7.5
            gravity_constant = 320.0
            spawn_template = ""
        "#;
        let parsed: LineSettings = toml::from_str(legacy).expect("legacy TOML must parse");
        assert!(
            (parsed.particle_density - 7.5).abs() < 1e-6,
            "particle_density not preserved"
        );
        assert!(
            (parsed.gravity_constant - 320.0).abs() < 1e-6,
            "gravity_constant not preserved"
        );
        // Missing `gamma` should fall back to default (1.0), not zero out
        // particle_density too.
        assert!((parsed.gamma - 1.0).abs() < 1e-6, "gamma not default");
    }
}
```

- [ ] **Step 2: Run; confirm failure**

```bash
cargo test -p wc-sketches --lib line::settings::tests::missing_field_preserves_sibling_values
```

Expected: FAIL with a TOML deserialize error (missing `gamma` field). The fix follows.

- [ ] **Step 3: Add `#[serde(default)]` to every field**

```rust
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "line")]
pub struct LineSettings {
    #[setting(
        default = 10.0_f32,
        min = 0.1_f32,
        max = 30.0_f32,
        step = 0.5_f32,
        category = User,
        requires_restart
    )]
    #[serde(default = "default_particle_density")]
    pub particle_density: f32,

    #[setting(default = 280.0_f32, min = 0.0_f32, max = 1000.0_f32, step = 10.0_f32, category = User)]
    #[serde(default = "default_gravity_constant")]
    pub gravity_constant: f32,

    #[setting(default = 1.0_f32, min = 0.1_f32, max = 4.0_f32, step = 0.1_f32, category = User)]
    #[serde(default = "default_gamma")]
    pub gamma: f32,

    #[setting(default = String::new(), ty = FilePath, extensions = ["png", "jpg", "jpeg", "webp"], category = User, requires_restart)]
    #[serde(default)]
    pub spawn_template: String,
}

// `#[serde(default = "fn_name")]` requires a free function rather than an
// inline expression. These mirror the `#[setting(default = ...)]` values
// above; if the slider defaults change, update both sites.
fn default_particle_density() -> f32 {
    10.0
}
fn default_gravity_constant() -> f32 {
    280.0
}
fn default_gamma() -> f32 {
    1.0
}
```

- [ ] **Step 4: Update the module rustdoc**

The existing serde forward-compat note (lines 11-17) mentions ignoring *unknown* fields. Append a paragraph about *missing* fields:

```
//! ### Missing-field forward-compat
//!
//! Each field carries `#[serde(default = "default_<name>")]` so a legacy
//! persisted TOML written before a new field was added still deserializes
//! cleanly: the missing field falls back to its default, and the sibling
//! fields are preserved. Without per-field defaults, missing one key
//! would fail the whole-section deserialize and silently revert *every*
//! sibling to default (the bug surfaced when Plan 8 added `gamma`).
//!
//! Apply the same pattern to every settings struct: when adding a field
//! mid-cycle, also add a `default_<name>()` free function and the
//! `#[serde(default = "...")]` attribute.
```

- [ ] **Step 5: Run the test; confirm pass**

```bash
cargo test -p wc-sketches --lib line::settings::tests::missing_field_preserves_sibling_values
```

Expected: PASS.

- [ ] **Step 6: Commit Phase C**

```bash
git add Cargo.toml \
        crates/wc-core/Cargo.toml \
        crates/wc-core/src/settings/def.rs \
        crates/wc-core/src/settings/panel_user.rs \
        crates/wc-core/src/settings/panel_dev.rs \
        crates/wc-core-macros/src/lib.rs \
        crates/wc-sketches/src/line/settings.rs

git commit -m "$(cat <<'EOF'
Plan 11 Phase C: rfd file picker + per-field serde defaults

Two settings concerns: (1) `spawn_template` was a free-text input —
typing absolute paths is a poor kiosk UX. New `SettingKind::FilePath
{ extensions }` variant + `rfd::FileDialog` Browse… button. (2)
`LineSettings` lacked per-field `#[serde(default)]`, so a future field
addition would silently revert sibling values on legacy TOML.

`rfd = "0.15"` added to the workspace, gated `cfg(not(target_arch =
"wasm32"))` — wasm wants a different code path. The derive macro's
attribute grammar gains `ty = FilePath` and `extensions = [...]`.

Closes carry-forwards #57, #62.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase D — Heatmap-spawn end-to-end verification

Plan 10 Phase A's heatmap-image spawn is unit-tested at the math layer but no integration test drives `spawn_line` with a real PNG. Carry-forward #63 calls this out. The Madison manual run will exercise it visually; Plan 11 also adds an automated test so future regressions are caught.

### Task 17: Add a heatmap-spawn E2E test

**File:** `crates/wc-sketches/tests/line_heatmap_e2e.rs` (new)

- [ ] **Step 1: Write the test**

```rust
//! End-to-end test for the heatmap-image spawn path.
//!
//! Drives `spawn_line` with a real PNG path (`assets/sketches/line/star.png`)
//! and confirms particle positions follow the image's luminance × alpha
//! distribution. Also exercises the fallback path with a deliberately wrong
//! path.
//!
//! Carry-forward #63.

#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

mod common;
use common::input::tap_key;
use common::sketches_test_app;

use bevy::input::keyboard::KeyCode;
use bevy::prelude::*;
use wc_core::lifecycle::state::AppState;
use wc_sketches::line::settings::LineSettings;
use wc_sketches::line::sim_cpu::LineCpuMirror;

/// Mirror of `line_input.rs::enter_line` — three updates suffice (one fold,
/// one nav handler, one OnEnter). Inlined here rather than imported because
/// `line_input.rs` is a sibling test target, not a library module.
fn enter_line(app: &mut App) {
    tap_key(app, KeyCode::Digit1);
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line,
        "Digit1 keyboard nav should enter AppState::Line",
    );
}

fn app_with_template(template: &str) -> App {
    let mut app = sketches_test_app();
    {
        // `LineSettings` is registered by `LinePlugin::build`; set the
        // template *before* entering Line so `spawn_line` reads the override.
        let mut settings = app.world_mut().resource_mut::<LineSettings>();
        settings.spawn_template = template.to_string();
    }
    app.update();
    enter_line(&mut app);
    app
}

/// star.png as an absolute path resolved at compile time. `cargo test` for
/// integration tests sets CWD to the crate root (`crates/wc-sketches/`), but
/// `image::open` (called inside `heatmap::sample_from_heatmap`) uses
/// `std::fs`, which doesn't auto-prepend CARGO_MANIFEST_DIR — so we build the
/// absolute path explicitly. Mirrors `LINE_BACKGROUND_PATH` in
/// `crates/waveconductor/src/main.rs`.
const STAR_PNG_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/sketches/line/star.png"
);

#[test]
fn heatmap_spawn_clusters_particles_near_bright_pixels() {
    // star.png is a 64x64 soft-diamond glow with luminance peaking at the
    // center. We expect particle X positions to cluster around the canvas
    // center (and not be uniformly distributed like the fallback layout).
    let app = app_with_template(STAR_PNG_PATH);

    let mirror = app.world().resource::<LineCpuMirror>();
    let particles = &mirror.particles;
    assert!(
        particles.len() >= 100,
        "expected ≥100 particles; got {}",
        particles.len()
    );

    // Compute X-coordinate mean and stddev. For a uniformly-spread
    // horizontal layout, X stddev would approximate the canvas width / sqrt(12)
    // ≈ width / 3.46. For a center-clustered heatmap from a soft-diamond
    // sprite, stddev should be substantially smaller.
    let mean_x: f32 = particles.iter().map(|p| p.position[0]).sum::<f32>()
        / particles.len() as f32;
    let var_x: f32 = particles
        .iter()
        .map(|p| (p.position[0] - mean_x).powi(2))
        .sum::<f32>()
        / particles.len() as f32;
    let stddev_x = var_x.sqrt();

    // Mean should be near 0 (centered window-space) — star.png is symmetric.
    assert!(
        mean_x.abs() < 50.0,
        "expected mean_x near 0; got {mean_x} (suggests offset bias)"
    );
    // A 1280-wide canvas with uniform distribution → stddev ≈ 370.
    // The star sprite's center cluster should drive stddev well below that.
    let win_w = 1280.0_f32;
    let uniform_stddev = win_w / 3.46;
    assert!(
        stddev_x < uniform_stddev * 0.75,
        "stddev_x={stddev_x} suggests uniform layout (uniform≈{uniform_stddev}); \
         heatmap should cluster particles toward the center"
    );
}

#[test]
fn missing_template_falls_back_to_horizontal_layout() {
    let app = app_with_template("/this/path/does/not/exist.png");
    let mirror = app.world().resource::<LineCpuMirror>();
    let particles = &mirror.particles;
    assert!(!particles.is_empty(), "fallback must still produce particles");

    // Fallback layout: Y stays near mid-Y (== 0 in window-centered world);
    // sawtooth jitter is ±4px. If we got the heatmap path or a different
    // fallback, Y would spread further.
    for p in particles {
        assert!(
            p.position[1].abs() <= 4.0 + 0.001,
            "fallback Y {} not near 0±4 (got heatmap or wrong layout?)",
            p.position[1]
        );
    }
}
```

> The local `enter_line` helper above inlines the nav-key + 3-update pattern from `line_input.rs::enter_line` rather than extracting it to `tests/common/mod.rs`, because each `crates/wc-sketches/tests/*.rs` is a separate integration-test binary — they don't share private helpers, only `mod common` modules. Hoisting `enter_line` into `tests/common/mod.rs` is a Plan 12+ refactor once a third file needs it.

- [ ] **Step 2: Add `star.png` (and a fallback PNG if needed) to the test asset path**

Check if `assets/sketches/line/star.png` exists:

```bash
ls assets/sketches/line/
```

If yes (it should — Plan 8 added it), nothing to do. If not, copy from v4's `src/materials/starMaterial/star.png` to the same destination:

```bash
git show main:src/materials/starMaterial/star.png > assets/sketches/line/star.png
```

- [ ] **Step 3: Run the test**

```bash
cargo test -p wc-sketches --test line_heatmap_e2e
```

Expected: both pass. If the cluster test is flaky (random sampling), increase the tolerance — the assertion is "clustered, not uniform," not a specific number.

- [ ] **Step 4: Commit Phase D**

```bash
git add crates/wc-sketches/tests/line_heatmap_e2e.rs

git commit -m "$(cat <<'EOF'
Plan 11 Phase D: heatmap-spawn end-to-end verification

Plan 10 Phase A's heatmap sampler was unit-tested at the math layer but
no integration test drove `spawn_line` with a real PNG. New
`line_heatmap_e2e` test loads `assets/sketches/line/star.png` and
asserts particle X-stddev is substantially below the uniform-layout
expectation (cluster signal); a companion test confirms a bogus path
falls back to the horizontal-line layout without panicking.

Closes carry-forward #63.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase E — Manual side-by-side parity capture + PARITY.md sign-off

The human-in-the-loop step. Madison drives both apps and signs `PARITY.md`. The agent's role: prepare the runner, write the verdict text once the capture is in hand, and commit.

### Task 18: Prepare both runners

- [ ] **Step 1: Verify v5 boots from current HEAD**

```bash
cargo run -p waveconductor --release
```

Expected: the home screen appears, Tab+Return enters Line, click+drag activates the attractor and shows visibly spinning rings (Phase A's payoff). Quit cleanly.

- [ ] **Step 2: Verify v4 is reachable**

```bash
git rev-parse main
```

The v4 codebase lives at the repo root on `main`. The Plan 10 PARITY.md verdict pinned v4 reference commit as `3b85676` ("Bump version to 4.2.0"). Confirm:

```bash
git rev-parse main | head -c 7
```

Should print `3b85676` (or whatever HEAD of `main` currently is — re-pin if it's moved).

- [ ] **Step 3: Print instructions for Madison**

The agent stops here and asks Madison to perform the side-by-side capture. Suggested message:

> Phase E ready. To complete the parity verdict:
>
> 1. In a separate terminal, clone or check out `main` at `<commit hash>`. From that working tree, run `npm install && npm run dev`. v4 boots on `http://localhost:5173/` (or whatever Vite's default is).
> 2. In this terminal: `cargo run -p waveconductor --release`. v5 boots in a native window.
> 3. Resize both to 1280×720 (or as close as you can).
> 4. Capture three matched states from each side:
>    - **Idle** — no input, particles settled.
>    - **Mid-press at center** — left button held at canvas center, smear + spinning rings visible.
>    - **Mid-decay** — release, capture ~5s after release while power is still decaying.
> 5. Save the six screenshots somewhere you can attach to the verdict (or just compare visually — the verdict is text, not embedded images).
> 6. Tell me PASS / NEEDS-TUNING / FAIL with a one-line reason, plus the v4 commit hash you used. I'll fill in the PARITY.md verdict and commit.

### Task 19: Update PARITY.md with the signed verdict

Once Madison reports back, fill in the verdict.

**File:** `crates/wc-sketches/src/line/PARITY.md`

- [ ] **Step 1: Replace the Verdict section**

Find:
```markdown
## Verdict

**Status:** PENDING — verdict deferred to Plan 11.
```

Replace through to the end-of-file with (substituting the actual values Madison reports):

```markdown
## Verdict

**Status:** PASS — signed 2026-05-25 by Madison.

**Reference v4:** `main` branch at commit `<madison's reported hash>` (`<title from that commit>`).

**Reference v5:** `rewrite/bevy` branch at commit `<HEAD before tag>` — tagged as `v5-line-parity`.

**Captured states:**

1. **Idle** — particles settle to the spawn layout (horizontal-line + sawtooth jitter when `spawn_template` is empty; heatmap distribution when set). v5 matches v4's idle frame perceptually.
2. **Mid-press at center** — gravity-smear ray-march produces concentric chromatic rings emanating from the cursor. Attractor visual: 10 nested polygonal rings (Plan 11 § A), color `#C5E2CC`, group scale = √power/5, per-ring rotation `(10−i)/20 · power`. v5's rings are visibly spinning (the parity gap Phase A closed). The smear's color separation and concentric ring spacing track v4 within the perceptual tolerance.
3. **Mid-decay (~5s after release)** — power decays geometrically (floor + (P−floor)·0.9 per frame). Idle veto (`line_idle_veto`) keeps the sketch `Active` until power < floor+ε. Audio reactivity tracks `groupedUpness` back down through silence. v5's decay envelope and audio tail match v4.

**Approved deviations** (carried from prior phases):
- Polygonal ring mesh (6 segments) replaces v4's `RingGeometry(15, 18, 32)` with 0.8 rad X-axis tilt. v4's 3D-tilt approach is unavailable in this 2D port; the polygon's corner-anchored Z-rotation is the perceptual substitute. See Plan 11 § A.
- CPU mirror integration drift ≤1% over long timescales (documented as an approved deviation since Plan 7).

**Known remaining parity deviations after Plan 11:** none expected. The CPU mirror drift is the only documented divergence and is imperceptible at the festival-loop horizon.
```

- [ ] **Step 2: Commit Phase E**

```bash
git add crates/wc-sketches/src/line/PARITY.md

git commit -m "$(cat <<'EOF'
Plan 11 Phase E: sign PARITY.md verdict — PASS

Side-by-side capture against v4 `main` at <hash> at 1280×720, three
states (idle / mid-press / mid-decay). All three match v5 within
perceptual tolerance. The Phase A polygonal-ring substitution for v4's
3D-tilted annulus is documented as an approved deviation.

Closes the Line parity loop. Plan 12+ moves to the next sketch.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase F — Push, watch CI, tag `v5-line-parity`

### Task 20: Final gates

- [ ] **Step 1: Format + clippy + tests**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p wc-sketches --all-targets --features hand-tracking-gestures -- -D warnings
cargo test --workspace
cargo test -p wc-sketches --features hand-tracking-gestures
```

All five must pass.

- [ ] **Step 2: Run the heatmap E2E test specifically (one more time, in release)**

```bash
cargo test --release -p wc-sketches --test line_heatmap_e2e
```

Expected: PASS. Worth running in release because random-sampling tolerance can shift between dev and release optimizations.

- [ ] **Step 3: Push**

```bash
git push origin rewrite/bevy
```

- [ ] **Step 4: Wait for CI**

```bash
gh pr checks  # if a PR exists; otherwise watch the branch CI
# or
gh run list --branch rewrite/bevy --limit 3
```

Wait for green. If anything fails, fix and push fixup commits — *not* `--amend` (per global git safety rules).

- [ ] **Step 5: Tag and push**

```bash
git tag v5-line-parity
git push origin v5-line-parity
```

### Task 21: Update the roadmap

**File:** `docs/superpowers/roadmap.md`

- [ ] **Step 1: Flip Plan 10 and Plan 11 status rows**

The status table currently reads:

```
| 10 | Line polish + heatmap spawn + soak harness | 🟡 shipped, parity gaps deferred to Plan 11 | — |
| 11 | Line parity completion (rings, touch/hand activation, file picker, sign-off) | ⏳ next | `v5-line-parity` |
```

Replace with:

```
| 10 | Line polish + heatmap spawn + soak harness | ✅ shipped (gaps closed in Plan 11) | — |
| 11 | Line parity completion (rings, touch/hand activation, file picker, sign-off) | ✅ shipped | `v5-line-parity` |
| 12 | Next sketch (Flame / Dots / Cymatics / Waves — order TBD) | ⏳ next | — |
```

- [ ] **Step 2: Update the prose paragraph below the table**

The current paragraph ends "...Plan 11 closes those and earns the `v5-line-parity` tag." Change to past tense and append a celebratory sentence:

```
> **Line is done.** Plans 7–11 carried the sketch from scaffolding through multi-attractor physics, the gravity-smear post-process, the fundsp synthesis graph, the audio↔visual reactivity coupling, the heatmap-image spawn template, the 8-hour soak harness, and the four parity gaps Plan 11 closed (visible ring rotation, touch + hand-tracking activation, native file picker, per-field serde defaults). The architectural pattern established here — per-sketch plugin under `wc-sketches`, settings via the `wc-core` registry, `OnEnter`/`OnExit` lifecycle, audio reactivity via `AudioCommand`, a `PARITY.md` per module closing with a tagged verdict — generalizes cleanly to Flame, Dots, Cymatics, and Waves (Plan 12+).
```

- [ ] **Step 3: Commit the roadmap update**

```bash
git add docs/superpowers/roadmap.md

git commit -m "$(cat <<'EOF'
roadmap: Plan 11 shipped (v5-line-parity); Plan 12 (next sketch) is next

Line parity loop closed. Plans 7–11 stack:
- Plan 7: simulation parity + idle veto
- Plan 8: rendering parity (gravity smear, sprite, rings)
- Plan 9: audio + reactivity coupling
- Plan 10: heatmap spawn + 8-hour soak harness
- Plan 11: visible rings, touch + hand-tracking activation, file picker,
          serde defaults, manual sign-off — tag `v5-line-parity`

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"

git push origin rewrite/bevy
```

---

## Self-review checklist

After all phases complete:

- [ ] Attractor rings visibly spin when the attractor is active (Phase A verified by Madison in the manual run).
- [ ] Touch press activates the attractor end-to-end (Phase B test passes).
- [ ] Hand pinch activates the attractor with the `hand-tracking-gestures` feature on; compiles and tests cleanly with the feature off (Phase B).
- [ ] `spawn_template` setting has a working Browse… button that filters to image extensions (Phase C visually verified).
- [ ] `LineSettings` deserializes legacy TOML missing one field without zeroing siblings (Phase C test).
- [ ] Heatmap spawn drives particles toward bright pixels with `assets/sketches/line/star.png`; bogus paths fall back to horizontal layout (Phase D).
- [ ] `PARITY.md` verdict reads PASS with the actual v4 commit hash and three captured states (Phase E).
- [ ] `v5-line-parity` tag exists on `rewrite/bevy` HEAD (Phase F).
- [ ] Roadmap shows Plan 11 ✅ and Plan 12 ⏳ (Phase F).

## Carry-forwards for Plan 12+

- #44, #47, #48, #49 (test-infra reminders).
- #53, #54 (render-graph: post-process gating outside `AppState::Line`, per-frame uniform buffer reuse).
- #55 (visual gravity-smear verification — closed if Madison signs off in Phase E).
- Hand-tracking provider implementation (Plan 12+). The `hand-tracking-gestures` feature flag stays off in the default build until then. When the Leap / MediaPipe provider lands, flip the flag default to `on`.

## Execution handoff

Two execution options: **subagent-driven (recommended)** or **inline**. Subagent-driven works well for Plan 11 because each phase is independent enough that fresh-context subagents can land each one cleanly with a two-stage review checkpoint between phases.

Phase E is the operator-blocking phase — the controller agent should report status and wait for Madison's PASS / NEEDS-TUNING / FAIL signal before proceeding to Phase F.
