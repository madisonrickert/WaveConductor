# Flame Two-Hand Camera Gestures Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two grabbed hands drive Flame's camera: spread/squeeze zooms (pinch-to-zoom convention), midpoint drag pans; one grabbed hand keeps the shipped v4 orbit-and-fling.

**Architecture:** No new systems or registrations. `FlameCamera` (CPU resource, `systems/camera.rs`) gains a pan `target` plus `pan_by_pixels`/`set_distance_clamped` methods; the pure `step_grab` state machine (`systems/hands.rs`) gains a two-hand branch fed by a `GrabGather` struct (centroid + count + spread). Two live Dev settings tune gamma and pan sensitivity. A `WC_DEBUG_FORCE_FLAME_CAMERA_POSE` toggle + capture scenario regression-guard the target-aware view matrix.

**Tech Stack:** Rust, Bevy (ECS resources/systems), existing `TrackedHand`/`GrabStrength` input components.

**Spec:** `docs/superpowers/specs/2026-07-03-flame-two-hand-camera-gestures-design.md` — read it first; it pins every convention (signs, ratios, clamps) used below.

## Global Constraints

- **Implementer agents make edits only — never run `cargo` (build/test/clippy/fmt).** Verification is one batched gate pass run by the orchestrator after all parallel tasks land (project rule: long builds, no concurrent cargo).
- Tasks 1–3 touch disjoint files and run in parallel; the **Interfaces** blocks are contracts — use the exact names/signatures written there, even though the other files don't exist in your view yet.
- AGENTS.md standards apply: `///` rustdoc on every public item; inline `//` explaining math terms; no `unwrap()`/`expect()` outside tests; no per-frame heap allocation (fixed stack buffers); no `as` numeric casts where `From`/`TryFrom` works (existing `#[allow]`'d count-to-f32 average is the sanctioned exception pattern); tests in `#[cfg(test)] mod tests` at file footer.
- Clippy runs `-D warnings` with `--all-targets --all-features`. Doc build runs `-D warnings` (no broken intra-doc links).
- Zoom convention: hands spreading **apart** = zoom **in** (distance shrinks). Pan convention: content **follows** the hands (grab metaphor). Window-logical pixels are top-left origin, +y down.
- Constants: `PAN_MAX_RADIUS = 2.0`, `FOVY = 60°`, spread guard `.max(1.0)` px, distance clamp `[0.1, 8.0]` (existing `MIN_DISTANCE`/`MAX_DISTANCE`). Home return: dt-correct exponential ease, `alpha = 1 - exp(-dt / camera_return_seconds)`, default time constant 8 s; `azimuth` exempt.
- Commits happen at the batched-gate checkpoints (orchestrator), messages via `git commit -F <file>` (backticks in `-m` get shell-substituted).

---

### Task 1: `FlameCamera` pan target + zoom/pan methods + settle-to-home ease

**Files:**
- Modify: `crates/wc-sketches/src/flame/systems/camera.rs` (whole file is in scope; ~238 lines today)

**Interfaces:**
- Consumes: `crate::flame::systems::hands::FlameGrabState` (exists today; you only read its existing `grabbing_count: usize` field) and the existing `FlameSettings` resource param, whose new `camera_return_seconds: f32` field Task 3 adds (default 8.0) — code against that name.
- Produces (Task 2 calls these — exact signatures):
  - `pub target: Vec3` field on `FlameCamera` (default `Vec3::ZERO`)
  - `pub fn set_distance_clamped(&mut self, distance: f32)`
  - `pub fn pan_by_pixels(&mut self, delta_px: Vec2, window_height: f32, sensitivity: f32)`
  - `pub fn ease_toward_home(&mut self, alpha: f32)`

- [ ] **Step 1: Write the new/extended tests first** (file footer `mod tests`; they will fail to compile until Step 2 — that is the red phase; do not run cargo):

```rust
    /// Pan moves the target opposite the hand delta's screen-right direction
    /// (content follows the hands), scaled by distance and fovy.
    #[test]
    fn pan_by_pixels_moves_target_left_when_hands_move_right() {
        let mut cam = FlameCamera::default(); // azimuth 0: eye on +Z, right = +X
        cam.pan_by_pixels(Vec2::new(10.0, 0.0), 720.0, 1.0);
        assert!(cam.target.x < 0.0, "target must move -X, got {}", cam.target.x);
        assert!(cam.target.y.abs() < 1e-6);
        assert!(cam.target.z.abs() < 1e-6);
    }

    /// Hands moving down (window +y) aim the camera up: target moves toward
    /// world +Y (camera-up at the default pose has positive Y).
    #[test]
    fn pan_by_pixels_moves_target_up_when_hands_move_down() {
        let mut cam = FlameCamera::default();
        cam.pan_by_pixels(Vec2::new(0.0, 10.0), 720.0, 1.0);
        assert!(cam.target.y > 0.0, "target must gain +Y, got {}", cam.target.y);
    }

    /// The pan target never leaves the PAN_MAX_RADIUS ball.
    #[test]
    fn pan_by_pixels_clamps_target_radius() {
        let mut cam = FlameCamera::default();
        for _ in 0..100 {
            cam.pan_by_pixels(Vec2::new(500.0, 300.0), 720.0, 1.0);
        }
        assert!(cam.target.length() <= PAN_MAX_RADIUS + 1e-4);
    }

    /// Sensitivity 0 disables pan entirely.
    #[test]
    fn pan_by_pixels_sensitivity_zero_is_inert() {
        let mut cam = FlameCamera::default();
        cam.pan_by_pixels(Vec2::new(50.0, 50.0), 720.0, 0.0);
        assert_eq!(cam.target, Vec3::ZERO);
    }

    /// set_distance_clamped enforces the v4 OrbitControls bounds.
    #[test]
    fn set_distance_clamped_enforces_bounds() {
        let mut cam = FlameCamera::default();
        cam.set_distance_clamped(0.001);
        assert!((cam.distance - 0.1).abs() < 1e-6);
        cam.set_distance_clamped(100.0);
        assert!((cam.distance - 8.0).abs() < 1e-6);
        cam.set_distance_clamped(1.5);
        assert!((cam.distance - 1.5).abs() < 1e-6);
    }

    /// The settle-to-home ease converges polar/distance/target back to the
    /// default pose while leaving azimuth alone (autorotate owns it).
    #[test]
    fn ease_toward_home_converges_pose_and_exempts_azimuth() {
        let home = FlameCamera::default();
        let mut cam = FlameCamera {
            azimuth: 2.7,
            polar: 3.0,
            distance: 7.5,
            target: Vec3::new(1.5, -0.5, 1.0),
            ..FlameCamera::default()
        };
        let dt = 1.0_f32 / 60.0;
        // 8s time constant, simulated for 60s: deviation shrinks by e^-7.5.
        let alpha = 1.0 - (-dt / 8.0_f32).exp();
        for _ in 0..3600 {
            cam.ease_toward_home(alpha);
        }
        assert!((cam.polar - home.polar).abs() < 1e-2);
        assert!((cam.distance - home.distance).abs() < 1e-2);
        assert!(cam.target.length() < 1e-2);
        assert!((cam.azimuth - 2.7).abs() < 1e-6, "azimuth must be exempt");
    }

    /// A single ease step moves each regressing channel strictly toward home.
    #[test]
    fn ease_toward_home_single_step_moves_toward_home() {
        let home = FlameCamera::default();
        let mut cam = FlameCamera {
            polar: home.polar + 1.0,
            distance: home.distance + 3.0,
            target: Vec3::new(1.0, 0.0, 0.0),
            ..FlameCamera::default()
        };
        cam.ease_toward_home(0.1);
        assert!((cam.polar - home.polar - 0.9).abs() < 1e-5);
        assert!((cam.distance - home.distance - 2.7).abs() < 1e-5);
        assert!((cam.target.x - 0.9).abs() < 1e-5);
    }
```

Also extend the existing `matrices_are_finite` test's loop body to set
`target: Vec3::new(1.5, -0.8, 0.4)` on the constructed camera (add the field to the
struct literal), keeping the existing polar sweep.

- [ ] **Step 2: Implement.** Add constants next to the existing ones (each with an
AGENTS-style `///` doc explaining what the number means):

```rust
/// Vertical field of view shared by [`FlameCamera::clip_from_view`] and the
/// pan math in [`FlameCamera::pan_by_pixels`] (v4 camera: 60 degrees).
const FOVY: f32 = std::f32::consts::PI / 3.0;
/// Maximum distance the pan target may wander from the origin: keeps the
/// fractal recoverable in-frame no matter how far a two-hand drag runs.
const PAN_MAX_RADIUS: f32 = 2.0;
```

Struct/Default changes: add `pub target: Vec3` (rustdoc: orbit/look-at center in
model-world space, `Vec3::ZERO` = v4's fixed origin; written only by
`pan_by_pixels` and the screensaver recenter) and `target: Vec3::ZERO` in `Default`.

`eye()` becomes `self.target + self.distance * Vec3::new(...)` (same spherical
terms). `view_from_model()` looks at `self.target` instead of `Vec3::ZERO`.
`clip_from_view` uses `FOVY` instead of the inline `60.0_f32.to_radians()`.

New methods on `impl FlameCamera` (public API section, above the private
helpers):

```rust
    /// Set the orbit radius, clamped to v4's `OrbitControls` bounds
    /// `[MIN_DISTANCE, MAX_DISTANCE]` — the single write path shared by wheel
    /// zoom and the two-hand spread zoom.
    pub fn set_distance_clamped(&mut self, distance: f32) {
        self.distance = distance.clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// Translate the pan `target` by a window-pixel delta (top-left origin,
    /// +y down), so on-screen content follows the hands (grab metaphor):
    /// hands moving right pan the camera left, hands moving down aim it up.
    ///
    /// `world_per_pixel = 2 * distance * tan(fovy/2) / window_height` is the
    /// world-space width of one pixel at the target plane, so pan speed
    /// matches apparent on-screen hand speed at any zoom. The target is
    /// clamped to [`PAN_MAX_RADIUS`] so the fractal can always be recovered.
    pub fn pan_by_pixels(&mut self, delta_px: Vec2, window_height: f32, sensitivity: f32) {
        // Unit vector from target toward the eye (the spherical terms are
        // already normalized); forward is its negation.
        let toward_eye = Vec3::new(
            self.polar.sin() * self.azimuth.sin(),
            self.polar.cos(),
            self.polar.sin() * self.azimuth.cos(),
        );
        let forward = -toward_eye;
        // look_at_rh basis: right = forward x up_world, up = right x forward.
        // The polar clamp keeps forward off the poles, so the cross products
        // stay well-conditioned.
        let right = forward.cross(Vec3::Y).normalize();
        let up = right.cross(forward);
        let world_per_pixel = 2.0 * self.distance * (FOVY * 0.5).tan() / window_height.max(1.0);
        let motion = (-right * delta_px.x + up * delta_px.y) * world_per_pixel * sensitivity;
        self.target = (self.target + motion).clamp_length_max(PAN_MAX_RADIUS);
    }

    /// Ease `polar`, `distance`, and the pan `target` toward the default
    /// (v4 start) pose by `alpha` — the settle-to-home restoration that
    /// guarantees no gesture leaves the kiosk in a permanently ugly state
    /// (Dots' `fabric_tension` home-spring is the same idea for particles).
    /// `azimuth` is deliberately exempt: autorotate owns it, and every
    /// azimuth is an equally valid view of the fractal.
    pub fn ease_toward_home(&mut self, alpha: f32) {
        let home = Self::default();
        self.polar += (home.polar - self.polar) * alpha;
        self.distance += (home.distance - self.distance) * alpha;
        // Home target is the origin, so the lerp reduces to a scale.
        self.target *= 1.0 - alpha;
    }
```

`update_flame_camera` changes:
1. Add system param `grab: Res<'_, FlameGrabState>` (import
   `crate::flame::systems::hands::FlameGrabState` — same-crate sibling module,
   the reverse of hands.rs's existing `camera::FlameCamera` import).
2. Refactor the wheel-zoom block onto the new method:

```rust
    if scroll.delta.y != 0.0 {
        let zoomed = camera.distance * (1.0 - ZOOM_SENSITIVITY * scroll.delta.y);
        camera.set_distance_clamped(zoomed);
    }
```

3. After the fling block, before the polar clamp:

```rust
    // Settle-to-home: whenever nothing actively holds the camera (no hand
    // grabbing, no mouse drag), polar/distance/target ease back to the v4
    // start pose so the kiosk always recovers from any gesture. The ease is
    // dt-correct (`1 - exp(-dt/tau)`), gentle enough to coexist with a
    // decaying fling, and also runs during the screensaver — that is what
    // recenters an abandoned pan for attract mode.
    if grab.grabbing_count == 0 && camera.last_drag.is_none() {
        // `.max(0.1)` guards a hand-edited settings file against div-by-zero.
        let alpha = 1.0 - (-dt / settings.camera_return_seconds.max(0.1)).exp();
        camera.ease_toward_home(alpha);
    }
```

4. Update the module doc comment (`//!`) and the system's `///` doc to mention
   pan target + settle-to-home. Never strip existing comments; extend them.

- [ ] **Step 3: Self-check compile plausibility only** (read your edits over; no cargo). Report the exact public surface you produced.

---

### Task 2: two-hand branch in `step_grab` + gather spread + world test

**Files:**
- Modify: `crates/wc-sketches/src/flame/systems/hands.rs` (whole file in scope; ~375 lines today)

**Interfaces:**
- Consumes (produced by Task 1, exact signatures — code against them even though your view of `camera.rs` predates them):
  - `FlameCamera.target: Vec3`, `FlameCamera::set_distance_clamped(&mut self, f32)`, `FlameCamera::pan_by_pixels(&mut self, delta_px: Vec2, window_height: f32, sensitivity: f32)`
- Consumes (produced by Task 3): `FlameSettings.two_hand_zoom_gamma: f32`, `FlameSettings.hand_pan_sensitivity: f32` (both default 1.0; `FlameSettings::default()` exists — see the settings file's own tests for precedent).
- Produces: `pub(crate) struct GrabGather { pub centroid: Vec2, pub count: usize, pub spread: f32 }`; `gather_grabbing(&[(Vec2, f32)]) -> Option<GrabGather>` replacing `average_grabbing`; `step_grab(state, camera, gather: Option<GrabGather>, window: Vec2, zoom_gamma: f32, pan_sensitivity: f32)`. `FlameGrabState` gains `pub anchor_spread: f32` and `pub anchor_distance: f32` (serde-free resource; `Default` derive still applies). No consumer outside this file reads the new fields (`update_flame_sim` reads only `warp_px`/`grabbing_count`, which keep their exact semantics).

- [ ] **Step 1: Write/adjust tests first** (red phase; no cargo). Port the six existing `step_grab`/`average_grabbing` tests to the new signatures mechanically (an `avg`+`count` pair becomes `Some(GrabGather { centroid, count, spread: 0.0 })`; pass `1.0, 1.0` for the new gamma/sensitivity params), then add:

```rust
    /// Spread is the distance between the first two grabbing hands.
    #[test]
    fn gather_spread_is_distance_between_first_two_grabbing() {
        let samples = [
            (Vec2::new(100.0, 300.0), 0.9),
            (Vec2::new(400.0, 300.0), 0.8),
        ];
        let gather = gather_grabbing(&samples).expect("two grabbing hands");
        assert_eq!(gather.count, 2);
        assert!((gather.spread - 300.0).abs() < 1e-4);
        assert_eq!(gather.centroid, Vec2::new(250.0, 300.0));
    }

    /// Engaging a second hand stashes the zoom anchors and moves nothing.
    #[test]
    fn two_hand_engage_stashes_anchors_and_moves_nothing() {
        let mut state = FlameGrabState { grabbing_count: 1, ..FlameGrabState::default() };
        let mut camera = FlameCamera::default();
        let d0 = camera.distance;
        let gather = GrabGather { centroid: Vec2::new(640.0, 360.0), count: 2, spread: 400.0 };

        step_grab(&mut state, &mut camera, Some(gather), WINDOW, 1.0, 1.0);

        assert_eq!(state.grabbing_count, 2);
        assert!((state.anchor_spread - 400.0).abs() < 1e-6);
        assert!((state.anchor_distance - d0).abs() < 1e-6);
        assert!((camera.distance - d0).abs() < 1e-6, "no zoom on the stash frame");
        assert_eq!(camera.target, Vec3::ZERO, "no pan on the stash frame");
    }

    /// Steady two-hand: spreading apart zooms in (distance shrinks by the
    /// inverse spread ratio); squeezing zooms out; gamma exponentiates.
    #[test]
    fn two_hand_spread_ratio_drives_distance() {
        let mut state = FlameGrabState {
            grabbing_count: 2,
            last: Vec2::new(640.0, 360.0),
            anchor_spread: 400.0,
            anchor_distance: 2.0,
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera { distance: 2.0, ..FlameCamera::default() };
        let apart = GrabGather { centroid: Vec2::new(640.0, 360.0), count: 2, spread: 500.0 };
        step_grab(&mut state, &mut camera, Some(apart), WINDOW, 1.0, 1.0);
        assert!((camera.distance - 2.0 * (400.0 / 500.0)).abs() < 1e-5);

        let together = GrabGather { centroid: Vec2::new(640.0, 360.0), count: 2, spread: 200.0 };
        step_grab(&mut state, &mut camera, Some(together), WINDOW, 1.0, 1.0);
        assert!((camera.distance - 2.0 * (400.0 / 200.0)).abs() < 1e-5);

        // gamma = 2 squares the ratio (anchor-based, so it replaces, not compounds).
        let apart2 = GrabGather { centroid: Vec2::new(640.0, 360.0), count: 2, spread: 800.0 };
        step_grab(&mut state, &mut camera, Some(apart2), WINDOW, 2.0, 1.0);
        assert!((camera.distance - 2.0 * (400.0_f32 / 800.0).powi(2)).abs() < 1e-5);
    }

    /// Steady two-hand midpoint drag pans (target moves) and never orbits:
    /// azimuth/polar hold still and angular momentum stays zeroed.
    #[test]
    fn two_hand_midpoint_drag_pans_without_orbiting() {
        let mut state = FlameGrabState {
            grabbing_count: 2,
            last: Vec2::new(640.0, 360.0),
            anchor_spread: 400.0,
            anchor_distance: 0.7826,
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera {
            angular_velocity: Vec2::new(0.2, 0.1), // stale fling must be suppressed
            ..FlameCamera::default()
        };
        let az0 = camera.azimuth;
        let polar0 = camera.polar;
        let gather = GrabGather { centroid: Vec2::new(660.0, 360.0), count: 2, spread: 400.0 };

        step_grab(&mut state, &mut camera, Some(gather), WINDOW, 1.0, 1.0);

        assert!(camera.target.x < 0.0, "content follows hands: +x drag pans target -X");
        assert!((camera.azimuth - az0).abs() < 1e-6);
        assert!((camera.polar - polar0).abs() < 1e-6);
        assert_eq!(camera.angular_velocity, Vec2::ZERO);
        assert_eq!(state.last, gather.centroid);
        assert_eq!(state.warp_px, gather.centroid + state.mouse_offset, "warp still tracks the midpoint");
    }

    /// Releasing straight out of two-hand mode leaves no fling.
    #[test]
    fn two_hand_release_leaves_no_fling() {
        let mut state = FlameGrabState {
            grabbing_count: 2,
            last: Vec2::new(640.0, 360.0),
            anchor_spread: 400.0,
            anchor_distance: 1.0,
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera::default();
        let gather = GrabGather { centroid: Vec2::new(700.0, 400.0), count: 2, spread: 420.0 };
        step_grab(&mut state, &mut camera, Some(gather), WINDOW, 1.0, 1.0);
        step_grab(&mut state, &mut camera, None, WINDOW, 1.0, 1.0);
        assert_eq!(state.grabbing_count, 0);
        assert_eq!(camera.angular_velocity, Vec2::ZERO, "no fling out of two-hand mode");
    }

    /// Dropping from two hands to one re-stashes (no jump) and then resumes
    /// normal one-hand orbit on the following steady frame.
    #[test]
    fn two_to_one_transition_restashes_then_orbits() {
        let mut state = FlameGrabState {
            grabbing_count: 2,
            last: Vec2::new(640.0, 360.0),
            anchor_spread: 400.0,
            anchor_distance: 1.0,
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera::default();
        let az0 = camera.azimuth;

        let one = GrabGather { centroid: Vec2::new(300.0, 200.0), count: 1, spread: 0.0 };
        step_grab(&mut state, &mut camera, Some(one), WINDOW, 1.0, 1.0);
        assert!((camera.azimuth - az0).abs() < 1e-6, "transition frame must not jump");

        let moved = GrabGather { centroid: Vec2::new(320.0, 200.0), count: 1, spread: 0.0 };
        step_grab(&mut state, &mut camera, Some(moved), WINDOW, 1.0, 1.0);
        assert!((camera.azimuth - az0).abs() > 1e-6, "steady one-hand frame orbits again");
    }

    /// World-level: two grabbing hands spreading apart zoom the camera in.
    #[test]
    fn update_flame_hands_two_hands_spreading_zooms_in() {
        let mut world = World::new();
        world.insert_resource(FlameCamera::default());
        world.insert_resource(FlameGrabState::default());
        world.insert_resource(FlameSettings::default());
        world.spawn(Window::default());
        let left = world
            .spawn((
                TrackedHand,
                Transform::default(),
                Visibility::default(),
                PalmPosition(Vec3::new(-50.0, 195.0, 200.0)),
                GrabStrength(0.9),
            ))
            .id();
        let right = world
            .spawn((
                TrackedHand,
                Transform::default(),
                Visibility::default(),
                PalmPosition(Vec3::new(50.0, 195.0, 200.0)),
                GrabStrength(0.9),
            ))
            .id();

        // Frame 1: engage (stash anchors, no movement yet).
        world.run_system_once(update_flame_hands).expect("engage frame");
        let d0 = world.resource::<FlameCamera>().distance;
        assert_eq!(world.resource::<FlameGrabState>().grabbing_count, 2);

        // Frame 2: hands spread apart symmetrically -> zoom in.
        world.entity_mut(left).insert(PalmPosition(Vec3::new(-120.0, 195.0, 200.0)));
        world.entity_mut(right).insert(PalmPosition(Vec3::new(120.0, 195.0, 200.0)));
        world.run_system_once(update_flame_hands).expect("spread frame");
        let d1 = world.resource::<FlameCamera>().distance;
        assert!(d1 < d0, "spreading apart must zoom in: {d1} !< {d0}");
    }
```

(`WINDOW` is the existing test const. Keep every existing test, ported, plus these.)

- [ ] **Step 2: Implement.**

Replace `average_grabbing` with `gather_grabbing` (keep the `#[allow]`'d
count-to-f32 pattern and its `reason`):

```rust
/// One frame's gathered grabbing-hand geometry: hands whose grab strength
/// clears [`GRAB_THRESHOLD`] contribute to the centroid; the first two also
/// define `spread` (`MAX_HANDS` is 2 upstream, so "first two" is exhaustive).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GrabGather {
    /// Mean window-logical position of all grabbing hands (for two hands,
    /// the midpoint the pan gesture drags).
    pub centroid: Vec2,
    /// Number of grabbing hands (drives the 0/1/2 interaction mode).
    pub count: usize,
    /// Window-pixel distance between the first two grabbing hands; `0.0`
    /// when fewer than two grab.
    pub spread: f32,
}

/// Gather the grabbing hands out of this frame's samples. Returns `None`
/// when no hand clears [`GRAB_THRESHOLD`].
pub(crate) fn gather_grabbing(samples: &[(Vec2, f32)]) -> Option<GrabGather> {
    let mut sum = Vec2::ZERO;
    let mut count = 0_usize;
    let mut first_two = [Vec2::ZERO; 2];
    for &(position, grab) in samples {
        if grab > GRAB_THRESHOLD {
            if count < 2 {
                first_two[count] = position;
            }
            sum += position;
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    #[allow(
        clippy::as_conversions,
        clippy::cast_precision_loss,
        reason = "hand count is small and bounded (<= MAX_GRAB_SAMPLES)"
    )]
    let n = count as f32;
    let spread = if count >= 2 {
        first_two[0].distance(first_two[1])
    } else {
        0.0
    };
    Some(GrabGather { centroid: sum / n, count, spread })
}
```

`FlameGrabState` gains (doc every field per AGENTS):

```rust
    /// Inter-hand spread (window px, floored to 1.0) stashed when the second
    /// hand engaged — the denominator anchor of the zoom ratio.
    pub anchor_spread: f32,
    /// Camera distance stashed when the second hand engaged — the numerator
    /// anchor of the zoom ratio (anchor-based zoom accumulates no drift).
    pub anchor_distance: f32,
```

`step_grab` becomes (update its `///` doc to describe all three modes; keep the
v4-lineage references for the one-hand branch):

```rust
pub(crate) fn step_grab(
    state: &mut FlameGrabState,
    camera: &mut FlameCamera,
    gather: Option<GrabGather>,
    window: Vec2,
    zoom_gamma: f32,
    pan_sensitivity: f32,
) {
    let Some(gather) = gather else {
        state.grabbing_count = 0;
        return;
    };

    if state.grabbing_count != gather.count {
        state.mouse_offset = state.warp_px - gather.centroid;
        state.last = gather.centroid;
        camera.angular_velocity = Vec2::ZERO;
        state.grabbing_count = gather.count;
        if gather.count >= 2 {
            // The `.max(1.0)` px floor guards the ratio against a degenerate
            // zero spread (overlapping palms).
            state.anchor_spread = gather.spread.max(1.0);
            state.anchor_distance = camera.distance;
        }
        return;
    }

    if gather.count >= 2 {
        // Two-hand mode: the spread ratio drives zoom (anchor-based, so
        // returning the hands returns the distance — no drift), the midpoint
        // delta drives pan, and orbit momentum stays suppressed so releasing
        // out of a zoom never flings.
        let ratio = state.anchor_spread / gather.spread.max(1.0);
        camera.set_distance_clamped(state.anchor_distance * ratio.powf(zoom_gamma));
        camera.pan_by_pixels(gather.centroid - state.last, window.y, pan_sensitivity);
        camera.angular_velocity = Vec2::ZERO;
    } else {
        // One-hand mode: v4's grab-orbit, per-axis delta (unlike the mouse
        // drag's uniform /height split in `update_flame_camera`).
        let delta = (gather.centroid - state.last) / window * TAU;
        camera.azimuth -= delta.x;
        camera.polar -= delta.y;
        camera.angular_velocity = camera.angular_velocity * 0.7 + delta * 0.3;
    }
    state.last = gather.centroid;
    state.warp_px = gather.centroid + state.mouse_offset;
}
```

`update_flame_hands`: add `settings: Res<'_, FlameSettings>` (import
`crate::flame::settings::FlameSettings`); the tail becomes:

```rust
    let gather = gather_grabbing(&samples[..n]);
    step_grab(
        &mut grab_state,
        &mut camera,
        gather,
        window_size,
        settings.two_hand_zoom_gamma,
        settings.hand_pan_sensitivity,
    );
```

Update the module `//!` doc's data-flow paragraph to cover the two-hand mode
(zoom via spread ratio, pan via midpoint, warp still tracks the centroid).

- [ ] **Step 3: Self-check by reading your edits; report the final `step_grab` signature and any deviation.** No cargo.

---

### Task 3: settings knobs

**Files:**
- Modify: `crates/wc-sketches/src/flame/settings.rs`

**Interfaces:**
- Produces on `FlameSettings`: `pub two_hand_zoom_gamma: f32` (default 1.0) and `pub hand_pan_sensitivity: f32` (default 1.0) — Task 2 reads both; `pub camera_return_seconds: f32` (default 8.0) — Task 1 reads it. Exactly these names.

- [ ] **Step 1: Add the three fields**, copying `autorotate_speed`'s attribute shape exactly (same `#[setting(...)]` key spelling and section value as that field; if `autorotate_speed` is `category = User`, look at any existing `category = Dev` field in the same file for the category spelling). Field definitions:
  - `two_hand_zoom_gamma: f32` — `default = 1.0, min = 0.25, max = 3.0, step = 0.05`, label `"Two-hand zoom gamma"`, **Dev** category, same section as `autorotate_speed`. Rustdoc: `/// Exponent on the two-hand spread ratio driving hand zoom: 1 is proportional; higher values zoom more aggressively per hand movement.`
  - `hand_pan_sensitivity: f32` — `default = 1.0, min = 0.0, max = 3.0, step = 0.05`, label `"Hand pan sensitivity"`, **Dev** category, same section. Rustdoc: `/// Scale on the two-hand midpoint drag that pans the view; 0 disables pan.`
  - `camera_return_seconds: f32` — `default = 8.0, min = 0.5, max = 60.0, step = 0.5`, label `"Camera return time"`, **Dev** category, same section; add `unit = "s"` only if other fields in the file use a `unit` attribute (copy their spelling). Rustdoc: `/// Exponential time constant of the settle-to-home camera ease: seconds to recover ~63% of the deviation after the user lets go. The ease keeps polar/distance/pan from being stranded in an ugly pose (azimuth is exempt; autorotate owns it).`
- [ ] **Step 2: Add the matching serde default fns** (`default_two_hand_zoom_gamma` and `default_hand_pan_sensitivity` returning `1.0`; `default_camera_return_seconds` returning `8.0`) next to the existing `default_*` fns, and wire `#[serde(default = "...")]` on each field — the file header documents that the two must stay in sync.
- [ ] **Step 3: Extend the `default_values_match_serde_defaults` test** (and any other exhaustive field-list test in the file) with the three new fields, following the existing entries' exact pattern.
- [ ] **Step 4: Self-check by reading your edits; report exact field names/defaults.** No cargo.

---

### CHECKPOINT A (orchestrator, after Tasks 1–3 land)

- [ ] `cargo fmt --all`
- [ ] `cargo clippy --all-targets --all-features --workspace -- -D warnings`
- [ ] `cargo nextest run --workspace --all-features` (plus `cargo test --doc --workspace`)
- [ ] `cargo doc --no-deps --workspace --document-private-items` with `RUSTDOCFLAGS="-D warnings"`
- [ ] `cargo xtask check-secrets`
- [ ] Fix anything small inline; commit `feat(flame): two-hand grab zoom + pan camera gestures` via `-F`.

---

### Task 4: debug pose toggle + capture scenario

**Files:**
- Modify: `crates/wc-core/src/debug/mod.rs` (new toggle; follow the `force_flame_warp` field/parse/doc/test pattern at ~lines 70-73/104/249-253)
- Modify: `crates/wc-sketches/src/flame/systems/camera.rs` (consume the toggle at the very end of `update_flame_camera`, after the polar clamp, mirroring how `crates/wc-sketches/src/flame/systems/sim_params.rs:152-156` consumes `force_flame_warp` — same access idiom, same `#[cfg(debug_assertions)]` gating as that precedent)
- Modify: `tests/visual/scenarios.toml` (new scenario cloned from the `flame-warp` block at ~lines 221-231)

**Interfaces:**
- Consumes: Task 1's `FlameCamera.target`.
- Produces: env toggle `WC_DEBUG_FORCE_FLAME_CAMERA_POSE`; capture scenario `flame-camera-pose`.

- [ ] **Step 1:** Add `force_flame_camera_pose: bool` to `DebugToggles` with doc ("pins a deterministic non-default Flame camera pose — zoomed in, panned off-center — so captures regression-guard the target-aware view matrix"), parse `WC_DEBUG_FORCE_FLAME_CAMERA_POSE`, extend the toggles' parse test.
- [ ] **Step 2:** In `update_flame_camera`, after the polar clamp, when the toggle is set, pin: `azimuth = 0.9`, `polar = 1.1`, `distance = 0.35`, `target = Vec3::new(0.2, 0.0, 0.1)`, `angular_velocity = Vec2::ZERO` (comment: deterministic capture pose; overrides all interaction each frame).
- [ ] **Step 3:** Add the `flame-camera-pose` scenario to `scenarios.toml`: copy `flame-warp`'s block (same sketch/frame settings), set the env to `WC_DEBUG_FORCE_FLAME_CAMERA_POSE = "1"` (keeping any base env the block requires), description "Flame with a pinned zoomed/panned camera pose (two-hand gesture regression guard)". Read `tests/visual/CLAUDE.md` first and follow its add-a-scenario instructions, including baseline notes.
- [ ] **Step 4:** Self-check by reading your edits. No cargo, no capture runs (orchestrator handles baselines).

---

### CHECKPOINT B (orchestrator, after Task 4)

- [ ] Re-run the Checkpoint A gate list.
- [ ] `cargo xtask capture flame-camera-pose --update-baseline` (or the exact baseline flow `tests/visual/CLAUDE.md` prescribes); review the PNG by eye — expect an off-center, zoomed-in flame. If frames come back all-black, check a known-good scenario first (documented environment issue when the window isn't foregrounded) and leave the baseline for Madison with a note.
- [ ] Commit `test(flame): pinned-camera-pose capture scenario + debug toggle` via `-F`.

---

### Task 5 (orchestrator): docs + review + wrap-up

- [ ] Add a "Post-parity additions" note to `crates/wc-sketches/src/flame/PARITY.md`: two-hand spread zoom + midpoint pan (spec link), operator checklist item — feel-tune `two_hand_zoom_gamma` / `hand_pan_sensitivity` with real hands (`cargo rund`, flip the settings panel's ADVANCED toggle to see Dev knobs).
- [ ] Run the code-review skill over the full diff; apply confirmed findings.
- [ ] Final gate pass; commit docs via `-F`.

## Self-Review (done at authoring time)

- Spec coverage: interaction table → Tasks 1-2; settings → Task 3; debug/capture → Task 4; docs/manual-feel sign-off → Task 5. Unaffected-consumers section requires no task (verified read-only).
- Placeholder scan: none (every code step carries the code; Task 3/4 "copy the precedent's shape" items name the exact precedent file/lines because attribute spellings must be copied from the file, not invented here).
- Type consistency: `GrabGather`/`pan_by_pixels`/`set_distance_clamped`/`two_hand_zoom_gamma`/`hand_pan_sensitivity` spellings match across Tasks 1-4 and the spec.
