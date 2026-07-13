//! Pure soak analysis: turn a series of samples into a trend fit and a verdict.
//!
//! Everything here is a pure function over `&[Sample]` so the leak detector can
//! be unit-tested against synthetic series (flat, climbing, noisy-but-flat,
//! sawtooth) in milliseconds instead of by running an eight-hour soak. That is
//! the whole reason the analysis lives apart from the orchestration in
//! [`crate::soak`].
//!
//! ## What is decided mechanically, and what is not
//!
//! Mechanical (a machine can be trusted with these):
//! - The app died, hung, or froze. Unambiguous.
//! - RSS rises with a steep, well-fit slope over hours. A least-squares fit
//!   plus its r² separates "climbing" from "noisy but level".
//! - FPS in the last quarter of the run is materially below the first quarter.
//!
//! Not mechanical: *is this drift acceptable?* A 3 MiB/hour climb might be a
//! bounded cache filling to its ceiling, or the first hour of a slow leak. The
//! honest answer for the middle band is [`Verdict::Review`] — emit the numbers,
//! name the artifact, and let the operating agent (or Madison) judge. This tool
//! never fabricates a pass.

use serde::{Deserialize, Serialize};

/// One joined sample: the app's self-reported health plus the externally
/// measured RSS, taken by the launcher on its own schedule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sample {
    /// Seconds since the app was launched, on the launcher's wall clock.
    pub t_secs: f64,
    /// Resident set size in KiB, measured from outside the process. `None` when
    /// the platform RSS probe failed (see [`crate::soak::rss`]).
    pub rss_kib: Option<u64>,
    /// The app's own uptime, from its `health.json` snapshot. Failure of *this*
    /// to advance between samples is the freeze signal.
    pub app_uptime_secs: Option<f64>,
    /// Smoothed FPS from the app's diagnostics.
    pub fps: Option<f64>,
    /// Current top-level state (`Line`, `Dots`, ...).
    pub state: Option<String>,
    /// Current sketch activity (`Active` / `Idle` / `Screensaver`).
    pub activity: Option<String>,
    /// Current thermal tier (`cool` / `warm` / `hot`).
    pub thermal_tier: Option<String>,
    /// Latest raw temperature in Celsius, when a sensor produced one.
    pub thermal_temp_c: Option<f64>,
    /// Sketch advances the app has performed so far.
    pub cycles: Option<u64>,
}

/// How the app's process ended. The launcher supplies this; it is not
/// derivable from the samples alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The app self-exited after its configured duration. The only good ending.
    Completed,
    /// The app did not exit cleanly at the end of its run: it either quit early
    /// (crash, panic, a stray window close) or exited with a failure status.
    ExitedEarly,
    /// The app was still running past `duration + grace` and had to be killed.
    TimedOut,
}

/// Overall soak verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Ran to completion with no leak trend, no FPS decay, and no freeze.
    Pass,
    /// Nothing failed outright, but something needs human/agent judgment
    /// (a mild upward RSS trend, mild FPS decay, or too few samples to fit).
    Review,
    /// A mechanically-certain failure: died, froze, leaked, or decayed.
    Fail,
}

impl Verdict {
    /// The more severe of two verdicts (`Fail` > `Review` > `Pass`).
    fn worst(self, other: Self) -> Self {
        match (self, other) {
            (Self::Fail, _) | (_, Self::Fail) => Self::Fail,
            (Self::Review, _) | (_, Self::Review) => Self::Review,
            _ => Self::Pass,
        }
    }
}

/// A least-squares linear fit of one metric against wall-clock time, plus the
/// summary statistics that make the fit interpretable.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Trend {
    /// Fitted slope, in metric units per hour.
    pub slope_per_hour: f64,
    /// Coefficient of determination (0..=1). Near 1 = a clean straight climb;
    /// near 0 = noise with no trend. This is what separates a real leak from a
    /// jittery-but-level series.
    pub r_squared: f64,
    /// Mean of the metric over the first quarter of the samples.
    pub first_quarter_mean: f64,
    /// Mean of the metric over the last quarter of the samples.
    pub last_quarter_mean: f64,
    /// `last_quarter_mean - first_quarter_mean`.
    pub delta: f64,
    /// Minimum observed value.
    pub min: f64,
    /// Maximum observed value.
    pub max: f64,
    /// Number of points in the fit.
    pub n: usize,
}

