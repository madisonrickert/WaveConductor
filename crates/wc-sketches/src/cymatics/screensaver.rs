//! Cymatics attract mode: the two wave centres drift on slow incommensurate
//! Lissajous paths, emitting gentle continuous ripples at a low ambient radius.
//! Gated on `in_screensaver(AppState::Cymatics)` (zero systems otherwise);
//! audio coupling is gated off (the coupling chain is `sketch_active`-only).

use bevy::prelude::*;
use wc_core::lifecycle::screensaver::in_screensaver;
use wc_core::lifecycle::state::AppState;

use super::settings::CymaticsSettings;
use super::{CymaticsState, DEFAULT_NUM_CYCLES};

// ---------------------------------------------------------------------------
// Lissajous speed bundle
// ---------------------------------------------------------------------------

/// Live-tunable Lissajous angular speeds for the two attract-mode centres.
///
/// Default values reproduce the v4 incommensurate ratios (43:31 for centre 1,
/// 37:29 for centre 2) so no visual change occurs at the default settings.
/// Sourced from [`CymaticsSettings`] each frame via
/// [`LissajousSpeeds::from_settings`].
#[derive(Clone, Copy, Debug)]
pub struct LissajousSpeeds {
    /// Angular speed for centre-1 X component (rad/s). v4 default `0.043`.
    pub c1_omega_x: f32,
    /// Angular speed for centre-1 Y component (rad/s). v4 default `0.031`.
    pub c1_omega_y: f32,
    /// Angular speed for centre-2 X component (rad/s). v4 default `0.037`.
    pub c2_omega_x: f32,
    /// Angular speed for centre-2 Y component (rad/s). v4 default `0.029`.
    pub c2_omega_y: f32,
}

impl Default for LissajousSpeeds {
    fn default() -> Self {
        Self {
            c1_omega_x: 0.043,
            c1_omega_y: 0.031,
            c2_omega_x: 0.037,
            c2_omega_y: 0.029,
        }
    }
}

impl LissajousSpeeds {
    /// Construct from live [`CymaticsSettings`].
    pub fn from_settings(s: &CymaticsSettings) -> Self {
        Self {
            c1_omega_x: s.c1_omega_x,
            c1_omega_y: s.c1_omega_y,
            c2_omega_x: s.c2_omega_x,
            c2_omega_y: s.c2_omega_y,
        }
    }
}

// ---------------------------------------------------------------------------
// Pure Lissajous path
// ---------------------------------------------------------------------------

/// Two slow incommensurate Lissajous paths in [0,1]².
///
/// The angular frequencies in `speeds` should be chosen so no pair is
/// rationally related; the pattern therefore does not visibly repeat over a
/// multi-hour kiosk runtime. Amplitudes of 0.3 around centre 0.5 keep both
/// sources in \[0.2, 0.8\] — well inside the sim UV field.
///
/// `elapsed` is the phase clock in seconds (typically
/// `Time::elapsed_secs()`). This function is pure and headless-testable:
/// no Bevy world state is read or written.
#[must_use]
pub fn wander_centers(elapsed: f32, speeds: &LissajousSpeeds) -> (Vec2, Vec2) {
    // Centre 1: traces a slow figure-eight with the given X/Y omegas.
    // No phase offset; starts at (0.5, 0.8).
    let c1 = Vec2::new(
        0.5 + 0.3 * (elapsed * speeds.c1_omega_x).sin(),
        0.5 + 0.3 * (elapsed * speeds.c1_omega_y).cos(),
    );
    // Centre 2: phase-offset (+1.7, +0.6 rad) so both centres are spatially
    // separated at t=0. The different omegas keep the interference pattern
    // incommensurate with centre 1.
    let c2 = Vec2::new(
        0.5 + 0.3 * (elapsed * speeds.c2_omega_x + 1.7).sin(),
        0.5 + 0.3 * (elapsed * speeds.c2_omega_y + 0.6).cos(),
    );
    (c1, c2)
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

/// Drive `CymaticsState` from the Lissajous wander while the screensaver shows.
///
/// Writes `center`, `center2`, `active_radius`, and `num_cycles` each frame.
/// The `active_radius` is read from `CymaticsSettings::attract_radius` (Dev
/// knob; default 0.6 matching v4). Lissajous speeds are read from the four
/// `c[12]_omega_[xy]` Dev knobs.
///
/// Does **not** advance `simulation_time` — `update_cymatics_sim_params` (C8)
/// is the sole advancer of that field (single-owner invariant). The GPU sim
/// therefore keeps animating at the same phase rate as in the active sketch;
/// only the spatial position of the two wave sources changes.
///
/// Per-frame no-allocation guarantee: all arithmetic is on stack scalars.
fn drive_cymatics_attract(
    time: Res<'_, Time>,
    mut state: ResMut<'_, CymaticsState>,
    settings: Res<'_, CymaticsSettings>,
) {
    let speeds = LissajousSpeeds::from_settings(&settings);
    let (c1, c2) = wander_centers(time.elapsed_secs(), &speeds);
    state.center = c1;
    state.center2 = c2;
    // attract_radius defaults to 0.6 (v4 ATTRACT_ACTIVE_RADIUS).
    state.active_radius = settings.attract_radius;
    state.num_cycles = DEFAULT_NUM_CYCLES;
    // `simulation_time` is advanced by `update_cymatics_sim_params` (C8) which
    // runs under `sketch_active OR in_screensaver`; do not advance it here
    // (single-owner invariant).
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wander_is_deterministic_and_in_bounds() {
        let speeds = LissajousSpeeds::default();
        for &t in &[0.0_f32, 1.5, 7.3, 100.0] {
            let (c1, c2) = wander_centers(t, &speeds);
            // Pure fn with no hidden state: same elapsed + same speeds → bit-exact same output.
            assert_eq!((c1, c2), wander_centers(t, &speeds)); // deterministic
            for c in [c1, c2] {
                // Amplitude 0.3 around centre 0.5 → always in [0.2, 0.8] ⊆ [0.0, 1.0].
                assert!(c.x >= 0.0 && c.x <= 1.0 && c.y >= 0.0 && c.y <= 1.0);
            }
        }
    }

    #[test]
    fn centers_move_over_time() {
        let speeds = LissajousSpeeds::default();
        let (a1, _) = wander_centers(0.0, &speeds);
        let (b1, _) = wander_centers(3.0, &speeds);
        assert!(a1.distance(b1) > 1e-3);
    }

    /// Custom speeds produce different positions than the defaults.
    #[test]
    fn custom_speeds_differ_from_defaults() {
        let defaults = LissajousSpeeds::default();
        let fast = LissajousSpeeds {
            c1_omega_x: 0.2,
            ..defaults
        };
        let (d1, _) = wander_centers(10.0, &defaults);
        let (f1, _) = wander_centers(10.0, &fast);
        assert!(
            d1.distance(f1) > 1e-3,
            "different omegas must yield different positions"
        );
    }
}
