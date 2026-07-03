//! Per-frame Flame simulation writer plus the idle freeze.
//!
//! Owns [`FlameState`] (the main-world mirror of everything the fractal needs
//! between name changes), the pure [`flame_cx`] oscillation, and the single
//! [`bake_flame_sim`] baker that both the live writer ([`update_flame_sim`])
//! and, later, the screensaver performer call — one baker, multiple writers,
//! so the warp/dispatch derivation cannot drift (Condition A1).
//!
//! Nothing here allocates: every value is stack math over `Copy` inputs, so
//! the per-frame path is heap-free per the multi-hour soak target.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "f64 sigmoid -> f32 and window-derived f32 sizing are intentional, \
              on bounded values, and documented at each site"
)]

use bevy::prelude::*;
use wc_core::input::pointer::PointerState;
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;

use crate::flame::branches::FlameSpec;
use crate::flame::compute::sim_params::FlameSimParams;
use crate::flame::levels::LevelLayout;
use crate::flame::settings::FlameSettings;

/// Main-world mirror of the live fractal, rebuilt on name change and read every
/// frame by the writer. Held out of [`FlameSimParams`] so the extract resource
/// stays a memcpy-cloneable POD (the branch table + handle) while the CPU-side
/// spec/layout/scalars live here.
#[derive(Resource)]
pub struct FlameState {
    /// Name-derived branch set + `cY` + audio character.
    pub spec: FlameSpec,
    /// Branch-major level layout for the current branch count + point budget.
    pub layout: LevelLayout,
    /// Last normalized name applied (change detection vs. settings).
    pub last_name: String,
    /// Last point-budget applied (change detection vs. settings).
    pub last_target_points: f32,
    /// v4 `cX`: the time-oscillated attractor x-driver, in (-1, 1).
    pub c_x: f32,
    /// Pointer/hand warp offset in normalized device coords ([-1, 1]).
    pub warp_input: Vec2,
    /// Live fraction of the tree ([0, 1]); 1.0 while active, lowered by the
    /// screensaver ember ramp. Drives the dispatch prefix in [`bake_flame_sim`].
    pub complexity: f32,
}

/// Pure v4 oscillation: `cX = 2*sigmoid(6*sin(elapsed/3)) - 1`.
///
/// v4's `±10` sigmoid clamps are unreachable here (`|6*sin| <= 6`), so the
/// closed form is exact. Bounded in (-1, 1).
#[must_use]
pub fn flame_cx(elapsed_secs: f64) -> f32 {
    // Inner drive: 6 * sin(t/3). Peaks at ±6 as sin sweeps ±1.
    let x = 6.0 * (elapsed_secs / 3.0).sin();
    // Logistic sigmoid in [0, 1].
    let sig = 1.0 / (1.0 + (-x).exp());
    // Remap [0, 1] -> (-1, 1).
    (2.0 * sig - 1.0) as f32
}

/// One baker, two writers (live + screensaver) — Condition A1.
///
/// Writes the per-frame attractor warp `(cX/5 + cDx, cY/5 + cDy)` and the
/// dispatch prefix: `live` visible nodes at the current complexity map to the
/// number of leading levels, minus the never-dispatched root, so `complexity
/// == 0.0` freezes to zero dispatches beyond the root.
pub fn bake_flame_sim(state: &FlameState, sim: &mut FlameSimParams) {
    // v4 warp: base cX/cY divided by 5, plus the pointer/hand push.
    sim.params.warp = [
        state.c_x / 5.0 + state.warp_input.x,
        state.spec.c_y / 5.0 + state.warp_input.y,
    ];
    // Visible node count at this complexity -> leading levels that intersect
    // it. Subtract 1: level 0 (the root) is never dispatched.
    let live = state.layout.live_count_for_complexity(state.complexity);
    sim.level_count = state
        .layout
        .dispatch_levels_for_live(live)
        .saturating_sub(1);
}

/// Ember complexity decay driven by [`ScreensaverFade::alpha`]: full tree at
/// fade 0 (Active steady state), [`FlameSettings::ember_fraction`] at fade 1
/// (Screensaver steady state), linear between. The same curve rides
/// `ScreensaverFade`'s 1.5 s ramp in both directions, so the graceful decay
/// into the ember and the roar-back on wake are symmetric.
#[must_use]
pub fn ember_complexity(fade_alpha: f32, ember_fraction: f32) -> f32 {
    1.0 - fade_alpha * (1.0 - ember_fraction)
}

