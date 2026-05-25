# Plan 8: Line Rendering Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** v5 Line *looks like* v4 Line. After this plan, particles render as soft star sprites (not flat quads), attractors show as concentric rotating rings while active, and the rendered scene runs through a gravity-smear post-process that produces the signature concentric trail + chromatic-aberration look of v4 Line. The audio-reactive modulations of the post-process uniforms (`G`, `iMouseFactor`) are wired with temporary host-side constants; Plan 9 will replace them with particle-stats coupling.

**Architecture:** Three independent visual systems land on the existing Phase 7 simulation:

1. **Star sprite path** — load `assets/sketches/line/star.png` (ported from v4 `src/materials/starMaterial/star.png`) as a Bevy `Image`. Extend `LineMaterial` with a texture binding. The render fragment shader samples the texture by the quad corner UV and modulates by particle `alpha`.
2. **Attractor ring mesh entities** — for each `MouseAttractorState.power > 0` (and future Leap attractors), spawn a child entity per ring (10 concentric annuli) under the sketch's `LineRoot`. A new `attractor_visuals` system updates per-frame: rotation by `(10 - idx) / 20 * power`, group scale by `sqrt(power) / 5`. Despawn rings when power returns to zero.
3. **Gravity-smear post-process** — a new render-graph node `LinePostProcessNode` runs after `Core2d::MainTransparentPass`. Reads the scene texture as input, dispatches a fullscreen-triangle fragment shader that does the 11-iteration UV-warp + chromatic-factor accumulation + gamma curve, writes to the swap chain. Uniforms (`G`, `iMouseFactor`, `gamma`, `iMouse`, `iResolution`, `iGlobalTime`) come from a new `LinePostParams` resource that `update_post_params` writes each frame.

**Tech Stack:** Bevy 0.18.1, existing `bytemuck` / `wc-core-macros` / WGSL pipeline from Plans 6–7. Star sprite uses `bevy::image::Image` + `bevy::render::texture::ImagePlugin` (already present via `DefaultPlugins`).

**Reference spec:** `docs/superpowers/specs/2026-05-22-bevy-rewrite-design.md` §5.1 (asset layout), §5.6 (WebGPU-only), §8 (perceptual parity).

**Reference v4 sources** (read-only via `git show main:<path>`):
- `src/sketches/line/shaders/gravity/fragment.glsl` — the post-process to port
- `src/sketches/line/shaders/gravity/vertex.glsl`, `shader.ts`, `index.ts` — uniform shape
- `src/materials/starMaterial/index.ts`, `star.png` — the sprite
- `src/particles/attractor.ts` — the 10-ring visual (radius 15→18, 0xC5E2CC, scale = `sqrt(power)/5`, per-ring rotation = `(10 - idx) / 20 * power`)
- `src/sketches/line/index.ts` — how v4 wires uniforms into the shader per frame

**Branch:** `rewrite/bevy`. Pre-flight: verify HEAD is at or after `v5-test-harness` (`09b46d2`).

---

## Scope check

Plan 8 is the second of four Line parity plans. It is visual only; particle physics already match v4 (Plan 7). The deliverable: side-by-side at a fixed input, v5 Line is visually within perceptual-parity tolerance of v4 — modulo audio-reactive modulation of the gravity-smear `G` uniform, which is Plan 9.

Five phases, five commits, Phase E pushes and tags `v5-line-render`.

## File map

**New files:**

- `assets/sketches/line/star.png` — ported from v4 `src/materials/starMaterial/star.png` (64×64 RGBA). Lives under `assets/sketches/line/` (not `assets/shaders/line/`) since it's a texture, not a shader.
- `crates/wc-sketches/src/line/post_process.rs` — `LinePostProcessPlugin`, `LinePostParams` resource, render-graph node, pipeline cache, bind group setup.
- `crates/wc-sketches/src/line/attractor_visuals.rs` — `AttractorVisual` marker component, `spawn_attractor_rings`, `update_attractor_rings` systems, `despawn_attractor_rings_when_inactive`.
- `assets/shaders/line/gravity.wgsl` — WGSL port of v4 `fragment.glsl` (gravity-smear post-process).

**Modified files:**

