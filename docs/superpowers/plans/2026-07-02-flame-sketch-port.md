# Flame Sketch Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port v4's Flame IFS-fractal sketch to v5 (Rust/Bevy) at perceptual parity, with the IFS evaluated entirely on the GPU (level-parallel compute), envelope/DSP-approximated generative audio, a name-input overlay with a persisted editable carousel list, and a name-carousel + ember-decay attract mode.

**Architecture:** The fractal's ~100k nodes live in one persistent level-ordered `ShaderBuffer`; a render-graph compute pass runs **one dispatch per tree level** (5–16 per frame, sequential — the Cymatics multi-dispatch pattern with a storage *buffer* instead of ping-pong textures), each computing `state[i] = lerp(state[i], apply_branch(state[parent(i)]), 0.8)`. Rendering needs **no `Camera3d`**: a custom `Material2d` receives the orbit camera as two mat4 uniforms and does perspective projection + screen-space billboarding + fake-DoF sizing in its vertex shader, drawing additive quads (blend override via `specialize`) into the existing global HDR `Camera2d` — bloom, tonemapping, `apply_render_profile`, and the shared hand-mesh overlay all work unchanged. Audio is a new fundsp `FlameSynth` fed two per-frame scalars (morph-energy, camera distance) over the lock-free ring; all v4 mapping curves live inside the synth. Name → fractal generation (hash, PRNG, branch selection) is ported with **f64 arithmetic to reproduce v4's JavaScript float semantics bit-for-bit**, golden-tested against values generated from the v4 source.

**Tech Stack:** Rust, Bevy 0.19 (`Material2d` + `specialize` blend override, `ShaderBuffer` storage asset, render-graph-as-systems compute, dynamic-offset uniforms, `ExtractResourcePlugin`), WGSL, fundsp (DSP), cpal + rtrb (audio thread), egui via `bevy_egui` (name-input overlay, TextList widget).

## Approved deviations from the spec (discovered during planning)

Two refinements to `docs/superpowers/specs/2026-07-02-flame-sketch-port-design.md`, both reducing risk with no product-visible change (Task F17 amends the spec and records them in PARITY.md):

1. **No `Camera3d`.** The spec's "perspective HDR `Camera3d` under `FlameRoot`" is replaced by in-material projection (see Architecture). Rationale: the app runs exactly one window camera (the global HDR `Camera2d`, `crates/waveconductor/src/main.rs:214`); `apply_render_profile` filters on `With<Camera2d>` (`crates/wc-core/src/sketch/lifecycle.rs:170`); a second window camera would re-open the shared-MSAA-texture landmine documented on the main camera. The hand-mesh-on-3D-camera risk from the spec's risk table disappears entirely.
2. **No single-branch CPU-chain carve-out.** v4's `FlameNameInput` substitutes `DEFAULT_NAME` for empty input (`.worktrees/v4/src/sketches/flame/FlameNameInput.tsx:9-10`), and `updateName` is never called with a name shorter than 1 char — so `numBranches = ceil(1 + len%5 + wraps)` is always **2–8**. The 1000-node chain branch of v4's `computeDepth` is dead code. v5 mirrors the trim-or-default normalization (`normalize_name`) and asserts `branch_count >= 2`.

Two v4 float quirks discovered while generating goldens, **ported faithfully** (f64 reproduces them):

- For names longer than ~4 chars, `stringHash` returns a double so large it is a multiple of 1024, so `hashNorm = (hash % 1024)/1024 = 0` and `cY = -2.5` for most names. Only very short names get varied `cY`.
- `hash2 % 2 === 0` for all but very short names (float truncation), so `isMajor` is almost always true.

## Global Constraints

Every task's requirements implicitly include this section. Values are copied verbatim from the spec and AGENTS.md.

- **No new dependencies.** Reuse crates already in the graph (`cargo tree -i <crate>` to confirm). fundsp, rtrb, bytemuck, egui are all present.
- **GPU-only / no readback.** The node buffer lives on the GPU. Audio reads only CPU-side scalars (analytic `|dcX/dt|`, warp deltas, camera distance, name-change config). Never read GPU → CPU.
- **f64 for name→fractal math.** `string_hash`, the PRNG, and all hash-derived audio config use `f64` end-to-end so the same name produces the same fractal and timbre as v4 (JS numbers are f64; `*`, `+`, `%` are IEEE-exact in both languages). Hashing operates on **UTF-16 code units** (`str::encode_utf16`), matching JS `charCodeAt`.
- **No hot-path allocation.** Per-frame systems, the audio callback, and the compute prepare path pre-allocate and reuse. Name-change rebuild (branch build, node-buffer reseed, mesh rebuild) allocates — acceptable, it is event-driven and rare, like `LineSynth` graph construction.
- **Audio thread is real-time-safe.** Lock-free rtrb ring only, no `Mutex`, no allocation after `FlameSynth::new`. `AudioCommand` stays `Copy` — param keys are `&'static str`.
- **Zero systems when idle.** Every `Update` system gated `sketch_active(AppState::Flame)`; attract systems gated `in_screensaver(AppState::Flame)`; `OnEnter(SketchActivity::Idle)` zeroes the dispatch level count so the compute pass does no work while frozen (v4 froze on idle too). GPU resources owned by `FlameRoot`-tagged entities, despawned `OnExit`; `FlameSimParams` removed `OnExit` with a manual `ExtractSchedule` removal companion (the `ExtractResourcePlugin` no-removal-propagation landmine).
- **Kernel parity discipline.** The WGSL variation/affine kernel and the Rust mirror in `branches.rs` change together term-for-term. The Rust mirror is the unit-test reference; goldens come from v4.
- **No `unwrap()`/`expect()`** in non-test code unless a documented invariant. No `as` casts where `From`/`TryFrom`/`u32::try_from` work (`as` on f32↔f64 or f64→index after explicit floor/clamp is acceptable where documented, matching JS semantics).
- **Docs:** `///` on every public item, `//!` on every module root, signal/data flow at `FlamePlugin::build()`, inline `//` for the IFS/DSP/shader math (explain each term).
- **One concept per file**, files ~300 lines guideline, shaders external (never inline WGSL in Rust).
- **No em dashes in user-facing copy** (manifest display name, overlay placeholder, settings labels). Internal docs/comments/commit messages may use them.
- **WebGPU-only target.** Storage-buffer read_write in compute is core WebGPU (no downlevel concern, unlike rgba32float textures).
- **Verification gate (run before claiming any task done):** `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features --workspace -- -D warnings`; `cargo nextest run --workspace --all-features` (+ `cargo test --doc --workspace`); `cargo doc --no-deps --workspace --document-private-items`; `cargo deny check`; `cargo xtask check-secrets`. Dev iteration uses `cargo rund`.
- **Pre-build before capture:** `cargo build -p waveconductor` first, so `cargo xtask capture` launches an already-compiled binary.
- **Commit messages:** never put backticks in `-m` strings (shell substitution); plain words only, or `-F` a file.

---

## File Structure

**New files (`crates/wc-sketches/src/flame/`):**
- `mod.rs` — `FlamePlugin::build` (settings, manifest, lifecycle, idle veto, hand-mesh, sub-plugins, clear-color swap). `FlameRoot` marker lives in `systems/spawn.rs`.
- `branches.rs` — name normalization, `string_hash` (f64/UTF-16), PRNG, `BranchSpec`/`FlameSpec` builder, affine matrix tables, CPU variation mirror, `NameAudioConfig` (incl. pseudo-density → chord degree). Pure, headless-testable.
- `levels.rs` — depth formula, branch-major level layout (`LevelLayout`/`LevelSpan`), parent-index arithmetic, ember prefix math. Pure.
- `settings.rs` — `FlameSettings` (User: name, carousel list, gamma, brightness, synth volume; Dev: point budget, camera, DoF, fog, attract, audio-tuning knobs) + `SketchLifecycle` impl.
- `compute/mod.rs` — `pub mod pipeline; pub mod sim_params;`
- `compute/sim_params.rs` — `FlameNodeGpu`, `FlameBranchGpu`, `FlameSimParamsGpu`, `FlameLevelParamsGpu` PODs (+ size asserts), `FlameSimParams` extract resource, branch→GPU encoding.
- `compute/pipeline.rs` — `FlameComputePlugin`: pipeline init, per-level dynamic-offset uniform slots, bind-group caching keyed on buffer id, the per-level dispatch loop, removal companion.
- `render.rs` — `FlameMaterial` (`Material2d`, additive blend via `specialize`), uniform packers.
- `systems/mod.rs` — re-exports.
- `systems/spawn.rs` — `FlameRoot`, node-buffer + mesh + material spawn, `OnExit` resource removal.
- `systems/name_change.rs` — settings watcher: normalize → `build_flame_spec` → encode branches → reseed buffer → rebuild mesh → push audio config + duck.
- `systems/sim_params.rs` — per-frame writer: virtual-time `cX` oscillation, warp from pointer/hands, level count; idle freeze; `WC_DEBUG_FORCE_FLAME_WARP` pin.
- `systems/camera.rs` — `FlameCamera` orbit resource (azimuth/polar/distance/momentum), autorotate + drag + wheel zoom + fling, view/proj matrix builders, material uniform writer.
- `systems/hands.rs` — grab-and-fling from `TrackedHand`, grab→warp mapping, `flame_idle_veto`.
- `ui.rs` — centered name-input overlay (Active) + ghost seed label (Screensaver) + debounced carousel admission.
- `audio_coupling.rs` — morph-energy envelope (analytic `|dcX/dt|` + warp speed), camera distance push, enter/exit/config wiring.
- `screensaver.rs` — `FlameScreensaverPlugin`: carousel driver, ember complexity + brightness lift on `ScreensaverFade`, screensaver audio fade.
- `PARITY.md` — parity record (Task F17).

**New files (`crates/wc-core/src/audio/`):**
- `flame_synth.rs` — `FlameSynth` fundsp voice (noise + DC-through-resonant-lowpass "osc" voice + 5-osc chord + tanh shaper) with in-synth v4 mapping curves.

**Modified (`crates/wc-core/`):**
- `src/audio/command.rs` — `AddFlameSynth`, `RemoveFlameSynth`, `SetFlameParam`; `AudioMessage::{FlameSynthActivated, FlameSynthDeactivated}`.
- `src/audio/dsp.rs` — `flame_synth: Option<FlameSynth>` field, `apply` arms, `render` mix, `Debug` impl.
- `src/audio/engine.rs` — echo arms for the new commands.
- `src/audio/state.rs` — `pump_audio_messages` arms + `AudioState::flame_synth_active`.
- `src/audio/mod.rs` — `pub mod flame_synth;`.
- `src/lifecycle/state.rs` — `SKETCH_ORDER` 4 entries, `from_name` flame arm, `next_sketch`/`prev_sketch` rewiring, test updates.
- `src/lifecycle/actions.rs` — `SelectFlame` variant, `ALL` → 11.
- `src/lifecycle/action_map.rs` — `(SelectFlame, Key(Digit2))` binding.
- `src/lifecycle/nav.rs` — `SelectFlame` arm.
- `src/settings/def.rs` — `SettingKind::TextList`.
- `src/settings/panel_user/widgets.rs` — `render_text_list` widget + dispatch arm.
- `src/debug/mod.rs` — `force_flame_warp` toggle.
- `tests/ui_picker.rs`, `tests/lifecycle.rs` — guard-test updates.

**Modified (`crates/wc-core-macros/`):** `src/lib.rs` — `Kind::TextList` (parse + emit + doc table); `tests/derive.rs` — TextList fixture.

**Modified (`crates/wc-sketches/src/lib.rs`):** register `Material2dPlugin::<FlameMaterial>`, `FlameComputePlugin`, `FlamePlugin`.

**New shaders:** `assets/shaders/flame/simulate.wgsl`, `assets/shaders/flame/render.wgsl`.

**New assets:** `assets/sketches/flame/disc.png` (copied from v4), `assets/sketches/flame/screenshot.png` (copied from v4 `screenshots/flame.png`).

**New scenarios:** `tests/visual/scenarios.toml` — `flame-synthetic`, `flame-warp`, `flame-screensaver`.

**Test-literal touch-ups (DebugToggles gained a field):** `crates/wc-sketches/src/line/mod.rs`, `crates/wc-sketches/src/dots/mod.rs`, `crates/wc-core/src/lifecycle/screensaver/mod.rs`, `crates/wc-core/src/capture/system.rs` (each holds a full `DebugToggles { .. }` literal in tests).

---

## Golden values (generated from v4 source, 2026-07-02)

Generated by a dependency-free Node script mirroring `.worktrees/v4/src/sketches/flame/index.tsx` exactly (archived at `docs/superpowers/plans/assets/2026-07-02-flame-goldens.mjs` in Task F1). JS doubles round-trip exactly through the shortest-repr literals below. Used throughout Stage 1 tests.

```text
PRNG (seed string "who ", seed_hash 412668525337596):
  sequence = [1192329537, 156370942, 1983029636, 1795717194,
              665652336, 1952893588, 819161423, 587530468]

"who are you?" : wraps 2, branches 5, depth 7, cY -2.5,
  filter_freq 173.66857531392, filter_q 5.278918295552,
  noise_gain_scale 0.7, is_major true, has_noise true
  b0 sub "wh" : affine Negate(4),        varA Spherical(2), mode interp, varB Sin(1),
     color [0.29498686512166555, 0.00762611990484428, -0.02811902919097892]
  b1 sub "o " : affine Up1(6),           varA Linear(0),    mode interp, varB Shrink(6),
     color [0.06939554413951993, 0.22468531498105382, 0.004966701616051932]
  b2 sub "are": affine TowardsOrigin2(1), varA Polar(3),    mode single,
     color [-0.04277928677783048, -0.005155820568231251, 0.3548946429831816]
  b3 sub " y" : affine Up1(6),           varA Normalize(5), mode single,
     color [0.26517298507345916, -0.010330283852847023, -0.05484055367723399]
  b4 sub "ou?": affine NegateSwap(5),    varA Normalize(5), mode single,
     color [-0.02929900697732431, 0.22999513901703367, 0.015850284044428058]

"madison" : wraps 1, branches 4, depth 8, cY -2.5,
  filter_freq 187.40572618752, filter_q 7.749201502208,
  noise_gain_scale 0.94, is_major true, has_noise true
  b0 sub "m" : affine TowardsOrigin2(1), varA Sin(1), mode interp, varB Normalize(5),
     color [0.23718786317191987, 0.051779865417793065, -0.012281404680836532]
  b1 sub "ad": affine TowardsOriginNegativeBias(0), varA Linear(0), mode single,
     color [0.05382216769515105, 0.19371599619915525, 0.04114896925751937]
  b2 sub "is": affine Swap(2), varA Swirl(4), mode single,
     color [0.00695912825001629, 0.006782149127516568, 0.21700342544481055]
  b3 sub "on": affine TowardsOriginNegativeBias(0), varA Shrink(6), mode single,
     color [0.18445540103071423, 0.013337905897450588, 0.018719401458746333]

"a" : wraps 0, branches 2, depth 16, hash 291679, hash2 85085681099,
  cY 1.7138671875, filter_freq 131.91199535386, filter_q 6.077572800512,
  noise_gain_scale 0.64, is_major false, has_noise false
  b0 sub ""  : affine TowardsOriginNegativeBias(0), varA Normalize(5), mode single,
     color [0.10630441737388732, 0.02241883682066654, 0.02310486406365503]
  b1 sub "a" : affine Up1(6), varA Shrink(6), mode single,
     color [0.00502685687871836, 0.10374412529477908, 0.010241985472164912]

"xy" : wraps 0, branches 3, depth 10, hash 457351711, cY 0.1513671875,
  is_major true, has_noise true
"abcdefghijklmnopqrs" : wraps 3, branches 8, depth 5, cY -2.5, is_major true,
  has_noise false; b1 sub "cd" is the router-mode golden:
  affine NegateSwap(5), varA Normalize(5), mode router, varB Linear(0),
  color [-0.08664759715530311, 0.4661658770845911, 0.02246783094728596]
"Xiaohan" : wraps 1, branches 4, depth 8, cY -2.5, filter_freq 330.09130684416004,
  is_major true, has_noise true

Depth/total-node cross-check: b=2,d=16 -> 131071; b=3,d=10 -> 88573; b=4,d=8 -> 87381;
b=5,d=7 -> 97656; b=8,d=5 -> 37449. All <= 200_000 capacity.
```

---

## Stage 1 — Core math (pure Rust, no Bevy)

### Task F1: Name hashing, PRNG, and branch generation (`branches.rs`)

**Files:**
- Create: `crates/wc-sketches/src/flame/mod.rs` (module skeleton only: `pub mod branches;`)
- Create: `crates/wc-sketches/src/flame/branches.rs`
- Create: `docs/superpowers/plans/assets/2026-07-02-flame-goldens.mjs` (the generator script, for future re-derivation)
- Modify: `crates/wc-sketches/src/lib.rs` (add `pub mod flame;`)
- Test: `#[cfg(test)] mod tests` in `branches.rs`

**Interfaces:**
- Produces: `pub const DEFAULT_NAME: &str = "who are you?"`; `pub const MAX_BRANCHES: usize = 8`; `pub fn normalize_name(raw: &str) -> &str` (trim; empty → `DEFAULT_NAME`); `pub fn string_hash(name: &str) -> f64`; `pub enum VariationId { Linear = 0, Sin, Spherical, Polar, Swirl, Normalize, Shrink }` (`#[repr(u32)]`, `Copy`); `pub enum VariationMode { Single = 0, Interpolated, Router }` (`#[repr(u32)]`, `Copy`); `pub struct BranchSpec { pub affine_idx: usize, pub var_a: VariationId, pub var_b: VariationId, pub mode: VariationMode, pub color: [f32; 3] }`; `pub struct NameAudioConfig { pub filter_freq: f32, pub filter_q: f32, pub noise_gain_scale: f32, pub is_major: bool, pub has_noise: bool, pub pseudo_density: f32, pub chord_degree: f32 }`; `pub struct FlameSpec { pub branches: Vec<BranchSpec>, pub c_y: f32, pub audio: NameAudioConfig }`; `pub fn build_flame_spec(name: &str) -> FlameSpec`; `pub const AFFINE_MATS: [[f32; 9]; 7]` (row-major) and `pub const AFFINE_OFFSETS: [[f32; 3]; 7]`; `pub fn apply_variation_cpu(id: VariationId, p: [f32; 3]) -> [f32; 3]` (the kernel mirror); `pub fn apply_branch_cpu(spec: &BranchSpec, warp: [f32; 2], p: [f32; 3]) -> [f32; 3]`.
- Consumes: nothing (pure).

- [ ] **Step 1: Archive the golden generator**

Copy the generator script (below, exactly) to `docs/superpowers/plans/assets/2026-07-02-flame-goldens.mjs`. It documents how the goldens in this plan's header were produced and lets anyone re-derive them with `node`:

```js
// Golden-value generator for the Flame v4->v5 port.
// Mirrors .worktrees/v4/src/sketches/flame/index.tsx (stringHash, randomBranch*)
// and src/math.ts (map) EXACTLY, with THREE.js scalar math inlined.
// All arithmetic is IEEE-754 f64, which Rust reproduces bit-for-bit.
const GEN_DIVISOR = 2147483648 - 1; // 2^31 - 1
const AFFINE_KEYS = ["TowardsOriginNegativeBias", "TowardsOrigin2", "Swap", "SwapSub", "Negate", "NegateSwap", "Up1"];
const VAR_KEYS = ["Linear", "Sin", "Spherical", "Polar", "Swirl", "Normalize", "Shrink"];
function stringHash(s) {
    let hash = 0, char;
    if (s.length === 0) { return hash; }
    for (let i = 0, l = s.length; i < l; i++) {
        char = s.charCodeAt(i);
        hash = hash * 31 + char;
        hash |= 0; // ToInt32 wrap
    }
    hash *= hash * 31;
    return hash;
}
function map(x, xStart, xStop, yStart, yStop) {
    return yStart + (yStop - yStart) * ((x - xStart) / (xStop - xStart));
}
function randomBranch(idx, substring, numBranches, numWraps) {
    let gen = stringHash(substring);
    function next() { return (gen = (gen * 4194303 + 127) % GEN_DIVISOR); }
    for (let i = 0; i < 5 + idx * numWraps; i++) { next(); }
    const newVariationIdx = () => { next(); return gen % VAR_KEYS.length; };
    const random = () => { next(); return gen / GEN_DIVISOR; };
    const affineIdx = gen % AFFINE_KEYS.length; // gen as left by the skip loop
    const varA = newVariationIdx();
    let mode = 0, varB = -1;
    if (random() < numWraps * 0.25) {
        mode = 1; varB = newVariationIdx();
    } else if (numWraps > 2 && random() < 0.2) {
        mode = 2; varB = newVariationIdx();
    }
    const colorValues = [random() * 0.1 - 0.05, random() * 0.1 - 0.05, random() * 0.1 - 0.05];
    colorValues[idx % 3] += 0.2;
    const scale = numBranches / 3.5;
    return { affine_idx: affineIdx, var_a_idx: varA, mode, var_b_idx: varB,
             color: colorValues.map((c) => c * scale) };
}
function goldensFor(name) {
    const numWraps = Math.floor(name.length / 5);
    const numBranches = Math.ceil(1 + (name.length % 5) + numWraps);
    const branches = [];
    for (let i = 0; i < numBranches; i++) {
        const stringStart = map(i, 0, numBranches, 0, name.length);
        const stringEnd = map(i + 1, 0, numBranches, 0, name.length);
        const substring = name.substring(stringStart, stringEnd);
        branches.push({ substring, ...randomBranch(i, substring, numBranches, numWraps) });
    }
    const hash = stringHash(name);
    const hashNorm = (hash % 1024) / 1024;
    const hash2 = hash * hash + hash * 31 + 9;
    const hash3 = hash2 * hash2 + hash2 * 31 + 9;
    return {
        name, num_wraps: numWraps, num_branches: numBranches,
        depth: Math.floor(Math.log(100000) / Math.log(numBranches)),
        hash, hash2, hash3,
        c_y: map(hashNorm, 0, 1, -2.5, 2.5),
        filter_freq: map((hash2 % 2e12) / 2e12, 0, 1, 120, 400),
        filter_q: map((hash3 % 2e12) / 2e12, 0, 1, 5, 8),
        noise_gain_scale: map(((hash2 * hash3) % 100) / 100, 0, 1, 0.5, 1),
        is_major: hash2 % 2 === 0,
        has_noise: (hash3 % 100) >= 50,
        branches,
    };
}
function prngSequence(seedString, n) {
    let gen = stringHash(seedString);
    const out = [];
    for (let i = 0; i < n; i++) { gen = (gen * 4194303 + 127) % GEN_DIVISOR; out.push(gen); }
    return { seed_string: seedString, seed_hash: stringHash(seedString), sequence: out };
}
const result = {
    prng: prngSequence("who ", 8),
    names: ["who are you?", "madison", "a", "xy", "abcdefghijklmnopqrs", "Xiaohan"].map(goldensFor),
};
console.log(JSON.stringify(result, null, 1));
```

- [ ] **Step 2: Write the failing tests**

Create `crates/wc-sketches/src/flame/mod.rs`:

```rust
//! Flame sketch: a name-seeded IFS fractal flame, evaluated level-parallel on
//! the GPU and drawn as an additive point cloud with a fake depth of field.
//!
//! Modules are added stage by stage (see the 2026-07-02 flame port plan).

pub mod branches;
```

Add to `crates/wc-sketches/src/lib.rs` next to the other sketch modules:

```rust
pub mod flame;
```

In `branches.rs`, write the test module first (implementation stubs come in Step 4):

