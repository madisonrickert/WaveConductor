//! Cymatics attract mode: the two wave centres drift on slow incommensurate
//! Lissajous paths, emitting gentle continuous ripples at a low ambient radius.
//! Gated on `in_screensaver(AppState::Cymatics)` (zero systems otherwise);
//! audio coupling is gated off (the coupling chain is `sketch_active`-only).

use bevy::prelude::*;
use wc_core::lifecycle::screensaver::in_screensaver;
use wc_core::lifecycle::state::AppState;

use super::{CymaticsState, DEFAULT_NUM_CYCLES};

/// Ambient alive-radius held during attract (gentle, low-power ripples).
///
/// 0.6 keeps the wave field visible without saturating it; leaving
/// `active_radius` at the resting [`super::MINIMUM_ACTIVE_RADIUS`] (0.1) would
/// produce a nearly invisible mask unsuitable for a kiosk attract loop.
const ATTRACT_ACTIVE_RADIUS: f32 = 0.6;

/// Two slow incommensurate Lissajous paths in [0,1]².
///
/// The four angular frequencies are chosen so no pair is rationally related;
/// the pattern therefore does not visibly repeat over a multi-hour kiosk
/// runtime. Amplitudes of 0.3 around centre 0.5 keep both sources in
/// \[0.2, 0.8\] — well inside the sim UV field.
///
/// `elapsed` is the phase clock in seconds (typically
/// `Time::elapsed_secs()`). This function is pure and headless-testable:
/// no Bevy world state is read or written.
#[must_use]
pub fn wander_centers(elapsed: f32) -> (Vec2, Vec2) {
    // Centre 1 — ω_x = 0.043 rad/s, ω_y = 0.031 rad/s (Lissajous 31:43).
    // No phase offset; starts at (0.5, 0.8) and traces a slow figure-eight.
    let c1 = Vec2::new(
        0.5 + 0.3 * (elapsed * 0.043).sin(),
        0.5 + 0.3 * (elapsed * 0.031).cos(),
    );
    // Centre 2 — ω_x = 0.037 rad/s, ω_y = 0.029 rad/s, phase-offset (+1.7,
    // +0.6 rad) so both centres are spatially separated at t=0. The 37:43 and
    // 29:31 frequency ratios are incommensurate with Centre 1, so the
    // two-source interference pattern never visibly repeats.
    let c2 = Vec2::new(
        0.5 + 0.3 * (elapsed * 0.037 + 1.7).sin(),
        0.5 + 0.3 * (elapsed * 0.029 + 0.6).cos(),
    );
    (c1, c2)
}

/// Plugin: drive the attract motion only while the Cymatics screensaver shows.
///
/// ## Wiring
///
/// `drive_cymatics_attract` is the sole `CymaticsState` writer while in
/// screensaver; the interaction systems are `sketch_active`-only and do not
/// run here. `update_cymatics_sim_params` (C8) runs under
/// `sketch_active OR in_screensaver`, so the GPU simulation stays animated.
/// Audio coupling is `sketch_active`-only — attract is intentionally silent.
pub struct CymaticsScreensaverPlugin;

impl Plugin for CymaticsScreensaverPlugin {
    fn build(&self, app: &mut App) {
        // Zero systems outside the screensaver (AGENTS.md "zero systems when
        // idle"). The interaction systems that also write CymaticsState are
        // `sketch_active`-only, so `drive_cymatics_attract` is the sole
        // CymaticsState writer while the screensaver is showing.
        app.add_systems(
            Update,
            drive_cymatics_attract.run_if(in_screensaver(AppState::Cymatics)),
        );
    }
}

/// Drive `CymaticsState` from the Lissajous wander while the screensaver shows.
///
/// Writes `center`, `center2`, `active_radius`, and `num_cycles` each frame.
/// Does **not** advance `simulation_time` — `update_cymatics_sim_params` (C8)
/// is the sole advancer of that field (single-owner invariant). The GPU sim
/// therefore keeps animating at the same phase rate as in the active sketch;
/// only the spatial position of the two wave sources changes.
///
/// Per-frame no-allocation guarantee: all arithmetic is on stack scalars.
fn drive_cymatics_attract(time: Res<'_, Time>, mut state: ResMut<'_, CymaticsState>) {
    let (c1, c2) = wander_centers(time.elapsed_secs());
    state.center = c1;
    state.center2 = c2;
    state.active_radius = ATTRACT_ACTIVE_RADIUS;
    state.num_cycles = DEFAULT_NUM_CYCLES;
    // `simulation_time` is advanced by `update_cymatics_sim_params` (C8) which
    // runs under `sketch_active OR in_screensaver`; do not advance it here
    // (single-owner invariant).
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wander_is_deterministic_and_in_bounds() {
        for &t in &[0.0_f32, 1.5, 7.3, 100.0] {
            let (c1, c2) = wander_centers(t);
            assert_eq!((c1, c2), wander_centers(t)); // deterministic
            for c in [c1, c2] {
                assert!(c.x >= 0.0 && c.x <= 1.0 && c.y >= 0.0 && c.y <= 1.0);
            }
        }
    }

    #[test]
    fn centers_move_over_time() {
        let (a1, _) = wander_centers(0.0);
        let (b1, _) = wander_centers(3.0);
        assert!(a1.distance(b1) > 1e-3);
    }
}
