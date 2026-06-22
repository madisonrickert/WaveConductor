//! Pure attract-mode choreography math for the Line sketch.
//!
//! All functions here are deterministic functions of elapsed time and window
//! geometry — no `World`, no resources, no RNG — so the wandering-pulse paths
//! and envelopes are unit-testable in isolation and reproduce exactly under
//! the fixed-`dt` capture clock.
//!
//! ## Composition: "Wandering Pulses"
//!
//! ```text
//! SETTLED FIELD (base)     zero ambient attraction — the particle "picture"
//!         │                 stays readable; the noise turbulence (see
//!         │                 `crate::line::screensaver`) supplies the slow,
//!         │                 continuous between-pulse morphing
//!         │                 (see AMBIENT_POWER for why ambient must be zero)
//!         ▼ every walker period (14 / 19 / 23.5 s, staggered)
//! WANDERING PULSE          one of PULSE_COUNT (3) points — each tracing a
//!                           slow incommensurate-frequency Lissajous path
//!                           across the central region — swells to
//!                           PULSE_PEAK_POWER (0.35) for PULSE_ON_SECS (1.2 s),
//!                           nudging the field, then releases
//! ```
//!
//! Design intent (operator art direction): a mostly undisturbed field that
//! **slowly morphs**, explicitly **not** a vortex. The continuous organic drift
//! is the noise turbulence's job (`crate::line::screensaver`); the wandering
//! pulses add occasional gentle local bows on top. (An earlier composition also
//! had fast-crossing "meteor" attractors, but their moving pull jolted the
//! gravity smear's focal point too hard for a calm screensaver, so they were
//! cut in favour of leaning on the turbulence.) The walker periods are mutually
//! incommensurate and phase-staggered so pulses rarely sync. Across a full
//! recurrence (12,502 s) the pulses' instantaneous total raw power is bounded by
//! `PULSE_COUNT · PULSE_PEAK_POWER = 1.05`, ~15× below the old phantom-hand
//! grab's 15.7. The attract-mode lifetime respawn (`simulate.wgsl`) continuously
//! heals whatever the drift displaces, so the picture re-forms over time.

use crate::line::systems::sim_params::{weighted_focal, FOCAL_CENTER_WEIGHT};

/// World-space attractor sample: position + raw power, ready to bake into a
/// [`crate::particles::particle::Attractor`] (after the caller multiplies power by
/// `gravity_constant`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttractorSample {
    /// World-space position (centered on origin, +y up).
    pub position: [f32; 2],
    /// Raw attractor power (pre-`gravity_constant`). `0.0` = inactive.
    pub power: f32,
}

/// Number of wandering pulse points.
pub const PULSE_COUNT: usize = 3;

/// Full attract-frame snapshot the driver writes to the sim/post params.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttractFrame {
    /// The wandering pulse points. Power is zero at rest ([`AMBIENT_POWER`]),
    /// swelling to [`PULSE_PEAK_POWER`] at the crest of each walker's pulse.
    pub pulses: [AttractorSample; PULSE_COUNT],
    /// World-space focal point for the gravity smear (`i_mouse`): the
    /// envelope-weighted centroid of the pulse points, relaxing toward screen
    /// center when no pulse is active (see [`attract_frame`] for the math). The
    /// smear glow drifts gently with the cresting pulse.
    pub focal_world: [f32; 2],
    /// Overall activity `0..=1` — 0 in the settled field, 1 at the crest of a
    /// pulse. The smear `g_constant` scales with this.
    pub activity: f32,
}

/// Seconds a pulse stays "on" (the smooth swell-and-release window). Short
/// relative to every walker's period so the field is settled most of the time.
pub const PULSE_ON_SECS: f32 = 1.2;

/// Peak raw pulse power (pre-`gravity_constant`). Deliberately small — a
/// nudge that perturbs the field, nowhere near the old phantom-hand grab
/// (7.0 per hand) that read as a vortex.
///
/// Capture-tuned. The kernel's attractor force is constant-magnitude over
/// the whole frame (v4 parity: `power·G·size_scale·dx/dist`), so a pulse
/// nudges *every* particle toward the pulse point, and attract-only pulses
/// do monotonic inward work: 2.0 collapsed the line into the pulse point
/// within one pulse; 1.0 looked gentle per-pulse but still scrunched the
/// field into a clump by t ≈ 60 s. 0.35 over 1.2 s keeps each nudge to tens
/// of pixels so the wandering pulse positions sustain the field's spread
/// over hours instead of gathering it.
pub const PULSE_PEAK_POWER: f32 = 0.35;