```rust
#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "golden parity with v4 requires bit-exact f64/f32 comparison"
)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// PRNG golden: seed string "who ", first 8 draws. Values generated from
    /// the v4 source via docs/superpowers/plans/assets/2026-07-02-flame-goldens.mjs.
    #[test]
    fn prng_matches_v4_sequence() {
        assert_eq!(string_hash("who "), 412_668_525_337_596.0);
        let mut gen = string_hash("who ");
        let expected: [f64; 8] = [
            1_192_329_537.0,
            156_370_942.0,
            1_983_029_636.0,
            1_795_717_194.0,
            665_652_336.0,
            1_952_893_588.0,
            819_161_423.0,
            587_530_468.0,
        ];
        for want in expected {
            assert_eq!(prng_next(&mut gen), want);
        }
    }

    /// stringHash edge cases: empty string is 0; the int32 wrap and the final
    /// f64 squaring both match v4 exactly.
    #[test]
    fn string_hash_matches_v4() {
        assert_eq!(string_hash(""), 0.0);
        assert_eq!(string_hash("a"), 291_679.0);
        assert_eq!(string_hash("xy"), 457_351_711.0);
        assert_eq!(string_hash("who are you?"), 7_885_686_694_543_608_000.0);
    }

    #[test]
    fn normalize_name_trims_and_defaults() {
        assert_eq!(normalize_name("  madison  "), "madison");
        assert_eq!(normalize_name(""), DEFAULT_NAME);
        assert_eq!(normalize_name("   "), DEFAULT_NAME);
    }

    /// Full branch-generation golden for the default name. Asserts branch
    /// count, per-branch affine/variation selection, combinator mode, and
    /// bit-exact colors (f32-rounded from the v4 f64 values).
    #[test]
    fn default_name_branches_match_v4() {
        let spec = build_flame_spec("who are you?");
        assert_eq!(spec.branches.len(), 5);
        assert_eq!(spec.c_y, -2.5);

        let b0 = &spec.branches[0];
        assert_eq!(b0.affine_idx, 4); // Negate
        assert_eq!(b0.var_a, VariationId::Spherical);
        assert_eq!(b0.mode, VariationMode::Interpolated);
        assert_eq!(b0.var_b, VariationId::Sin);
        assert_eq!(
            b0.color,
            [
                0.294_986_865_121_665_55_f64 as f32,
                0.007_626_119_904_844_28_f64 as f32,
                -0.028_119_029_190_978_92_f64 as f32,
            ]
        );

        let b2 = &spec.branches[2];
        assert_eq!(b2.affine_idx, 1); // TowardsOrigin2
        assert_eq!(b2.var_a, VariationId::Polar);
        assert_eq!(b2.mode, VariationMode::Single);

        let b4 = &spec.branches[4];
        assert_eq!(b4.affine_idx, 5); // NegateSwap
        assert_eq!(b4.var_a, VariationId::Normalize);
    }

    /// The 19-char name exercises the router combinator (numWraps > 2) and the
    /// 8-branch maximum.
    #[test]
    fn nineteen_char_name_hits_router_and_max_branches() {
        let spec = build_flame_spec("abcdefghijklmnopqrs");
        assert_eq!(spec.branches.len(), 8);
        let b1 = &spec.branches[1]; // substring "cd"
        assert_eq!(b1.affine_idx, 5); // NegateSwap
        assert_eq!(b1.var_a, VariationId::Normalize);
        assert_eq!(b1.mode, VariationMode::Router);
        assert_eq!(b1.var_b, VariationId::Linear);
    }

    /// Branch counts are always 2..=8 for any non-empty trimmed name (the v4
    /// name input substitutes the default for empty, so 1 branch is unreachable).
    #[test]
    fn branch_count_bounds() {
        for name in ["a", "ab", "abcd", "abcde", "abcdefghij", "abcdefghijklmnopqrst"] {
            let n = build_flame_spec(name).branches.len();
            assert!((2..=8).contains(&n), "{name}: {n} branches");
        }
    }

    /// Audio config goldens (f64 hash math), including the two v4 float
    /// quirks: cY collapses to -2.5 for long names, is_major true for all but
    /// tiny names.
    #[test]
    fn audio_config_matches_v4() {
        let who = build_flame_spec("who are you?").audio;
        assert_eq!(who.filter_freq, 173.668_575_313_92_f64 as f32);
        assert_eq!(who.filter_q, 5.278_918_295_552_f64 as f32);
        assert_eq!(who.noise_gain_scale, 0.7_f64 as f32);
        assert!(who.is_major);
        assert!(who.has_noise);

        let a = build_flame_spec("a").audio;
        assert!(!a.is_major, "short-name hash math is exact; 'a' is minor");
        assert!(!a.has_noise);

        let xiaohan = build_flame_spec("Xiaohan").audio;
        assert_eq!(xiaohan.filter_freq, 330.091_306_844_160_04_f64 as f32);
    }

    /// cY: short names get varied values (exact math), long names collapse to
    /// -2.5 (hash is a multiple of 1024 once the double exceeds ~2^53).
    #[test]
    fn c_y_matches_v4_including_float_quirk() {
        assert_eq!(build_flame_spec("a").c_y, 1.713_867_187_5);
        assert_eq!(build_flame_spec("xy").c_y, 0.151_367_187_5);
        assert_eq!(build_flame_spec("madison").c_y, -2.5);
        assert_eq!(build_flame_spec("who are you?").c_y, -2.5);
    }

    /// Affine tables must equal v4's closed forms. Spot-check each of the 7
    /// affines by applying matrix+offset to a probe point and comparing with
    /// the hand-derived v4 expression.
    #[test]
    fn affine_tables_match_v4_formulas() {
        let p = [0.3_f32, -0.7, 1.1];
        let apply = |idx: usize| -> [f32; 3] {
            let m = &AFFINE_MATS[idx];
            let o = &AFFINE_OFFSETS[idx];
            [
                m[0] * p[0] + m[1] * p[1] + m[2] * p[2] + o[0],
                m[3] * p[0] + m[4] * p[1] + m[5] * p[2] + o[1],
                m[6] * p[0] + m[7] * p[1] + m[8] * p[2] + o[2],
            ]
        };
        // 0 TowardsOriginNegativeBias: ((x-1)/2 + 0.25, (y-1)/2, z/2)
        let got = apply(0);
        assert!((got[0] - ((p[0] - 1.0) / 2.0 + 0.25)).abs() < 1e-7);
        assert!((got[1] - (p[1] - 1.0) / 2.0).abs() < 1e-7);
        assert!((got[2] - p[2] / 2.0).abs() < 1e-7);
        // 2 Swap: ((y+z)/2.5, (x+z)/2.5, (x+y)/2.5)
        let got = apply(2);
        assert!((got[0] - (p[1] + p[2]) / 2.5).abs() < 1e-7);
        assert!((got[1] - (p[0] + p[2]) / 2.5).abs() < 1e-7);
        assert!((got[2] - (p[0] + p[1]) / 2.5).abs() < 1e-7);
        // 3 SwapSub: ((y-z)/2, (z-x)/2, (x-y)/2)
        let got = apply(3);
        assert!((got[0] - (p[1] - p[2]) / 2.0).abs() < 1e-7);
        assert!((got[1] - (p[2] - p[0]) / 2.0).abs() < 1e-7);
        assert!((got[2] - (p[0] - p[1]) / 2.0).abs() < 1e-7);
        // 4 Negate: (-x, -y, -z)
        assert_eq!(apply(4), [-p[0], -p[1], -p[2]]);
        // 5 NegateSwap: ((-x+y+z)/2.1, (-y+x+z)/2.1, (-z+x+y)/2.1)
        let got = apply(5);
        assert!((got[0] - (-p[0] + p[1] + p[2]) / 2.1).abs() < 1e-7);
        // 6 Up1: (x, y, z+1)
        assert_eq!(apply(6), [p[0], p[1], p[2] + 1.0]);
        // 1 TowardsOrigin2: ((x+1)/2, (y-1)/2 - 0.1, (z+1)/2 - 0.1)
        let got = apply(1);
        assert!((got[0] - (p[0] + 1.0) / 2.0).abs() < 1e-7);
        assert!((got[1] - ((p[1] - 1.0) / 2.0 - 0.1)).abs() < 1e-7);
        assert!((got[2] - ((p[2] + 1.0) / 2.0 - 0.1)).abs() < 1e-7);
    }

    /// CPU variation mirror matches v4's formulas, including the zero-length
    /// guards THREE.js applies (normalize/setLength of a zero vector is a no-op).
    #[test]
    fn variations_match_v4_formulas() {
        let p = [0.5_f32, -0.25, 0.75];
        // Sin
        let got = apply_variation_cpu(VariationId::Sin, p);
        assert_eq!(got, [p[0].sin(), p[1].sin(), p[2].sin()]);
        // Spherical: p / |p|^2, zero-safe
        let l2 = p[0] * p[0] + p[1] * p[1] + p[2] * p[2];
        let got = apply_variation_cpu(VariationId::Spherical, p);
        assert!((got[0] - p[0] / l2).abs() < 1e-7);
        assert_eq!(
            apply_variation_cpu(VariationId::Spherical, [0.0; 3]),
            [0.0; 3]
        );
        // Polar: (atan2(y,x)/pi, |p| - 1, atan2(z,x))
        let got = apply_variation_cpu(VariationId::Polar, p);
        assert!((got[0] - p[1].atan2(p[0]) / std::f32::consts::PI).abs() < 1e-7);
        assert!((got[1] - (l2.sqrt() - 1.0)).abs() < 1e-7);
        assert!((got[2] - p[2].atan2(p[0])).abs() < 1e-7);
        // Swirl
        let r2 = l2;
        let got = apply_variation_cpu(VariationId::Swirl, p);
        assert!((got[0] - (p[2] * r2.sin() - p[1] * r2.cos())).abs() < 1e-6);
        assert!((got[1] - (p[0] * r2.cos() + p[2] * r2.sin())).abs() < 1e-6);
        assert!((got[2] - (p[0] * r2.sin() - p[1] * r2.sin())).abs() < 1e-6);
        // Normalize, zero-safe
        let got = apply_variation_cpu(VariationId::Normalize, p);
        let len = l2.sqrt();
        assert!((got[0] - p[0] / len).abs() < 1e-7);
        assert_eq!(
            apply_variation_cpu(VariationId::Normalize, [0.0; 3]),
            [0.0; 3]
        );
        // Shrink: setLength(exp(-|p|^2)), zero-safe
        let got = apply_variation_cpu(VariationId::Shrink, p);
        let want_len = (-l2).exp();
        let got_len = (got[0] * got[0] + got[1] * got[1] + got[2] * got[2]).sqrt();
        assert!((got_len - want_len).abs() < 1e-6);
        assert_eq!(apply_variation_cpu(VariationId::Shrink, [0.0; 3]), [0.0; 3]);
        // Linear: identity
        assert_eq!(apply_variation_cpu(VariationId::Linear, p), p);
    }

    /// apply_branch_cpu = affine -> +warp on x/y -> variation, matching v4's
    /// randomBranch closure order (warp is added AFTER the base affine and
    /// BEFORE the variation).
    #[test]
    fn apply_branch_order_is_affine_warp_variation() {
        let spec = BranchSpec {
            affine_idx: 6, // Up1: identity + (0,0,1)
            var_a: VariationId::Sin,
            var_b: VariationId::Sin,
            mode: VariationMode::Single,
            color: [0.0; 3],
        };
        let warp = [0.4_f32, -0.3];
        let p = [0.2_f32, 0.5, -0.1];
        let got = apply_branch_cpu(&spec, warp, p);
        let expect = [
            (p[0] + warp[0]).sin(),
            (p[1] + warp[1]).sin(),
            (p[2] + 1.0).sin(),
        ];
        assert_eq!(got, expect);
    }

    /// Pseudo-density is deterministic per name, within [1, 3.2], and ranks a
    /// dense many-branch name above a sparse two-branch one.
    #[test]
    fn pseudo_density_is_bounded_and_monotonic_in_branches() {
        let lo = build_flame_spec("a").audio.pseudo_density;
        let hi = build_flame_spec("abcdefghijklmnopqrs").audio.pseudo_density;
        assert!((1.0..=3.2).contains(&lo));
        assert!((1.0..=3.2).contains(&hi));
        assert!(hi > lo, "8 branches must read denser than 2");
        // chord_degree follows v4's mapping shape from density.
        let cd = build_flame_spec("madison").audio.chord_degree;
        assert!((0.0..=48.0).contains(&cd));
    }
}
```

- [ ] **Step 3: Run the tests, verify they fail**

Run: `cargo nextest run -p wc-sketches flame::branches`
Expected: FAIL (types/functions not defined; compile error).

- [ ] **Step 4: Implement `branches.rs`**

```rust
//! Name -> fractal generation: v4's `stringHash`, PRNG, and `randomBranch*`
//! ported with f64 arithmetic so the same name produces the same fractal as
//! v4 (JS numbers are f64; `*`, `+`, `%` are IEEE-exact in both languages).
//!
//! Also owns the CPU mirror of the WGSL kernel's affine tables and variation
//! functions (`AFFINE_MATS`/`AFFINE_OFFSETS`, [`apply_variation_cpu`],
//! [`apply_branch_cpu`]). Kernel parity discipline: this file and
//! `assets/shaders/flame/simulate.wgsl` change together term-for-term.

/// The default name shown as the input placeholder and used when the input is
/// empty. v4: `FlameNameInput.DEFAULT_NAME`.
pub const DEFAULT_NAME: &str = "who are you?";

/// Maximum branch count. `numBranches = ceil(1 + len%5 + floor(len/5))` with
/// `len` in 1..=20 peaks at 8 (len = 19).
pub const MAX_BRANCHES: usize = 8;

/// v4's PRNG modulus: 2^31 - 1.
const GEN_DIVISOR: f64 = 2_147_483_647.0;

/// The seven affine maps from v4 `transforms.ts::AFFINES`, decomposed into a
/// row-major 3x3 matrix plus offset (every v4 affine is linear + constant).
/// Order matches v4's object-key order; `affine_idx` indexes both tables.
///
/// 0 TowardsOriginNegativeBias  1 TowardsOrigin2  2 Swap  3 SwapSub
/// 4 Negate                     5 NegateSwap      6 Up1
pub const AFFINE_MATS: [[f32; 9]; 7] = [
    [0.5, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.5],
    [0.5, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.5],
    [0.0, 0.4, 0.4, 0.4, 0.0, 0.4, 0.4, 0.4, 0.0],
    [0.0, 0.5, -0.5, -0.5, 0.0, 0.5, 0.5, -0.5, 0.0],
    [-1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, -1.0],
    [
        -0.476_190_48, 0.476_190_48, 0.476_190_48,
        0.476_190_48, -0.476_190_48, 0.476_190_48,
        0.476_190_48, 0.476_190_48, -0.476_190_48,
    ],
    [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
];

/// Constant offsets paired with [`AFFINE_MATS`].
pub const AFFINE_OFFSETS: [[f32; 3]; 7] = [
    [-0.25, -0.5, 0.0],
    [0.5, -0.6, 0.4],
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 1.0],
];

/// The seven nonlinear variations from v4 `transforms.ts::VARIATIONS`,
/// in object-key order. The u32 repr is the WGSL kernel's switch key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VariationId {
    /// Identity.
    Linear = 0,
    /// Component-wise sine.
    Sin = 1,
    /// `p / |p|^2` (zero-safe).
    Spherical = 2,
    /// `(atan2(y,x)/pi, |p| - 1, atan2(z,x))`.
    Polar = 3,
    /// Rotation-like mix by `sin/cos(|p|^2)`.
    Swirl = 4,
    /// `p / |p|` (zero-safe, THREE `normalize`).
    Normalize = 5,
    /// `setLength(exp(-|p|^2))` (zero-safe).
    Shrink = 6,
}

impl VariationId {
    /// Variation table in v4 object-key order; `gen % 7` indexes this.
    const TABLE: [Self; 7] = [
        Self::Linear,
        Self::Sin,
        Self::Spherical,
        Self::Polar,
        Self::Swirl,
        Self::Normalize,
        Self::Shrink,
    ];
}

/// How `var_a`/`var_b` combine, from v4 `createInterpolatedVariation` /
/// `createRouterVariation`. The u32 repr is the WGSL switch key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VariationMode {
    /// Apply `var_a` only.
    Single = 0,
    /// `mix(var_a(p), var_b(p), 0.5)` (v4's constant interpolation fn).
    Interpolated = 1,
    /// `if p.z < 0 { var_a(p) } else { var_b(p) }`.
    Router = 2,
}

/// One IFS branch: affine + variation combinator + additive color.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchSpec {
    /// Index into [`AFFINE_MATS`]/[`AFFINE_OFFSETS`].
    pub affine_idx: usize,
    /// Primary variation.
    pub var_a: VariationId,
    /// Secondary variation (== `var_a` when `mode` is `Single`).
    pub var_b: VariationId,
    /// Combinator mode.
    pub mode: VariationMode,
    /// Additive per-application color (can exceed [0,1]; additive blending
    /// and the HDR camera absorb it, as in v4).
    pub color: [f32; 3],
}

/// Name-derived audio character (v4 `configureForName` + the density
/// approximation replacing the box-count visitor).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NameAudioConfig {
    /// Lowpass cutoff for the DC-osc voice, Hz. v4: map(hash2, 120..400).
    pub filter_freq: f32,
    /// Lowpass resonance. v4: map(hash3, 5..8).
    pub filter_q: f32,
    /// Velocity-to-noise scale. v4: map(hash2*hash3 % 100, 0.5..1).
    pub noise_gain_scale: f32,
    /// Major/minor chord flavor. v4: hash2 % 2 == 0 (a float quirk makes this
    /// true for all but very short names; ported faithfully).
    pub is_major: bool,
    /// Whether the white-noise voice is active. v4: hash3 % 100 >= 50.
    pub has_noise: bool,
    /// Hash-derived stand-in for v4's box-count density, in ~[1, 3.2]. See
    /// [`pseudo_density`] for the formula and the PARITY fallback seam.
    pub pseudo_density: f32,
    /// Chord register: v4's `clamp(floor(map(density, 1, 3, 0, 24)), 0, 48)`.
    pub chord_degree: f32,
}

/// Everything derived from a name: the branch set plus scalar drivers.
#[derive(Debug, Clone, PartialEq)]
pub struct FlameSpec {
    /// 2..=8 branches (see [`normalize_name`]).
    pub branches: Vec<BranchSpec>,
    /// Name-hash attractor offset, v4 `cY` in [-2.5, 2.5].
    pub c_y: f32,
    /// Name-derived audio character.
    pub audio: NameAudioConfig,
}

/// Trim the raw input; empty falls back to [`DEFAULT_NAME`]. Mirrors v4's
/// `FlameNameInput` (`trimmed || DEFAULT_NAME`), which is what makes a
/// 1-branch fractal unreachable.
#[must_use]
pub fn normalize_name(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        DEFAULT_NAME
    } else {
        trimmed
    }
}

/// v4 `stringHash`, bit-for-bit: an i32-wrapping polynomial over UTF-16 code
/// units, then `hash *= hash * 31` in f64 (which overflows f64 precision for
/// long names — deliberately kept, the quirks are part of v4's output).
#[must_use]
pub fn string_hash(s: &str) -> f64 {
    let mut hash: i32 = 0;
    let mut any = false;
    for unit in s.encode_utf16() {
        any = true;
        // JS: hash = (hash * 31 + char) | 0  — i32 wrapping semantics.
        hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    if !any {
        return 0.0;
    }
    let h = f64::from(hash);
    // JS: hash *= hash * 31 — pure f64 from here on.
    h * (h * 31.0)
}

/// One PRNG step: `gen = (gen * 4194303 + 127) % (2^31 - 1)` in f64,
/// matching JS exactly (both `*` and `%` are IEEE-exact / fmod).
pub(crate) fn prng_next(gen: &mut f64) -> f64 {
    *gen = (*gen * 4_194_303.0 + 127.0) % GEN_DIVISOR;
    *gen
}

/// v4 `map` (unclamped linear map) in f64.
fn map_f64(x: f64, x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
    y0 + (y1 - y0) * ((x - x0) / (x1 - x0))
}

/// JS `String.prototype.substring` over UTF-16 units with fractional f64
/// bounds: ToInteger truncation, clamp to length, swap if start > end.
fn substring_utf16(units: &[u16], start: f64, end: f64) -> Vec<u16> {
    let len = units.len();
    let to_index = |x: f64| -> usize {
        if x.is_nan() || x <= 0.0 {
            0
        } else {
            (x.trunc() as usize).min(len)
        }
    };
    let (mut a, mut b) = (to_index(start), to_index(end));
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    units[a..b].to_vec()
}

/// `string_hash` over pre-decoded UTF-16 units (substring seeds).
fn string_hash_units(units: &[u16]) -> f64 {
    if units.is_empty() {
        return 0.0;
    }
    let mut hash: i32 = 0;
    for &unit in units {
        hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    let h = f64::from(hash);
    h * (h * 31.0)
}

/// v4 `randomBranch`, ported draw-for-draw. The PRNG draw ORDER is part of
/// the contract: skip loop, (affine reads gen without a draw), varA draw,
/// combinator draw(s) — the router probe draw happens ONLY when numWraps > 2
/// (JS `&&` short-circuit) — then three color draws.
fn random_branch(idx: usize, substring: &[u16], num_branches: usize, num_wraps: usize) -> BranchSpec {
    let mut gen = string_hash_units(substring);
    // Skip 5 + idx*numWraps draws (v4's per-branch decorrelation).
    for _ in 0..(5 + idx * num_wraps) {
        prng_next(&mut gen);
    }
    // Affine: uses gen as left by the skip loop (no extra draw).
    let affine_idx = (gen % 7.0) as usize;
    // varA: one draw, then gen % 7.
    prng_next(&mut gen);
    let var_a = VariationId::TABLE[(gen % 7.0) as usize];

    let mut mode = VariationMode::Single;
    let mut var_b = var_a;
    // Combinator selection, preserving v4's draw order and short-circuit.
    prng_next(&mut gen);
    let interp_roll = gen / GEN_DIVISOR;
    if interp_roll < num_wraps as f64 * 0.25 {
        mode = VariationMode::Interpolated;
        prng_next(&mut gen);
        var_b = VariationId::TABLE[(gen % 7.0) as usize];
    } else if num_wraps > 2 {
        prng_next(&mut gen);
        let router_roll = gen / GEN_DIVISOR;
        if router_roll < 0.2 {
            mode = VariationMode::Router;
            prng_next(&mut gen);
            var_b = VariationId::TABLE[(gen % 7.0) as usize];
        }
    }

    // Three color draws in [-0.05, 0.05), focus channel +0.2, scaled.
    let mut color = [0.0_f64; 3];
    for c in &mut color {
        prng_next(&mut gen);
        *c = (gen / GEN_DIVISOR) * 0.1 - 0.05;
    }
    color[idx % 3] += 0.2;
    let scale = num_branches as f64 / 3.5;
    BranchSpec {
        affine_idx,
        var_a,
        var_b,
        mode,
        color: [
            (color[0] * scale) as f32,
            (color[1] * scale) as f32,
            (color[2] * scale) as f32,
        ],
    }
}

/// Hash-derived stand-in for v4's box-count density (see the spec's Audio
/// section). Branch count dominates; contractive variations raise it, spread
/// variations lower it. Ear-tunable; the documented fallback seam is a
/// one-shot ~2k-point CPU evaluation + box-count at name-change only.
fn pseudo_density(branches: &[BranchSpec]) -> f32 {
    // Per-variation "contractiveness" weight, judged from the maps' effect on
    // typical |p| ~ 1 points: Shrink and Spherical pull hard toward compact
    // clusters; Polar and Normalize spread onto shells/sheets.
    fn weight(v: VariationId) -> f32 {
        match v {
            VariationId::Shrink => 1.0,
            VariationId::Spherical => 0.9,
            VariationId::Sin => 0.7,
            VariationId::Linear | VariationId::Swirl => 0.5,
            VariationId::Polar => 0.4,
            VariationId::Normalize => 0.3,
        }
    }
    let contract: f32 = branches
        .iter()
        .map(|b| match b.mode {
            VariationMode::Single => weight(b.var_a),
            _ => (weight(b.var_a) + weight(b.var_b)) * 0.5,
        })
        .sum::<f32>()
        / branches.len() as f32;
    let b = branches.len() as f32;
    // 2 branches, contract 0.3 -> 1.18 ; 8 branches, contract 1.0 -> 3.0.
    1.0 + 1.4 * ((b - 2.0) / 6.0) + 0.6 * contract
}

/// Build the full spec for a (pre-normalized or raw) name. Applies
/// [`normalize_name`] internally so callers can pass raw input.
#[must_use]
pub fn build_flame_spec(name: &str) -> FlameSpec {
    let name = normalize_name(name);
    let units: Vec<u16> = name.encode_utf16().collect();
    let len = units.len();
    let num_wraps = len / 5;
    // ceil() of an integer-valued f64 is itself; kept for v4 shape.
    let num_branches = (1.0 + (len % 5) as f64 + num_wraps as f64).ceil() as usize;

    let mut branches = Vec::with_capacity(num_branches);
    for i in 0..num_branches {
        let start = map_f64(i as f64, 0.0, num_branches as f64, 0.0, len as f64);
        let end = map_f64((i + 1) as f64, 0.0, num_branches as f64, 0.0, len as f64);
        let sub = substring_utf16(&units, start, end);
        branches.push(random_branch(i, &sub, num_branches, num_wraps));
    }
    debug_assert!(
        (2..=MAX_BRANCHES).contains(&branches.len()),
        "normalize_name guarantees 2..=8 branches"
    );

    // Audio character, v4 `updateName` + `configureForName` in f64.
    let hash = string_hash(name);
    let hash_norm = (hash % 1024.0) / 1024.0;
    let hash2 = hash * hash + hash * 31.0 + 9.0;
    let hash3 = hash2 * hash2 + hash2 * 31.0 + 9.0;
    let density = pseudo_density(&branches);
    // v4: clamp(floor(map(density, 1, 3, 0, 24)), 0, 48).
    let chord_degree = map_f64(f64::from(density), 1.0, 3.0, 0.0, 24.0)
        .floor()
        .clamp(0.0, 48.0) as f32;
    let audio = NameAudioConfig {
        filter_freq: map_f64((hash2 % 2e12) / 2e12, 0.0, 1.0, 120.0, 400.0) as f32,
        filter_q: map_f64((hash3 % 2e12) / 2e12, 0.0, 1.0, 5.0, 8.0) as f32,
        noise_gain_scale: map_f64(((hash2 * hash3) % 100.0) / 100.0, 0.0, 1.0, 0.5, 1.0) as f32,
        is_major: hash2 % 2.0 == 0.0,
        has_noise: (hash3 % 100.0) >= 50.0,
        pseudo_density: density,
        chord_degree,
    };

    FlameSpec {
        branches,
        c_y: map_f64(hash_norm, 0.0, 1.0, -2.5, 2.5) as f32,
        audio,
    }
}

/// CPU mirror of the WGSL variation switch. Zero-length guards match
/// THREE.js (`normalize`/`setLength` divide by `length || 1`).
#[must_use]
pub fn apply_variation_cpu(id: VariationId, p: [f32; 3]) -> [f32; 3] {
    let len_sq = p[0] * p[0] + p[1] * p[1] + p[2] * p[2];
    match id {
        VariationId::Linear => p,
        VariationId::Sin => [p[0].sin(), p[1].sin(), p[2].sin()],
        VariationId::Spherical => {
            if len_sq == 0.0 {
                p
            } else {
                [p[0] / len_sq, p[1] / len_sq, p[2] / len_sq]
            }
        }
        VariationId::Polar => [
            p[1].atan2(p[0]) / std::f32::consts::PI,
            len_sq.sqrt() - 1.0,
            p[2].atan2(p[0]),
        ],
        VariationId::Swirl => {
            let (s, c) = (len_sq.sin(), len_sq.cos());
            [
                p[2] * s - p[1] * c,
                p[0] * c + p[2] * s,
                p[0] * s - p[1] * s,
            ]
        }
        VariationId::Normalize => {
            if len_sq == 0.0 {
                p
            } else {
                let inv = 1.0 / len_sq.sqrt();
                [p[0] * inv, p[1] * inv, p[2] * inv]
            }
        }
        VariationId::Shrink => {
            if len_sq == 0.0 {
                p
            } else {
                let scale = (-len_sq).exp() / len_sq.sqrt();
                [p[0] * scale, p[1] * scale, p[2] * scale]
            }
        }
    }
}

/// CPU mirror of the full per-node branch application: affine matrix+offset,
/// then the per-frame warp added to x/y, then the variation combinator.
/// Mirrors the WGSL kernel term-for-term (kernel parity discipline).
#[must_use]
pub fn apply_branch_cpu(spec: &BranchSpec, warp: [f32; 2], p: [f32; 3]) -> [f32; 3] {
    let m = &AFFINE_MATS[spec.affine_idx];
    let o = &AFFINE_OFFSETS[spec.affine_idx];
    let affine = [
        m[0] * p[0] + m[1] * p[1] + m[2] * p[2] + o[0] + warp[0],
        m[3] * p[0] + m[4] * p[1] + m[5] * p[2] + o[1] + warp[1],
        m[6] * p[0] + m[7] * p[1] + m[8] * p[2] + o[2],
    ];
    match spec.mode {
        VariationMode::Single => apply_variation_cpu(spec.var_a, affine),
        VariationMode::Interpolated => {
            let a = apply_variation_cpu(spec.var_a, affine);
            let b = apply_variation_cpu(spec.var_b, affine);
            [
                a[0] + (b[0] - a[0]) * 0.5,
                a[1] + (b[1] - a[1]) * 0.5,
                a[2] + (b[2] - a[2]) * 0.5,
            ]
        }
        VariationMode::Router => {
            if affine[2] < 0.0 {
                apply_variation_cpu(spec.var_a, affine)
            } else {
                apply_variation_cpu(spec.var_b, affine)
            }
        }
    }
}
```

