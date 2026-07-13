//! `cargo xtask soak-test` — run the app under representative load for hours,
//! sample its health, and produce a verdict plus a machine-readable artifact.
//!
//! This is the automation of the release gate `AGENTS.md` requires before any
//! tag: "an 8-hour soak on the target deployment hardware". It used to be a
//! human watching Activity Monitor.
//!
//! ## Signal flow
//!
//! 1. Resolve the pre-built DEBUG binary (fail fast with the build command if
//!    it is missing — soak-test does not build the app, for the same reason
//!    `capture` does not).
//! 2. Launch it with the representative-load env: a starting sketch, a hand
//!    provider (audio is always live), and `WC_SOAK` — the app-side schedule
//!    that republishes `<dir>/health.json`, advances to the next sketch every
//!    `--cycle`, and self-exits at `--duration`. Sketch *cycling* is the point:
//!    it is the enter/exit lifecycle, over hours, where this project's
//!    GPU-resource leaks have actually lived.
//! 3. Every `--sample` seconds, read that snapshot and pair it with an
//!    externally-measured RSS ([`rss`]); append the joined row to
//!    `<dir>/samples.ndjson` immediately, so a killed run still leaves data.
//! 4. When the app exits (or blows past its grace window), scan `app.log` for
//!    the failures no metric can see ([`logscan`] — a worker-thread panic the
//!    process survived, an `ERROR`-level line), fit the trends and draw a
//!    verdict ([`analysis`]), write `<dir>/run.json`, and report.
//!
//! ## Memory, over eight hours
//!
//! The tool that hunts leaks must not leak. Samples are appended to disk as
//! they are taken and also retained in a `Vec` for the final fit: at the
//! default 30 s interval an 8-hour run is 960 rows of ~10 scalars — well under
//! a megabyte, and bounded by `duration / sample`, which the operator sets. The
//! log tee streams the child's output straight to `app.log` through a 4 KiB
//! stack buffer and never buffers it in memory. Nothing here grows without a
//! bound the operator chose.
//!
//! ## Exit codes
//!
//! `0` = pass, or review-required (read the directive — the tool will not
//! fabricate a verdict it cannot justify). `1` = a mechanically-certain
//! failure: the app died, froze, leaked, or decayed.

#![allow(clippy::print_stdout, reason = "xtask is a CLI; printing is its job")]

pub mod analysis;
pub mod logscan;
pub mod rss;
pub mod timefmt;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::Args as ClapArgs;
use serde::{Deserialize, Serialize};

use analysis::{analyze, Analysis, Outcome, Sample, Thresholds, Verdict};
use timefmt::{format_duration, format_utc_label, parse_duration, unix_now};

use crate::util::{
    git_short_commit, resolve_built_binary, spawn_log_tee, warn_if_stale, workspace_root,
};

/// Subcommand name used in this module's operator-facing error messages.
const TOOL: &str = "soak-test";

/// How long past its configured duration the app may take to shut down before
/// the launcher kills it and calls the run a hang. Generous: an 8-hour run's
/// teardown (GPU resources, audio device, settings flush) is seconds, not
/// minutes, so a minute of slack is only ever consumed by a real wedge.
const EXIT_GRACE: Duration = Duration::from_mins(1);

/// How often the launcher wakes to check the clock and the child. Fine enough
/// that a sample lands within a quarter-second of its slot, coarse enough to be
/// free over eight hours.
const TICK: Duration = Duration::from_millis(250);

