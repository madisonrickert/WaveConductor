# Line Gravity Smear Follows the Attractors (Smoothed) — Design Spec

**Date:** 2026-06-20
**Status:** Approved design, ready for implementation plan
**Related:** [2026-05-29-line-screensaver-attract-mode-design.md](2026-05-29-line-screensaver-attract-mode-design.md) (the screensaver work that pinned the smear focal to avoid the jolt this feature must also avoid)

## Goal

In **active** (live) mode, make the gravity-smear focal point ease toward the active attractors (mouse + tracked/simulated hands) instead of sitting at screen center, so the luminous smear tracks where the user is pulling — **without** the ring-snap jolt that led the screensaver to pin its focal.

## Context: how the smear focal works today

The gravity smear (`assets/shaders/line/gravity.wgsl`, `LinePostProcessPlugin`) ray-marches gravity-distorted UV samples centered on a focal point, `LinePostParams::i_mouse` (window-pixel space). Two writers set it via the shared `bake_post_base` (Plan 11.8 Condition A1):

- **Active** (`systems/sim_params.rs::update_sim_params`, gated `sketch_active(Line)`): passes `mouse.position` as the focal. Tracked hands are added to the **simulation** attractor array (so particles *are* pulled toward a hand), but they never move the smear focal. With no mouse press, `mouse.position` is its resting value (world origin → screen center), so the rings stay centered while a hand drags particles off-center — the disconnect this feature fixes.
- **Screensaver** (`screensaver/mod.rs::drive_line_attract`): **deliberately pins** the focal to `[0, 0]` (center). It *computes* a center-biased weighted centroid of the pulse points (`AttractFrame::focal_world`) but does **not** use it for the smear, because a moving focal "yanks the whole concentric ring pattern toward it and snaps back — a hard jolt of the gravity shader" (operator report; commit `e8239e4`).

So the centroid math already exists (`screensaver/choreography.rs`, `focal = Σ envᵢ·posᵢ / (Σ envᵢ + W₀)`, `W₀ = FOCAL_CENTER_WEIGHT = 0.15`) but is unused for the smear, and the missing ingredient is **temporal smoothing** so the focal eases instead of snapping.

## Approach: two layers that together kill the jolt

1. **Center-biased weighted centroid (spatial).** A power-weighted centroid of the active attractors plus a virtual center sample, so the focal sits near the dominant puller and **relaxes smoothly to center** as powers fade (no divide-by-zero, no pop on release). This is the screensaver's existing formula.
2. **Temporal exponential smoothing (motion).** Ease the focal toward that target with a framerate-independent exponential filter, so a moving (or jittery) hand can't snap the rings. This is the layer the screensaver lacked.

## Shared focal helper (DRY with the screensaver)

Factor the centroid into one shared, pure helper in `systems/sim_params.rs` (the shared-baker module), so the active path and the screensaver's `attract_frame` compute it identically and cannot drift:

```rust
/// Center-bias weight `W₀` for the smear-focal centroid: a virtual sample at
/// the origin so the focal stays defined and relaxes smoothly to center when
/// all attractor weights are zero. Shared by the live writer and the
/// screensaver choreography.
pub const FOCAL_CENTER_WEIGHT: f32 = 0.15;

/// Center-biased, weight-weighted centroid of `(weight, world_pos)` samples:
/// `Σ wᵢ·posᵢ / (Σ wᵢ + W₀)`. Returns `[0, 0]` (screen center, world origin)
/// when all weights are zero. Pure; allocation-free.
#[must_use]
pub fn weighted_focal(samples: &[(f32, [f32; 2])], center_weight: f32) -> [f32; 2] { ... }
```