- `crates/wc-sketches/src/line/material.rs` — add `#[texture(1)]` + `#[sampler(2)]` for the star texture; update vertex/fragment of `render.wgsl` to sample it.
- `assets/shaders/line/render.wgsl` — sample texture by corner UV, premultiply by `alpha`.
- `crates/wc-sketches/src/line/settings.rs` — add `gamma: f32` (default 1.0, range 0.1–4.0, step 0.1, User category).
- `crates/wc-sketches/src/line/systems/spawn.rs` — load `star.png` via `AssetServer`, attach handle to `LineMaterial`.
- `crates/wc-sketches/src/line/systems/sim_params.rs` — also populate `LinePostParams.iMouse` (the cursor world position) and `iGlobalTime`.
- `crates/wc-sketches/src/line/mod.rs` — register the new sub-modules, plugin assembly.
- `crates/wc-core/src/input/pointer.rs` (Phase 0) — close the test-fidelity gap from Plan 7.5 (carry-forward #45) by extending `pointer_merge_system` to consume `CursorMoved` messages directly when no hand source is active.
- `crates/wc-sketches/tests/common/mod.rs` (Phase 0) — install the merge system in `sketches_test_app` if not already pulled in.
- `crates/wc-sketches/tests/line_input.rs` (Phase 0) — drop `seed_pointer`; use `move_pointer` from `common::input`.
- `crates/wc-sketches/src/line/PARITY.md` (Phase D) — reflect the rendering progression.

---

## Conventions

- All paths absolute from the repo root.
- Code blocks show full new content or full added section.
- Cargo commands list expected outcomes.
- Each phase ends with one explicit commit.
- Bevy 0.18 deviations from plan literals (4 hit during Plan 7, 2 during Plan 7.5) — adapt and note in phase report.

---

# Phase 0 — Plan 7/7.5 carry-forwards

Six targeted items from `docs/superpowers/next-plan-carry-forwards.md`. One commit.

### Task 1: Close the `seed_pointer` test-fidelity gap (carry-forwards #45, #46)

Synthesized `CursorMoved` events in tests don't reach `PointerState` because the production `pointer_merge_system` reads `window.cursor_position()`. Extend it to also consume `CursorMoved` messages as a mouse-input source.

- [ ] **Step 1: Audit current behavior**

Read `crates/wc-core/src/input/pointer.rs`. Find `pointer_merge_system` (~line 84). Document the current cursor-source priority (Hand > Mouse). The Mouse branch reads `window.cursor_position()`; that's what fails in tests because winit isn't running.

- [ ] **Step 2: Extend the Mouse branch to also drain `CursorMoved`**

In `pointer_merge_system`, before reading `window.cursor_position()`, drain any pending `CursorMoved` messages and use the latest one's `position` if present. The merge stays Hand-first; if no hand source is active, the latest `CursorMoved` position wins; if neither is present, fall back to `window.cursor_position()`. Production behavior is unchanged (in production the Window's cursor position is updated by winit *and* `CursorMoved` events fire, so the latest of the two is correct).

```rust
// Drain pending CursorMoved messages and use the latest, if any.
let cursor_msg_position: Option<Vec2> = cursor_moved_reader
    .read()
    .last()
    .map(|c| c.position);

// Mouse source resolves in this priority:
// 1) latest CursorMoved message this tick (works in tests too)
// 2) Window::cursor_position (production winit fallback)
let mouse_position = cursor_msg_position
    .or_else(|| window.cursor_position());
```