/// `Update` (gated `sketch_active(AppState::Flame)`): advance the virtual-time
/// `cX`, map the pixel-space warp source to the `[-1, 1]` warp offset, apply
/// the ember complexity curve (so the F15 wake roar-back completes during
/// Active's fade-out — the Dots dual-gate lesson), then bake.
///
/// [`FlameGrabState::warp_px`] (F10) is the single pixel-space source of the
/// warp: the pointer only writes it while `grabbing_count == 0` (a hand grab
/// is driving the warp otherwise, via [`super::hands::step_grab`]), but the
/// `[-1, 1]` mapping below always reads it, so the pointer and hand paths
/// converge on one downstream write.
///
/// Reads `Time` in virtual seconds so the capture harness (which pins the sim
/// timestep) produces deterministic frames. All stack math — no allocation.
pub fn update_flame_sim(
    time: Res<'_, Time>,
    pointer: Res<'_, PointerState>,
    window: Single<'_, '_, &Window>,
    settings: Res<'_, FlameSettings>,
    fade: Res<'_, ScreensaverFade>,
    mut state: ResMut<'_, FlameState>,
    mut sim: ResMut<'_, FlameSimParams>,
    mut grab_state: ResMut<'_, super::hands::FlameGrabState>,
) {
    // v4 time oscillation on the virtual clock.
    state.c_x = flame_cx(time.elapsed_secs_f64());

    let w = window.width();
    let h = window.height();

    // Pointer drives `warp_px` only while no hand is grabbing; while grabbing,
    // `update_flame_hands` (ordered before this system) has already written
    // it for this frame via `step_grab`'s steady-grab branch.
    if grab_state.grabbing_count == 0 {
        if let Some(p) = pointer.primary {
            grab_state.warp_px = p;
        }
    }

    // Map the pixel-space warp source (window logical coords, top-left
    // origin) to normalized device coords in [-1, 1], matching v4's
    // `mapLinear`. Guard against a zero-sized window (no divide-by-zero spike).
    if w > 0.0 && h > 0.0 {
        state.warp_input = Vec2::new(
            grab_state.warp_px.x / w * 2.0 - 1.0,
            grab_state.warp_px.y / h * 2.0 - 1.0,
        );
    }

    // Live sketch shows the full tree once the wake roar-back completes;
    // `ember_complexity` reads the SAME fade envelope the screensaver's
    // `drive_flame_attract_sim` does, so `complexity` rides `fade.alpha()`
    // back to 1.0 over the fade-out instead of snapping there on wake.
    state.complexity = ember_complexity(fade.alpha(), settings.ember_fraction);

    bake_flame_sim(&state, &mut sim);
}

/// `OnEnter(SketchActivity::Idle)` (gated `in_state(AppState::Flame)`): zero the
/// dispatch level count so the compute pass does no work while frozen (v4 froze
/// the fractal on idle too). Waking re-enters `Active`, where [`update_flame_sim`]
/// restores the count next frame.
pub fn freeze_flame_sim(mut sim: ResMut<'_, FlameSimParams>) {
    sim.level_count = 0;
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
#[allow(
    clippy::excessive_precision,
    reason = "v4 cX golden literal preserved verbatim; f32 truncates it, the \
              1e-5 tolerance still holds"
)]
mod tests {
    use super::*;
    use crate::flame::branches::build_flame_spec;
    use crate::flame::compute::sim_params::{encode_branches, encode_levels, FlameLevelParamsGpu};
    use crate::flame::levels::{LevelLayout, MAX_LEVELS};
    use bytemuck::Zeroable;

    /// Build a `FlameSimParams` from a state's spec/layout with a default node
    /// handle, mirroring what `spawn_flame` inserts (minus the real buffer).
    fn test_sim_params(state: &FlameState) -> FlameSimParams {
        let mut levels = [FlameLevelParamsGpu::zeroed(); MAX_LEVELS];
        let level_count = encode_levels(&state.layout, &mut levels);
        FlameSimParams {
            params: encode_branches(&state.spec),
            levels,
            level_count,
            nodes: Handle::default(),
        }
    }

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

    /// Ember endpoints and midpoint: fade 0 -> full, fade 1 -> ember fraction.
    #[test]
    fn ember_complexity_endpoints() {
        assert!((ember_complexity(0.0, 0.5) - 1.0).abs() < 1e-6);
        assert!((ember_complexity(1.0, 0.5) - 0.5).abs() < 1e-6);
        assert!((ember_complexity(0.5, 0.5) - 0.75).abs() < 1e-6);
        // ember_fraction 1.0 disables the decay entirely.
        assert!((ember_complexity(1.0, 1.0) - 1.0).abs() < 1e-6);
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
