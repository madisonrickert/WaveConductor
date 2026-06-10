//! Pure attract-mode choreography math for the Line sketch.
//!
//! All functions here are deterministic functions of elapsed time and window
//! geometry — no `World`, no resources, no RNG — so the wandering-pulse paths
//! and envelopes are unit-testable in isolation and reproduce exactly under
//! the fixed-`dt` capture clock.
//!
//! ## Composition: "Wandering Pulses + Meteors"
//!
//! ```text
//! SETTLED FIELD (base)     zero ambient attraction — the particle "picture"
//!         │                 stays readable; post-pulse coasting under
//!         │                 inertial drag supplies the between-pulse motion
//!         │                 (see AMBIENT_POWER for why it must be zero)
//!         ▼ every walker period (14 / 19 / 23.5 s, staggered)
//! WANDERING PULSE          one of PULSE_COUNT (3) points — each tracing a
//!         │                 slow incommensurate-frequency Lissajous path
//!         │                 across the frame — swells to PULSE_PEAK_POWER
//!         │                 (0.35) for PULSE_ON_SECS (1.2 s), nudging the
//!         │                 field, then releases
//!         ▼ every lane period (29 / 43 s, staggered)
//! METEOR                   one of METEOR_COUNT (2) invisible attractors
//!                           crosses the frame in METEOR_CROSS_SECS (4 s)
//!                           on a per-cycle hashed straight-to-gently-curved
//!                           trajectory at METEOR_PEAK_POWER (0.12) — the
//!                           moving pull drags particles into a comet wake
//!                           that the gravity smear's focal point follows
//! ```
//!
//! Design intent (operator art direction): a mostly undisturbed field with
//! minor perturbances — explicitly **not** a vortex — plus 1–2 "meteors" per
//! ~15–25 s window whose moving attraction drags the field into comet wakes
//! rather than collapsing it toward a fixed point. Meteor trajectories are a
//! pure hash of the lane's cycle number (no RNG), so the composition stays a
//! deterministic function of `t` and reproduces exactly under the capture
//! clock. The walker periods are mutually incommensurate and phase-staggered
//! so pulses rarely sync; the meteor lane periods (29 / 43 s) are coprime to
//! each other and incommensurate with the walkers. Across a full pulse
//! recurrence (12,502 s) the pulses' instantaneous total raw power is bounded
//! by `PULSE_COUNT · PULSE_PEAK_POWER = 1.05`; meteors add at most
//! `METEOR_COUNT · METEOR_PEAK_POWER = 0.56` more, keeping the combined
//! worst case ~10× below the old phantom-hand grab's 15.7. The attract-mode
//! lifetime respawn (`simulate.wgsl`) continuously heals whatever the wakes
//! displace, so the picture re-forms between passes.

/// World-space attractor sample: position + raw power, ready to bake into a
/// [`crate::line::particle::Attractor`] (after the caller multiplies power by
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

/// Number of meteor lanes (independent crossing schedules).
pub const METEOR_COUNT: usize = 2;

/// Full attract-frame snapshot the driver writes to the sim/post params.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttractFrame {
    /// The wandering pulse points. Power is zero at rest ([`AMBIENT_POWER`]),
    /// swelling to [`PULSE_PEAK_POWER`] at the crest of each walker's pulse.
    pub pulses: [AttractorSample; PULSE_COUNT],
    /// The meteor points — invisible attractors crossing the frame, dragging
    /// particles into comet wakes. Power is zero between crossings, plateauing
    /// at [`METEOR_PEAK_POWER`] across the middle of each pass.
    pub meteors: [AttractorSample; METEOR_COUNT],
    /// World-space focal point for the gravity smear (`i_mouse`): the
    /// envelope-weighted centroid of the pulse + meteor points, relaxing
    /// toward screen center when nothing is active (see [`attract_frame`] for
    /// the math). During a meteor pass the smear visibly trails the meteor —
    /// the "comet glow".
    pub focal_world: [f32; 2],
    /// Overall activity `0..=1` — 0 in the settled field, 1 at the crest of a
    /// pulse or the plateau of a meteor pass. The smear `g_constant` scales
    /// with this.
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

/// Wander-path amplitude as a fraction of the half-width. Wide enough that
/// the pulses visit most of the frame over minutes.
const WANDER_X_FRAC: f32 = 0.72;

/// Wander-path amplitude as a fraction of the half-height.
const WANDER_Y_FRAC: f32 = 0.62;

