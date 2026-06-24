# Shared Hand-Mesh Overlay Module Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract the duplicated, diverged wireframe-hand overlay from `line/` and `dots/` into one shared `crates/wc-sketches/src/hand_mesh/` module both sketches consume, deleting the six per-sketch files and four per-sketch shaders, with no change to either sketch's rendered output.

**Architecture:** A shared `HandMeshPlugin { config }` (added once per sketch) owns the off-screen HDR bone camera + 20-bone reconciliation + per-frame transforms + resize/teardown, parameterized by a plain-data `HandMeshConfig` (bone colour, glow, radius, `AppState`). A single global `HandMeshCompositePlugin` (registered once by `SketchesPlugin`) owns the additive pre-bloom composite pipeline/node and a unified `HandPresence` gate that skips the composite when no hand is tracked. Migration is incremental and safe because the shared composite keys off a distinct `HandMeshTarget` type: an unmigrated sketch keeps its own target type, so the shared composite no-ops for it until that sketch is migrated.

**Tech Stack:** Rust, Bevy 0.19 (render sub-app, `Core2d`/`Core2dSystems::EarlyPostProcess`, `ExtractResourcePlugin`, `Material`/`MaterialPlugin`), WGSL, `cargo nextest`, `cargo xtask capture`.

## Global Constraints

- **Behavior-preserving:** `cargo xtask capture line` and `cargo xtask capture dots` must match current baselines. If a frame differs, stop and explain before re-baselining.
- **Colours exact:** Line `#add6b6`, Dots `#b0d8ff`; `glow_intensity = 5.0`; `bone_radius = 10.0`; icosphere subdivisions `1`; `BONE_COUNT = 20`.
- **Composite ordering exact:** the composite runs in `Core2dSystems::EarlyPostProcess` after each sketch's own post-process node.
- **One concept per file**, `mod.rs` is the module entry, shaders live under `assets/shaders/hand_mesh/` (never inlined in Rust). Public items get `///` rustdoc; module roots get `//!`.
- **No `unwrap()`/`expect()`** in non-test code unless a documented invariant.
- **No new dependencies.**
- **AGENTS.md gate set** before claiming done: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features --workspace -- -D warnings`, `cargo nextest run --workspace --all-features`, `cargo test --doc --workspace`, `cargo doc --no-deps --workspace --document-private-items`, `cargo deny check`, `cargo xtask check-secrets`.
- **Commit messages** end with `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`; use `git commit -F <file>` when the message contains backticks.

---

## File Structure

**New (shared module):**
- `crates/wc-sketches/src/hand_mesh/mod.rs` — overlay plugin (`HandMeshPlugin`, `HandMeshConfig`), bone camera + reconciliation + transform + resize + teardown + presence systems, shared markers + layer constants.
- `crates/wc-sketches/src/hand_mesh/bone_wireframe.rs` — `BoneWireframeMaterial` + `icosphere_line_mesh`.
- `crates/wc-sketches/src/hand_mesh/bone_composite.rs` — `HandMeshTarget`, `HandPresence`, `HandMeshCompositePlugin`, `HandMeshCompositeSet`, composite pipeline + node + render-world removal systems.
- `assets/shaders/hand_mesh/bone_wireframe.wgsl`, `assets/shaders/hand_mesh/bone_composite.wgsl`.

**Modified:**
- `crates/wc-sketches/src/lib.rs` — declare `pub mod hand_mesh;`, register the shared `MaterialPlugin` + `HandMeshCompositePlugin` once (gated by `WC_DEBUG_DISABLE_BONE_COMPOSITE`).
- `crates/wc-sketches/src/dots/mod.rs`, `crates/wc-sketches/src/line/mod.rs` — register `HandMeshPlugin { config }` + the composite ordering edge; drop the old plugin/system registrations.

**Deleted:**
- `crates/wc-sketches/src/{line,dots}/{hand_mesh,bone_composite,bone_wireframe}.rs`
- `assets/shaders/{line,dots}/{bone_wireframe,bone_composite}.wgsl`

---

## Task H1: Shared bone wireframe (material + mesh + shader)

Additive only — creates the shared module skeleton and the clean (near-identical) wireframe extract. Nothing is wired into any app yet; the per-sketch copies stay in place.

**Files:**
- Create: `crates/wc-sketches/src/hand_mesh/mod.rs`
- Create: `crates/wc-sketches/src/hand_mesh/bone_wireframe.rs`
- Create: `assets/shaders/hand_mesh/bone_wireframe.wgsl`
- Modify: `crates/wc-sketches/src/lib.rs:7-9` (module declarations)

**Interfaces:**
- Produces: `hand_mesh::bone_wireframe::BoneWireframeMaterial { color: LinearRgba }`, `hand_mesh::bone_wireframe::icosphere_line_mesh(radius: f32) -> Mesh`.

- [ ] **Step 1: Create the shared WGSL** at `assets/shaders/hand_mesh/bone_wireframe.wgsl` — an exact copy of `assets/shaders/line/bone_wireframe.wgsl` (read it first), with only the first comment line changed to:

```wgsl
// Unlit flat-color material for the shared hand-mesh wireframe bones.
```

(All WGSL code below the header is byte-identical to the Line copy — do not change it.)

- [ ] **Step 2: Create `hand_mesh/bone_wireframe.rs`** as a copy of `crates/wc-sketches/src/line/bone_wireframe.rs` (read it first) with these exact changes:
  - Module doc `//!` first line → `//! Metal-safe wireframe-bone material + geometry for the shared hand-mesh overlay.`
  - The shader-path constant:

```rust
/// Path to the bone wireframe WGSL (vertex + fragment), relative to the asset root.
const BONE_WIREFRAME_SHADER: &str = "shaders/hand_mesh/bone_wireframe.wgsl";
```

  - Keep `BoneWireframeMaterial`, `icosphere_line_mesh`, `BONE_ICO_SUBDIVISIONS`, and the `#[cfg(test)] mod tests` (`icosphere_line_mesh_is_line_list_with_paired_indices`) verbatim.

- [ ] **Step 3: Create `hand_mesh/mod.rs`** with just the module doc + the wireframe submodule for now (the overlay code lands in H2):

```rust
//! Shared wireframe-bone hand overlay for sketches.
//!
//! Extracts the off-screen HDR bone-camera + additive composite that Line and
//! Dots each forked. Each consumer registers [`HandMeshPlugin`] with a
//! [`HandMeshConfig`]; the global [`bone_composite::HandMeshCompositePlugin`]
//! (registered once by `SketchesPlugin`) owns the composite pipeline and node.
//!
//! See [`bone_wireframe`] for the Metal-safe LineList bone mesh + material.

pub mod bone_wireframe;

pub use bone_wireframe::{icosphere_line_mesh, BoneWireframeMaterial};
```

- [ ] **Step 4: Declare the module** — in `crates/wc-sketches/src/lib.rs`, add `pub mod hand_mesh;` to the module list (alphabetical, after `pub mod dots;`):

```rust
pub mod dots;
pub mod hand_mesh;
pub mod line;
pub mod particles;
```

- [ ] **Step 5: Run the shared wireframe test**

Run: `cargo nextest run -p wc-sketches hand_mesh::bone_wireframe`
Expected: PASS (`icosphere_line_mesh_is_line_list_with_paired_indices`).

- [ ] **Step 6: Build check**

Run: `cargo build -p wc-sketches`
Expected: compiles (the new module coexists with the per-sketch copies).

- [ ] **Step 7: Commit**

```bash
git add crates/wc-sketches/src/hand_mesh assets/shaders/hand_mesh/bone_wireframe.wgsl crates/wc-sketches/src/lib.rs
git commit -F <msg-file>   # "refactor(hand_mesh): shared bone-wireframe material + mesh + shader"
```

---

## Task H2: Shared overlay + composite (code only, unwired)

Creates the full shared overlay and composite plus the unified `HandPresence` gate. Still additive — `HandMeshPlugin` and `HandMeshCompositePlugin` are defined but not added to any app, so behavior is unchanged. Unit-tested headlessly.

**Files:**
- Modify: `crates/wc-sketches/src/hand_mesh/mod.rs`
- Create: `crates/wc-sketches/src/hand_mesh/bone_composite.rs`
- Create: `assets/shaders/hand_mesh/bone_composite.wgsl`

**Interfaces:**
- Consumes: `BoneWireframeMaterial`, `icosphere_line_mesh` (H1); `wc_core::input::entity::{BoneCenters, TrackedHand, BONE_COUNT}`, `wc_core::input::projection::palm_to_world`, `wc_core::lifecycle::state::AppState`, `wc_core::sketch::sketch_active`.
- Produces:
  - `hand_mesh::HandMeshConfig { app_state: AppState, bone_color: Color, glow_intensity: f32, bone_radius: f32 }` (Resource, Clone)
  - `hand_mesh::HandMeshPlugin { config: HandMeshConfig }`
  - `hand_mesh::{HandMeshCamera3d, BoneIndex, HAND_MESH_LAYER, HAND_MESH_LAYER_INDEX}`
  - `hand_mesh::bone_composite::{HandMeshTarget { image: Handle<Image> }, HandPresence(pub bool), HandMeshCompositePlugin, HandMeshCompositeSet, hand_mesh_composite}`

