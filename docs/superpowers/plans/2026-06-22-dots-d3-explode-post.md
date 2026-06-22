# Dots D3 — Explode post-process — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** Give Dots its signature "fabric" look — the v4 **explode** post-process: a 5-iteration radial chromatic-aberration zoom that spirals around the pointer, with a final per-channel gamma. It runs only while in `AppState::Dots`.

**Architecture:** A new Dots-owned post-process render-graph pass mirroring `crate::line::post_process` exactly in *structure* (ExtractResource uniform → persistent uniform buffer → a `Core2dSystems::EarlyPostProcess` render system → `ViewTarget::post_process_write()` source/dest → fullscreen-triangle draw into the `Rgba16Float` HDR target). The pass is gated to Dots by a `DotsPostParams` resource that is inserted on `OnEnter(AppState::Dots)` and removed on `OnExit` (the same per-sketch gating `ParticleSimParams` uses); the render system early-returns when the extracted resource is absent, so the explode is a no-op outside Dots. The WGSL shader is a faithful port of v4's `explode/fragment.glsl`.

**Tech Stack:** Rust, Bevy 0.19 render graph (Core2d), WGSL fragment + fullscreen-triangle vertex, bytemuck `Pod` uniform.

## Global Constraints

- **Bevy 0.19.** Mirror `crate::line::post_process` (read it) for the plugin/pipeline/node shape, the `EarlyPostProcess` set, the HDR-camera gate (`camera.hdr`), the `Rgba16Float` target, and the persistent-uniform-buffer + `queue.write_buffer` pattern (no per-frame GPU allocation).
- **Explode is gated to Dots.** Unlike Line's gravity-smear (which self-gates via `g_constant = 0`), the explode has NO inert parameter set (`pow(col, 0)` whites out; `shrink = 1` brightens). So gating is at the resource level: `DotsPostParams` exists only while in Dots. The render system takes `Option<Res<DotsPostParams>>` and early-returns when absent — exactly no-op outside Dots.
- **v4 reference (`/Users/madison/Developer/WaveConductor/.worktrees/v4/src/sketches/dots/shaders/explode/`):** port `fragment.glsl` faithfully; `shrinkFactor` default `0.98`, the spiral matrix `m2 = mat2(1.6,-1.2,1.2,1.6)`, the spiral step `center -= m2*(center - 0.5)*0.5928`, 5 iterations, per-channel shrink compounding (`shrink *= shrinkFactor` after each of R/G/B), `gl_FragColor = pow(col + original, gamma)`. `iMouse` is in UV space; v4 sets `iMouse = (mouseX/width, (height - mouseY)/height)`.
- **No `unwrap()`/`expect()`** in non-test code unless documented; **no `as` casts** where `TryFrom` works (GPU size casts may reuse the `#[allow(clippy::as_conversions, clippy::cast_possible_truncation, reason="GPU buffer sizes")]` block Line's `post_process.rs` uses).
- **`///`/`//!` docs;** WGSL lives in `assets/shaders/dots/explode.wgsl` (never inline WGSL in Rust, per AGENTS.md).
- **Verification gates:** `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features --workspace -- -D warnings`; `cargo nextest run --workspace --all-features` + `cargo test --doc --workspace`; `cargo doc --no-deps --workspace --document-private-items`; `cargo xtask check-secrets`. Interactive `cargo rund` is the operator's (do NOT run it).
- **Commit messages:** `git commit -F` (no backticks).

## Reference material (read these)

- v4 shader: `.worktrees/v4/src/sketches/dots/shaders/explode/fragment.glsl` (+ `vertex.glsl`, `shader.ts` for the uniform defaults).
- `crates/wc-sketches/src/line/post_process.rs` (the full node/pipeline/plugin pattern to mirror).
- `assets/shaders/line/gravity.wgsl` (the WGSL fullscreen-triangle `vertex` stage + `fragment` entry-point + uniform `struct` pattern; the explode shader uses the same vertex stage and bind-group layout shape: `@group(0) @binding(0)` uniform, `@binding(1)` texture, `@binding(2)` sampler).
- `crates/wc-sketches/src/dots/mod.rs` + `dots/systems/` (where to wire the plugin + the OnEnter/OnExit insert/remove + the per-frame driver).

> **Note for both tasks — visual confirmation is the operator's.** The explode's *look* (does it emanate from the cursor, is the chromatic spiral right, does it match the v4 `dots1.png`/`dots3.png` "fabric" screenshots) cannot be auto-verified without `cargo rund`. Implement the math faithfully from the v4 GLSL, unit-test the param plumbing, and flag the visual as operator-deferred. In particular the `i_mouse` Y-convention (v4 flips y) and the fullscreen-triangle UV origin must be made self-consistent so the explode centers on the cursor — get them consistent with `gravity.wgsl`'s conventions and note it for the operator to confirm.

---

### Task 1: `DotsPostParams` + `explode.wgsl` + `DotsPostProcessPlugin` (the pass)

**Files:**
- Create: `crates/wc-sketches/src/dots/post_process.rs` (`DotsPostParams`, `DotsPostProcessPlugin`, `DotsPostProcessPipeline`, `dots_post_process` render system)
- Create: `assets/shaders/dots/explode.wgsl` (fullscreen-triangle `vertex` + explode `fragment`)
- Modify: `crates/wc-sketches/src/dots/mod.rs` (declare `pub mod post_process;`; add `DotsPostProcessPlugin`; insert `DotsPostParams` `OnEnter(Dots)`, remove `OnExit(Dots)`)

**Interfaces:**
- Produces: `crate::dots::post_process::DotsPostParams { i_resolution: [f32;2], i_mouse: [f32;2], shrink_factor: f32, gamma: f32 }` (`#[repr(C)]`, `Pod`, `Resource`, `ExtractResource`); `DotsPostProcessPlugin`.
- The render system runs in `Core2dSystems::EarlyPostProcess`, gated on `camera.hdr` AND `Option<Res<DotsPostParams>>` being present.

- [ ] **Step 1: Port the explode shader to `assets/shaders/dots/explode.wgsl`**

Mirror `assets/shaders/line/gravity.wgsl`'s structure: a `struct PostParams` matching `DotsPostParams` field order (`i_resolution: vec2<f32>, i_mouse: vec2<f32>, shrink_factor: f32, gamma: f32`); `@group(0) @binding(0) var<uniform> params`, `@binding(1) var scene_tex: texture_2d<f32>`, `@binding(2) var scene_sampler: sampler`; a fullscreen-triangle `@vertex fn vertex(...)` (copy gravity.wgsl's) that outputs a `uv`; and `@fragment fn fragment(...)`. Port v4 `fragment.glsl` faithfully:
- `explodedTexture2D(center, shrink)`: `offset = uv - center; samplePos = center + normalize(offset) * length(offset) * shrink;` sample `scene_tex` at `samplePos`; if `samplePos` is outside `[0,1]²` return `vec4(0.0)` else the sample. (Use `textureSampleLevel(scene_tex, scene_sampler, samplePos, 0.0)` — post-process samples need an explicit LOD in a non-uniform-control-flow fragment.)
- main: `center = params.i_mouse; original = sample at uv; col = vec4(0); shrink = 1.0;` loop 5×: `col.r += exploded(center, shrink).r / (i+1); shrink *= params.shrink_factor; col.g += ...; shrink *= ...; col.b += ...; shrink *= ...; center -= m2 * (center - vec2(0.5)) * 0.5928;` with `m2 = mat2x2<f32>(1.6, -1.2, 1.2, 1.6)`. Return `pow(col + original, vec4(params.gamma))`. Mind WGSL `pow` on negative bases is undefined — clamp the base to `max(col+original, vec4(0.0))` before `pow` (HDR values are ≥0 here, but guard defensively).

