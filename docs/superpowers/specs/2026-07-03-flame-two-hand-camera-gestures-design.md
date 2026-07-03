# Flame two-hand camera gestures — design

**Date:** 2026-07-03
**Status:** approved for implementation (autonomous /goal session; Madison reviews post-hoc)
**Goal:** 3D-hand grabbing drives the full Flame camera — zoom, rotate, and move the
view — intuitively. Moving both grabbed hands closer together / farther apart mimics
pinch-to-zoom.

## Context

Flame already has single-hand grab-and-fling orbit (ported line-for-line from v4):
`update_flame_hands` (`crates/wc-sketches/src/flame/systems/hands.rs`) gathers every
`TrackedHand` with `GrabStrength > 0.5`, averages them into one centroid, and the pure
`step_grab` state machine drives `FlameCamera.azimuth/polar` with a `0.7/0.3` momentum
blend that becomes fling on release. Zoom is mouse-wheel-only
(`FlameCamera.distance`, clamped `[0.1, 8.0]`), and no pan exists anywhere — v4 ran
`OrbitControls` with `enablePan = false` and had no two-hand gesture. This is new
interaction design, not a parity port (confirmed against `.worktrees/v4` and
`PARITY.md`).

Key structural fact: there is no `Camera3d` entity. `FlameCamera` is a plain CPU
`Resource` whose `view_from_model()` / `clip_from_view()` matrices ship as material
uniforms (approved deviation #2 in `PARITY.md`). All camera work happens in
`systems/camera.rs` + `systems/hands.rs`.

## Interaction model

| Hands grabbing | Gesture | Camera effect |
|---|---|---|
| 0 | — | fling momentum decays; autorotate; mouse still works |
| 1 | drag | orbit (azimuth/polar) — **unchanged v4 behavior**, fling on release |
| 2 | spread / squeeze | zoom: `distance` scales by the inverse spread ratio |
| 2 | midpoint drag | pan: `target` translates in the camera plane (content follows hands) |

- **Zoom** is anchor-ratio based: on the frame both hands engage, stash
  `anchor_spread` (inter-hand distance, window px) and `anchor_distance`. Each steady
  frame: `distance = clamp(anchor_distance * (anchor_spread / spread)^gamma, 0.1, 8.0)`.
  Spreading hands apart zooms in (camera closes), squeezing them together zooms out —
  the pinch-to-zoom convention. Anchor-ratio (rather than per-frame incremental)
  avoids drift accumulation and returns to the anchor pose when hands return.
- **Pan** uses the two-hand midpoint's frame-to-frame pixel delta, converted to world
  units at the target plane (`world_per_pixel = 2 * distance * tan(fovy/2) / window_h`)
  along the camera's right/up basis, signed so the fractal follows the hands (grab
  metaphor). `target` is clamped to `PAN_MAX_RADIUS = 2.0` from the origin.
- **Mode transitions reuse the existing re-anchor pattern:** any grabbing-count change
  (0↔1↔2) re-stashes anchors and moves nothing that frame, so threshold flicker can
  never cause a jump. Going 2→1 resumes orbit cleanly; 2→0 releases with **no** fling
  (angular velocity is held at zero throughout two-hand mode — zoom/pan momentum was
  considered and rejected as disorienting on a kiosk).
- **Fractal warp routing is preserved:** `warp_px` keeps tracking the grabbing
  centroid (which for two hands is the midpoint) through the stashed offset, exactly
  as today.
- **Depth (palm z) is deliberately unused.** MediaPipe's z is a smoothed monocular
  size-estimate; inter-hand XY distance is the reliable cross-provider signal, and it
  is also the gesture Madison described. (Considered and rejected: one-hand z-push
  dolly.)
- **Grab detection stays Flame's raw `GrabStrength > 0.5`** (no switch to the
  `HandButton` 0.8/0.5 hysteresis layer): consistent feel with the shipped one-hand
  grab, and the re-anchor-on-count-change rule already makes threshold flicker
  harmless (a lost frame, never a jump). Chirality/identity machinery is unnecessary —
  midpoint and spread are symmetric under hand swap.

## Architecture

No new systems, no new registrations (avoids the double-register schedule-panic class
entirely). Both existing systems grow:

### `systems/camera.rs`

```rust
pub struct FlameCamera {
    pub azimuth: f32,
    pub polar: f32,
    pub distance: f32,
    pub target: Vec3,            // NEW — orbit/look-at center, default Vec3::ZERO
    pub angular_velocity: Vec2,
    pub last_drag: Option<Vec2>,
}
```

- `eye()` returns `self.target + distance * spherical` ; `view_from_model()` looks at
  `self.target` (model `rotateX(-PI/2)` baking unchanged).
- New `const FOVY: f32 = 60 deg` shared by `clip_from_view` and the pan math; new
  `const PAN_MAX_RADIUS: f32 = 2.0`; new `const RECENTER_DECAY: f32 = 0.98`
  (per-frame-at-60fps, dt-scaled like `MOMENTUM_DECAY`).