/// Constant raw ambient power on every walker (pre-`gravity_constant`).
///
/// **Deliberately zero.** Any nonzero value keeps the compute kernel in
/// pulling-drag mode, whose per-step velocity damping is only ~0.23 %
/// (`V4_PULLING_DRAG_CONSTANT^V4_FIXED_DT`), so even a tiny constant pull
/// integrates nearly undamped into full field collapse — capture-verified:
/// 0.12 balled the whole line up at screen center within ~4 s, before the
/// first pulse even fired. With zero ambient the kernel sits in inertial
/// drag between pulses (~2 %/step), so each pulse leaves a few seconds of
/// natural coasting that settles instead of compounding.
pub const AMBIENT_POWER: f32 = 0.0;

/// Wander-path amplitude as a fraction of the half-width. Kept to the central
/// region so the drifting pulses move gently and never far — an operator note
/// that the old 0.72 sweep felt too dramatic. The pulses still visit a wide
/// span over minutes, just closer to centre.
const WANDER_X_FRAC: f32 = 0.46;

/// Wander-path amplitude as a fraction of the half-height. Softened from 0.62
/// alongside [`WANDER_X_FRAC`] for the same gentler-drift reason.
const WANDER_Y_FRAC: f32 = 0.40;

/// Per-walker wander path + pulse schedule.
///
/// Each walker traces a Lissajous figure:
/// `x = half_w · WANDER_X_FRAC · sin(freq.x · t + phase.x)` (same for y) —
/// the x/y frequencies are mutually incommensurate so the figure never
/// closes and the point eventually visits the whole amplitude box.
struct Walker {
    /// Lissajous angular frequencies (rad/s) for x / y. ~0.02–0.04 rad/s:
    /// one frame-crossing (half a sine cycle, `π/ω`) takes ~80–145 s, so the
    /// path spans the central region slowly over minutes — softened from the
    /// original ~0.03–0.05 band for a gentler drift (operator note).
    freq: [f32; 2],
    /// Lissajous phase offsets (rad) for x / y, spreading the walkers so
    /// they start in different regions of the frame.
    phase: [f32; 2],
    /// Pulse repeat period (s). The three periods are mutually
    /// incommensurate so pulse coincidences are rare and transient.
    period: f32,
    /// Pulse schedule offset (s): the walker's pulse window starts at
    /// `t = period − offset (mod period)`. Staggered so the first pulses
    /// land at t ≈ 4.0 / 8.1 / 15.0 s — after the 3 s particle fade-in, and
    /// never simultaneously.
    offset: f32,
}

/// The three walkers. All frequency/period choices are pairwise
/// incommensurate (no small integer ratios), which is what keeps the
/// composition aperiodic without RNG — the same trick as a wind chime.
const WALKERS: [Walker; PULSE_COUNT] = [
    Walker {
        freq: [0.035, 0.023],
        phase: [0.0, 1.7],
        period: 14.0,
        offset: 10.0,
    },
    Walker {
        freq: [0.022, 0.031],
        phase: [2.1, 4.0],
        period: 19.0,
        offset: 10.9,
    },
    Walker {
        freq: [0.028, 0.040],
        phase: [4.4, 0.9],
        period: 23.5,
        offset: 8.5,
    },
];

/// Window geometry the choreography needs (half-extents in world units).
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    /// Half the window width in world units.
    pub half_w: f32,
    /// Half the window height in world units.
    pub half_h: f32,
}

impl Bounds {
    /// Build from full window width/height.
    #[must_use]
    pub fn from_size(width: f32, height: f32) -> Self {
        Self {
            half_w: width * 0.5,
            half_h: height * 0.5,
        }
    }
}

/// Smooth 0→1→0 hump over `x in 0..=1`, zero-derivative at both ends
/// (`sin²(πx)`). Shapes each pulse's swell-and-release so the nudge has no
/// hard starts or stops.
#[must_use]
fn smooth_hump(x: f32) -> f32 {
    let s = (std::f32::consts::PI * x).sin();
    s * s
}

/// Walker `index`'s pulse envelope at time `t`: 0 in the settled field, a
/// smooth hump over the walker's `PULSE_ON_SECS` window once per period.
///
/// `phase = ((t + offset) / period) mod 1` — the pulse occupies the leading
/// `PULSE_ON_SECS / period` fraction of each cycle.
///
/// # Panics
///
/// Panics if `index >= PULSE_COUNT` (invariant violation: callers iterate
/// `0..PULSE_COUNT`).
#[must_use]
pub fn pulse_envelope(t: f32, index: usize) -> f32 {
    let walker = &WALKERS[index];
    let phase = ((t + walker.offset) / walker.period).rem_euclid(1.0);
    let on_frac = PULSE_ON_SECS / walker.period;
    if phase >= on_frac {
        return 0.0;
    }
    smooth_hump(phase / on_frac)
}