/// Thresholds the verdict is drawn against. Grouped into one struct (rather
/// than free constants) so tests can drive the classifier at its boundaries
/// without recompiling, and so the values appear verbatim in `run.json`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Thresholds {
    /// RSS slope at or above which the run fails outright (MiB/hour). At the
    /// default, an 8-hour run would have to gain ~160 MiB on a straight line.
    pub rss_fail_mib_per_hour: f64,
    /// RSS slope at or above which the run needs review (MiB/hour).
    pub rss_review_mib_per_hour: f64,
    /// Absolute growth below which any slope is treated as noise (MiB). A
    /// 30-minute smoke run can extrapolate a scary hourly slope out of a few
    /// MiB of startup jitter; this floor stops that.
    pub rss_noise_floor_mib: f64,
    /// Minimum r² for a positive RSS slope to count as a *trend* rather than
    /// noise. A sawtooth (allocate/release) fits badly and is not a leak.
    pub rss_min_r_squared: f64,
    /// Fractional FPS drop (first quarter -> last quarter) that fails the run.
    pub fps_fail_decay: f64,
    /// Fractional FPS drop that needs review.
    pub fps_review_decay: f64,
    /// Samples needed before a trend fit is trusted at all.
    pub min_trend_samples: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            rss_fail_mib_per_hour: 20.0,
            rss_review_mib_per_hour: 5.0,
            rss_noise_floor_mib: 16.0,
            rss_min_r_squared: 0.5,
            fps_fail_decay: 0.20,
            fps_review_decay: 0.05,
            min_trend_samples: 8,
        }
    }
}

/// The full analysis of a soak run: the verdict, why, and the trends behind it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Analysis {
    /// Overall verdict.
    pub verdict: Verdict,
    /// Mechanically-certain failures, in plain language. Empty unless
    /// `verdict == Fail`.
    pub failures: Vec<String>,
    /// Things a human or agent must judge. Empty unless `verdict == Review`
    /// (or the run also failed for another reason).
    pub review: Vec<String>,
    /// RSS trend in MiB. `None` when no sample carried an RSS reading.
    pub rss: Option<Trend>,
    /// FPS trend over `Active` samples only — the screensaver's deliberate
    /// low present rate is not a regression and must not be read as one.
    pub fps_active: Option<Trend>,
    /// Fractional FPS drop from the first to the last quarter of the active
    /// samples (0.1 = 10% slower). Negative = it got faster.
    pub fps_decay: Option<f64>,
    /// Launcher wall-clock times (seconds) at which the app's own clock had not
    /// advanced since the previous sample — i.e. it was frozen.
    pub freezes: Vec<f64>,
    /// Sketch advances the app reported performing.
    pub cycles: u64,
    /// Number of samples analyzed.
    pub samples: usize,
    /// Thresholds this verdict was drawn against.
    pub thresholds: Thresholds,
}

/// KiB per MiB, as an `f64` (the analysis works in MiB throughout).
const KIB_PER_MIB: f64 = 1024.0;

/// Convert an RSS reading in KiB to MiB. Isolated so the lossy-cast allowance
/// sits on one three-line function rather than inside a filter chain: an RSS in
/// KiB is far below `2^53`, so the `f64` conversion is exact in practice.
#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "u64 -> f64 has no From impl; an RSS in KiB is far below 2^53, so the conversion is exact"
)]
fn kib_to_mib(kib: u64) -> f64 {
    kib as f64 / KIB_PER_MIB
}

/// Seconds per hour, for the per-hour slope normalization.
const SECS_PER_HOUR: f64 = 3600.0;

/// The app's clock must advance by at least this many seconds between two
/// launcher samples, or the render loop is considered frozen. The launcher
/// samples far more slowly than the app republishes `health.json`, so any
/// healthy app advances by (roughly) the whole sample interval; a threshold
/// this small only fires on a genuine freeze, never on scheduling jitter.
const FREEZE_ADVANCE_SECS: f64 = 0.5;

