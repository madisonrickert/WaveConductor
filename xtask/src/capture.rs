//! `cargo xtask capture <scenario>` — orchestrate a deterministic capture run,
//! compute metrics, diff baselines, and report.
//!
//! Independent of `wc-core`/`wc-sketches`: this launches the pre-built DEBUG
//! `waveconductor` binary (`target/debug/waveconductor`), teeing its output to
//! `<dir>/app.log`, then reads the PNGs + `run.json` the app wrote. It does NOT
//! build the app — build it first with `cargo build -p waveconductor` (a
//! separate, watchable step); capture fails fast if the binary is missing, so a
//! cold build can never be misattributed to the launch-timeout safety net.
//!
//! ## Signal flow
//!
//! 1. Resolve `<scenario>` from `tests/visual/scenarios.toml`.
//! 2. Assemble env: `WAVECONDUCTOR_START_SKETCH`, `WAVECONDUCTOR_HAND_PROVIDER`,
//!    `WAVECONDUCTOR_CONFIG_DIR` (fresh temp unless pinned), `WC_DEBUG_*`
//!    (scenario + `--debug` overrides), and `WC_CAPTURE` (the capture schedule).
//! 3. Launch the DEBUG binary; tee stdout+stderr to `<dir>/app.log`; enforce a
//!    wall-clock timeout safety net (the app self-exits via `AppExit`).
//! 4. Read the PNGs + `run.json`; compute metrics (`metrics`) -> `metrics.json`;
//!    diff each frame vs its committed baseline (`diff`).
//! 5. Report: human table (default) or `--json` (per-frame metrics + diff
//!    verdict + paths + which frames to open). Exit 0 on pass / nonzero on
//!    regression.

#![allow(clippy::print_stdout, reason = "xtask is a CLI; printing is its job")]

pub mod diff;
pub mod metrics;
pub mod scenarios;

use std::collections::BTreeMap;
// Both `Write` traits are imported anonymously: `io::Write` for `write_all` to
// files, `fmt::Write` for `write!` into a `String`. Trait method resolution
// selects the right one by receiver type, so the `_` aliases never collide.
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clap::Args as ClapArgs;

use diff::diff_frames;
use metrics::{global_std, luma_from_mean, region_mean, FrameMetrics, Region};
use scenarios::{Scenario, Scenarios};

use crate::util::{
    git_short_commit, resolve_built_binary, spawn_log_tee, warn_if_stale, workspace_root,
};

/// Subcommand name used in this module's operator-facing error messages.
const TOOL: &str = "capture";

/// Per-pixel max-channel delta above which a pixel counts as changed.
const PIXEL_THRESHOLD: u8 = 12;

/// Mean-abs-diff tolerance (0..=255) below which a frame passes the baseline.
const DIFF_TOLERANCE: f64 = 6.0;

/// Mean-luma floor (0..=255 Rec. 601) below which a frame is treated as
/// near-zero-luminance ("all-black") by the `--update-baselines` guard. This
/// is the signature of an unrendered/backgrounded capture (see the black-frame
/// trap documented in `tests/visual/CLAUDE.md`), not a legitimately dark
/// sketch frame — real sketch output always has some non-zero structure even
/// at its darkest.
const BLACK_LUMA_THRESHOLD: f64 = 1.0;

/// Wall-clock safety timeout for the launched app (seconds). The app normally
/// self-exits via `AppExit` after the last scheduled frame; this is the net for
/// the case where a screenshot observer never fires.
const LAUNCH_TIMEOUT_SECS: u64 = 90;

