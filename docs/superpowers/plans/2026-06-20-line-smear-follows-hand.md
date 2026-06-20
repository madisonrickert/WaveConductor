# Line Smear Follows the Attractors (Smoothed) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In live (Active) mode, ease the Line gravity-smear focal point toward the active attractors (mouse + tracked/simulated hands) with a center-biased weighted centroid plus frame-rate-independent exponential smoothing, so the luminous smear tracks where the user is pulling without the ring-snap jolt that led the screensaver to pin its focal.

**Architecture:** Two pure helpers (`weighted_focal`, `ease_focal`) and a shared `FOCAL_CENTER_WEIGHT` constant live in the shared baker module `systems/sim_params.rs`. A new world-space `LineSmearFocal` resource holds the smoothed focal; `update_sim_params` eases it toward the attractor centroid each frame and feeds it to the existing `bake_post_base` in place of the raw `mouse.position`. The screensaver's existing inline centroid (`choreography::attract_frame`) is refactored onto the same `weighted_focal` helper (behavior-preserving DRY). One new Dev setting `smear_focal_smoothing` (the ease time constant τ) controls the follow.

**Tech Stack:** Rust, Bevy 0.18, WaveConductor v5 Line sketch (compute + gravity-smear post-process pipeline).

## Global Constraints

