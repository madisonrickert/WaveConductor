# WaveConductor v5 — Flame sketch port

**Status:** Draft for review, 2026-07-02. Scope, GPU sim architecture, attract concept
(name carousel + ember decay), name-input UX, and the simplified audio design were
approved in-session; the hash-derived chord register (see *Audio*) was adopted per the
"simplify/optimize audio" direction with a documented fallback seam. Spec awaits
Madison's review before the implementation plan is written.

**Parity target:** Perceptual, against v4 `src/sketches/flame/` (`index.tsx`,
`superPoint.ts`, `transforms.ts`, `branch.ts`, `updateVisitor.ts`,
`flamePointsMaterial.ts`, `flamePoints.vert.frag`, `flamePoints.frag`, `audio.ts`,
and `screenshots/`). The v4 tree is checked out at `.worktrees/v4/`.

**Internal name** `Flame` (`AppState::Flame` already exists; de-routed from live input
by the 2026-07 audit T5). User-facing manifest name is **"Flame"** unless v4's
HomePage used a different display label (confirm during implementation; Line ships
internally `Line` / displays "Gravity", Dots / "Fabric").

## Goal

Port v4's fractal-flame sketch to v5 at perceptual parity, on the patterns
established by the Line, Dots, and Cymatics ports, carrying the v5-era upgrades:
GPU-first computation, a deeper settings surface, a screensaver/attract performer
(v4 had none), and the GPU-only data-flow discipline (graphics stay on the GPU; the
audio thread is fed only CPU-side scalars, never a GPU readback).

Flame is an IFS (Iterated Function System) fractal: a name typed by the visitor
("who are you?") is hashed into a set of 1–7 branches (affine transform + nonlinear
variation + additive color), and ~100k points are evaluated through the branch tree
every frame, drawn as an additive point cloud with a fake depth-of-field. In v4 the
entire evaluation ran on the CPU in TypeScript and was a known responsiveness pain.
Flame is also **v5's first sketch with a 3D perspective look** (orbit/autorotate,
fog) — achieved by in-material projection into the existing 2D camera, with no
`Camera3d` (see Rendering).

## Scope

**In scope (perceptual-parity target):**

- The IFS evaluation: v4's 7 affine maps and 8 variations (Linear, Sin, Spherical,
  Polar, Swirl, Normalize, Shrink, plus the interpolated/router combinators), the
  per-frame lerp animation (position 0.8, color 0.75), escape pullback
  (`length_sq > 2500` → Spherical), point budget ~100k (capacity 200k), tree depth
  `floor(ln(100_000) / ln(branch_count))`, `jumpiness = 3` seeding.
- Name-seeded generation ported verbatim: the deterministic PRNG
  (`gen = (gen * 4194303 + 127) % (2^31 - 1)`) and hash mappings, so the same name
  produces the same fractal as v4.
- The attractor drivers: time oscillation `cX = 2·sigmoid(6·sin(t)) - 1`
  (`t` = virtual-time seconds / 3), name-hash `cY` mapped to −2.5..2.5,
  pointer/hand warp offsets `cDx`/`cDy` mapped from screen position to −1..1.
- Rendering: additive point cloud, disc sprite, fake depth-of-field point sizing
  (`out_of_focus = (|dist − focal| / focal)² · 3`, size clamp, opacity falloff,
  min-alpha floor 1/255), fog to background `#10101f` (near 2, far 60), v4 gamma
  `pow(color, 0.545)` as the starting point for AgX-era tuning.
- Camera: perspective FOV 60, start `z = 0.7, y = 0.35`, orbit + wheel zoom
  (distance clamp 0.1–8), autorotate speed 1 with damping, hand grab-and-fling
  (grab threshold 0.5, velocity smoothing `v = 0.7v + 0.3Δ`, release momentum
  decay 0.95/frame), grab position feeding the warp offset like the pointer.
- The name input: centered overlay, "who are you?" placeholder, 20-char max,
  live rebuild on change (no restart), brief audio duck during rebuild.
- Generative audio at perceptual parity via the simplified design (see *Audio*).
- Bloomed wireframe hand-mesh overlay via the shared `hand_mesh` module.
- Manifest tile + full re-entry (see *Re-entry punch list*).

**In scope (v5 upgrades):**

- **GPU level-parallel IFS** — the headline architecture change (see next section).
- **Attract performer**: name carousel over remembered names + ember complexity
  decay, with smooth enter/exit.
- **Editable carousel name list** as a first-class setting (new list-of-text
  setting kind).
- **Dev settings surface** (point budget, camera, DoF, fog, attract knobs,
  `RenderProfile`).