/// Arguments for the capture subcommand.
#[derive(ClapArgs)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "clap CLI flags — each bool is an independent --flag toggle, not packed state"
)]
pub struct Args {
    /// Scenario name from `tests/visual/scenarios.toml`. Omit with `--list`.
    pub scenario: Option<String>,
    /// Copy the freshly-captured frames into the baseline dir (no tolerance
    /// diff gate — but see `--allow-black`, which *is* a gate).
    #[arg(long)]
    pub update_baselines: bool,
    /// Let `--update-baselines` bless near-zero-luminance (all-black) frames.
    /// Only pass this when black is genuinely the correct rendered output;
    /// otherwise an all-black frame almost always means the app window wasn't
    /// foregrounded during capture (see `tests/visual/CLAUDE.md`).
    #[arg(long)]
    pub allow_black: bool,
    /// Emit machine-readable JSON instead of the human table.
    #[arg(long)]
    pub json: bool,
    /// Launch the scenario for hands-on inspection (no capture); quit after N
    /// seconds (default 10). Runs the normal variable-dt clock.
    #[arg(long, value_name = "SECS", num_args = 0..=1, default_missing_value = "10")]
    pub watch: Option<u64>,
    /// List available scenarios and exit.
    #[arg(long)]
    pub list: bool,
    /// Ad-hoc `WC_DEBUG_*` overrides as `KEY=VAL` (KEY without the prefix).
    #[arg(long = "debug", value_name = "KEY=VAL")]
    pub debug: Vec<String>,
}

/// Execute the capture subcommand.
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let root = workspace_root();
    let scenarios = load_scenarios(&root)?;

    if args.list {
        print_list(&scenarios, args.json);
        return Ok(());
    }

    let name = args
        .scenario
        .as_deref()
        .ok_or("capture: a scenario name is required (or use --list)")?;
    let scenario = scenarios
        .get(name)
        .ok_or_else(|| format!("capture: unknown scenario {name:?}; try --list"))?;

    let out_dir = root.join("target").join("capture").join(name);
    std::fs::create_dir_all(&out_dir)?;

    if let Some(secs) = args.watch {
        return run_watch(&root, scenario, secs);
    }

    launch(&root, name, scenario, &out_dir, &args.debug)?;

    let report = analyze(&root, name, scenario, &out_dir)?;

    if args.update_baselines {
        update_baselines(&root, name, scenario, &out_dir, &report, args.allow_black)?;
        if args.json {
            println!("{{\"scenario\":\"{name}\",\"updated_baselines\":true}}");
        } else {
            println!("Updated baselines for {name}.");
        }
        return Ok(());
    }

    let passed = report.frames.iter().all(|f| f.passed);
    if args.json {
        print_json_report(name, &out_dir, &report);
    } else {
        print_human_report(name, &report);
    }
    if passed {
        Ok(())
    } else {
        Err(format!("capture: {name} regressed beyond tolerance").into())
    }
}

/// Assemble the `WC_CAPTURE` env value for a scenario + output dir.
///
/// `name` and `commit` are threaded into the schedule string so the app can
/// record them in `run.json` for provenance (the app is otherwise unaware of
/// the scenario name or the repo state). `commit` is `None` outside a git repo.
pub fn build_wc_capture(
    name: &str,
    scenario: &Scenario,
    out_dir: &Path,
    commit: Option<&str>,
) -> String {
    let frames = scenario
        .frames
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut wc = format!("dir={};frames={}", out_dir.display(), frames);
    if let Some(dt) = scenario.dt {
        // `write!` to a `String` is infallible; the discard documents that.
        let _ = write!(wc, ";dt={dt}");
    }
    let _ = write!(wc, ";scenario={name}");
    if let Some(commit) = commit {
        let _ = write!(wc, ";commit={commit}");
    }
    wc
}

