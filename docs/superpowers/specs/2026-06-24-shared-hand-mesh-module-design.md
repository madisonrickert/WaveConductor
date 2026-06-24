# WaveConductor v5 — Shared hand-mesh overlay module (Line + Dots dedup)

**Status:** Design approved 2026-06-24. First of two sequenced specs; the
follow-on *Cymatics sketch port* consumes this module as its third caller.

**Verification target:** Behavior-preserving refactor. The existing
`cargo xtask capture line` and `cargo xtask capture dots` baselines must not
change (re-baseline only if a frame genuinely differs and the difference is
understood — see *Behavior-preservation contract*).

## Goal

Extract the bloomed wireframe-hand overlay — today **duplicated** and **diverged**
across `line/` and `dots/` — into a single shared `hand_mesh/` module under
`crates/wc-sketches/src/`, migrate both shipped sketches onto it, and delete the
six duplicated files plus four per-sketch shaders. The shared module is
parameterized by plain-data config (color, `AppState`, ordering) with one shared
behavior (skip the composite when no hands are tracked). The upcoming Cymatics
port registers as the third caller instead of forking a fourth copy.

This is a prerequisite, broken out as its own spec, because deduplicating two
*shipped* sketches is a reconciliation with its own regression surface and
verification; bundling it into the Cymatics port would make a Line/Dots
regression hard to isolate from new sketch work.

## Scope

**In scope:**

- New shared module `crates/wc-sketches/src/hand_mesh/` (`mod.rs`,
  `bone_wireframe.rs`, `bone_composite.rs`).
- Shared shaders `assets/shaders/hand_mesh/bone_wireframe.wgsl` and
  `assets/shaders/hand_mesh/bone_composite.wgsl`.
- Migrate `line/` and `dots/` to register the shared `HandMeshPlugin` with their
  own config; delete their `hand_mesh.rs`, `bone_composite.rs`,
  `bone_wireframe.rs` bodies and their per-sketch `bone_*` shaders.
- Unify Dots' `DotsBoneActive` hand-presence gate into a shared `HandPresence`
  resource + update system; make "skip composite when no hands present"
  unconditional for all callers (Line gains it as a pure-perf win).

**Out of scope:**

- Any visual change to Line or Dots. Colors, glow, bone geometry, and composite
  ordering are preserved exactly.
- The Cymatics sketch itself (its own spec). This spec only ensures the shared
  interface is shaped so a third caller is a few lines of config.
- Hand-tracking input/attractor logic (`leap_attractors` / `hand_attractors`).
  Untouched — the mesh consumes `TrackedHand` entities exactly as today.

## Background: what is duplicated and what diverged

Today each sketch owns three near-identical files (measured divergence, by
differing lines):

| File pair | Line | Dots | Differing |
|---|---|---|---|
| `hand_mesh.rs` | 437 | 594 | 525 |
| `bone_composite.rs` | 305 | 373 | 214 |
| `bone_wireframe.rs` | 138 | 157 | 47 |

The large differing-line counts are mostly renames (`HandMeshCamera3d` vs
`DotsHandMeshCamera3d`, etc.) and doc comments. The substantive picture:

- **Shaders are identical code.** `diff` of both `bone_wireframe.wgsl` and
  `bone_composite.wgsl` across `line/` and `dots/` shows only doc-comment lines.
  → one shared shader each.
- **`bone_wireframe.rs`** is identical but for names: a `Material` with a single
  `color: LinearRgba` uniform plus a pure `icosphere_line_mesh(radius)` that
  builds a Metal-safe `LineList` icosphere (subdivisions = 1). → fully shared.
- **`hand_mesh.rs` / `bone_composite.rs`** share 85–90% of their logic: an
  off-screen HDR `Camera3d` rendering 20 bone icospheres per `TrackedHand` into a
  private `Rgba16Float` target (no bloom, no tonemap), per-frame bone transforms
  via `palm_to_world`, resize + teardown, and an additive composite render node
  inserted into `Core2d` `EarlyPostProcess` after the sketch's own post-process.
  The remaining differences are **config** plus **one real behavior**.