- [ ] **Step 1: Create the composite shader** at `assets/shaders/hand_mesh/bone_composite.wgsl` — an exact copy of `assets/shaders/line/bone_composite.wgsl` (read it first) with only the header comment block generalized (first line → `// Additive bone-glow composite for the shared hand-mesh overlay.`). The `@group/@binding` declarations, vertex, and fragment stages are byte-identical — do not change them.

- [ ] **Step 2: Create `hand_mesh/bone_composite.rs`.** Base it on `crates/wc-sketches/src/dots/bone_composite.rs` (read it first — it already has the presence gate), with these exact changes:
  - Define the render resources here (Line defined `HandMeshTarget` in its composite file; Dots defined `DotsBoneActive` in `hand_mesh.rs` — unify both here):

```rust
/// Off-screen render target the hand-mesh bones are rasterized into.
///
/// `Rgba16Float` so emissive bones (`> 1.0`) survive un-clamped. Inserted on
/// `OnEnter` by `super::spawn_hand_mesh_camera`, removed on exit;
/// [`ExtractResource`] mirrors it into the render world. When absent (every
/// non-overlay state) the composite node is a clean no-op.
#[derive(Resource, Clone, ExtractResource)]
pub struct HandMeshTarget {
    /// Handle to the off-screen HDR image, sized to the window's physical resolution.
    pub image: Handle<Image>,
}

/// Hand-presence gate for the bone camera and composite.
///
/// Set each frame by `super::update_hand_presence`: `true` when ≥1
/// [`wc_core::input::entity::TrackedHand`] exists. Extracted to the render world
/// so [`hand_mesh_composite`] can early-return before flipping the post-process
/// ping-pong when no hand is tracked — preventing stale-bone ghosting.
#[derive(Resource, Clone, Copy, ExtractResource)]
pub struct HandPresence(pub bool);

/// Render-system set the shared composite runs in. Each sketch orders this set
/// after its own post-process node via `configure_sets` (see `line`/`dots` mod).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct HandMeshCompositeSet;
```

  - Rename the plugin/pipeline/system/labels from `Dots*`/`dots_*` to the shared names: `HandMeshCompositePlugin`, `HandMeshCompositePipeline`, `hand_mesh_composite`, bind-group/pipeline/pass/layout labels `"hand_mesh_composite_*"`, layout name `"hand_mesh_composite_layout"`.
  - Shader path → `"shaders/hand_mesh/bone_composite.wgsl"`.
  - `HandMeshCompositePlugin::build` registers the node in the set:

```rust
impl Plugin for HandMeshCompositePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ExtractResourcePlugin::<HandMeshTarget>::default());
        app.add_plugins(ExtractResourcePlugin::<HandPresence>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        // EarlyPostProcess; each sketch orders this set after its own
        // post-process via `configure_sets(Core2d, HandMeshCompositeSet.after(..))`.
        render_app.add_systems(
            Core2d,
            hand_mesh_composite
                .in_set(Core2dSystems::EarlyPostProcess)
                .in_set(HandMeshCompositeSet),
        );
        render_app.add_systems(
            ExtractSchedule,
            (remove_hand_mesh_target_if_absent, remove_hand_presence_if_absent),
        );
    }

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.init_resource::<HandMeshCompositePipeline>();
    }
}
```

  - The `hand_mesh_composite` system body is Dots' `dots_bone_composite` verbatim except: parameter types use `HandMeshTarget`/`HandPresence`/`HandMeshCompositePipeline`, the labels are the shared ones, and the gate keeps the **unconditional** presence check (already present in Dots):