/// Least-squares fit of `(t_secs, value)` points, normalized to a per-hour
/// slope, with r² and the quartile summaries.
///
/// Returns `None` for fewer than two points, or when every sample lands at the
/// same instant (a vertical fit has no slope).
#[must_use]
pub fn linear_trend(points: &[(f64, f64)]) -> Option<Trend> {
    let n = points.len();
    if n < 2 {
        return None;
    }
    #[allow(
        clippy::as_conversions,
        clippy::cast_precision_loss,
        reason = "usize -> f64 has no From impl; sample counts are in the thousands, exact in f64"
    )]
    let n_f = n as f64;

    let mean_t = points.iter().map(|p| p.0).sum::<f64>() / n_f;
    let mean_v = points.iter().map(|p| p.1).sum::<f64>() / n_f;

    // Covariance of time with the value, and the two variances. Named for what
    // they are rather than the textbook's `Sxy`/`Sxx`/`Syy` so no two locals
    // read alike.
    let mut covariance = 0.0;
    let mut time_variance = 0.0;
    let mut value_variance = 0.0;
    for &(t, v) in points {
        let dt = t - mean_t;
        let dv = v - mean_v;
        covariance += dt * dv;
        time_variance += dt * dt;
        value_variance += dv * dv;
    }
    if time_variance <= 0.0 {
        return None; // every point at the same time: no slope exists
    }
    let slope_per_sec = covariance / time_variance;
    // r² = explained variance / total variance. A perfectly flat series has
    // zero total variance and is, definitionally, perfectly explained: r² = 1.
    let r_squared = if value_variance <= 0.0 {
        1.0
    } else {
        ((covariance * covariance) / (time_variance * value_variance)).clamp(0.0, 1.0)
    };

    let quarter = (n / 4).max(1);
    let mean_of = |slice: &[(f64, f64)]| -> f64 {
        #[allow(
            clippy::as_conversions,
            clippy::cast_precision_loss,
            reason = "usize -> f64 has no From impl; slice lengths are in the thousands at most"
        )]
        let len = slice.len() as f64;
        slice.iter().map(|p| p.1).sum::<f64>() / len
    };
    let first_quarter_mean = mean_of(&points[..quarter]);
    let last_quarter_mean = mean_of(&points[n - quarter..]);

    Some(Trend {
        slope_per_hour: slope_per_sec * SECS_PER_HOUR,
        r_squared,
        first_quarter_mean,
        last_quarter_mean,
        delta: last_quarter_mean - first_quarter_mean,
        min: points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min),
        max: points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max),
        n,
    })
}

/// Launcher wall-clock times at which the app's own clock failed to advance
/// since the previous sample — the freeze detector.
///
/// Samples with no `app_uptime_secs` (an unreadable or not-yet-written
/// `health.json`) are skipped rather than treated as freezes: the app may
/// simply not have published its first snapshot yet.
#[must_use]
pub fn detect_freezes(samples: &[Sample]) -> Vec<f64> {
    let mut freezes = Vec::new();
    let mut prev: Option<f64> = None;
    for s in samples {
        let Some(uptime) = s.app_uptime_secs else {
            continue;
        };
        if let Some(p) = prev {
            if uptime - p < FREEZE_ADVANCE_SECS {
                freezes.push(s.t_secs);
            }
        }
        prev = Some(uptime);
    }
    freezes
}

