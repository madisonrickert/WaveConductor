//! In-app soak instrumentation: publish a health snapshot, cycle sketches,
//! self-exit when the configured duration elapses.
//!
//! ## The channel
//!
//! The app's health surface is a single small file, `<dir>/health.json`,
//! rewritten every [`SoakConfig::health`] seconds with the *latest* readings
//! (never appended to — it is a snapshot, not a log, so it stays O(1) on disk
//! for an 8-hour run). `cargo xtask soak-test` polls it on its own, coarser
//! schedule and joins each read with an externally-measured RSS to form one
//! sample row. This deliberately reuses the `WC_CAPTURE` shape (env in,
//! self-describing file out) instead of opening a network port or an IPC
//! server: the launcher already owns the process, and a file is inspectable
//! after the fact by a human or an agent.
//!
//! The write is `health.json.tmp` + `rename`, which is atomic on every
//! filesystem we deploy to, so the launcher can never read a half-written
//! snapshot.
//!
//! ## Staleness is the freeze detector
//!
//! Each snapshot carries the app's own `uptime_secs`. If the launcher takes two
//! samples and the app's clock has not moved between them, the render loop is
//! wedged — a signal an external RSS/FPS poll alone cannot produce.
//!
//! ## Cost
//!
//! Every system here early-returns without a [`SoakConfig`] (i.e. on every
//! normal run), and the module as a whole is `#[cfg(debug_assertions)]`-gated
//! by its parent. Under an active soak the snapshot is formatted into a scratch
//! `String` owned by [`SoakRuntime`] and `clear()`ed each time, so the 8-hour
//! steady state performs no growing allocation.
//!
//! ## Sketch cycling is a graceful reload, not an instant `NextState` write
//!
//! The cycle timer routes through [`SketchReloadState::begin_fade_out`] with
//! [`ReloadReason::SketchSwitch`] — the same machine `nav::handle_navigation_actions`
//! drives for a live keypress (see `crate::lifecycle::nav`'s module doc) —
//! rather than calling `NextState::set` directly. Two reasons: it keeps an
//! unattended soak run's sketch transitions representative of what a real
//! visitor sees (dip to black, not an instant cut), and it exercises the
//! *graceful* teardown/rebuild path for the multi-hour duration this harness
//! exists to stress, rather than only ever testing the instant one. If a
//! reload is already in flight when a cycle comes due (a settings-restart or
//! resize reload racing the cycle timer — vanishingly unlikely at the default
//! 5-minute cycle interval, but not impossible), the cycle is skipped for
//! this tick rather than retargeting the in-flight reload, mirroring
//! `nav::handle_navigation_actions`'s same choice; the schedule still
//! advances by a full interval either way so a stuck reload cannot wedge the
//! cycle timer into firing every frame.

use std::fmt::Write as _;
use std::io::Write as _;
use std::time::Duration;