/// Per-walker wander path + pulse schedule.
///
/// Each walker traces a Lissajous figure:
/// `x = half_w · WANDER_X_FRAC · sin(freq.x · t + phase.x)` (same for y) —
/// the x/y frequencies are mutually incommensurate so the figure never
/// closes and the point eventually visits the whole amplitude box.
struct Walker {
    /// Lissajous angular frequencies (rad/s) for x / y. ~0.03–0.05 rad/s:
    /// one frame-crossing (half a sine cycle, `π/ω`) takes ~60–110 s, so the
    /// path spans the frame over minutes, not a tight orbit.
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
        freq: [0.047, 0.031],
        phase: [0.0, 1.7],
        period: 14.0,
        offset: 10.0,
    },
    Walker {
        freq: [0.029, 0.041],
        phase: [2.1, 4.0],
        period: 19.0,
        offset: 10.9,
    },
    Walker {
        freq: [0.037, 0.053],
        phase: [4.4, 0.9],
        period: 23.5,
        offset: 8.5,
    },
];

/// Seconds a meteor takes to cross the frame. Long enough to read as a
/// deliberate traveling body (not a flick), short enough that the pull at any
/// one point along the path is brief — the wake forms and releases instead of
/// gathering the field.
///
/// Capture-tuned alongside [`METEOR_PEAK_POWER`]: at 5.5 s the final seconds
/// of integration still gathered the line into a hook even at low power;
/// 4.0 s caps the per-pass impulse near the mid-pass look (a clean traveling
/// wave) and reads faster and more meteor-like (~350 px/s).
pub const METEOR_CROSS_SECS: f32 = 4.0;

/// Peak raw meteor power (pre-`gravity_constant`). Well below
/// [`PULSE_PEAK_POWER`]: the kernel's attractor force is constant-magnitude
/// over the whole frame, so a *sustained* pull does far more cumulative work
/// than a 1.2 s pulse (≈4.6 s at the plateau ≈ 6× a pulse's impulse at equal
/// power) — but unlike a stationary pulse the meteor's pull direction sweeps
/// as it travels, so particles are dragged *along* the path (a wake) instead
/// of monotonically inward (a collapse). The attract-mode lifetime respawn
/// then heals the displaced particles between passes.
///
/// Capture-tuned: 0.28 wound the entire line into a comet *curl* by the end
/// of each crossing — striking, but the picture was unreadable for most of
/// the pass-to-pass interval (operator brief: "definitely not full-on
/// vortex", "retain some integrity to the original image"). 0.12 leaves the
/// line legible through the pass while the wake clearly sweeps it.
pub const METEOR_PEAK_POWER: f32 = 0.12;

/// Fraction of the crossing (`u` in `0..=1`) spent ramping the meteor's power
/// in at the start and out at the end. The ramps coincide with the path
/// segments where the meteor is (or may be) off-screen, so full power only
/// ever applies mid-frame.
const METEOR_RAMP_FRAC: f32 = 0.18;

/// Meteor path length as a multiple of the half-width: long enough that every
/// crossing enters and exits past the frame edge (the wake runs off-screen
/// instead of stopping dead mid-frame).
const METEOR_PATH_LEN_HALF_WIDTHS: f32 = 2.2;

/// Half-extent (as a fraction of each half-dimension) of the box the meteor's
/// path midpoint is hashed into. Keeps every crossing passing through the
/// central region of the frame where the particle picture lives.
const METEOR_MID_BOX_FRAC: f32 = 0.45;

/// Maximum perpendicular bow of a curved crossing, as a fraction of the path
/// length. Hashed per cycle in `±` this range: 0 = straight, the extreme is a
/// gentle arc (~10 % of the path length at its apex).
const METEOR_CURVE_FRAC: f32 = 0.10;

/// One meteor lane: an independent, deterministic crossing schedule.
///
/// The lane fires once per `period`, crossing for [`METEOR_CROSS_SECS`]; the
/// trajectory of each crossing is hashed from the lane's cycle number, so the
/// path varies pass to pass while remaining a pure function of `t`.
struct MeteorLane {
    /// Crossing repeat period (s). The two lane periods (29 / 43) are coprime
    /// and incommensurate with the walker periods, so meteor/meteor and
    /// meteor/pulse coincidences are rare and transient. Combined cadence
    /// averages one meteor per ~17 s — the operator's "1–2 per ~15–25 s
    /// window".
    period: f32,
    /// Schedule offset (s): the lane's crossing starts at
    /// `t = period − offset (mod period)`. Staggered so the first crossings
    /// land at t = 7.5 s / 20.0 s — after the fade-in and walker 0's first
    /// pulse, and never simultaneously.
    offset: f32,
    /// Per-lane hash salt so the two lanes draw independent trajectories.
    salt: u32,
}