- Capture scenarios + determinism toggles for the visual-regression harness.

**Out of scope:**

- Waves (its seams stay de-routed; nothing here builds for it — YAGNI).
- v4's `quality = "low"` small-screen static fallback (no mobile target in v5).
- Pixel-exact recreation of the React/SCSS overlay styling (egui approximation
  keeps the spirit: dark translucent box, centered).
- v4's Leap overlay hand rendering (replaced by shared `hand_mesh`).
- WebGL2/CPU render fallback (WebGPU-only target).

## Architecture: GPU level-parallel IFS

**Decision:** the IFS runs entirely on the GPU. This supersedes the roadmap's
"CPU-bound (no GPU parallelism)" characterization of Flame, which described v4's
recursive-tree *formulation*, not the algorithm: every node's new state depends only
on its parent's new state this frame, so the tree is embarrassingly parallel within
each level. Update `docs/superpowers/roadmap.md` (Flame character line + the
"visitor stats already CPU-side" note) as part of the port.

- **State**: one persistent storage buffer of node states (`pos: vec3f`,
  `color: vec3f`, padded; 32 B/node × 200k capacity ≈ 6.4 MB), **level-ordered**.
  Within a level, nodes are ordered **branch-major** (all branch-0 children of the
  level contiguous, then branch-1, …) so warps share the same variation `switch`
  arm and parent lookup stays pure arithmetic on precomputed level/branch offsets.
- **Per frame**: one compute dispatch per tree level, sequential (≈ 6–17 levels for
  2–7 branches), each computing
  `state[i] = lerp(state[i], apply_branch(state[parent(i)]), 0.8)` (color 0.75)
  plus the escape pullback. Per-level parameters (level offset, parent offset,
  counts) ride a dynamic-offset uniform array at 256-byte stride — the Cymatics
  multi-dispatch pattern reused verbatim. Cymatics already runs up to 120
  sequential dispatches per frame; Flame's ≤ 17 are well inside precedent.
- **Per-frame CPU→GPU traffic**: one small uniform (`cX`, `cY`, `cDx`, `cDy`,
  lerp factors, focal length, live node count). No per-frame CPU simulation, no
  per-frame buffer upload.
- **Name change** (rare, CPU-side): rebuild the branch set from the hash-seeded
  PRNG, write the branch-params uniform (max 7 branches: affine, variation id(s),
  color), recompute level offsets, re-seed node states so the new shape blooms in
  through the lerp — matching v4's rebuild-then-settle behavior. Scratch buffers
  reused across rebuilds (no steady-state allocation).