/// Arguments for the soak-test subcommand.
#[derive(ClapArgs)]
pub struct Args {
    /// How long to run: `8h`, `30m`, `90s`, or a bare number of seconds.
    /// Use a short duration for a smoke run — nobody should wait eight hours to
    /// find out the harness is broken.
    #[arg(long, default_value = "8h")]
    pub duration: String,
    /// Seconds between health samples (`30s`, `1m`, ...).
    #[arg(long, default_value = "30s")]
    pub sample: String,
    /// Time between automatic sketch advances (`5m`). `0` disables cycling —
    /// but cycling is what exercises the enter/exit lifecycle, so don't.
    #[arg(long, default_value = "5m")]
    pub cycle: String,
    /// Startup window excluded from the trend fits (asset loads and shader
    /// compilation are not a leak).
    #[arg(long, default_value = "60s")]
    pub warmup: String,
    /// Sketch to start on (`line`, `flame`, `dots`, `cymatics`).
    #[arg(long, default_value = "line")]
    pub sketch: String,
    /// Hand provider (`synthetic`, `mock`, `leap`, `mediapipe`, `auto`).
    /// `synthetic` gives a deterministic tracked hand without hardware.
    #[arg(long, default_value = "synthetic")]
    pub provider: String,
    /// Let the idle timer run (`Active` -> `Idle` -> `Screensaver`) instead of
    /// holding the sketch active. Soaks the kiosk's *unattended* path.
    #[arg(long)]
    pub natural_idle: bool,
    /// Name the run directory under `target/soak/` (default: a UTC timestamp).
    #[arg(long)]
    pub label: Option<String>,
    /// Re-analyze a finished run directory and re-report. No launch; the same
    /// verdict logic runs over the recorded `samples.ndjson`.
    #[arg(long, value_name = "DIR")]
    pub report: Option<PathBuf>,
    /// Emit machine-readable JSON instead of the human report.
    #[arg(long)]
    pub json: bool,
}

/// The self-describing `run.json` artifact: everything needed to understand,
/// reproduce, or compare a soak run without the terminal it was launched from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    /// Run label (the directory name under `target/soak/`).
    pub label: String,
    /// Unix timestamp at which the run started.
    pub started_unix: u64,
    /// Short git commit the app was built from, when resolvable.
    pub commit: Option<String>,
    /// Host OS and architecture — a soak verdict is only meaningful about the
    /// hardware it ran on.
    pub host: String,
    /// The load the run was subjected to.
    pub config: RunConfig,
    /// How the app's process ended.
    pub outcome: Outcome,
    /// Launcher wall-clock seconds from launch to exit.
    pub elapsed_secs: f64,
    /// The app's process exit code, when it exited on its own.
    pub exit_code: Option<i32>,
    /// Verdict and trends. Fitted over post-warmup samples only.
    pub analysis: Analysis,
    /// Samples taken (including the warmup samples excluded from the fit).
    pub samples_taken: usize,
    /// Where the raw samples, the app log, and the last health snapshot live.
    pub artifacts: Artifacts,
}

/// The load parameters of a run, recorded so two runs can be compared honestly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunConfig {
    /// Requested duration, in seconds.
    pub duration_secs: f64,
    /// Launcher sample interval, in seconds.
    pub sample_secs: f64,
    /// Sketch cycle interval, in seconds. `0` = no cycling.
    pub cycle_secs: f64,
    /// Startup window excluded from the fits, in seconds.
    pub warmup_secs: f64,
    /// Sketch the run started on.
    pub sketch: String,
    /// Hand-tracking provider.
    pub provider: String,
    /// `active` (idle timer held off) or `natural` (idle -> screensaver runs).
    pub activity: String,
}

/// Paths to the run's artifacts, relative to the workspace root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifacts {
    /// The run directory itself.
    pub dir: String,
    /// One JSON sample per line, in order.
    pub samples: String,
    /// The app's teed stdout+stderr.
    pub app_log: String,
    /// The app's final health snapshot.
    pub health: String,
}