use bevy::app::AppExit;
use bevy::diagnostic::{Diagnostic, DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::ecs::message::MessageWriter;
use bevy::prelude::*;

use super::config::{SoakActivity, SoakConfig};
use crate::audio::state::AudioState;
use crate::lifecycle::idle::InteractionTimer;
use crate::lifecycle::reload::{ReloadReason, SketchReloadState};
use crate::lifecycle::state::{AppState, SketchActivity};
use crate::lifecycle::thermal::ThermalState;

/// Soak progress: when the next snapshot / sketch advance is due, how many
/// snapshots have been published, and the reusable formatting buffer.
#[derive(Resource, Debug, Default)]
pub struct SoakRuntime {
    /// Wall-clock offset at which the next `health.json` publish is due.
    next_health: Duration,
    /// Wall-clock offset at which the next sketch advance is due. Only
    /// meaningful when [`SoakConfig::cycle`] is `Some`.
    next_cycle: Duration,
    /// Snapshots published so far (recorded in each snapshot).
    published: u64,
    /// Sketch advances performed so far (recorded in each snapshot, so the
    /// launcher's report can state how much enter/exit lifecycle the run
    /// actually exercised).
    cycles: u64,
    /// True once `AppExit` has been requested, so we request it only once.
    exit_requested: bool,
    /// Longest single frame (in milliseconds) seen *since the last publish*.
    ///
    /// The high-water mark exists because the launcher's other freeze signal —
    /// `uptime_secs` failing to advance between two launcher samples — can only
    /// resolve a wedge longer than the sample interval (30 s by default). A
    /// 25-second hitch every ten minutes advances the app's clock plenty
    /// between samples and is invisible to it, while Bevy's *smoothed* FPS
    /// recovers long before the next sample too. A watermark that survives to
    /// the next publish is the only lane that sees it. Reset on each publish,
    /// so it describes the interval, not the whole run.
    max_frame_time_ms: f32,
    /// Reused snapshot-formatting buffer — never reallocated in steady state.
    scratch: String,
}

impl SoakRuntime {
    /// Arm the schedule for `config`.
    ///
    /// The first *health* snapshot is due immediately (`next_health` = 0): a
    /// launcher sampling at t=5 s should not find an empty directory. The first
    /// *cycle*, by contrast, is due one full interval in — arming it at zero
    /// would advance the sketch on frame one, silently discarding the
    /// `--sketch` the operator asked the run to start on (observed, and fixed,
    /// in the first end-to-end smoke run).
    #[must_use]
    pub fn new(config: &SoakConfig) -> Self {
        Self {
            next_health: Duration::ZERO,
            next_cycle: config.cycle.unwrap_or(Duration::ZERO),
            ..Self::default()
        }
    }
}

/// A snapshot of everything the app can say about its own health, in the shape
/// written to `health.json`. Pulled out of the system as a plain struct so the
/// serializer below is a pure function over it.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthSnapshot {
    /// The app's own wall-clock uptime, in seconds. The launcher's freeze
    /// detector watches this for advancement.
    pub uptime_secs: f64,
    /// Smoothed FPS from `FrameTimeDiagnosticsPlugin`. `None` before the
    /// diagnostic has enough history.
    pub fps: Option<f64>,
    /// Smoothed frame time in milliseconds. `None` as for `fps`.
    pub frame_time_ms: Option<f64>,
    /// Longest single frame, in milliseconds, since the previous snapshot. Not
    /// smoothed and not averaged: this is the hitch lane, and a hitch that is
    /// averaged away is a hitch that ships.
    pub max_frame_time_ms: f32,
    /// Current top-level state (`Line`, `Dots`, `Home`, ...).
    pub state: String,
    /// Current sketch activity (`Active`, `Idle`, `Screensaver`), or `None` at
    /// `Home`, where the sub-state does not exist.
    pub activity: Option<String>,
    /// Current thermal tier (`cool` / `warm` / `hot`).
    pub thermal_tier: String,
    /// Latest raw temperature, when a sensor produced one.
    pub thermal_temp_c: Option<f32>,
    /// Snapshots published so far, including this one.
    pub published: u64,
    /// Sketch advances performed so far.
    pub cycles: u64,
}

