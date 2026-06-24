//! Two-centre interaction state machine, ported from v4 `index.ts::step()`.
//!
//! Pure [`step_centers`] (unit-tested, no Bevy dependencies) advances the CPU
//! state one frame given pointer and hand-grab input. [`update_cymatics_centers`]
//! is the thin Bevy system wrapper that reads input resources and calls it.
//!
//! ## Y-convention
//!
//! All sim UV coordinates are top-left origin (Bevy-native, `[0,1]` with
//! `y=0` at the top). Bevy's cursor position is also top-left, so
//! [`screen_to_sim_uv`] normalises without flipping Y. v4's GLSL used
//! bottom-left origin; that `y = 1 - y` flip is absent here.
//!
//! ## Hands
//!
//! The `c1_held`/`c2_held` flags come from [`super::hand::CymaticsHandGrabs`]
//! (Task C10). Until C10 lands both slots are `None` and only mouse/touch
//! drives the centres.

use bevy::prelude::*;
#[cfg(debug_assertions)]
use wc_core::debug::DebugToggles;
use wc_core::input::pointer::PointerState;
use wc_core::settings::EguiPointerCaptured;

use crate::cymatics::settings::CymaticsSettings;
use crate::cymatics::CymaticsState;

// ---------------------------------------------------------------------------
// v4 module constants (verbatim — do not adjust)
// ---------------------------------------------------------------------------

/// Resting alive-mask radius floor (v4 `MINIMUM_ACTIVE_RADIUS`).
pub const MINIMUM_ACTIVE_RADIUS: f32 = 0.1;

/// Alive-mask radius floor while interacting (v4 `MINIMUM_ACTIVE_RADIUS_INTERACTING`).
/// The radius is clamped to at least this value the moment interaction begins.
pub const MINIMUM_ACTIVE_RADIUS_INTERACTING: f32 = 0.5;

/// Alive-mask radius target while interacting (v4 `TARGET_ACTIVE_RADIUS_INTERACTING`).
/// The radius lerps toward this value each frame that interaction is active.
pub const TARGET_ACTIVE_RADIUS_INTERACTING: f32 = 7.5;

/// Per-frame lerp factor for radius growth toward `TARGET_ACTIVE_RADIUS_INTERACTING`
/// (v4 `ACTIVE_RADIUS_INTERACTING_GROW_FACTOR`).
pub const ACTIVE_RADIUS_INTERACTING_GROW_FACTOR: f32 = 0.01;

/// Per-frame lerp factor for radius decay toward `MINIMUM_ACTIVE_RADIUS` when idle
/// (v4 `ACTIVE_RADIUS_IDLE_DECAY_FACTOR`).
pub const ACTIVE_RADIUS_IDLE_DECAY_FACTOR: f32 = 0.005;

/// Per-frame lerp factor for centre-position tracking (v4
/// `INTERACTION_CENTER_LERP_FACTOR`).
pub const INTERACTION_CENTER_LERP_FACTOR: f32 = 0.01;

/// Default `num_cycles` when at rest (v4 `DEFAULT_NUM_CYCLES`).
pub const DEFAULT_NUM_CYCLES: f32 = 1.002;

// ---------------------------------------------------------------------------
// Tunable interaction parameters
// ---------------------------------------------------------------------------

/// Live-tunable interaction parameters sourced from [`CymaticsSettings`].
///
/// Default values match the v4 constants so the sketch behaves identically
/// when no override is set. Passed into [`step_centers`] each frame instead
/// of referencing the module constants directly, making the parameters
/// adjustable from the Dev settings panel without a restart.
#[derive(Clone, Copy, Debug)]
pub struct CenterTuning {
    /// Resting alive-mask radius floor. Matches `MINIMUM_ACTIVE_RADIUS`.
    pub min_radius: f32,
    /// Radius floor on interaction onset. Matches `MINIMUM_ACTIVE_RADIUS_INTERACTING`.
    pub interacting_radius: f32,
    /// Radius lerp target while interacting. Matches `TARGET_ACTIVE_RADIUS_INTERACTING`.
    pub target_radius: f32,
    /// Per-frame growth lerp factor toward `target_radius`. Matches
    /// `ACTIVE_RADIUS_INTERACTING_GROW_FACTOR`.
    pub grow_factor: f32,
    /// Per-frame decay lerp factor toward `min_radius` when idle. Matches
    /// `ACTIVE_RADIUS_IDLE_DECAY_FACTOR`.
    pub decay_factor: f32,
    /// Per-frame lerp factor for centre-position tracking. Matches
    /// `INTERACTION_CENTER_LERP_FACTOR`.
    pub lerp_factor: f32,
}