- **Single-branch case is unreachable** (approved deviation, found during
  planning; see the plan's "Approved deviations" note). v4's name input
  substitutes the default name for empty input, so `numBranches = ceil(1 +
  len%5 + wraps)` is always 2–8; the 1,000-node sequential *chain* in v4's
  `computeDepth` is dead code. v5 mirrors the trim-or-default normalization
  (`normalize_name`) and asserts `branch_count >= 2`, so every fractal is
  GPU-parallel with no CPU-chain fallback.
- **Simulation shaders** in `assets/shaders/flame/simulate.wgsl` (never inline);
  the 8 variations port as a WGSL `switch` on variation id; the Rust transform
  mirror (used by the golden/parity tests) and the WGSL kernel **change together
  term-for-term** (Dots kernel-parity discipline).
- **Lifecycle discipline**: dispatches gated on sketch activity (zero dispatches in
  `Idle` and after exit); all GPU resources owned by entities under a `FlameRoot`
  marker, despawned `OnExit`; resource removal mirrored into the render world by a
  manual `ExtractSchedule` companion system (the known `ExtractResourcePlugin`
  no-removal-propagation landmine).

## Rendering — 3D projection without a 3D camera

- **No `Camera3d`** (approved deviation; see the plan's deviation note). The app
  runs exactly one window camera — the global HDR `Camera2d` — and
  `apply_render_profile`, the hand-mesh composite, and the shared-MSAA contract
  all assume it; a second window camera would re-open the shared-MSAA-texture
  landmine. So the orbit camera is a CPU `FlameCamera` resource passed to a
  custom `Material2d` as two `mat4` uniforms (view-from-model, clip-from-view),
  and the vertex shader does the perspective projection + billboarding
  in-material, drawing into the existing `Camera2d`. Clear color `#10101f`; the
  house tonemapping/bloom stack and per-sketch `RenderProfile` are untouched
  (v4's baked `pow(0.545)` gamma is the profile's starting gamma; final look is
  eye-tuned by the operator under AgX, as with every prior port).
- Points draw as **instanced camera-facing quads** whose vertex shader reads the
  sim storage buffer directly at the instance index — no CPU round-trip. Vertex:
  fake-DoF sizing + opacity falloff (v4 formulas). Fragment: disc sprite sample
  (copy `disc.png` from v4), additive blending, depth test off, distance fog
  toward the background color, min-alpha floor. Shader:
  `assets/shaders/flame/render.wgsl`.
- **Instance count = live node count.** Because the buffer is level-ordered, a
  level-prefix count is a smooth complexity knob — this is the attract-mode ember
  mechanism and the point-budget control.
- **Hand-mesh composite is unaffected**: because there is no second camera (see
  above), the shared hand-mesh composite keeps running against the single
  `Camera2d` exactly as it does for every other sketch. The original spec's
  "compositing into a `Camera3d` HDR target" early-verification risk does not
  arise under the in-material approach.

## Interaction

- **Orbit camera** written in-sketch (azimuth/polar/distance + damping + autorotate;
  no new dependency): pointer drag orbits, wheel zooms (0.1–8), autorotate speed 1.
- **Warp**: pointer position (via `PointerState`) maps to `cDx`/`cDy`, shifting the
  fractal attractor live, as in v4.
- **Hands** (`TrackedHand` entities): `GrabStrength > 0.5` engages grab-and-fling —
  averaged grab position drives azimuth/polar deltas, smoothed angular velocity
  applies as decaying momentum on release (v4 constants ported). Grab position also
  feeds the warp offset.
- **Idle veto** holds `Active` while fling momentum decays (Dots pattern).
- **Keybinding suppression**: while the name input has egui keyboard focus, the
  action map must not fire (typing "2" in a name must not switch sketches). Gate on
  egui's keyboard-capture state.

## Name input, write-through, and the carousel list

- Centered egui overlay, v4-styled, visible while `Active`; writes through to a
  persisted User-category `name` Text setting. Name changes are **live** (rebuild
  in place; no `SketchRestart`), with a brief anti-click audio duck.
- **Name history / carousel list**: a bounded (~16 entries) list of names, shown and
  **editable in the settings dock** — view, delete, add manually, reorder, clear.
  This needs a new list-of-text setting kind (`SettingKind` gains a `TextList`
  variant + a panel widget + derive-macro support), added the same way
  `TemplateLibrary` and `FilePath` were: a small, reusable extension of the
  settings system, persisted like any other settings field.
- **Debounced auto-admission** from the input (the "no half-typed garbage" rule): a
  name is admitted only once *settled* — ≥ 4 s since the last keystroke, ≥ 2 chars,
  not the "who are you?" default, case-insensitive dedupe (move-to-front), evict
  oldest past the bound.

## Attract performer — name carousel + ember decay

`FlameScreensaverPlugin`, gated `in_screensaver(AppState::Flame)`, driving the real
sketch pipeline (house Approach A — never a separate renderer). Smooth transitions
in and out are a first-class requirement; everything below rides the framework's
`ScreensaverFade` envelope rather than switching discretely.

- **Enter**: complexity ramps down to the ember fraction (default ~50% of points,
  a level-prefix instance/dispatch count), brightness lifts past the AgX white knee
  (shared `attract_color_params` pattern, ≈ 2.2 like Dots), synth volume ramps out
  with the fade — no hard mute cut.
- **Dwell**: slow autorotate continues as the motion (no extra choreography — v4's
  time-oscillation morph plus rotation already carries it). Every carousel period
  (default ~2 min, setting) the performer advances to the next name from the
  carousel list — falling back to a small curated built-in seed list while the
  list is empty (word choices picked with Madison during implementation) — through
  the same live name-change path, so each shape blooms in.
- **UI reflection**: the input overlay is replaced by a dim ghost label naming the
  current seed, so the displayed fractal is always attributed.
- **Wake**: the **carousel name is adopted** as the active name (it populates the
  input; the pre-screensaver name remains in the list) — approved decision; this
  makes wake a pure roar-back (complexity ramps to 100% over ~1–2 s, brightness
  and audio ramp home) with no shape snap-back.
- Power saving beyond the ember fraction comes from the framework's thermal-tier
  present-rate throttle (Cool/Warm/Hot), as for all sketches.

## Audio — simplified envelope/DSP approximation

No per-frame stats over the point cloud, no GPU readback, no CPU shadow tree.
`FlameSynth` lives on the audio thread (modeled on the Line/Cymatics synths,
reusing existing oscillator/noise/filter DSP blocks; commands via the lock-free
ring: `AddFlameSynth` / `RemoveFlameSynth` / `SetFlameParam`).

**Voices** (all pre-allocated at init): white noise → lowpass (name-tuned cutoff
120–400 Hz, Q 5–8), one oscillator, a 5-sine chord (root 120 Hz, major/minor by
name hash), a `tanh`-style soft limiter, master gain. The limiter **replaces**
v4's DynamicsCompressor (ratio-1.8 glue + camera-proximity intimacy): the glue is
inaudible at these settings, and the camera modulation folds into the master-gain
curve.

**Per-frame cross-thread surface: two scalars.**

1. **Morph-energy** — a smoothed envelope over the CPU-known attractor inputs:
   analytic `|d(cX)/dt|` (closed form), warp deltas, and the natural spike from a
   name-change re-seed (no dedicated transient path; the anti-click duck covers the
   swap). Replaces v4's `VelocityTrackerVisitor`.
2. **Camera distance** — master gain `1/(1+dist) + 0.5` and the folded intimacy
   curve.

All of v4's mapping curves (velocity-factor clamps, the 0.9/0.1 and 0.5/0.5
one-pole smoothings, per-voice gain laws) move into the synth and operate on those
two inputs, so voicing lives in one place.

**Per name-change: one config message** — filter cutoff/Q, major/minor, has-noise
(hash-gated, ~50% of names), noise-gain scale, and the **chord register** from a
**hash-derived pseudo-density** (branch count + variation mix + affine contraction,
computed from the branch set already built at name-change). This replaces v4's
box-count density and is the port's largest parity deviation. **Fallback seam** (if
ear-tuning says the register feels mismatched): a one-shot ~2k-point CPU evaluation
+ box-count at name-change only — contained, never per-frame. Record both in
PARITY.md.