```rust
    let Some(target) = target else { return; };
    // No hands tracked → skip BEFORE post_process_write so the ping-pong is not
    // flipped (prevents stale-bone ghosting). Unconditional for every consumer.
    if !presence.is_some_and(|p| p.0) { return; }
```

  - Provide both render-world removal systems (generalize Dots' two), keeping the D3 rationale doc:

```rust
fn remove_hand_mesh_target_if_absent(
    mut commands: Commands<'_, '_>,
    main_resource: Extract<'_, '_, Option<Res<'_, HandMeshTarget>>>,
    render_resource: Option<Res<'_, HandMeshTarget>>,
) {
    if main_resource.is_none() && render_resource.is_some() {
        commands.remove_resource::<HandMeshTarget>();
    }
}

fn remove_hand_presence_if_absent(
    mut commands: Commands<'_, '_>,
    main_resource: Extract<'_, '_, Option<Res<'_, HandPresence>>>,
    render_resource: Option<Res<'_, HandPresence>>,
) {
    if main_resource.is_none() && render_resource.is_some() {
        commands.remove_resource::<HandPresence>();
    }
}
```

  - Keep the `#![allow(clippy::as_conversions, clippy::cast_possible_truncation, …)]` header and the `dots_bone_composite_plugin_builds` smoke test, renamed to `hand_mesh_composite_plugin_builds` using `HandMeshCompositePlugin`.

- [ ] **Step 3: Extend `hand_mesh/mod.rs`** with the overlay. Add the submodule + re-exports at the top:

```rust
pub mod bone_composite;
pub mod bone_wireframe;

pub use bone_composite::{
    HandMeshCompositePlugin, HandMeshCompositeSet, HandMeshTarget, HandPresence,
};
pub use bone_wireframe::{icosphere_line_mesh, BoneWireframeMaterial};

use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Hdr, RenderTarget, ScalingMode};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureFormat};
use bevy::render::view::Msaa;
use bevy::window::WindowResized;
use wc_core::input::entity::{BoneCenters, TrackedHand, BONE_COUNT};
use wc_core::input::projection::palm_to_world;
use wc_core::lifecycle::state::AppState;
use wc_core::sketch::sketch_active;

/// `RenderLayers` index for the Camera3d wireframe pass. Layer 0 is the default
/// (main Camera2d + 2D content); layer 1 is reserved for bone spheres + the
/// overlay `HandMeshCamera3d`.
pub const HAND_MESH_LAYER_INDEX: usize = 1;

/// The [`RenderLayers`] value for bone spheres and the overlay Camera3d.
pub const HAND_MESH_LAYER: RenderLayers = RenderLayers::layer(HAND_MESH_LAYER_INDEX);

/// Per-sketch configuration for the hand-mesh overlay. Inserted on
/// `OnEnter(config.app_state)`, read by [`ensure_bone_meshes`], removed on exit.
#[derive(Resource, Clone)]
pub struct HandMeshConfig {
    /// Sketch this overlay belongs to.
    pub app_state: AppState,
    /// Base wireframe colour (sRGB); scaled by `glow_intensity` for HDR bloom.
    pub bone_color: Color,
    /// Emissive multiplier (`~3–8`) pushing the linear colour above 1.0.
    pub glow_intensity: f32,
    /// Radius of each bone icosphere, in logical pixels.
    pub bone_radius: f32,
}

/// Marker for the off-screen `Camera3d` that rasterizes the wireframe bones into
/// the [`HandMeshTarget`] image (raw linear HDR, no bloom, no tonemap).
#[derive(Component)]
pub struct HandMeshCamera3d;

/// Index of a bone sphere child on a `TrackedHand`. `0..BONE_COUNT` (20).
#[derive(Component, Debug, Clone, Copy)]
pub struct BoneIndex(pub usize);

/// Marker placed on a `TrackedHand` once its 20 bone children are attached, so
/// [`ensure_bone_meshes`] is idempotent. Removed on `OnExit` with the children.
#[derive(Component)]
struct HandMeshBones;

/// Plugin wiring the wireframe bone overlay for one sketch. Add once per sketch
/// with that sketch's [`HandMeshConfig`]. The composite node is owned globally
/// by [`HandMeshCompositePlugin`] (registered once by `SketchesPlugin`).
pub struct HandMeshPlugin {
    /// Per-sketch overlay configuration.
    pub config: HandMeshConfig,
}

impl Plugin for HandMeshPlugin {
    fn build(&self, app: &mut App) {
        let config = self.config.clone();
        let state = config.app_state;

        // Always insert per-sketch config + presence on enter.
        {
            let cfg = config.clone();
            app.add_systems(OnEnter(state), move |mut commands: Commands<'_, '_>| {
                commands.insert_resource(cfg.clone());
                commands.insert_resource(HandPresence(false));
            });
        }

        // `WC_DEBUG_DISABLE_BONE_CAMERA` skips the off-screen camera in debug.
        #[cfg(debug_assertions)]
        let spawn_camera = !app
            .world()
            .get_resource::<wc_core::debug::DebugToggles>()
            .is_some_and(|t| t.disable_bone_camera);
        #[cfg(not(debug_assertions))]
        let spawn_camera = true;
        if spawn_camera {
            app.add_systems(OnEnter(state), spawn_hand_mesh_camera);
        }

        app.add_systems(
            OnExit(state),
            (
                despawn_hand_mesh_camera,
                despawn_all_bone_children,
                remove_hand_mesh_config_and_presence,
            ),
        )
        .add_systems(
            Update,
            (ensure_bone_meshes, update_bone_transforms, update_hand_presence)
                .chain()
                .run_if(sketch_active(state)),
        )
        .add_systems(Update, resize_bone_target.run_if(in_state(state)));
    }
}
```

- [ ] **Step 4: Add the overlay systems to `hand_mesh/mod.rs`.** Port them from `crates/wc-sketches/src/line/hand_mesh.rs` (the camera/target/resize/despawn/transforms are identical between Line and Dots) and `crates/wc-sketches/src/dots/hand_mesh.rs` (the presence gate). Concretely:
  - `spawn_hand_mesh_camera` and `create_bone_target`: copy from `line/hand_mesh.rs:209-275` verbatim, using the shared `HandMeshCamera3d`, `HandMeshTarget`, and `HAND_MESH_LAYER`.
  - `resize_bone_target`, `despawn_hand_mesh_camera`, `despawn_all_bone_children`, `update_bone_transforms`: copy from `line/hand_mesh.rs:281-437` verbatim, using shared `HandMeshCamera3d`/`HandMeshTarget`/`HandMeshBones`/`BoneIndex`.
  - `ensure_bone_meshes`: copy from `line/hand_mesh.rs:362-412` but read colour/radius/glow from config (replacing the `hand_mesh_color()`/`BONE_RADIUS`/`BONE_GLOW_INTENSITY` constants):

```rust
fn ensure_bone_meshes(
    mut commands: Commands<'_, '_>,
    new_hands: Query<'_, '_, Entity, (With<TrackedHand>, Without<HandMeshBones>)>,
    meshes: Option<ResMut<'_, Assets<Mesh>>>,
    materials: Option<ResMut<'_, Assets<BoneWireframeMaterial>>>,
    config: Option<Res<'_, HandMeshConfig>>,
) {
    if new_hands.is_empty() {
        return;
    }
    let (Some(mut meshes), Some(mut materials), Some(config)) = (meshes, materials, config) else {
        return;
    };

    let line_mesh = meshes.add(icosphere_line_mesh(config.bone_radius));
    let base = config.bone_color.to_linear();
    let bone_material = materials.add(BoneWireframeMaterial {
        color: LinearRgba::rgb(
            base.red * config.glow_intensity,
            base.green * config.glow_intensity,
            base.blue * config.glow_intensity,
        ),
    });

    for hand in &new_hands {
        commands
            .entity(hand)
            .insert(HandMeshBones)
            .with_children(|parent_builder| {
                for i in 0..BONE_COUNT {
                    parent_builder.spawn((
                        Mesh3d(line_mesh.clone()),
                        MeshMaterial3d(bone_material.clone()),
                        HAND_MESH_LAYER,
                        BoneIndex(i),
                        Transform::default(),
                    ));
                }
            });
    }
}
```

  - `update_hand_presence`: port from `dots/hand_mesh.rs:148-158` (`update_dots_bone_activity`) using shared types:

```rust
/// Per-frame: gate the bone camera + composite on tracked-hand presence.
/// Sets [`HandPresence`] and the bone camera's `is_active` from the
/// `TrackedHand` count. O(≤2 hands); no allocation.
fn update_hand_presence(
    hands: Query<'_, '_, (), With<TrackedHand>>,
    mut cameras: Query<'_, '_, &mut Camera, With<HandMeshCamera3d>>,
    mut presence: ResMut<'_, HandPresence>,
) {
    let present = !hands.is_empty();
    presence.0 = present;
    for mut cam in &mut cameras {
        cam.is_active = present;
    }
}
```

  - `remove_hand_mesh_config_and_presence` (new, OnExit):

```rust
/// `OnExit` — drop the per-sketch config + presence flag. Removing
/// [`HandPresence`] triggers the render-world removal system so the composite
/// guard resets cleanly when the sketch is not active.
fn remove_hand_mesh_config_and_presence(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<HandMeshConfig>();
    commands.remove_resource::<HandPresence>();
}
```

- [ ] **Step 5: Move the overlay unit tests** into `hand_mesh/mod.rs` `#[cfg(test)] mod tests`. Port the three tests from `dots/hand_mesh.rs:446-594`, renaming to shared types: `ensure_bone_meshes_spawns_20_bone_children` and `ensure_bone_meshes_is_idempotent` (register `Assets<BoneWireframeMaterial>` and seed a `HandMeshConfig` resource via `app.insert_resource(HandMeshConfig { app_state: AppState::Dots, bone_color: Color::WHITE, glow_intensity: 5.0, bone_radius: 10.0 })` before `run_system_once(ensure_bone_meshes)`), and `update_hand_presence_gates_camera_and_resource` (seed `HandPresence(false)`, assert it flips with `TrackedHand` presence).

- [ ] **Step 6: Run the shared module tests**

Run: `cargo nextest run -p wc-sketches hand_mesh::`
Expected: PASS (wireframe + overlay + `hand_mesh_composite_plugin_builds`).

- [ ] **Step 7: Build the workspace** (the shared code coexists with the still-present per-sketch copies)

Run: `cargo build -p wc-sketches`
Expected: compiles.

- [ ] **Step 8: Commit**

```bash
git add crates/wc-sketches/src/hand_mesh assets/shaders/hand_mesh/bone_composite.wgsl
git commit -F <msg-file>   # "refactor(hand_mesh): shared overlay plugin + composite + HandPresence gate"
```

---

## Task H3: Wire shared globals + migrate Dots

Registers the shared globals once and moves Dots onto them, deleting Dots' three files. Line is untouched (keeps its own `HandMeshTarget` type), so the shared composite no-ops in Line — both captures stay green.

**Files:**
- Modify: `crates/wc-sketches/src/lib.rs` (SketchesPlugin)
- Modify: `crates/wc-sketches/src/dots/mod.rs`
- Delete: `crates/wc-sketches/src/dots/{hand_mesh,bone_composite,bone_wireframe}.rs`

**Interfaces:**
- Consumes: `hand_mesh::{HandMeshPlugin, HandMeshConfig, HandMeshCompositePlugin, HandMeshCompositeSet, BoneWireframeMaterial}` (H2).

- [ ] **Step 1: Register shared globals once** in `crates/wc-sketches/src/lib.rs` `SketchesPlugin::build`. After the particle plugin registrations (line 36), add:

```rust
        // Shared hand-mesh overlay infra, registered once (like the particle
        // plugins above) so each sketch's `HandMeshPlugin` can be added without
        // re-registering the material or composite node. `MaterialPlugin` and
        // `HandMeshCompositePlugin` are `Plugin` singletons.
        app.add_plugins(
            bevy::pbr::MaterialPlugin::<crate::hand_mesh::BoneWireframeMaterial>::default(),
        );
        // `WC_DEBUG_DISABLE_BONE_COMPOSITE` gates the composite globally (debug only).
        #[cfg(debug_assertions)]
        let register_bone_composite = !app
            .world()
            .get_resource::<wc_core::debug::DebugToggles>()
            .is_some_and(|t| t.disable_bone_composite);
        #[cfg(not(debug_assertions))]
        let register_bone_composite = true;
        if register_bone_composite {
            app.add_plugins(crate::hand_mesh::HandMeshCompositePlugin);
        }
```

- [ ] **Step 2: Delete Dots' three files**

```bash
git rm crates/wc-sketches/src/dots/hand_mesh.rs \
       crates/wc-sketches/src/dots/bone_composite.rs \
       crates/wc-sketches/src/dots/bone_wireframe.rs
```

- [ ] **Step 3: Update `dots/mod.rs` module declarations** — remove the three `pub mod` lines (`dots/mod.rs:45,46,48`): delete `pub mod bone_composite;`, `pub mod bone_wireframe;`, `pub mod hand_mesh;`. (Keep `pub mod post_process;`.)

- [ ] **Step 4: Update `dots/mod.rs` plugin wiring.**
  - Remove the `update_dots_bone_activity` Update registration (`dots/mod.rs:167-172`).
  - Replace the `DotsHandMeshPlugin` + `DotsBoneCompositePlugin` adds (`dots/mod.rs:178-187`) with the shared overlay registration + ordering edge:

```rust
        // Shared wireframe bone overlay (was DotsHandMeshPlugin + DotsBoneCompositePlugin).
        app.add_plugins(crate::hand_mesh::HandMeshPlugin {
            config: crate::hand_mesh::HandMeshConfig {
                app_state: AppState::Dots,
                // Ice blue `#b0d8ff` — unchanged from the old DotsHandMesh colour.
                bone_color: Color::srgb(
                    f32::from(0xb0_u8) / 255.0,
                    f32::from(0xd8_u8) / 255.0,
                    f32::from(0xff_u8) / 255.0,
                ),
                glow_intensity: 5.0,
                bone_radius: 10.0,
            },
        });
        // Order the shared composite after the explode post-process (was the
        // `.after(dots_post_process)` edge inside DotsBoneCompositePlugin).
        if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
            render_app.configure_sets(
                bevy::core_pipeline::Core2d,
                crate::hand_mesh::HandMeshCompositeSet
                    .after(post_process::dots_post_process),
            );
        }