(Adjust to match the function's actual signature; add `cursor_moved_reader: MessageReader<'_, '_, bevy::window::CursorMoved>` as a parameter.)

- [ ] **Step 3: Drop `seed_pointer` from `line_input.rs`**

Replace `seed_pointer(&mut app, 640.0, 360.0)` calls with `move_pointer(&mut app, 640.0, 360.0, Vec2::ZERO)` from `common::input`. Remove the `seed_pointer` helper function.

- [ ] **Step 4: Update `move_pointer` rustdoc**

In `crates/wc-core/tests/common/input.rs`, update `move_pointer`'s `///` block to read accurately:

```rust
/// Move the pointer to `(x, y)` in window pixel coordinates. Writes
/// `CursorMoved` (which Plan 7.5 wired into `pointer_merge_system`'s mouse
/// source path) and `MouseMotion` (for idle-detection). `from` supplies
/// the previous position so the motion delta is correct; pass
/// `Vec2::ZERO` if unknown.
```

- [ ] **Step 5: Tests**

Run: `cargo test --workspace`
Expected: pass, including line_input's 5 tests using the new `move_pointer` path.

### Task 2: `const SIM_PARAMS_SIZE: NonZeroU64` (carry-forward #11)

**File:** `crates/wc-sketches/src/line/compute.rs`

Replace the runtime `NonZeroU64::new(...).expect(...)` with a `const` so the assertion runs at compile time and the `#[allow(clippy::expect_used)]` drops.

- [ ] **Step 1: Hoist to const**

Near the top of `compute.rs` (after the imports), add:

```rust
/// Compile-time validated `SimParams` size for the uniform bind-group entry.
///
/// `SimParams` is non-zero-sized by definition (it has fields). The
/// `unwrap()` is inside a `const` block, so any future change that made it
/// zero-sized would fail at compile time, not at runtime.
const SIM_PARAMS_SIZE: NonZeroU64 = match NonZeroU64::new(
    std::mem::size_of::<super::particle::SimParams>() as u64,
) {
    Some(n) => n,
    None => panic!("SimParams must be non-zero-sized"),
};
```

In `init_line_pipeline`, replace the `let sim_params_size = ...;` block with a reference to `SIM_PARAMS_SIZE`. Delete the `#[allow(clippy::expect_used, ...)]` block.

- [ ] **Step 2: Build**

Run: `cargo build -p wc-sketches`
Expected: clean.

### Task 3: Brittleness of `update_sim_params_writes_mouse_attractor_with_gravity_scaling` (carry-forward #32)

**File:** `crates/wc-sketches/tests/line_lifecycle.rs`

Promote the hardcoded post-decay power value to a named const computed from the same v4 constants the production code uses.

- [ ] **Step 1: Replace the magic number**

Find `update_sim_params_writes_mouse_attractor_with_gravity_scaling`. Replace the inline numeric assertion with:

```rust
use wc_sketches::line::systems::{MOUSE_POWER_DECAY, MOUSE_POWER_FLOOR, MOUSE_POWER_PRESS};

const EXPECTED_POST_DECAY_POWER: f32 =
    MOUSE_POWER_FLOOR + (MOUSE_POWER_PRESS - MOUSE_POWER_FLOOR) * MOUSE_POWER_DECAY;
```

Use `EXPECTED_POST_DECAY_POWER * settings.gravity_constant` in the assertion. The const lives at module scope (inside the test file).

- [ ] **Step 2: Run the test**

Run: `cargo test -p wc-sketches --test line_lifecycle update_sim_params_writes_mouse_attractor_with_gravity_scaling`
Expected: pass.

### Task 4: Structured trace tags on `LineComputeNode` (carry-forward #14)

**File:** `crates/wc-sketches/src/line/compute.rs`

Change the `tracing::trace!` calls from string-prefixed to structured-tag form.

- [ ] **Step 1: Restructure the trace calls**

In `LineComputeNode::run`, replace each `tracing::trace!("LineComputeNode: ...")` with:

```rust
tracing::trace!(node = "LineComputeNode", "no bind group");
tracing::trace!(node = "LineComputeNode", "no LinePipeline resource");
tracing::trace!(node = "LineComputeNode", "pipeline still compiling");
```

- [ ] **Step 2: Build**

Run: `cargo build -p wc-sketches`
Expected: clean.

### Task 5: Group veto-aware tests (carry-forward #40)

**File:** `crates/wc-sketches/tests/line_lifecycle.rs`

Move `idle_veto_keeps_line_active_during_attractor_decay` to sit directly after `update_sim_params_does_not_run_when_idle`. Two veto-aware tests, one logical group.

- [ ] **Step 1: Reposition the test**

Cut-paste `idle_veto_keeps_line_active_during_attractor_decay` immediately after `update_sim_params_does_not_run_when_idle`. No code changes — just reorder.

- [ ] **Step 2: Run tests**

Run: `cargo test -p wc-sketches --test line_lifecycle`
Expected: 7 tests pass (same as before, in different order).

### Task 6: Hoist `use wc_core::lifecycle::RegisterIdleVetoExt;` (carry-forward #41)

**File:** `crates/wc-sketches/src/line/mod.rs`

The import currently sits inside `LinePlugin::build`. Move it to the file's top `use` block for consistency.

- [ ] **Step 1: Move the import**

Find `use wc_core::lifecycle::RegisterIdleVetoExt;` inside `LinePlugin::build`. Cut it and paste next to the existing `use wc_core::lifecycle::...` at the top of the file.

- [ ] **Step 2: Build**

Run: `cargo build -p wc-sketches`
Expected: clean.

### Task 7: Commit Phase 0

- [ ] **Step 1: Stage**

```bash
git add \
    crates/wc-core/src/input/pointer.rs \
    crates/wc-core/tests/common/input.rs \
    crates/wc-sketches/tests/line_input.rs \
    crates/wc-sketches/src/line/compute.rs \
    crates/wc-sketches/src/line/mod.rs \
    crates/wc-sketches/tests/line_lifecycle.rs
```

- [ ] **Step 2: Commit**

```bash
git commit -m "$(cat <<'EOF'
Plan 8 Phase 0: Plan 7/7.5 carry-forwards

Closes the seed_pointer test-fidelity gap (#45, #46) by extending
pointer_merge_system to also consume CursorMoved messages in its
mouse-source branch — production behavior is unchanged because winit
fires both window.cursor_position writes and CursorMoved events.
Tests now use move_pointer end-to-end.

Also: const SIM_PARAMS_SIZE replaces the runtime expect, post-decay
power becomes a named const in the lifecycle test, LineComputeNode
trace calls take structured tags, the two veto-aware tests neighbor,
and the RegisterIdleVetoExt import hoists to file top.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase A — Star sprite

Replace flat-quad particle rendering with textured point sprite.

### Task 8: Port `star.png`

**File:** `assets/sketches/line/star.png` (new)

- [ ] **Step 1: Copy from v4**

Run from the repo root:

```bash
git show main:src/materials/starMaterial/star.png > assets/sketches/line/star.png
file assets/sketches/line/star.png
```

Expected: PNG 64×64 RGBA. The file is binary — `git add` will stage it as-is.

### Task 9: Extend `LineMaterial`

**File:** `crates/wc-sketches/src/line/material.rs`

- [ ] **Step 1: Add texture + sampler bindings**

Modify `LineMaterial` to include the star texture:

```rust
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct LineMaterial {
    /// Particle storage buffer, read-only from the vertex shader.
    #[storage(0, read_only)]
    pub particles: Handle<ShaderStorageBuffer>,
    /// Star sprite texture sampled in the fragment shader.
    #[texture(1)]
    #[sampler(2)]
    pub star_texture: Handle<Image>,
}
```

Imports: add `use bevy::image::Image;` (or `bevy::prelude::Image` if available).

### Task 10: Update `render.wgsl`

**File:** `assets/shaders/line/render.wgsl`

- [ ] **Step 1: Replace fragment + add sampler bindings**

```wgsl
#import bevy_sprite::mesh2d_view_bindings::view

struct Particle {
    position: vec2<f32>,
    velocity: vec2<f32>,
    original_xy: vec2<f32>,
    alpha: f32,
    _pad: f32,
};

@group(2) @binding(0) var<storage, read> particles: array<Particle>;
@group(2) @binding(1) var star_texture: texture_2d<f32>;
@group(2) @binding(2) var star_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) brightness: f32,
    @location(2) alpha: f32,
};

// Quad half-size in world units. v4 uses 13px screen-space sprites; here we
// match perceptually using a constant world-space size — Plan 10 will tune.
const QUAD_HALF: f32 = 8.0;