Final voicing is ear-tuned by Madison at a hardware checkpoint (joins the existing
pending audio-tuning item). Audio-thread rules apply: no allocation after init, no
locks, cutoffs clamped below Nyquist, phase clocks wrapped for multi-hour soaks.

## Settings surface (initial)

| Setting | Category | Kind | Notes |
|---|---|---|---|
| Name | User | Text | Live rebuild; write-through from overlay |
| Carousel names | User | TextList (new) | Editable list; auto-admission from input |
| Point budget | Dev | Number | Target total points, default 100k; restart |
| Autorotate speed | Dev | Number | Default 1; live |
| DoF strength | Dev | Number | Default 3; live |
| Base point size / opacity | Dev | Number | v4 defaults (2 / 0.2); live |
| Fog near / far | Dev | Number | Defaults 2 / 60; live |
| Carousel period | Dev | Number | Default 120 s; live |
| Ember fraction | Dev | Number | 0.4–0.6, default 0.5; live |
| Attract brightness | Dev | Number | Default ≈ 2.2; live |
| RenderProfile knobs | Dev | — | Tonemapping, gamma (init 0.545-equivalent), master brightness, bloom |

Every field carries the `#[serde(default = "...")]` free function per house
convention; exact live-vs-restart classification is finalized per-field in the plan.

## Re-entry punch list

Exactly the audit T5 reversal, plus the roadmap correction:

1. Add `Flame` to `SKETCH_ORDER`; split it out of the grouped `next_sketch` /
   `prev_sketch` arms into real cycle routing.
2. `"flame"` resolves in `AppState::from_name` (re-enables
   `WAVECONDUCTOR_START_SKETCH=flame`, which capture needs).
3. Reintroduce a `SelectFlame` action + `Digit2` binding.
4. Register `FlamePlugin` in `SketchesPlugin`; register the manifest tile with
   `assets/sketches/flame/screenshot.png` (scrubbed).
5. Add `Flame` to `KNOWN_IMPLEMENTED_SKETCHES`; update the tripwire tests
   (`flame_and_waves_arms_are_present_but_unreachable_from_the_cycle` becomes
   Waves-only; the manifest-registration test now covers Flame).
6. Revise `docs/superpowers/roadmap.md`'s Flame lines (CPU-bound characterization,
   audio-visitor note, re-entry checklist item) and the `wc-sketches` crate
   description ("line, dots, cymatics" → include flame).

## Testing & verification

- **Unit (CPU)**: name-hash PRNG + branch generation golden-tested against v4
  values (port the meaningful assertions from v4's vitest suites for
  `transforms`, `branch`, `superPoint`); level/branch offset arithmetic and
  parent-index math; prefix-count (ember) math; history debounce/dedupe rules;
  carousel advance; morph-energy envelope; pseudo-density mapping; settings serde
  defaults.