/// Execute the soak-test subcommand.
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(dir) = args.report.clone() {
        return report_existing(&dir, args.json);
    }

    let root = workspace_root();
    let duration =
        parse_duration(&args.duration).map_err(|e| format!("{TOOL}: --duration: {e}"))?;
    let sample = parse_duration(&args.sample).map_err(|e| format!("{TOOL}: --sample: {e}"))?;
    let warmup = parse_duration(&args.warmup).map_err(|e| format!("{TOOL}: --warmup: {e}"))?;
    // `--cycle 0` is a legal "never cycle", so it cannot go through
    // `parse_duration` (which rejects zero for the others).
    let cycle = parse_cycle(&args.cycle)?;

    if sample >= duration {
        return Err(format!(
            "{TOOL}: --sample ({}) must be shorter than --duration ({}) — otherwise the run \
             collects no samples",
            format_duration(sample),
            format_duration(duration),
        )
        .into());
    }

    let started_unix = unix_now();
    let label = args
        .label
        .clone()
        .unwrap_or_else(|| format_utc_label(started_unix));
    let out_dir = root.join("target").join("soak").join(&label);
    std::fs::create_dir_all(&out_dir)?;

    let binary = resolve_built_binary(&root, TOOL)?;
    warn_if_stale(&binary, &root);

    let config = RunConfig {
        duration_secs: duration.as_secs_f64(),
        sample_secs: sample.as_secs_f64(),
        cycle_secs: cycle.map_or(0.0, |c| c.as_secs_f64()),
        warmup_secs: warmup.as_secs_f64(),
        sketch: args.sketch.clone(),
        provider: args.provider.clone(),
        activity: if args.natural_idle {
            "natural".to_string()
        } else {
            "active".to_string()
        },
    };

    if !args.json {
        println!(
            "SOAK {label}: {} on {} ({} provider), sampling every {}, cycling every {}.",
            format_duration(duration),
            config.sketch,
            config.provider,
            format_duration(sample),
            cycle.map_or_else(|| "never".to_string(), format_duration),
        );
        println!("     -> {}", out_dir.display());
    }

    let run = launch_and_sample(&root, &binary, &out_dir, &config, duration, sample)?;

    // Warmup samples are recorded but excluded from the fits: an app loading
    // assets and compiling shaders climbs in RSS for reasons that are not a
    // leak, and letting that into the slope would make every short run look
    // like one.
    let fitted: Vec<Sample> = run
        .samples
        .iter()
        .filter(|s| s.t_secs >= warmup.as_secs_f64())
        .cloned()
        .collect();
    // The log lane is not filtered by warmup: a panic at second 3 is a panic.
    let log = logscan::scan_log(&out_dir.join("app.log"));
    let analysis = analyze(&fitted, run.outcome, Thresholds::default(), &log);

    let report = RunReport {
        label,
        started_unix,
        commit: git_short_commit(&root),
        host: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        config,
        outcome: run.outcome,
        elapsed_secs: run.elapsed.as_secs_f64(),
        exit_code: run.exit_code,
        analysis,
        samples_taken: run.samples.len(),
        artifacts: Artifacts {
            dir: out_dir.display().to_string(),
            samples: out_dir.join("samples.ndjson").display().to_string(),
            app_log: out_dir.join("app.log").display().to_string(),
            health: out_dir.join("health.json").display().to_string(),
        },
    };

    write_run_json(&out_dir, &report)?;
    emit(&report, args.json);
    verdict_result(&report)
}

/// What the launch + sample loop observed.
struct RunOutput {
    samples: Vec<Sample>,
    outcome: Outcome,
    elapsed: Duration,
    exit_code: Option<i32>,
}

/// Launch the app under the soak env and sample it until it exits.
fn launch_and_sample(
    root: &Path,
    binary: &Path,
    out_dir: &Path,
    config: &RunConfig,
    duration: Duration,
    sample: Duration,
) -> Result<RunOutput, Box<dyn std::error::Error>> {
    // A fresh config dir per run: an 8-hour verdict must not depend on whatever
    // the operator last left in the settings panel.
    let clean_config = out_dir.join("clean-config");
    std::fs::create_dir_all(&clean_config)?;

    let mut cmd = Command::new(binary);
    cmd.current_dir(root)
        .env("WAVECONDUCTOR_START_SKETCH", &config.sketch)
        .env("WAVECONDUCTOR_HAND_PROVIDER", &config.provider)
        .env("WAVECONDUCTOR_CONFIG_DIR", &clean_config)
        .env("WC_SOAK", build_wc_soak(out_dir, config))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let pid = child.id();

    let log_path = out_dir.join("app.log");
    let handles = spawn_log_tee(&mut child, &log_path)?;

    let health_path = out_dir.join("health.json");
    let samples_path = out_dir.join("samples.ndjson");
    // Truncate rather than append: a re-run under the same `--label` is a fresh
    // run, and silently interleaving two runs' samples would poison the fit.
    let mut samples_file = std::fs::File::create(&samples_path)?;

    let mut samples: Vec<Sample> = Vec::new();
    let start = Instant::now();
    // First sample lands one interval in, not at t=0: a snapshot taken before
    // the app has published anything is a row of nulls.
    let mut next_sample = sample;
    let mut exit_code = None;

    let outcome = loop {
        let elapsed = start.elapsed();

        if let Some(status) = child.try_wait()? {
            exit_code = status.code();
            // A clean completion is *both* "ran the whole duration" and "exited
            // successfully". A panic at hour 7 exits nonzero past the duration,
            // and calling that `Completed` would hand a failing build a pass.
            break if elapsed >= duration && status.success() {
                Outcome::Completed
            } else {
                Outcome::ExitedEarly
            };
        }

        if elapsed >= next_sample {
            let s = take_sample(elapsed, pid, &health_path);
            writeln!(samples_file, "{}", serde_json::to_string(&s)?)?;
            samples_file.flush()?;
            samples.push(s);
            next_sample = (next_sample + sample).max(elapsed);
        }

        if elapsed > duration + EXIT_GRACE {
            let _ = child.kill();
            let _ = child.wait();
            break Outcome::TimedOut;
        }

        std::thread::sleep(TICK);
    };

    for h in handles {
        let _ = h.join();
    }

    Ok(RunOutput {
        samples,
        outcome,
        elapsed: start.elapsed(),
        exit_code,
    })
}