Note on `as` casts: `(gen % 7.0) as usize`, `(x.trunc() as usize)`, and the f64→f32 narrowings replicate JS ToInteger / Number→float semantics and are documented by the surrounding comments; clippy's `cast_possible_truncation` group is not denied workspace-wide, but if clippy flags them, `#[allow(...)]` with a `reason` referencing v4 parity is the correct resolution.

- [ ] **Step 5: Run the tests, verify they pass**

Run: `cargo nextest run -p wc-sketches flame::branches`
Expected: PASS (11 tests).

- [ ] **Step 6: Full verification gate, then commit**

```bash
cargo fmt --all
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
git add crates/wc-sketches/src/flame docs/superpowers/plans/assets/2026-07-02-flame-goldens.mjs crates/wc-sketches/src/lib.rs
git commit -m "feat(flame): name-to-branch core with v4 f64 golden parity"
```

### Task F2: Level layout math (`levels.rs`)

**Files:**
- Create: `crates/wc-sketches/src/flame/levels.rs`
- Modify: `crates/wc-sketches/src/flame/mod.rs` (add `pub mod levels;`)
- Test: `#[cfg(test)] mod tests` in `levels.rs`

**Interfaces:**
- Produces: `pub const MAX_POINTS: u32 = 200_000`; `pub const MAX_LEVELS: usize = 24`; `pub struct LevelSpan { pub start: u32, pub count: u32, pub parent_start: u32, pub parent_count: u32 }`; `pub struct LevelLayout { pub levels: Vec<LevelSpan>, pub total: u32 }` with `LevelLayout::build(branch_count: u32, target_points: f64) -> Self`; `pub fn compute_depth(branch_count: u32, target_points: f64) -> u32`; `impl LevelLayout { pub fn live_count_for_complexity(&self, complexity: f32) -> u32; pub fn dispatch_levels_for_live(&self, live: u32) -> u32 }`.
- Consumes: nothing (pure). Node ordering contract consumed by F5/F6/F8: **level-ordered, branch-major within a level** — node with in-level index `local` has `branch = local / parent_count`, `parent = parent_start + (local % parent_count)`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
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
            assert_eq!(layout.levels.len(), usize::try_from(depth + 1).expect("fits"));
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
```

- [ ] **Step 2: Run the tests, verify they fail**

Run: `cargo nextest run -p wc-sketches flame::levels`
Expected: FAIL (module not defined).

- [ ] **Step 3: Implement `levels.rs`**

```rust
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
```

- [ ] **Step 4: Run the tests, verify they pass**

Run: `cargo nextest run -p wc-sketches flame::levels`
Expected: PASS (6 tests).

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
git add crates/wc-sketches/src/flame
git commit -m "feat(flame): branch-major level layout with ember prefix math"
```

---

## Stage 2 — Lifecycle scaffold + re-entry (the audit-T5 reversal)

F3 gives Flame a registered plugin, settings, and picker tile (still unreachable). F4 then reverses the audit de-routing so `WAVECONDUCTOR_START_SKETCH=flame`, the picker tile, `Digit2`, and the next/prev cycle all reach it — this ordering keeps the "every `SKETCH_ORDER` entry has a manifest" guard green at every commit.

### Task F3: `FlamePlugin` scaffold, `FlameSettings`, manifest tile, clear-color swap

**Files:**
- Create: `crates/wc-sketches/src/flame/settings.rs`
- Modify: `crates/wc-sketches/src/flame/mod.rs` (plugin skeleton)
- Modify: `crates/wc-sketches/src/lib.rs` (register `FlamePlugin`)
- Create: `assets/sketches/flame/screenshot.png` (copied from v4)
- Test: `#[cfg(test)]` in `settings.rs` and `mod.rs`

**Interfaces:**
- Produces: `pub struct FlamePlugin;` (registered by `SketchesPlugin`); `pub struct FlameSettings { ... }` (`SketchSettings` derive, `storage_key = "flame"`, `SketchLifecycle` impl with `STATE = AppState::Flame`); `pub(crate) fn register_flame_manifest(app: &mut App)`; `SavedClearColor` resource (private).
- Consumes: `wc_core::sketch::{register_sketch_tile, apply_render_profile, reset_render_profile, restart_on_settings_change}`, `wc_core::settings::RegisterSketchSettingsExt`, `wc_core::render::{TonemapChoice, BloomComposite}`.
- Display name: **"Flame"** unless v4's HomePage uses a different label — check `.worktrees/v4/src/**/HomePage*` for the flame tile text before committing; if it differs, use v4's label.

- [ ] **Step 1: Copy the screenshot asset**

```bash
mkdir -p assets/sketches/flame
cp .worktrees/v4/src/sketches/flame/screenshots/flame.png assets/sketches/flame/screenshot.png
git add assets/sketches/flame/screenshot.png
```

(It is a rendered sketch image — no system chrome to scrub.)

- [ ] **Step 2: Write the failing tests**

In `settings.rs` (bottom of the file you are about to create — write tests first, stub the struct so it compiles only after Step 4):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Legacy persisted TOML missing fields still deserializes cleanly;
    /// siblings preserved (per-field serde defaults, the house pattern).
    #[test]
    #[allow(clippy::expect_used, reason = "test-only")]
    fn missing_field_preserves_sibling_values() {
        let legacy = r#"
            name = "madison"
            gamma = 0.6
        "#;
        let parsed: FlameSettings = toml::from_str(legacy).expect("legacy TOML must parse");
        assert_eq!(parsed.name, "madison");
        assert!((parsed.gamma - 0.6).abs() < 1e-6);
        assert!((parsed.master_brightness - 1.0).abs() < 1e-6, "sibling default");
        assert!((parsed.carousel_period_secs - 120.0).abs() < 1e-6, "sibling default");
    }

    /// Every `#[setting(default = ...)]` matches its `default_*` serde fn.
    #[test]
    fn default_values_match_serde_defaults() {
        let d = FlameSettings::default();
        assert_eq!(d.name, default_name());
        assert!((d.target_points - default_target_points()).abs() < f32::EPSILON);
        assert!((d.autorotate_speed - default_autorotate_speed()).abs() < f32::EPSILON);
        assert!((d.dof_strength - default_dof_strength()).abs() < f32::EPSILON);
        assert!((d.base_point_size - default_base_point_size()).abs() < f32::EPSILON);
        assert!((d.point_opacity - default_point_opacity()).abs() < f32::EPSILON);
        assert!((d.point_size_clamp - default_point_size_clamp()).abs() < f32::EPSILON);
        assert!((d.fog_near - default_fog_near()).abs() < f32::EPSILON);
        assert!((d.fog_far - default_fog_far()).abs() < f32::EPSILON);
        assert!((d.gamma - default_gamma()).abs() < f32::EPSILON);
        assert!((d.master_brightness - default_master_brightness()).abs() < f32::EPSILON);
        assert_eq!(d.tonemapping, default_tonemapping());
        assert!((d.bloom_intensity - default_bloom_intensity()).abs() < f32::EPSILON);
        assert!((d.bloom_threshold - default_bloom_threshold()).abs() < f32::EPSILON);
        assert_eq!(d.bloom_composite, default_bloom_composite());
        assert!((d.carousel_period_secs - default_carousel_period_secs()).abs() < f32::EPSILON);
        assert!((d.ember_fraction - default_ember_fraction()).abs() < f32::EPSILON);
        assert!((d.attract_brightness - default_attract_brightness()).abs() < f32::EPSILON);
        assert!((d.morph_energy_scale - default_morph_energy_scale()).abs() < f32::EPSILON);
        assert!((d.chord_energy_scale - default_chord_energy_scale()).abs() < f32::EPSILON);
        assert!((d.synth_volume_scale - default_synth_volume_scale()).abs() < f32::EPSILON);
        assert!((d.synth_attack_ms - default_synth_attack_ms()).abs() < f32::EPSILON);
        assert!((d.synth_release_ms - default_synth_release_ms()).abs() < f32::EPSILON);
    }
}
```

In `mod.rs` tests:

```rust
#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use wc_core::sketch::SketchManifest;

    /// Mirrors register_dots_manifest_appends_entry: the free-function path
    /// registers a Flame tile without needing a RenderApp.
    #[test]
    fn register_flame_manifest_appends_entry() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::asset::AssetPlugin::default());
        app.add_plugins(bevy::image::ImagePlugin::default());
        register_flame_manifest(&mut app);
        let manifest = app.world().resource::<SketchManifest>();
        let entry = manifest
            .get(AppState::Flame)
            .expect("Flame manifest entry should be registered");
        assert_eq!(entry.display_name, "Flame");
    }

    /// OnEnter swaps the clear color to v4's #10101f and stashes the prior
    /// value; OnExit restores it and drops the stash.
    #[test]
    fn clear_color_swap_and_restore() {
        let mut world = World::new();
        world.insert_resource(ClearColor(Color::WHITE));

        world
            .run_system_once(enter_flame_clear_color)
            .expect("enter runs");
        let cc = world.resource::<ClearColor>();
        assert_eq!(cc.0, Color::srgb_u8(0x10, 0x10, 0x1f));
        assert!(world.get_resource::<SavedClearColor>().is_some());

        world
            .run_system_once(exit_flame_clear_color)
            .expect("exit runs");
        assert_eq!(world.resource::<ClearColor>().0, Color::WHITE);
        assert!(world.get_resource::<SavedClearColor>().is_none());
    }
}
```

- [ ] **Step 3: Run the tests, verify they fail**

Run: `cargo nextest run -p wc-sketches flame`
Expected: FAIL (compile errors: `FlameSettings`, `register_flame_manifest` undefined).

- [ ] **Step 4: Implement `settings.rs`**

Follow the Dots settings file shape exactly (`crates/wc-sketches/src/dots/settings.rs` is the model: struct + `SketchLifecycle` impl + `default_*` fns + tests, module doc listing every knob):

```rust
//! Flame sketch settings.
//!
//! The `name` is the sketch's identity: it seeds branch count, transforms,
//! colors, and audio character (see `super::branches`). It is a LIVE setting:
//! the name-change watcher rebuilds the fractal in place (no restart fade).
//! The carousel list (`carousel_names`) is added in the name-input task once
//! the `TextList` setting kind exists.
//!
//! Per-field serde defaults follow the house pattern: every field carries
//! `#[serde(default = "default_<name>")]` so legacy TOML deserializes cleanly.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// User-tunable parameters for the Flame sketch.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "flame")]
pub struct FlameSettings {
    /// The visitor's name. Empty means "use the default placeholder name".
    /// Live: the watcher rebuilds branches + reseeds the node buffer on change.
    #[setting(
        default = String::new(),
        ty = Text,
        label = "Name",
        section = "Identity",
        category = User
    )]
    #[serde(default = "default_name")]
    pub name: String,

    /// Approximate total point budget. The tree depth is
    /// floor(ln(budget)/ln(branches)), so actual totals land under this.
    /// Live: the watcher rebuilds layout + mesh when it changes.
    #[setting(
        default = 100_000.0_f32,
        min = 10_000.0_f32,
        max = 200_000.0_f32,
        step = 10_000.0_f32,
        label = "Point budget",
        section = "Fractal",
        category = Dev
    )]
    #[serde(default = "default_target_points")]
    pub target_points: f32,

    /// Camera auto-rotation speed. 1.0 = one orbit per minute (v4's
    /// OrbitControls autoRotateSpeed = 1). 0 disables.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 10.0_f32,
        step = 0.1_f32,
        label = "Autorotate speed",
        section = "Camera",
        category = Dev
    )]
    #[serde(default = "default_autorotate_speed")]
    pub autorotate_speed: f32,

    /// Fake depth-of-field strength: the `* 3.0` factor in v4's
    /// `outOfFocusAmount`. 0 disables the DoF entirely.
    #[setting(
        default = 3.0_f32,
        min = 0.0_f32,
        max = 10.0_f32,
        step = 0.1_f32,
        label = "DoF strength",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_dof_strength")]
    pub dof_strength: f32,

    /// In-focus point size in pixels (v4 `originalSize = 2.0`).
    #[setting(
        default = 2.0_f32,
        min = 0.5_f32,
        max = 8.0_f32,
        step = 0.1_f32,
        label = "Point size",
        unit = "px",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_base_point_size")]
    pub base_point_size: f32,

    /// Per-point base opacity (v4 material `opacity = 0.2`). The additive
    /// accumulation of ~100k points does the brightening.
    #[setting(
        default = 0.2_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.01_f32,
        label = "Point opacity",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_point_opacity")]
    pub point_opacity: f32,

    /// Point size clamp in pixels (v4 `min(50., gl_PointSize)`); bounds
    /// additive overdraw when zoomed close.
    #[setting(
        default = 50.0_f32,
        min = 4.0_f32,
        max = 128.0_f32,
        step = 1.0_f32,
        label = "Point size clamp",
        unit = "px",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_point_size_clamp")]
    pub point_size_clamp: f32,

    /// Fog start distance in view units (v4 `THREE.Fog(bg, 2, 60)`).
    #[setting(
        default = 2.0_f32,
        min = 0.0_f32,
        max = 20.0_f32,
        step = 0.5_f32,
        label = "Fog near",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_fog_near")]
    pub fog_near: f32,

    /// Fog full-fade distance in view units.
    #[setting(
        default = 60.0_f32,
        min = 5.0_f32,
        max = 200.0_f32,
        step = 5.0_f32,
        label = "Fog far",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_fog_far")]
    pub fog_far: f32,

    /// Output gamma applied in the render shader (v4 baked pow(0.545)).
    /// Starting point for the operator's AgX-era eye tune.
    #[setting(
        default = 0.545_f32,
        min = 0.1_f32,
        max = 4.0_f32,
        step = 0.005_f32,
        label = "Gamma",
        section = "Visual",
        category = User
    )]
    #[serde(default = "default_gamma")]
    pub gamma: f32,

    /// Pre-tonemap exposure multiplier on the point contribution. Mirrors
    /// the Dots/Line/Cymatics master_brightness knob.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 3.0_f32,
        step = 0.05_f32,
        label = "Master brightness",
        section = "Visual",
        category = User
    )]
    #[serde(default = "default_master_brightness")]
    pub master_brightness: f32,

    /// Camera tonemapping operator while Flame is active. House default.
    #[setting(
        default = wc_core::render::TonemapChoice::ReinhardLuminance,
        ty = Enum,
        label = "Tonemapping",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_tonemapping")]
    pub tonemapping: wc_core::render::TonemapChoice,

    /// Bloom intensity for this sketch (main camera).
    #[setting(
        default = 0.35_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.05_f32,
        label = "Bloom intensity",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_bloom_intensity")]
    pub bloom_intensity: f32,

    /// Bloom prefilter threshold (0.0 pairs with EnergyConserving).
    #[setting(
        default = 0.0_f32,
        min = 0.0_f32,
        max = 3.0_f32,
        step = 0.05_f32,
        label = "Bloom threshold",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_bloom_threshold")]
    pub bloom_threshold: f32,

    /// Bloom composite mode.
    #[setting(
        default = wc_core::render::BloomComposite::EnergyConserving,
        ty = Enum,
        label = "Bloom composite",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_bloom_composite")]
    pub bloom_composite: wc_core::render::BloomComposite,

    /// Seconds between screensaver carousel advances.
    #[setting(
        default = 120.0_f32,
        min = 15.0_f32,
        max = 600.0_f32,
        step = 15.0_f32,
        label = "Carousel period",
        unit = "s",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_carousel_period_secs")]
    pub carousel_period_secs: f32,

    /// Fraction of full complexity the ember decays to during the
    /// screensaver (Madison: "40-60%"). 1.0 disables the decay.
    #[setting(
        default = 0.5_f32,
        min = 0.2_f32,
        max = 1.0_f32,
        step = 0.05_f32,
        label = "Ember fraction",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_ember_fraction")]
    pub ember_fraction: f32,

    /// Brightness lift past the tonemapper's white knee during attract mode
    /// (the Dots-established pattern, default 2.2).
    #[setting(
        default = 2.2_f32,
        min = 1.0_f32,
        max = 4.0_f32,
        step = 0.1_f32,
        label = "Attract brightness",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_attract_brightness")]
    pub attract_brightness: f32,

    /// Scale on the CPU morph-energy proxy (analytic |dcX/dt| + warp speed)
    /// before it enters the synth's v4 velocity curves. The primary ear-tune
    /// knob standing in for v4's measured point velocity.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 10.0_f32,
        step = 0.1_f32,
        label = "Morph energy scale",
        section = "Audio",
        category = Dev
    )]
    #[serde(default = "default_morph_energy_scale")]
    pub morph_energy_scale: f32,

    /// Stand-in for v4's `count^2 / 8` chord-gain factor (box-count `count`
    /// has no v5 source). Ear-tune target.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 8.0_f32,
        step = 0.1_f32,
        label = "Chord energy scale",
        section = "Audio",
        category = Dev
    )]
    #[serde(default = "default_chord_energy_scale")]
    pub chord_energy_scale: f32,

    /// Master output trim for the Flame synth voice.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 2.0_f32,
        step = 0.05_f32,
        label = "Synth volume",
        section = "Audio",
        category = User
    )]
    #[serde(default = "default_synth_volume_scale")]
    pub synth_volume_scale: f32,

    /// Morph-energy envelope attack, ms.
    #[setting(
        default = 120.0_f32,
        min = 5.0_f32,
        max = 500.0_f32,
        step = 5.0_f32,
        label = "Synth attack",
        unit = "ms",
        section = "Audio",
        category = Dev
    )]
    #[serde(default = "default_synth_attack_ms")]
    pub synth_attack_ms: f32,

    /// Morph-energy envelope release, ms.
    #[setting(
        default = 600.0_f32,
        min = 100.0_f32,
        max = 5000.0_f32,
        step = 50.0_f32,
        label = "Synth release",
        unit = "ms",
        section = "Audio",
        category = Dev
    )]
    #[serde(default = "default_synth_release_ms")]
    pub synth_release_ms: f32,
}