/// The meteor lanes. See [`MeteorLane`] for the period/offset rationale.
const METEOR_LANES: [MeteorLane; METEOR_COUNT] = [
    MeteorLane {
        period: 29.0,
        offset: 21.5,
        salt: 0x9E37_79B9,
    },
    MeteorLane {
        period: 43.0,
        offset: 23.0,
        salt: 0x85EB_CA6B,
    },
];

/// Center-bias weight in the smear-focal centroid: a virtual sample of this
/// weight pinned at the origin. Keeps the focal point defined (and smoothly
/// moving) when every pulse envelope is zero, instead of dividing by ~0 or
/// snapping between walkers.
const FOCAL_CENTER_WEIGHT: f32 = 0.15;

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

/// Hermite smoothstep over `e0..e1`, clamped — the standard GLSL/WGSL shape,
/// used for the meteor power ramps.
#[must_use]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (2.0_f32.mul_add(-t, 3.0))
}

/// Draw `N` decorrelated unit-interval hashes for a meteor cycle by chaining
/// [`crate::line::hash::wang_hash`] from `cycle ^ salt`.
#[must_use]
fn meteor_cycle_hashes<const N: usize>(cycle: u32, salt: u32) -> [f32; N] {
    let mut h = crate::line::hash::wang_hash(cycle ^ salt);
    let mut out = [0.0_f32; N];
    for slot in &mut out {
        *slot = crate::line::hash::hash_to_unit(h);
        h = crate::line::hash::wang_hash(h);
    }
    out
}