/// Assemble the `WC_SOAK` env value: the app-side half of the schedule.
///
/// Pure over its inputs so the env contract is unit-testable (and readable)
/// without launching anything.
#[must_use]
pub fn build_wc_soak(out_dir: &Path, config: &RunConfig) -> String {
    format!(
        "dir={};duration={};health={};cycle={};activity={}",
        out_dir.display(),
        config.duration_secs,
        // Republish far faster than the launcher samples, so a *stale* snapshot
        // is unambiguous evidence of a frozen app rather than a race between
        // two similar-rate timers.
        health_interval_secs(config.sample_secs),
        config.cycle_secs,
        config.activity,
    )
}

/// The app's `health.json` republish interval for a given launcher sample
/// interval: an order of magnitude faster, clamped to a sane 0.25..=5 s.
#[must_use]
pub fn health_interval_secs(sample_secs: f64) -> f64 {
    (sample_secs / 10.0).clamp(0.25, 5.0)
}

/// Take one joined sample: the app's latest health snapshot + an external RSS.
fn take_sample(elapsed: Duration, pid: u32, health_path: &Path) -> Sample {
    let health = std::fs::read_to_string(health_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<HealthSnapshot>(&raw).ok());
    Sample {
        t_secs: elapsed.as_secs_f64(),
        rss_kib: rss::sample_rss_kib(pid),
        app_uptime_secs: health.as_ref().map(|h| h.uptime_secs),
        fps: health.as_ref().and_then(|h| h.fps),
        max_frame_time_ms: health.as_ref().and_then(|h| h.max_frame_time_ms),
        state: health.as_ref().map(|h| h.state.clone()),
        activity: health.as_ref().and_then(|h| h.activity.clone()),
        thermal_tier: health.as_ref().map(|h| h.thermal_tier.clone()),
        thermal_temp_c: health.as_ref().and_then(|h| h.thermal_temp_c),
        cycles: health.as_ref().map(|h| h.cycles),
    }
}

/// The app's `health.json` shape (see `wc_core::soak::system`). Deserialized
/// here; the app hand-writes it (wc-core has no `serde_json`).
#[derive(Debug, Clone, Deserialize)]
struct HealthSnapshot {
    uptime_secs: f64,
    fps: Option<f64>,
    /// The worst single frame since the app's previous snapshot. `#[serde(default)]`
    /// so a stale app binary (built before this field existed) still parses —
    /// the analysis then reports the hitch lane as blind rather than silently
    /// dropping every other reading in the snapshot.
    #[serde(default)]
    max_frame_time_ms: Option<f64>,
    state: String,
    activity: Option<String>,
    thermal_tier: String,
    thermal_temp_c: Option<f64>,
    cycles: u64,
}