/// Ties `FlameSettings` to the shared sketch lifecycle glue.
impl wc_core::sketch::SketchLifecycle for FlameSettings {
    const STATE: wc_core::lifecycle::state::AppState =
        wc_core::lifecycle::state::AppState::Flame;

    fn render_profile(&self) -> wc_core::sketch::RenderProfile {
        wc_core::sketch::RenderProfile {
            tonemapping: self.tonemapping,
            bloom_intensity: self.bloom_intensity,
            bloom_threshold: self.bloom_threshold,
            bloom_composite: self.bloom_composite,
        }
    }
}

// Per-field serde defaults. Values MUST match the `#[setting(default = ...)]`
// attributes above; update both sites together.
fn default_name() -> String {
    String::new()
}
fn default_target_points() -> f32 {
    100_000.0
}
fn default_autorotate_speed() -> f32 {
    1.0
}
fn default_dof_strength() -> f32 {
    3.0
}
fn default_base_point_size() -> f32 {
    2.0
}
fn default_point_opacity() -> f32 {
    0.2
}
fn default_point_size_clamp() -> f32 {
    50.0
}
fn default_fog_near() -> f32 {
    2.0
}
fn default_fog_far() -> f32 {
    60.0
}
fn default_gamma() -> f32 {
    0.545
}
fn default_master_brightness() -> f32 {
    1.0
}
fn default_tonemapping() -> wc_core::render::TonemapChoice {
    wc_core::render::TonemapChoice::ReinhardLuminance
}
fn default_bloom_intensity() -> f32 {
    0.35
}
fn default_bloom_threshold() -> f32 {
    0.0
}
fn default_bloom_composite() -> wc_core::render::BloomComposite {
    wc_core::render::BloomComposite::EnergyConserving
}
fn default_carousel_period_secs() -> f32 {
    120.0
}
fn default_ember_fraction() -> f32 {
    0.5
}
fn default_attract_brightness() -> f32 {
    2.2
}
fn default_morph_energy_scale() -> f32 {
    1.0
}
fn default_chord_energy_scale() -> f32 {
    1.0
}
fn default_synth_volume_scale() -> f32 {
    1.0
}
fn default_synth_attack_ms() -> f32 {
    120.0
}
fn default_synth_release_ms() -> f32 {
    600.0
}
```

(Then the test module from Step 2 at the file footer.)

- [ ] **Step 5: Implement the plugin skeleton in `mod.rs`**

Replace the F1 skeleton with:

```rust
//! Flame sketch: a name-seeded IFS fractal flame, evaluated level-parallel on
//! the GPU and drawn as an additive point cloud with a fake depth of field.
//!
//! ## Data flow (grows stage by stage; see the 2026-07-02 flame port plan)
//!
//! 1. `OnEnter(AppState::Flame)` swaps the clear color to v4's `#10101f`
//!    (stashing the previous value) — the fog fades points into this color.
//! 2. Settings register with the shared panel/persistence system; the
//!    `RenderProfile` applier drives the main camera's tonemapping/bloom
//!    while Flame is active.
//! 3. `OnExit` restores the clear color and resets the render profile.
//!
//! Simulation, rendering, interaction, audio, and the attract performer are
//! wired in later stages of the port plan.

pub mod branches;
pub mod levels;
pub mod settings;

use bevy::prelude::*;
use wc_core::lifecycle::state::AppState;
use wc_core::settings::RegisterSketchSettingsExt;

/// Plugin that registers the Flame sketch.
pub struct FlamePlugin;

impl Plugin for FlamePlugin {
    fn build(&self, app: &mut App) {
        // Settings: panel + persistence (storage key "flame").
        app.register_sketch_settings::<settings::FlameSettings>();

        // Picker-tile manifest entry (async screenshot load).
        register_flame_manifest(app);

        // v4 scene background: #10101f. The whole sketch reads against it
        // (fog fades points toward it), so it is swapped at the state seam.
        app.add_systems(OnEnter(AppState::Flame), enter_flame_clear_color);
        app.add_systems(
            OnExit(AppState::Flame),
            (exit_flame_clear_color, wc_core::sketch::reset_render_profile),
        );

        // Tonemapping + bloom profile onto the main camera while Flame is
        // active (live dev-panel tuning), via the shared generic applier.
        app.add_systems(
            Update,
            wc_core::sketch::apply_render_profile::<settings::FlameSettings>
                .run_if(in_state(AppState::Flame)),
        );

        // Restart listener (requires_restart fields fade out/in via the
        // shared reload overlay). The name is NOT requires_restart — it
        // rebuilds live through the name-change watcher (later stage).
        app.add_systems(
            Update,
            wc_core::sketch::restart_on_settings_change::<settings::FlameSettings>,
        );
    }
}

/// Register Flame's picker-tile metadata. Factored out of `FlamePlugin::build`
/// so it is unit-testable without rendering plugins (mirrors
/// `register_dots_manifest`).
pub(crate) fn register_flame_manifest(app: &mut App) {
    wc_core::sketch::register_sketch_tile(
        app,
        AppState::Flame,
        "Flame",
        "sketches/flame/screenshot.png",
    );
}

/// Stash for the pre-Flame clear color, restored on exit.
#[derive(Resource)]
struct SavedClearColor(ClearColor);

/// `OnEnter(AppState::Flame)`: stash the current clear color and swap in
/// v4's scene background `#10101f`.
fn enter_flame_clear_color(mut commands: Commands<'_, '_>, current: Res<'_, ClearColor>) {
    commands.insert_resource(SavedClearColor(current.clone()));
    commands.insert_resource(ClearColor(Color::srgb_u8(0x10, 0x10, 0x1f)));
}

/// `OnExit(AppState::Flame)`: restore the stashed clear color.
fn exit_flame_clear_color(
    mut commands: Commands<'_, '_>,
    saved: Option<Res<'_, SavedClearColor>>,
) {
    if let Some(saved) = saved {
        commands.insert_resource(saved.0.clone());
    }
    commands.remove_resource::<SavedClearColor>();
}
```

(Then the test module from Step 2 at the file footer.)

In `crates/wc-sketches/src/lib.rs`, register the plugin next to the others (after `dots::DotsPlugin`, before `cymatics::CymaticsPlugin` to match `SKETCH_ORDER`):

```rust
app.add_plugins(flame::FlamePlugin);
```

- [ ] **Step 6: Run the tests, verify they pass**

Run: `cargo nextest run -p wc-sketches flame`
Expected: PASS (Stage 1 tests + `register_flame_manifest_appends_entry`, `clear_color_swap_and_restore`, both settings tests).

- [ ] **Step 7: Gate and commit**

```bash
cargo fmt --all
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo xtask check-secrets
git add crates/wc-sketches assets/sketches/flame
git commit -m "feat(flame): plugin scaffold, settings surface, picker tile"
```

### Task F4: Re-entry routing (`SKETCH_ORDER`, bindings, guard tests)

**Files:**
- Modify: `crates/wc-core/src/lifecycle/state.rs`
- Modify: `crates/wc-core/src/lifecycle/actions.rs`
- Modify: `crates/wc-core/src/lifecycle/action_map.rs`
- Modify: `crates/wc-core/src/lifecycle/nav.rs`
- Modify: `crates/wc-core/tests/ui_picker.rs`
- Modify: `crates/wc-core/tests/lifecycle.rs`

**Interfaces:**
- Produces: `AppState::SKETCH_ORDER = [Line, Flame, Dots, Cymatics]`; `AppState::from_name("flame") == Some(AppState::Flame)`; `WaveConductorAction::SelectFlame` bound to `Digit2`; cycle routing `Line → Flame → Dots → Cymatics → Line`.
- Consumes: `register_flame_manifest` (F3) — the picker now renders a live Flame tile.

- [ ] **Step 1: Update the guard tests first (they define the new contract)**

In `crates/wc-core/src/lifecycle/state.rs` tests:

Replace `flame_and_waves_arms_are_present_but_unreachable_from_the_cycle` with:

```rust
    /// Waves stays a de-routed seam (2026-07 audit T5); Flame re-entered the
    /// cycle in the 2026-07-02 flame port.
    #[test]
    fn waves_arms_are_present_but_unreachable_from_the_cycle() {
        assert!(AppState::SKETCH_ORDER.contains(&AppState::Flame));
        assert!(!AppState::SKETCH_ORDER.contains(&AppState::Waves));
        assert_eq!(AppState::Waves.next_sketch(), AppState::Line);
        assert_eq!(AppState::Waves.prev_sketch(), AppState::Cymatics);
    }
```

In `from_name_parses_every_sketch_case_insensitively`, change the Flame line:

```rust
        assert_eq!(AppState::from_name("Flame"), Some(AppState::Flame));
        assert_eq!(AppState::from_name("waves"), None);
```

In `crates/wc-core/tests/ui_picker.rs`:

```rust
    const KNOWN_IMPLEMENTED_SKETCHES: [AppState; 4] =
        [AppState::Line, AppState::Flame, AppState::Dots, AppState::Cymatics];
```

and remove `AppState::Flame` from the unregistered set in
`manifest_distinguishes_registered_vs_unregistered_sketches` (leaving Waves).

In `crates/wc-core/tests/lifecycle.rs` `next_and_prev_cycle_through_sketches`: after Home → `X` lands on Line, the next `X` now lands on **Flame**, then Dots; the wrap loop becomes `for _ in 0..4` (SKETCH_ORDER has 4 entries) landing back on Dots, and `Z` from Dots goes to **Flame** (not Line). Update the assertions and the comment ("4 entries: Line, Flame, Dots, Cymatics — Waves is a de-routed seam"):

```rust
    press_key(&mut app, KeyCode::KeyX);
    app.update();
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Line);
    press_key(&mut app, KeyCode::KeyX);
    app.update();
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Flame);
    press_key(&mut app, KeyCode::KeyX);
    app.update();
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Dots);
    for _ in 0..4 {
        press_key(&mut app, KeyCode::KeyX);
        app.update();
    }
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Dots);
    press_key(&mut app, KeyCode::KeyZ);
    app.update();
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Flame);
```

- [ ] **Step 2: Run the tests, verify they fail**

Run: `cargo nextest run -p wc-core lifecycle state ui_picker`
Expected: FAIL (`SKETCH_ORDER` still 3 entries; `from_name("Flame")` still `None`).

- [ ] **Step 3: Rewire `state.rs`**

```rust
    pub const SKETCH_ORDER: [Self; 4] = [Self::Line, Self::Flame, Self::Dots, Self::Cymatics];
```

`from_name`: add `"flame" => Some(Self::Flame),` after the `"line"` arm.

`next_sketch` / `prev_sketch`:

```rust
    #[must_use]
    pub fn next_sketch(self) -> Self {
        match self {
            Self::Home | Self::Cymatics | Self::Waves => Self::Line,
            Self::Line => Self::Flame,
            Self::Flame => Self::Dots,
            Self::Dots => Self::Cymatics,
        }
    }

    #[must_use]
    pub fn prev_sketch(self) -> Self {
        match self {
            Self::Home | Self::Line | Self::Waves => Self::Cymatics,
            Self::Flame => Self::Line,
            Self::Dots => Self::Flame,
            Self::Cymatics => Self::Dots,
        }
    }
```

Update the doc comments on `SKETCH_ORDER`/`next_sketch`/`prev_sketch` that mention "Flame and Waves are de-routed seams" to say only Waves remains de-routed.

- [ ] **Step 4: Add the action, binding, and nav arm**

`actions.rs` — add the variant (with the doc-comment style of its neighbors) and extend `ALL`:

```rust
    /// Jump directly to the Flame sketch (2).
    SelectFlame,
```

```rust
    pub const ALL: [WaveConductorAction; 11] = [
        WaveConductorAction::SelectLine,
        WaveConductorAction::SelectFlame,
        WaveConductorAction::SelectDots,
        WaveConductorAction::SelectCymatics,
        WaveConductorAction::NavigateHome,
        WaveConductorAction::NavigateNext,
        WaveConductorAction::NavigatePrev,
        WaveConductorAction::ToggleVolume,
        WaveConductorAction::ToggleDevPanel,
        WaveConductorAction::ToggleFullscreen,
        WaveConductorAction::StartScreensaver,
    ];
```

Also update the `SelectDots` doc comment (it currently says "Digit2 intentionally unbound — was Flame").

`action_map.rs` `default_bindings()` — insert after the `SelectLine` line:

```rust
        (A::SelectFlame, Key(KeyCode::Digit2)),
```

(The existing `default_bindings_cover_all_actions` test now passes because `ALL` and the bindings both gained Flame.)

`nav.rs` `handle_navigation_actions` — add next to the other select arms (the match has a `_ => {}` wildcard, so the compiler will NOT remind you):

```rust
            A::SelectFlame => pressed_select = pressed_select.or(Some(AppState::Flame)),
```

- [ ] **Step 5: Run the tests, verify they pass**

Run: `cargo nextest run -p wc-core`
Expected: PASS, including `next_prev_cycle_matches_sketch_order`, `default_bindings_cover_all_actions`, and the updated guard tests.

- [ ] **Step 6: Smoke-test routing end to end**

Run: `WAVECONDUCTOR_START_SKETCH=flame cargo rund`
Expected: the app opens directly on Flame — currently a near-black `#10101f` screen with the settings dock available (no sim yet). Verify `Digit2` from Home also lands on Flame, `X`/`Z` cycle through four sketches, and the picker shows a live Flame tile.

- [ ] **Step 7: Gate and commit**

```bash
cargo fmt --all
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
git add crates/wc-core
git commit -m "feat(lifecycle): route Flame back into the sketch cycle (audit T5 reversal)"
```

---

## Stage 3 — GPU compute (level-parallel IFS)

Three tasks: the POD types + encoding (F5), the compute pipeline + kernel (F6), and the main-world drivers (F7). Nothing is visible until Stage 4's renderer, so F5–F7 verify through unit tests + `cargo xtask validate-shaders` + a crash-free `cargo rund` smoke; the first visual confirmation is F8's.

### Task F5: GPU POD types + branch encoding (`compute/sim_params.rs`)

**Files:**
- Create: `crates/wc-sketches/src/flame/compute/mod.rs` (`pub mod pipeline; pub mod sim_params;` — `pipeline` commented out until F6)
- Create: `crates/wc-sketches/src/flame/compute/sim_params.rs`
- Modify: `crates/wc-sketches/src/flame/mod.rs` (add `pub mod compute;`)
- Test: `#[cfg(test)]` in `sim_params.rs`

**Interfaces:**
- Produces (all `#[repr(C)]`, `bytemuck::{Pod, Zeroable}`, `Clone, Copy`):
  - `pub struct FlameNodeGpu { pub pos: [f32; 3], pub _pad0: f32, pub color: [f32; 3], pub _pad1: f32 }` (32 B)
  - `pub struct FlameBranchGpu { pub mat_x: [f32; 4], pub mat_y: [f32; 4], pub mat_z: [f32; 4], pub offset: [f32; 4], pub color: [f32; 4], pub var_a: u32, pub var_b: u32, pub mode: u32, pub _pad: u32 }` (96 B)
  - `pub struct FlameSimParamsGpu { pub branches: [FlameBranchGpu; 8], pub warp: [f32; 2], pub lerp_pos: f32, pub lerp_col: f32, pub branch_count: u32, pub _pad: [u32; 3] }` (800 B)
  - `pub struct FlameLevelParamsGpu { pub level_start: u32, pub node_count: u32, pub parent_start: u32, pub parent_count: u32, pub _pad: [u32; 60] }` (256 B — the dynamic-offset stride)
- `pub const LEVEL_PARAMS_STRIDE: u64 = 256;` and compile-time size asserts for all four types.
- `pub fn encode_branches(spec: &FlameSpec) -> FlameSimParamsGpu` — packs `AFFINE_MATS`/`AFFINE_OFFSETS` rows, colors, variation ids/modes; `warp = [0,0]`, `lerp_pos = 0.8`, `lerp_col = 0.75`.
- `pub fn encode_levels(layout: &LevelLayout, out: &mut [FlameLevelParamsGpu; MAX_LEVELS]) -> u32` — fills slot `i` from tree level `i + 1` (the root level 0 is never dispatched) and returns the filled count.
- `#[derive(Resource, Clone, ExtractResource)] pub struct FlameSimParams { pub params: FlameSimParamsGpu, pub levels: [FlameLevelParamsGpu; MAX_LEVELS], pub level_count: u32, pub nodes: Handle<ShaderBuffer> }` — POD + one Handle, so the per-frame extract clone is a memcpy (the Cymatics F2 lesson). `level_count` is the number of **dispatched** levels this frame; `0` freezes the sim (Idle).
- Consumes: `FlameSpec`/`AFFINE_*` (F1), `LevelLayout`/`MAX_LEVELS` (F2).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use crate::flame::branches::build_flame_spec;
    use crate::flame::levels::LevelLayout;

    /// WGSL layout contract: sizes are exact and 16-byte aligned; the level
    /// slot equals the dynamic-offset stride.
    #[test]
    fn pod_sizes_match_wgsl_layout() {
        assert_eq!(std::mem::size_of::<FlameNodeGpu>(), 32);
        assert_eq!(std::mem::size_of::<FlameBranchGpu>(), 96);
        assert_eq!(std::mem::size_of::<FlameSimParamsGpu>(), 800);
        assert_eq!(
            std::mem::size_of::<FlameLevelParamsGpu>() as u64,
            LEVEL_PARAMS_STRIDE
        );
    }

    /// Encoding packs the affine tables row-for-row and the variation
    /// ids/modes as their u32 reprs; unused branch slots stay zeroed.
    #[test]
    fn encode_branches_packs_v4_tables() {
        let spec = build_flame_spec("who are you?"); // 5 branches (F1 golden)
        let gpu = encode_branches(&spec);
        assert_eq!(gpu.branch_count, 5);
        assert!((gpu.lerp_pos - 0.8).abs() < f32::EPSILON);
        assert!((gpu.lerp_col - 0.75).abs() < f32::EPSILON);
        // Branch 0 golden: affine Negate(4) -> -I, varA Spherical(2),
        // mode Interpolated(1), varB Sin(1).
        let b0 = &gpu.branches[0];
        assert_eq!(b0.mat_x, [-1.0, 0.0, 0.0, 0.0]);
        assert_eq!(b0.mat_y, [0.0, -1.0, 0.0, 0.0]);
        assert_eq!(b0.mat_z, [0.0, 0.0, -1.0, 0.0]);
        assert_eq!(b0.offset, [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(b0.var_a, 2);
        assert_eq!(b0.var_b, 1);
        assert_eq!(b0.mode, 1);
        assert!((b0.color[0] - spec.branches[0].color[0]).abs() < f32::EPSILON);
        // Slot 5..8 unused -> zeroed.
        assert_eq!(gpu.branches[5].mode, 0);
        assert_eq!(gpu.branches[7].mat_x, [0.0; 4]);
    }

    /// Level encoding fills dispatched levels only (tree level i+1 in slot i)
    /// and returns the dispatch count = levels - 1 (root is never dispatched).
    #[test]
    fn encode_levels_skips_root() {
        let layout = LevelLayout::build(5, 100_000.0);
        let mut slots = [FlameLevelParamsGpu::zeroed(); crate::flame::levels::MAX_LEVELS];
        let n = encode_levels(&layout, &mut slots);
        assert_eq!(n as usize, layout.levels.len() - 1);
        // Slot 0 = tree level 1: 5 nodes starting at 1, parented on the root.
        assert_eq!(slots[0].level_start, 1);
        assert_eq!(slots[0].node_count, 5);
        assert_eq!(slots[0].parent_start, 0);
        assert_eq!(slots[0].parent_count, 1);
        // Last slot = deepest level.
        let deepest = layout.levels.last().expect("levels");
        let last = &slots[(n - 1) as usize];
        assert_eq!(last.level_start, deepest.start);
        assert_eq!(last.node_count, deepest.count);
    }
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo nextest run -p wc-sketches flame::compute`
Expected: FAIL (module undefined).

- [ ] **Step 3: Implement `sim_params.rs`**

```rust
//! GPU-side POD mirrors for the Flame IFS compute pass, plus the extract
//! resource the render world reads.
//!
//! Layout contract with `assets/shaders/flame/simulate.wgsl` (kernel parity
//! discipline: change both together, term for term). All structs are
//! 16-byte-multiple sized, compile-time asserted.

use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResource;
use bevy::render::storage::ShaderBuffer;
use bytemuck::{Pod, Zeroable};

use crate::flame::branches::{FlameSpec, AFFINE_MATS, AFFINE_OFFSETS};
use crate::flame::levels::{LevelLayout, MAX_LEVELS};

/// One IFS node: position + accumulated color. 32 bytes, matching WGSL
/// `struct FlameNode { pos: vec3<f32>, _pad0: f32, color: vec3<f32>, _pad1: f32 }`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct FlameNodeGpu {
    /// World-space position (pre camera/model transform).
    pub pos: [f32; 3],
    /// Padding (vec3 alignment).
    pub _pad0: f32,
    /// Accumulated additive color (can exceed [0,1]).
    pub color: [f32; 3],
    /// Padding.
    pub _pad1: f32,
}

/// One branch: row-major affine (rows in `mat_x/y/z.xyz`), constant offset,
/// additive color, and the variation switch keys. 96 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct FlameBranchGpu {
    /// Affine matrix row 0 (`.w` unused).
    pub mat_x: [f32; 4],
    /// Affine matrix row 1.
    pub mat_y: [f32; 4],
    /// Affine matrix row 2.
    pub mat_z: [f32; 4],
    /// Affine constant offset (`.w` unused).
    pub offset: [f32; 4],
    /// Additive per-application color (`.w` unused).
    pub color: [f32; 4],
    /// Primary variation id (`VariationId` repr).
    pub var_a: u32,
    /// Secondary variation id (== `var_a` for Single mode).
    pub var_b: u32,
    /// Combinator mode (`VariationMode` repr).
    pub mode: u32,
    /// Padding to 96 bytes.
    pub _pad: u32,
}

/// Frame-constant sim uniform: the branch table plus the per-frame attractor
/// drivers. 800 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct FlameSimParamsGpu {
    /// Up to 8 branches (see `branches::MAX_BRANCHES`); unused slots zeroed.
    pub branches: [FlameBranchGpu; 8],
    /// Per-frame attractor offset added to x/y after the base affine:
    /// `(cX/5 + cDx, cY/5 + cDy)` — v4's time oscillation + pointer/hand warp.
    pub warp: [f32; 2],
    /// Position lerp factor (v4: 0.8).
    pub lerp_pos: f32,
    /// Color lerp factor (v4: 0.75).
    pub lerp_col: f32,
    /// Live branch count (2..=8).
    pub branch_count: u32,
    /// Padding to 800 bytes.
    pub _pad: [u32; 3],
}

