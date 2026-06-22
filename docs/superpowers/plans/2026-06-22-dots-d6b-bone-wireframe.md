# Dots D6b — Bone-wireframe hand rendering — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** Render tracked hands as glowing wireframe skeletons in Dots (v4 Dots rendered the Leap hands in-scene), porting Line's `HandMesh` subsystem onto Dots — 20 wireframe-icosphere bones per `TrackedHand`, rasterized by a dedicated off-screen `Camera3d` and composited back over the Dots scene.

**Architecture:** A faithful port of Line's three bone-mesh pieces (carry-forward #74: land per-sketch, don't upstream-extract prematurely): (1) the `BoneWireframeMaterial` + `icosphere_line_mesh` + WGSL (generic — copied for Dots, with a Dots-appropriate hue); (2) `DotsHandMeshPlugin` — a `DotsHandMeshCamera3d` (off-screen HDR `Camera3d` on `HAND_MESH_LAYER`) + a reconcile that spawns 20 bone children on every `TrackedHand` while Dots is active; (3) `DotsBoneCompositePlugin` — the render-graph node that additively composites the bone camera's HDR target back over the main scene. Known limitation (carry-forward #75): the wireframes render at full brightness without the main camera's bloom rolloff — same as Line; not fixed here.

**Tech Stack:** Rust, Bevy 0.19 (`Camera3d`, `RenderLayers`, `MaterialPlugin`, render-graph composite node), WGSL, the `TrackedHand` entity model.

## Global Constraints