/// Meteor lane `lane`'s sample at time `t`: position along the current
/// crossing and its ramped power (zero between crossings).
///
/// The trajectory is a pure hash of the lane's cycle number — four unit
/// hashes pick the path midpoint (inside the central
/// [`METEOR_MID_BOX_FRAC`] box), the travel direction (full circle), and the
/// perpendicular bow (±[`METEOR_CURVE_FRAC`] of the path length) — so each
/// pass crosses a different part of the frame on a different heading, while
/// the same `t` always reproduces the same meteor (capture determinism).
///
/// Power ramps in/out over the leading/trailing [`METEOR_RAMP_FRAC`] of the
/// crossing and plateaus at [`METEOR_PEAK_POWER`] in between: the attractor
/// *leads* the wake at constant speed while its strength has no hard edges.
///
/// # Panics
///
/// Panics if `lane >= METEOR_COUNT` (invariant violation: callers iterate
/// `0..METEOR_COUNT`).
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "cycle index: t >= 0 and offset > 0 make the floor non-negative; \
              wrapping past u32::MAX would need a ~3,900-year session"
)]
pub fn meteor_sample(t: f32, lane: usize, bounds: Bounds) -> AttractorSample {
    let l = &METEOR_LANES[lane];
    let cycles = (t + l.offset) / l.period;
    let cycle = cycles.floor();
    let phase = cycles - cycle; // fractional cycle position, 0..1
    let on_frac = METEOR_CROSS_SECS / l.period;
    if phase >= on_frac {
        return AttractorSample {
            position: [0.0, 0.0],
            power: 0.0,
        };
    }
    // Crossing progress 0..1 and the per-cycle hashed trajectory parameters.
    let u = phase / on_frac;
    let [h_mid_x, h_mid_y, h_angle, h_curve] = meteor_cycle_hashes::<4>(cycle as u32, l.salt);

    // Path: a straight segment of length `path_len` through `mid`, bowed
    // perpendicular by a half-sine of amplitude `curve_amp`:
    //   pos(u) = mid + dir·(u − ½)·path_len + perp·curve_amp·sin(πu)
    // The bow's sin(πu) term is zero at both endpoints, so curvature never
    // changes where the crossing enters/exits — only how it sweeps mid-frame.
    let mid = [
        (2.0_f32.mul_add(h_mid_x, -1.0)) * METEOR_MID_BOX_FRAC * bounds.half_w,
        (2.0_f32.mul_add(h_mid_y, -1.0)) * METEOR_MID_BOX_FRAC * bounds.half_h,
    ];
    let theta = h_angle * std::f32::consts::TAU;
    let (dir_y, dir_x) = theta.sin_cos();
    let path_len = METEOR_PATH_LEN_HALF_WIDTHS * bounds.half_w;
    let s = (u - 0.5) * path_len;
    let curve_amp = (2.0_f32.mul_add(h_curve, -1.0)) * METEOR_CURVE_FRAC * path_len;
    let bow = curve_amp * (std::f32::consts::PI * u).sin();
    let position = [
        // perp = (−dir_y, dir_x): dir rotated +90°.
        dir_x.mul_add(s, -dir_y * bow) + mid[0],
        dir_y.mul_add(s, dir_x * bow) + mid[1],
    ];

    // Power: smooth in/out ramps bracketing a full-power plateau.
    let env =
        smoothstep(0.0, METEOR_RAMP_FRAC, u) * (1.0 - smoothstep(1.0 - METEOR_RAMP_FRAC, 1.0, u));
    AttractorSample {
        position,
        power: METEOR_PEAK_POWER * env,
    }
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

    // Meteors join the same centroid/activity accumulation, weighted by their
    // power envelope, so the gravity smear's focal point trails an active
    // meteor (the comet glow) and `g_constant` swells with the pass.
    let mut meteors = [AttractorSample {
        position: [0.0, 0.0],
        power: 0.0,
    }; METEOR_COUNT];
    for (i, slot) in meteors.iter_mut().enumerate() {
        let sample = meteor_sample(t, i, bounds);
        let env = sample.power / METEOR_PEAK_POWER;
        weighted_pos[0] += env * sample.position[0];
        weighted_pos[1] += env * sample.position[1];
        env_sum += env;
        *slot = sample;
    }

    let focal_denom = env_sum + FOCAL_CENTER_WEIGHT;
    let focal_world = [weighted_pos[0] / focal_denom, weighted_pos[1] / focal_denom];

    AttractFrame {
        pulses,
        meteors,
        focal_world,
        // Overall activity: total envelope, clamped — with the staggered
        // schedule this is effectively "the strongest perturbance right now".
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
        // Duty cycle over a 10-minute sweep: pulses are active ~16 % of the
        // time and a meteor is crossing ~23 % (4 s per 29 s + 4 s per 43 s —
        // the operator-specified "1–2 meteors per ~15–25 s window"), so
        // something is meaningfully active (activity > 0.05) roughly 35 % of
        // the time. The field must still spend a substantial share of the
        // loop fully settled — the brief is "mostly undisturbed field, minor
        // perturbances", not continuous churn.
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
            duty < 0.55,
            "the field should be settled a large share of the time, duty = {duty}"
        );
        assert!(
            duty > 0.2,
            "pulses + meteors should actually fire, duty = {duty}"
        );
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

    /// t at the middle of meteor lane 0's first crossing (window 7.5–11.5 s).
    /// The nearby pulse windows (walker 0: 4.0–5.2 s, walker 1: 8.1–9.3 s,
    /// walker 2: 15.0–16.2 s) all miss this instant, so the meteor is the
    /// sole active sample at `LANE0_MID_T`.
    const LANE0_MID_T: f32 = 9.5;

    #[test]
    fn meteors_fire_only_inside_their_window() {
        // Lane 0's first window is t ∈ [7.5, 11.5); lane 1's is [20.0, 24.0).
        for lane in 0..METEOR_COUNT {
            assert_eq!(meteor_sample(0.0, lane, bounds()).power, 0.0);
            assert_eq!(meteor_sample(6.5, lane, bounds()).power, 0.0);
        }
        let mid = meteor_sample(LANE0_MID_T, 0, bounds());
        assert!(
            (mid.power - METEOR_PEAK_POWER).abs() < 1e-4,
            "mid-crossing should plateau at peak power, got {}",
            mid.power
        );
        assert_eq!(meteor_sample(LANE0_MID_T, 1, bounds()).power, 0.0);
    }

    #[test]
    fn meteor_power_is_bounded_and_reaches_its_plateau() {
        // Sweep past several full cycles of both lanes (lcm(29, 43) = 1247 s).
        let mut max_power = 0.0_f32;
        for i in 0..26_000_u32 {
            #[allow(
                clippy::cast_precision_loss,
                clippy::as_conversions,
                reason = "test loop counter"
            )]
            let t = i as f32 * 0.05;
            for lane in 0..METEOR_COUNT {
                let s = meteor_sample(t, lane, bounds());
                assert!(s.power >= 0.0);
                max_power = max_power.max(s.power);
            }
        }
        assert!(
            max_power <= METEOR_PEAK_POWER + 1e-3,
            "meteor power must never exceed the peak, got {max_power}"
        );
        assert!(
            max_power > METEOR_PEAK_POWER * 0.99,
            "the plateau should actually be reached, got {max_power}"
        );
        // The combined analytic worst case (all pulses + all meteors cresting
        // at once) stays far below the old phantom-hand grab total (~15.7).
        #[allow(
            clippy::cast_precision_loss,
            clippy::as_conversions,
            reason = "test bound arithmetic"
        )]
        let combined_bound =
            PULSE_COUNT as f32 * PULSE_PEAK_POWER + METEOR_COUNT as f32 * METEOR_PEAK_POWER;
        assert!(combined_bound < OLD_HAND_PEAK_POWER * 0.25);
    }

    #[test]
    fn meteor_travels_across_the_frame() {
        // Positions at 20 % and 80 % of the crossing are far apart: the
        // attractor leads a moving wake, never sitting still. With a path of
        // 2.2 half-widths, 60 % of it spans 1.32 half-widths (the bow term
        // cancels: sin(0.2π) = sin(0.8π)).
        let b = bounds();
        let window_start = 7.5;
        let early = meteor_sample(window_start + 0.2 * METEOR_CROSS_SECS, 0, b);
        let late = meteor_sample(window_start + 0.8 * METEOR_CROSS_SECS, 0, b);
        let dx = late.position[0] - early.position[0];
        let dy = late.position[1] - early.position[1];
        let dist = dx.hypot(dy);
        assert!(
            (dist - 1.32 * b.half_w).abs() < 1.0,
            "60 % of the crossing should span 1.32 half-widths, got {dist}"
        );
    }

    #[test]
    fn meteor_trajectories_vary_between_cycles() {
        // The same lane draws a fresh hashed trajectory each cycle: the
        // mid-crossing positions of consecutive cycles must differ visibly.
        let b = bounds();
        let a = meteor_sample(LANE0_MID_T, 0, b);
        let next = meteor_sample(LANE0_MID_T + 29.0, 0, b);
        let dx = next.position[0] - a.position[0];
        let dy = next.position[1] - a.position[1];
        assert!(
            dx.hypot(dy) > 50.0,
            "consecutive crossings should take different paths, got {} px apart",
            dx.hypot(dy)
        );
        // Both at the plateau — the schedule itself is periodic, only the
        // path varies.
        assert!((a.power - next.power).abs() < 1e-4);
    }

    #[test]
    fn meteor_passes_stay_in_reach_of_the_frame() {
        // While at meaningful power (env > 0.5) the meteor must be near the
        // visible frame — pulling toward a far-off-screen point would drag
        // the whole field off-frame. Bound: mid-box (0.45) + half the
        // mid-power path span (|u−½| ≤ 0.3 → 0.66 half-widths) + max bow
        // (0.22 half-widths) ≈ 1.35 half-widths, with a small margin.
        let b = bounds();
        for i in 0..26_000_u32 {
            #[allow(
                clippy::cast_precision_loss,
                clippy::as_conversions,
                reason = "test loop counter"
            )]
            let t = i as f32 * 0.05;
            for lane in 0..METEOR_COUNT {
                let s = meteor_sample(t, lane, b);
                if s.power > METEOR_PEAK_POWER * 0.5 {
                    let limit = 1.4 * b.half_w;
                    assert!(
                        s.position[0].abs() < limit && s.position[1].abs() < limit,
                        "meteor at meaningful power too far off-frame: {:?} at t={t}",
                        s.position
                    );
                }
            }
        }
    }

    #[test]
    fn focal_follows_an_active_meteor() {
        // Mid-crossing with no pulse active, the meteor is the only weighted
        // sample: focal = pos / (1 + FOCAL_CENTER_WEIGHT) — the smear's glow
        // visibly trails the meteor.
        let f = attract_frame(LANE0_MID_T, bounds());
        let m = f.meteors[0];
        assert!((f.activity - 1.0).abs() < 1e-3);
        let expect = [m.position[0] / 1.15, m.position[1] / 1.15];
        assert!((f.focal_world[0] - expect[0]).abs() < 1.0);
        assert!((f.focal_world[1] - expect[1]).abs() < 1.0);
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