- **No allocations in hot paths.** `update_sim_params` runs every frame while Line is active. The focal-centroid samples MUST be a fixed-size stack array `[(f32, [f32; 2]); 1 + MAX_ATTRACTORS]` with a running count, never a `Vec` (AGENTS.md "Never allocate in a hot path").
- **Settings defaults match in both places.** A `#[setting(default = X)]` attribute and its `#[serde(default = "default_<name>")]` free function MUST produce the same value; add both together (settings.rs module doc mandate).
- **No `unwrap()` / `expect()` in non-test code** unless a documented invariant violation.
- **No `as` numeric casts** where `From` / `TryFrom` work. (The existing `attractor_count as usize` in `update_sim_params` is already covered by a module-level `#![allow(clippy::as_conversions, …)]` and is unchanged.)
- **`///` rustdoc on every new public item; `//!` module docs stay accurate.** Never strip comments during a refactor — update stale ones.
- **The screensaver refactor is behavior-preserving.** Every existing `choreography.rs` test must still pass unchanged: keep the same constant value `0.15` and the same walker-order summation so the focal value is bit-identical.
- **`LineSmearFocal` is a `Resource`, never a `Local`** — so it cannot carry stale focal state across a Line re-entry (the exact stale-`Local` trap the palette feature's final review caught).
- **Setting UI strings are user-facing copy: no em dashes** (en dashes for number ranges are fine). The label "Smear follow smoothing" and unit "s" are clean.
- **Commit messages contain no backticks** (they shell-substitute under `git commit -m`). Use plain text.
- **All CI gates green before a task is "done":** `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features --workspace -- -D warnings`; `cargo nextest run --workspace --all-features` (or `cargo test`); `cargo test --doc --workspace`; `cargo doc --no-deps --workspace --document-private-items`; `cargo deny check`; `cargo xtask check-secrets`.

---

## File Structure

- **Modify** `crates/wc-sketches/src/line/systems/sim_params.rs` — add `FOCAL_CENTER_WEIGHT`, `weighted_focal`, `ease_focal`, and the `LineSmearFocal` resource (Tasks 1, 4); rewire `update_sim_params` to ease the focal (Task 5). This is the shared-baker module both the live and screensaver writers already depend on.
- **Modify** `crates/wc-sketches/src/line/screensaver/choreography.rs` — refactor `attract_frame` onto `weighted_focal`; remove the local `FOCAL_CENTER_WEIGHT` const (Task 2).
- **Modify** `crates/wc-sketches/src/line/settings.rs` — add the `smear_focal_smoothing` Dev setting, its serde default, its module-doc bullet, and a forward-compat assertion (Task 3).
- **Modify** `crates/wc-sketches/src/line/systems/spawn.rs` — insert `LineSmearFocal(Vec2::ZERO)` in `spawn_line` (Task 4).
- **Modify** `crates/wc-sketches/src/line/mod.rs` — remove `LineSmearFocal` in `remove_sim_params`; add an exit-removal test (Task 4).

---

## Task 1: Pure focal helpers (`weighted_focal`, `ease_focal`, `FOCAL_CENTER_WEIGHT`)

**Files:**
- Modify: `crates/wc-sketches/src/line/systems/sim_params.rs` (add helpers near `bake_post_base`, ~line 182; add tests to the footer `mod tests`)

**Interfaces:**
- Consumes: nothing (pure functions, `std` only).
- Produces:
  - `pub const FOCAL_CENTER_WEIGHT: f32 = 0.15;`
  - `pub fn weighted_focal(samples: &[(f32, [f32; 2])], center_weight: f32) -> [f32; 2]`
  - `pub fn ease_focal(current: [f32; 2], target: [f32; 2], dt: f32, tau: f32) -> [f32; 2]`

- [ ] **Step 1: Write the failing tests**

Add these tests inside the existing `#[cfg(test)] mod tests` block at the bottom of `crates/wc-sketches/src/line/systems/sim_params.rs` (it already has `use super::*;`):

```rust
    #[test]
    fn weighted_focal_empty_is_center() {
        assert_eq!(weighted_focal(&[], FOCAL_CENTER_WEIGHT), [0.0, 0.0]);
    }

    #[test]
    fn weighted_focal_zero_weight_is_center() {
        assert_eq!(
            weighted_focal(&[(0.0, [100.0, 50.0])], FOCAL_CENTER_WEIGHT),
            [0.0, 0.0]
        );
    }

    #[test]
    fn weighted_focal_single_sample_sits_on_it_as_center_weight_vanishes() {
        // With no center bias a lone sample sits exactly on its position.
        let f = weighted_focal(&[(10.0, [100.0, 50.0])], 0.0);
        assert!((f[0] - 100.0).abs() < 1e-4);
        assert!((f[1] - 50.0).abs() < 1e-4);
        // With the center bias it sits slightly center-ward (power 10 >> W0).
        let biased = weighted_focal(&[(10.0, [100.0, 50.0])], FOCAL_CENTER_WEIGHT);
        assert!(biased[0] > 98.0 && biased[0] < 100.0);
    }

    #[test]
    fn weighted_focal_is_biased_toward_center() {
        // Two equal-weight samples at x = 100 and x = 200: the unbiased midpoint
        // is 150; the center bias pulls the focal below it (toward 0).
        let f = weighted_focal(
            &[(1.0, [100.0, 0.0]), (1.0, [200.0, 0.0])],
            FOCAL_CENTER_WEIGHT,
        );
        assert!(
            f[0] > 0.0 && f[0] < 150.0,
            "expected center-biased midpoint, got {}",
            f[0]
        );
    }

    #[test]
    fn ease_focal_moves_toward_target() {
        let f = ease_focal([0.0, 0.0], [100.0, 0.0], 0.016, 0.25);
        assert!(f[0] > 0.0 && f[0] < 100.0, "should ease partway, got {}", f[0]);
    }

    #[test]
    fn ease_focal_is_framerate_independent() {
        // One step of dt equals two steps of dt/2 for a constant target — the
        // discrete exponential form composes exactly.
        let target = [100.0, 40.0];
        let one = ease_focal([0.0, 0.0], target, 0.02, 0.3);
        let half = ease_focal([0.0, 0.0], target, 0.01, 0.3);
        let two = ease_focal(half, target, 0.01, 0.3);
        assert!((one[0] - two[0]).abs() < 1e-5, "{} vs {}", one[0], two[0]);
        assert!((one[1] - two[1]).abs() < 1e-5, "{} vs {}", one[1], two[1]);
    }

    #[test]
    fn ease_focal_converges_to_center() {
        let mut f = [300.0, 150.0];
        for _ in 0..200 {
            f = ease_focal(f, [0.0, 0.0], 0.016, 0.25);
        }
        assert!(
            f[0].abs() < 0.5 && f[1].abs() < 0.5,
            "should converge to center, got {f:?}"
        );
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "tau<=0 snaps to the target exactly")]
    fn ease_focal_zero_tau_snaps() {
        assert_eq!(
            ease_focal([0.0, 0.0], [100.0, 50.0], 0.016, 0.0),
            [100.0, 50.0]
        );
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "dt cap makes the two calls bit-identical")]
    fn ease_focal_caps_dt() {
        let huge = ease_focal([0.0, 0.0], [100.0, 0.0], 10.0, 0.25);
        let capped = ease_focal([0.0, 0.0], [100.0, 0.0], 0.05, 0.25);
        assert_eq!(huge, capped);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p wc-sketches --lib line::systems::sim_params`
Expected: FAIL to compile — `cannot find function 'weighted_focal'`, `cannot find function 'ease_focal'`, `cannot find value 'FOCAL_CENTER_WEIGHT'`.

- [ ] **Step 3: Implement the helpers**

Insert this block in `crates/wc-sketches/src/line/systems/sim_params.rs` immediately **before** the `bake_post_base` doc comment (currently ~line 183):

```rust
/// Center-bias weight `W₀` for the smear-focal centroid: a virtual sample
/// pinned at the world origin (screen center). Keeps the focal point defined
/// and smoothly moving when every attractor weight is zero, instead of dividing
/// by ~0 or snapping. Shared by the live writer ([`update_sim_params`]) and the
/// screensaver choreography
/// (`crate::line::screensaver::choreography::attract_frame`) so the two compute
/// the focal identically and cannot drift.
pub const FOCAL_CENTER_WEIGHT: f32 = 0.15;

/// Center-biased, weight-weighted centroid of `(weight, world_pos)` samples:
/// `Σ wᵢ·posᵢ / (Σ wᵢ + center_weight)`. The extra `center_weight` term is a
/// virtual sample at the origin, so the result is always defined and relaxes to
/// `[0, 0]` (screen center, world origin) as the sample weights fall to zero —
/// no divide-by-zero and no pop when the last sample releases.
///
/// Pure and allocation-free; the caller supplies a stack slice.
#[must_use]
pub fn weighted_focal(samples: &[(f32, [f32; 2])], center_weight: f32) -> [f32; 2] {
    let mut weighted = [0.0_f32, 0.0_f32];
    let mut weight_sum = 0.0_f32;
    for &(w, pos) in samples {
        weighted[0] += w * pos[0];
        weighted[1] += w * pos[1];
        weight_sum += w;
    }
    let denom = weight_sum + center_weight;
    // Degenerate guard: with no center bias and no (or net-negative) weights,
    // fall back to screen center rather than dividing by ~0.
    if denom <= 0.0 {
        return [0.0, 0.0];
    }
    [weighted[0] / denom, weighted[1] / denom]
}

/// Frame-rate-independent exponential ease of `current` toward `target` over
/// time constant `tau` seconds: `current + (target − current)·(1 − e^(−dt/τ))`.
///
/// `dt` is capped at 50 ms (matching the sim's `dt.min(0.05)`) so a long pause
/// can't teleport the focal in one frame. `tau <= 0` snaps instantly (α = 1) —
/// the un-smoothed / "off" setting. The discrete form composes exactly, so N
/// small steps land on the same point as one big step for a constant target
/// (the frame-rate-independence guarantee). Pure; operates on `[f32; 2]` so it
/// has no Bevy dependency.
#[must_use]
pub fn ease_focal(current: [f32; 2], target: [f32; 2], dt: f32, tau: f32) -> [f32; 2] {
    let dt = dt.min(0.05);
    let alpha = if tau <= 0.0 {
        1.0
    } else {
        1.0 - (-dt / tau).exp()
    };
    [
        current[0] + (target[0] - current[0]) * alpha,
        current[1] + (target[1] - current[1]) * alpha,
    ]
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wc-sketches --lib line::systems::sim_params`
Expected: PASS (all the new `weighted_focal_*` / `ease_focal_*` tests, plus the pre-existing `bake_*` tests).

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings` (expect clean), then:

```bash
git add crates/wc-sketches/src/line/systems/sim_params.rs
git commit -m "feat(line): add weighted_focal and ease_focal smear-focal helpers"
```

---

## Task 2: Refactor the screensaver centroid onto `weighted_focal` (behavior-preserving)

**Files:**
- Modify: `crates/wc-sketches/src/line/screensaver/choreography.rs` (remove local `FOCAL_CENTER_WEIGHT` ~lines 156-160; rewrite the focal accumulation inside `attract_frame` ~lines 221-260; add the import)

**Interfaces:**
- Consumes: `weighted_focal`, `FOCAL_CENTER_WEIGHT` from Task 1 (`crate::line::systems::sim_params`).
- Produces: no API change — `attract_frame` keeps its signature and returns the identical `AttractFrame`.

- [ ] **Step 1: Establish the green baseline**

Run: `cargo test -p wc-sketches --lib line::screensaver::choreography`
Expected: PASS — note especially `focal_relaxes_to_center_when_settled`, `deterministic_same_t_same_frame`, `peak_pulse_power_is_gentle`. These pin the behavior the refactor must preserve.

- [ ] **Step 2: Add the import**

At the top of `crates/wc-sketches/src/line/screensaver/choreography.rs` (the file currently has no `use` statements — it is pure `std` math), add:

```rust
use crate::line::systems::sim_params::{weighted_focal, FOCAL_CENTER_WEIGHT};
```

- [ ] **Step 3: Remove the local constant**

Delete the local constant and its doc comment (currently lines 156-160):

```rust
/// Center-bias weight in the smear-focal centroid: a virtual sample of this
/// weight pinned at the origin. Keeps the focal point defined (and smoothly
/// moving) when every pulse envelope is zero, instead of dividing by ~0 or
/// snapping between walkers.
const FOCAL_CENTER_WEIGHT: f32 = 0.15;
```

The shared `FOCAL_CENTER_WEIGHT` (Task 1, value `0.15`) now provides this; the value is unchanged.

- [ ] **Step 4: Refactor `attract_frame`'s focal accumulation**

In `attract_frame`, replace the focal-centroid accumulation. The current body (~lines 221-260) is:

```rust
    let mut pulses = [AttractorSample {
        position: [0.0, 0.0],
        power: 0.0,
    }; PULSE_COUNT];
    // Accumulators for the envelope-weighted focal centroid:
    //   focal = Σ envᵢ·posᵢ / (Σ envᵢ + W₀)
    // where W₀ = FOCAL_CENTER_WEIGHT is a virtual sample at the origin. When
    // one pulse dominates the focal sits (almost) on it; when all envelopes
    // are zero the focal relaxes exactly to screen center — continuous in t,
    // no branch, no snap.
    let mut weighted_pos = [0.0_f32, 0.0_f32];
    let mut env_sum = 0.0_f32;

    for (i, walker) in WALKERS.iter().enumerate() {
        // Lissajous wander: x and y are independent sines at incommensurate
        // frequencies, so the point sweeps the amplitude box over minutes.
        let position = [
            ax * (walker.freq[0] * t + walker.phase[0]).sin(),
            ay * (walker.freq[1] * t + walker.phase[1]).sin(),
        ];
        let env = pulse_envelope(t, i);
        // Power rests at the ambient floor (zero — see AMBIENT_POWER) and
        // swells linearly in the envelope: AMBIENT + (PEAK − AMBIENT)·env.
        let power = AMBIENT_POWER + (PULSE_PEAK_POWER - AMBIENT_POWER) * env;
        pulses[i] = AttractorSample { position, power };
        weighted_pos[0] += env * position[0];
        weighted_pos[1] += env * position[1];
        env_sum += env;
    }

    let focal_denom = env_sum + FOCAL_CENTER_WEIGHT;
    let focal_world = [weighted_pos[0] / focal_denom, weighted_pos[1] / focal_denom];

    AttractFrame {
        pulses,
        focal_world,
        // Overall activity: total pulse envelope, clamped — with the staggered
        // schedule this is effectively "the strongest pulse right now".
        activity: env_sum.min(1.0),
    }
```

Replace it with this (builds `(env, position)` samples in walker order — identical summation order, so the focal is bit-identical — and delegates the centroid to the shared helper):

```rust
    let mut pulses = [AttractorSample {
        position: [0.0, 0.0],
        power: 0.0,
    }; PULSE_COUNT];
    // (envelope, world_pos) samples for the shared center-biased focal
    // centroid: focal = Σ envᵢ·posᵢ / (Σ envᵢ + W₀), where W₀ is a virtual
    // sample at the origin (see FOCAL_CENTER_WEIGHT). When one pulse dominates
    // the focal sits (almost) on it; when all envelopes are zero it relaxes
    // exactly to screen center. Built in walker order so the centroid is
    // bit-identical to the prior inline accumulation.
    let mut focal_samples = [(0.0_f32, [0.0_f32, 0.0_f32]); PULSE_COUNT];
    let mut env_sum = 0.0_f32;

    for (i, walker) in WALKERS.iter().enumerate() {
        // Lissajous wander: x and y are independent sines at incommensurate
        // frequencies, so the point sweeps the amplitude box over minutes.
        let position = [
            ax * (walker.freq[0] * t + walker.phase[0]).sin(),
            ay * (walker.freq[1] * t + walker.phase[1]).sin(),
        ];
        let env = pulse_envelope(t, i);
        // Power rests at the ambient floor (zero — see AMBIENT_POWER) and
        // swells linearly in the envelope: AMBIENT + (PEAK − AMBIENT)·env.
        let power = AMBIENT_POWER + (PULSE_PEAK_POWER - AMBIENT_POWER) * env;
        pulses[i] = AttractorSample { position, power };
        focal_samples[i] = (env, position);
        env_sum += env;
    }

    // Shared center-biased weighted centroid (DRY with the live writer in
    // `systems::sim_params`): same formula, same constant — behavior-identical
    // to the prior inline math.
    let focal_world = weighted_focal(&focal_samples, FOCAL_CENTER_WEIGHT);

    AttractFrame {
        pulses,
        focal_world,
        // Overall activity: total pulse envelope, clamped — with the staggered
        // schedule this is effectively "the strongest pulse right now".
        activity: env_sum.min(1.0),
    }
```

- [ ] **Step 5: Run the tests to verify they still pass**

Run: `cargo test -p wc-sketches --lib line::screensaver::choreography`
Expected: PASS — all existing tests unchanged, especially `focal_relaxes_to_center_when_settled` (settled focal still exactly `[0.0, 0.0]`) and `deterministic_same_t_same_frame`.

- [ ] **Step 6: Lint and commit**

Run: `cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings` (expect clean), then:

```bash
git add crates/wc-sketches/src/line/screensaver/choreography.rs
git commit -m "refactor(line): screensaver focal centroid uses shared weighted_focal"
```

---

## Task 3: Add the `smear_focal_smoothing` Dev setting

**Files:**
- Modify: `crates/wc-sketches/src/line/settings.rs` (module-doc bullet ~line 85; new field after `smear_chroma_gain` ~line 268; new `default_smear_focal_smoothing` after `default_smear_chroma_gain` ~line 527; extend `missing_field_preserves_sibling_values` test ~line 599)

**Interfaces:**
- Consumes: nothing.
- Produces: `LineSettings::smear_focal_smoothing: f32` (Dev category, default `0.25`, range `0.0..=1.0`), consumed by Task 5.

- [ ] **Step 1: Write the failing assertion**

In `crates/wc-sketches/src/line/settings.rs`, extend the existing `missing_field_preserves_sibling_values` test. After the `smear_incoming_color` assertion (currently ~line 646-650), add:

```rust
        assert!(
            (parsed.smear_focal_smoothing - 0.25).abs() < 1e-6,
            "smear_focal_smoothing not default"
        );
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p wc-sketches --lib line::settings`
Expected: FAIL to compile — `no field 'smear_focal_smoothing' on type 'LineSettings'`.

- [ ] **Step 3: Add the field**

In `crates/wc-sketches/src/line/settings.rs`, add this field immediately **after** the `smear_chroma_gain` field (after its closing `pub smear_chroma_gain: f32,` ~line 268):

```rust
    /// Live-mode smear-focal ease time constant τ (seconds): how slowly the
    /// gravity-smear focal eases toward the active-attractor centroid. `0.0` =
    /// snap (instant follow, the un-smoothed feel); larger values lag and calm
    /// the follow so a moving hand can't jolt the concentric rings. Dev-only
    /// knob.
    #[setting(
        default = 0.25_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.05_f32,
        label = "Smear follow smoothing",
        unit = "s",
        category = Dev
    )]
    #[serde(default = "default_smear_focal_smoothing")]
    pub smear_focal_smoothing: f32,