- **Kernel parity**: the Rust transform mirror (needed anyway for the
  golden/parity tests) is the reference for the WGSL kernel under the
  change-together discipline; golden numbers come from v4.
- **Shaders**: `cargo xtask validate-shaders` covers the two new WGSL files.
- **Capture scenarios** (`tests/visual/scenarios.toml`): `flame-synthetic` (clean
  config → default name → deterministic under the pinned virtual clock),
  `flame-warp` (the `WC_DEBUG_FORCE_FLAME_WARP` toggle pins the warp offset to a
  fixed `(0.35, -0.2)`, deforming the attractor deterministically),
  `flame-screensaver` (`FORCE_SCREENSAVER`, carousel period exceeds the capture
  span so the seed stays the default name). New toggles join `DebugToggles`
  (debug-assertions-gated, absent from release).
- **Deferred to the operator pre-tag checklist** (house pattern; template at the
  bottom of `dots/PARITY.md` / `cymatics/PARITY.md`): baseline seeding on
  deployment-class hardware, AgX/gamma + palette eye-tune, audio ear-tune, the
  8-hour soak.
- **PARITY.md** at closure records the approved deviations: GPU formulation,
  in-material projection (no `Camera3d`), envelope audio + pseudo-density (+
  fallback seam), tanh limiter, instanced billboards for point sprites, the
  not-ported single-branch chain (unreachable in v4), dropped `quality="low"`
  fallback, and the v5-only additions (carousel, ember, editable list).

## Performance & stability constraints

Beyond the repo-wide rules (AGENTS.md), the Flame-specific commitments:

- Zero per-frame CPU simulation (every fractal is GPU-parallel; the single-branch
  chain is unreachable, not a fallback); per-frame CPU work is uniform assembly,
  envelope math, camera integration, and egui overlay only.
- No steady-state allocation anywhere: name-change rebuild uses pre-allocated
  scratch; persistent GPU buffers with `queue.write_buffer`; bind groups cached
  and invalidated on resize.
- Zero dispatches in `Idle`/after exit; instance count and dispatch level count
  both honor the live/ember node count (no dead workgroups).
- Additive overdraw bounded by the ported point-size clamp and opacity 0.2; the
  frame limiter (default 60) provides saturation headroom per the Dots runbook —
  watch this during the soak, not with shader micro-opts.
- Audio thread: ring-only communication, pre-allocated voices, wrapped phase.

## Risks

| Risk | Mitigation |
|---|---|
| Hand-mesh composite on a second camera | Resolved: no `Camera3d`; in-material projection draws into the existing `Camera2d`, composite unchanged |
| Hash-derived chord register misses v4's density feel | Documented one-shot box-count fallback, name-change-only |
| Additive overdraw when zoomed close | Ported size clamp + frame-limiter headroom; soak watch |
| GPU float behavior across ≤ 17 lerp'd levels | Lerp is contractive/stable; capture tolerance-diff covers |
| Keybindings firing while typing a name | egui keyboard-capture gate; unit-testable |
| Warp divergence in the variation `switch` | Branch-major level ordering keeps warps coherent |
| Carousel-adopted name surprises the owner | Approved behavior; the prior name stays in the list |

## Plan staging (input to writing-plans)

1. **Core math** — Rust transforms/PRNG/branch builder + golden tests vs v4;
   level/offset arithmetic. Pure CPU, no Bevy.
2. **Lifecycle scaffold + re-entry** — stub `FlamePlugin` (clear-color swap, no
   3D camera), manifest, `SKETCH_ORDER`/bindings/`from_name`, tripwire tests
   updated; `WAVECONDUCTOR_START_SKETCH=flame` works for all later stages.
3. **Compute pipeline** — storage buffer, per-level dispatch node, seeding,
   uniforms, `simulate.wgsl`.
4. **Render** — billboard material + `render.wgsl` (in-material projection, DoF,
   disc, fog, additive), `RenderProfile`; hand-mesh composite unchanged.
5. **Interaction** — orbit camera, warp, grab-fling, idle veto.
6. **Name input + settings depth** — overlay, write-through, keybinding gate,
   `TextList` setting kind, history admission.
7. **Audio** — `FlameSynth`, two-scalar coupling, name-change config.
8. **Attract performer** — carousel + ember decay + fade + ghost label.
9. **Parity closure** — capture scenarios + determinism toggles, PARITY.md,
   roadmap edits, operator pre-tag checklist.

Each stage lands behind the full CI gate set (fmt, clippy `-D warnings`, nextest +
doctests, doc build, deny, check-secrets) with TDD per task, per the house plan
template (`docs/superpowers/plans/2026-06-24-cymatics-sketch-port.md`).