/// Per-level dispatch parameters, one 256-byte dynamic-offset slot per
/// dispatched level (the Cymatics stride pattern).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct FlameLevelParamsGpu {
    /// First node slot of this level.
    pub level_start: u32,
    /// Node count in this level.
    pub node_count: u32,
    /// First node slot of the parent level.
    pub parent_start: u32,
    /// Node count of the parent level (branch-major divisor).
    pub parent_count: u32,
    /// Padding to the 256-byte dynamic-offset stride.
    pub _pad: [u32; 60],
}

/// Dynamic-offset stride: `min_uniform_buffer_offset_alignment` is <= 256 on
/// every WebGPU target (verified at pipeline init, as Cymatics does).
pub const LEVEL_PARAMS_STRIDE: u64 = 256;

const _: () = assert!(std::mem::size_of::<FlameNodeGpu>() == 32);
const _: () = assert!(std::mem::size_of::<FlameBranchGpu>() == 96);
const _: () = assert!(std::mem::size_of::<FlameSimParamsGpu>() == 800);
const _: () = assert!(std::mem::size_of::<FlameLevelParamsGpu>() as u64 == LEVEL_PARAMS_STRIDE);

/// Extract resource mirrored into the render world each frame. POD fields +
/// one `Handle` so the `ExtractResourcePlugin` clone is a memcpy (no heap —
/// the Cymatics F2 lesson).
#[derive(Resource, Clone, ExtractResource)]
pub struct FlameSimParams {
    /// Frame-constant sim uniform contents.
    pub params: FlameSimParamsGpu,
    /// Per-level slots; `levels[i]` is tree level `i + 1` (root never
    /// dispatched). Only `level_count` slots are meaningful.
    pub levels: [FlameLevelParamsGpu; MAX_LEVELS],
    /// Levels to dispatch this frame. `0` freezes the fractal (Idle), the
    /// ember prefix lowers it during the screensaver.
    pub level_count: u32,
    /// The node storage buffer (owned here; the render material clones it).
    pub nodes: Handle<ShaderBuffer>,
}

/// Pack a [`FlameSpec`] into the GPU branch table. Warp starts at zero; the
/// per-frame writer overwrites it every frame.
#[must_use]
pub fn encode_branches(spec: &FlameSpec) -> FlameSimParamsGpu {
    let mut branches = [FlameBranchGpu::zeroed(); 8];
    for (slot, b) in branches.iter_mut().zip(&spec.branches) {
        let m = &AFFINE_MATS[b.affine_idx];
        let o = &AFFINE_OFFSETS[b.affine_idx];
        slot.mat_x = [m[0], m[1], m[2], 0.0];
        slot.mat_y = [m[3], m[4], m[5], 0.0];
        slot.mat_z = [m[6], m[7], m[8], 0.0];
        slot.offset = [o[0], o[1], o[2], 0.0];
        slot.color = [b.color[0], b.color[1], b.color[2], 0.0];
        slot.var_a = b.var_a as u32;
        slot.var_b = b.var_b as u32;
        slot.mode = b.mode as u32;
    }
    FlameSimParamsGpu {
        branches,
        warp: [0.0, 0.0],
        lerp_pos: 0.8,
        lerp_col: 0.75,
        branch_count: u32::try_from(spec.branches.len()).unwrap_or(2),
        _pad: [0; 3],
    }
}

/// Fill the per-level slots from a layout (tree level `i + 1` into slot `i`)
/// and return the total dispatchable level count.
pub fn encode_levels(
    layout: &LevelLayout,
    out: &mut [FlameLevelParamsGpu; MAX_LEVELS],
) -> u32 {
    let mut n = 0_u32;
    for (slot, level) in out.iter_mut().zip(layout.levels.iter().skip(1)) {
        slot.level_start = level.start;
        slot.node_count = level.count;
        slot.parent_start = level.parent_start;
        slot.parent_count = level.parent_count;
        n += 1;
    }
    n
}
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo nextest run -p wc-sketches flame::compute`
Expected: PASS (3 tests).

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features --workspace -- -D warnings && cargo nextest run --workspace --all-features
git add crates/wc-sketches/src/flame
git commit -m "feat(flame): GPU POD types and branch/level encoding"
```

### Task F6: Compute pipeline + `simulate.wgsl`

**Files:**
- Create: `crates/wc-sketches/src/flame/compute/pipeline.rs`
- Create: `assets/shaders/flame/simulate.wgsl`
- Modify: `crates/wc-sketches/src/flame/compute/mod.rs` (enable `pub mod pipeline;`)
- Modify: `crates/wc-sketches/src/lib.rs` (register `FlameComputePlugin` next to `CymaticsComputePlugin`)

**Interfaces:**
- Produces: `pub struct FlameComputePlugin;` — adds `ExtractResourcePlugin::<FlameSimParams>`, the `ExtractSchedule` removal companion, `RenderStartup` pipeline init, `Render/PrepareBindGroups` prepare (gated `resource_exists::<FlameSimParams>`), and the dispatch system in the `RenderGraph` schedule `.before(camera_driver)`.
- Consumes: F5's types. Model every mechanism on `crates/wc-sketches/src/cymatics/compute/pipeline.rs` (init at :198, prepare at :320, dispatch at :468, removal companion at :559) — same resource shapes, same caching discipline, storage **buffer** bindings instead of textures.

- [ ] **Step 1: Write `simulate.wgsl`**

```wgsl
// Flame IFS: one dispatch per tree level; each thread computes one node from
// its parent (updated by the previous dispatch — WebGPU guarantees storage
// visibility between dispatches in a pass).
//
// Kernel parity: this file mirrors crates/wc-sketches/src/flame/branches.rs
// (apply_variation_cpu / apply_branch_cpu) term for term. Change both together.

struct FlameNode {
    pos: vec3<f32>,
    _pad0: f32,
    color: vec3<f32>,
    _pad1: f32,
}

struct Branch {
    mat_x: vec4<f32>,
    mat_y: vec4<f32>,
    mat_z: vec4<f32>,
    offset: vec4<f32>,
    color: vec4<f32>,
    var_a: u32,
    var_b: u32,
    mode: u32,
    _pad: u32,
}

struct SimParams {
    branches: array<Branch, 8>,
    warp: vec2<f32>,
    lerp_pos: f32,
    lerp_col: f32,
    branch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

struct LevelParams {
    level_start: u32,
    node_count: u32,
    parent_start: u32,
    parent_count: u32,
}

@group(0) @binding(0) var<uniform> sim: SimParams;
@group(0) @binding(1) var<storage, read_write> nodes: array<FlameNode>;
@group(0) @binding(2) var<uniform> level: LevelParams;

const PI: f32 = 3.14159265358979;
// v4: points escaping |p| > 50 are pulled back with the Spherical variation.
const ESCAPE_RADIUS_SQ: f32 = 2500.0;

// The seven v4 variations (transforms.ts::VARIATIONS), zero-safe like
// THREE.js (normalize/setLength divide by length || 1).
fn apply_variation(id: u32, p: vec3<f32>) -> vec3<f32> {
    let len_sq = dot(p, p);
    switch id {
        case 0u: { return p; }                                   // Linear
        case 1u: { return sin(p); }                              // Sin
        case 2u: {                                               // Spherical
            if (len_sq == 0.0) { return p; }
            return p / len_sq;
        }
        case 3u: {                                               // Polar
            return vec3<f32>(atan2(p.y, p.x) / PI, sqrt(len_sq) - 1.0, atan2(p.z, p.x));
        }
        case 4u: {                                               // Swirl
            let s = sin(len_sq);
            let c = cos(len_sq);
            return vec3<f32>(p.z * s - p.y * c, p.x * c + p.z * s, p.x * s - p.y * s);
        }
        case 5u: {                                               // Normalize
            if (len_sq == 0.0) { return p; }
            return p / sqrt(len_sq);
        }
        default: {                                               // Shrink
            if (len_sq == 0.0) { return p; }
            return p * (exp(-len_sq) / sqrt(len_sq));
        }
    }
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let local = gid.x;
    if (local >= level.node_count) {
        return;
    }
    // Branch-major layout: contiguous runs share a branch (warp-coherent switch).
    let branch_idx = local / level.parent_count;
    let parent_idx = level.parent_start + (local % level.parent_count);
    let node_idx = level.level_start + local;

    let b = sim.branches[branch_idx];
    let parent = nodes[parent_idx];

    // Affine (row-major mat * p + offset), then the per-frame warp on x/y,
    // matching v4's randomBranch closure order (warp BEFORE the variation).
    var target = vec3<f32>(
        dot(b.mat_x.xyz, parent.pos),
        dot(b.mat_y.xyz, parent.pos),
        dot(b.mat_z.xyz, parent.pos),
    ) + b.offset.xyz;
    target = vec3<f32>(target.x + sim.warp.x, target.y + sim.warp.y, target.z);

    // Variation combinator: single / interpolated(0.5) / router(z < 0).
    switch b.mode {
        case 0u: { target = apply_variation(b.var_a, target); }
        case 1u: {
            target = mix(apply_variation(b.var_a, target), apply_variation(b.var_b, target), 0.5);
        }
        default: {
            if (target.z < 0.0) {
                target = apply_variation(b.var_a, target);
            } else {
                target = apply_variation(b.var_b, target);
            }
        }
    }

    // Per-frame settle: lerp toward the target (v4: 0.8 pos / 0.75 color).
    let node = nodes[node_idx];
    var new_pos = mix(node.pos, target, sim.lerp_pos);
    // Escape pullback (v4: Spherical when |p|^2 > 2500).
    let esc_sq = dot(new_pos, new_pos);
    if (esc_sq > ESCAPE_RADIUS_SQ) {
        new_pos = new_pos / esc_sq;
    }
    let target_col = parent.color + b.color.rgb;
    let new_col = mix(node.color, target_col, sim.lerp_col);

    nodes[node_idx].pos = new_pos;
    nodes[node_idx].color = new_col;
}
```

- [ ] **Step 2: Validate the shader**

Run: `cargo xtask validate-shaders`
Expected: PASS (naga accepts `simulate.wgsl`).

- [ ] **Step 3: Implement `pipeline.rs`**

Model every mechanism on the Cymatics pipeline (same file layout, same doc style). The full shape:

```rust
//! Render-world compute plugin for the Flame IFS.
//!
//! Per frame: upload the sim uniform + per-level slots, then run ONE compute
//! pass with `level_count` sequential dispatches — dispatch i updates tree
//! level i+1 from level i via dynamic offset i * 256 into the level-params
//! buffer. WebGPU's implicit ordering between dispatches in a pass makes the
//! parent level's writes visible to the child level's reads.

use std::borrow::Cow;

use bevy::prelude::*;
use bevy::render::camera::camera_driver;
use bevy::render::extract_resource::{Extract, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::*;
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue};
use bevy::render::storage::GpuShaderBuffer;
use bevy::render::{RenderApp, RenderStartup, RenderSystems};

use super::sim_params::{
    FlameLevelParamsGpu, FlameSimParams, FlameSimParamsGpu, LEVEL_PARAMS_STRIDE,
};
use crate::flame::levels::MAX_LEVELS;

/// Compute workgroup width; level dispatches are ceil(node_count / 256).
const WORKGROUP_SIZE: u32 = 256;

/// Registers extraction, pipeline init, per-frame prepare, and the dispatch.
pub struct FlameComputePlugin;

impl Plugin for FlameComputePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ExtractResourcePlugin::<FlameSimParams>::default());
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        // ExtractResourcePlugin does NOT propagate removals — manual companion
        // (the established landmine; see cymatics/compute/pipeline.rs:559).
        render_app.add_systems(ExtractSchedule, remove_flame_sim_params_if_absent);
        render_app
            .add_systems(RenderStartup, init_flame_pipeline)
            .add_systems(
                Render,
                prepare_flame_bind_groups
                    .in_set(RenderSystems::PrepareBindGroups)
                    .run_if(resource_exists::<FlameSimParams>),
            );
        render_app.add_systems(bevy::render::RenderGraph, flame_compute.before(camera_driver));
    }
}
```

`FlamePipeline` resource: `bind_group_layout_descriptor` (three entries: binding 0 uniform `min_binding_size = 800`; binding 1 `BufferBindingType::Storage { read_only: false }`; binding 2 uniform, `has_dynamic_offset: true`, `min_binding_size = 16`), `pipeline_id` (queued compute pipeline on `shaders/flame/simulate.wgsl`, entry `main`), `sim_params_buffer` (800 B, `UNIFORM | COPY_DST`), `level_buffer` (`LEVEL_PARAMS_STRIDE * MAX_LEVELS as u64`, `UNIFORM | COPY_DST`). Copy the Cymatics `init_cymatics_pipeline` body shape including the `min_uniform_buffer_offset_alignment <= 256` check.

`FlameComputeBindGroups` resource: `bind_group: BindGroup`, `dispatch: [(u32, u32); MAX_LEVELS]` (dynamic offset, workgroup count per level), `level_count: u32`.