```

- [ ] **Step 4: Add the serde default function**

Add this free function immediately **after** `default_smear_chroma_gain` (~line 527):

```rust
fn default_smear_focal_smoothing() -> f32 {
    0.25
}
```

- [ ] **Step 5: Add the module-doc bullet**

In the `//!` module doc list, add a bullet immediately **after** the `smear_chroma_gain` bullet (~line 85):

```rust
//! - **`smear_focal_smoothing`** — live-mode smear-focal ease time constant τ
//!   (seconds): how slowly the gravity-smear focal eases toward the active
//!   attractor centroid. `0.0` = instant snap; larger = calmer/laggier follow.
//!   Dev-only knob.
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p wc-sketches --lib line::settings`
Expected: PASS — `missing_field_preserves_sibling_values` now asserts the `0.25` default, and the `palette_mode_setting_is_enum_combobox` / other settings tests are unaffected.

- [ ] **Step 7: Lint and commit**

Run: `cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings` (expect clean), then:

```bash
git add crates/wc-sketches/src/line/settings.rs
git commit -m "feat(line): add smear_focal_smoothing Dev setting (focal ease tau)"
```

---

## Task 4: Add the `LineSmearFocal` resource and its lifecycle

**Files:**
- Modify: `crates/wc-sketches/src/line/systems/sim_params.rs` (define the resource near the top, after the `WindowGeom` impl ~line 78)
- Modify: `crates/wc-sketches/src/line/systems/spawn.rs` (import + insert in `spawn_line` ~line 285)
- Modify: `crates/wc-sketches/src/line/mod.rs` (remove in `remove_sim_params` ~line 278; update its doc; add exit-removal test ~line 435)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub struct LineSmearFocal(pub Vec2)` (a `Resource`), in world space. Inserted at `Vec2::ZERO` on `OnEnter(AppState::Line)`, removed on `OnExit(AppState::Line)`. Consumed by Task 5.

- [ ] **Step 1: Write the failing exit-removal test**

In `crates/wc-sketches/src/line/mod.rs`, inside the existing `#[cfg(test)] mod tests` block (it already carries `#![allow]`-equivalent module attributes and `use super::*;`), add:

```rust
    /// `remove_sim_params` must drop `LineSmearFocal` on Line exit so a
    /// re-entry's `spawn_line` re-seeds a fresh centered focal rather than
    /// inheriting a stale off-center one (the resource-not-Local guarantee).
    #[test]
    fn remove_sim_params_drops_smear_focal() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.insert_resource(systems::sim_params::LineSmearFocal(Vec2::new(123.0, 45.0)));
        world
            .run_system_once(remove_sim_params)
            .expect("remove_sim_params run");
        assert!(
            world
                .get_resource::<systems::sim_params::LineSmearFocal>()
                .is_none(),
            "LineSmearFocal must be removed on Line exit"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p wc-sketches --lib line::tests::remove_sim_params_drops_smear_focal`
Expected: FAIL to compile — `cannot find type 'LineSmearFocal' in module 'systems::sim_params'`.

- [ ] **Step 3: Define the resource**

In `crates/wc-sketches/src/line/systems/sim_params.rs`, add this immediately **after** the `impl WindowGeom { … }` block (~line 78, before the `AttractGate` doc comment):

```rust
/// The live-mode smoothed gravity-smear focal point, in world space (centered
/// on the origin, +y up). [`update_sim_params`] eases it toward the
/// active-attractor centroid each frame; [`bake_post_base`] converts it to the
/// shader's window-pixel space for [`LinePostParams::i_mouse`].
///
/// Inserted at [`Vec2::ZERO`] (screen center) in
/// [`crate::line::systems::spawn::spawn_line`] (`OnEnter(AppState::Line)`) and
/// removed in `remove_sim_params` (`OnExit(AppState::Line)`). Deliberately a
/// `Resource`, not a `Local`, so it cannot carry a stale focal across a Line
/// re-entry.
#[derive(Resource, Debug, Clone, Copy)]
pub struct LineSmearFocal(pub Vec2);
```