/// `--cycle`: like [`parse_duration`], but `0` is the legal "never cycle".
fn parse_cycle(raw: &str) -> Result<Option<Duration>, Box<dyn std::error::Error>> {
    if raw.trim() == "0" {
        return Ok(None);
    }
    parse_duration(raw)
        .map(Some)
        .map_err(|e| format!("{TOOL}: --cycle: {e}").into())
}

/// Write the self-describing `run.json` sidecar.
fn write_run_json(out_dir: &Path, report: &RunReport) -> Result<(), Box<dyn std::error::Error>> {
    let path = out_dir.join("run.json");
    let mut f = std::fs::File::create(&path)?;
    f.write_all(serde_json::to_string_pretty(report)?.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

/// `--report <dir>`: re-analyze a finished run from its own artifacts.
///
/// Idempotent and offline — this is how a run is reviewed after the fact, and
/// how two runs are compared, without relaunching anything.
fn report_existing(dir: &Path, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let run_json = dir.join("run.json");
    let raw = std::fs::read_to_string(&run_json).map_err(|e| {
        format!(
            "{TOOL}: cannot read {}: {e}\n       \
             --report expects a finished run directory (e.g. target/soak/<label>).",
            run_json.display()
        )
    })?;
    let previous: RunReport = serde_json::from_str(&raw)
        .map_err(|e| format!("{TOOL}: {} is not a soak run.json: {e}", run_json.display()))?;

    let samples = read_samples(&dir.join("samples.ndjson"))?;
    let fitted: Vec<Sample> = samples
        .into_iter()
        .filter(|s| s.t_secs >= previous.config.warmup_secs)
        .collect();
    // Re-scanned, not read back from the old `run.json`: `--report` re-derives
    // every lane from the run's own artifacts, so a fix to the scanner applies
    // to runs already on disk.
    let log = logscan::scan_log(&dir.join("app.log"));
    let analysis = analyze(
        &fitted,
        previous.outcome,
        previous.analysis.thresholds,
        &log,
    );

    let report = RunReport {
        analysis,
        ..previous
    };
    emit(&report, json);
    verdict_result(&report)
}

/// Read `samples.ndjson` — one JSON sample per line. A malformed trailing line
/// (a run killed mid-write) is skipped rather than failing the whole review.
fn read_samples(path: &Path) -> Result<Vec<Sample>, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("{TOOL}: cannot read {}: {e}", path.display()))?;
    Ok(raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Sample>(l).ok())
        .collect())
}

/// Map the verdict onto the process result: only a mechanically-certain failure
/// is an error. A review-required run exits `0` — with a directive nobody can
/// miss, because pretending to a verdict we cannot justify would be worse.
fn verdict_result(report: &RunReport) -> Result<(), Box<dyn std::error::Error>> {
    match report.analysis.verdict {
        Verdict::Fail => Err(format!(
            "{TOOL}: {} FAILED the soak gate — see {}",
            report.label, report.artifacts.dir
        )
        .into()),
        Verdict::Pass | Verdict::Review => Ok(()),
    }
}