`screensaver/choreography.rs::attract_frame` is refactored to call `weighted_focal` (passing `(env, pos)` pairs and the shared `FOCAL_CENTER_WEIGHT`) in place of its inline accumulation. **Behavior is identical** — same formula, same constant, same `focal_relaxes_to_center_when_settled` test still passes — this is a DRY refactor, not a screensaver behavior change. (`choreography.rs`'s local `FOCAL_CENTER_WEIGHT` const is removed in favor of the shared one.)

## Temporal smoothing helper

```rust
/// Frame-rate-independent exponential ease of `current` toward `target` over
/// time constant `tau` seconds: `current + (target − current)·(1 − e^(−dt/τ))`.
/// `dt` is capped (50 ms, matching the sim) so a long pause can't teleport the
/// focal. `tau <= 0` snaps instantly (α = 1) — the un-smoothed/"off" setting.
/// Pure; the discrete form composes exactly, so N small steps land on the same
/// point as one big step for a constant target (framerate independence).
#[must_use]
pub fn ease_focal(current: [f32; 2], target: [f32; 2], dt: f32, tau: f32) -> [f32; 2] { ... }
```

## State + lifecycle

The smoothed focal (world space) lives in a small resource:

```rust
/// The active-mode smoothed gravity-smear focal, world space. Eased toward the
/// attractor centroid each frame by `update_sim_params`; read by `bake_post_base`.
#[derive(Resource, Debug, Clone, Copy)]
pub struct LineSmearFocal(pub Vec2);
```

- **Inserted at center** (`Vec2::ZERO`) in `spawn_line` (`OnEnter(AppState::Line)`), alongside the existing `LineSimParams` install — so it starts centered (no entry snap).
- **Removed** in `remove_sim_params` (`OnExit(AppState::Line)`).
- Deliberately a **resource, not a `Local`**, so it cannot carry stale focal state across a Line re-entry (the exact stale-`Local` trap the palette feature's final review caught).

## Active-mode wiring

`update_sim_params` already has `mouse: Res<MouseAttractorState>` and `line_hands: Query<&LineHandAttractor, With<TrackedHand>>`, and builds the attractor array. Add:

1. Build the centroid samples from **source powers** (pre-`gravity_constant`): `(mouse.power, mouse.position)` when `mouse.power > 0`, plus `(hand.power, hand.position)` for each active hand. Weighting by source power keeps `W₀` decoupled from the `gravity_constant` User knob; a hard mouse press (`power` up to `MOUSE_POWER_PRESS = 10`) naturally dominates a light hand (`power ~0..1`) — moot in the kiosk (hands only).
2. `let target = weighted_focal(&samples, FOCAL_CENTER_WEIGHT);`
3. `focal.0 = Vec2::from(ease_focal(focal.0.to_array(), target, time.delta_secs(), settings.smear_focal_smoothing));` (reads/writes the `LineSmearFocal` resource).
4. Pass `focal.0.to_array()` to `bake_post_base` **instead of** `mouse.position`.

The screensaver writer is unchanged (still pins `[0, 0]`).

## Settings

One new Dev knob on `LineSettings`:

| field | type | category | default | range | meaning |
|---|---|---|---|---|---|
| `smear_focal_smoothing` | `f32` | Dev | `0.25` | `0.0–1.0` | smear-focal ease time constant τ (seconds). `0.0` = snap (instant follow, the un-smoothed feel); larger = laggier/calmer. |

Follows the established settings pattern: `#[setting(default = 0.25_f32, min = 0.0, max = 1.0, step = 0.05, label = "Smear follow smoothing", unit = "s", category = Dev)]` plus a `default_smear_focal_smoothing()` serde fn mirroring the default (forward-compat mandate).

## Scope guards

- **Screensaver smear behavior is unchanged** — it keeps its deliberate pinned-to-center focal; this feature is active-mode only. The only screensaver edit is the behavior-preserving DRY refactor to the shared `weighted_focal`.
- **The mouse smear becomes smoothed too.** Today it snaps to `mouse.position` instantly; now it eases via the same filter. With `mouse.power` (≥2 when active) ≫ `W₀ = 0.15`, the center-bias is negligible for the mouse (focal ≈ `0.99 × mouse.pos`), so the only change is the ease — arguably an improvement (no snap), flagged because it is not purely additive. `smear_focal_smoothing = 0.0` recovers the old instant-snap feel.

## Performance

Per-frame, in an already-running gated system: a few multiply-adds for the centroid, one `exp` for the ease — negligible. Allocation-free (the samples are a small stack array or a fixed-capacity buffer sized to `1 + MAX_ATTRACTORS`; both helpers take/return `[f32; 2]`/slices, no heap). One extra tiny resource. Zero systems added; zero cost when not in `AppState::Line` (the resource is absent and `update_sim_params` is gated). No change to the soak profile.

## Testing

- `weighted_focal`: empty / all-zero-weight samples → `[0, 0]`; a single dominant sample sits near its position (and exactly on it as `W₀ → 0`); two equal-weight opposed samples → midpoint biased toward center; matches the screensaver's prior `focal_relaxes_to_center_when_settled` expectation.
- `ease_focal`: moves toward the target; **framerate independence** — one step of `dt` equals two steps of `dt/2` for a constant target (the discrete form composes exactly), asserted within float epsilon; converges to center when target is center; `tau = 0` snaps (returns `target`); `dt` is capped (a 10 s `dt` behaves like 50 ms).
- Screensaver no-regression: the refactored `attract_frame` still passes `focal_relaxes_to_center_when_settled` and its pulse-crest focal test (the centroid value is unchanged by the extraction).
- Lifecycle: `LineSmearFocal` is present after `OnEnter(Line)` at `Vec2::ZERO` and absent after `OnExit(Line)` (re-entry starts centered, not stale).
- Forward-compat: extend the `LineSettings` missing-field test so legacy TOML lacking `smear_focal_smoothing` deserializes to the `0.25` default with siblings preserved.

## Out of scope

- **Visual/aesthetic tuning** of `τ` and `W₀` (the jolt-vs-lag feel) — that's a `cargo rund` checkpoint for the operator, like the palette's tuning step.
- **Per-hand focal selection** beyond the weighted centroid (e.g. "always follow the right hand"). The centroid handles multi-hand uniformly; a named-hand policy is a future option if the centroid-between-two-hands case proves distracting.
- **Smear focal in the screensaver** — it stays pinned to center by deliberate prior decision; revisiting that is separate work.