(`Vec2`, `Resource`, and the derive macro are all already in scope via the file's `use bevy::prelude::*;`.)

- [ ] **Step 4: Insert the resource in `spawn_line`**

In `crates/wc-sketches/src/line/systems/spawn.rs`, add the import alongside the other `crate::line::...` imports near the top (after `use crate::line::settings::LineSettings;` ~line 32):

```rust
use crate::line::systems::sim_params::LineSmearFocal;
```

Then, in `spawn_line`, immediately **after** the `commands.insert_resource(LineSimParams { … });` block (~line 289), add:

```rust
    // Seed the smoothed smear focal at screen center. `update_sim_params` eases
    // it toward the active-attractor centroid each frame; a resource (not a
    // Local) so a Line re-entry can't inherit a stale off-center focal.
    commands.insert_resource(LineSmearFocal(Vec2::ZERO));
```

- [ ] **Step 5: Remove the resource in `remove_sim_params` and update its doc**

In `crates/wc-sketches/src/line/mod.rs`, in `remove_sim_params` (~line 278), add the removal after the existing `LineCpuMirror` removal:

```rust
fn remove_sim_params(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<compute::LineSimParams>();
    commands.remove_resource::<sim_cpu::LineCpuMirror>();
    commands.remove_resource::<systems::sim_params::LineSmearFocal>();
    commands.insert_resource(post_process::LinePostParams::default());
}
```

Update the `remove_sim_params` doc comment: after the sentence describing the CPU-mirror drop, add a sentence so the comment stays accurate (AGENTS.md "update stale comments rather than removing them"):

```rust
/// Also drops [`systems::sim_params::LineSmearFocal`] so the next `OnEnter`
/// re-seeds a centered focal instead of inheriting the last in-Line value.
```

(Place that line within the existing doc block, after the `LineCpuMirror` paragraph and before the `LinePostParams` reset paragraph.)

- [ ] **Step 6: Run the exit-removal test to verify it passes**

Run: `cargo test -p wc-sketches --lib line::tests::remove_sim_params_drops_smear_focal`
Expected: PASS.

> **Coverage note (read before reviewing):** the `OnEnter` insert is a single unconditional `commands.insert_resource(LineSmearFocal(Vec2::ZERO))` mirroring the adjacent `LineSimParams` insert. Running `spawn_line` end-to-end requires a render-capable harness (registered `Assets<ShaderStorageBuffer>` / `Assets<LineMaterial>` / `Assets<Mesh>` plus a `Window` singleton and `AssetServer`), disproportionate for verifying one literal insert. The lifecycle's real risk — a stale focal surviving a re-entry — is guarded by the exit-removal test above (exit drops it, so the next enter always re-seeds `ZERO`). The eased-from-`ZERO` start is exercised by Task 5's `update_sim_params` system tests. No vacuous "assert the constant is the constant" test is added.

- [ ] **Step 7: Lint and commit**

Run: `cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings` (expect clean), then:

```bash
git add crates/wc-sketches/src/line/systems/sim_params.rs crates/wc-sketches/src/line/systems/spawn.rs crates/wc-sketches/src/line/mod.rs
git commit -m "feat(line): add LineSmearFocal resource with enter/exit lifecycle"
```

---

## Task 5: Wire `update_sim_params` to ease the smear focal

**Files:**
- Modify: `crates/wc-sketches/src/line/systems/sim_params.rs` (`update_sim_params` signature + body ~lines 226-313; update its doc; add two system tests to the footer `mod tests`)

**Interfaces:**
- Consumes: `weighted_focal`, `ease_focal`, `FOCAL_CENTER_WEIGHT` (Task 1); `LineSmearFocal` (Task 4); `LineSettings::smear_focal_smoothing` (Task 3); the existing `bake_post_base`.
- Produces: no public API change — `update_sim_params` keeps its system registration; it now eases `LineSmearFocal` and feeds it to `bake_post_base` in place of `mouse.position`.

- [ ] **Step 1: Write the failing system tests**

In `crates/wc-sketches/src/line/systems/sim_params.rs`, at the top of the `#[cfg(test)] mod tests` block add these imports under the existing `use super::*;`:

```rust
    use bevy::ecs::system::RunSystemOnce;
    use std::time::Duration;
```

Then add these two tests to the same block:

```rust
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    fn update_sim_params_eases_focal_toward_active_mouse() {
        let mut world = World::new();
        world.insert_resource(LineSettings::default()); // smear_focal_smoothing = 0.25
        world.insert_resource(MouseAttractorState {
            power: 10.0,
            position: [200.0, 100.0],
        });
        world.insert_resource(LineSimParams {
            params: SimParams::default(),
            particles_handle: Handle::default(),
            particle_count: 0,
        });
        world.insert_resource(LinePostParams::default());
        world.insert_resource(LineSmearFocal(Vec2::ZERO));
        let mut time = Time::default();
        time.advance_by(Duration::from_millis(16));
        world.insert_resource(time);
        world.spawn(Window::default()); // 1280x720 default resolution

        world
            .run_system_once(update_sim_params)
            .expect("update_sim_params run");

        // One 16 ms step at τ = 0.25 s eases ~6% of the way from center toward
        // the (center-biased) mouse target — strictly between center and mouse.
        let focal = world.resource::<LineSmearFocal>().0;
        assert!(focal.x > 0.0 && focal.x < 200.0, "x eased partway: {}", focal.x);
        assert!(focal.y > 0.0 && focal.y < 100.0, "y eased partway: {}", focal.y);

        // The eased focal reaches the smear uniform: i_mouse is the focal in
        // window-pixel space (top-left origin, +y down) for a 1280x720 window,
        // so a positive-x/positive-y world focal shifts i_mouse right of and
        // above center (640, 360).
        let post = world.resource::<LinePostParams>();
        assert!(post.i_mouse[0] > 640.0, "focal.x>0 shifts i_mouse right: {}", post.i_mouse[0]);
        assert!(post.i_mouse[1] < 360.0, "focal.y>0 shifts i_mouse up: {}", post.i_mouse[1]);
    }

    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    fn update_sim_params_relaxes_focal_to_center_when_idle() {
        let mut world = World::new();
        world.insert_resource(LineSettings::default());
        // No active attractors: mouse power 0, no tracked hands spawned.
        world.insert_resource(MouseAttractorState {
            power: 0.0,
            position: [0.0, 0.0],
        });
        world.insert_resource(LineSimParams {
            params: SimParams::default(),
            particles_handle: Handle::default(),
            particle_count: 0,
        });
        world.insert_resource(LinePostParams::default());
        // Start off-center; with no attractors the target is center, so the
        // eased focal must move back toward the origin (without overshooting).
        world.insert_resource(LineSmearFocal(Vec2::new(300.0, 150.0)));
        let mut time = Time::default();
        time.advance_by(Duration::from_millis(16));
        world.insert_resource(time);
        world.spawn(Window::default());

        world
            .run_system_once(update_sim_params)
            .expect("update_sim_params run");

        let focal = world.resource::<LineSmearFocal>().0;
        assert!(focal.x < 300.0 && focal.x > 0.0, "x relaxes toward center: {}", focal.x);
        assert!(focal.y < 150.0 && focal.y > 0.0, "y relaxes toward center: {}", focal.y);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p wc-sketches --lib line::systems::sim_params`
Expected: FAIL — `update_sim_params_*` tests fail at runtime (the focal is never eased: `LineSmearFocal` stays at its inserted value, so `focal.x`/`focal.y` assertions fail), because `update_sim_params` does not yet read or write `LineSmearFocal`. (If you added the `LineSmearFocal` param to the signature first, it would instead fail to compile — either red is acceptable.)

- [ ] **Step 3: Add the `LineSmearFocal` system param**

In `crates/wc-sketches/src/line/systems/sim_params.rs`, add the resource to the `update_sim_params` signature. Change:

```rust
    mut sim: ResMut<'_, LineSimParams>,
    mut post: ResMut<'_, LinePostParams>,
) {
```

to:

```rust
    mut sim: ResMut<'_, LineSimParams>,
    mut post: ResMut<'_, LinePostParams>,
    mut focal: ResMut<'_, LineSmearFocal>,
) {
```

- [ ] **Step 4: Build the focal samples alongside the attractor array**

In the same function, the attractor-building block currently reads (lines ~235-275):

```rust
    // --- Attractor list -------------------------------------------------
    let mut attractors = [Attractor::default(); MAX_ATTRACTORS];
    let mut attractor_count = 0_u32;
    if mouse.power > 0.0 {
        attractors[0] = Attractor {
            position: mouse.position,
            // Bake `gravity_constant` into the attractor's `power` so the
            // WGSL kernel can treat power uniformly across attractor sources.
            power: mouse.power * settings.gravity_constant,
            // Unbounded pull (v4 parity): no current attractor localizes its radius.
            radius: 0.0,
        };
        attractor_count = 1;
    }

    // Append LineHandAttractor entries after the mouse attractor.
    // Skip very-low-power entries to avoid wasting uniform slots on
    // fully-decayed hands.
    //
    // `slot` tracks the usize index in parallel with `attractor_count` (u32)
    // to avoid a `usize::try_from` / `expect` in the hot path. Both advance
    // in lockstep and are capped at MAX_ATTRACTORS (= 8), which fits in both.
    let mut slot = attractor_count as usize;
    for hand_attractor in &line_hands {
        if hand_attractor.power.abs() <= 1e-2 {
            continue;
        }
        if slot >= MAX_ATTRACTORS {
            break;
        }
        attractors[slot] = Attractor {
            position: hand_attractor.position.to_array(),
            // Bake gravity_constant into power, matching the mouse
            // attractor's treatment.
            power: hand_attractor.power * settings.gravity_constant,
            // Unbounded pull (v4 parity): no current attractor localizes its radius.
            radius: 0.0,
        };
        attractor_count += 1;
        slot += 1;
    }
```

Replace it with this (same attractor logic, plus a parallel stack-allocated `focal_samples` buffer carrying the **raw** source powers so `W₀` stays decoupled from `gravity_constant`):

```rust
    // --- Attractor list + smear-focal samples ---------------------------
    let mut attractors = [Attractor::default(); MAX_ATTRACTORS];
    let mut attractor_count = 0_u32;

    // Center-biased focal-centroid samples (raw source power pre-
    // `gravity_constant`, world position). Fixed-size stack buffer sized to the
    // worst case (mouse + every attractor slot) — no heap in this per-frame
    // hot path. `focal_count` tracks the live entries.
    let mut focal_samples = [(0.0_f32, [0.0_f32, 0.0_f32]); 1 + MAX_ATTRACTORS];
    let mut focal_count = 0_usize;

    if mouse.power > 0.0 {
        attractors[0] = Attractor {
            position: mouse.position,
            // Bake `gravity_constant` into the attractor's `power` so the
            // WGSL kernel can treat power uniformly across attractor sources.
            power: mouse.power * settings.gravity_constant,
            // Unbounded pull (v4 parity): no current attractor localizes its radius.
            radius: 0.0,
        };
        attractor_count = 1;
        // Focal sample uses the RAW mouse power (pre-`gravity_constant`).
        focal_samples[focal_count] = (mouse.power, mouse.position);
        focal_count += 1;
    }

    // Append LineHandAttractor entries after the mouse attractor.
    // Skip very-low-power entries to avoid wasting uniform slots on
    // fully-decayed hands.
    //
    // `slot` tracks the usize index in parallel with `attractor_count` (u32)
    // to avoid a `usize::try_from` / `expect` in the hot path. Both advance
    // in lockstep and are capped at MAX_ATTRACTORS (= 8), which fits in both.
    let mut slot = attractor_count as usize;
    for hand_attractor in &line_hands {
        if hand_attractor.power.abs() <= 1e-2 {
            continue;
        }
        if slot >= MAX_ATTRACTORS {
            break;
        }
        attractors[slot] = Attractor {
            position: hand_attractor.position.to_array(),
            // Bake gravity_constant into power, matching the mouse
            // attractor's treatment.
            power: hand_attractor.power * settings.gravity_constant,
            // Unbounded pull (v4 parity): no current attractor localizes its radius.
            radius: 0.0,
        };
        attractor_count += 1;
        slot += 1;
        // Focal sample uses the RAW hand power (pre-`gravity_constant`), so the
        // center-bias weight stays decoupled from the gravity_constant knob.
        focal_samples[focal_count] =
            (hand_attractor.power, hand_attractor.position.to_array());
        focal_count += 1;
    }
```

- [ ] **Step 5: Ease the focal and feed it to `bake_post_base`**

In the same function, the post-process baking block currently reads (lines ~290-302):

```rust
    // --- Gravity-smear post-process uniforms ---------------------------
    //
    // The post-process shader works in window-pixel space (matches v4's
    // `gl_FragCoord.xy` reference). Particles live in world space centred at
    // the origin (+y up) — `bake_post_base` converts the mouse position back
    // to window-pixel coords (top-left origin, +y down) for `iMouse`.
    bake_post_base(
        &mut post,
        geom,
        mouse.position,
        time.elapsed_secs(),
        settings.gamma,
    );
```

Replace it with this (compute the center-biased target, ease the stored focal toward it, then bake the eased focal):

```rust
    // --- Smear focal: ease toward the active-attractor centroid ---------
    //
    // Center-biased weighted centroid of the active attractors (relaxing to
    // screen center as powers fade), then a frame-rate-independent exponential
    // ease (τ = `smear_focal_smoothing`) so a moving or jittery hand can't snap
    // the concentric rings. `smear_focal_smoothing = 0.0` recovers the old
    // instant-snap-to-mouse feel.
    let target = weighted_focal(&focal_samples[..focal_count], FOCAL_CENTER_WEIGHT);
    focal.0 = Vec2::from(ease_focal(
        focal.0.to_array(),
        target,
        time.delta_secs(),
        settings.smear_focal_smoothing,
    ));

    // --- Gravity-smear post-process uniforms ---------------------------
    //
    // The post-process shader works in window-pixel space (matches v4's
    // `gl_FragCoord.xy` reference). Particles live in world space centred at
    // the origin (+y up) — `bake_post_base` converts the eased smear focal back
    // to window-pixel coords (top-left origin, +y down) for `iMouse`.
    bake_post_base(
        &mut post,
        geom,
        focal.0.to_array(),
        time.elapsed_secs(),
        settings.gamma,
    );
```

- [ ] **Step 6: Update the `update_sim_params` doc comment**

The current doc (lines ~220-225) reads:

```rust
/// `Update` — gated by `sketch_active(AppState::Line)`.
///
/// Collects the live attractors (mouse + tracked hands), bakes them via the
/// shared [`bake_sim_params`] / [`bake_post_base`] (Condition A1), and writes
/// placeholder `g_constant` / `i_mouse_factor` that `audio_coupling` overrides
/// later in the same frame.
```

Replace with:

```rust
/// `Update` — gated by `sketch_active(AppState::Line)`.
///
/// Collects the live attractors (mouse + tracked hands), bakes them via the
/// shared [`bake_sim_params`] (Condition A1), eases the [`LineSmearFocal`]
/// toward the center-biased attractor centroid (so the gravity smear tracks the
/// user's pull without snapping), bakes the post-process base via
/// [`bake_post_base`] with that eased focal, and writes placeholder
/// `g_constant` / `i_mouse_factor` that `audio_coupling` overrides later in the
/// same frame.
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p wc-sketches --lib line::systems::sim_params`
Expected: PASS — both new `update_sim_params_*` tests, plus the Task 1 helper tests and the pre-existing `bake_*` tests.

- [ ] **Step 8: Lint and commit**

Run: `cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings` (expect clean), then:

```bash
git add crates/wc-sketches/src/line/systems/sim_params.rs
git commit -m "feat(line): smear focal eases toward active attractors in live mode"
```

---

## Final verification (after all tasks)

Run the full CI gate set from the workspace root and confirm all green before declaring the branch done (AGENTS.md "Verifying changes"):

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
```

Expected: all pass (the ~29 pre-existing rustdoc-link warnings on stable are non-fatal; no new ones should appear). Then prompt the operator for the live-tuning checkpoint below.

## Operator live-tuning checkpoint (out of scope for implementation)

Per the spec, the jolt-vs-lag feel of τ (`smear_focal_smoothing`, default `0.25`) is an operator judgment, not a coded value to defend. After the gates pass, prompt Madison to run `cargo rund`, enter Gravity, and confirm with the synthetic hand (and mouse): the smear glow should now drift toward where particles are being pulled and ease back to center on release, with no hard ring snap. The Dev panel's "Smear follow smoothing" slider tunes it live (`0.0` = instant snap; larger = calmer). Note the deliberate, flagged behavior change: the mouse smear now eases too (it used to snap to the cursor); `smear_focal_smoothing = 0.0` recovers the old instant feel.

## Self-review notes (plan author)

- **Spec coverage:** shared `weighted_focal` + `FOCAL_CENTER_WEIGHT` (Task 1) ✓; `ease_focal` with dt cap + tau≤0 snap + framerate independence (Task 1) ✓; screensaver `attract_frame` refactor onto the shared helper, local const removed, existing tests preserved (Task 2) ✓; `LineSmearFocal` resource as a Resource-not-Local with enter-insert / exit-remove lifecycle (Task 4) ✓; `smear_focal_smoothing` Dev setting + serde default + forward-compat (Task 3) ✓; `update_sim_params` rewiring with raw-power samples, center-biased target, ease, and focal fed to `bake_post_base` (Task 5) ✓; screensaver writer left pinned to center (untouched) ✓.
- **Testing coverage vs spec:** `weighted_focal` empty/zero/single/biased cases ✓; `ease_focal` toward-target / framerate-independence / converge-to-center / tau=0 snap / dt cap ✓; screensaver no-regression via the unchanged suite ✓; lifecycle exit-removal ✓ (enter-insert coverage rationale documented in Task 4 Step 6); forward-compat ✓; eased-from-ZERO behavior via Task 5 system tests ✓.
- **Deliberate scoping deviation flagged for the operator:** the spec's "LineSmearFocal present after OnEnter at Vec2::ZERO" is covered by the exit-removal test + Task 5's eased-from-ZERO tests rather than a heavyweight `spawn_line` render harness (Task 4 Step 6). Surface this in the final review.
- **Type consistency:** `weighted_focal(&[(f32,[f32;2])], f32) -> [f32;2]`, `ease_focal([f32;2],[f32;2],f32,f32) -> [f32;2]`, `LineSmearFocal(pub Vec2)`, `smear_focal_smoothing: f32` — names and signatures identical across every task that references them.
