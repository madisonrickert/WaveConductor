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
//! - The app panicked on a worker thread and kept rendering (see
//!   [`crate::soak::logscan`] — the process survives, so only the log knows).
//! - A single frame took long enough to be a visible wedge.
//! - RSS rises with a steep, well-fit slope over hours. A least-squares fit
//!   plus its r² separates "climbing" from "noisy but level".
//! - FPS in the last quarter of the run is materially below the first quarter.
//!
//! Not mechanical: *is this drift acceptable?* A 3 MiB/hour climb might be a
//! bounded cache filling to its ceiling, or the first hour of a slow leak. The
//! honest answer for the middle band is [`Verdict::Review`] — emit the numbers,
//! name the artifact, and let the operating agent (or Madison) judge. This tool
//! never fabricates a pass.
//!
//! And *not answerable at all* by a run of a given length: see
//! [`resolves_a_leak`]. A run whose span is too short for the fit to resolve a
//! leak even at the fail slope cannot pass — it validated the harness, not the
//! build, and it says so.

use serde::{Deserialize, Serialize};

use super::logscan::LogFindings;

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
    /// The app's longest *single* frame, in milliseconds, since its previous
    /// health snapshot — a high-water mark, not an average. This is the only
    /// lane that can see a wedge shorter than the launcher's sample interval:
    /// `app_uptime_secs` advances right through a 25-second hitch in a 30-second
    /// window, and smoothed FPS has recovered by the time the next sample lands.
    /// `None` from an app binary older than this field.
    #[serde(default)]
    pub max_frame_time_ms: Option<f64>,
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
    /// Longest single frame (ms) at or above which the run fails outright.
    ///
    /// The app caps itself at 60 fps (16.7 ms/frame), so 2 s is ~120 dropped
    /// frames: two full seconds in which a kiosk window is a frozen image.
    /// Nothing on the steady-state path — not a sketch swap, not a settings
    /// reload, not a debug-build pipeline compile — legitimately costs that.
    /// A wedge does. This is the lane that sees the 20-to-29-second hitch the
    /// `app_uptime_secs` freeze detector (which only resolves to one 30 s sample
    /// interval) walks straight past.
    pub hitch_fail_ms: f64,
    /// Longest single frame (ms) at or above which the run needs review.
    ///
    /// Deliberately *not* one dropped frame: the soak runs a debug binary, and
    /// entering a sketch there really can cost a few hundred milliseconds of
    /// pipeline and asset work. 500 ms (~30 frames) is above that and below
    /// anything a person would call a stutter rather than a stall — so it lands
    /// in front of an operator instead of failing the build.
    pub hitch_review_ms: f64,
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
            hitch_fail_ms: 2000.0,
            hitch_review_ms: 500.0,
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
    /// Launcher wall-clock times (seconds) of the samples whose frame-time
    /// watermark reached [`Thresholds::hitch_review_ms`] — the sub-sample wedges.
    pub hitches: Vec<f64>,
    /// The worst single frame (ms) the app reported over the whole fitted run.
    /// `None` when no sample carried a watermark.
    pub max_hitch_ms: Option<f64>,
    /// What the scan of `app.log` found: a worker-thread panic the process
    /// survived, or `ERROR`-level lines. Every metric lane can report perfect
    /// health through both.
    pub log: LogFindings,
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

    let quarter = quarter_len(n);
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

/// The number of leading (and trailing) points that make up a "quarter" in
/// [`linear_trend`]. Shared so [`quarter_gap_hours`] measures the gap between
/// exactly the two windows whose means become [`Trend::delta`].
fn quarter_len(n: usize) -> usize {
    (n / 4).max(1)
}