fn emit(report: &RunReport, json: bool) {
    if json {
        match serde_json::to_string(report) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("{TOOL}: cannot serialize report: {e}"),
        }
    } else {
        print_human(report);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one linear report renderer; splitting it would scatter the layout"
)]
fn print_human(report: &RunReport) {
    let a = &report.analysis;
    println!();
    println!("SOAK {} [{}]", report.label, report.host);
    println!(
        "  ran      {} ({:?}), {} sample(s), {} sketch cycle(s)",
        format_duration(Duration::from_secs_f64(report.elapsed_secs)),
        report.outcome,
        report.samples_taken,
        a.cycles,
    );
    if let Some(t) = a.rss {
        println!(
            "  RSS      {:.0} -> {:.0} MiB   slope {:+.1} MiB/h (r²={:.2}), min {:.0} / max {:.0}",
            t.first_quarter_mean, t.last_quarter_mean, t.slope_per_hour, t.r_squared, t.min, t.max,
        );
    } else {
        println!("  RSS      (no readings)");
    }
    if let Some(t) = a.fps_active {
        println!(
            "  FPS      {:.1} -> {:.1} (active sketch), decay {:.0}%, min {:.1}",
            t.first_quarter_mean,
            t.last_quarter_mean,
            a.fps_decay.unwrap_or(0.0) * 100.0,
            t.min,
        );
    } else {
        println!("  FPS      (no active-sketch readings)");
    }
    match a.max_hitch_ms {
        Some(worst) => println!(
            "  hitches  {} over {:.0} ms (worst single frame {:.0} ms)",
            a.hitches.len(),
            a.thresholds.hitch_review_ms,
            worst,
        ),
        None => println!("  hitches  (no frame-time watermark — lane blind)"),
    }
    println!("  freezes  {}", a.freezes.len());
    if a.log.is_clean() {
        println!("  log      {} line(s), clean", a.log.lines_scanned);
    } else {
        println!(
            "  log      {} line(s), {} panic(s), {} ERROR(s){}",
            a.log.lines_scanned,
            a.log.panic_count,
            a.log.error_count,
            a.log
                .unreadable
                .as_ref()
                .map_or_else(String::new, |why| format!(" — UNREADABLE: {why}")),
        );
    }
    println!();

    match a.verdict {
        Verdict::Pass => {
            println!(
                "VERDICT: PASS — ran to completion; no leak trend the fit could resolve, no FPS \
                 decay,"
            );
            println!(
                "         no freeze, no frame over {:.0} ms, no panic or ERROR in the log.",
                a.thresholds.hitch_review_ms,
            );
            println!(
                "         PASS is not \"no leak\": see the blind spots in AGENTS.md > Soak testing."
            );
        }
        Verdict::Fail => {
            println!("VERDICT: FAIL");
            for f in &a.failures {
                println!("  - {f}");
            }
            println!();
            println!("  Read the log:     {}", report.artifacts.app_log);
            println!("  Raw samples:      {}", report.artifacts.samples);
        }
        Verdict::Review => {
            println!("VERDICT: REVIEW REQUIRED — this tool will not guess.");
            for r in &a.review {
                println!("  - {r}");
            }
            println!();
            // The pause-and-prompt handoff: the harness has done every
            // deterministic thing it can, and the remaining question is a
            // judgment call. Name the artifacts and hand it to the operator.
            println!("  Claude Code: judge whether the drift above is a leak or a bounded cache");
            println!("  settling, then say so. Read, in order:");
            println!(
                "    samples  {}   (one JSON sample per line: t_secs, rss_kib, fps, state,",
                report.artifacts.samples
            );
            println!("                    activity, thermal_tier)");
            println!(
                "    trends   {}   (fitted slopes + the thresholds they were drawn against)",
                Path::new(&report.artifacts.dir).join("run.json").display(),
            );
            println!(
                "    log      {}   (the app's own stdout+stderr)",
                report.artifacts.app_log
            );
        }
    }
    println!();
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "unwrap/expect are appropriate in test code — every value is a literal"
)]
mod tests {
    use super::*;

    fn config() -> RunConfig {
        RunConfig {
            duration_secs: 28_800.0,
            sample_secs: 30.0,
            cycle_secs: 300.0,
            warmup_secs: 60.0,
            sketch: "line".to_string(),
            provider: "synthetic".to_string(),
            activity: "active".to_string(),
        }
    }

    #[test]
    fn wc_soak_env_carries_the_whole_schedule() {
        let env = build_wc_soak(Path::new("target/soak/x"), &config());
        assert!(env.starts_with("dir=target/soak/x;"), "{env}");
        assert!(env.contains("duration=28800"), "{env}");
        assert!(env.contains("cycle=300"), "{env}");
        assert!(env.contains("activity=active"), "{env}");
        assert!(env.contains("health=3"), "{env}");
    }