/// Merge CLI `--debug KEY=VAL` overrides over a scenario's `debug` table. CLI
/// values win; new keys are added.
pub fn merge_debug(scenario: &Scenario, overrides: &[String]) -> BTreeMap<String, String> {
    let mut merged = scenario.debug.clone();
    for ov in overrides {
        if let Some((k, v)) = ov.split_once('=') {
            merged.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    merged
}

/// Turn a merged debug table into `(WC_DEBUG_<KEY>, VAL)` env pairs.
pub fn debug_env_pairs(merged: &BTreeMap<String, String>) -> Vec<(String, String)> {
    merged
        .iter()
        .map(|(k, v)| (format!("WC_DEBUG_{k}"), v.clone()))
        .collect()
}

/// The `WC_CAPTURE_RESOLUTION` value for a scenario: `"WxH"` when the scenario
/// pins a window resolution, `None` when it relies on the app's 1280x720
/// default. The env var is honoured by debug builds only (the override is
/// `#[cfg(debug_assertions)]`-gated in the app, like the rest of `WC_CAPTURE`).
pub fn resolution_env(scenario: &Scenario) -> Option<String> {
    scenario.resolution.map(|[w, h]| format!("{w}x{h}"))
}

// ---- private orchestration helpers --------------------------------------

/// Load `tests/visual/scenarios.toml`.
fn load_scenarios(root: &Path) -> Result<Scenarios, Box<dyn std::error::Error>> {
    let path = root.join("tests").join("visual").join("scenarios.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("capture: cannot read {}: {e}", path.display()))?;
    Ok(toml::from_str(&text)?)
}

/// Launch the debug binary with scenario env + capture schedule, teeing
/// stdout+stderr to `<dir>/app.log`, enforcing a wall-clock timeout.
fn launch(
    root: &Path,
    name: &str,
    scenario: &Scenario,
    out_dir: &Path,
    cli_debug: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let commit = git_short_commit(root);
    let binary = resolve_built_binary(root, TOOL)?;
    warn_if_stale(&binary, root);
    let mut cmd = Command::new(&binary);
    cmd.current_dir(root)
        .env("WAVECONDUCTOR_START_SKETCH", &scenario.sketch)
        .env("WAVECONDUCTOR_HAND_PROVIDER", &scenario.provider)
        .env(
            "WC_CAPTURE",
            build_wc_capture(name, scenario, out_dir, commit.as_deref()),
        );

    // Optional per-scenario window resolution (portrait scenarios etc.); the
    // app's debug-only window override reads it at startup.
    if let Some(res) = resolution_env(scenario) {
        cmd.env("WC_CAPTURE_RESOLUTION", res);
    }

    // Config isolation: a fresh temp dir for `config = "clean"`, else a pinned
    // path. The temp dir is created under the output dir so it is inspectable.
    if scenario.config == "clean" {
        let clean = out_dir.join("clean-config");
        std::fs::create_dir_all(&clean)?;
        cmd.env("WAVECONDUCTOR_CONFIG_DIR", &clean);
    } else {
        cmd.env("WAVECONDUCTOR_CONFIG_DIR", &scenario.config);
    }

    for (k, v) in debug_env_pairs(&merge_debug(scenario, cli_debug)) {
        cmd.env(k, v);
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    // Drain both pipes into app.log (shared with soak-test; see `util`).
    let log_path = out_dir.join("app.log");
    let handles = spawn_log_tee(&mut child, &log_path)?;

    // Wall-clock timeout safety net (the app self-exits via AppExit normally).
    let start = std::time::Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if start.elapsed().as_secs() > LAUNCH_TIMEOUT_SECS {
            let _ = child.kill();
            return Err(format!(
                "capture: app did not exit within {LAUNCH_TIMEOUT_SECS}s; see {}",
                log_path.display()
            )
            .into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

/// `--watch`: launch for hands-on inspection (no `WC_CAPTURE`), kill after N s.
fn run_watch(
    root: &Path,
    scenario: &Scenario,
    secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let binary = resolve_built_binary(root, TOOL)?;
    warn_if_stale(&binary, root);
    let mut cmd = Command::new(&binary);
    cmd.current_dir(root)
        .env("WAVECONDUCTOR_START_SKETCH", &scenario.sketch)
        .env("WAVECONDUCTOR_HAND_PROVIDER", &scenario.provider);
    // Match the capture run's window size so what you watch is what captures.
    if let Some(res) = resolution_env(scenario) {
        cmd.env("WC_CAPTURE_RESOLUTION", res);
    }
    let mut child = cmd.spawn()?;
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < secs {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = child.kill();
    Ok(())
}

/// One frame's report row.
struct FrameReport {
    frame: u32,
    metrics: FrameMetrics,
    mean_abs_diff: Option<f64>,
    passed: bool,
    current_path: PathBuf,
    baseline_path: Option<PathBuf>,
}

/// Aggregate report.
struct Report {
    frames: Vec<FrameReport>,
}

/// Read PNGs + run.json, compute metrics + baseline diffs.
fn analyze(
    root: &Path,
    name: &str,
    scenario: &Scenario,
    out_dir: &Path,
) -> Result<Report, Box<dyn std::error::Error>> {
    let baseline_dir = root
        .join("tests")
        .join("visual")
        .join("baselines")
        .join(name);
    let mut frames = Vec::new();
    let mut prev: Option<image::RgbaImage> = None;

    // Write metrics.json alongside the report.
    let mut metrics_out: Vec<FrameMetrics> = Vec::new();

    for &frame in &scenario.frames {
        let current_path = out_dir.join(format!("frame_{frame:04}.png"));
        let current = image::open(&current_path)
            .map_err(|e| format!("capture: cannot read {}: {e}", current_path.display()))?
            .to_rgba8();

        let delta_prev = prev
            .as_ref()
            .map(|p| metrics::frame_mean_abs_delta(p, &current));
        let fm = FrameMetrics {
            frame,
            full_mean: region_mean(&current, Region::Full),
            center_mean: region_mean(&current, Region::Center),
            global_std: global_std(&current),
            delta_prev,
        };
        metrics_out.push(fm.clone());

        let baseline_path = baseline_dir.join(format!("frame_{frame:04}.png"));
        let (mean_abs_diff, passed, baseline_ref) = if baseline_path.exists() {
            let baseline = image::open(&baseline_path)?.to_rgba8();
            let d = diff_frames(&current, &baseline, PIXEL_THRESHOLD);
            (
                Some(d.mean_abs_diff),
                d.passes(DIFF_TOLERANCE),
                Some(baseline_path),
            )
        } else {
            // No baseline yet -> cannot regress; flag for the agent to review.
            (None, true, None)
        };

        frames.push(FrameReport {
            frame,
            metrics: fm,
            mean_abs_diff,
            passed,
            current_path,
            baseline_path: baseline_ref,
        });
        prev = Some(current);
    }

    let metrics_path = out_dir.join("metrics.json");
    let mut f = std::fs::File::create(&metrics_path)?;
    f.write_all(serde_json::to_string_pretty(&metrics_out)?.as_bytes())?;

    Ok(Report { frames })
}

/// Frame indices from `report` whose mean luma falls below `threshold`
/// (0..=255 Rec. 601) — the near-zero-luminance guard for
/// [`update_baselines`]. Pulled out as a pure function over an already-built
/// `Report` (reusing `full_mean`, computed once in [`analyze`]) so the
/// detection logic is unit-testable without touching disk or the app.
fn near_black_frames(report: &Report, threshold: f64) -> Vec<u32> {
    report
        .frames
        .iter()
        .filter(|f| luma_from_mean(f.metrics.full_mean) < threshold)
        .map(|f| f.frame)
        .collect()
}

/// Copy captured frames into the baseline dir (plain committed PNGs, no LFS).
///
/// Refuses to bless a batch containing a near-zero-luminance ("all-black")
/// frame unless `allow_black` is set: seeding a baseline from an
/// unrendered/backgrounded capture (see the black-frame trap documented in
/// `tests/visual/CLAUDE.md`) would commit a PNG that can never honestly match
/// a correctly-rendered frame, silently reintroducing the exact
/// orphaned-baseline problem this guard exists to prevent.
fn update_baselines(
    root: &Path,
    name: &str,
    scenario: &Scenario,
    out_dir: &Path,
    report: &Report,
    allow_black: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !allow_black {
        let black = near_black_frames(report, BLACK_LUMA_THRESHOLD);
        if !black.is_empty() {
            return Err(format!(
                "capture: refusing to bless {name} baselines — frame(s) {black:?} are near-zero \
                 luminance (all-black, mean luma < {BLACK_LUMA_THRESHOLD}). This is almost always the \
                 app window not being foregrounded during capture, not a real render (see \
                 tests/visual/CLAUDE.md); re-run in the foreground, or pass --allow-black if black is \
                 genuinely the correct rendered output."
            )
            .into());
        }
    }

    let baseline_dir = root
        .join("tests")
        .join("visual")
        .join("baselines")
        .join(name);
    std::fs::create_dir_all(&baseline_dir)?;
    for &frame in &scenario.frames {
        let src = out_dir.join(format!("frame_{frame:04}.png"));
        let dst = baseline_dir.join(format!("frame_{frame:04}.png"));
        std::fs::copy(&src, &dst)
            .map_err(|e| format!("capture: cannot copy baseline {}: {e}", dst.display()))?;
    }
    Ok(())
}

fn print_list(scenarios: &Scenarios, json: bool) {
    if json {
        let names: Vec<String> = scenarios
            .names()
            .into_iter()
            .map(|n| format!("\"{n}\""))
            .collect();
        println!("[{}]", names.join(","));
    } else {
        println!("SCENARIOS");
        for n in scenarios.names() {
            println!("  {n}");
        }
    }
}

fn print_human_report(name: &str, report: &Report) {
    println!("CAPTURE {name}");
    println!(
        "{:<8} {:<22} {:<10} {:<10} VERDICT",
        "FRAME", "FULL_MEAN(RGB)", "STD", "DIFF"
    );
    for f in &report.frames {
        let diff = f
            .mean_abs_diff
            .map_or_else(|| "n/a".to_string(), |d| format!("{d:.2}"));
        let verdict = if f.baseline_path.is_none() {
            "NEW (review)"
        } else if f.passed {
            "pass"
        } else {
            "REGRESS (open)"
        };
        println!(
            "{:<8} {:<22} {:<10.2} {:<10} {}",
            f.frame,
            format!(
                "{:.0},{:.0},{:.0}",
                f.metrics.full_mean[0], f.metrics.full_mean[1], f.metrics.full_mean[2]
            ),
            f.metrics.global_std,
            diff,
            verdict,
        );
    }
    let to_open: Vec<String> = report
        .frames
        .iter()
        .filter(|f| !f.passed || f.baseline_path.is_none())
        .map(|f| f.current_path.display().to_string())
        .collect();
    if to_open.is_empty() {
        println!("All frames within tolerance.");
    } else {
        println!("Open & judge these frames:");
        for p in to_open {
            println!("  {p}");
        }
    }
}

fn print_json_report(name: &str, out_dir: &Path, report: &Report) {
    // Hand-rolled JSON so the shape is explicit and stable for the agent.
    let mut frames_json = Vec::new();
    for f in &report.frames {
        let diff = f
            .mean_abs_diff
            .map_or_else(|| "null".to_string(), |d| format!("{d:.4}"));
        let baseline = f
            .baseline_path
            .as_ref()
            .map_or_else(|| "null".to_string(), |p| format!("\"{}\"", p.display()));
        frames_json.push(format!(
            "{{\"frame\":{},\"full_mean\":[{:.2},{:.2},{:.2}],\"center_mean\":[{:.2},{:.2},{:.2}],\"global_std\":{:.4},\"mean_abs_diff\":{},\"passed\":{},\"current\":\"{}\",\"baseline\":{}}}",
            f.frame,
            f.metrics.full_mean[0], f.metrics.full_mean[1], f.metrics.full_mean[2],
            f.metrics.center_mean[0], f.metrics.center_mean[1], f.metrics.center_mean[2],
            f.metrics.global_std,
            diff,
            f.passed,
            f.current_path.display(),
            baseline,
        ));
    }
    let open: Vec<String> = report
        .frames
        .iter()
        .filter(|f| !f.passed || f.baseline_path.is_none())
        .map(|f| format!("\"{}\"", f.current_path.display()))
        .collect();
    let passed = report.frames.iter().all(|f| f.passed);
    println!(
        "{{\"scenario\":\"{}\",\"dir\":\"{}\",\"passed\":{},\"frames\":[{}],\"open_for_review\":[{}]}}",
        name,
        out_dir.display(),
        passed,
        frames_json.join(","),
        open.join(","),
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "expect is appropriate in test code")]

    use super::*;
    use crate::capture::scenarios::Scenario;
    use std::collections::BTreeMap;

    fn scenario() -> Scenario {
        Scenario {
            sketch: "line".into(),
            provider: "synthetic".into(),
            config: "clean".into(),
            debug: BTreeMap::from([("FORCE_G".into(), "8000".into())]),
            frames: vec![30, 60],
            // Digit separators satisfy `clippy::unreadable_literal`; the parsed
            // `f64` value (and thus its formatted string) is unchanged.
            dt: Some(0.016_666_667),
            resolution: None,
        }
    }

    #[test]
    fn resolution_env_is_absent_by_default() {
        assert_eq!(resolution_env(&scenario()), None);
    }

    #[test]
    fn resolution_env_formats_wxh() {
        let mut s = scenario();
        s.resolution = Some([1080, 1920]);
        assert_eq!(resolution_env(&s).as_deref(), Some("1080x1920"));
    }

    #[test]
    fn builds_wc_capture_string() {
        let s = scenario();
        let wc = build_wc_capture(
            "line-synthetic",
            &s,
            std::path::Path::new("target/capture/x"),
            Some("abc1234"),
        );
        assert!(wc.starts_with("dir=target/capture/x;frames=30,60"));
        assert!(wc.contains("dt=0.016666667"));
        assert!(wc.contains("scenario=line-synthetic"));
        assert!(wc.contains("commit=abc1234"));
    }

    #[test]
    fn wc_capture_omits_commit_when_absent() {
        let s = scenario();
        let wc = build_wc_capture("line-synthetic", &s, std::path::Path::new("out"), None);
        assert!(wc.contains("scenario=line-synthetic"));
        assert!(!wc.contains("commit="));
    }

    #[test]
    fn cli_debug_overrides_merge_over_scenario() {
        let s = scenario();
        let overrides = vec!["FORCE_G=4000".to_string(), "DISABLE_SMEAR=1".to_string()];
        let merged = merge_debug(&s, &overrides);
        assert_eq!(merged.get("FORCE_G").map(String::as_str), Some("4000")); // overridden
        assert_eq!(merged.get("DISABLE_SMEAR").map(String::as_str), Some("1")); // added
    }

    #[test]
    fn env_pairs_prefix_wc_debug() {
        let merged = BTreeMap::from([("FORCE_G".to_string(), "8000".to_string())]);
        let pairs = debug_env_pairs(&merged);
        assert!(pairs.contains(&("WC_DEBUG_FORCE_G".to_string(), "8000".to_string())));
    }

    /// A [`FrameReport`] with only `frame` and `metrics.full_mean` set
    /// meaningfully — the two fields [`near_black_frames`] reads. Other
    /// fields are filled with harmless placeholders.
    fn frame_report(frame: u32, full_mean: [f64; 3]) -> FrameReport {
        FrameReport {
            frame,
            metrics: FrameMetrics {
                frame,
                full_mean,
                center_mean: full_mean,
                global_std: 0.0,
                delta_prev: None,
            },
            mean_abs_diff: None,
            passed: true,
            current_path: PathBuf::from(format!("frame_{frame:04}.png")),
            baseline_path: None,
        }
    }

    #[test]
    fn near_black_frames_flags_only_dark_frames() {
        let report = Report {
            frames: vec![
                frame_report(30, [0.0, 0.0, 0.0]),     // all-black
                frame_report(60, [120.0, 80.0, 60.0]), // normal rendered frame
                frame_report(90, [0.3, 0.2, 0.1]),     // still effectively black
            ],
        };
        assert_eq!(
            near_black_frames(&report, BLACK_LUMA_THRESHOLD),
            vec![30, 90]
        );
    }

    #[test]
    fn near_black_frames_empty_when_all_lit() {
        let report = Report {
            frames: vec![frame_report(30, [10.0, 10.0, 10.0])],
        };
        assert!(near_black_frames(&report, BLACK_LUMA_THRESHOLD).is_empty());
    }
}