/// Analyze a completed (or aborted) soak run.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one linear classifier: outcome, then freezes, then RSS, then FPS. Each branch \
              carries the operator-facing sentence it emits; splitting them apart would \
              scatter the verdict logic across four functions to satisfy a line count"
)]
pub fn analyze(samples: &[Sample], outcome: Outcome, thresholds: Thresholds) -> Analysis {
    let mut failures: Vec<String> = Vec::new();
    let mut review: Vec<String> = Vec::new();
    let mut verdict = Verdict::Pass;

    match outcome {
        Outcome::ExitedEarly => {
            failures.push(
                "the app did not exit cleanly at the end of its run — it quit before its \
                 configured duration elapsed, or exited with a failure status. Read app.log for \
                 the panic or error that ended it"
                    .to_string(),
            );
            verdict = Verdict::Fail;
        }
        Outcome::TimedOut => {
            failures.push(
                "the app was still running past its duration + grace window and had to be killed \
                 — it never processed its own AppExit, which is itself a hang"
                    .to_string(),
            );
            verdict = Verdict::Fail;
        }
        Outcome::Completed => {}
    }

    let freezes = detect_freezes(samples);
    if !freezes.is_empty() {
        failures.push(format!(
            "the app's clock stopped advancing at {} sample(s) (first at t={:.0}s) — the render \
             loop froze",
            freezes.len(),
            freezes.first().copied().unwrap_or_default(),
        ));
        verdict = Verdict::Fail;
    }

    // --- RSS: the leak signal ------------------------------------------------
    let rss_points: Vec<(f64, f64)> = samples
        .iter()
        .filter_map(|s| s.rss_kib.map(|k| (s.t_secs, kib_to_mib(k))))
        .collect();
    let rss = linear_trend(&rss_points);

    if rss_points.is_empty() {
        review.push(
            "no RSS readings were captured — the leak signal is missing entirely (is the platform \
             RSS probe supported here?)"
                .to_string(),
        );
        verdict = verdict.worst(Verdict::Review);
    } else if rss_points.len() < thresholds.min_trend_samples {
        review.push(format!(
            "only {} RSS sample(s) — fewer than the {} needed to trust a trend fit; read the \
             samples yourself rather than the slope",
            rss_points.len(),
            thresholds.min_trend_samples,
        ));
        verdict = verdict.worst(Verdict::Review);
    } else if let Some(t) = rss {
        let above_noise = t.delta >= thresholds.rss_noise_floor_mib;
        let well_fit = t.r_squared >= thresholds.rss_min_r_squared;
        if t.slope_per_hour >= thresholds.rss_fail_mib_per_hour && above_noise && well_fit {
            failures.push(format!(
                "RSS climbed {:.1} MiB/hour (r²={:.2}, {:.0} MiB gained over the run) — a \
                 sustained, well-fit climb at or above the {:.0} MiB/hour leak threshold",
                t.slope_per_hour, t.r_squared, t.delta, thresholds.rss_fail_mib_per_hour,
            ));
            verdict = Verdict::Fail;
        } else if t.slope_per_hour >= thresholds.rss_review_mib_per_hour && above_noise {
            review.push(format!(
                "RSS trended up {:.1} MiB/hour (r²={:.2}, {:.0} MiB gained) — above the {:.0} \
                 MiB/hour review threshold but not conclusive. Is this a bounded cache filling to \
                 its ceiling, or the start of a leak? Plot the samples and judge.",
                t.slope_per_hour, t.r_squared, t.delta, thresholds.rss_review_mib_per_hour,
            ));
            verdict = verdict.worst(Verdict::Review);
        } else if t.slope_per_hour >= thresholds.rss_fail_mib_per_hour && !well_fit {
            // Steep but badly fit: a sawtooth or a single step, not a leak line.
            review.push(format!(
                "RSS has a steep fitted slope ({:.1} MiB/hour) but fits the line badly \
                 (r²={:.2}) — that is a sawtooth or a one-time step, not a straight climb. Look \
                 at the shape before calling it a leak.",
                t.slope_per_hour, t.r_squared,
            ));
            verdict = verdict.worst(Verdict::Review);
        }
    }

    // --- FPS: the thermal / GPU-saturation signal ---------------------------
    // Active samples only: the screensaver's low present rate is a designed
    // thermal lever (down to ~3 fps at the Hot tier), and folding it into the
    // decay fit would report the app's own heat management as a regression.
    let fps_points: Vec<(f64, f64)> = samples
        .iter()
        .filter(|s| s.activity.as_deref() == Some("Active"))
        .filter_map(|s| s.fps.map(|f| (s.t_secs, f)))
        .collect();
    let fps_active = linear_trend(&fps_points);
    let mut fps_decay = None;

    if fps_points.len() < thresholds.min_trend_samples {
        review.push(format!(
            "only {} active-sketch FPS sample(s) — fewer than the {} needed to judge decay",
            fps_points.len(),
            thresholds.min_trend_samples,
        ));
        verdict = verdict.worst(Verdict::Review);
    } else if let Some(t) = fps_active {
        if t.first_quarter_mean > 0.0 {
            let decay = (t.first_quarter_mean - t.last_quarter_mean) / t.first_quarter_mean;
            fps_decay = Some(decay);
            if decay >= thresholds.fps_fail_decay {
                failures.push(format!(
                    "FPS decayed {:.0}% ({:.1} -> {:.1} fps, first quarter to last) — a thermal \
                     stall or a GPU-saturation regression",
                    decay * 100.0,
                    t.first_quarter_mean,
                    t.last_quarter_mean,
                ));
                verdict = Verdict::Fail;
            } else if decay >= thresholds.fps_review_decay {
                review.push(format!(
                    "FPS drifted down {:.0}% ({:.1} -> {:.1} fps) — under the failure threshold, \
                     but check the thermal tier column before shipping",
                    decay * 100.0,
                    t.first_quarter_mean,
                    t.last_quarter_mean,
                ));
                verdict = verdict.worst(Verdict::Review);
            }
        }
    }

    Analysis {
        verdict,
        failures,
        review,
        rss,
        fps_active,
        fps_decay,
        freezes,
        cycles: samples.iter().filter_map(|s| s.cycles).max().unwrap_or(0),
        samples: samples.len(),
        thresholds,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "unwrap/expect are appropriate in test code — every value is constructed locally"
)]
mod tests {
    use super::*;