/// Per-frame soak driver: publish a snapshot when due, advance the sketch when
/// due, and request `AppExit` once the configured duration elapses.
///
/// No-op without a [`SoakConfig`] (every normal run).
#[allow(
    clippy::too_many_arguments,
    reason = "a Bevy system's arguments are its dependency list; splitting this \
              across systems would duplicate the same resource reads across systems"
)]
pub fn drive_soak(
    config: Option<Res<'_, SoakConfig>>,
    runtime: Option<ResMut<'_, SoakRuntime>>,
    time: Res<'_, Time<Real>>,
    reload_clock: Res<'_, Time>,
    diagnostics: Option<Res<'_, DiagnosticsStore>>,
    thermal: Option<Res<'_, ThermalState>>,
    app_state: Option<Res<'_, State<AppState>>>,
    activity: Option<Res<'_, State<SketchActivity>>>,
    reload_state: Option<ResMut<'_, SketchReloadState>>,
    audio_state: Option<Res<'_, AudioState>>,
    mut exit: MessageWriter<'_, AppExit>,
) {
    let (Some(config), Some(mut runtime)) = (config, runtime) else {
        return;
    };
    let elapsed = time.elapsed();

    // The hitch watermark, updated every frame: one `f32` compare, no branch of
    // consequence, no allocation. `Time<Real>`'s delta is the *wall-clock* length
    // of the previous frame, which is exactly what a wedged frame is long in —
    // a virtual-time delta would be clamped and hide the very thing we hunt.
    let frame_ms = time.delta().as_secs_f32() * 1000.0;
    if frame_ms > runtime.max_frame_time_ms {
        runtime.max_frame_time_ms = frame_ms;
    }

    if elapsed >= runtime.next_health {
        runtime.published += 1;
        let snapshot = snapshot(
            elapsed,
            diagnostics.as_deref(),
            thermal.as_deref(),
            app_state.as_deref(),
            activity.as_deref(),
            runtime.published,
            runtime.cycles,
            runtime.max_frame_time_ms,
        );
        publish(&config, &mut runtime, &snapshot);
        // Reset the watermark *after* publishing it: each snapshot reports the
        // worst frame of the interval it closes, not of the run so far. A
        // run-long maximum would report one hitch forever and could never say
        // *when* it happened.
        runtime.max_frame_time_ms = 0.0;
        // Advance by whole intervals from the deadline (not from `elapsed`), so
        // a late frame does not drift the schedule; `max` guarantees progress
        // even if a very long hitch skipped several intervals.
        runtime.next_health = (runtime.next_health + config.health).max(elapsed);
    }

    if let Some(cycle) = config.cycle {
        if elapsed >= runtime.next_cycle {
            if let (Some(current), Some(mut reload_state)) = (app_state.as_deref(), reload_state) {
                if reload_state.is_idle() {
                    let next = current.get().next_sketch();
                    tracing::info!(?next, "soak: cycling sketch");
                    let pre_fade_volume = audio_state.as_deref().map_or(1.0, |s| s.volume);
                    reload_state.begin_fade_out(
                        reload_clock.elapsed(),
                        pre_fade_volume,
                        next,
                        ReloadReason::SketchSwitch,
                    );
                    runtime.cycles += 1;
                } else {
                    // A reload was already in flight when the cycle came due
                    // (see the module doc) — skip this tick rather than
                    // retargeting it.
                    tracing::debug!(
                        "soak: cycle due but a reload is already in flight; skipping this tick"
                    );
                }
            }
            runtime.next_cycle = (runtime.next_cycle + cycle).max(elapsed);
        }
    }

    if elapsed >= config.duration && !runtime.exit_requested {
        runtime.exit_requested = true;
        tracing::info!(
            elapsed_secs = elapsed.as_secs_f64(),
            "soak: duration elapsed, requesting AppExit"
        );
        exit.write(AppExit::Success);
    }
}

/// Hold the sketch in `SketchActivity::Active` by marking the interaction timer
/// every frame, so an unattended soak exercises the *active* render path rather
/// than drifting into the screensaver after 60 s.
///
/// Registered only when [`SoakConfig::activity`] is [`SoakActivity::Active`];
/// `Natural` runs leave the idle path completely alone.
pub fn hold_sketch_active(
    config: Option<Res<'_, SoakConfig>>,
    time: Res<'_, Time>,
    timer: Option<ResMut<'_, InteractionTimer>>,
) {
    let (Some(config), Some(mut timer)) = (config, timer) else {
        return;
    };
    if config.activity == SoakActivity::Active {
        timer.mark(time.elapsed());
    }
}

/// Gather the current readings into a [`HealthSnapshot`]. Pure over its inputs.
#[allow(
    clippy::too_many_arguments,
    reason = "the snapshot's fields are its arguments; grouping them into a struct \
              would only rename this list"
)]
fn snapshot(
    elapsed: Duration,
    diagnostics: Option<&DiagnosticsStore>,
    thermal: Option<&ThermalState>,
    app_state: Option<&State<AppState>>,
    activity: Option<&State<SketchActivity>>,
    published: u64,
    cycles: u64,
    max_frame_time_ms: f32,
) -> HealthSnapshot {
    let smoothed = |path: &bevy::diagnostic::DiagnosticPath| -> Option<f64> {
        diagnostics?.get(path).and_then(Diagnostic::smoothed)
    };
    HealthSnapshot {
        uptime_secs: elapsed.as_secs_f64(),
        fps: smoothed(&FrameTimeDiagnosticsPlugin::FPS),
        frame_time_ms: smoothed(&FrameTimeDiagnosticsPlugin::FRAME_TIME),
        max_frame_time_ms,
        state: app_state.map_or_else(|| "Unknown".to_string(), |s| format!("{:?}", s.get())),
        activity: activity.map(|a| format!("{:?}", a.get())),
        thermal_tier: thermal.map_or_else(
            || "unknown".to_string(),
            |t| format!("{:?}", t.tier).to_lowercase(),
        ),
        thermal_temp_c: thermal.and_then(|t| t.last_temp_c),
        published,
        cycles,
    }
}

