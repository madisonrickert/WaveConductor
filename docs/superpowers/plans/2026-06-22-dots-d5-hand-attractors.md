# Dots D5 — Leap/MediaPipe hand attractors — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** Let tracked hands (Leap or MediaPipe) act as gravity wells in Dots, alongside the mouse — each grabbing hand pulls the grid, and hand activity also drives the synth.

**Architecture:** Dots gets a `DotsHandAttractor` component attached to every `TrackedHand` while Dots is active (mirroring `LineHandAttractor`), updated each frame by v4's `computeLeapAttractorPower` continuous-power curve with the v4 **Dots** config. The hand attractors are appended to the shared `SimParams` attractor array after the mouse (slot 0), so the same compute kernel pulls the grid toward every active well. Hand grab activity also feeds the D4 audio activity envelope. The Leap/MediaPipe provider, the `TrackedHand` entity model, and `PalmPosition`/`GrabStrength`/`CameraDistance` components already exist in `wc-core` (Line consumes them) — Dots reuses them; only the per-sketch attractor component + wiring is new.

**Tech Stack:** Rust, Bevy 0.19 ECS (the `TrackedHand` entity model), the shared `crate::particles` engine, the existing hand-tracking provider in `wc-core::input`.

## Global Constraints

- **Reuse the existing hand infra:** `wc_core::input::entity::{TrackedHand, PalmPosition, GrabStrength, CameraDistance}` and the provider that populates them. Do NOT re-implement hand tracking — only add the Dots-side attractor component + systems.
- **v4 Dots leap config (from `.worktrees/v4/src/sketches/dots/index.ts` `LEAP_POWER_CONFIG`), verbatim:** `attackSpeed = 0.005`, `decaySpeed = 0.5`, `grabThreshold = 0.1`, `powerFloor = 0.05`.
- **v4 `computeLeapAttractorPower` curve (from `.worktrees/v4/src/particles/leapAttractorPower.ts`), exact:**
  - if `grab <= grabThreshold`: `decayed = power * decaySpeed`; if `decayed < powerFloor` → `0`, else `decayed`.
  - else: `wanted = grab^1.5 × 5^((-palmZ + 350) / 160)`; `power = power×(1-attackSpeed) + wanted×attackSpeed`.
  - (Dots has a `powerFloor`; Line dropped it. Otherwise identical to Line's `update_line_hand_attractors`.)
- **Self-contained:** Dots' hand code lives under `crates/wc-sketches/src/dots/`; copy the small pure curve + the `palm_to_world` projection from Line (they're Line-private). **Carry-forward:** v4 shared `computeLeapAttractorPower`; Line + Dots now duplicate it — a later cleanup can extract it to the shared `particles/` foundation. Note this in the code.
- **Feed hands into the SAME attractor array** the mouse uses: mouse at slot 0 (when active), hands at slots `1..MAX_ATTRACTORS`, each with `power × DOTS_GRAVITY_CONSTANT` baked in and near-zero powers skipped — mirror `line/systems/sim_params.rs`'s hand-append loop exactly so the shared kernel's force math is identical.
- **No `unwrap()`/`expect()`** in non-test code unless documented; **no `as` casts** where `TryFrom` works (the `slot`/`attractor_count` casts may reuse Line's `#[allow]` blocks); **`///`/`//!` docs**; **no per-frame allocation** (the sim_params writer already uses a fixed `[Attractor; MAX_ATTRACTORS]` stack buffer — keep it).
- **Hardware verification is the operator's.** Real Leap/MediaPipe behavior (does a grab pull the grid, does proximity scale force) needs hardware via `cargo rund` — NOT auto-verifiable. Unit-test the curve + the attractor-array assembly with synthetic `TrackedHand` entities + known `PalmPosition`/`GrabStrength`; flag the hardware feel as operator-deferred.
- **Verification gates:** fmt; clippy `--all-targets --all-features --workspace -D warnings`; nextest `--workspace --all-features` + `cargo test --doc`; `cargo doc`; `cargo xtask check-secrets`. Do NOT run `cargo rund`.
- **Commit messages:** `git commit -F` (no backticks).

## Reference material (read these)

- v4: `.worktrees/v4/src/sketches/dots/index.ts` (the `onFrame` hand loop, `LEAP_POWER_CONFIG`, `getLeapAttractor`) + `.worktrees/v4/src/particles/leapAttractorPower.ts` (the curve).
- `crates/wc-sketches/src/line/leap_attractors.rs` — `LineHandAttractor`, `LineLeapAttractorsPlugin` (attach `reconcile`/detach systems gated on `AppState::Line`), `update_line_hand_attractors` (the curve), `palm_to_world` (the projection). Mirror structure; swap config + add the Dots floor.
- `crates/wc-sketches/src/line/systems/sim_params.rs` lines ~300-360 — the mouse-at-0 + hand-append-after loop (`for hand_attractor in &line_hands { ... attractors[slot] = ...; slot += 1; attractor_count += 1; }`, skipping `power.abs() <= 1e-2`). Mirror for Dots.
- `crates/wc-sketches/src/dots/systems/sim_params.rs` (`update_dots_sim_params` — where the hand attractors get appended) + `crates/wc-sketches/src/dots/audio_coupling.rs` (the D4 activity envelope to extend).

---

### Task 1: `DotsHandAttractor` + Dots leap plugin + feed hands into sim params

**Files:**
- Create: `crates/wc-sketches/src/dots/hand_attractors.rs` (`DotsHandAttractor`, the curve, `palm_to_world`, `DotsLeapAttractorsPlugin`)
- Modify: `crates/wc-sketches/src/dots/systems/sim_params.rs` (append the hand attractors after the mouse)
- Modify: `crates/wc-sketches/src/dots/mod.rs` (`pub mod hand_attractors;`; add `DotsLeapAttractorsPlugin`)

**Interfaces:**
- Produces: `crate::dots::hand_attractors::DotsHandAttractor { power: f32, position: Vec2 }` (Component, Reflect like Line's), `DotsLeapAttractorsPlugin`, a `pub(crate) fn dots_leap_power(power, grab, palm_z) -> f32` (the curve with Dots config baked), `pub(crate) fn dots_palm_to_world(palm, window_size) -> Vec2`.
- Consumes: `wc_core::input::entity::{TrackedHand, PalmPosition, GrabStrength}`, the shared `Attractor`/`SimParams`.

- [ ] **Step 1: The curve + projection (pure functions)**

In `hand_attractors.rs`, write `dots_leap_power(power: f32, grab: f32, palm_z: f32) -> f32` implementing v4's `computeLeapAttractorPower` with the Dots config baked as named consts (`DOTS_HAND_ATTACK_SPEED=0.005`, `DOTS_HAND_DECAY_SPEED=0.5`, `DOTS_HAND_GRAB_THRESHOLD=0.1`, `DOTS_HAND_POWER_FLOOR=0.05`): the decay branch zeroes below the floor; the grab branch uses the `grab^1.5 × 5^((-z+350)/160)` EMA. Copy `palm_to_world` from `line/leap_attractors.rs` as `dots_palm_to_world` (identical projection). Add the carry-forward comment that v4 shared this curve and Line/Dots now duplicate it.

- [ ] **Step 2: The component + plugin (attach/detach/update)**

`DotsHandAttractor { power, position }` (mirror `LineHandAttractor`). `DotsLeapAttractorsPlugin`: while `AppState::Dots`, a reconcile system inserts `DotsHandAttractor::default()` on every `TrackedHand` `Without<DotsHandAttractor>` (mirror `reconcile_line_attractors` — timing-independent, idempotent); `OnExit(AppState::Dots)` removes `DotsHandAttractor` from all (mirror `detach_all_line_attractors`); a per-frame `update_dots_hand_attractors` (gated `sketch_active(Dots)`) sets each `position = dots_palm_to_world(palm, window)` and `power = dots_leap_power(power, grab.0, palm.z)`. Register the systems (read how `LineLeapAttractorsPlugin` orders + gates them).

- [ ] **Step 3: Append hands into `update_dots_sim_params`**

In `dots/systems/sim_params.rs`, add a `Query<&DotsHandAttractor, With<TrackedHand>>` param and, after writing the mouse at slot 0, append each hand attractor with `power.abs() > 1e-2` to `attractors[slot]` with `power: hand.power × DOTS_GRAVITY_CONSTANT`, `position: hand.position.into()`, incrementing `slot`/`attractor_count`, capped at `MAX_ATTRACTORS` — mirror `line/systems/sim_params.rs`'s loop exactly (same threshold, same gravity bake, same cap). Keep the fixed stack buffer (no allocation).

- [ ] **Step 4: Tests**

`dots_leap_power` unit tests (the highest-value, fully-deterministic part): grab below threshold decays by 0.5 and zeroes below 0.05 (e.g. `power=0.08 → 0.04 → 0`); grab above threshold EMAs toward `grab^1.5 × 5^((-z+350)/160)` (assert one concrete value, e.g. from the v4 test `leapAttractorPower.test.ts` if it has Dots cases). `update_dots_sim_params` with synthetic `TrackedHand` entities carrying `DotsHandAttractor { power, position }`: assert the array has the mouse at 0 (if active) + the hands appended with `power × 100` baked, `attractor_count` correct, near-zero hands skipped. Mirror the synthetic-`TrackedHand` test setup Line uses (read `crates/wc-sketches/tests/` for the hand-entity harness).

- [ ] **Step 5: Build, test, gates**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run --workspace --all-features
cargo clippy --all-targets --all-features --workspace -- -D warnings
```

Expected: PASS. (Real hand-tracking feel is operator-verified via `cargo rund` — not run here.)

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "feat(dots): Leap/MediaPipe hand attractors (v4 power curve + sim feed)" "" "DotsHandAttractor on TrackedHand entities, v4 computeLeapAttractorPower with Dots config (floor 0.05), appended to the shared attractor array after the mouse. Hardware feel operator-verified." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

### Task 2: Hand activity feeds the audio envelope

**Files:**
- Modify: `crates/wc-sketches/src/dots/audio_coupling.rs` (extend the activity envelope to include hand grab activity)

**Interfaces:**
- Consumes: `DotsHandAttractor` (or a derived hand-activity scalar), in addition to `DotsMouseAttractorState.power`.

- [ ] **Step 1: Include hand activity in the envelope target**

The D4 activity envelope currently rises from `DotsMouseAttractorState.power > 0`. Extend it so the envelope target is "any active attractor": rise toward 1.0 when the mouse is active OR any `DotsHandAttractor.power` is above a small threshold (use the max over hands so a second farther hand doesn't duck a near grab — mirror Line's `update_hand_audio_drive` "loudest hand wins"). Keep the same attack/release ease + `[0,1]` clamp + allocation-free discipline. The envelope still drives `volume`/`bandpass_freq`/`lfo_depth` as in D4 (no change there).

- [ ] **Step 2: Tests**

Extend the envelope test: with the mouse inactive but a hand `power` above threshold, the envelope RISES; with both inactive it decays. Assert the "loudest hand wins" max behavior with two hands. Keep the existing D4 envelope tests passing.

- [ ] **Step 3: Build, test, gates**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run --workspace --all-features
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo fmt --all -- --check
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "feat(dots): hand grab activity drives the audio envelope" "" "The activity envelope now rises for an active hand grab (loudest hand wins) as well as the mouse, so the synth speaks for hand interaction." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

## Self-Review

**Spec coverage** (against §"Dots-specific components" hand-attractor line + D5 row):
- `DotsHandAttractor` on `TrackedHand`, v4 curve + Dots config → Task 1. ✓
- Hands fed into the shared attractor array alongside the mouse → Task 1 Step 3. ✓
- Reuse of the existing `TrackedHand`/provider infra → constraints + Task 1 (no re-impl). ✓
- Hand activity drives audio → Task 2. ✓
- Bone-wireframe hand RENDERING → NOT this plan (D6). ✓ scope holds.

**Placeholder scan:** No TBD-as-deliverable. The curve + config + projection are given exactly (from named v4 source); the plumbing mirrors named Line code. The hardware FEEL is explicitly operator-deferred, with the deterministic curve + array-assembly unit-tested.

**Type consistency:** `DotsHandAttractor { power, position }`, `DotsLeapAttractorsPlugin`, `dots_leap_power`, `dots_palm_to_world`, the `DOTS_HAND_*` consts — consistent across tasks; feeds the shared `Attractor`/`SimParams` by their D1 names. `DOTS_GRAVITY_CONSTANT` (from D2 sim_params) is the gravity bake.

**Risks:** (1) The attractor-array append must exactly mirror Line's (gravity bake, threshold, cap) so the shared kernel force math matches — pinned by the Task 1 Step 4 array test. (2) Curve faithfulness (the floor Line omitted) — pinned by the curve unit test. (3) Hardware feel is operator-verified. (4) Duplicated curve/projection is a flagged carry-forward (v4 shared it).