    /// Seconds between samples in the synthetic series (the launcher default).
    const DT: f64 = 30.0;

    /// MiB -> KiB, for building a fixture's RSS reading.
    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "test fixture: values are small positive MiB counts"
    )]
    fn mib_to_kib(mib: f64) -> u64 {
        (mib * KIB_PER_MIB) as u64
    }

    /// Sample index -> wall-clock seconds.
    #[allow(
        clippy::as_conversions,
        clippy::cast_precision_loss,
        reason = "test fixture: sample counts are in the hundreds"
    )]
    fn at(i: usize) -> f64 {
        i as f64 * DT
    }

    /// A sample at `t` seconds with `rss_mib` MiB resident and `fps` FPS, in an
    /// active sketch, with the app's clock tracking the launcher's.
    fn sample(t: f64, rss_mib: f64, fps: f64) -> Sample {
        Sample {
            t_secs: t,
            rss_kib: Some(mib_to_kib(rss_mib)),
            app_uptime_secs: Some(t),
            fps: Some(fps),
            state: Some("Line".to_string()),
            activity: Some("Active".to_string()),
            thermal_tier: Some("cool".to_string()),
            thermal_temp_c: Some(45.0),
            cycles: Some(0),
        }
    }

    /// `n` samples 30 s apart, whose RSS and FPS are functions of the elapsed
    /// wall-clock seconds. `series(960, …)` is an 8-hour run.
    fn series(n: usize, rss: impl Fn(f64) -> f64, fps: impl Fn(f64) -> f64) -> Vec<Sample> {
        (0..n)
            .map(|i| {
                let t = at(i);
                sample(t, rss(t), fps(t))
            })
            .collect()
    }

    // ---- linear_trend -------------------------------------------------------

    #[test]
    fn trend_of_a_perfect_line_recovers_the_slope_exactly() {
        // +1 MiB every 30 s = 120 MiB/hour.
        let points: Vec<(f64, f64)> = (0..20).map(|i| (at(i), at(i) / DT)).collect();
        let t = linear_trend(&points).expect("a 20-point line has a trend");
        assert!(
            (t.slope_per_hour - 120.0).abs() < 1e-9,
            "slope was {}",
            t.slope_per_hour
        );
        assert!((t.r_squared - 1.0).abs() < 1e-9, "r² was {}", t.r_squared);
    }

    #[test]
    fn trend_of_a_flat_line_is_zero_slope() {
        let points: Vec<(f64, f64)> = (0..20).map(|i| (at(i), 400.0)).collect();
        let t = linear_trend(&points).expect("a flat line still has a trend");
        assert!(t.slope_per_hour.abs() < 1e-9);
        assert!((t.delta).abs() < 1e-9);
    }

    #[test]
    fn trend_needs_two_distinct_points() {
        assert!(linear_trend(&[]).is_none());
        assert!(linear_trend(&[(0.0, 1.0)]).is_none());
        // Both samples at t=0: no slope exists.
        assert!(linear_trend(&[(0.0, 1.0), (0.0, 9.0)]).is_none());
    }

    // ---- the leak detector, against synthetic series ------------------------

    #[test]
    fn flat_rss_and_flat_fps_passes() {
        let s = series(960, |_| 400.0, |_| 60.0); // 8 h at 30 s
        let a = analyze(&s, Outcome::Completed, Thresholds::default());
        assert_eq!(
            a.verdict,
            Verdict::Pass,
            "{:?} / {:?}",
            a.failures,
            a.review
        );
        assert!(a.failures.is_empty());
        assert!(a.review.is_empty());
    }

    #[test]
    fn steadily_climbing_rss_fails_as_a_leak() {
        // +40 MiB/hour: a hard leak. 960 samples * 30 s = 8 h.
        let s = series(960, |t| 400.0 + (t / SECS_PER_HOUR) * 40.0, |_| 60.0);
        let a = analyze(&s, Outcome::Completed, Thresholds::default());
        assert_eq!(a.verdict, Verdict::Fail);
        assert!(
            a.failures.iter().any(|f| f.contains("RSS climbed")),
            "{:?}",
            a.failures
        );
        let rss = a.rss.expect("rss trend");
        assert!((rss.slope_per_hour - 40.0).abs() < 0.5);
    }

    /// The critical false-positive case: memory that jitters ±30 MiB but does
    /// not trend. A slope-only detector with no r² gate would flag this.
    #[test]
    fn noisy_but_level_rss_passes() {
        // Deterministic pseudo-noise: a fast sine, no drift.
        let s = series(960, |t| 400.0 + (t * 0.023).sin() * 30.0, |_| 60.0);
        let a = analyze(&s, Outcome::Completed, Thresholds::default());
        assert_eq!(
            a.verdict,
            Verdict::Pass,
            "noise must not read as a leak: {:?} / {:?}",
            a.failures,
            a.review
        );
    }

    /// A sawtooth (allocate, release, allocate) with a level floor: steep local
    /// rises, no net growth. Must not fail.
    #[test]
    fn sawtooth_rss_with_a_level_floor_does_not_fail() {
        // 30-minute teeth: +60 MiB across each tooth, then straight back down.
        let s = series(960, |t| 400.0 + (t % 1800.0) / 30.0, |_| 60.0);
        let a = analyze(&s, Outcome::Completed, Thresholds::default());
        assert_ne!(
            a.verdict,
            Verdict::Fail,
            "a level-floor sawtooth is not a leak: {:?}",
            a.failures
        );
    }

    /// A sawtooth whose *floor* rises — the genuinely dangerous shape (each
    /// cycle releases less than it took). The fit is noisy but the trend is
    /// real, so this must at least reach the operator, not silently pass.
    #[test]
    fn sawtooth_with_a_rising_floor_is_not_silently_passed() {
        let s = series(
            960,
            |t| 400.0 + (t / SECS_PER_HOUR) * 30.0 + (t % 1800.0) / 120.0,
            |_| 60.0,
        );
        let a = analyze(&s, Outcome::Completed, Thresholds::default());
        assert_ne!(
            a.verdict,
            Verdict::Pass,
            "a rising sawtooth floor must not pass silently"
        );
    }

    #[test]
    fn decaying_fps_fails() {
        // 60 -> 40 fps over the 8-hour run: a 33% decay.
        let s = series(
            960,
            |_| 400.0,
            |t| 60.0 - (t / (8.0 * SECS_PER_HOUR)) * 20.0,
        );
        let a = analyze(&s, Outcome::Completed, Thresholds::default());
        assert_eq!(a.verdict, Verdict::Fail);
        assert!(
            a.failures.iter().any(|f| f.contains("FPS decayed")),
            "{:?}",
            a.failures
        );
        assert!(a.fps_decay.expect("decay") > 0.2);
    }

    #[test]
    fn mild_fps_drift_asks_for_review_rather_than_failing() {
        // ~8% decay: inside the review band.
        let s = series(960, |_| 400.0, |t| 60.0 - (t / (8.0 * SECS_PER_HOUR)) * 5.0);
        let a = analyze(&s, Outcome::Completed, Thresholds::default());
        assert_eq!(a.verdict, Verdict::Review);
        assert!(a.failures.is_empty(), "{:?}", a.failures);
        assert!(a.review.iter().any(|r| r.contains("FPS drifted")));
    }

    /// The screensaver deliberately drops the present rate. Those samples must
    /// not be read as FPS decay.
    #[test]
    fn screensaver_samples_are_excluded_from_the_fps_fit() {
        let mut s = series(960, |_| 400.0, |_| 60.0);
        for sample in s.iter_mut().skip(480) {
            sample.activity = Some("Screensaver".to_string());
            sample.fps = Some(3.0); // the Hot-tier "resting ember"
        }
        let a = analyze(&s, Outcome::Completed, Thresholds::default());
        assert!(
            a.failures.iter().all(|f| !f.contains("FPS")),
            "screensaver present-rate is not a regression: {:?}",
            a.failures
        );
    }

    // ---- freezes and process outcomes --------------------------------------

    #[test]
    fn a_stalled_app_clock_is_detected_as_a_freeze() {
        let mut s = series(20, |_| 400.0, |_| 60.0);
        // The app's clock stops at sample 10 while the launcher's keeps going.
        for sample in s.iter_mut().skip(10) {
            sample.app_uptime_secs = Some(300.0);
        }
        let freezes = detect_freezes(&s);
        assert_eq!(freezes.len(), 9, "every sample after the stall is frozen");
        let a = analyze(&s, Outcome::Completed, Thresholds::default());
        assert_eq!(a.verdict, Verdict::Fail);
        assert!(a.failures.iter().any(|f| f.contains("froze")));
    }

    #[test]
    fn missing_health_snapshots_are_not_freezes() {
        let mut s = series(20, |_| 400.0, |_| 60.0);
        s[3].app_uptime_secs = None; // health.json not yet written / unreadable
        assert!(detect_freezes(&s).is_empty());
    }

    #[test]
    fn an_early_exit_fails_regardless_of_the_metrics() {
        let s = series(960, |_| 400.0, |_| 60.0);
        let a = analyze(&s, Outcome::ExitedEarly, Thresholds::default());
        assert_eq!(a.verdict, Verdict::Fail);
        assert!(a
            .failures
            .iter()
            .any(|f| f.contains("did not exit cleanly")));
    }

    #[test]
    fn a_timeout_fails() {
        let s = series(960, |_| 400.0, |_| 60.0);
        let a = analyze(&s, Outcome::TimedOut, Thresholds::default());
        assert_eq!(a.verdict, Verdict::Fail);
    }

    /// A short smoke run cannot support a trend fit, and says so rather than
    /// claiming a pass it has not earned.
    #[test]
    fn too_few_samples_asks_for_review_instead_of_passing() {
        let s = series(4, |_| 400.0, |_| 60.0);
        let a = analyze(&s, Outcome::Completed, Thresholds::default());
        assert_eq!(a.verdict, Verdict::Review);
        assert!(a.review.iter().any(|r| r.contains("trend fit")));
    }

    #[test]
    fn no_rss_readings_at_all_asks_for_review() {
        let mut s = series(960, |_| 400.0, |_| 60.0);
        for sample in &mut s {
            sample.rss_kib = None;
        }
        let a = analyze(&s, Outcome::Completed, Thresholds::default());
        assert_eq!(a.verdict, Verdict::Review);
        assert!(a.review.iter().any(|r| r.contains("no RSS readings")));
    }

    #[test]
    fn worst_verdict_is_the_most_severe() {
        assert_eq!(Verdict::Pass.worst(Verdict::Review), Verdict::Review);
        assert_eq!(Verdict::Review.worst(Verdict::Fail), Verdict::Fail);
        assert_eq!(Verdict::Fail.worst(Verdict::Pass), Verdict::Fail);
        assert_eq!(Verdict::Pass.worst(Verdict::Pass), Verdict::Pass);
    }
}