`prepare_flame_bind_groups` (mirror the Cymatics prepare):
- `render_queue.0.write_buffer(&pipeline.sim_params_buffer, 0, bytemuck::bytes_of(&sim.params))`.
- For `i in 0..sim.level_count`: write the 16-byte head of `sim.levels[i]` at offset `u64::from(i) * LEVEL_PARAMS_STRIDE` (`bytemuck::bytes_of` of a `[u32; 4]` built from the slot's four fields — do not upload the 240 pad bytes).
- Resolve the node buffer: `let Some(gpu_nodes) = buffers.get(&sim.nodes) else { return; }` with `buffers: Res<RenderAssets<GpuShaderBuffer>>`. **If the Gpu render-asset type name differs in this Bevy version, copy the exact `RenderAssets<...>` parameter type used by `crates/wc-sketches/src/particles/compute/` (it binds the same `ShaderBuffer` asset).**
- Bind-group cache: `Local<Option<(BufferId, BindGroup)>>` keyed on `gpu_nodes.buffer.id()` — rebuild only when the buffer changed (name-change reseed reallocates it). Entries: 0 = `pipeline.sim_params_buffer.as_entire_binding()`, 1 = `gpu_nodes.buffer.as_entire_binding()`, 2 = `BindingResource::Buffer(BufferBinding { buffer: &pipeline.level_buffer, offset: 0, size: Some(<NonZeroU64 of 16>) })`.
- Fill `dispatch[i] = (i * 256, sim.levels[i].node_count.div_ceil(WORKGROUP_SIZE))` and insert `FlameComputeBindGroups`.

`flame_compute` (mirror `cymatics_compute`): early-return unless bind groups + pipeline + compiled compute pipeline exist; skip entirely when `level_count == 0`; one `begin_compute_pass`, `set_pipeline`, then:

```rust
        for i in 0..bg.level_count {
            let (offset, workgroups) = bg.dispatch[i as usize];
            pass.set_bind_group(0, &bg.bind_group, &[offset]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
```

`remove_flame_sim_params_if_absent`: identical shape to the Cymatics companion (`Extract<Option<Res<FlameSimParams>>>` + render-world `Option<Res>` → `remove_resource`).

In `crates/wc-sketches/src/lib.rs`, register once next to `CymaticsComputePlugin`:

```rust
app.add_plugins(crate::flame::compute::pipeline::FlameComputePlugin);
```

- [ ] **Step 4: Compile + gate**

Run: `cargo clippy --all-targets --all-features --workspace -- -D warnings && cargo nextest run --workspace --all-features && cargo xtask validate-shaders`
Expected: PASS (no behavior yet — `FlameSimParams` is never inserted until F7, so the render systems no-op).

- [ ] **Step 5: Commit**

```bash
git add crates/wc-sketches assets/shaders/flame
git commit -m "feat(flame): level-parallel IFS compute pipeline and kernel"
```

### Task F7: Main-world drivers (spawn, name-change rebuild, per-frame writer, idle freeze)

**Files:**
- Create: `crates/wc-sketches/src/flame/systems/mod.rs`, `systems/spawn.rs`, `systems/name_change.rs`, `systems/sim_params.rs`
- Modify: `crates/wc-sketches/src/flame/mod.rs` (wire the systems)
- Test: `#[cfg(test)]` in each new file

**Interfaces:**
- Produces:
  - `pub struct FlameRoot;` marker component (in `spawn.rs`).
  - `#[derive(Resource)] pub struct FlameState { pub spec: FlameSpec, pub layout: LevelLayout, pub last_name: String, pub last_target_points: f32, pub c_x: f32, pub warp_input: Vec2, pub complexity: f32 }`.
  - `spawn_flame` (`OnEnter`): allocates the node `ShaderBuffer` (capacity `MAX_POINTS` zeroed `FlameNodeGpu`s — mirror the exact `ShaderBuffer::new` + usage-flag construction in `crates/wc-sketches/src/line/systems/spawn.rs:213-218`, adding `BufferUsages::COPY_DST`), inserts `FlameSimParams` (branches encoded from the persisted name, `level_count` full) and `FlameState` (`complexity: 1.0`), and seeds the buffer via `reseed_nodes`.
  - `remove_flame_resources` (`OnExit`): removes `FlameSimParams` + `FlameState` (drops the buffer handle → VRAM released; the render-world copy dies via the F6 companion).
  - `pub fn reseed_nodes(buffers: &mut Assets<ShaderBuffer>, handle: &Handle<ShaderBuffer>, total: u32)` (in `name_change.rs`): writes `total` zeroed nodes with node 0 = root at `pos [3.0, 3.0, 3.0]` (v4 `jumpiness`), color black — v4's fresh tree starts all children at the origin and lets the 0.8 lerp bloom the shape in.
  - `watch_flame_name` (`Update`, gated `in_state(AppState::Flame)` — NOT `sketch_active`, because the screensaver carousel changes the name too): when `normalize_name(&settings.name) != state.last_name` or `settings.target_points != state.last_target_points` → rebuild `spec` + `layout`, re-encode `sim.params.branches`/`branch_count`, `encode_levels`, `reseed_nodes`, update `state`. (F8 extends it to rebuild the mesh; F14 extends it to push the audio config.)
  - `pub fn flame_cx(elapsed_secs: f64) -> f32` — `2 * sigmoid(6 * sin(t/3)) - 1` (pure; v4's `±10` sigmoid clamps are unreachable at `|x| <= 6`).
  - `pub fn bake_flame_sim(state: &FlameState, sim: &mut FlameSimParams)` — the ONE baker (Condition A1: one baker, multiple writers, cannot drift): writes `sim.params.warp = [state.c_x / 5.0 + state.warp_input.x, state.spec.c_y / 5.0 + state.warp_input.y]`, computes `live = layout.live_count_for_complexity(state.complexity)` and `sim.level_count = layout.dispatch_levels_for_live(live).saturating_sub(1)`.
  - `update_flame_sim` (`Update`, gated `sketch_active(AppState::Flame)`): advances `state.c_x` from `Res<Time>` (`time.elapsed_secs_f64()` — the capture harness pins virtual time, so captures are deterministic), maps `PointerState.primary` to `warp_input` (`x/w * 2 - 1`, `y/h * 2 - 1`, matching v4's `mapLinear`; keep the last value when `primary` is `None`, as v4's mouse position persists), holds `complexity = 1.0`, then `bake_flame_sim`.
  - `freeze_flame_sim` (`OnEnter(SketchActivity::Idle)`, gated `in_state(AppState::Flame)`): sets `sim.level_count = 0` — zero dispatches while frozen (v4 froze on idle). Waking re-enters `Active`, where `update_flame_sim` restores it next frame.
- Consumes: F1 `build_flame_spec`/`normalize_name`, F2 `LevelLayout`, F5 `encode_branches`/`encode_levels`/`FlameSimParams`, `wc_core::input::pointer::PointerState`, `wc_core::sketch::despawn_with`.

**Wiring added to `FlamePlugin::build`:**

```rust
        app.add_systems(
            OnEnter(AppState::Flame),
            (systems::spawn::spawn_flame, enter_flame_clear_color),
        );
        app.add_systems(
            OnExit(AppState::Flame),
            (
                wc_core::sketch::despawn_with::<systems::spawn::FlameRoot>,
                systems::spawn::remove_flame_resources,
                exit_flame_clear_color,
                wc_core::sketch::reset_render_profile,
            ),
        );
        app.add_systems(
            Update,
            systems::name_change::watch_flame_name.run_if(in_state(AppState::Flame)),
        );
        app.add_systems(
            Update,
            systems::sim_params::update_flame_sim
                .after(systems::name_change::watch_flame_name)
                .run_if(wc_core::sketch::sketch_active(AppState::Flame)),
        );
        app.add_systems(
            OnEnter(wc_core::lifecycle::state::SketchActivity::Idle),
            systems::sim_params::freeze_flame_sim.run_if(in_state(AppState::Flame)),
        );
```

- [ ] **Step 1: Write the failing tests**

In `systems/sim_params.rs`:

```rust
#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use crate::flame::branches::build_flame_spec;
    use crate::flame::levels::LevelLayout;

    /// cX golden points: sigmoid oscillation matches v4's closed form.
    /// At t=0: sin=0, sigmoid(0)=0.5 -> cX=0. Quarter period (sin arg = pi/2
    /// at elapsed = 3*pi/2): cX = 2*sigmoid(6)-1 ~ 0.99505475.
    #[test]
    fn flame_cx_matches_v4_formula() {
        assert!(flame_cx(0.0).abs() < 1e-6);
        let quarter = flame_cx(3.0 * std::f64::consts::FRAC_PI_2);
        assert!((quarter - 0.995_054_75).abs() < 1e-5, "got {quarter}");
        // Bounded in (-1, 1).
        for i in 0..100 {
            let v = flame_cx(f64::from(i) * 0.37);
            assert!((-1.0..=1.0).contains(&v));
        }
    }

    /// The baker writes warp = (cX/5 + cdx, cY/5 + cdy) and a full dispatch
    /// prefix at complexity 1.0; complexity 0.0 freezes to zero dispatches
    /// beyond the root.
    #[test]
    fn bake_writes_warp_and_levels() {
        let spec = build_flame_spec("madison");
        let c_y = spec.c_y;
        let layout = LevelLayout::build(4, 100_000.0);
        let full_levels = u32::try_from(layout.levels.len()).expect("fits") - 1;
        let mut state = FlameState {
            spec,
            layout,
            last_name: "madison".into(),
            last_target_points: 100_000.0,
            c_x: 0.5,
            warp_input: Vec2::new(0.2, -0.1),
            complexity: 1.0,
        };
        let mut sim = test_sim_params(&state);
        bake_flame_sim(&state, &mut sim);
        assert!((sim.params.warp[0] - (0.5 / 5.0 + 0.2)).abs() < 1e-6);
        assert!((sim.params.warp[1] - (c_y / 5.0 - 0.1)).abs() < 1e-6);
        assert_eq!(sim.level_count, full_levels);

        state.complexity = 0.0;
        bake_flame_sim(&state, &mut sim);
        assert_eq!(sim.level_count, 0, "root-only prefix dispatches nothing");
    }
}
```

(`test_sim_params` is a small local helper constructing `FlameSimParams` with `encode_branches`/`encode_levels` and `Handle::default()`.)

In `systems/name_change.rs`, a `World`-level test (`run_system_once` pattern from the Dots tests): insert `FlameSettings { name: "xy".into(), ..default }`, `FlameState` built for `"madison"`, `Assets<ShaderBuffer>` with a seeded buffer, and a `FlameSimParams`; run `watch_flame_name`; assert `state.last_name == "xy"`, `sim.params.branch_count == 3` (the "xy" golden), and the buffer asset's byte length equals `total * 32` with node 0 = root `[3.0, 3.0, 3.0]`.

- [ ] **Step 2: Run, verify failure** — `cargo nextest run -p wc-sketches flame::systems` → FAIL (modules undefined).

- [ ] **Step 3: Implement the three system files + wiring**

Follow the Interfaces block above precisely. Key bodies not already spelled out there:

```rust
/// Pure v4 oscillation: cX = 2*sigmoid(6*sin(elapsed/3)) - 1.
#[must_use]
pub fn flame_cx(elapsed_secs: f64) -> f32 {
    let x = 6.0 * (elapsed_secs / 3.0).sin();
    let sig = 1.0 / (1.0 + (-x).exp());
    (2.0 * sig - 1.0) as f32
}
```

```rust
/// One baker, two writers (live + screensaver) — Condition A1.
pub fn bake_flame_sim(state: &FlameState, sim: &mut FlameSimParams) {
    sim.params.warp = [
        state.c_x / 5.0 + state.warp_input.x,
        state.spec.c_y / 5.0 + state.warp_input.y,
    ];
    let live = state.layout.live_count_for_complexity(state.complexity);
    sim.level_count = state
        .layout
        .dispatch_levels_for_live(live)
        .saturating_sub(1);
}
```

`reseed_nodes` allocates a `Vec<FlameNodeGpu>` of `total` zeroed nodes, sets node 0 to the root, and replaces the asset's data (name-change path — allocation acceptable, documented). `update_flame_sim` reads `Res<Time>`, `Res<PointerState>`, `Single<&Window>`, `ResMut<FlameState>`, `ResMut<FlameSimParams>` — all stack math, no allocation.

- [ ] **Step 4: Run tests, verify pass** — `cargo nextest run -p wc-sketches flame` → PASS.

- [ ] **Step 5: Smoke** — `WAVECONDUCTOR_START_SKETCH=flame cargo rund`: still visually near-black (no renderer yet), but no panics, and Shift+D's inspector shows `FlameSimParams` present with a non-zero `level_count`. Exit to Home and re-enter: no leak, no panic (resource removal path).

- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features --workspace -- -D warnings && cargo nextest run --workspace --all-features
git add crates/wc-sketches
git commit -m "feat(flame): sim drivers - spawn, name rebuild, warp writer, idle freeze"
```

---

## Stage 4 — Rendering (additive billboards, in-material 3D projection)

### Task F8: `FlameMaterial` + `render.wgsl` + mesh spawn

**Files:**
- Create: `crates/wc-sketches/src/flame/render.rs`
- Create: `assets/shaders/flame/render.wgsl`
- Create: `assets/sketches/flame/disc.png` (copied from v4)
- Modify: `crates/wc-sketches/src/flame/mod.rs` (wire `drive_flame_material`)
- Modify: `crates/wc-sketches/src/flame/systems/spawn.rs` (spawn mesh + material under `FlameRoot`)
- Modify: `crates/wc-sketches/src/flame/systems/name_change.rs` (resize mesh on rebuild)
- Modify: `crates/wc-sketches/src/lib.rs` (register `Material2dPlugin::<FlameMaterial>`)
- Test: `#[cfg(test)]` in `render.rs` + extended `name_change.rs` test

**Interfaces:**
- Produces:

```rust
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct FlameMaterial {
    #[storage(0, read_only)]
    pub nodes: Handle<ShaderBuffer>,
    #[texture(1)]
    #[sampler(2)]
    pub disc_texture: Handle<Image>,
    /// view_from_world * model. Model bakes v4's `pointCloud.rotateX(-PI/2)`.
    #[uniform(3)]
    pub view_from_model: Mat4,
    /// Perspective projection: fovy 60 deg, near 0.01, far 25 (v4 camera).
    #[uniform(4)]
    pub clip_from_view: Mat4,
    /// x: focal_length (camera distance), y: base point size px,
    /// z: DoF strength, w: point opacity.
    #[uniform(5)]
    pub render_a: Vec4,
    /// x: live node count, y: gamma, z: brightness, w: point size clamp px.
    #[uniform(6)]
    pub render_b: Vec4,
    /// xyz: fog color (linear), w: unused.
    #[uniform(7)]
    pub fog_color: Vec4,
    /// x: fog near, y: fog far, z/w: viewport width/height px.
    #[uniform(8)]
    pub fog_range: Vec4,
}
```

  - `impl Material2d for FlameMaterial`: both shader stages → `"shaders/flame/render.wgsl"`, `alpha_mode = AlphaMode2d::Blend` (routes into `Transparent2d`), and **`specialize` overrides the color-target blend to pure additive** — the mechanism that reproduces v4's `THREE.AdditiveBlending` inside the 2D pipeline:

```rust
    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(Some(target)) = fragment.targets.get_mut(0) {
                target.blend = Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                });
            }
        }
        Ok(())
    }
```

  - `pub fn default_view_matrices(aspect: f32) -> (Mat4, Mat4)` — the v4 start pose until the orbit driver (F9) takes over: eye `(0.0, 0.35, 0.7)` looking at origin (up +Y), `view_from_model = view * Mat4::from_rotation_x(-FRAC_PI_2)`, `clip_from_view = Mat4::perspective_rh(60_f32.to_radians(), aspect, 0.01, 25.0)`.
  - `pub fn flame_fog_color() -> Vec4` — `#10101f` converted to linear (`Color::srgb_u8(0x10,0x10,0x1f).to_linear()` components).
  - `drive_flame_material` (`Update`, gated `in_state(AppState::Flame)` so it also runs during Idle/Screensaver like `drive_dots_master_brightness`): packs settings + `FlameState` into the uniforms each frame (matrices come from F9's `FlameCamera` once it exists; until then `default_view_matrices`); `render_b.x = layout.live_count_for_complexity(state.complexity) as f32`.
- Consumes: F5 `FlameSimParams` (shares the `nodes` handle), F7 `FlameState`, `FlameSettings`.
- Blend-factor semantics note (documented in the module doc): v4's `AdditiveBlending` is `(SrcAlpha, One)` with the shader's final `pow(rgba, 0.545)`; with `(One, One)` the fragment multiplies its own alpha in — `contribution = pow(rgb, gamma) * pow(alpha, gamma)` — which is algebraically identical.

- [ ] **Step 1: Copy the sprite asset**

```bash
cp .worktrees/v4/src/sketches/flame/disc.png assets/sketches/flame/disc.png
git add assets/sketches/flame/disc.png
```

- [ ] **Step 2: Write the failing tests**

In `render.rs`:

```rust
#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// The default pose matches v4: camera at (0, 0.35, 0.7), distance
    /// ~0.7826; the origin projects in front of the camera at that distance.
    #[test]
    fn default_pose_matches_v4_camera() {
        let (view_model, clip_from_view) = default_view_matrices(16.0 / 9.0);
        let origin_view = view_model * Vec4::new(0.0, 0.0, 0.0, 1.0);
        let dist = -origin_view.z;
        assert!((dist - (0.35_f32 * 0.35 + 0.7 * 0.7).sqrt()).abs() < 1e-5);
        let clip = clip_from_view * origin_view;
        assert!(clip.w > 0.0, "origin is in front of the near plane");
    }

    /// The model rotation is v4's rotateX(-PI/2): model-space +Z maps to
    /// world/view -Y-ish (a point above the fractal plane tips toward the
    /// camera's down axis, not away).
    #[test]
    fn model_bakes_negative_x_quarter_turn() {
        let (view_model, _) = default_view_matrices(1.0);
        let up_model = view_model * Vec4::new(0.0, 0.0, 1.0, 1.0);
        let origin = view_model * Vec4::new(0.0, 0.0, 0.0, 1.0);
        // rotateX(-PI/2) sends +Z to +Y in world; +Y is up on screen, so the
        // transformed point sits higher (greater view-space y) than the origin.
        assert!(up_model.y > origin.y);
    }

    /// Fog color is v4's #10101f in linear space.
    #[test]
    fn fog_color_is_linear_10101f() {
        let fog = flame_fog_color();
        let expect = Color::srgb_u8(0x10, 0x10, 0x1f).to_linear();
        assert!((fog.x - expect.red).abs() < 1e-6);
        assert!((fog.y - expect.green).abs() < 1e-6);
        assert!((fog.z - expect.blue).abs() < 1e-6);
    }
}
```

Extend the `name_change.rs` test: after `watch_flame_name` rebuilds for `"xy"` (3 branches, depth 10, total 88,573), the `FlameRoot` entity's mesh asset has `88_573 * 6` vertices.

- [ ] **Step 3: Run, verify failure** — `cargo nextest run -p wc-sketches flame::render` → FAIL.

- [ ] **Step 4: Write `render.wgsl`**

Copy the `#import` lines, `Corner`/`quad_corner` table, and `VertexOutput` scaffolding from `assets/shaders/particles/render.wgsl` (the house billboard idiom: 6 `vertex_index`es per instance, corner from `vertex_index % 6u`), then replace the projection and fragment logic:

```wgsl
// Flame additive point cloud. Projection happens HERE, not in a Camera3d:
// view_from_model/clip_from_view come from the CPU orbit camera as uniforms,
// so the global 2D camera pipeline (HDR + bloom + tonemapping) is untouched.
// Ports v4 flamePoints.vert.frag / flamePoints.frag:
//   - fake DoF: size and opacity fall off with |dist - focal| / focal
//   - additive accumulation with per-point opacity and a 1/255 alpha floor
//   - fog toward the scene background, then pow(x, gamma) shaping

struct FlameNode {
    pos: vec3<f32>,
    _pad0: f32,
    color: vec3<f32>,
    _pad1: f32,
}

@group(2) @binding(0) var<storage, read> nodes: array<FlameNode>;
@group(2) @binding(1) var disc_texture: texture_2d<f32>;
@group(2) @binding(2) var disc_sampler: sampler;
@group(2) @binding(3) var<uniform> view_from_model: mat4x4<f32>;
@group(2) @binding(4) var<uniform> clip_from_view: mat4x4<f32>;
@group(2) @binding(5) var<uniform> render_a: vec4<f32>; // focal, size, dof, opacity
@group(2) @binding(6) var<uniform> render_b: vec4<f32>; // live, gamma, brightness, clamp
@group(2) @binding(7) var<uniform> fog_color: vec4<f32>;
@group(2) @binding(8) var<uniform> fog_range: vec4<f32>; // near, far, viewport wh

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec3<f32>,
    @location(2) opacity_scalar: f32,
    @location(3) fog_factor: f32,
}

// quad_corner(corner: u32) -> Corner { pos in [-0.5, 0.5]^2, uv } — copied
// verbatim from particles/render.wgsl.

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @location(0) _local_pos: vec3<f32>,
) -> VertexOutput {
    let node_index = vertex_index / 6u;
    let corner = quad_corner(vertex_index % 6u);
    let node = nodes[node_index];

    // Ember/live prefix: nodes beyond the live count collapse to a point.
    let live = f32(node_index < u32(render_b.x));

    let view_pos = view_from_model * vec4<f32>(node.pos, 1.0);
    let dist = max(-view_pos.z, 1e-4);

    // v4 fake DoF: out-of-focus points grow and fade.
    let focal = max(render_a.x, 1e-4);
    let oof = pow(abs(dist - focal) / focal, 2.0) * render_a.z;
    // v4: gl_PointSize = size * (1 + oof) * ((viewport_h / 2) / dist), clamped.
    let size_px = min(render_b.w, render_a.y * (1.0 + oof) * (fog_range.w * 0.5) / dist);

    var clip = clip_from_view * view_pos;
    // Screen-space billboard: pixel offset scaled into NDC, pre-divide.
    let viewport = vec2<f32>(fog_range.z, fog_range.w);
    clip = vec4<f32>(
        clip.xy + corner.pos * live * size_px * 2.0 / viewport * clip.w,
        clip.zw,
    );

    var out: VertexOutput;
    out.clip_position = clip;
    out.uv = corner.uv;
    out.color = node.color;
    out.opacity_scalar = live / pow(1.0 + oof, 2.0);
    out.fog_factor = clamp((dist - fog_range.x) / (fog_range.y - fog_range.x), 0.0, 1.0);
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let sprite = textureSample(disc_texture, disc_sampler, in.uv);
    // v4 fragment order: sprite modulate -> fog mix -> alpha (opacity * DoF,
    // floored at 1/255) -> pow(rgba, gamma). Under (One, One) blending the
    // fragment multiplies its own shaped alpha in.
    var rgb = in.color * sprite.rgb;
    rgb = mix(rgb, fog_color.rgb, in.fog_factor);
    let alpha = max(render_a.w * sprite.a * in.opacity_scalar, 1.0 / 255.0);
    let gamma = render_b.y;
    let shaped_rgb = pow(max(rgb, vec3<f32>(0.0)), vec3<f32>(gamma));
    let shaped_a = pow(alpha, gamma);
    return vec4<f32>(shaped_rgb * shaped_a * render_b.z, 1.0);
}
```

Run: `cargo xtask validate-shaders` → PASS.

- [ ] **Step 5: Implement `render.rs`, spawn wiring, and the uniform driver**

`render.rs` holds the material (Interfaces block above), `default_view_matrices`, `flame_fog_color`, and `drive_flame_material`. Spawn extension in `spawn.rs` (mirroring the Line mesh idiom at `crates/wc-sketches/src/line/systems/spawn.rs:258-279` — flat `TriangleList` mesh of `total * 6` origin vertices whose data is unused):

```rust
    let vertex_count = usize::try_from(layout.total).unwrap_or(0) * 6;
    let positions: Vec<[f32; 3]> = vec![[0.0, 0.0, 0.0]; vertex_count];
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    let mesh_handle = meshes.add(mesh);

    let (view_from_model, clip_from_view) = default_view_matrices(aspect);
    let material_handle = materials.add(FlameMaterial {
        nodes: nodes_handle.clone(),
        disc_texture: asset_server.load("sketches/flame/disc.png"),
        view_from_model,
        clip_from_view,
        render_a: Vec4::new(0.782_6, 2.0, 3.0, 0.2),
        render_b: Vec4::new(layout.total as f32, 0.545, 1.0, 50.0),
        fog_color: flame_fog_color(),
        fog_range: Vec4::new(2.0, 60.0, w, h),
    });

    commands.spawn((
        FlameRoot,
        bevy::mesh::Mesh2d(mesh_handle),
        bevy::sprite_render::MeshMaterial2d(material_handle),
        Transform::default(),
        GlobalTransform::default(),
        Visibility::default(),
    ));
```

`watch_flame_name` extension: on rebuild, `meshes.get_mut(&mesh2d.0)` and replace `ATTRIBUTE_POSITION` with `new_total * 6` origin vertices (query `&Mesh2d` with `With<FlameRoot>`).

`drive_flame_material` (registered `.run_if(in_state(AppState::Flame))`): queries `&MeshMaterial2d<FlameMaterial>` on `FlameRoot`, `Res<FlameSettings>`, `Res<FlameState>`, `Single<&Window>`, and writes every uniform each frame (the camera never stops autorotating, so change-gating buys nothing here; the per-frame cost is an 8-uniform bind-group re-prepare, the same class as Cymatics' per-frame render params). Until F9 lands, matrices come from `default_view_matrices(window aspect)`; F9 swaps in `FlameCamera` matrices. `render_a.x` (focal) = camera distance (0.7826 until F9).

Register the material plugin once in `crates/wc-sketches/src/lib.rs` next to the others:

```rust
app.add_plugins(Material2dPlugin::<crate::flame::render::FlameMaterial>::default());
```

- [ ] **Step 6: Run tests** — `cargo nextest run -p wc-sketches flame` → PASS.

- [ ] **Step 7: FIRST VISUAL — smoke-test the fractal**

Run: `WAVECONDUCTOR_START_SKETCH=flame cargo rund`
Expected: the default-name fractal **blooms in from the center over ~1-2 s** (the 0.8 lerp settling from the zero seed) and then continuously morphs (the cX oscillation) — an additive glowing point cloud against `#10101f`, DoF-blurred at the edges, from a fixed v4 camera pose. Moving the mouse warps the shape (`cDx/cDy`). Typing is not wired yet; test a name change by editing "Name" in the settings dock — the fractal must rebuild and bloom into a different shape. Verify Line/Dots/Cymatics still render correctly (blend-state override must not leak — it is per-material-pipeline, but confirm visually).

If the window is backgrounded during any later capture and frames come back all-black, that is the known environment trap (`project_capture_black_when_backgrounded`), not a code bug.

- [ ] **Step 8: Gate and commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features --workspace -- -D warnings && cargo nextest run --workspace --all-features && cargo xtask validate-shaders && cargo xtask check-secrets
git add crates/wc-sketches assets/shaders/flame assets/sketches/flame
git commit -m "feat(flame): additive billboard renderer with in-material 3D projection"
```

---

## Stage 5 — Interaction (orbit camera, hands)

### Task F9: Orbit camera (`systems/camera.rs`)

**Files:**
- Create: `crates/wc-sketches/src/flame/systems/camera.rs`
- Modify: `crates/wc-sketches/src/flame/systems/mod.rs`, `flame/mod.rs` (wiring)
- Modify: `crates/wc-sketches/src/flame/render.rs` (`drive_flame_material` reads `FlameCamera`)
- Test: `#[cfg(test)]` in `camera.rs`

**Interfaces:**
- Produces:

```rust
/// CPU orbit camera around the origin. Produces the two mat4 uniforms; no
/// Camera3d entity exists (see the plan's deviation note).
#[derive(Resource, Debug, Clone, Copy)]
pub struct FlameCamera {
    /// Azimuth around +Y, radians.
    pub azimuth: f32,
    /// Polar angle from +Y, radians, clamped to (0.01, PI - 0.01).
    pub polar: f32,
    /// Orbit radius, clamped to v4's OrbitControls bounds [0.1, 8.0].
    pub distance: f32,
    /// Grab-fling momentum (azimuth, polar) in rad/frame-at-60fps (v4 kept
    /// per-frame units; applied dt-scaled: v * dt * 60).
    pub angular_velocity: Vec2,
    /// Cursor position at the previous frame while dragging.
    pub last_drag: Option<Vec2>,
}
```

  `Default` = v4 start pose: `distance = 0.782_623_8` (`(0.35^2 + 0.7^2).sqrt()`), `polar = (0.35_f32 / 0.782_623_8).acos()`, `azimuth = 0.0`, zero momentum. Methods: `pub fn eye(&self) -> Vec3` (spherical: `distance * Vec3::new(polar.sin() * azimuth.sin(), polar.cos(), polar.sin() * azimuth.cos())`); `pub fn view_from_model(&self) -> Mat4` (`Mat4::look_at_rh(self.eye(), Vec3::ZERO, Vec3::Y) * Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2)`); `pub fn clip_from_view(aspect: f32) -> Mat4`.
  - `update_flame_camera` (`Update`): autorotate `azimuth += settings.autorotate_speed * (TAU / 60.0) * dt` (v4 OrbitControls speed 1 = one orbit per minute); pointer drag while left button held (`PointerState.cursor` delta against `last_drag`): `azimuth -= dx / h * TAU`, `polar -= dy / h * TAU` (THREE uses client height for both axes); wheel zoom from `Res<AccumulatedMouseScroll>`: `distance = (distance * (1.0 - 0.1 * scroll_lines)).clamp(0.1, 8.0)`; apply decaying fling momentum when no hand grabs (F10 sets it): `azimuth -= angular_velocity.x * dt * 60.0; polar -= ...; angular_velocity *= 0.95_f32.powf(dt * 60.0)`; clamp polar. **Registered under BOTH `sketch_active(Flame)` and `in_screensaver(Flame)` gates** (autorotate is the screensaver's motion; drag/zoom input is inert there).
  - `drive_flame_material` change: matrices + focal (`render_a.x = camera.distance`... precisely `camera.eye().length()`, identical for an origin orbit) now come from `Res<FlameCamera>`.
  - v4 damping (`dampingFactor 0.05`) is NOT ported — the fling momentum plus autorotate covers the feel; recorded as an approved deviation in PARITY.md (F17).
- Consumes: `PointerState`, `ButtonInput<MouseButton>`, `bevy::input::mouse::AccumulatedMouseScroll`, `FlameSettings`, `Time`.

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Default pose = v4 camera (0, 0.35, 0.7).
    #[test]
    fn default_pose_is_v4_camera() {
        let cam = FlameCamera::default();
        let eye = cam.eye();
        assert!((eye.x - 0.0).abs() < 1e-5);
        assert!((eye.y - 0.35).abs() < 1e-4);
        assert!((eye.z - 0.7).abs() < 1e-4);
    }

    /// Autorotate at speed 1 covers TAU in 60 s of accumulated dt.
    #[test]
    fn autorotate_speed_one_is_one_orbit_per_minute() {
        let mut az = 0.0_f32;
        let dt = 1.0 / 60.0;
        for _ in 0..3600 {
            az += 1.0 * (std::f32::consts::TAU / 60.0) * dt;
        }
        assert!((az - std::f32::consts::TAU).abs() < 1e-3);
    }

    /// Zoom clamps to v4's OrbitControls bounds [0.1, 8.0].
    #[test]
    fn zoom_clamps_to_v4_bounds() {
        let mut cam = FlameCamera::default();
        for _ in 0..200 {
            cam.distance = (cam.distance * 0.9).clamp(0.1, 8.0);
        }
        assert!((cam.distance - 0.1).abs() < 1e-6);
        for _ in 0..200 {
            cam.distance = (cam.distance * 1.1).clamp(0.1, 8.0);
        }
        assert!((cam.distance - 8.0).abs() < 1e-6);
    }

    /// Momentum decays toward zero at 0.95/frame and moves the azimuth.
    #[test]
    fn fling_momentum_decays() {
        let mut cam = FlameCamera {
            angular_velocity: Vec2::new(0.02, 0.0),
            ..FlameCamera::default()
        };
        let az0 = cam.azimuth;
        let dt = 1.0 / 60.0;
        for _ in 0..240 {
            cam.azimuth -= cam.angular_velocity.x * dt * 60.0;
            cam.angular_velocity *= 0.95_f32.powf(dt * 60.0);
        }
        assert!(cam.azimuth != az0);
        assert!(cam.angular_velocity.length() < 1e-4, "momentum must decay");
    }

    /// Matrices are finite for arbitrary poses (no NaN at polar clamp edges).
    #[test]
    fn matrices_are_finite() {
        for polar in [0.011_f32, 1.0, std::f32::consts::PI - 0.011] {
            let cam = FlameCamera {
                polar,
                azimuth: 2.3,
                distance: 3.0,
                ..FlameCamera::default()
            };
            let m = cam.view_from_model();
            assert!(m.is_finite());
        }
    }
}
```

- [ ] **Step 2: Run, verify failure** → compile error.
- [ ] **Step 3: Implement** per the Interfaces block; wire in `FlamePlugin::build`:

```rust
        app.init_resource::<systems::camera::FlameCamera>();
        app.add_systems(
            Update,
            systems::camera::update_flame_camera
                .run_if(wc_core::sketch::sketch_active(AppState::Flame)),
        );
        app.add_systems(
            Update,
            systems::camera::update_flame_camera
                .run_if(wc_core::lifecycle::screensaver::in_screensaver(AppState::Flame)),
        );
```

Reset the resource to `FlameCamera::default()` in `spawn_flame` (fresh pose per entry).

- [ ] **Step 4: Tests pass** — `cargo nextest run -p wc-sketches flame` → PASS.
- [ ] **Step 5: Smoke** — `WAVECONDUCTOR_START_SKETCH=flame cargo rund`: the fractal slowly autorotates; drag orbits; wheel zooms (DoF focus follows — points sharpen at the focal distance); zooming close fattens points up to the 50 px clamp.
- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features --workspace -- -D warnings && cargo nextest run --workspace --all-features
git add crates/wc-sketches
git commit -m "feat(flame): CPU orbit camera with autorotate, drag, zoom"
```

### Task F10: Hand grab-and-fling + idle veto + hand mesh (`systems/hands.rs`)

**Files:**
- Create: `crates/wc-sketches/src/flame/systems/hands.rs`
- Modify: `crates/wc-sketches/src/flame/mod.rs` (wiring, `HandMeshPlugin`, idle veto)
- Test: `#[cfg(test)]` in `hands.rs`

**Interfaces:**
- Produces:
  - `#[derive(Resource, Default)] pub struct FlameGrabState { pub grabbing_count: usize, pub last: Vec2, pub mouse_offset: Vec2, pub warp_px: Vec2 }` — v4's `_grabbingHandCount/_lastGrabX/_grabMouseOffset*` state plus `warp_px`, the v4 `mousePosition` analogue in window pixels: the single pixel-space source the warp is derived from. F7's `update_flame_sim` is extended here to route through it — pointer writes `warp_px` when `grabbing_count == 0`, grabs write it below, and the `[-1, 1]` mapping into `FlameState.warp_input` always reads `warp_px`.
  - `pub const GRAB_THRESHOLD: f32 = 0.5;` (v4 `grabStrength > 0.5`).
  - `update_flame_hands` (`Update`, gated `sketch_active(Flame)`, `.before(update_flame_camera)`): queries `(&PalmPosition, &GrabStrength)` with `With<TrackedHand>`; converts palms to window-logical coords via `palm_to_world` + the world→window formula (`x + w/2`, `h/2 - y`, from `input/projection.rs` docs); averages grabbing hands only. First grab frame (count changed): stash `mouse_offset = grab_state.warp_px - avg`, `last = avg`, zero momentum (v4 lines 243-252). Steady grab: `delta = (avg - last) / vec2(w, h) * TAU`; `camera.azimuth -= delta.x; camera.polar -= delta.y`; `camera.angular_velocity = camera.angular_velocity * 0.7 + delta * 0.3`; `last = avg`; and `grab_state.warp_px = avg + mouse_offset` (v4 line 264: the grab drives the fractal warp like the mouse). All hands released: `grabbing_count = 0` (momentum, already stored on the camera, decays in `update_flame_camera`). Ordering: `update_flame_hands` runs before `update_flame_sim`, which maps `warp_px` → `FlameState.warp_input` and only lets the pointer overwrite `warp_px` while `grabbing_count == 0`.
  - `pub(crate) fn flame_idle_veto(world: &World) -> bool` — true while `FlameCamera.angular_velocity.length() > 1e-4` or `FlameGrabState.grabbing_count > 0` (keeps `Active` during fling decay, mirroring `dots_idle_veto`).
  - Hand-mesh overlay: register in `build()` with a warm amber matching the flame palette:

```rust
        app.add_plugins(crate::hand_mesh::HandMeshPlugin {
            config: crate::hand_mesh::HandMeshConfig {
                app_state: AppState::Flame,
                // Warm amber #ffb84d, flame-palette counterpart to Dots' ice blue.
                bone_color: Color::srgb(
                    f32::from(0xff_u8) / 255.0,
                    f32::from(0xb8_u8) / 255.0,
                    f32::from(0x4d_u8) / 255.0,
                ),
                glow_intensity: 5.0,
                bone_radius: 10.0,
            },
        });
        app.register_idle_veto(systems::hands::flame_idle_veto);
        app.init_resource::<systems::hands::FlameGrabState>();
```

- Consumes: `TrackedHand`/`PalmPosition`/`GrabStrength` (`wc_core::input::entity`), `palm_to_world` (`wc_core::input::projection`), `FlameCamera` (F9), `FlameState` (F7).

- [ ] **Step 1: Failing tests** — pure-helper tests: (a) grab-average over two synthetic palm positions with one below `GRAB_THRESHOLD` contributes only the grabbing hand; (b) the first-grab-frame branch stashes the offset and zeroes momentum; (c) the steady-grab branch produces `angular_velocity = 0.7 * old + 0.3 * delta` (extract the state-transition math into a pure `pub(crate) fn step_grab(state: &mut FlameGrabState, camera: &mut FlameCamera, avg: Option<Vec2>, grab_count: usize, window: Vec2)` and test it directly, the `step_dots_envelope` pattern); (d) `flame_idle_veto` false with zero momentum + no grab, true with either. World-level test mirroring `drive_dots_audio_raises_envelope_from_hand_alone`: spawn a `TrackedHand` + `PalmPosition` + `GrabStrength(0.9)`, run `update_flame_hands` via `run_system_once`, assert `FlameGrabState.grabbing_count == 1`.
- [ ] **Step 2: Run, verify failure.**
- [ ] **Step 3: Implement** per Interfaces (the system gathers query results into the pure `step_grab`).
- [ ] **Step 4: Tests pass** — `cargo nextest run -p wc-sketches flame` → PASS.
- [ ] **Step 5: Smoke (hardware permitting)** — with a Leap/MediaPipe provider: grab → orbit follows the hand with the amber bone overlay visible; release with motion → the camera flings and coasts; the sketch stays Active (veto) until the coast dies. Without hardware, `WAVECONDUCTOR_HAND_PROVIDER=synthetic cargo rund` at least proves the overlay + no-panic path.
- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features --workspace -- -D warnings && cargo nextest run --workspace --all-features
git add crates/wc-sketches
git commit -m "feat(flame): hand grab-and-fling orbit with idle veto and bone overlay"
```

---

## Stage 6 — Name input, TextList setting kind, carousel list

### Task F11: `SettingKind::TextList` (settings-system extension)

**Files:**
- Modify: `crates/wc-core/src/settings/def.rs` (new variant)
- Modify: `crates/wc-core/src/settings/panel_user/widgets.rs` (dispatch arm + widget)
- Modify: `crates/wc-core-macros/src/lib.rs` (Kind variant, parse arm, emit arm, doc table)
- Test: `crates/wc-core-macros/tests/derive.rs` (fixture) + widget unit test if the file has precedent

**Interfaces:**
- Produces: `SettingKind::TextList` — an editable list of short strings backed by a `Vec<String>` field; dock widget renders one text edit per entry with up/down/remove buttons and an "Add entry" button. Persistence needs NO changes (`Vec<String>` → TOML array, confirmed generic).
- The macro edit points (verified 2026-07-02): `enum Kind` at `crates/wc-core-macros/src/lib.rs:104-118`; the `ty` parse match at `:252-267` (add `"TextList" => Kind::TextList,` and extend the error string); the `kind_tokens` emit match at `:379-442` (add `Kind::TextList => quote! { ::wc_core::settings::SettingKind::TextList },`); the attribute-grammar doc table at `:31-62` (add the row; also add the missing `TemplateLibrary` entry while there). `default_kind_for_type` stays untouched — `TextList` must be requested explicitly via `ty = TextList`, like `Text`.

- [ ] **Step 1: Failing derive test** (`crates/wc-core-macros/tests/derive.rs`, modeled on the `TemplateFixture` at :176-205):

```rust
/// Fixture exercising `ty = TextList`: a Vec<String> list setting.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "derive_test_textlist")]
struct TextListFixture {
    #[setting(
        default = Vec::new(),
        ty = TextList,
        label = "Names",
        category = User
    )]
    #[serde(default)]
    names: Vec<String>,
}

#[test]
fn text_list_kind_is_emitted() {
    let defs = TextListFixture::settings_def();
    assert!(matches!(defs[0].kind, SettingKind::TextList));
    assert_eq!(defs[0].label, "Names");
}

#[test]
fn text_list_default_is_empty_and_roundtrips_toml() {
    let d = TextListFixture::default();
    assert!(d.names.is_empty());
    let with_names = TextListFixture {
        names: vec!["madison".into(), "Xiaohan".into()],
    };
    let toml_str = toml::to_string(&with_names).expect("serialize");
    let back: TextListFixture = toml::from_str(&toml_str).expect("deserialize");
    assert_eq!(back, with_names);
}
```

- [ ] **Step 2: Run, verify failure** — `cargo nextest run -p wc-core-macros` → FAIL (unknown `ty`).

- [ ] **Step 3: Implement.** `def.rs`, after `Text`:

```rust
    /// Editable list of short strings, stored as `Vec<String>`. Rendered as
    /// one single-line text edit per entry with reorder/remove buttons and an
    /// add button. Persists as a TOML array (no persistence changes needed).
    TextList,
```

`widgets.rs` dispatch arm (after `SettingKind::Text`):

```rust
        SettingKind::TextList => render_text_list(field, ui),
```

Widget (next to `render_text`, same reflection write-back idiom — mutating the downcast value fires change detection, autosave, and restart diffing identically):

```rust
/// Render an editable string-list widget: per-row edit + up/down/remove,
/// plus an add button. Mutates the reflected `Vec<String>` in place.
fn render_text_list(field: &mut dyn bevy::reflect::PartialReflect, ui: &mut egui::Ui) {
    let Some(list) = field.try_downcast_mut::<Vec<String>>() else {
        ui.label("(expected Vec<String>)");
        return;
    };
    ui.vertical(|ui| {
        let len = list.len();
        let mut remove: Option<usize> = None;
        let mut swap: Option<(usize, usize)> = None;
        for (i, item) in list.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(item).desired_width(140.0));
                if ui.small_button("up").clicked() && i > 0 {
                    swap = Some((i, i - 1));
                }
                if ui.small_button("dn").clicked() && i + 1 < len {
                    swap = Some((i, i + 1));
                }
                if ui.small_button("x").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some((a, b)) = swap {
            list.swap(a, b);
        }
        if let Some(i) = remove {
            list.remove(i);
        }
        if ui.button("Add entry").clicked() {
            list.push(String::new());
        }
    });
}
```

Macro: the three edit points listed in Interfaces, plus the doc-table row.

- [ ] **Step 4: Tests pass** — `cargo nextest run -p wc-core-macros -p wc-core` → PASS.
- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features --workspace -- -D warnings && cargo nextest run --workspace --all-features
git add crates/wc-core crates/wc-core-macros
git commit -m "feat(settings): TextList setting kind with list-editor widget"
```

### Task F12: Name-input overlay + debounced carousel admission (`ui.rs`)

**Files:**
- Create: `crates/wc-sketches/src/flame/ui.rs`
- Modify: `crates/wc-sketches/src/flame/settings.rs` (add `carousel_names`)
- Modify: `crates/wc-sketches/src/flame/mod.rs` (wiring)
- Test: `#[cfg(test)]` in `ui.rs` + settings test extension

**Interfaces:**
- Produces:
  - `FlameSettings.carousel_names: Vec<String>` — `#[setting(default = Vec::new(), ty = TextList, label = "Carousel names", section = "Screensaver", category = User)]`, `#[serde(default = "default_carousel_names")]`. Extend both settings tests (defaults-match + missing-field fallback).
  - `pub const MAX_CAROUSEL_NAMES: usize = 16;` and `pub const NAME_SETTLE_SECS: f32 = 4.0;`
  - `pub(crate) fn admit_name(list: &mut Vec<String>, candidate: &str) -> bool` — the debounced-admission core: trim; reject `< 2` chars (`chars().count()`) or `== DEFAULT_NAME`; case-insensitive dedupe (existing entry moves to front, returns `false`); otherwise insert at front, truncate to `MAX_CAROUSEL_NAMES`, return `true`.
  - `#[derive(Resource, Default)] pub struct FlameNameDebounce { pub pending: String, pub settled_at: Option<f32> }` and `debounce_name_admission` (`Update`, gated `sketch_active(Flame)`): when `settings.name != pending` → `pending = settings.name.clone(); settled_at = Some(now + NAME_SETTLE_SECS)`; when `settled_at` elapsed → `admit_name(&mut settings.carousel_names, &pending)` and clear. (The clone allocates only on an actual keystroke-driven change — event-driven, not hot-path.)
  - `flame_name_input_overlay` — registered in `bevy_egui::EguiPrimaryContextPass` (the house schedule for egui, see `crates/wc-core/src/ui/mod.rs:50-55`), self-gated: `in_state(AppState::Flame)` AND `SketchActivity::Active` (hidden in Idle/Screensaver). Draws a centered-bottom `egui::Area` (`Align2::CENTER_BOTTOM`, offset `[0.0, -64.0]`, `Order::Foreground`) with a dark translucent frame and a single `TextEdit::singleline` bound directly to `settings.name` via `bypass_change_detection`, with `.char_limit(20)` (v4 `maxLength`) and `.hint_text("who are you?")`; when `response.changed()`, call `settings.set_changed()` so autosave + the name watcher fire. Model the Area/paint scaffolding on `crates/wc-core/src/ui/reload_overlay.rs:26-63`. Exact placement/styling is an operator eye-tune item.
  - **Hotkey safety is free:** `emit_action_input` is globally gated on `egui_not_capturing_keyboard` (`crates/wc-core/src/lifecycle/mod.rs:50-51`), so typing "2" in the input cannot switch sketches. Verified manually in Step 6.
- Consumes: F11's `TextList`, `FlameSettings`, `bevy_egui::EguiContexts`, `wc_core::lifecycle::state::SketchActivity`.

- [ ] **Step 1: Failing tests** (pure `admit_name` + debounce):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::flame::branches::DEFAULT_NAME;

    #[test]
    fn admit_rejects_short_and_default() {
        let mut list = vec![];
        assert!(!admit_name(&mut list, "a"));
        assert!(!admit_name(&mut list, "  x "));
        assert!(!admit_name(&mut list, DEFAULT_NAME));
        assert!(list.is_empty());
    }

    #[test]
    fn admit_inserts_at_front_and_truncates() {
        let mut list: Vec<String> = (0..16).map(|i| format!("name{i}")).collect();
        assert!(admit_name(&mut list, "madison"));
        assert_eq!(list.len(), MAX_CAROUSEL_NAMES);
        assert_eq!(list[0], "madison");
        assert!(!list.iter().any(|n| n == "name15"), "oldest evicted");
    }

    #[test]
    fn admit_dedupes_case_insensitively_move_to_front() {
        let mut list = vec!["Xiaohan".to_string(), "madison".to_string()];
        assert!(!admit_name(&mut list, "MADISON"));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], "madison", "existing entry moved to front, original casing kept");
    }

    #[test]
    fn admit_trims_whitespace() {
        let mut list = vec![];
        assert!(admit_name(&mut list, "  ember  "));
        assert_eq!(list[0], "ember");
    }
}
```

Plus a `World`-level debounce test (the `run_system_once`-with-registered-system pattern from `dots/screensaver.rs` tests): set `settings.name = "madison"`, run `debounce_name_admission` with `Time` advanced past `NAME_SETTLE_SECS`, assert the name landed in `carousel_names`; a second run with an unchanged name admits nothing new.

- [ ] **Step 2: Run, verify failure.**
- [ ] **Step 3: Implement** per the Interfaces block. Wiring in `FlamePlugin::build`:

```rust
        app.init_resource::<ui::FlameNameDebounce>();
        app.add_systems(
            Update,
            ui::debounce_name_admission
                .run_if(wc_core::sketch::sketch_active(AppState::Flame)),
        );
        app.add_systems(
            bevy_egui::EguiPrimaryContextPass,
            ui::flame_name_input_overlay,
        );