- **Faithful port of the working Line subsystem.** Mirror `crates/wc-sketches/src/line/{hand_mesh.rs, bone_wireframe.rs, bone_composite.rs}` + `assets/shaders/line/{bone_wireframe.wgsl, bone_composite.wgsl}` for Dots. Where a piece is generic (the icosphere mesh, the LineList material, the WGSL), COPY it for Dots with a carry-forward note (these could move to a shared module later, like the leap-power curve).
- **Reuse the existing hand infra + the `HAND_MESH_LAYER` index.** `HAND_MESH_LAYER` (RenderLayers layer 1) is a single global; Dots' `Camera3d` and Line's are mutually exclusive (only one sketch active), so they can share the layer index. Read how Line defines/uses it and reuse the same constant if it's `pub`, else mirror it. Reuse `wc_core::input::entity::TrackedHand` + the palm/bone projection.
- **Metal-safe:** bones are `PrimitiveTopology::LineList` icospheres shaded by a custom material (NOT Bevy's `WireframePlugin`, which needs `POLYGON_MODE_LINE` that Metal lacks) — this is exactly why Line's `bone_wireframe.rs` exists; the Dots copy must keep the LineList approach.
- **Lifecycle:** the bone camera + bones exist only while in Dots — spawn the camera on enter (or reconcile), despawn on exit; reconcile bones onto `TrackedHand` while Dots active, despawn on exit. Mirror Line's lifecycle exactly (read the rationale comments about the reconcile-vs-observer).
- **No `unwrap()`/`expect()`** in non-test code unless documented; **no `as`** where `TryFrom` works; **`///`/`//!` docs;** WGSL in `assets/shaders/dots/`.
- **Hardware verification is the operator's.** The wireframe skeletons only render with tracked hands present (Leap/MediaPipe) — NOT auto-verifiable. Unit-test what's deterministic (the icosphere mesh shape, the per-hand bone count, the bone-transform math if extracted); flag the rendered appearance + the composite as operator-deferred via `cargo rund` + hardware.
- **Verification gates:** fmt; clippy `--all-targets --all-features --workspace -D warnings`; nextest `--workspace --all-features` + `cargo test --doc`; `cargo doc`; `cargo xtask check-secrets`. Do NOT run `cargo rund`.
- **Commit messages:** `git commit -F` (no backticks).

## Reference material (read these)

- `crates/wc-sketches/src/line/bone_wireframe.rs` (`BoneWireframeMaterial`, `icosphere_line_mesh`, `BONE_ICO_SUBDIVISIONS`) + `assets/shaders/line/bone_wireframe.wgsl`.
- `crates/wc-sketches/src/line/hand_mesh.rs` (`LineHandMeshPlugin`, `HAND_MESH_LAYER`/`HAND_MESH_LAYER_INDEX`, `HandMeshCamera3d`, `BoneIndex`, `BONE_GLOW_INTENSITY`, `BONE_RADIUS_PX`, the camera setup, the bone-reconcile that spawns 20 children per `TrackedHand`, the orthographic projection, the per-bone transform update).
- `crates/wc-sketches/src/line/bone_composite.rs` + `assets/shaders/line/bone_composite.wgsl` (the additive composite node that blends the bone camera's HDR target back; note `LineBoneCompositePlugin` is registered by `LinePlugin::build`, NOT inside `LineHandMeshPlugin` — read the gating rationale).
- `crates/wc-sketches/src/dots/mod.rs` (where to register `DotsHandMeshPlugin` + `DotsBoneCompositePlugin`).
- Carry-forwards #74 (land per-sketch) + #75 (wireframes bypass bloom — known limitation, not fixed here).

---

### Task 1: Bone material/mesh/shader (copy) + `DotsHandMeshPlugin` (camera + bones)

**Files:**
- Create: `crates/wc-sketches/src/dots/bone_wireframe.rs` (`BoneWireframeMaterial` copy → `DotsBoneWireframeMaterial`, `icosphere_line_mesh`)
- Create: `assets/shaders/dots/bone_wireframe.wgsl` (copy)
- Create: `crates/wc-sketches/src/dots/hand_mesh.rs` (`DotsHandMeshPlugin`, `DotsHandMeshCamera3d`, the bone reconcile + transform update)
- Modify: `crates/wc-sketches/src/dots/mod.rs` (`pub mod bone_wireframe; pub mod hand_mesh;`; add `DotsHandMeshPlugin`)

**Interfaces:**
- Produces: `DotsBoneWireframeMaterial { color: LinearRgba }`, `icosphere_line_mesh(radius) -> Mesh`, `DotsHandMeshPlugin`, `DotsHandMeshCamera3d` (marker), `BoneIndex(usize)`.
- Consumes: `wc_core::input::entity::TrackedHand` + the palm/bone data, `HAND_MESH_LAYER`.

- [ ] **Step 1: Copy the bone material/mesh/shader**

Copy `line/bone_wireframe.rs` → `dots/bone_wireframe.rs` as `DotsBoneWireframeMaterial` (the `color: LinearRgba` uniform) + `icosphere_line_mesh` (verbatim — it's geometry). Copy `assets/shaders/line/bone_wireframe.wgsl` → `assets/shaders/dots/bone_wireframe.wgsl`, point the material's `ShaderRef` at the new path. Pick a Dots-appropriate bone hue (carry-forward #74 notes per-sketch materials — Line uses its own; choose a colour that reads on the dark "fabric" field, e.g. a cool white/cyan; the operator can re-tune). Add a carry-forward comment: the material/mesh/shader are generic and duplicate Line's — a shared `particles/` (or wc-core) home is the eventual move. Keep the `icosphere_line_mesh_is_line_list_with_paired_indices` test (adapt for Dots).

- [ ] **Step 2: `DotsHandMeshPlugin` (camera + bone reconcile)**

Mirror `LineHandMeshPlugin` (read it carefully): register `MaterialPlugin::<DotsBoneWireframeMaterial>`; spawn a `DotsHandMeshCamera3d` (off-screen HDR `Camera3d`, `Msaa::Sample4`, the orthographic projection, on `HAND_MESH_LAYER`, `ClearColorConfig` appropriate for the additive composite) on Dots enter (or reconcile), despawned on exit; a reconcile system that, while `sketch_active(Dots)` (or the same gate Line uses), spawns 20 bone children (`BoneIndex(0..20)`, each an `icosphere_line_mesh` at `BONE_RADIUS_PX` with `DotsBoneWireframeMaterial` on `HAND_MESH_LAYER`) on every `TrackedHand` `Without` bones; a per-frame system that positions each bone from its hand's bone/palm data (mirror Line's transform update exactly — same projection); detach/despawn on Dots exit. Reuse Line's `BONE_GLOW_INTENSITY`/`BONE_RADIUS_PX` constants' values. Register the systems with the same gating/ordering Line uses.

- [ ] **Step 3: Tests**

The icosphere mesh test (adapted). A bone-count test: spawning a synthetic `TrackedHand` while Dots active and running the reconcile yields 20 `BoneIndex` children (mirror Line's hand-mesh test if one exists — read `crates/wc-sketches/tests/` for the synthetic-`TrackedHand` + hand-mesh harness). If the bone-transform projection is extractable as a pure fn, unit-test one bone position. The rendered appearance is operator-deferred.

- [ ] **Step 4: Build, test, gates**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run --workspace --all-features
cargo clippy --all-targets --all-features --workspace -- -D warnings
```

Expected: PASS. (The wireframe rendering is operator-verified with hardware — not run here.)

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "feat(dots): wireframe bone hand rendering (material + camera + 20 bones/hand)" "" "Port of Line's HandMesh: Metal-safe LineList icosphere bones on a dedicated off-screen Camera3d (HAND_MESH_LAYER), 20 per TrackedHand while Dots active. Composite lands in D6b Task 2. Rendering hardware-verified." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

### Task 2: `DotsBoneCompositePlugin` (additive composite)

**Files:**
- Create: `crates/wc-sketches/src/dots/bone_composite.rs` (`DotsBoneCompositePlugin` + the composite render node)
- Create: `assets/shaders/dots/bone_composite.wgsl` (copy)
- Modify: `crates/wc-sketches/src/dots/mod.rs` (register `DotsBoneCompositePlugin` — mirror where `LinePlugin` registers `LineBoneCompositePlugin`)

**Interfaces:**
- Produces: `DotsBoneCompositePlugin`.
- Consumes: the `DotsHandMeshCamera3d`'s HDR target (or the shared `HandMeshTarget` resource Line uses — read how Line's composite reads the bone camera output) + the main Dots view.

- [ ] **Step 1: Copy the composite node + shader**

Copy `line/bone_composite.rs` → `dots/bone_composite.rs` as `DotsBoneCompositePlugin` + its node (rename `Line*`→`Dots*`), and `assets/shaders/line/bone_composite.wgsl` → `assets/shaders/dots/bone_composite.wgsl` (point the pipeline at the new path). Read how Line's composite reads the bone camera's render target (a `HandMeshTarget` resource, or the camera's `Image` target) and blends it additively over the main scene in the render graph — mirror exactly. Note in the doc that it no-ops cleanly when the bone target is absent (mirror Line's "no-ops when `HandMeshTarget` is absent" property, so it's safe outside Dots / with no hands).

- [ ] **Step 2: Register `DotsBoneCompositePlugin`**

Register it where Line registers `LineBoneCompositePlugin` (Line does this in `LinePlugin::build`, hoisted out of `LineHandMeshPlugin` so a debug toggle can gate it — read the rationale and decide whether Dots needs the same hoist; for Dots, registering it in `DotsPlugin::build` alongside `DotsHandMeshPlugin` is fine unless a debug toggle requires the split). Ensure it's gated/no-ops outside Dots like Line's (the per-sketch bone target absent → composite no-op).

- [ ] **Step 3: Tests**

The composite is a render-graph node (no RenderApp under MinimalPlugins), so unit coverage is limited to what's deterministic (e.g. the plugin builds without panic under MinimalPlugins — mirror the `particle_compute_plugin_builds` smoke test pattern). A build-smoke test that `DotsBoneCompositePlugin` adds cleanly. The visual composite is operator-verified.

- [ ] **Step 4: Build, test, gates**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run --workspace --all-features
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo fmt --all -- --check
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "feat(dots): additive bone-wireframe composite over the Dots scene" "" "Ports Line's bone composite: blends the off-screen bone camera's HDR target back over Dots. No-ops cleanly when no hands/target. Visual composite hardware-verified. Known limit (carry-forward #75): bones bypass the main bloom rolloff." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

## Self-Review

**Spec coverage** (against §"v5 kiosk extras" bone-wireframe + D6 row + carry-forward #74):
- Bone material/mesh/shader (Metal-safe LineList icosphere) → Task 1 Step 1. ✓
- `DotsHandMeshPlugin`: off-screen Camera3d + 20 bones/hand on `TrackedHand` while Dots active → Task 1 Step 2. ✓
- `DotsBoneCompositePlugin`: additive composite back over the scene → Task 2. ✓
- Per-sketch (not upstream-extracted), with copies flagged as a future-share carry-forward → constraints + Task 1. ✓
- Known bloom-bypass limitation (#75) acknowledged, not fixed → architecture + Task 2 commit. ✓

**Placeholder scan:** No TBD-as-deliverable. Each piece mirrors a named, working Line file; the generic pieces are copied with a flagged carry-forward. The rendered appearance + the render-graph composite are explicitly operator-deferred (no RenderApp/hardware in unit tests), with the deterministic parts (mesh shape, bone count) unit-tested.

**Type consistency:** `DotsBoneWireframeMaterial`, `icosphere_line_mesh`, `DotsHandMeshPlugin`, `DotsHandMeshCamera3d`, `BoneIndex`, `DotsBoneCompositePlugin` — consistent across tasks; reuses `HAND_MESH_LAYER` + `TrackedHand`.

**Risks:** (1) The render-graph composite is the hardest, least-unit-testable piece — mitigated by faithfully mirroring Line's working node + the build-smoke test + operator verification. (2) Camera/layer coexistence with Line's bone camera — both gated to their own sketch (mutually exclusive), reusing the shared layer index. (3) The copied material/mesh/shader/composite are flagged carry-forwards (v4-shared-style; extract later). (4) Hardware-only rendering verification is operator-deferred.