// Six vertex positions of a triangle list quad and their UVs in [0, 1].
struct Corner {
    pos: vec2<f32>,
    uv:  vec2<f32>,
};
fn quad_corner(corner: u32) -> Corner {
    var c: Corner;
    switch corner {
        case 0u: { c.pos = vec2<f32>(-QUAD_HALF, -QUAD_HALF); c.uv = vec2<f32>(0.0, 1.0); }
        case 1u: { c.pos = vec2<f32>( QUAD_HALF, -QUAD_HALF); c.uv = vec2<f32>(1.0, 1.0); }
        case 2u: { c.pos = vec2<f32>( QUAD_HALF,  QUAD_HALF); c.uv = vec2<f32>(1.0, 0.0); }
        case 3u: { c.pos = vec2<f32>(-QUAD_HALF, -QUAD_HALF); c.uv = vec2<f32>(0.0, 1.0); }
        case 4u: { c.pos = vec2<f32>( QUAD_HALF,  QUAD_HALF); c.uv = vec2<f32>(1.0, 0.0); }
        default: { c.pos = vec2<f32>(-QUAD_HALF,  QUAD_HALF); c.uv = vec2<f32>(0.0, 0.0); }
    }
    return c;
}

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @location(0) local_pos: vec3<f32>,
) -> VertexOutput {
    let particle_index = vertex_index / 6u;
    let corner_index   = vertex_index % 6u;

    let p = particles[particle_index];
    let c = quad_corner(corner_index);
    let world_pos = vec4<f32>(p.position + c.pos, 0.0, 1.0);

    var out: VertexOutput;
    out.clip_position = view.clip_from_world * world_pos;
    out.uv = c.uv;
    out.brightness = clamp(length(p.velocity) * 0.005, 0.05, 1.0);
    out.alpha = p.alpha;
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(star_texture, star_sampler, in.uv);
    let b = in.brightness;
    let color = vec3<f32>(b, b * 0.85, b * 0.6);
    // Star texture alpha modulates particle alpha for soft point sprites.
    return vec4<f32>(color * texel.rgb, texel.a * in.alpha);
}
```

### Task 11: Load and bind the texture in `spawn_line`

**File:** `crates/wc-sketches/src/line/systems/spawn.rs`

- [ ] **Step 1: Load the texture**

Add `asset_server: Res<'_, AssetServer>` to the system signature. In the body, after `let count = ...;` and before the `Particle` loop:

```rust
let star_texture: Handle<Image> = asset_server.load("sketches/line/star.png");
```

Pass it into the material:

```rust
let material_handle = materials.add(LineMaterial {
    particles: particles_handle.clone(),
    star_texture,
});
```

Import: `use bevy::image::Image;` (or whatever path the installed Bevy 0.18 exports it from — possibly `bevy::prelude::Image`).

- [ ] **Step 2: Run + visual**

Run: `cargo run -p waveconductor`
Expected: window opens; particles render as soft star sprites (visible texture) instead of flat-color quads. Click and drag — same physics, new appearance.

### Task 12: Commit Phase A

- [ ] **Step 1: Stage**

```bash
git add \
    assets/sketches/line/star.png \
    assets/shaders/line/render.wgsl \
    crates/wc-sketches/src/line/material.rs \
    crates/wc-sketches/src/line/systems/spawn.rs
```

- [ ] **Step 2: Commit**

```bash
git commit -m "$(cat <<'EOF'
Plan 8 Phase A: star sprite particle rendering

Ports v4's starMaterial/star.png (64×64 RGBA soft-diamond sprite)
into assets/sketches/line/. LineMaterial gains a #[texture(1)] +
#[sampler(2)] binding; the render.wgsl fragment samples the texture
by corner UV and modulates rgb by velocity-brightness and alpha by
particle.alpha. Particles now render as soft star points instead of
flat quads.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase B — Attractor ring meshes

10 concentric annuli per active attractor, rotating + scaling by power.

### Task 13: `attractor_visuals` module

**File:** `crates/wc-sketches/src/line/attractor_visuals.rs` (new)

```rust
//! Visual ring meshes for active attractors.
//!
//! For each attractor with `power > 0`, spawn 10 concentric annulus mesh
//! entities. Per-frame rotate each ring (speed ∝ power, varies by ring
//! index) and scale the group by `sqrt(power) / 5` — matching v4's
//! `Attractor.animate()` (src/particles/attractor.ts).
//!
//! Ring base geometry: inner radius 15, outer radius 18 world units.
//! Color: v4 `#C5E2CC` = `Color::srgb(0.77, 0.886, 0.8)`.

use bevy::color::Color;
use bevy::math::primitives::Annulus;
use bevy::prelude::*;
use bevy::sprite_render::ColorMaterial;

use super::LineRoot;
use super::systems::MouseAttractorState;

/// Marker on the parent entity that holds all 10 ring children for an attractor.
#[derive(Component)]
pub struct AttractorVisual;

/// Marker on each individual ring child (carries its ring index 0..=9).
#[derive(Component)]
pub struct AttractorRing(pub u32);

/// v4 ring colour `#C5E2CC` linearly. Stored once; mesh material uses this.
pub const ATTRACTOR_RING_COLOR: Color = Color::srgb(0.77, 0.886, 0.8);

const NUM_RINGS: u32 = 10;
const RING_INNER_RADIUS: f32 = 15.0;
const RING_OUTER_RADIUS: f32 = 18.0;