/// Hours between the *mean sample time* of the first quarter and that of the
/// last quarter — the two windows whose difference in value is [`Trend::delta`].
///
/// For an evenly-sampled run this is about three quarters of the run's span. It
/// is measured rather than assumed, so an unevenly-sampled run (a stalled
/// launcher, a `--report` over a partial file) is judged on what it actually
/// covered.
#[must_use]
pub fn quarter_gap_hours(points: &[(f64, f64)]) -> f64 {
    if points.len() < 2 {
        return 0.0;
    }
    let quarter = quarter_len(points.len());
    #[allow(
        clippy::as_conversions,
        clippy::cast_precision_loss,
        reason = "usize -> f64 has no From impl; quarter lengths are in the hundreds"
    )]
    let len = quarter as f64;
    let mean_t = |slice: &[(f64, f64)]| slice.iter().map(|p| p.0).sum::<f64>() / len;
    let first = mean_t(&points[..quarter]);
    let last = mean_t(&points[points.len() - quarter..]);
    (last - first) / SECS_PER_HOUR
}

/// Could this run resolve a leak *at all*?
///
/// The leak verdict is gated on `delta` (last-quarter mean minus first-quarter
/// mean) clearing [`Thresholds::rss_noise_floor_mib`]. Over a short run, the two
/// quarter windows are close together in time, so even a textbook leak at the
/// *fail* slope gains less than the noise floor between them and falls through
/// every branch to a pass. That is a false PASS on a real leak, and the only
/// honest answer is that the run was too short to say.
///
/// So: a leak at [`Thresholds::rss_fail_mib_per_hour`], sustained across this
/// run's own quarter gap, must be able to produce a `delta` at or above the
/// noise floor. At the defaults (20 MiB/h fail, 16 MiB floor) that needs a
/// quarter gap of 0.8 h — about a 64-minute run. Nothing here is hardcoded: the
/// question is asked of the thresholds and the samples in front of it.
#[must_use]
pub fn resolves_a_leak(points: &[(f64, f64)], thresholds: &Thresholds) -> bool {
    thresholds.rss_fail_mib_per_hour * quarter_gap_hours(points) >= thresholds.rss_noise_floor_mib
}

/// Launcher wall-clock times at which the app's own clock failed to advance
/// since the previous sample — the freeze detector.
///
/// Samples with no `app_uptime_secs` (an unreadable or not-yet-written
/// `health.json`) are skipped rather than treated as freezes: the app may
/// simply not have published its first snapshot yet.
///
/// **Resolution.** This lane only sees a wedge that spans a whole launcher
/// sample interval (30 s by default): a 25-second hitch still advances the app's
/// clock by ~5 s across the window, which clears [`FREEZE_ADVANCE_SECS`] with
/// room to spare. Everything shorter than a sample interval is [`detect_hitches`]'
/// job, which is why the app publishes a per-interval frame-time high-water mark.
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

/// Launcher wall-clock times of the samples whose frame-time watermark reached
/// `review_ms` — the wedges too short for [`detect_freezes`] to see.
#[must_use]
pub fn detect_hitches(samples: &[Sample], review_ms: f64) -> Vec<f64> {
    samples
        .iter()
        .filter(|s| s.max_frame_time_ms.is_some_and(|ms| ms >= review_ms))
        .map(|s| s.t_secs)
        .collect()
}