- [ ] **Step 2: Write `DotsPostParams` + the plugin/pipeline/node**

Create `crates/wc-sketches/src/dots/post_process.rs` mirroring `line/post_process.rs`: the `#[repr(C)] DotsPostParams` (Pod/Zeroable/Resource/ExtractResource) with a `const POST_PARAMS_SIZE: NonZeroU64`; `DotsPostProcessPlugin` (adds `ExtractResourcePlugin::<DotsPostParams>`, the `dots_post_process` system in `Core2dSystems::EarlyPostProcess`, and inits `DotsPostProcessPipeline` in `finish`); `DotsPostProcessPipeline` (FromWorld: bind-group layout `uniform_buffer_sized + texture_2d(filterable) + sampler(Filtering)`, a sampler, the pipeline targeting `Rgba16Float`, the persistent uniform buffer; loads `shaders/dots/explode.wgsl`, `vertex`/`fragment` entry points); the `dots_post_process` render system (gate `camera.hdr`, early-return on absent `DotsPostParams`/pipeline, `write_buffer` the params, `post_process_write()` source→dest, bind, `draw(0..3, 0..1)`). Do NOT `init_resource::<DotsPostParams>()` globally — it is inserted per-sketch (Step 3).

- [ ] **Step 3: Wire into `DotsPlugin` + per-sketch insert/remove**