/// Spawn the 10-ring visual for the mouse attractor when its power becomes
/// positive (and no visual already exists).
pub fn spawn_attractor_visual(
    mut commands: Commands<'_, '_>,
    mouse: Res<'_, MouseAttractorState>,
    visuals: Query<'_, '_, Entity, With<AttractorVisual>>,
    mut meshes: ResMut<'_, Assets<Mesh>>,
    mut materials: ResMut<'_, Assets<ColorMaterial>>,
    line_root: Query<'_, '_, Entity, With<LineRoot>>,
) {
    if mouse.power <= 0.0 || !visuals.is_empty() {
        return;
    }
    let Some(root) = line_root.iter().next() else {
        return;
    };

    let mesh_handle = meshes.add(Mesh::from(Annulus::new(RING_INNER_RADIUS, RING_OUTER_RADIUS)));
    let material_handle = materials.add(ColorMaterial::from(ATTRACTOR_RING_COLOR));

    let parent = commands
        .spawn((
            AttractorVisual,
            Transform::from_translation(Vec3::new(mouse.position[0], mouse.position[1], -1.0)),
            GlobalTransform::default(),
            Visibility::Visible,
        ))
        .set_parent(root)
        .id();

    for i in 0..NUM_RINGS {
        #[allow(clippy::cast_precision_loss, reason = "i ∈ 0..=9; f32 lossless")]
        let scale = 1.0 + (i as f32 / 10.0).powi(2) * 2.0;
        commands
            .spawn((
                AttractorRing(i),
                bevy::mesh::Mesh2d(mesh_handle.clone()),
                bevy::sprite_render::MeshMaterial2d(material_handle.clone()),
                Transform::from_scale(Vec3::splat(scale)),
                GlobalTransform::default(),
                Visibility::default(),
            ))
            .set_parent(parent);
    }
}

/// Animate the rings: per-frame rotation speed scales with attractor power
/// and ring index; the group's scale tracks `sqrt(power)/5`.
pub fn animate_attractor_visual(
    time: Res<'_, Time>,
    mouse: Res<'_, MouseAttractorState>,
    mut visuals: Query<'_, '_, (&mut Transform,), (With<AttractorVisual>, Without<AttractorRing>)>,
    mut rings: Query<'_, '_, (&AttractorRing, &mut Transform), With<AttractorRing>>,
) {
    let power = mouse.power;
    if power <= 0.0 {
        return;
    }
    let scale = power.sqrt() / 5.0;
    for (mut t,) in &mut visuals {
        t.translation.x = mouse.position[0];
        t.translation.y = mouse.position[1];
        t.scale = Vec3::splat(scale);
    }
    let dt = time.delta_secs();
    for (ring, mut t) in &mut rings {
        #[allow(clippy::cast_precision_loss, reason = "ring.0 ∈ 0..=9")]
        let speed = (10.0 - ring.0 as f32) / 20.0 * power;
        t.rotation = t.rotation * Quat::from_rotation_z(speed * dt);
    }
}

/// Despawn the ring visual when attractor power drops to zero.
pub fn despawn_attractor_visual(
    mut commands: Commands<'_, '_>,
    mouse: Res<'_, MouseAttractorState>,
    visuals: Query<'_, '_, Entity, With<AttractorVisual>>,
) {
    if mouse.power > 0.0 {
        return;
    }
    for entity in &visuals {
        commands.entity(entity).despawn();
    }
}
```

> **Bevy 0.18 deviations:** `Annulus` is in `bevy::math::primitives::Annulus`. `ColorMaterial`/`Mesh2d`/`MeshMaterial2d` paths may differ; consult `bevy::prelude` or the installed `bevy::sprite_render` re-exports. The implementer adapts.

### Task 14: Wire into `LinePlugin`

**File:** `crates/wc-sketches/src/line/mod.rs`

- [ ] **Step 1: Add the module + plugin systems**

Add `pub mod attractor_visuals;` to the module list. In `LinePlugin::build`, append to the gated update chain (after `step_cpu_mirror`):

```rust
                attractor_visuals::spawn_attractor_visual,
                attractor_visuals::animate_attractor_visual,
                attractor_visuals::despawn_attractor_visual,
```

### Task 15: Test the visual lifecycle

**File:** `crates/wc-sketches/tests/line_input.rs`

- [ ] **Step 1: Add a test**

Append:

```rust
#[test]
fn attractor_visual_spawns_on_press_and_despawns_on_release() {
    use wc_sketches::line::attractor_visuals::AttractorVisual;

    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);
    move_pointer(&mut app, 640.0, 360.0, Vec2::ZERO);
    app.update();

    let before = app.world_mut().query::<&AttractorVisual>().iter(app.world()).count();
    assert_eq!(before, 0, "no visual before press");

    press_left(&mut app);
    app.update();
    app.update();
    let after_press = app.world_mut().query::<&AttractorVisual>().iter(app.world()).count();
    assert_eq!(after_press, 1, "one visual after press");

    release_left(&mut app);
    // Power decays geometrically; need enough frames to reach < floor + ε.
    for _ in 0..30 {
        app.update();
    }
    let after_decay = app.world_mut().query::<&AttractorVisual>().iter(app.world()).count();
    assert_eq!(after_decay, 0, "visual despawned after power reaches zero");
}
```

### Task 16: Commit Phase B

- [ ] **Step 1: Stage**

```bash
git add \
    crates/wc-sketches/src/line/attractor_visuals.rs \
    crates/wc-sketches/src/line/mod.rs \
    crates/wc-sketches/tests/line_input.rs
```

- [ ] **Step 2: Commit**

```bash
git commit -m "$(cat <<'EOF'
Plan 8 Phase B: attractor ring mesh visuals

For each MouseAttractorState.power > 0, spawn 10 concentric annulus
mesh children under the LineRoot at the attractor world position.
animate_attractor_visual rotates each ring per frame (speed ∝
power × (10 - ring_index)/20) and scales the group by sqrt(power)/5,
matching v4's Attractor.animate(). Despawn when power returns to
zero. New integration test asserts spawn-on-press / despawn-on-decay.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase C — Gravity-smear post-process