```

- [ ] **Step 5: Remove the `DotsBoneActive` lifecycle** in `dots/mod.rs`:
  - Delete the insert at `dots/mod.rs:311` (`commands.insert_resource(hand_mesh::DotsBoneActive(false));`) and its preceding comment (`dots/mod.rs:309-310`).
  - Delete the remove at `dots/mod.rs:332` (`commands.remove_resource::<hand_mesh::DotsBoneActive>();`).
  - `HandPresence` insert/remove is now owned by the shared `HandMeshPlugin` — no replacement needed here.

- [ ] **Step 6: Fix the affected `dots/mod.rs` tests.** In the two lifecycle tests that reference `DotsBoneActive` (`dots/mod.rs` ~498-599), remove every `DotsBoneActive` line: the `use hand_mesh::DotsBoneActive;` imports, the `world.insert_resource(DotsBoneActive(..))` seeds, and the `DotsBoneActive` assertions. The `HandPresence` lifecycle is covered by the shared module's `update_hand_presence_gates_camera_and_resource` test (H2). Leave the rest of each test (the `ParticleSimParams`/`DotsPostParams`/`DotsExplodeFocal` assertions) intact.

- [ ] **Step 7: Build + unit tests**

Run: `cargo build -p wc-sketches && cargo nextest run -p wc-sketches`
Expected: compiles; all tests pass (no dangling `Dots*` bone references).

- [ ] **Step 8: Capture-verify Dots (migrated) and Line (untouched)**

Run: `cargo xtask capture dots` then `cargo xtask capture line`
Review the PNGs against the current baselines. Expected: both match. (Line still uses its own composite; Dots now uses the shared one.) If Dots differs, stop — the colour or ordering was not preserved.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -F <msg-file>   # "refactor(dots): migrate hand mesh onto shared module; register shared globals"
```