In `dots/mod.rs`: `pub mod post_process;`; `app.add_plugins(post_process::DotsPostProcessPlugin)`. On `OnEnter(AppState::Dots)`, insert a `DotsPostParams` with sensible static values (`shrink_factor = 0.98`, `gamma = DotsSettings.gamma` read at enter or just `1.0` for now — the live driver lands in Task 2; `i_mouse = [0.5, 0.5]` center, `i_resolution` from the window). On `OnExit`, remove `DotsPostParams` (fold into `remove_dots_sim_params` or a sibling system). This makes the explode render in Dots with a fixed center until Task 2 drives it.

- [ ] **Step 4: Tests**

`DotsPostParams` is a render-world uniform with no pure logic, so cover what's testable without a GPU: a `dots_post_params_layout` test asserting `size_of::<DotsPostParams>()` matches the const and the field order (a compile-time `POST_PARAMS_SIZE` plus a runtime size assertion, mirroring Line's pattern); and a test that `OnEnter(Dots)` inserts `DotsPostParams` and `OnExit` removes it (drive the state transition in a `MinimalPlugins` app, or `RunSystemOnce` the insert/remove systems and assert resource presence). The render pass itself is operator-verified (no RenderApp under MinimalPlugins).

- [ ] **Step 5: Build, test, gates**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run -p wc-sketches --all-features
cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings
```

Expected: PASS. (The explode's actual appearance is operator-confirmed via `cargo rund` — not run here.)

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "feat(dots): explode post-process pass (DotsPostParams + explode.wgsl + node)" "" "v4 radial chromatic-aberration zoom around the pointer, gated to AppState::Dots via the per-sketch DotsPostParams resource. Static center until D3 Task 2 drives it." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

### Task 2: Per-frame explode driver (cursor + resolution + gamma)

**Files:**
- Create: `crates/wc-sketches/src/dots/systems/post_params.rs` (the `update_dots_post_params` driver)
- Modify: `crates/wc-sketches/src/dots/systems/mod.rs` (re-export); `crates/wc-sketches/src/dots/mod.rs` (register the driver in the `Update` chain)

**Interfaces:**
- Produces: `update_dots_post_params` (`Update`, `run_if(sketch_active(AppState::Dots))`).
- Consumes: `DotsPostParams` (writes it), `wc_core::input::pointer::PointerState`, `Single<&Window>`, `DotsSettings` (for `gamma`).

- [ ] **Step 1: Write the driver**

`update_dots_post_params` each frame writes `DotsPostParams`:
- `i_resolution = [window.width(), window.height()]`.
- `i_mouse` = the pointer in UV space matching the v4 convention: v4 `iMouse = (mouseX / width, (height - mouseY) / height)`. Read `PointerState.cursor` (window pixels, top-left origin, +y down — confirm against `mouse.rs`'s usage) and convert to that UV. Make the Y-flip consistent with the explode shader's UV origin (see the Task-1 note). When no cursor is present, keep the last value (or center) — do not zero it (zero = top-left corner explode).
- `shrink_factor = 0.98` (v4 default).
- `gamma = DotsSettings.gamma`.

- [ ] **Step 2: Register in the Update chain**

Add `update_dots_post_params` to Dots' `Update` set, `.run_if(sketch_active(AppState::Dots))`. It can join the existing chain or be its own system; it only WRITES `DotsPostParams` (the render world extracts it), so it has no ordering dependency on the sim-params writer — keep it simple.

- [ ] **Step 3: Tests**

In `post_params.rs` tests: insert `DotsPostParams::default()` (or the OnEnter value), a `DotsSettings { gamma: 1.7, .. }`, a `Window`, and a `PointerState` with a known cursor; run `update_dots_post_params`; assert `i_resolution` equals the window size, `gamma == 1.7`, `shrink_factor == 0.98`, and `i_mouse` equals the exact UV you computed for that cursor (pin the v4 Y-flip formula in the assertion so a future change to the convention is caught). Mirror the resource-driver test setup Line uses (read `crates/wc-sketches/tests/` or a `common` helper for `PointerState`/`Window` setup).

- [ ] **Step 4: Build, test, gates**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run -p wc-sketches --all-features
cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "feat(dots): drive explode post-process from cursor + resolution + gamma" "" "update_dots_post_params writes i_mouse (v4 UV convention), i_resolution, and gamma each frame so the explode spiral tracks the pointer." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

## Self-Review

**Spec coverage** (against `2026-06-22-dots-fabric-design.md` §"explode post-process" + D3 row):
- `assets/shaders/dots/explode.wgsl` + `DotsPostProcessPlugin` mirroring `line/post_process.rs` → Task 1. ✓
- 5-iteration radial chromatic-aberration, spiral, `pow(col, gamma)`, `shrink_factor 0.98` → Task 1 Step 1 (faithful v4 port). ✓
- `DotsPostParams { i_mouse, i_resolution, shrink_factor, gamma }` → Task 1. ✓
- Gated to Dots (per-sketch resource) → Task 1 Step 3 (insert/remove). ✓
- Cursor-driven i_mouse with v4 Y-flip → Task 2. ✓

**Placeholder scan:** No TBD-as-deliverable. The shader is ported from the named v4 GLSL with the exact algorithm spelled out; the node mirrors a named in-repo file. The one genuinely-deferred item — the explode's visual correctness (cursor centering / look vs the v4 screenshots) — is explicitly operator-confirmed (no GPU under MinimalPlugins), with the math/plumbing unit-tested.

**Type consistency:** `DotsPostParams { i_resolution, i_mouse, shrink_factor, gamma }`, `DotsPostProcessPlugin`, `DotsPostProcessPipeline`, `dots_post_process`, `update_dots_post_params` — consistent across both tasks. The WGSL `struct PostParams` field order matches `DotsPostParams` (`#[repr(C)]` byte compatibility — same discipline as `SimParams`/`LinePostParams`).

**Risks:** (1) The HDR-target/format gate must match the Dots camera (the main `Camera2d` is HDR — same camera Line uses; the explode targets `Rgba16Float` like Line's smear). (2) The `i_mouse` UV/Y convention is the one visual-correctness detail that can't be unit-confirmed — pinned by a Task-2 test on the formula and flagged for operator confirmation. (3) `pow` on a negative base is guarded by clamping the accumulator to ≥0.