    /// The app must republish health far faster than the launcher samples, or a
    /// stale snapshot could not be distinguished from a slow one — the freeze
    /// detector would be unreliable in both directions.
    #[test]
    fn health_interval_is_an_order_of_magnitude_under_the_sample_interval() {
        assert!((health_interval_secs(30.0) - 3.0).abs() < f64::EPSILON);
        // Clamped at both ends: never busier than 4 Hz, never lazier than 5 s.
        assert!((health_interval_secs(1.0) - 0.25).abs() < f64::EPSILON);
        assert!((health_interval_secs(3600.0) - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn cycle_zero_means_never() {
        assert_eq!(parse_cycle("0").unwrap(), None);
        assert_eq!(parse_cycle("5m").unwrap(), Some(Duration::from_mins(5)));
        assert!(parse_cycle("nope").is_err());
    }

    /// The health snapshot the app hand-writes must round-trip into the shape
    /// the launcher deserializes. This is the contract between the two crates;
    /// nothing else checks it.
    #[test]
    fn app_health_json_deserializes() {
        let raw = "{\"uptime_secs\":12.500,\"fps\":59.940,\"frame_time_ms\":16.680,\
                   \"max_frame_time_ms\":33.400,\
                   \"state\":\"Line\",\"activity\":\"Active\",\"thermal_tier\":\"cool\",\
                   \"thermal_temp_c\":45.50,\"published\":3,\"cycles\":1}\n";
        let h: HealthSnapshot = serde_json::from_str(raw).expect("app's health.json parses");
        assert!((h.uptime_secs - 12.5).abs() < f64::EPSILON);
        assert_eq!(h.fps, Some(59.94));
        assert_eq!(h.max_frame_time_ms, Some(33.4));
        assert_eq!(h.state, "Line");
        assert_eq!(h.activity.as_deref(), Some("Active"));
        assert_eq!(h.thermal_tier, "cool");
        assert_eq!(h.cycles, 1);
    }

    /// The nulls the app writes when a reading is unavailable must parse too.
    #[test]
    fn app_health_json_with_nulls_deserializes() {
        let raw = "{\"uptime_secs\":1.0,\"fps\":null,\"frame_time_ms\":null,\
                   \"max_frame_time_ms\":null,\"state\":\"Home\",\
                   \"activity\":null,\"thermal_tier\":\"unknown\",\"thermal_temp_c\":null,\
                   \"published\":1,\"cycles\":0}";
        let h: HealthSnapshot = serde_json::from_str(raw).expect("parses");
        assert_eq!(h.fps, None);
        assert_eq!(h.max_frame_time_ms, None);
        assert_eq!(h.activity, None);
        assert_eq!(h.thermal_temp_c, None);
    }

    /// A snapshot from an app binary older than the hitch lane must still parse:
    /// dropping the whole snapshot would blind *every* lane (uptime, FPS, state)
    /// over a stale binary, which is a far worse failure than one absent field.
    /// The analysis reports the missing watermark as a blind lane instead.
    #[test]
    fn a_health_json_without_the_watermark_still_parses() {
        let raw = "{\"uptime_secs\":1.0,\"fps\":60.0,\"frame_time_ms\":16.6,\"state\":\"Line\",\
                   \"activity\":\"Active\",\"thermal_tier\":\"cool\",\"thermal_temp_c\":null,\
                   \"published\":1,\"cycles\":0}";
        let h: HealthSnapshot = serde_json::from_str(raw).expect("parses without the field");
        assert_eq!(h.max_frame_time_ms, None);
        assert_eq!(h.fps, Some(60.0));
    }

    #[test]
    fn samples_ndjson_round_trips_and_skips_a_torn_final_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("samples.ndjson");
        let s = Sample {
            t_secs: 30.0,
            rss_kib: Some(412_345),
            app_uptime_secs: Some(29.5),
            fps: Some(60.0),
            max_frame_time_ms: Some(18.2),
            state: Some("Line".to_string()),
            activity: Some("Active".to_string()),
            thermal_tier: Some("cool".to_string()),
            thermal_temp_c: Some(45.0),
            cycles: Some(0),
        };
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{}", serde_json::to_string(&s).unwrap()).unwrap();
        // A run killed mid-write leaves a torn line; reviewing must still work.
        write!(f, "{{\"t_secs\":60.0,\"rss_ki").unwrap();
        drop(f);

        let read = read_samples(&path).expect("reads");
        assert_eq!(
            read,
            vec![s],
            "the complete sample survives; the torn one is dropped"
        );
    }

    #[test]
    fn missing_run_dir_reports_a_useful_error() {
        let err =
            report_existing(Path::new("target/soak/does-not-exist"), false).expect_err("must fail");
        let msg = err.to_string();
        assert!(msg.contains("cannot read"), "{msg}");
        assert!(msg.contains("finished run directory"), "{msg}");
    }
}
