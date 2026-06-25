//! Cymatics attract mode: the two wave centres drift on slow incommensurate
//! Lissajous paths while each intermittently drops a single "raindrop" — one
//! Hann-enveloped source pulse that launches one expanding ring on the otherwise
//! calm field. [`drive_cymatics_attract`] positions the centres and holds the
//! ambient alive-mask radius; [`drive_cymatics_pings`] schedules the staggered
//! drops via [`CymaticsPingState`]. Both are gated on
//! `in_screensaver(AppState::Cymatics)` (zero systems otherwise); audio coupling
//! is gated off (the coupling chain is `sketch_active`-only).

use bevy::prelude::*;
use wc_core::lifecycle::screensaver::in_screensaver;
use wc_core::lifecycle::state::AppState;

use super::compute::{CymaticsSimParams, MAX_ITERATIONS};
use super::settings::CymaticsSettings;
use super::CymaticsState;

// ---------------------------------------------------------------------------
// Lissajous speed bundle
// ---------------------------------------------------------------------------

/// Live-tunable Lissajous angular speeds for the two attract-mode centres.
///
/// Default values are 3.5× the v4 Lissajous speeds, scaled by a single common
/// factor so the v4 incommensurate ratios (43:31 for centre 1, 37:29 for centre
/// 2, and the cross ratios) are preserved while the two centres wander — and
/// their ripples interfere — noticeably faster (periods drop from ~145–217 s to
/// ~42–62 s). These match the [`CymaticsSettings`] defaults, so the live path
/// (sourced each frame via [`LissajousSpeeds::from_settings`]) agrees with this
/// `Default`.
#[derive(Clone, Copy, Debug)]
pub struct LissajousSpeeds {
    /// Angular speed for centre-1 X component (rad/s). Default `0.1505` (3.5× v4's `0.043`).
    pub c1_omega_x: f32,
    /// Angular speed for centre-1 Y component (rad/s). Default `0.1085` (3.5× v4's `0.031`).
    pub c1_omega_y: f32,
    /// Angular speed for centre-2 X component (rad/s). Default `0.1295` (3.5× v4's `0.037`).
    pub c2_omega_x: f32,
    /// Angular speed for centre-2 Y component (rad/s). Default `0.1015` (3.5× v4's `0.029`).
    pub c2_omega_y: f32,
}