impl Default for CenterTuning {
    fn default() -> Self {
        Self {
            min_radius: MINIMUM_ACTIVE_RADIUS,
            interacting_radius: MINIMUM_ACTIVE_RADIUS_INTERACTING,
            target_radius: TARGET_ACTIVE_RADIUS_INTERACTING,
            grow_factor: ACTIVE_RADIUS_INTERACTING_GROW_FACTOR,
            decay_factor: ACTIVE_RADIUS_IDLE_DECAY_FACTOR,
            lerp_factor: INTERACTION_CENTER_LERP_FACTOR,
        }
    }
}

impl CenterTuning {
    /// Construct from live [`CymaticsSettings`].
    pub fn from_settings(s: &CymaticsSettings) -> Self {
        Self {
            min_radius: s.min_radius,
            interacting_radius: s.interacting_radius,
            target_radius: s.target_radius,
            grow_factor: s.grow_factor,
            decay_factor: s.decay_factor,
            lerp_factor: s.lerp_factor,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-frame input bundle
// ---------------------------------------------------------------------------

/// Per-frame interaction input passed to [`step_centers`].
///
/// Mouse/touch drives the unheld primary centre; the held flags and positions
/// come from [`super::hand::CymaticsHandGrabs`] (Task C10 wires these).
#[derive(Clone, Copy)]
pub struct CenterInput {
    /// True while the left mouse button or any touch contact is active.
    pub mouse_pressed: bool,
    /// Cursor position in sim UV `[0,1]`, top-left origin.
    pub mouse_uv: Vec2,
    /// True when the hand-tracking gesture holds the primary centre.
    pub c1_held: bool,
    /// Primary-centre position from the hand gesture, sim UV top-left origin.
    pub c1_uv: Vec2,
    /// True when the hand-tracking gesture holds the secondary centre.
    pub c2_held: bool,
    /// Secondary-centre position from the hand gesture, sim UV top-left origin.
    pub c2_uv: Vec2,
}

// ---------------------------------------------------------------------------
// Pure state machine (unit-tested, no Bevy ECS in scope)
// ---------------------------------------------------------------------------

/// True when the active-mask radius is at (or near) its resting floor — i.e.,
/// the wave field has fully decayed and the sketch may sleep.
///
/// v4 `isReadyToSleep`.
#[must_use]
pub fn is_ready_to_sleep(state: &CymaticsState) -> bool {
    state.active_radius <= MINIMUM_ACTIVE_RADIUS + 1e-2
}

/// Advance the two-centre state machine one logical frame.
///
/// Verbatim port of v4 `step()`. Takes the current state and a
/// [`CenterInput`] bundle, mutates `state` in place. No allocations; safe to
/// call from tests without a Bevy world.
///
/// ## What this updates
///
/// - `active_radius`: grows toward `TARGET_ACTIVE_RADIUS_INTERACTING` while
///   interacting; decays toward `MINIMUM_ACTIVE_RADIUS` while idle.
/// - `num_cycles`: increments slightly while interacting (v4 formula);
///   lerps back toward `DEFAULT_NUM_CYCLES` while idle.
/// - `center` / `center2`: held centres follow their hand position; free
///   centres mirror the opposite centre (or follow the mouse when both free).
/// - `center_speed`: scalar approximating the primary centre's displacement
///   this frame — used by the audio coupling (Task C11).
/// - `slow_down`: decays `×0.95` per frame; raised on interaction onset by
///   the audio coupling.
///
/// ## Does NOT advance `simulation_time`
///
/// That field is owned by `update_cymatics_sim_params` in `cymatics/mod.rs`.
/// Exactly one system advances the phase clock; this function is not it.
pub fn step_centers(state: &mut CymaticsState, input: CenterInput, tuning: CenterTuning) {
    let interacting = input.mouse_pressed || input.c1_held || input.c2_held;

    if interacting {
        // v4: numCycles increments per-frame while active, with a small
        // exponential acceleration term.
        state.num_cycles += 0.0003 + (state.num_cycles - DEFAULT_NUM_CYCLES) * 0.0008;
        // v4: snap the radius up to the interacting floor immediately, then
        // lerp toward the target so the mask expands smoothly.
        if state.active_radius < tuning.interacting_radius {
            state.active_radius = tuning.interacting_radius;
        }
        state.active_radius = lerp(
            state.active_radius,
            tuning.target_radius,
            tuning.grow_factor,
        );
    } else {
        // v4: radius decays geometrically toward the resting floor.
        state.active_radius = lerp(state.active_radius, tuning.min_radius, tuning.decay_factor);
        // v4: numCycles lerps back toward the resting default (×0.95 each frame).
        state.num_cycles = state.num_cycles * 0.95 + DEFAULT_NUM_CYCLES * 0.05;
    }

    // v4: the "wanted" position for the primary centre. A hand grab overrides
    // the mouse; otherwise the mouse cursor drives it.
    let wanted_c1 = if input.c1_held {
        input.c1_uv
    } else {
        input.mouse_uv
    };

    // Held centres follow their hand position (smooth lerp, not snap).
    if input.c1_held {
        state.center = lerp2(state.center, wanted_c1, tuning.lerp_factor);
    }
    if input.c2_held {
        state.center2 = lerp2(state.center2, input.c2_uv, tuning.lerp_factor);
    }

    // Free centres: mirror the other held centre, or follow the mouse when
    // both are free.
    if !input.c1_held {
        if input.c2_held {
            // v4: free c1 mirrors c2's current position across the UV centre.
            let mirror = Vec2::new(1.0 - state.center2.x, 1.0 - state.center2.y);
            state.center = lerp2(state.center, mirror, tuning.lerp_factor);
        } else {
            // v4: no hand grabs — primary centre follows the mouse/touch cursor.
            state.center = lerp2(state.center, wanted_c1, tuning.lerp_factor);
        }
    }
    if !input.c2_held {
        // v4: free c2 always mirrors c1 (which may just have been updated above).
        let mirror = Vec2::new(1.0 - state.center.x, 1.0 - state.center.y);
        state.center2 = lerp2(state.center2, mirror, tuning.lerp_factor);
    }

    // v4 `centerSpeed`: the primary centre's per-frame displacement estimate.
    // Used by the audio coupling (Task C11) as an excitation magnitude.
    // Formula: distance(wantedC1, c1) * lerpFactor.
    state.center_speed = wanted_c1.distance(state.center) * tuning.lerp_factor;

    // v4 `slowDownAmount`: decays ×0.95 per frame; the audio coupling raises it
    // on interaction onset to temporarily lower the effective cycle count.
    state.slow_down *= 0.95;
}

// ---------------------------------------------------------------------------
// Coordinate helpers
// ---------------------------------------------------------------------------

/// Map a screen-space NDC position to sim UV `[0, 1]` with aspect-ratio
/// correction.
///
/// `ndc` must be in Bevy-native convention: `(-1, -1)` = top-left,
/// `(1, 1)` = bottom-right (i.e. `ndc.y = cursor.y / win.y * 2 - 1`, no
/// Y-flip). The resulting UV is top-left origin, matching `simulate.wgsl` and
/// `render.wgsl`.
///
/// When `screen_ar == sim_ar` the output is `cursor / window_size`, a
/// straight normalise. The `screen_ar / sim_ar` ratio corrects for cases
/// where the sim grid has a different aspect than the window.
///
/// v4 `screenToSimUV` (adapted for top-left origin; the v4 version accepted
/// bottom-left NDC — the Y-flip now lives at the call site: absent).
#[must_use]
pub fn screen_to_sim_uv(ndc: Vec2, screen_ar: f32, sim_ar: f32) -> Vec2 {
    // sc is in [-0.5, 0.5]; adding 0.5 maps to [0, 1] UV.
    let sc = ndc * 0.5;
    Vec2::new(
        (sc.x * (screen_ar / sim_ar) + 0.5).clamp(0.0, 1.0),
        (sc.y + 0.5).clamp(0.0, 1.0),
    )
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
fn lerp2(a: Vec2, b: Vec2, t: f32) -> Vec2 {
    a + (b - a) * t
}

// ---------------------------------------------------------------------------
// Bevy system
// ---------------------------------------------------------------------------

/// Read pointer input and hand-grab state, then call [`step_centers`] to
/// advance [`CymaticsState`] for this frame.
///
/// Runs only while `sketch_active(AppState::Cymatics)` — screensaver mode
/// drives centres itself (Task C13) and must not race with this system.
///
/// ## Cursor convention (no Y-flip)
///
/// `PointerState::cursor` is Bevy's window-logical coordinate (top-left
/// origin, `y` increases downward). The NDC conversion therefore does NOT
/// flip Y; `screen_to_sim_uv` then maps that directly to a top-left UV,
/// matching the shader convention.
#[allow(
    clippy::too_many_arguments,
    reason = "Bevy system — each parameter is a distinct ECS resource; \
              the system cannot be split without losing the single \
              step_centers call-site guarantee. Mirrors hand/dots systems."
)]
pub fn update_cymatics_centers(
    mut state: ResMut<'_, CymaticsState>,
    window: Single<'_, '_, &Window>,
    hands: Res<'_, super::hand::CymaticsHandGrabs>,
    pointer: Res<'_, PointerState>,
    mouse_buttons: Res<'_, bevy::input::ButtonInput<bevy::input::mouse::MouseButton>>,
    touches: Res<'_, bevy::input::touch::Touches>,
    egui_captured: Option<Res<'_, EguiPointerCaptured>>,
    settings: Res<'_, CymaticsSettings>,
    // Optional debug toggles (present only when a `WC_DEBUG_*` var is set, and
    // only in debug builds). Placed last so the release signature is unchanged.
    #[cfg(debug_assertions)] debug_toggles: Option<Res<'_, DebugToggles>>,
) {
    let win = Vec2::new(window.width().max(1.0), window.height().max(1.0));
    let screen_ar = win.x / win.y;
    // Sim AR tracks the window AR (sim resolution = vy·aspect × vy).
    let sim_ar = screen_ar;

    // Gate mouse/touch presses on egui not having captured the pointer,
    // matching the pattern in `dots/systems/mouse.rs::update_dots_mouse_attractor`.
    // Position updates (not presses) are always applied so held-then-dragged
    // interactions release cleanly.
    let pointer_captured = egui_captured.is_some_and(|c| c.0);
    let mouse_held = mouse_buttons.pressed(bevy::input::mouse::MouseButton::Left)
        || touches.iter().next().is_some();
    let mouse_pressed = mouse_held && !pointer_captured;

    // Cursor → Bevy-native NDC (no Y-flip; `cursor` is already top-left).
    // ndc.x: left = -1, right = +1.
    // ndc.y: top = -1, bottom = +1  (Bevy window-logical direction).
    // `screen_to_sim_uv` maps this directly to top-left UV without further
    // flipping.
    let mouse_uv = pointer.cursor.map_or(Vec2::new(0.5, 0.5), |p| {
        let ndc = Vec2::new(p.x / win.x * 2.0 - 1.0, p.y / win.y * 2.0 - 1.0);
        screen_to_sim_uv(ndc, screen_ar, sim_ar)
    });

    // Debug: WC_DEBUG_FORCE_CYMATICS_INTERACTION forces a deterministic centre
    // press at UV (0.5, 0.5) for the `cymatics-interacting` capture scenario so
    // active_radius grows reproducibly without hardware or a real mouse.
    #[cfg(debug_assertions)]
    let (mouse_pressed, mouse_uv) = if debug_toggles
        .as_ref()
        .is_some_and(|t| t.force_cymatics_interaction)
    {
        (true, Vec2::new(0.5, 0.5))
    } else {
        (mouse_pressed, mouse_uv)
    };

    let input = CenterInput {
        mouse_pressed,
        mouse_uv,
        c1_held: hands.c1.is_some(),
        c1_uv: hands.c1.unwrap_or(Vec2::new(0.5, 0.5)),
        c2_held: hands.c2.is_some(),
        c2_uv: hands.c2.unwrap_or(Vec2::new(0.5, 0.5)),
    };

    // Build the tuning struct from live settings (defaults match v4 constants).
    step_centers(&mut state, input, CenterTuning::from_settings(&settings));
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cymatics::CymaticsState;
    use bevy::math::Vec2;

    fn idle_input() -> CenterInput {
        CenterInput {
            mouse_pressed: false,
            mouse_uv: Vec2::new(0.5, 0.5),
            c1_held: false,
            c1_uv: Vec2::ZERO,
            c2_held: false,
            c2_uv: Vec2::ZERO,
        }
    }

    #[test]
    fn idle_decays_active_radius_toward_minimum() {
        let mut s = CymaticsState {
            active_radius: 5.0,
            ..Default::default()
        };
        for _ in 0..2000 {
            step_centers(&mut s, idle_input(), CenterTuning::default());
        }
        assert!((s.active_radius - MINIMUM_ACTIVE_RADIUS).abs() < 1e-2);
    }

    #[test]
    fn interacting_grows_active_radius_toward_target() {
        let mut s = CymaticsState::default();
        let input = CenterInput {
            mouse_pressed: true,
            ..idle_input()
        };
        for _ in 0..2000 {
            step_centers(&mut s, input, CenterTuning::default());
        }
        assert!(s.active_radius > 5.0); // approaches TARGET (7.5)
        assert!(s.active_radius >= MINIMUM_ACTIVE_RADIUS_INTERACTING);
    }

    #[test]
    fn free_center2_mirrors_center1() {
        // c1 held at (0.3,0.4); c2 free -> mirrors to (0.7,0.6) over time.
        let mut s = CymaticsState::default();
        let input = CenterInput {
            mouse_pressed: false,
            mouse_uv: Vec2::new(0.3, 0.4),
            c1_held: true,
            c1_uv: Vec2::new(0.3, 0.4),
            c2_held: false,
            c2_uv: Vec2::ZERO,
        };
        for _ in 0..3000 {
            step_centers(&mut s, input, CenterTuning::default());
        }
        assert!((s.center.x - 0.3).abs() < 0.05);
        assert!((s.center2.x - 0.7).abs() < 0.05); // 1 - 0.3
        assert!((s.center2.y - 0.6).abs() < 0.05); // 1 - 0.4
    }

    #[test]
    fn num_cycles_decays_to_default_when_idle() {
        let mut s = CymaticsState {
            num_cycles: 1.5,
            ..Default::default()
        };
        for _ in 0..500 {
            step_centers(&mut s, idle_input(), CenterTuning::default());
        }
        assert!((s.num_cycles - DEFAULT_NUM_CYCLES).abs() < 1e-2);
    }

    #[test]
    fn is_ready_to_sleep_when_radius_low() {
        let s = CymaticsState {
            active_radius: MINIMUM_ACTIVE_RADIUS,
            ..Default::default()
        };
        assert!(is_ready_to_sleep(&s));
        let s2 = CymaticsState {
            active_radius: 1.0,
            ..Default::default()
        };
        assert!(!is_ready_to_sleep(&s2));
    }
}