### The one genuinely sketch-specific behavior

Dots gates its composite on hand presence. `dots/bone_composite.rs` returns
*before* `post_process_write()` when no hands are tracked, so the post-process
ping-pong is not flipped and the last-rendered bone image cannot ghost onto the
explode output. Dots carries `DotsBoneActive` (an `ExtractResource` bool) updated
each frame by `update_dots_bone_activity`, plus explicit render-world cleanup.

Line has no such gate today: with no hands, its bone camera renders an empty
(black) target and the additive composite is a visual no-op, so Line never
ghosted. Per the 2026-06-24 decision, **Line will adopt the gate too** — purely
to skip a no-op composite pass when idle (a small GPU/power win on the multi-hour
soak target). This is expected to be visually identical for Line.

## Architecture

### Shared module `crates/wc-sketches/src/hand_mesh/`

One concept per file, `mod.rs` as entry, shaders external — per AGENTS.md.

**`bone_wireframe.rs`**
- `pub struct BoneWireframeMaterial { color: LinearRgba }` — one unlit flat-color
  `Material` bound to the shared `assets/shaders/hand_mesh/bone_wireframe.wgsl`.
- `pub fn icosphere_line_mesh(radius: f32) -> Mesh` — the Metal-safe `LineList`
  icosphere, moved verbatim (it is byte-identical across the current copies).

**`mod.rs`** — the overlay plugin and its shared systems:
- `pub struct HandMeshConfig` (plain data, `Clone`):
  - `app_state: AppState` — which sketch this instance belongs to.
  - `bone_color: Color` — Line `#add6b6`, Dots `#b0d8ff`, Cymatics `#eb5938`.
  - `glow_intensity: f32` (default 5.0), `bone_radius: f32` (default 10.0).
  - `render_layer: usize` — the existing `HAND_MESH_LAYER_INDEX` constant.
  - `after_post_process: Option<…>` — the composite-ordering hook (see below).
- `pub struct HandMeshPlugin { config: HandMeshConfig }` — registers, gated on its
  `AppState`: `OnEnter` camera/target spawn, `Update` bone reconciliation +
  transform + `HandPresence` update, resize handler, `OnExit` teardown, and the
  `BoneCompositePlugin` (below).
- Shared systems (moved from the duplicated copies, parameterized by config):
  `spawn_hand_mesh_camera`, `ensure_bone_meshes`, `update_bone_transforms`,
  `resize_bone_target`, `despawn_hand_mesh_camera`, `despawn_all_bone_children`.
- Marker components are shared types (`HandMeshCamera3d`, `HandMeshBones`,
  `BoneIndex`) — safe because `AppState` is exclusive, so only one sketch's
  overlay entities exist at a time.

**`bone_composite.rs`**
- The additive-composite render pipeline + node (bind group: scene texture,
  sampler, bone texture), moved from the duplicated copies; loads the shared
  `assets/shaders/hand_mesh/bone_composite.wgsl`.
- `pub struct HandMeshTarget` — the private off-screen `Rgba16Float`
  `Handle<Image>`, `ExtractResource`.
- The composite system **unconditionally** consults `HandPresence` and returns
  before `post_process_write()` when no hands are tracked (the unified gate).

### The unified hand-presence gate

Generalize Dots' `DotsBoneActive` into the shared module:
- `pub struct HandPresence(pub bool)` — `Resource`, `ExtractResource`. True when
  any `TrackedHand` exists this frame.
- An `Update` system (active-sketch-gated) sets it from the `TrackedHand` query
  and toggles the bone camera's `Camera::is_active` to match (as Dots does today).
- Extracted to the render world; removed on `OnExit`. The composite reads it and
  skips the pass (and the ping-pong flip) when false.

No config flag: every caller wants the gate, so it is unconditional (YAGNI on a
flag nobody would set to false). The behavior matches Dots' shipped output and is
a no-op-skip for Line.

### Composite ordering hook

Each caller composites after its own post-process node so bones land on the
final sketch image: Line after `line_post_process`, Dots after `dots_post_process`.
A future caller whose sketch renders in the main 2D pass with no post-process
(Cymatics) orders simply within `EarlyPostProcess` after the 2D pass. The config
expresses this as an optional ordering target; the plan picks the exact
mechanism (a render-system ordering label) and Cymatics passes `None`.