---

## Task H4: Migrate Line

Moves Line onto the shared overlay/composite and deletes Line's three files. After this, both sketches use the shared path.

**Files:**
- Modify: `crates/wc-sketches/src/line/mod.rs`
- Delete: `crates/wc-sketches/src/line/{hand_mesh,bone_composite,bone_wireframe}.rs`

**Interfaces:**
- Consumes: `hand_mesh::{HandMeshPlugin, HandMeshConfig, HandMeshCompositeSet}` (H2). The global `MaterialPlugin` + `HandMeshCompositePlugin` are already registered (H3).

- [ ] **Step 1: Delete Line's three files**

```bash
git rm crates/wc-sketches/src/line/hand_mesh.rs \
       crates/wc-sketches/src/line/bone_composite.rs \
       crates/wc-sketches/src/line/bone_wireframe.rs
```

- [ ] **Step 2: Update `line/mod.rs` module declarations** — remove `pub mod bone_composite;`, `pub mod bone_wireframe;`, `pub mod hand_mesh;` (`line/mod.rs:40,41,42`). Keep `pub mod post_process;`.

- [ ] **Step 3: Replace the composite + hand-mesh registration.** Remove the bone-composite gating block (`line/mod.rs:114-125`) and the `LineHandMeshPlugin` add (`line/mod.rs:135-136`), replacing both with the shared overlay registration + ordering edge:

```rust
        // Shared wireframe bone overlay (was LineHandMeshPlugin + LineBoneCompositePlugin;
        // the composite is now a global plugin gated in SketchesPlugin).
        app.add_plugins(crate::hand_mesh::HandMeshPlugin {
            config: crate::hand_mesh::HandMeshConfig {
                app_state: AppState::Line,
                // `#add6b6` — unchanged from the old LineHandMesh colour.
                bone_color: Color::srgb(
                    f32::from(0xad_u8) / 255.0,
                    f32::from(0xd6_u8) / 255.0,
                    f32::from(0xb6_u8) / 255.0,
                ),
                glow_intensity: 5.0,
                bone_radius: 10.0,
            },
        });
        // Order the shared composite after the gravity smear (was the
        // `.after(line_post_process)` edge inside LineBoneCompositePlugin).
        if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
            render_app.configure_sets(
                bevy::core_pipeline::Core2d,
                crate::hand_mesh::HandMeshCompositeSet
                    .after(post_process::line_post_process),
            );
        }
```

- [ ] **Step 4: Remove the now-dead `should_register_bone_composite`** helper (`line/mod.rs:444-449`) and its use. Update the `render_stage_gating_predicate` test (`line/mod.rs:462+`): it constructs a `DebugToggles` and asserts the predicate — drop the `should_register_bone_composite` assertion (the composite gate now lives in `SketchesPlugin`); keep the `should_register_smear` assertion. If removing leaves the test trivial, delete the test.

- [ ] **Step 5: Build + unit tests**

Run: `cargo build -p wc-sketches && cargo nextest run -p wc-sketches`
Expected: compiles; tests pass; no `line::hand_mesh`/`line::bone_composite`/`line::bone_wireframe` references remain.

Run: `rg -n "line::(hand_mesh|bone_composite|bone_wireframe)|dots::(hand_mesh|bone_composite|bone_wireframe)" crates/`
Expected: no matches.

- [ ] **Step 6: Capture-verify both sketches**

Run: `cargo xtask capture line` then `cargo xtask capture dots`
Review against baselines. Expected: both match. The Line gate is expected to be a visual no-op (with no hands, the bone camera renders black, so the previously-run composite was a no-op). If a Line frame differs, stop and explain before re-baselining.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -F <msg-file>   # "refactor(line): migrate hand mesh onto shared module"
```