impl Default for LissajousSpeeds {
    fn default() -> Self {
        Self {
            c1_omega_x: 0.1505,
            c1_omega_y: 0.1085,
            c2_omega_x: 0.1295,
            c2_omega_y: 0.1015,
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
// Raindrop ping scheduler
// ---------------------------------------------------------------------------

/// Golden ratio φ — the low-discrepancy multiplier for the raindrop jitter.
/// Truncated to f32 precision; only its fractional irrationality matters here.
const PHI: f32 = 1.618_034;

/// Per-centre phase offsets into the golden-ratio jitter sequence, so the two
/// attractors draw different intervals and never fire in lock-step.
const PING_CENTER_OFFSETS: [f32; 2] = [0.0, 0.5];

// Raindrop tuning defaults. Commit 3 promotes these to live Dev knobs
// (`ping_interval` / `ping_jitter` / `ping_strength` / `ping_duration` in
// `CymaticsSettings`); until then the scheduler and the hand-off read these
// module constants so the behaviour is fixed and self-contained.

/// Seconds between drops per attractor (floor of the jittered interval).
const PING_INTERVAL: f32 = 3.5;
/// Extra seconds of golden-ratio jitter added on top of [`PING_INTERVAL`].
const PING_JITTER: f32 = 2.5;
/// Peak displacement of one raindrop's Hann lobe (drop strength / vividness).
pub(crate) const PING_STRENGTH: f32 = 4.0;
/// Raindrop Hann-window length `D` in sub-step ticks (splash length).
pub(crate) const PING_DURATION: f32 = 30.0;

/// Per-attractor raindrop scheduler state for the Cymatics screensaver.
///
/// One entry per wave centre. [`drive_cymatics_pings`] is the sole writer: it
/// counts down to each centre's next drop, fires by restarting that centre's
/// Hann window (`envelope_tick = 0`), and rolls the window forward one frame's
/// worth of sub-steps. [`super::update_cymatics_sim_params`] reads
/// `envelope_tick` into the GPU `ping_base` so the compute prepare loop can
/// evaluate the per-sub-step envelope.
///
/// Persists across screensaver entries (init-once): between sessions
/// `drive_cymatics_pings` does not run, so the countdowns freeze and resume, and
/// `ping_count` keeps climbing so the staggered interval sequence continues
/// without repeating.
#[derive(Resource, Debug, Clone)]
pub struct CymaticsPingState {
    /// Seconds until each centre's next drop fires. Decremented by the frame
    /// delta; on reaching `0` the centre fires and reschedules.
    pub seconds_until_next_ping: [f32; 2],
    /// Each centre's Hann-window position in sub-step ticks. `>= ping_duration`
    /// means the window has closed (the source is quiet between drops). Reset to
    /// `0` on fire, advanced by N sub-steps per frame.
    pub envelope_tick: [f32; 2],
    /// Monotonic drop counter per centre, feeding the golden-ratio jitter so each
    /// successive interval is deterministic yet non-repeating.
    pub ping_count: [u32; 2],
}

impl Default for CymaticsPingState {
    fn default() -> Self {
        Self {
            // Stagger the first drops: centre 0 fires almost immediately when the
            // screensaver appears, centre 1 about half a base interval later, so
            // the two never start in lock-step (the golden-ratio jitter keeps them
            // desynced thereafter).
            seconds_until_next_ping: [0.0, PING_INTERVAL * 0.5],
            // Both windows start closed (>= any positive duration) so no drop is
            // mid-flight until the first scheduled fire.
            envelope_tick: [f32::MAX, f32::MAX],
            ping_count: [0, 0],
        }
    }
}

impl CymaticsPingState {
    /// Advance centre `c`'s scheduler by one frame: roll the Hann window forward
    /// `n` sub-steps, count down `dt` seconds toward the next drop, and on expiry
    /// restart the window (`envelope_tick = 0`) and schedule the next drop.
    ///
    /// The window advance happens BEFORE the fire-check so the frame a drop fires
    /// renders the pulse from tick 0 (the fire-reset overrides the advance); on
    /// later frames the window simply marches forward by `n` until it passes
    /// `ping_duration` and the source falls quiet. Pure (no ECS / clock) so the
    /// fire cadence and window advance are unit-testable.
    fn step(&mut self, c: usize, dt: f32, n: f32, interval: f32, jitter: f32) {
        self.envelope_tick[c] += n;
        self.seconds_until_next_ping[c] -= dt;
        if self.seconds_until_next_ping[c] <= 0.0 {
            self.envelope_tick[c] = 0.0;
            self.ping_count[c] = self.ping_count[c].wrapping_add(1);
            self.seconds_until_next_ping[c] +=
                next_ping_interval(self.ping_count[c], PING_CENTER_OFFSETS[c], interval, jitter);
        }
    }
}

/// Golden-ratio low-discrepancy interval generator (no RNG dependency).
///
/// Returns `interval + jitter · frac(count·φ + offset)`. `frac(·) ∈ [0, 1)`, so
/// the result lies in `[interval, interval + jitter)`. Because φ is irrational
/// the sequence is equidistributed and never periodic, and a distinct `offset`
/// per centre keeps the two centres' interval sequences out of phase — so the
/// drops desync and never lock into a shared cadence. `count` is wrapped to a
/// small range before the multiply so `frac` stays full-precision over a
/// multi-hour soak. Pure and deterministic, so the scheduling is unit-testable
/// without a clock.
fn next_ping_interval(count: u32, offset: f32, interval: f32, jitter: f32) -> f32 {
    // Wrap so count·φ never grows past f32's precise integer range (the frac
    // would otherwise quantize). u16::try_from on a value < 1000 never fails.
    let wrapped = u16::try_from(count % 1000).unwrap_or(0);
    let x = f32::from(wrapped) * PHI + offset;
    let frac = x - x.floor();
    interval + jitter * frac
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
/// run here. `drive_cymatics_pings` advances the raindrop scheduler (its own
/// `CymaticsPingState` resource — no overlap with `CymaticsState`) and runs
/// `.before(update_cymatics_sim_params)` so the freshest envelope ticks are
/// handed to the GPU resource the same frame. `update_cymatics_sim_params` (C8)
/// runs under `sketch_active OR in_screensaver`, so the GPU simulation stays
/// animated. Audio coupling is `sketch_active`-only — attract is intentionally
/// silent.
pub struct CymaticsScreensaverPlugin;

impl Plugin for CymaticsScreensaverPlugin {
    fn build(&self, app: &mut App) {
        // Raindrop scheduler state persists across screensaver entries (init-once).
        app.init_resource::<CymaticsPingState>();

        // Zero systems outside the screensaver (AGENTS.md "zero systems when
        // idle"). The interaction systems that also write CymaticsState are
        // `sketch_active`-only, so `drive_cymatics_attract` is the sole
        // CymaticsState writer while the screensaver is showing.
        app.add_systems(
            Update,
            (
                drive_cymatics_attract,
                // Ordered before the CPU→GPU bridge so this frame's fresh envelope
                // ticks reach `CymaticsSimParams` (and so the render world) the
                // same frame, not one frame late.
                drive_cymatics_pings.before(super::update_cymatics_sim_params),
            )
                .run_if(in_screensaver(AppState::Cymatics)),
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
/// knob; default 0.6 = v4's `ATTRACT_ACTIVE_RADIUS` baseline, for wide idle
/// coverage). `num_cycles`
/// is read from `CymaticsSettings::attract_cycles` (default 0.1) so the source
/// phase advances slowly: small smooth per-frame deltas instead of a big
/// full-screen kick each throttled present (the old "jolt"). Lissajous speeds
/// are read from the four `c[12]_omega_[xy]` Dev knobs.
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
    // attract_radius defaults to 0.6 (v4's ATTRACT_ACTIVE_RADIUS baseline) for
    // wide idle coverage; gentleness comes from the slow attract_cycles rate,
    // not a small mask. The operator can push it to 0.7–0.8 live.
    state.active_radius = settings.attract_radius;
    // Slow source rate → small smooth per-frame phase deltas = gentle drift.
    // Pinning DEFAULT_NUM_CYCLES (1.002) advanced ~one full ±2 sine cycle per
    // rendered frame at the throttled present rate — a big discrete full-screen
    // kick (the "jolt"). attract_cycles is clamped to 0.02–0.3 (settings) to
    // avoid the ~0.5/1.5 half-integer rates that invert the source each frame.
    state.num_cycles = settings.attract_cycles;
    // `simulation_time` is advanced by `update_cymatics_sim_params` (C8) which
    // runs under `sketch_active OR in_screensaver`; do not advance it here
    // (single-owner invariant).
}

/// Advance the raindrop scheduler for both centres each screensaver frame.
///
/// Sole writer of [`CymaticsPingState`]. Reads `Time` for the frame delta and
/// `CymaticsSimParams` for `iterations` (the per-frame sub-step count N, which
/// the Hann window advances by so its progress is fps-independent — locked to
/// sub-step ticks, matching the compute prepare loop's slot count). The actual
/// drop timing/strength/duration are fixed module constants here; Commit 3
/// promotes them to live Dev knobs.
///
/// Per-frame no-allocation guarantee: all arithmetic is on stack scalars.
fn drive_cymatics_pings(
    time: Res<'_, Time>,
    sim: Res<'_, CymaticsSimParams>,
    mut ping: ResMut<'_, CymaticsPingState>,
) {
    let dt = time.delta_secs();
    // N sub-steps this frame = the Hann-window advance, matching the prepare
    // loop's slot count (`sim.iterations` clamped to the buffer capacity) so the
    // window progresses contiguously across frames. Converted without an `as`
    // cast: the clamp keeps the value <= MAX_ITERATIONS (120), within u16.
    let cap = u32::try_from(MAX_ITERATIONS).unwrap_or(u32::MAX);
    let n = f32::from(u16::try_from(sim.iterations.min(cap)).unwrap_or(0));
    for c in 0..2 {
        ping.step(c, dt, n, PING_INTERVAL, PING_JITTER);
    }
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

    /// Golden-ratio jitter: every interval lands in `[interval, interval + jitter)`
    /// over a long run, never escaping the configured band.
    #[test]
    fn ping_intervals_stay_in_band() {
        for count in 0..2000u32 {
            for &offset in &PING_CENTER_OFFSETS {
                let iv = next_ping_interval(count, offset, PING_INTERVAL, PING_JITTER);
                assert!(
                    (PING_INTERVAL..PING_INTERVAL + PING_JITTER).contains(&iv),
                    "interval {iv} escaped [{PING_INTERVAL}, {})",
                    PING_INTERVAL + PING_JITTER
                );
            }
        }
    }

    /// The two centres never lock into a fixed phase relationship: the offset
    /// between their k-th cumulative fire times drifts over many drops. A fixed
    /// offset would mean the centres always fire a constant time apart (visual
    /// lock-step); a drifting one proves they stay desynced.
    #[test]
    fn ping_centers_desync_over_many_pings() {
        let fire_times = |offset: f32| -> Vec<f32> {
            let mut t = 0.0_f32;
            (1..=200u32)
                .map(|count| {
                    t += next_ping_interval(count, offset, PING_INTERVAL, PING_JITTER);
                    t
                })
                .collect()
        };
        let a = fire_times(PING_CENTER_OFFSETS[0]);
        let b = fire_times(PING_CENTER_OFFSETS[1]);
        // The two centres draw different first intervals, so they start staggered.
        assert!(
            (a[0] - b[0]).abs() > 1e-3,
            "centres must start staggered (different first fire times)"
        );
        let diffs: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x - y).collect();
        let max = diffs.iter().copied().fold(f32::MIN, f32::max);
        let min = diffs.iter().copied().fold(f32::MAX, f32::min);
        assert!(
            max - min > 1.0,
            "inter-centre fire offset must drift (no lock-step); spread was {}",
            max - min
        );
    }

    /// One scheduler step: the Hann window advances by N before the fire-check,
    /// a zero-crossing countdown fires (window restarts at tick 0, drop count
    /// increments, next drop reschedules at least one base interval out), and the
    /// post-fire frame rolls the window forward to N.
    #[test]
    fn ping_step_fires_at_zero_and_advances_window() {
        let mut s = CymaticsPingState {
            seconds_until_next_ping: [1.0, 1.0],
            envelope_tick: [f32::MAX, f32::MAX],
            ping_count: [0, 0],
        };
        // No fire yet: window advances, countdown drops, tick stays past the window.
        s.step(0, 0.5, 20.0, PING_INTERVAL, PING_JITTER);
        assert!(s.seconds_until_next_ping[0] > 0.0, "still counting down");
        assert!(s.envelope_tick[0] > PING_DURATION, "window still closed");
        assert_eq!(s.ping_count[0], 0, "no fire yet");
        // Crossing zero fires: window restarts at 0, count increments, reschedules.
        s.step(0, 1.0, 20.0, PING_INTERVAL, PING_JITTER);
        assert!(
            s.envelope_tick[0].abs() < f32::EPSILON,
            "fire restarts the Hann window at tick 0 (exact-zero check via epsilon)"
        );
        assert_eq!(s.ping_count[0], 1, "fire increments the drop count");
        assert!(
            s.seconds_until_next_ping[0] >= PING_INTERVAL,
            "next drop scheduled at least one base interval out"
        );
        // Advance-first rule: the next frame (no fire) rolls the window to N.
        s.step(0, 0.001, 20.0, PING_INTERVAL, PING_JITTER);
        assert!(
            (s.envelope_tick[0] - 20.0).abs() < 1e-4,
            "post-fire frame advances the window to N"
        );
    }
}