```

(`wc-sketches` already depends on `bevy_egui` if any sketch draws egui; if not, add the workspace-internal dependency — it is in the graph, not a new dependency.)

- [ ] **Step 4: Tests pass** — `cargo nextest run -p wc-sketches flame` → PASS.
- [ ] **Step 5: Smoke** — `WAVECONDUCTOR_START_SKETCH=flame cargo rund`: the "who are you?" box floats bottom-center; typing morphs the fractal **live per keystroke** (the v4 joy — watch it reshape as letters land); pressing "2" while focused does NOT switch sketches; clicking away then pressing "2" does; after 4 s of not typing, the name appears at the top of the "Carousel names" list in the dock, where it can be edited/reordered/removed; garbage half-typed prefixes do NOT accumulate (only the settled value admits).
- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features --workspace -- -D warnings && cargo nextest run --workspace --all-features
git add crates/wc-sketches
git commit -m "feat(flame): name input overlay with debounced carousel admission"
```

---

## Stage 7 — Audio

### Task F13: `FlameSynth` + audio-thread plumbing (wc-core)

**Files:**
- Create: `crates/wc-core/src/audio/flame_synth.rs`
- Modify: `crates/wc-core/src/audio/command.rs`, `dsp.rs`, `engine.rs`, `state.rs`, `mod.rs`
- Test: `#[cfg(test)]` in `flame_synth.rs` + extended `dsp.rs` tests

**Interfaces:**
- Produces:
  - `AudioCommand::{AddFlameSynth, RemoveFlameSynth, SetFlameParam { key: &'static str, value: f32 }}` (enum stays `Copy`); `AudioMessage::{FlameSynthActivated, FlameSynthDeactivated}`.
  - `pub struct FlameSynth` with `pub fn new(sample_rate: f64) -> Self`, `pub fn set_param(&mut self, key: &'static str, value: f32)` (**`&mut`** — it carries the one-pole mapping state; `DspHost` matches with `&mut self.flame_synth`), `pub fn tick_mono(&mut self) -> f32`, `pub const KNOWN_KEYS: &'static [&'static str]`.
  - Every `AudioCommand` match site gains an arm (all verified exhaustive except `nav.rs`-adjacent sketch pushes): `dsp.rs:181` `DspHost::apply` + the `flame_synth: Option<FlameSynth>` field + the `render` mix at `:393` (`let flame_sample = self.flame_synth.as_mut().map_or(0.0, FlameSynth::tick_mono);` summed into `synth_sample`) + the `Debug` impl at `:434`; `engine.rs:193-212` echo match (`AddFlameSynth => Some(FlameSynthActivated)` etc.); `state.rs:124-162` `pump_audio_messages` + `AudioState::flame_synth_active: bool` (+ `Default`).