The signature visual element. WGSL port of v4's `fragment.glsl`.

### Task 17: Port the fragment shader

**File:** `assets/shaders/line/gravity.wgsl` (new)

```wgsl
// Gravity-smear post-process — WGSL port of v4 src/sketches/line/shaders/gravity/fragment.glsl.
//
// Reads the scene texture (Core2d main pass output) and produces a
// chromatic-smeared output by ray-marching 11 steps of gravity-distorted
// UV samples, accumulating per-step color shifts to produce the signature
// concentric-trail look.
//
// Uniforms come from LinePostParams; the input scene texture is bound at
// @group(0) @binding(2) (sampler at @binding(3)).

struct PostParams {
    iResolution: vec2<f32>,
    iMouse: vec2<f32>,
    iMouseFactor: f32,
    iGlobalTime: f32,
    g_constant: f32,
    gamma: f32,
};

@group(0) @binding(0) var<uniform> params: PostParams;
@group(0) @binding(1) var scene_texture: texture_2d<f32>;
@group(0) @binding(2) var scene_sampler: sampler;

const GRAVITY_EPSILON: f32 = 1e-4;
const NUM_STEPS: u32 = 11u;

// Precomputed `0.8 / (i + 6 + sqrt(i+1))` for i in 0..11, matching v4's
// INTENSITY_SCALARS table (line/shaders/gravity/fragment.glsl).
const INTENSITY_SCALARS = array<f32, 11>(
    0.114285714, 0.095077216, 0.082202612, 0.072727273,
    0.065380480, 0.059481810, 0.054623350, 0.050541977,
    0.047058824, 0.044047339, 0.041415103,
);

fn gravity(p: vec2<f32>, attraction_center: vec2<f32>, g: f32) -> vec2<f32> {
    let delta = attraction_center - p;
    let dist_sq = max(dot(delta, delta), GRAVITY_EPSILON);
    return delta * (g / dist_sq);
}

fn smear(uv_pixels: vec2<f32>, attraction_center: vec2<f32>) -> vec4<f32> {
    var incoming_p = uv_pixels;
    var outgoing_p = uv_pixels;
    var color = vec4<f32>(0.0);

    // Chromatic shift factors. v4: outgoing = (0.96, 1.0, 1.0/0.96, 1.0);
    //                            incoming = (1.0/0.96, 1.0, 0.96, 1.0).
    let outgoing_factor = vec4<f32>(0.96, 1.0, 1.0 / 0.96, 1.0);
    let incoming_factor = vec4<f32>(1.0 / 0.96, 1.0, 0.96, 1.0);

    let v_mouse_pull = (params.iMouse - uv_pixels) * params.iMouseFactor;

    var v_incoming_accum = incoming_factor;
    var v_outgoing_accum = outgoing_factor;

    for (var i: u32 = 0u; i < NUM_STEPS; i = i + 1u) {
        incoming_p = incoming_p - gravity(incoming_p, attraction_center, params.g_constant);
        outgoing_p = outgoing_p + gravity(outgoing_p, attraction_center, params.g_constant);

        incoming_p = incoming_p - v_mouse_pull;
        outgoing_p = outgoing_p + v_mouse_pull;

        let intensity = INTENSITY_SCALARS[i];

        let in_uv  = incoming_p / params.iResolution;
        let out_uv = outgoing_p / params.iResolution;

        color = color + textureSample(scene_texture, scene_sampler, in_uv)
                     * intensity * v_incoming_accum;
        color = color + textureSample(scene_texture, scene_sampler, out_uv)
                     * intensity * v_outgoing_accum;

        v_incoming_accum = v_incoming_accum * incoming_factor;
        v_outgoing_accum = v_outgoing_accum * outgoing_factor;
    }
    return color;
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Fullscreen triangle: three vertices that cover the screen with UV mapping.
@vertex
fn vertex(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var uv = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var out: VertexOutput;
    out.clip_position = vec4<f32>(pos[idx], 0.0, 1.0);
    out.uv = uv[idx];
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv_pixels = in.uv * params.iResolution;
    let base = textureSample(scene_texture, scene_sampler, in.uv);
    let smeared = smear(uv_pixels, params.iResolution / 2.0);
    let combined = base + smeared;
    // Per-channel gamma curve.
    return vec4<f32>(
        pow(combined.r, params.gamma),
        pow(combined.g, params.gamma),
        pow(combined.b, params.gamma),
        combined.a,
    );
}
```

### Task 18: `LinePostProcessPlugin`, `LinePostParams`, render-graph node

**File:** `crates/wc-sketches/src/line/post_process.rs` (new)

This is the largest single file in the plan. It sets up:
- A `LinePostParams` resource (extracted to render world)
- A `PostProcessPipeline` cached in `PipelineCache`
- A `LinePostProcessNode` render-graph node that runs after `Core2d::MainTransparentPass`, samples the scene texture, dispatches the fragment shader to the swap chain target

The implementer reads the existing `crates/wc-sketches/src/line/compute.rs` for the Bevy 0.18 render-graph pattern. The shape:

```rust
//! Gravity-smear post-process pipeline for the Line sketch.
//!
//! ## Render graph
//!
//! `LinePostProcessNode` is inserted into the Core2d sub-graph immediately
//! after `Core2dNode::MainTransparentPass`. It reads the camera's view
//! target as input and writes to the same target's swap (Bevy's
//! `ViewTarget::post_process_write()` rotates between two textures so a
//! node can sample its own input).
//!
//! ## Uniforms
//!
//! `LinePostParams` is updated each frame on the main thread by
//! `update_post_params` (see `crates/wc-sketches/src/line/systems/sim_params.rs`).
//! `ExtractResourcePlugin` mirrors it into the render world.

use bevy::asset::Handle;
use bevy::core_pipeline::core_2d::graph::{Core2d, Node2d};
use bevy::ecs::system::lifetimeless::SRes;
use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_graph::{
    NodeRunError, RenderGraphApp, RenderGraphContext, RenderLabel, ViewNode, ViewNodeRunner,
};
use bevy::render::render_resource::{
    binding_types::{sampler, texture_2d, uniform_buffer},
    BindGroupEntries, BindGroupLayout, BindGroupLayoutEntries, BufferUsages, CachedRenderPipelineId,
    ColorTargetState, ColorWrites, FragmentState, MultisampleState, PipelineCache, PrimitiveState,
    RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages,
    TextureFormat, TextureSampleType, VertexState,
};
use bevy::render::renderer::{RenderContext, RenderDevice};
use bevy::render::texture::BevyDefault;
use bevy::render::view::ViewTarget;
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::shader::Shader;
use bytemuck::{Pod, Zeroable};

/// Uniform layout for the post-process shader. Mirrors `struct PostParams`
/// in `assets/shaders/line/gravity.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable, Resource, ExtractResource)]
pub struct LinePostParams {
    pub i_resolution: [f32; 2],
    pub i_mouse: [f32; 2],
    pub i_mouse_factor: f32,
    pub i_global_time: f32,
    pub g_constant: f32,
    pub gamma: f32,
}

pub struct LinePostProcessPlugin;

impl Plugin for LinePostProcessPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LinePostParams>();
        app.add_plugins(ExtractResourcePlugin::<LinePostParams>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            .add_render_graph_node::<ViewNodeRunner<LinePostProcessNode>>(
                Core2d,
                LinePostProcessLabel,
            )
            .add_render_graph_edges(
                Core2d,
                (Node2d::MainTransparentPass, LinePostProcessLabel, Node2d::EndMainPass),
            );
    }

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.init_resource::<PostProcessPipeline>();
    }
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub struct LinePostProcessLabel;

#[derive(Resource)]
pub struct PostProcessPipeline {
    pub layout: BindGroupLayout,
    pub sampler: Sampler,
    pub pipeline_id: CachedRenderPipelineId,
}

impl FromWorld for PostProcessPipeline {
    fn from_world(world: &mut World) -> Self {
        let render_device = world.resource::<RenderDevice>();

        let layout = render_device.create_bind_group_layout(
            "line_post_layout",
            &BindGroupLayoutEntries::sequential(
                ShaderStages::FRAGMENT,
                (
                    uniform_buffer::<LinePostParams>(false),
                    texture_2d(TextureSampleType::Float { filterable: true }),
                    sampler(SamplerBindingType::Filtering),
                ),
            ),
        );

        let sampler = render_device.create_sampler(&SamplerDescriptor::default());

        let shader: Handle<Shader> = world
            .resource::<AssetServer>()
            .load("shaders/line/gravity.wgsl");

        let pipeline_id = world.resource_mut::<PipelineCache>().queue_render_pipeline(
            RenderPipelineDescriptor {
                label: Some("line_post_process_pipeline".into()),
                layout: vec![layout.clone()],
                push_constant_ranges: vec![],
                vertex: VertexState {
                    shader: shader.clone(),
                    shader_defs: vec![],
                    entry_point: "vertex".into(),
                    buffers: vec![],
                },
                fragment: Some(FragmentState {
                    shader,
                    shader_defs: vec![],
                    entry_point: "fragment".into(),
                    targets: vec![Some(ColorTargetState {
                        format: TextureFormat::bevy_default(),
                        blend: None,
                        write_mask: ColorWrites::ALL,
                    })],
                }),
                primitive: PrimitiveState::default(),
                depth_stencil: None,
                multisample: MultisampleState::default(),
            },
        );

        Self {
            layout,
            sampler,
            pipeline_id,
        }
    }
}

#[derive(Default)]
pub struct LinePostProcessNode;

impl ViewNode for LinePostProcessNode {
    type ViewQuery = &'static ViewTarget;