### What each sketch keeps

`line/` and `dots/` shrink to a thin registration: build a `HandMeshConfig` with
their color/state/ordering and `app.add_plugins(HandMeshPlugin { config })`. The
six duplicated `.rs` files and the four per-sketch `bone_*` shaders are deleted.
Dots' `DotsBoneActive`, `update_dots_bone_activity`, and the bespoke cleanup go
away (absorbed into the shared `HandPresence`).

## Behavior-preservation contract

The refactor must hold these invariant; each is a verification gate:

1. **Colors exact:** Line `#add6b6`, Dots `#b0d8ff`, scaled by `glow_intensity`
   5.0 exactly as today.
2. **Geometry exact:** 20 bones/hand, icosphere subdivisions 1, radius 10.0.
3. **Composite ordering exact:** after each sketch's own post-process.
4. **Captures unchanged:** `capture line` and `capture dots` match current
   baselines. The Line gate is expected to be a visual no-op; if any Line frame
   differs, stop and explain why before re-baselining — a real visual change to a
   shipped sketch is out of scope for a dedup.
5. **VRAM/idle behavior:** overlay resources still despawn on `OnExit`; the
   composite still no-ops off-sketch.

## Plan staging

The implementation plan (separate doc) will stage roughly:

- **H1 — Shared skeleton:** create `hand_mesh/` (`bone_wireframe` first, the
  clean extract) + shared shaders; unit-test `icosphere_line_mesh` edge dedup.
- **H2 — Shared overlay + composite:** move the camera/reconcile/transform/resize/
  teardown and the composite pipeline; add the unified `HandPresence` gate.
- **H3 — Migrate Dots:** register `HandMeshPlugin`, delete Dots' copies + shaders;
  `capture dots` must match baseline.
- **H4 — Migrate Line:** register with the gate on, delete Line's copies +
  shaders; `capture line` must match baseline (confirm the gate is a no-op).
- **H5 — Sweep:** delete dead shader files, update module docs, run the full gate
  set. Confirm no `line/`-or-`dots/`-local `bone_*` references remain.

## Testing

- `cargo xtask capture line` and `capture dots` vs current baselines (the primary
  gate; the operator reviews PNGs, no LLM spend).
- Unit test `icosphere_line_mesh` (vertex/edge counts, dedup) in the shared module.
- The AGENTS.md gate set: `fmt`, `clippy --all-targets --all-features`,
  `nextest run`, `test --doc`, `cargo doc`, `cargo deny check`,
  `cargo xtask check-secrets`.
- Manual smoke (`cargo rund`): enter Line and Dots, wave a tracked hand, confirm
  bones render in each sketch's color and vanish cleanly when hands leave.

## Risks

- **Line gate changes a frame.** Mitigation: gated on the Line capture; if a frame
  differs, investigate (stale bone target not clearing?) before accepting.
- **Composite ordering hook too rigid for a no-post-process caller.** Mitigation:
  model ordering as an optional target so Cymatics can pass `None`; validated when
  sub-project #2 lands, but the interface is decided here.
- **Shared marker-type collision.** Mitigation: `AppState` exclusivity guarantees
  one overlay at a time; asserted by the existing single-sketch lifecycle.
- **Render-layer/3D-camera interplay** differs subtly between sketches.
  Mitigation: `render_layer` stays the existing shared `HAND_MESH_LAYER`; no
  change to layer indices.

## References

- Current copies: `crates/wc-sketches/src/{line,dots}/{hand_mesh,bone_composite,bone_wireframe}.rs`.
- Current shaders: `assets/shaders/{line,dots}/{bone_wireframe,bone_composite}.wgsl`.
- Prior shared-foundation precedent: `crates/wc-sketches/src/particles/` and
  `docs/superpowers/specs/2026-06-22-dots-fabric-design.md` (*share everything,
  gate features off per caller*).
- Follow-on consumer: the Cymatics port spec (sub-project #2, this session).