- **The synth graph** (fundsp expression DSL, `line_synth.rs` idioms: `shared`/`var` params, every `var` wrapped in `follow(0.016)` — the 16 ms anti-click smoother matching v4's `setTargetAtTime` tau):
  - **Chord voice** (v4 `createChord`): five oscillators — root/third/fifth sines, sub (root/2) and sub2 (root/4) triangles at gains 1.0/1.0/0.7/0.9/0.8 — each frequency a `Shared` recomputed by `recompute_chord()` from `chord_degree` + `is_major` via the ported scale math (`MAJOR = [0,2,4,5,7,9,11]`, `MINOR = [0,2,3,5,7,8,10]`, `ROOT_FREQ = 120.0`, `semi(i) = octave(i)*12 + scale[i % 7]`, `freq = 120 * 2^(semi/12)`; third at degree+3, fifth at degree+5; v4's minor/fifth biases are never driven and are not ported — PARITY note). All × `var(chord_gain)`.
  - **Noise voice**: `white() * (var(noise_gain) >> follow(0.016))` — v4's noise is UNfiltered into the compressor; the name-tuned lowpass belongs to the osc voice, not the noise.
  - **"Osc" voice — the v4 DC quirk, ported deliberately**: v4 runs a square oscillator at **0 Hz** (constant +1) at construction gain 0.6 through the name-tuned resonant lowpass; the gain *modulation* is the audible signal, and the resonant filter rings it. (The 0 Hz triangle sibling is constant 0 — silent — and is not ported.) Graph: `(dc(0.6) * (var(osc_gain) >> follow(0.016)) | (var(filter_freq) >> follow(0.016)) | (var(filter_q) >> follow(0.016))) >> lowpass()`.
  - **Post**: `mix >> shape(Shape::Tanh(1.0))` — the spec's soft limiter replacing v4's DynamicsCompressor — then `* (var(master) >> follow(0.016))`. If this fundsp version's tanh-shaper API differs, use `limiter(0.005, 0.100)` (Line's post stage) instead — ONE of the two, documented in the module doc.
  - Filter cutoff clamped `[1.0, min(18_000.0, sample_rate * 0.45)]` (the Nyquist-clamp carry-forward).
- **In-synth mapping state** (why `set_param` is `&mut`): fields `noise_gain_value/osc_gain_value/chord_gain_value` (the v4 one-pole accumulators), `noise_gain_scale`, `has_noise`, `is_major`, `chord_degree`, `density`, `chord_energy_scale`, `volume_scale`, `camera_gain`. Keys and effects:

```text
"morph_energy"    vf = min(value * noise_gain_scale, 0.06)
                  noise: has_noise ? g = g*0.5 + 0.5*(vf * (2/(1+density^2)) + 1e-5) : 0
                  osc:   g = g*0.9 + 0.1*max(0, min(value^2 * 2000, 0.6) - 0.01)
                  chord: g = g*0.9 + 0.1*(vf * chord_energy_scale + 1e-4)
                  (v4 updateFromFractalStats verbatim, with chord_energy_scale
                   standing in for count^2/8 — the envelope-primary trade)
"camera_distance" camera_gain = 1/(1+max(0,value)) + 0.5   -> recompute_master()
"volume_scale"    volume_scale = max(0, value)             -> recompute_master()
"duck_pulse"      master.set(0.0) immediately (name-change anti-click dip;
                  the next camera_distance push restores it through follow)
"filter_freq"     filter_freq.set(clamped)     "filter_q"  filter_q.set(max(0.1))
"noise_scale"     noise_gain_scale = value      "has_noise" has_noise = value > 0.5
"is_major"        is_major = value > 0.5 -> recompute_chord()
"chord_degree"    chord_degree = value    -> recompute_chord()
"density"         density = value          "chord_energy" chord_energy_scale = max(0, value)
unknown           tracing::warn! and drop (LineSynth pattern)
```

  `recompute_master()`: `master.set(camera_gain * volume_scale)`. The per-frame one-pole constants (0.5/0.9) are frame-tied exactly as v4's were (called once per render frame by the coupling system).

- [ ] **Step 1: Failing tests** (in `flame_synth.rs`):

```rust
#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    #[test]
    fn synth_ticks_finite_audio() {
        let mut synth = FlameSynth::new(48_000.0);
        // Raise the gains so the graph produces signal.
        synth.set_param("has_noise", 1.0);
        synth.set_param("noise_scale", 1.0);
        synth.set_param("density", 1.5);
        synth.set_param("camera_distance", 0.78);
        synth.set_param("volume_scale", 1.0);
        for _ in 0..60 {
            synth.set_param("morph_energy", 0.05);
        }
        let mut peak = 0.0_f32;
        for _ in 0..4800 {
            let s = synth.tick_mono();
            assert!(s.is_finite());
            peak = peak.max(s.abs());
        }
        assert!(peak > 0.0, "audible output after morph energy");
        assert!(peak <= 1.0, "tanh/limiter bounds the mix");
    }

    /// Chord frequency math golden: degree 0, major -> root 120 Hz, "third"
    /// = scale index 3 = 5 semitones -> 120 * 2^(5/12), fifth = index 5 = 9
    /// semitones -> 120 * 2^(9/12); subs at /2 and /4.
    #[test]
    fn chord_frequencies_match_v4_scale_math() {
        let f = chord_frequencies(0.0, true);
        assert!((f.root - 120.0).abs() < 1e-3);
        assert!((f.third - 120.0 * 2_f32.powf(5.0 / 12.0)).abs() < 1e-2);
        assert!((f.fifth - 120.0 * 2_f32.powf(9.0 / 12.0)).abs() < 1e-2);
        assert!((f.sub - f.root / 2.0).abs() < 1e-4);
        assert!((f.sub2 - f.root / 4.0).abs() < 1e-4);
        // Minor third: index 3 in MINOR = 5 semitones too, so probe degree 1
        // where major/minor diverge (MAJOR[4]=7 vs MINOR[4]=7 — use degree 2:
        // third index 5 -> MAJOR 9 vs MINOR 8).
        let maj = chord_frequencies(2.0, true);
        let min = chord_frequencies(2.0, false);
        assert!(maj.third > min.third);
    }

    /// The morph-energy one-poles rise monotonically under sustained energy
    /// and decay when it stops (v4's 0.5/0.9 accumulators).
    #[test]
    fn morph_energy_gains_rise_and_fall() {
        let mut synth = FlameSynth::new(48_000.0);
        synth.set_param("has_noise", 1.0);
        synth.set_param("noise_scale", 1.0);
        synth.set_param("density", 1.5);
        let mut last = 0.0;
        for _ in 0..30 {
            synth.set_param("morph_energy", 0.05);
            assert!(synth.debug_noise_gain() >= last);
            last = synth.debug_noise_gain();
        }
        let peak = last;
        for _ in 0..60 {
            synth.set_param("morph_energy", 0.0);
        }
        assert!(synth.debug_noise_gain() < peak);
    }

    #[test]
    fn unknown_key_is_dropped_without_panic() {
        let mut synth = FlameSynth::new(48_000.0);
        synth.set_param("definitely_not_a_key", 1.0);
    }
}
```

(`chord_frequencies(degree, is_major) -> ChordFreqs` is a pure helper the synth uses internally; `debug_noise_gain()` is a `#[cfg(test)]`-only accessor.)

Extend `dsp.rs` tests mirroring the existing per-synth ones: `AddFlameSynth` activates once (idempotent), `RemoveFlameSynth` drops, `SetFlameParam` on an absent synth bumps `stale_param_drops`.

- [ ] **Step 2: Run, verify failure.**
- [ ] **Step 3: Implement** — `flame_synth.rs` + the five plumbing files per Interfaces. `mod.rs`: `pub mod flame_synth;`.
- [ ] **Step 4: Tests pass** — `cargo nextest run -p wc-core audio` → PASS, including all pre-existing Line/Dots/Cymatics audio tests (the gate for shared-code stages).
- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features --workspace -- -D warnings && cargo nextest run --workspace --all-features && cargo test --doc --workspace
git add crates/wc-core
git commit -m "feat(audio): FlameSynth voice with in-synth v4 mapping curves"
```

### Task F14: Audio coupling (`audio_coupling.rs`)

**Files:**
- Create: `crates/wc-sketches/src/flame/audio_coupling.rs`
- Modify: `crates/wc-sketches/src/flame/systems/name_change.rs` (config push + duck)
- Modify: `crates/wc-sketches/src/flame/mod.rs` (wiring)
- Test: `#[cfg(test)]` in `audio_coupling.rs`

**Interfaces:**
- Produces:
  - `pub fn flame_cx_rate(elapsed_secs: f64) -> f32` — the analytic `|d(cX)/dt|`: with `u = 6·sin(t/3)`, `σ' = σ(u)(1-σ(u))`, rate `= |2·σ'·6·cos(t/3)/3|`. Replaces v4's `VelocityTrackerVisitor` as the time-driven morph source.
  - Named constants `const CX_ENERGY_WEIGHT: f32 = 0.03;` and `const WARP_ENERGY_WEIGHT: f32 = 0.01;` — normalize the two CPU sources into v4's velocity range (its `velocityFactor` clamps at 0.06); both scaled by `settings.morph_energy_scale`. Primary ear-tune surface, documented as such.
  - `#[derive(Resource, Default)] pub struct FlameMorphEnergy(pub f32)` + `pub(crate) fn step_flame_energy(env: f32, raw: f32, dt: f32, attack_rate: f32, release_rate: f32) -> f32` (the `step_dots_envelope` shape: exponential follow toward `raw`, attack when rising, release when falling, clamped `[0, 1]`).
  - `drive_flame_audio` (`Update`, registered under BOTH `sketch_active(Flame)` and `in_screensaver(Flame)` gates, `.after(update_flame_camera)`): computes `raw = (flame_cx_rate(t) * CX_ENERGY_WEIGHT + warp_speed * WARP_ENERGY_WEIGHT) * settings.morph_energy_scale` (warp_speed = `|warp_input - last| / dt`, `Local<Vec2>` for last); advances `FlameMorphEnergy` with rates from `synth_attack_ms`/`synth_release_ms`; then pushes two params (the whole per-frame ring surface):
    - `SetFlameParam { key: "morph_energy", value: env }`
    - `SetFlameParam { key: "camera_distance", value: camera.distance }`
    plus `SetFlameParam { key: "volume_scale", value: settings.synth_volume_scale * (1.0 - fade.alpha()) }` — the `ScreensaverFade` multiplier IS the smooth screensaver audio ramp (out during fade-in, back during wake fade-out; no hard mute). Envelope advanced before the `Option<NonSendMut<AudioCommandSender>>` early-return (headless-test observable), ring-full pushes warn-and-drop — the `drive_dots_audio` idioms exactly.
  - `enter_flame_audio` (`OnEnter(AppState::Flame)`): push `AddFlameSynth` + the full config (below). `exit_flame_audio` (`OnExit`): `RemoveFlameSynth`. Both `Option<NonSendMut>` early-return (headless-safe).
  - `pub(crate) fn push_flame_config(sender: &mut AudioCommandSender, audio: &NameAudioConfig, chord_energy: f32)` — pushes `filter_freq`, `filter_q`, `noise_scale`, `has_noise` (0/1), `is_major` (0/1), `chord_degree`, `density` (= `pseudo_density`), `chord_energy`. Called from `enter_flame_audio` and from `watch_flame_name` after a rebuild, preceded there by `SetFlameParam { key: "duck_pulse", value: 1.0 }` (v4's instant mute before the swap; the follow smoother turns it into a fast dip).
- Consumes: `FlameCamera` (F9), `FlameState.warp_input` (F7), `NameAudioConfig` (F1), `ScreensaverFade`, the F13 command surface.

- [ ] **Step 1: Failing tests** — (a) `flame_cx_rate` agrees with a central finite difference of `flame_cx` at 20 sample points (`|analytic - numeric| < 1e-3`); (b) rate is 0 at the oscillation's turning points (`t/3 = pi/2` → `cos = 0`); (c) `step_flame_energy` rises/decays monotonically and clamps (port the four `step_dots_envelope` test shapes); (d) `run_system_once` world test: `drive_flame_audio` without an `AudioCommandSender` still advances `FlameMorphEnergy` (the Dots headless pattern).
- [ ] **Step 2: Run, verify failure.**
- [ ] **Step 3: Implement + wire** (`init_resource::<FlameMorphEnergy>()`, the two gated registrations, `OnEnter`/`OnExit` additions to the existing chains).
- [ ] **Step 4: Tests pass.**
- [ ] **Step 5: Ear smoke** — `WAVECONDUCTOR_START_SKETCH=flame cargo rund`: near-silence when still; wiggling the mouse (warp) swells noise/osc; the slow cX oscillation breathes on its own at turning points; zooming in gets louder and closer; typing a new name dips then re-blooms with a different timbre/register (major names vs "a"-like minors). Final voicing is the operator's hardware ear-tune (checklist item; joins the pending Line audio tune).
- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features --workspace -- -D warnings && cargo nextest run --workspace --all-features
git add crates/wc-sketches
git commit -m "feat(flame): envelope-primary audio coupling on two per-frame scalars"
```

---

## Stage 8 — Attract performer (carousel + ember)

### Task F15: `FlameScreensaverPlugin` (`screensaver.rs` + ghost label)

**Files:**
- Create: `crates/wc-sketches/src/flame/screensaver.rs`
- Modify: `crates/wc-sketches/src/flame/ui.rs` (ghost seed label)
- Modify: `crates/wc-sketches/src/flame/render.rs` (`drive_flame_material` brightness lift)
- Modify: `crates/wc-sketches/src/flame/systems/sim_params.rs` (`ember_complexity` shared fn; `update_flame_sim` uses it)
- Modify: `crates/wc-sketches/src/flame/mod.rs` (add the plugin)
- Test: `#[cfg(test)]` in `screensaver.rs`

**Interfaces:**
- Produces:
  - `pub struct FlameScreensaverPlugin;` — registers `drive_flame_carousel` + `drive_flame_attract_sim` gated `in_screensaver(AppState::Flame)` (zero systems otherwise), and initializes `FlameCarousel`.
  - `#[derive(Resource, Default)] pub struct FlameCarousel { pub elapsed: f32, pub index: usize }`.
  - `pub const BUILTIN_SEEDS: &[&str] = &["Xiaohan", "wave conductor", "ember", "aurora", "who are you?"];` — the empty-list fallback. **Word choices to be reviewed with Madison before the release tag** (spec note); doc-comment says so.
  - `pub(crate) fn next_carousel_name<'a>(custom: &'a [String], builtin: &'a [&'a str], index: usize) -> (&'a str, usize)` — pure: pick list = `custom` if non-empty else `builtin`; return `(list[index % len], (index + 1) % len)`.
  - `drive_flame_carousel`: `carousel.elapsed += dt`; when `elapsed >= settings.carousel_period_secs` → reset, `(name, next) = next_carousel_name(...)`, `settings.name = name.to_string()` (allocates once per ~2 min — event-driven; the write triggers the F7 watcher rebuild + F14 config push + autosave, and because the name IS the setting, **wake adopts the carousel name for free** — the approved decision).
  - `pub(crate) fn ember_complexity(fade_alpha: f32, ember_fraction: f32) -> f32` — `1.0 - fade_alpha * (1.0 - ember_fraction)` (in `systems/sim_params.rs`): full at fade 0, `ember_fraction` at fade 1, linear between — the graceful decay AND the roar-back ride `ScreensaverFade`'s 1.5 s ramp in both directions.
  - `drive_flame_attract_sim` (`in_screensaver`): the screensaver's sim writer — advances `state.c_x` from virtual time (the fractal keeps morphing), leaves `warp_input` untouched (no pointer), sets `state.complexity = ember_complexity(fade.alpha(), settings.ember_fraction)`, calls `bake_flame_sim` (ONE baker, two writers — Condition A1). `update_flame_sim` (Active) also sets `complexity = ember_complexity(...)` so the wake roar-back completes during Active's fade-out (the Dots dual-gate lesson).
  - `drive_flame_material` change: `render_b.z = settings.master_brightness * (1.0 + fade.alpha() * (settings.attract_brightness - 1.0))` — the AgX/tonemap white-knee lift (`attract_brightness` 2.2 default), fading in/out with the envelope; and `render_b.x` follows the live prefix, so the ember visibly thins.
  - `flame_seed_ghost_label` (in `ui.rs`, `EguiPrimaryContextPass`, self-gated `Flame` + `SketchActivity::Screensaver`): a dim centered label (`egui::Area`, `Align2::CENTER_BOTTOM`, same anchor as the input it replaces) showing `normalize_name(&settings.name)` in a muted color — the fractal is always attributed; the input overlay itself stays hidden (it gates on Active).
- Consumes: `ScreensaverFade` (`fade.alpha()`), `in_screensaver`, `FlameSettings.{carousel_names, carousel_period_secs, ember_fraction, attract_brightness}`, F7's baker, F12's list.
- Power notes: the ember prefix cuts both dispatch levels and drawn quads to ~half; the framework's thermal-tier present throttle (Cool/Warm/Hot) does the rest — no custom throttling here.

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_carousel_prefers_custom_list() {
        let custom = vec!["madison".to_string(), "xiaohan".to_string()];
        let (name, next) = next_carousel_name(&custom, BUILTIN_SEEDS, 0);
        assert_eq!(name, "madison");
        assert_eq!(next, 1);
        let (name, next) = next_carousel_name(&custom, BUILTIN_SEEDS, 1);
        assert_eq!(name, "xiaohan");
        assert_eq!(next, 0, "wraps");
    }

    #[test]
    fn next_carousel_falls_back_to_builtin_when_empty() {
        let custom: Vec<String> = vec![];
        let (name, _) = next_carousel_name(&custom, BUILTIN_SEEDS, 0);
        assert_eq!(name, BUILTIN_SEEDS[0]);
    }

    #[test]
    fn next_carousel_index_out_of_range_wraps() {
        let custom = vec!["a2".to_string()];
        let (name, next) = next_carousel_name(&custom, BUILTIN_SEEDS, 99);
        assert_eq!(name, "a2");
        assert_eq!(next, 0);
    }
}
```

In `systems/sim_params.rs` tests:

```rust
    /// Ember endpoints and midpoint: fade 0 -> full, fade 1 -> ember fraction.
    #[test]
    fn ember_complexity_endpoints() {
        assert!((ember_complexity(0.0, 0.5) - 1.0).abs() < 1e-6);
        assert!((ember_complexity(1.0, 0.5) - 0.5).abs() < 1e-6);
        assert!((ember_complexity(0.5, 0.5) - 0.75).abs() < 1e-6);
        // ember_fraction 1.0 disables the decay entirely.
        assert!((ember_complexity(1.0, 1.0) - 1.0).abs() < 1e-6);
    }
```

Plus a `World`-level carousel test (registered-system pattern so `Local`/resources persist): with `carousel_period_secs = 0.1` and `Time` advanced past it, `drive_flame_carousel` writes `settings.name = "Xiaohan"` (builtin seed 0) and resets `elapsed`.

- [ ] **Step 2: Run, verify failure.**
- [ ] **Step 3: Implement + wire** (`app.add_plugins(screensaver::FlameScreensaverPlugin);` in `build()`; ghost label registered alongside the input overlay).
- [ ] **Step 4: Tests pass** — `cargo nextest run -p wc-sketches flame` → PASS.
- [ ] **Step 5: Smoke the full attract loop**

Run: `WC_DEBUG_FORCE_SCREENSAVER=1 WAVECONDUCTOR_START_SKETCH=flame cargo rund`
Expected: the fractal fades to the ember (visibly thinner, brighter-lifted, still slowly rotating and morphing), audio fades out with it, and the ghost label names the seed. With `carousel_period_secs` temporarily dropped to ~20 in the dock (ADVANCED toggle on — flip it to see Dev knobs), watch a carousel advance: the ember blooms into the next seed's shape. Move the mouse: the screensaver wakes, complexity roars back over ~1.5 s, brightness and audio ramp home, and the input box shows the adopted carousel name. Re-idle and confirm the transition is smooth both ways.

- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features --workspace -- -D warnings && cargo nextest run --workspace --all-features
git add crates/wc-sketches
git commit -m "feat(flame): name-carousel attract performer with ember decay"
```

---

## Stage 9 — Parity closure

### Task F16: Capture scenarios + `WC_DEBUG_FORCE_FLAME_WARP`

**Files:**
- Modify: `crates/wc-core/src/debug/mod.rs` (new toggle)
- Modify: `crates/wc-sketches/src/flame/systems/sim_params.rs` (consume it)
- Modify: `tests/visual/scenarios.toml` (three scenarios)
- Modify (test literals — `DebugToggles` gained a field): `crates/wc-sketches/src/line/mod.rs`, `crates/wc-sketches/src/dots/mod.rs`, `crates/wc-core/src/lifecycle/screensaver/mod.rs`, `crates/wc-core/src/capture/system.rs`

**Interfaces:**
- `DebugToggles.force_flame_warp: bool`, parsed as `present("WC_DEBUG_FORCE_FLAME_WARP")` in `from_env_vars` (`crates/wc-core/src/debug/mod.rs:76-101`), doc-commented like its siblings: "pin the Flame warp offset to a fixed (0.35, -0.2) for the `flame-warp` capture scenario". Every full `DebugToggles { .. }` struct literal in the four listed test sites gains `force_flame_warp: false`.
- `update_flame_sim` consumes it exactly like the Cymatics interaction pin (`cymatics/systems/interaction.rs:339-350`): `#[cfg(debug_assertions)]` block with `Option<Res<DebugToggles>>`, overriding `state.warp_input = Vec2::new(0.35, -0.2)` when set.

- [ ] **Step 1: Add the toggle + consumption + literal updates.** Test: extend the `from_env_vars` test coverage in `debug/mod.rs` if present (assert the var maps to the field); run the whole workspace to catch every literal site the compiler flags.

- [ ] **Step 2: Add the scenarios** (`tests/visual/scenarios.toml`, following the cymatics entries verbatim in shape):

```toml
# Flame, idle morph: default name ("who are you?"), pinned virtual clock, the
# cX oscillation + autorotate are the only motion. Early frames catch the
# bloom-in from the zero seed; 240 is the settled fractal.
[scenarios.flame-synthetic]
sketch = "flame"
provider = "synthetic"
config = "clean"
frames = [30, 60, 120, 240]
dt = 0.016666667

# Flame, warped attractor: WC_DEBUG_FORCE_FLAME_WARP pins the pointer warp to
# (0.35, -0.2), deterministically deforming the shape without hardware.
[scenarios.flame-warp]
sketch = "flame"
provider = "synthetic"
config = "clean"
frames = [60, 120, 240, 480]
dt = 0.016666667

[scenarios.flame-warp.debug]
FORCE_FLAME_WARP = "1"

# Flame attract mode: ember decay + brightness lift + ghost label. The
# carousel period (120 s) exceeds the capture span, so the seed stays the
# default name — deterministic. Wide frame spread across the fade ramp.
[scenarios.flame-screensaver]
sketch = "flame"
provider = "mock"
config = "clean"
frames = [180, 360, 600, 1200]
dt = 0.016666667

[scenarios.flame-screensaver.debug]
FORCE_SCREENSAVER = "1"
```

- [ ] **Step 3: Capture and review (agent-judged, no baselines yet)**

```bash
cargo build -p waveconductor
cargo xtask capture flame-synthetic
cargo xtask capture flame-warp
cargo xtask capture flame-screensaver
```

Read the captured PNGs and confirm: synthetic frames show the recognizable default-name fractal (compare against v4's `screenshots/flame.png` for silhouette character); warp frames show the same fractal visibly displaced/deformed; screensaver frames show the thinner, lifted ember with the ghost label. If all frames are `[0, 0, 0]`, check a known-good scenario (`cargo xtask capture dots-synthetic`) before debugging Flame — the all-black-when-backgrounded environment trap.

**Baselines are NOT seeded here** — deployment-class hardware only (operator pre-tag checklist, house pattern).

- [ ] **Step 4: Gate and commit**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features --workspace -- -D warnings && cargo nextest run --workspace --all-features && cargo xtask check-secrets
git add crates tests/visual/scenarios.toml
git commit -m "feat(flame): capture scenarios and deterministic warp toggle"
```

### Task F17: PARITY.md, roadmap correction, spec amendment, operator checklist

**Files:**
- Create: `crates/wc-sketches/src/flame/PARITY.md`
- Modify: `docs/superpowers/roadmap.md` (Flame rows)
- Modify: `crates/wc-sketches/Cargo.toml` (`description`)
- Modify: `docs/superpowers/specs/2026-07-02-flame-sketch-port-design.md` (two amendments)

- [ ] **Step 1: Write `PARITY.md`** — follow the structure of `crates/wc-sketches/src/dots/PARITY.md` / `cymatics/PARITY.md` (summary, task log F1-F17, approved deviations, known limits, operator pre-tag checklist). The **approved deviations** section must record, with rationale:
  1. GPU level-parallel IFS replaces v4's CPU recursive tree (visual math identical; f64 name→branch parity golden-tested).
  2. In-material 3D projection replaces both v4's `Camera3d` equivalent and the spec's `Camera3d` (no second window camera; `apply_render_profile`/hand-mesh/MSAA contracts untouched).
  3. Envelope/DSP audio replaces visitor-stat-driven audio: analytic `|dcX/dt|` + warp speed replace measured point velocity; hash-derived pseudo-density replaces box-counting (**fallback seam**: one-shot ~2k-point CPU evaluation + box-count at name-change only, if ear-tuning rejects the register feel); `chord_energy_scale` replaces `count^2/8`; tanh shaper (or `limiter`) replaces the DynamicsCompressor, camera-ratio folded into master gain.
  4. Instanced additive billboards + `specialize` blend override replace `THREE.Points`/`gl_PointSize`/`AdditiveBlending` (blend algebra note: `(One,One)` with in-shader alpha multiply ≡ v4's `(SrcAlpha, One)`).
  5. Single-branch chain not ported (unreachable in v4 — input substitutes the default name; `normalize_name` mirrors this).
  6. v4 float quirks ported faithfully: `cY = -2.5` for most names; `is_major` true for all but tiny names; the 0 Hz DC "osc" voice.
  7. Not ported: v4 `quality="low"` static mobile fallback; OrbitControls damping (fling momentum covers the feel); the silent 0 Hz triangle osc; chord minor/fifth biases (never driven in v4); v4's `tonemapping/colorspace` fragment includes (the HDR camera + RenderProfile own this now).
  8. v5-only additions: name carousel over the editable `carousel_names` TextList (adopt-on-wake), ember decay (level-prefix complexity), attract brightness lift, debounced name admission.
- [ ] **Step 2: Roadmap + crate description.** In `docs/superpowers/roadmap.md`: rewrite the `sketch-flame` row (line ~133) to record the shipped architecture ("GPU level-parallel IFS (supersedes the 'CPU-bound' characterization — the recursion is parallel within each level); audio = envelope/DSP approximation from CPU input scalars; PARITY record in `crates/wc-sketches/src/flame/PARITY.md`") and update the re-entry checklist note (line ~140) to say Flame re-entered the cycle on this date, Waves remains the only de-routed seam. Also line ~278's "Flame — no change (visitor stats already CPU-side)" → "Flame — shipped with envelope/DSP audio (no visitor stats)". `crates/wc-sketches/Cargo.toml` `description`: add flame to the sketch list.
- [ ] **Step 3: Spec amendments** (`docs/superpowers/specs/2026-07-02-flame-sketch-port-design.md`): in *Rendering*, replace the `Camera3d` sentence with the in-material projection design and a pointer to this plan's deviation note; in *Architecture*, replace the single-branch carve-out bullet with the unreachability finding (`normalize_name`); drop the carve-out row from *Risks* and the "sole documented exception" clause in *Performance & stability*; in *Testing*, rename the `flame-interacting` scenario mention to `flame-warp` (the shipped name).
- [ ] **Step 4: Operator pre-tag checklist** (bottom of PARITY.md, copy the dots/cymatics template shape):
  - Seed baselines on deployment-class hardware: `cargo xtask capture <scenario> --update-baselines` for the three flame scenarios, after Reading the frames.
  - AgX/tonemap eye-tune: `gamma` (0.545 start), `master_brightness`, bloom trio, `attract_brightness`, fog range, against v4 side-by-side.
  - Audio ear-tune at the hardware checkpoint (joins the pending Line hand-audio item): `morph_energy_scale`, `chord_energy_scale`, `synth_volume_scale`, attack/release, filter feel per name; verdict on the pseudo-density register (fallback seam if rejected).
  - Review `BUILTIN_SEEDS` word choices with Madison.
  - Confirm v4 HomePage display label ("Flame" assumed).
  - Hand grab-and-fling feel on real tracking hardware (thresholds, fling decay).
  - 8-hour soak (hand tracking + audio active, sketch cycling incl. Flame; watch RSS/GPU memory/FPS; overdraw check zoomed-in at the 50 px clamp).
- [ ] **Step 5: Final full gate + commit**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
git add crates docs
git commit -m "docs(flame): PARITY record, roadmap correction, operator checklist"
```

---

## Execution notes

- **Sequencing:** strictly F1 → F17; F11 (TextList) has no dependency on F5-F10 and may run in parallel with Stage 3-5 work if using subagents — everything else is ordered.
- **Parallel-agent rule (Madison's standing preference):** parallel implementers edit disjoint files and do NOT run concurrent cargo builds — batch one build/test pass after a wave. No worktrees (disk).
- **Manual-test prompts for Madison** land at F8 Step 7, F12 Step 5, F14 Step 5, F15 Step 5 — each is a `cargo rund` moment worth a look at the real thing.
- **Deferred to the operator** (never claim them done in-plan): baseline seeding, AgX/audio tuning, hand-feel pass, seed-word review, 8-hour soak.