    fn run<'w>(
        &self,
        _graph: &mut RenderGraphContext<'_>,
        render_context: &mut RenderContext<'w>,
        view_target: bevy::ecs::query::QueryItem<'w, Self::ViewQuery>,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        let pipeline_res = world.resource::<PostProcessPipeline>();
        let pipeline_cache = world.resource::<PipelineCache>();
        let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline_res.pipeline_id) else {
            return Ok(());
        };
        let Some(post_params) = world.get_resource::<LinePostParams>() else {
            return Ok(());
        };

        // Upload uniforms.
        let uniform_buf = render_context
            .render_device()
            .create_buffer_with_data(&bevy::render::render_resource::BufferInitDescriptor {
                label: Some("line_post_params"),
                contents: bytemuck::bytes_of(post_params),
                usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            });

        let post_process_write = view_target.post_process_write();
        let bind_group = render_context.render_device().create_bind_group(
            "line_post_bind_group",
            &pipeline_res.layout,
            &BindGroupEntries::sequential((
                uniform_buf.as_entire_binding(),
                post_process_write.source,
                &pipeline_res.sampler,
            )),
        );

        let mut pass = render_context
            .command_encoder()
            .begin_render_pass(&bevy::render::render_resource::RenderPassDescriptor {
                label: Some("line_post_pass"),
                color_attachments: &[Some(bevy::render::render_resource::RenderPassColorAttachment {
                    view: post_process_write.destination,
                    resolve_target: None,
                    ops: bevy::render::render_resource::Operations {
                        load: bevy::render::render_resource::LoadOp::Load,
                        store: bevy::render::render_resource::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}
```

> **Bevy 0.18 deviation risk:** This file uses several render-graph types whose exact names may differ in 0.18.1 (`ViewNodeRunner`, `add_render_graph_node::<...>`, `post_process_write`, `BindGroupLayoutEntries::sequential`, `binding_types::*`). The implementer references Bevy's bundled `core_2d` source if needed and adapts. The shape ("ViewNode with a `ViewTarget` query, dispatch fullscreen triangle that samples scene texture, write back to target") is correct.

### Task 19: Update `update_sim_params` to populate `LinePostParams`

**File:** `crates/wc-sketches/src/line/systems/sim_params.rs`

- [ ] **Step 1: Add LinePostParams to the system signature**

Add `mut post: ResMut<'_, LinePostParams>,` parameter. Inside the function, after the existing `sim.params = ...` block, populate post params:

```rust
post.i_resolution = [w, h];
post.i_mouse = [mouse.position[0] + w * 0.5, h - (mouse.position[1] + h * 0.5)];
// World coords back to window-pixel coords for the post-process shader,
// which works in pixel space (matches v4 fragment.glsl).
post.i_mouse_factor = 1.0 / 15.0;
post.i_global_time = time.elapsed_secs();
post.g_constant = 5000.0; // Plan 9 will modulate this with groupedUpness × triangleWave
post.gamma = settings.gamma;
```

### Task 20: Wire the plugin

**File:** `crates/wc-sketches/src/line/mod.rs`

- [ ] **Step 1: Register**

Add `pub mod post_process;` to the module list. In `LinePlugin::build`, add `app.add_plugins(post_process::LinePostProcessPlugin);` after the existing plugin additions.

### Task 21: Commit Phase C

```bash
git add \
    assets/shaders/line/gravity.wgsl \
    crates/wc-sketches/src/line/post_process.rs \
    crates/wc-sketches/src/line/systems/sim_params.rs \
    crates/wc-sketches/src/line/mod.rs

git commit -m "$(cat <<'EOF'
Plan 8 Phase C: gravity-smear post-process

WGSL port of v4 src/sketches/line/shaders/gravity/fragment.glsl
samples the rendered scene 22 times across 11 iterations of gravity-
distorted UVs, accumulating per-step chromatic factors to produce
the signature concentric-trail + RGB-split look. LinePostProcessNode
inserts into the Core2d sub-graph after MainTransparentPass and
before EndMainPass; LinePostParams uniforms are populated each frame
by update_sim_params (G modulation comes in Plan 9).

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase D — `gamma` setting + PARITY.md update

### Task 22: Add `gamma` to `LineSettings`

**File:** `crates/wc-sketches/src/line/settings.rs`

- [ ] **Step 1: Append the field**

```rust
    /// Per-channel gamma curve applied as the final step of the gravity-smear
    /// post-process. v4 default = 1.0.
    #[setting(default = 1.0_f32, min = 0.1_f32, max = 4.0_f32, step = 0.1_f32, category = User)]
    pub gamma: f32,
```

### Task 23: Update `PARITY.md`

**File:** `crates/wc-sketches/src/line/PARITY.md`

Update the plan-progression section to mark Plan 8 shipped. The "Approved deviations" section can drop the "render uses vertex-index-driven quads" note since Plan 8 keeps that decision but the rendering character now matches v4 anyway.

### Task 24: Commit Phase D

```bash
git add \
    crates/wc-sketches/src/line/settings.rs \
    crates/wc-sketches/src/line/PARITY.md

git commit -m "$(cat <<'EOF'
Plan 8 Phase D: gamma setting + PARITY.md update

LineSettings.gamma (User, default 1.0, range 0.1-4.0, step 0.1)
plumbs into the gravity post-process shader's gamma uniform.
PARITY.md tracks the rendering-tier ship.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase E — Push, verify CI, tag `v5-line-render`

### Task 25: Local gates + push + tag

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `git push origin rewrite/bevy`
- [ ] Watch CI; all jobs green.
- [ ] `git tag v5-line-render <last commit sha>; git push origin v5-line-render`
- [ ] Update `docs/superpowers/roadmap.md`: Plan 8 → ✅ shipped; Plan 9 → ⏳ next. Commit + push.

---

## Self-review checklist

- [ ] `cargo run -p waveconductor` opens the sketch; particles render as star sprites; clicking shows ring meshes; post-process produces concentric trail + chromatic split.
- [ ] All tests pass (~+1 from Phase B).
- [ ] `cargo fmt`, `cargo clippy -D warnings`, `cargo doc -D warnings` all clean.
- [ ] No production code touched outside `wc-sketches/src/line/`, `wc-core/src/input/pointer.rs` (Phase 0), `crates/wc-core/tests/common/input.rs` (Phase 0).
- [ ] Five commits land on `rewrite/bevy`; tag `v5-line-render` points at the last code commit before the roadmap update.
- [ ] PARITY.md mentions Plan 8 shipped.

## Carry-forwards for Plan 9

*(populated during execution)*

## Execution handoff

Plan saved to `docs/superpowers/plans/2026-05-25-v5-plan-8-line-render.md`. Two execution options:

**1. Subagent-Driven (recommended)** — fresh implementer per phase, two-stage review.

**2. Inline Execution** — via `superpowers:executing-plans`.

Which approach?