/// Format `snapshot` into `runtime.scratch` and rewrite `<dir>/health.json`.
///
/// Hand-rolled JSON (wc-core has no `serde_json`), matching `capture`'s
/// `run.json`. Written to a sibling `.tmp` and renamed so a reader can never
/// observe a partial file. Errors are logged, never fatal: a failed snapshot
/// costs the launcher one sample, and dying mid-soak would be strictly worse.
fn publish(config: &SoakConfig, runtime: &mut SoakRuntime, snapshot: &HealthSnapshot) {
    runtime.scratch.clear();
    write_health_json(&mut runtime.scratch, snapshot);

    if let Err(err) = std::fs::create_dir_all(&config.dir) {
        tracing::error!(?err, dir = ?config.dir, "soak: cannot create output dir");
        return;
    }
    let final_path = config.dir.join("health.json");
    let tmp_path = config.dir.join("health.json.tmp");
    let write = std::fs::File::create(&tmp_path)
        .and_then(|mut f| f.write_all(runtime.scratch.as_bytes()))
        .and_then(|()| std::fs::rename(&tmp_path, &final_path));
    if let Err(err) = write {
        tracing::error!(?err, path = ?final_path, "soak: failed to write health.json");
    }
}

/// Serialize a [`HealthSnapshot`] as one JSON object into `out` (which the
/// caller has cleared). Pure — the IO wrapper is [`publish`].
fn write_health_json(out: &mut String, s: &HealthSnapshot) {
    // `writeln!` into a `String` is infallible; the discard documents that.
    let _ = writeln!(
        out,
        "{{\"uptime_secs\":{:.3},\"fps\":{},\"frame_time_ms\":{},\"max_frame_time_ms\":{},\
         \"state\":\"{}\",\"activity\":{},\
         \"thermal_tier\":\"{}\",\"thermal_temp_c\":{},\"published\":{},\"cycles\":{}}}",
        s.uptime_secs,
        opt_f64(s.fps),
        opt_f64(s.frame_time_ms),
        // Through `opt_f64` for its non-finite guard: a NaN here would emit a
        // snapshot the launcher could not parse, blinding every lane at once.
        opt_f64(Some(f64::from(s.max_frame_time_ms))),
        s.state,
        s.activity
            .as_deref()
            .map_or_else(|| "null".to_string(), |a| format!("\"{a}\"")),
        s.thermal_tier,
        s.thermal_temp_c
            .map_or_else(|| "null".to_string(), |t| format!("{t:.2}")),
        s.published,
        s.cycles,
    );
}