---

## Task H5: Delete dead shaders, doc sweep, full gates

**Files:**
- Delete: `assets/shaders/{line,dots}/{bone_wireframe,bone_composite}.wgsl`
- Modify: stale references in module docs / `SketchesPlugin` comment.

- [ ] **Step 1: Confirm the per-sketch shaders are unreferenced**

Run: `rg -n "shaders/(line|dots)/bone_(wireframe|composite)" crates/`
Expected: no matches (the shared module uses `shaders/hand_mesh/...`).

- [ ] **Step 2: Delete the four dead shaders**

```bash
git rm assets/shaders/line/bone_wireframe.wgsl assets/shaders/line/bone_composite.wgsl \
       assets/shaders/dots/bone_wireframe.wgsl assets/shaders/dots/bone_composite.wgsl
```

- [ ] **Step 3: Update the stale `SketchesPlugin` comment** in `crates/wc-sketches/src/lib.rs:19-26` — it references `line::bone_wireframe` and "The Line sketch's `hand_mesh` module". Reword to point at `crate::hand_mesh::bone_wireframe` (the shared module) and note the `WireframePlugin` Metal rationale unchanged.

- [ ] **Step 4: Run the full AGENTS.md gate set**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
```
Expected: all pass (the ~29 pre-existing doc-link warnings are non-fatal).

- [ ] **Step 5: Manual smoke test**

Run: `cargo rund`
Enter Line, wave a tracked hand: bones render in warm teal and vanish cleanly when hands leave. Enter Dots: bones render in ice-blue, no ghosting on hand exit. (Hand rendering needs Leap/MediaPipe hardware; if unavailable, note it and rely on the capture gates + unit tests.)

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -F <msg-file>   # "refactor(hand_mesh): drop dead per-sketch bone shaders; doc sweep"
```

---

## Self-Review

**1. Spec coverage:**
- Shared `hand_mesh/` module (`mod.rs`+`bone_wireframe.rs`+`bone_composite.rs`) → H1, H2. ✓
- Shared shaders → H1 (wireframe), H2 (composite). ✓
- Migrate Line + Dots, delete six files + four shaders → H3 (Dots), H4 (Line), H5 (shaders). ✓
- Unified `HandPresence` gate, unconditional, Line adopts it → H2 (defined), H3/H4 (wired). ✓
- Plain-data `HandMeshConfig` (colour/`AppState`/glow/radius) → H2. ✓
- Behavior-preservation: captures unchanged at every migration step → H3 Step 8, H4 Step 6. ✓
- Composite ordering after each sketch's post-process via `HandMeshCompositeSet` edges → H3 Step 4, H4 Step 3. ✓
- `bone_wireframe` clean extract first → H1. ✓
- AGENTS.md gate set → H5 Step 4. ✓
- Render-layer constant stays shared (`HAND_MESH_LAYER`) → H2 Step 3. ✓
- `WC_DEBUG_DISABLE_BONE_CAMERA` (per-sketch camera) preserved → H2 Step 3; `WC_DEBUG_DISABLE_BONE_COMPOSITE` unified to global → H3 Step 1, H4 Step 4. ✓

**2. Placeholder scan:** No "TBD"/"handle appropriately". "Copy from `path:lines` verbatim with these changes" is precise relocation, not a placeholder; every changed/new code block is shown in full.

**3. Type consistency:** `HandMeshConfig`, `HandMeshPlugin`, `HandMeshTarget`, `HandPresence`, `HandMeshCamera3d`, `HandMeshBones`, `BoneIndex`, `HAND_MESH_LAYER`, `HandMeshCompositePlugin`, `HandMeshCompositeSet`, `hand_mesh_composite`, `BoneWireframeMaterial`, `icosphere_line_mesh` are used consistently across H1–H5. The ordering edge uses `HandMeshCompositeSet.after(<sketch>_post_process)` in both H3 and H4.