- New methods (pan/zoom math lives here so `hands.rs` stays a gather-and-step file):
  - `pub fn pan_by_pixels(&mut self, delta_px: Vec2, window_height: f32, sensitivity: f32)`
    — `delta_px` in window-logical px (y down); translates `target` by
    `(-right * delta.x + up * delta.y) * world_per_pixel * sensitivity`, then
    `clamp_length_max(PAN_MAX_RADIUS)`. Basis: `forward = -spherical_offset.normalize()`,
    `right = forward.cross(Y).normalize()`, `up = right.cross(forward)` (polar clamp
    keeps the basis non-degenerate).
  - `pub fn set_distance_clamped(&mut self, distance: f32)` — clamps to
    `[MIN_DISTANCE, MAX_DISTANCE]`; wheel zoom refactors onto it.
- `update_flame_camera` gains `Res<State<SketchActivity>>`: during
  `SketchActivity::Screensaver`, `target` eases geometrically toward `Vec3::ZERO`
  (`target *= RECENTER_DECAY.powf(dt * 60)`), so a walked-away-from pan never strands
  the attract loop off-center. Distance persists, matching the existing wheel-zoom
  precedent.

### `systems/hands.rs`

- Gathering also records the first two grabbing positions; a named struct replaces the
  `(Option<Vec2>, usize)` tuple:
  `struct GrabGather { centroid: Vec2, count: usize, spread: f32 }` (spread = distance
  between the first two grabbing hands, `0.0` when count < 2; `MAX_HANDS` is 2 so
  "first two" is exhaustive).
- `FlameGrabState` gains `anchor_spread: f32` and `anchor_distance: f32`.
- `step_grab(state, camera, gather: Option<GrabGather>, window, zoom_gamma, pan_sensitivity)`:
  - `None` → count 0, return (unchanged).
  - count changed → existing stash branch, plus stash `anchor_spread`
    (`spread.max(1.0)` px) and `anchor_distance` when count ≥ 2.
  - steady count == 1 → existing orbit branch (unchanged).
  - steady count ≥ 2 → `camera.set_distance_clamped(anchor_distance *
    (anchor_spread / spread.max(1.0)).powf(zoom_gamma))`;
    `camera.pan_by_pixels(centroid - last, window.y, pan_sensitivity)`;
    `camera.angular_velocity = Vec2::ZERO`; `last`/`warp_px` update as today.
- `update_flame_hands` gains `Res<FlameSettings>` to read the two new knobs. Fixed
  stack buffers throughout — no per-frame allocation.
- `flame_idle_veto` already covers two-hand mode (`grabbing_count > 0`).

### `settings.rs`

Two live Dev-category knobs (precedent: `autorotate_speed`; same section):

- `two_hand_zoom_gamma: f32` — default 1.0, min 0.25, max 3.0, step 0.05. Exponent on
  the spread ratio; >1 = more aggressive zoom per hand movement.
- `hand_pan_sensitivity: f32` — default 1.0, min 0.0, max 3.0, step 0.05. 0 disables
  pan. Each with the matching `default_*` serde fn and the defaults-sync test entry.

### Debug/capture hook

`WC_DEBUG_FORCE_FLAME_CAMERA_POSE` (pattern: `WC_DEBUG_FORCE_FLAME_WARP`): pins a
deterministic non-default pose (azimuth 0.9, polar 1.1, distance 0.35, target
(0.2, 0.0, 0.1)) at the end of `update_flame_camera`, plus a `flame-camera-pose`
capture scenario regression-guarding the new target-aware view matrix. Debug-gated,
absent from release builds.

## Unaffected consumers (verified)

`drive_flame_material` (reads matrices — internal change only), `audio_coupling`
(`camera_distance` semantics unchanged; hand zoom now modulates the synth like wheel
zoom does), `update_flame_sim` (reads `warp_px`/`grabbing_count`, both preserved),
`FlameCamera` reinsertion on `OnEnter` (fresh `target` each entry).

## Error handling / robustness

Spread division guarded by `.max(1.0)` px; polar clamp keeps the pan basis finite;
`target` length-clamped; distance clamped; all matrices already covered by the
`matrices_are_finite` test, which extends to nonzero `target`. Hand-tracking loss
mid-gesture is just count→0: anchors go stale harmlessly and re-stash on the next
engage.

## Testing

- Unit (colocated): zoom ratio math (apart→closer distance shrinks, together→grows,
  gamma exponent, clamp), pan direction/sign and radius clamp, no-jump on 1↔2
  transitions, no fling after 2-hand release, screensaver recenter convergence,
  matrices finite with nonzero target, gather spread correctness.
- World-level ECS test: two spawned `TrackedHand` entities spreading apart across two
  `update_flame_hands` runs → `distance` decreases (mirrors the existing one-hand
  world test).
- Capture: `flame-camera-pose` scenario (above). Manual feel pass (`cargo rund` +
  real hands) is Madison's sign-off, logged as a PARITY-style checklist item.