/// Render an optional float as a JSON number or `null`. Non-finite readings
/// (which JSON cannot represent) also degrade to `null`.
fn opt_f64(v: Option<f64>) -> String {
    match v {
        Some(v) if v.is_finite() => format!("{v:.3}"),
        _ => "null".to_string(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "unwrap is appropriate in test code (all values are constructed locally)"
)]
mod tests {
    use super::*;

    fn snap() -> HealthSnapshot {
        HealthSnapshot {
            uptime_secs: 12.5,
            fps: Some(59.94),
            frame_time_ms: Some(16.68),
            max_frame_time_ms: 33.4,
            state: "Line".to_string(),
            activity: Some("Active".to_string()),
            thermal_tier: "cool".to_string(),
            thermal_temp_c: Some(45.5),
            published: 3,
            cycles: 1,
        }
    }

    #[test]
    fn health_json_carries_every_field() {
        let mut out = String::new();
        write_health_json(&mut out, &snap());
        assert!(out.contains("\"uptime_secs\":12.500"), "{out}");
        assert!(out.contains("\"fps\":59.940"), "{out}");
        assert!(out.contains("\"frame_time_ms\":16.680"), "{out}");
        assert!(out.contains("\"max_frame_time_ms\":33.400"), "{out}");
        assert!(out.contains("\"state\":\"Line\""), "{out}");
        assert!(out.contains("\"activity\":\"Active\""), "{out}");
        assert!(out.contains("\"thermal_tier\":\"cool\""), "{out}");
        assert!(out.contains("\"thermal_temp_c\":45.50"), "{out}");
        assert!(out.contains("\"published\":3"), "{out}");
        assert!(out.contains("\"cycles\":1"), "{out}");
    }

    #[test]
    fn absent_readings_serialize_as_json_null() {
        let s = HealthSnapshot {
            fps: None,
            frame_time_ms: None,
            activity: None,
            thermal_temp_c: None,
            ..snap()
        };
        let mut out = String::new();
        write_health_json(&mut out, &s);
        assert!(out.contains("\"fps\":null"), "{out}");
        assert!(out.contains("\"frame_time_ms\":null"), "{out}");
        assert!(out.contains("\"activity\":null"), "{out}");
        assert!(out.contains("\"thermal_temp_c\":null"), "{out}");
    }

    /// A NaN/inf FPS reading is not representable in JSON — it must degrade to
    /// `null` rather than emit an unparseable snapshot the launcher would then
    /// (correctly) reject for the rest of the run.
    #[test]
    fn non_finite_readings_serialize_as_json_null() {
        let s = HealthSnapshot {
            fps: Some(f64::NAN),
            frame_time_ms: Some(f64::INFINITY),
            ..snap()
        };
        let mut out = String::new();
        write_health_json(&mut out, &s);
        assert!(out.contains("\"fps\":null"), "{out}");
        assert!(out.contains("\"frame_time_ms\":null"), "{out}");
    }

    #[test]
    fn snapshot_degrades_gracefully_without_any_resources() {
        let s = snapshot(Duration::from_secs(2), None, None, None, None, 1, 0, 40.0);
        assert_eq!(s.state, "Unknown");
        assert_eq!(s.activity, None);
        assert_eq!(s.thermal_tier, "unknown");
        assert_eq!(s.fps, None);
        assert!((s.uptime_secs - 2.0).abs() < f64::EPSILON);
        // The hitch lane does not depend on the diagnostics store, so it still
        // reports even when every other reading is missing.
        assert!((s.max_frame_time_ms - 40.0).abs() < f32::EPSILON);
    }

    /// The scratch buffer is reused across publishes: a second format must not
    /// append to the first (which would produce two concatenated objects).
    #[test]
    fn publish_reuses_the_scratch_buffer_without_appending() {
        let dir = std::env::temp_dir().join("wc_soak_publish_test");
        let config = SoakConfig {
            dir: dir.clone(),
            duration: Duration::from_mins(1),
            health: Duration::from_secs(1),
            cycle: None,
            activity: SoakActivity::Active,
        };
        let mut runtime = SoakRuntime::default();
        publish(&config, &mut runtime, &snap());
        publish(&config, &mut runtime, &snap());
        let written = std::fs::read_to_string(dir.join("health.json")).unwrap();
        assert_eq!(
            written.matches("uptime_secs").count(),
            1,
            "health.json must hold exactly one snapshot object, got: {written}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole soak scaffold must be inert without `WC_SOAK`: `drive_soak`
    /// with no `SoakConfig` may not touch state or request an exit.
    #[test]
    fn drive_soak_is_inert_without_config() {
        let mut app = App::new();
        app.add_plugins(bevy::time::TimePlugin);
        app.add_message::<AppExit>();
        app.init_resource::<NextState<AppState>>();
        app.add_systems(Update, drive_soak);
        app.update();
        let exits = app
            .world_mut()
            .resource_mut::<Messages<AppExit>>()
            .drain()
            .count();
        assert_eq!(exits, 0, "no AppExit without WC_SOAK");
    }

    /// Regression: the first sketch advance must be due one whole `cycle` in,
    /// not at t=0. A zero-armed cycle advances the sketch on the first frame,
    /// throwing away the `--sketch` the run was asked to start on (which is
    /// exactly what the first end-to-end smoke run did: it reported `Flame` at
    /// the first sample of a `--sketch line` run).
    #[test]
    fn the_first_cycle_is_due_one_interval_in_not_immediately() {
        let config = SoakConfig {
            dir: std::env::temp_dir().join("wc_soak_cycle_test"),
            duration: Duration::from_hours(8),
            health: Duration::from_secs(1),
            cycle: Some(Duration::from_mins(5)),
            activity: SoakActivity::Active,
        };
        let runtime = SoakRuntime::new(&config);
        assert_eq!(
            runtime.next_cycle,
            Duration::from_mins(5),
            "the run must spend its first interval on the sketch it was asked to start on"
        );
        assert_eq!(
            runtime.next_health,
            Duration::ZERO,
            "the first health snapshot, by contrast, is due immediately"
        );
    }

    /// Once the configured duration elapses, exactly one `AppExit` is written —
    /// the app self-terminates rather than relying on the launcher's kill.
    #[test]
    fn drive_soak_requests_exit_once_past_duration() {
        let mut app = App::new();
        app.add_plugins(bevy::time::TimePlugin);
        app.add_message::<AppExit>();
        app.init_resource::<NextState<AppState>>();
        app.insert_resource(SoakConfig {
            dir: std::env::temp_dir().join("wc_soak_exit_test"),
            // Zero-length in practice: any elapsed real time is already past it.
            duration: Duration::from_nanos(1),
            health: Duration::from_hours(1), // never publishes during the test
            cycle: None,
            activity: SoakActivity::Active,
        });
        app.init_resource::<SoakRuntime>();
        // `next_health` starts at zero, which would publish on the first tick;
        // push it out so this test exercises only the exit path.
        app.world_mut().resource_mut::<SoakRuntime>().next_health = Duration::from_hours(1);
        app.add_systems(Update, drive_soak);

        app.update();
        app.update();
        let exits = app
            .world_mut()
            .resource_mut::<Messages<AppExit>>()
            .drain()
            .count();
        assert_eq!(exits, 1, "AppExit is requested exactly once");
    }

    /// Build a soak app whose health interval is `health` and whose output goes
    /// to `dir`. Used by the two watermark tests below.
    fn watermark_app(dir: std::path::PathBuf, health: Duration) -> App {
        let mut app = App::new();
        app.add_plugins(bevy::time::TimePlugin);
        app.add_message::<AppExit>();
        app.init_resource::<NextState<AppState>>();
        let config = SoakConfig {
            dir,
            duration: Duration::from_hours(8),
            health,
            cycle: None,
            activity: SoakActivity::Active,
        };
        app.insert_resource(SoakRuntime::new(&config));
        app.insert_resource(config);
        app.add_systems(Update, drive_soak);
        app
    }

    /// The hitch lane: the worst frame between two publishes must survive to the
    /// publish, because *that* is the only signal that sees a wedge shorter than
    /// the launcher's sample interval. A smoothed average would erase it.
    #[test]
    fn the_watermark_holds_the_worst_frame_between_publishes() {
        // Health interval far longer than the test: exactly one publish (the one
        // armed at t=0), then the watermark accumulates undisturbed.
        let mut app = watermark_app(
            std::env::temp_dir().join("wc_soak_watermark_hold"),
            Duration::from_hours(1),
        );
        app.update();
        std::thread::sleep(Duration::from_millis(25));
        app.update();
        std::thread::sleep(Duration::from_millis(5));
        app.update();

        let mark = app.world().resource::<SoakRuntime>().max_frame_time_ms;
        assert!(
            mark >= 20.0,
            "the 25 ms frame must still be the high-water mark, not averaged away with the 5 ms \
             one; got {mark} ms"
        );
    }

    /// ...and it is reset by the publish, so each snapshot describes the interval
    /// it closes. A watermark that were never reset would report one early hitch
    /// for the rest of the run and could never say when a later one happened.
    #[test]
    fn publishing_resets_the_watermark() {
        let dir = std::env::temp_dir().join("wc_soak_watermark_reset");
        // Zero health interval: every frame publishes.
        let mut app = watermark_app(dir.clone(), Duration::ZERO);
        app.update();
        std::thread::sleep(Duration::from_millis(25));
        app.update();

        let mark = app.world().resource::<SoakRuntime>().max_frame_time_ms;
        assert!(
            mark.abs() < f32::EPSILON,
            "the publish must zero the watermark; got {mark} ms"
        );
        let written = std::fs::read_to_string(dir.join("health.json")).unwrap();
        assert!(
            written.contains("\"max_frame_time_ms\":"),
            "the snapshot carries the hitch lane: {written}"
        );
        assert!(
            !written.contains("\"max_frame_time_ms\":null"),
            "a real frame was measured, so it must not be null: {written}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