/// Compute the full attract frame at time `t` for the given bounds.
///
/// Deterministic in `t` — the capture clock pins `t = frame · dt`, so each
/// capture samples a reproducible point in the choreography.
#[must_use]
pub fn attract_frame(t: f32, bounds: Bounds) -> AttractFrame {
    let ax = bounds.half_w * WANDER_X_FRAC;
    let ay = bounds.half_h * WANDER_Y_FRAC;

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
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "choreography endpoints are exact; equality is the intended check"
)]
mod tests {
    use super::*;

    fn bounds() -> Bounds {
        Bounds::from_size(1280.0, 720.0)
    }

    /// Old phantom-hand peak power — kept only as the regression yardstick:
    /// the new design must stay far below it.
    const OLD_HAND_PEAK_POWER: f32 = 7.0;

    #[test]
    fn smooth_hump_endpoints_are_zero() {
        assert_eq!(smooth_hump(0.0), 0.0);
        assert!(smooth_hump(1.0).abs() < 1e-6);
        assert!((smooth_hump(0.5) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn envelope_zero_in_settled_field() {
        // t = 0 and t = 3 s sit between every walker's pulse window.
        for i in 0..PULSE_COUNT {
            assert_eq!(pulse_envelope(0.0, i), 0.0);
            assert_eq!(pulse_envelope(3.0, i), 0.0);
        }
    }

    #[test]
    fn envelope_peaks_mid_window() {
        // Walker 0's first pulse window is t ∈ [4.0, 5.8] (period 14, offset
        // 10): the hump crests at the window midpoint.
        let peak = pulse_envelope(4.0 + PULSE_ON_SECS * 0.5, 0);
        assert!(
            (peak - 1.0).abs() < 1e-4,
            "mid-window envelope should peak ~1, got {peak}"
        );
    }

    #[test]
    fn envelope_is_periodic() {
        let t = 4.0 + PULSE_ON_SECS * 0.5;
        let a = pulse_envelope(t, 0);
        let b = pulse_envelope(t + 14.0, 0);
        assert!((a - b).abs() < 1e-4, "envelope must repeat each period");
    }

    #[test]
    fn deterministic_same_t_same_frame() {
        // Same t → identical frame, bit-for-bit *within a process/platform*
        // (capture reproducibility on one machine). Cross-machine equality is
        // NOT pinned: sin() lowers to platform libm, whose last-ulp results
        // differ across OS/architecture — which is also why capture baselines
        // are tolerance-diffed rather than compared exactly.
        for i in 0..40 {
            #[allow(
                clippy::cast_precision_loss,
                clippy::as_conversions,
                reason = "test loop counter"
            )]
            let t = i as f32 * 7.3;
            assert_eq!(attract_frame(t, bounds()), attract_frame(t, bounds()));
        }
    }

    #[test]
    fn total_power_stays_far_below_old_grab() {
        // Sweep one FULL schedule recurrence at 50 ms. The pulse periods
        // (14 / 19 / 23.5 s = 28 / 38 / 47 half-seconds) realign exactly
        // every lcm(28, 38, 47) / 2 = 12,502 s, so this sweep covers every
        // pulse concurrency the schedule can ever produce — a shorter sweep
        // would be an empirical claim about a sliver of the cycle.
        //
        // Analytic worst case: all PULSE_COUNT pulses cresting simultaneously
        // = PULSE_COUNT · PULSE_PEAK_POWER = 1.05. The recurrence does
        // contain one triple NEAR-coincidence (t ≈ 12,282.7 s, all three
        // envelopes ≈ 0.98, measured total ≈ 1.03), so the honest ceiling is
        // the analytic bound, not "two pulses". Aesthetic judgment: even
        // that rare 1.05-bounded spike is ~15× below the old grab total
        // (2·7.0 + dreamers ≈ 15.7) and reads as a slightly firmer nudge,
        // not a vortex.
        const RECURRENCE_SECS: f32 = 12_502.0;
        const STEP_SECS: f32 = 0.05;
        #[allow(
            clippy::cast_precision_loss,
            clippy::as_conversions,
            reason = "test bound arithmetic"
        )]
        let analytic_bound = PULSE_COUNT as f32 * PULSE_PEAK_POWER;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::as_conversions,
            reason = "test iteration count"
        )]
        let steps = (RECURRENCE_SECS / STEP_SECS) as u32; // 250,040 samples
        let mut max_total = 0.0_f32;
        for i in 0..=steps {
            #[allow(
                clippy::cast_precision_loss,
                clippy::as_conversions,
                reason = "test loop counter"
            )]
            let t = i as f32 * STEP_SECS;
            let f = attract_frame(t, bounds());
            let total: f32 = f.pulses.iter().map(|p| p.power).sum();
            max_total = max_total.max(total);
        }
        assert!(
            max_total <= analytic_bound + 1e-3,
            "total power must stay within the analytic bound \
             (PULSE_COUNT × PULSE_PEAK_POWER = {analytic_bound}), got {max_total}"
        );
        // The triple near-coincidence is real: the sweep must actually find
        // it (> 1.0), proving the recurrence coverage isn't vacuous.
        assert!(
            max_total > 1.0,
            "full-recurrence sweep should hit the known triple \
             near-coincidence (~1.03), got {max_total}"
        );
        // And the ceiling is still nowhere near the old phantom-hand grab.
        assert!(max_total < OLD_HAND_PEAK_POWER * 0.2);
    }

    #[test]
    fn perturbances_leave_settled_stretches() {
        // Duty cycle over a 10-minute sweep. With only the three wandering
        // pulses (~1 s of meaningful envelope each, per 14 / 19 / 23.5 s), the
        // field is pulse-active (activity > 0.05) ~16 % of the time and fully
        // settled the rest — the brief is "mostly undisturbed field that slowly
        // morphs" (the morphing is the turbulence's job), not continuous churn.
        let mut active = 0_u32;
        let n = 12_000_u32;
        for i in 0..n {
            #[allow(
                clippy::cast_precision_loss,
                clippy::as_conversions,
                reason = "test loop counter"
            )]
            let t = i as f32 * 0.05;
            if attract_frame(t, bounds()).activity > 0.05 {
                active += 1;
            }
        }
        #[allow(
            clippy::cast_precision_loss,
            clippy::as_conversions,
            reason = "test ratio"
        )]
        let duty = active as f32 / n as f32;
        assert!(
            duty < 0.35,
            "the field should be settled most of the time, duty = {duty}"
        );
        assert!(duty > 0.08, "pulses should actually fire, duty = {duty}");
    }

    #[test]
    fn settled_field_has_zero_power() {
        // In the settled field every pulse point is fully off — the picture
        // is undisturbed and the kernel sits in inertial drag (a nonzero
        // ambient integrates nearly undamped into collapse; see
        // AMBIENT_POWER).
        let f = attract_frame(0.0, bounds());
        for p in &f.pulses {
            assert_eq!(p.power, 0.0);
        }
        assert_eq!(AMBIENT_POWER, 0.0, "ambient must stay zero (see its doc)");
        assert_eq!(f.activity, 0.0);
    }

    #[test]
    fn peak_pulse_power_is_gentle() {
        // Walker 0's crest (t = 4.9): its sample carries the full peak power
        // and that peak is well under the old phantom-hand 7.0.
        let f = attract_frame(4.0 + PULSE_ON_SECS * 0.5, bounds());
        assert!((f.pulses[0].power - PULSE_PEAK_POWER).abs() < 1e-3);
        assert!(f.pulses[0].power < OLD_HAND_PEAK_POWER * 0.1);
        assert!((f.activity - 1.0).abs() < 1e-3);
    }

    #[test]
    fn walkers_stay_inside_bounds() {
        let b = bounds();
        for i in 0..2_000 {
            #[allow(
                clippy::cast_precision_loss,
                clippy::as_conversions,
                reason = "test loop counter"
            )]
            let t = i as f32 * 0.5;
            let f = attract_frame(t, b);
            for p in &f.pulses {
                assert!(p.position[0].abs() <= b.half_w * WANDER_X_FRAC + 1e-3);
                assert!(p.position[1].abs() <= b.half_h * WANDER_Y_FRAC + 1e-3);
            }
        }
    }

    #[test]
    fn focal_relaxes_to_center_when_settled() {
        // No active pulse → centroid is the virtual center sample only.
        let f = attract_frame(0.0, bounds());
        assert_eq!(f.focal_world, [0.0, 0.0]);
        // At a pulse crest the focal sits near (biased slightly center-ward
        // of) the pulsing walker: focal = pos / (1 + W₀).
        let crest = attract_frame(4.0 + PULSE_ON_SECS * 0.5, bounds());
        let expect = [
            crest.pulses[0].position[0] / (1.0 + 0.15),
            crest.pulses[0].position[1] / (1.0 + 0.15),
        ];
        assert!((crest.focal_world[0] - expect[0]).abs() < 1.0);
        assert!((crest.focal_world[1] - expect[1]).abs() < 1.0);
    }
}