/// Analyze a completed (or aborted) soak run.
///
/// `log` is the scan of the run's `app.log` ([`crate::soak::logscan::scan_log`]),
/// passed in rather than read here so this stays a pure function of its inputs.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one linear classifier: outcome, then the log, then freezes and hitches, then RSS, \
              then FPS. Each branch carries the operator-facing sentence it emits; splitting them \
              apart would scatter the verdict logic across five functions to satisfy a line count"
)]
pub fn analyze(
    samples: &[Sample],
    outcome: Outcome,
    thresholds: Thresholds,
    log: &LogFindings,
) -> Analysis {
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

    // --- the log: the failures every metric lane reports health through -------
    // A panic on a worker thread does not kill the process (debug builds unwind,
    // and the audio watcher explicitly catches). Hand tracking can be dead for
    // five hours while RSS, FPS, and the app's clock all look perfect.
    if log.panic_count > 0 {
        failures.push(format!(
            "the app panicked {} time(s) and kept running — a worker thread died while the \
             process went on rendering, so no metric lane can see this. First: {}",
            log.panic_count,
            log.panics.first().map_or("(unquoted)", String::as_str),
        ));
        verdict = Verdict::Fail;
    }
    if log.error_count > 0 {
        let mut quoted = String::new();
        for line in &log.errors {
            quoted.push_str("\n      ");
            quoted.push_str(line);
        }
        review.push(format!(
            "{} ERROR-level line(s) in app.log — a lost device, an audio path that never \
             recovered, or a shader that failed to reload are all survivable and all silent in \
             the metrics. Judge these:{quoted}",
            log.error_count,
        ));
        verdict = verdict.worst(Verdict::Review);
    }
    if let Some(why) = &log.unreadable {
        review.push(format!(
            "app.log could not be read ({why}) — the panic / ERROR lane was blind for this run"
        ));
        verdict = verdict.worst(Verdict::Review);
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

    // --- hitches: the wedges shorter than one sample interval -----------------
    let hitches = detect_hitches(samples, thresholds.hitch_review_ms);
    let max_hitch_ms = samples
        .iter()
        .filter_map(|s| s.max_frame_time_ms)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_hitch_ms = max_hitch_ms.is_finite().then_some(max_hitch_ms);

    match max_hitch_ms {
        None if !samples.is_empty() => {
            review.push(
                "no sample carried a frame-time watermark — the hitch lane was blind, so a wedge \
                 shorter than the sample interval could not have been seen (is the app binary \
                 older than this launcher? rebuild it)"
                    .to_string(),
            );
            verdict = verdict.worst(Verdict::Review);
        }
        Some(worst) if worst >= thresholds.hitch_fail_ms => {
            failures.push(format!(
                "the app's worst single frame took {:.0} ms ({} sample interval(s) held a frame \
                 over {:.0} ms, first at t={:.0}s) — that is a wedged window, not a stutter, and \
                 it is far too short for the freeze detector to resolve",
                worst,
                hitches.len(),
                thresholds.hitch_review_ms,
                hitches.first().copied().unwrap_or_default(),
            ));
            verdict = Verdict::Fail;
        }
        Some(worst) if !hitches.is_empty() => {
            review.push(format!(
                "the app's worst single frame took {:.0} ms, over the {:.0} ms review threshold \
                 ({} sample interval(s), first at t={:.0}s). A debug-build sketch entry can cost \
                 a few hundred ms legitimately; a repeating hitch cannot. Check whether these land \
                 on the sketch-cycle boundaries.",
                worst,
                thresholds.hitch_review_ms,
                hitches.len(),
                hitches.first().copied().unwrap_or_default(),
            ));
            verdict = verdict.worst(Verdict::Review);
        }
        _ => {}
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
        // Before reading the slope: could a run this short have *resolved* a leak
        // at all? If a textbook leak at the fail slope would not have moved the
        // quarter means past the noise floor over this span, then every RSS
        // branch below is guaranteed to fall through — and a fall-through is a
        // PASS. That PASS would be a lie, so the run cannot have one.
        if !resolves_a_leak(&rss_points, &thresholds) {
            let gap_h = quarter_gap_hours(&rss_points);
            let would_gain = thresholds.rss_fail_mib_per_hour * gap_h;
            // What the *run* would have to span, extrapolating the gap-to-span
            // ratio this run actually exhibits (≈3/4 for even sampling).
            let span_h = (rss_points.last().map_or(0.0, |p| p.0)
                - rss_points.first().map_or(0.0, |p| p.0))
                / SECS_PER_HOUR;
            let ratio = if span_h > 0.0 { gap_h / span_h } else { 0.75 };
            let needed_h = if ratio > 0.0 {
                (thresholds.rss_noise_floor_mib / thresholds.rss_fail_mib_per_hour) / ratio
            } else {
                f64::INFINITY
            };
            review.push(format!(
                "this run is too short to say anything about a leak. Its quarter means are only \
                 {:.0} min apart, so even a textbook leak at the {:.0} MiB/hour fail threshold \
                 would gain just {:.1} MiB between them — under the {:.0} MiB noise floor, which \
                 means every leak branch is guaranteed to fall through no matter what memory did. \
                 Resolving a leak at that threshold needs a run of about {:.1} h. A run this short \
                 validates the HARNESS, not the BUILD: it says nothing about memory.",
                gap_h * 60.0,
                thresholds.rss_fail_mib_per_hour,
                would_gain,
                thresholds.rss_noise_floor_mib,
                needed_h,
            ));
            verdict = verdict.worst(Verdict::Review);
        }

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
        hitches,
        max_hitch_ms,
        log: log.clone(),
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

    /// A log scan that found nothing — the default for every fixture that is not
    /// about the log lane.
    fn clean() -> LogFindings {
        LogFindings::default()
    }

    /// A sample at `t` seconds with `rss_mib` MiB resident and `fps` FPS, in an
    /// active sketch, with the app's clock tracking the launcher's and a healthy
    /// frame-time watermark (one 60 fps frame).
    fn sample(t: f64, rss_mib: f64, fps: f64) -> Sample {
        Sample {
            t_secs: t,
            rss_kib: Some(mib_to_kib(rss_mib)),
            app_uptime_secs: Some(t),
            fps: Some(fps),
            max_frame_time_ms: Some(16.7),
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
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
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
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
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
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
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
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
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
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
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
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
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
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
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
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
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
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
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
        let a = analyze(&s, Outcome::ExitedEarly, Thresholds::default(), &clean());
        assert_eq!(a.verdict, Verdict::Fail);
        assert!(a
            .failures
            .iter()
            .any(|f| f.contains("did not exit cleanly")));
    }

    #[test]
    fn a_timeout_fails() {
        let s = series(960, |_| 400.0, |_| 60.0);
        let a = analyze(&s, Outcome::TimedOut, Thresholds::default(), &clean());
        assert_eq!(a.verdict, Verdict::Fail);
    }

    /// A short smoke run cannot support a trend fit, and says so rather than
    /// claiming a pass it has not earned.
    #[test]
    fn too_few_samples_asks_for_review_instead_of_passing() {
        let s = series(4, |_| 400.0, |_| 60.0);
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
        assert_eq!(a.verdict, Verdict::Review);
        assert!(a.review.iter().any(|r| r.contains("trend fit")));
    }

    #[test]
    fn no_rss_readings_at_all_asks_for_review() {
        let mut s = series(960, |_| 400.0, |_| 60.0);
        for sample in &mut s {
            sample.rss_kib = None;
        }
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
        assert_eq!(a.verdict, Verdict::Review);
        assert!(a.review.iter().any(|r| r.contains("no RSS readings")));
    }

    // ---- the r² gate, which is what separates a leak from a sawtooth --------

    /// The gate's own test. A series that is **steep** (25 MiB/hour, past the 20
    /// MiB/hour fail slope), **above the noise floor** (it gains ~150 MiB), and
    /// **badly fit** (a ±100 MiB oscillation dominates the variance, r² ≈ 0.4).
    ///
    /// Every other fixture clears one of the earlier conditions before r² is ever
    /// consulted, so this is the only test in which `rss_min_r_squared` actually
    /// decides anything. Hardcode `linear_trend`'s `r_squared` to `1.0` and this
    /// test — and only this test — turns the REVIEW into a FAIL. That is the
    /// point: r² is the one thing standing between a violently oscillating
    /// bounded cache and a build being failed for a leak it does not have.
    #[test]
    fn a_steep_above_noise_but_badly_fit_series_is_reviewed_not_failed() {
        let s = series(
            960,
            |t| 400.0 + (t / SECS_PER_HOUR) * 25.0 + (t * 0.01).sin() * 100.0,
            |_| 60.0,
        );
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
        let rss = a.rss.expect("rss trend");

        // The behaviour first, so that a mutated r² fails *here* — on the verdict
        // it changes — rather than only on a fixture invariant.
        assert!(
            a.failures.is_empty(),
            "the r² gate must keep a badly-fit series out of FAIL — that is the whole job of the \
             gate (r²={:.2}): {:?}",
            rss.r_squared,
            a.failures
        );
        assert_eq!(
            a.verdict,
            Verdict::Review,
            "steep + above noise + badly fit is a judgment call, not a mechanical leak: {:?}",
            a.review
        );

        // ...and then the fixture invariants, so that a series which quietly stops
        // being steep / above-noise / badly-fit fails loudly instead of passing
        // this test for the wrong reason.
        assert!(
            rss.slope_per_hour >= Thresholds::default().rss_fail_mib_per_hour,
            "fixture must be steep enough to reach the FAIL branch: {:.1} MiB/h",
            rss.slope_per_hour
        );
        assert!(
            rss.delta >= Thresholds::default().rss_noise_floor_mib,
            "fixture must clear the noise floor: {:.1} MiB",
            rss.delta
        );
        assert!(
            rss.r_squared < Thresholds::default().rss_min_r_squared,
            "fixture must fit BADLY, or r² never decides anything: r²={:.2}",
            rss.r_squared
        );
    }

    // ---- a run too short to resolve a leak cannot pass -----------------------

    /// The gap I3 named: a 30-minute run's quarter means are ~22 minutes apart,
    /// so a *textbook* 20 MiB/hour leak moves them only ~7.5 MiB — under the 16
    /// MiB noise floor. Every leak branch falls through, and a fall-through used
    /// to be a PASS. It must not be.
    #[test]
    fn a_real_leak_in_a_run_too_short_to_resolve_it_does_not_pass() {
        // 60 samples * 30 s = 30 minutes, climbing at exactly the fail slope.
        let s = series(60, |t| 400.0 + (t / SECS_PER_HOUR) * 20.0, |_| 60.0);
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
        assert_ne!(
            a.verdict,
            Verdict::Pass,
            "a 20 MiB/h leak in a 30-minute run must never read as a pass"
        );
        assert!(
            a.review.iter().any(|r| r.contains("too short")),
            "and it must say WHY it cannot judge: {:?}",
            a.review
        );
    }

    /// The smoke invocation AGENTS.md documents (`--duration 2m --sample 5s`)
    /// collects enough samples to fit — and must still not claim a pass.
    #[test]
    fn the_documented_smoke_run_reviews_rather_than_passing() {
        // 2 minutes at a 5 s sample = 24 samples, all flat and healthy.
        let s: Vec<Sample> = (0..24_i32)
            .map(|i| sample(f64::from(i) * 5.0, 400.0, 60.0))
            .collect();
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
        assert_eq!(
            a.verdict,
            Verdict::Review,
            "a 2-minute smoke run validates the harness, not the build: {:?}",
            a.review
        );
        assert!(
            a.review.iter().any(|r| r.contains("HARNESS")),
            "{:?}",
            a.review
        );
    }

    /// ...but the 8-hour gate run *can* resolve one, so it is still allowed to
    /// pass. The short-run guard must not swallow every verdict.
    #[test]
    fn the_eight_hour_gate_run_still_resolves_a_leak() {
        let points: Vec<(f64, f64)> = (0..960).map(|i| (at(i), 400.0)).collect();
        assert!(resolves_a_leak(&points, &Thresholds::default()));
        assert!((quarter_gap_hours(&points) - 6.0).abs() < 0.1);
    }

    // ---- the log: what every metric lane reports health through --------------

    /// C1's failure scenario, end to end: the inference worker panics at hour 3,
    /// the process keeps rendering, and RSS / FPS / uptime / exit code are all
    /// perfect. Only the log knows, so only the log can fail it.
    #[test]
    fn a_worker_panic_fails_a_run_whose_metrics_are_perfect() {
        let s = series(960, |_| 400.0, |_| 60.0);
        let log = LogFindings {
            lines_scanned: 40_000,
            panic_count: 1,
            panics: vec![
                "thread 'mediapipe-inference' panicked at inference_ort.rs:214".to_string(),
            ],
            ..LogFindings::default()
        };
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &log);
        assert_eq!(a.verdict, Verdict::Fail);
        assert!(
            a.failures.iter().any(|f| f.contains("mediapipe-inference")),
            "the verdict must quote the panic: {:?}",
            a.failures
        );
    }

    #[test]
    fn error_lines_in_the_log_ask_for_review() {
        let s = series(960, |_| 400.0, |_| 60.0);
        let log = LogFindings {
            error_count: 3,
            errors: vec!["ERROR wgpu_core::device: Device lost".to_string()],
            ..LogFindings::default()
        };
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &log);
        assert_eq!(a.verdict, Verdict::Review);
        assert!(
            a.review.iter().any(|r| r.contains("Device lost")),
            "{:?}",
            a.review
        );
    }

    #[test]
    fn an_unreadable_log_asks_for_review_because_the_lane_was_blind() {
        let s = series(960, |_| 400.0, |_| 60.0);
        let log = LogFindings {
            unreadable: Some("app.log: No such file".to_string()),
            ..LogFindings::default()
        };
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &log);
        assert_eq!(a.verdict, Verdict::Review);
        assert!(
            a.review.iter().any(|r| r.contains("blind")),
            "{:?}",
            a.review
        );
    }

    // ---- hitches: the wedges the freeze detector cannot resolve --------------

    /// C2's failure scenario: a 25-second wedge inside a 30-second sample window.
    /// The app's clock still advances ~5 s across it, so the freeze lane sees
    /// nothing; the smoothed FPS has recovered by the next sample, so that lane
    /// sees nothing either. The frame-time watermark is the only witness.
    #[test]
    fn a_sub_sample_wedge_fails_even_though_the_freeze_lane_misses_it() {
        let mut s = series(960, |_| 400.0, |_| 60.0);
        s[500].max_frame_time_ms = Some(25_000.0);

        assert!(
            detect_freezes(&s).is_empty(),
            "precondition: the app's clock advanced right through the wedge, so the freeze \
             detector is blind to it — this is exactly why the watermark exists"
        );

        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
        assert_eq!(a.verdict, Verdict::Fail);
        assert!(
            a.failures.iter().any(|f| f.contains("wedged window")),
            "{:?}",
            a.failures
        );
        assert_eq!(a.hitches, vec![at(500)]);
    }

    /// A few hundred milliseconds at a sketch entry, in a debug build, is not a
    /// failure — but it is not silence either.
    #[test]
    fn a_sub_second_hitch_is_reviewed_rather_than_failed() {
        let mut s = series(960, |_| 400.0, |_| 60.0);
        s[300].max_frame_time_ms = Some(700.0);
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
        assert_eq!(a.verdict, Verdict::Review);
        assert!(a.failures.is_empty(), "{:?}", a.failures);
        assert!(
            a.max_hitch_ms.is_some_and(|m| (m - 700.0).abs() < 1e-9),
            "{:?}",
            a.max_hitch_ms
        );
    }

    /// An app binary that predates the watermark publishes no `max_frame_time_ms`
    /// at all. The hitch lane is then blind, and a blind lane must say so rather
    /// than report a clean run.
    #[test]
    fn samples_with_no_watermark_ask_for_review() {
        let mut s = series(960, |_| 400.0, |_| 60.0);
        for sample in &mut s {
            sample.max_frame_time_ms = None;
        }
        let a = analyze(&s, Outcome::Completed, Thresholds::default(), &clean());
        assert_eq!(a.verdict, Verdict::Review);
        assert!(
            a.review.iter().any(|r| r.contains("hitch lane was blind")),
            "{:?}",
            a.review
        );
        assert_eq!(a.max_hitch_ms, None);
    }

    #[test]
    fn worst_verdict_is_the_most_severe() {
        assert_eq!(Verdict::Pass.worst(Verdict::Review), Verdict::Review);
        assert_eq!(Verdict::Review.worst(Verdict::Fail), Verdict::Fail);
        assert_eq!(Verdict::Fail.worst(Verdict::Pass), Verdict::Fail);
        assert_eq!(Verdict::Pass.worst(Verdict::Pass), Verdict::Pass);
    }
}
