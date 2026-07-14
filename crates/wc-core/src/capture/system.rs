//! Deterministic in-app frame-capture system.
//!
//! ## Determinism contract
//!
//! While capturing, the virtual clock is pinned to a fixed `dt`
//! ([`bevy::time::TimeUpdateStrategy::ManualDuration`]) so update *N* maps to
//! sim time *N·dt*. Frame counting starts only once the active sketch is
//! entered AND its required assets are loaded, then waits `settle` frames, so
//! `frame 0` is the first fully-loaded, settled sketch frame. This designs out
//! the wall-clock sampling bug that produced a false "dimmer with a hand"
//! reading: two runs now sample identical points on the gravity-smear triangle
//! wave.
//!
//! ## Release safety
//!
//! This module is `#[cfg(debug_assertions)]`-gated by its parent
//! ([`crate::capture`]). It exists ONLY in debug builds. Capture relies on
//! `debug-assertions = false` in the release/soak profiles — never enable debug
//! assertions there or this system (and its per-frame work) compiles back in.
//!
//! ## Activation
//!
//! Every system here early-returns when [`CaptureConfig`] is absent (i.e.
//! `WC_CAPTURE` was unset), so a normal debug run pays only an `Option<Res<_>>`
//! miss per frame.

use std::io::Write as _;

use bevy::app::AppExit;
use bevy::ecs::message::MessageWriter;
use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use bevy::time::TimeUpdateStrategy;

use super::config::CaptureConfig;
use crate::debug::DebugToggles;
use crate::lifecycle::state::AppState;

/// Frames to keep rendering after the last screenshot is dispatched, before
/// requesting `AppExit`. A [`Screenshot`] is a deferred command whose
/// `save_to_disk` observer flushes the PNG only after the frame it captures has
/// been fully rendered and read back from the GPU. Requesting `AppExit` in the
/// same tick tears the window down before that read-back completes, so the last
/// scheduled frame is silently dropped (observed as `WARN Unknown window for
/// screenshot`). This grace window lets the final screenshot reach disk.
const EXIT_GRACE_FRAMES: u32 = 6;

/// Capture progress + readiness gate. Inserted once; mutated each frame.
#[derive(Resource, Debug, Default)]
pub struct CaptureState {
    /// True once the active sketch is entered and its required assets are
    /// loaded. Set by [`detect_assets_ready`]; gates all frame counting.
    pub assets_ready: bool,
    /// Settle frames already consumed after `assets_ready` flipped true.
    pub settled: u32,
    /// Current sim-frame index (0 = first fully-loaded, settled frame).
    pub sim_frame: u32,
    /// Number of frames captured so far (drives the exit condition).
    pub captured: usize,
    /// `Some(remaining)` once the last scheduled frame has been dispatched: a
    /// per-tick countdown of grace frames before `AppExit`. `None` while frames
    /// are still being captured. See `EXIT_GRACE_FRAMES`.
    pub exit_grace: Option<u32>,
    /// True after `AppExit` has been requested, so we request it only once.
    pub exit_requested: bool,
}

impl CaptureState {
    /// Advance the gate one tick and return the sim-frame index that is "live"
    /// this tick, or `None` while not ready or still settling.
    ///
    /// `settle` is the configured number of settle frames. The first
    /// `settle` armed ticks are consumed silently; the next armed tick is
    /// sim-frame 0, and each subsequent armed tick increments by one.
    pub fn advance_and_current_frame(&mut self, settle: u32) -> Option<u32> {
        if !self.assets_ready {
            return None;
        }
        if self.settled < settle {
            self.settled += 1;
            return None;
        }
        let current = self.sim_frame;
        self.sim_frame += 1;
        Some(current)
    }
}

/// Pin the virtual clock to the configured fixed `dt` for the duration of the
/// capture run, so sim time is `update_index · dt`. Idempotent (re-inserts the
/// same value each frame). No-op without a [`CaptureConfig`].
pub fn pin_capture_timestep(
    config: Option<Res<'_, CaptureConfig>>,
    state: Res<'_, CaptureState>,
    mut commands: Commands<'_, '_>,
) {
    let Some(config) = config else {
        return;
    };
    // Only pin once assets are ready: before that we want the normal clock so
    // asset loading / the OnEnter transition proceed at real pace.
    if state.assets_ready {
        commands.insert_resource(TimeUpdateStrategy::ManualDuration(config.dt));
    }
}

/// Flip [`CaptureState::assets_ready`] to true on the first `Update` where the
/// app has entered a sketch (left `Home`). The sketch's own `OnEnter` has run
/// by then, queuing its asset loads; combined with the `settle` window this is
/// the robust "fully-loaded sketch frame" signal called for by the spec
/// (sketches enter `SketchActivity::Active` only after `OnEnter` completes).
///
/// No-op without a [`CaptureConfig`].
pub fn detect_assets_ready(
    config: Option<Res<'_, CaptureConfig>>,
    state: Option<ResMut<'_, CaptureState>>,
    app_state: Option<Res<'_, State<AppState>>>,
) {
    let (Some(_config), Some(mut state), Some(app_state)) = (config, state, app_state) else {
        return;
    };
    if !state.assets_ready && app_state.get().is_sketch() {
        state.assets_ready = true;
    }
}

/// Per-frame: when the live sim-frame index is in the configured schedule,
/// spawn a [`Screenshot`] of the primary window with a `save_to_disk` observer
/// targeting `<dir>/frame_NNNN.png`. After the last scheduled frame is
/// dispatched, write `run.json` and request [`AppExit`].
///
/// No-op without a [`CaptureConfig`].
pub fn drive_capture(
    config: Option<Res<'_, CaptureConfig>>,
    mut state: Option<ResMut<'_, CaptureState>>,
    toggles: Option<Res<'_, DebugToggles>>,
    mut commands: Commands<'_, '_>,
    mut exit: MessageWriter<'_, AppExit>,
) {
    let (Some(config), Some(state)) = (config.as_ref(), state.as_mut()) else {
        return;
    };

    // Once the last scheduled frame has been dispatched, stop advancing the
    // capture counter and instead spend the grace window keeping the app
    // rendering so the final screenshot's `save_to_disk` observer can flush to
    // disk before we tear the window down with `AppExit`.
    if let Some(remaining) = state.exit_grace {
        if remaining == 0 {
            if !state.exit_requested {
                state.exit_requested = true;
                tracing::info!("capture: grace window elapsed, requesting AppExit");
                exit.write(AppExit::Success);
            }
        } else {
            state.exit_grace = Some(remaining - 1);
        }
        return;
    }

    let Some(current) = state.advance_and_current_frame(config.settle) else {
        return;
    };

    if config.frames.contains(&current) {
        let path = config.dir.join(format!("frame_{current:04}.png"));
        if let Err(err) = std::fs::create_dir_all(&config.dir) {
            tracing::error!(?err, dir = ?config.dir, "capture: cannot create output dir");
        }
        tracing::info!(frame = current, path = ?path, "capture: requesting screenshot");
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(path));
        state.captured += 1;
    }

    // After the last scheduled frame is dispatched, write `run.json` and arm the
    // grace countdown rather than exiting immediately: the screenshot is a
    // deferred read-back that needs `EXIT_GRACE_FRAMES` more rendered frames to
    // reach disk. The xtask enforces a wall-clock timeout as a safety net in
    // case a screenshot observer never fires.
    let done = config.frames.last().is_some_and(|&last| current >= last);
    if done && state.exit_grace.is_none() {
        write_run_json(config, toggles.as_deref());
        state.exit_grace = Some(EXIT_GRACE_FRAMES);
        tracing::info!(
            grace = EXIT_GRACE_FRAMES,
            "capture: schedule complete, holding for screenshot flush"
        );
    }
}

/// Write the self-describing `run.json` sidecar next to the captured frames.
///
/// Hand-rolled JSON (no `serde_json` dependency in wc-core) keeps the sidecar
/// minimal; the xtask parses it with `serde_json`. Records the scenario name,
/// scheduled frames, `dt` (seconds), `settle`, the app version, the git commit
/// (the last two when the xtask supplied them via `WC_CAPTURE`), and the active
/// `WC_DEBUG_*` toggles — enough to reproduce a capture from its own sidecar.
fn write_run_json(config: &CaptureConfig, toggles: Option<&DebugToggles>) {
    let json = run_json_string(config, toggles);
    let path = config.dir.join("run.json");
    match std::fs::File::create(&path).and_then(|mut f| f.write_all(json.as_bytes())) {
        Ok(()) => tracing::info!(path = ?path, "capture: wrote run.json"),
        Err(err) => tracing::error!(?err, path = ?path, "capture: failed to write run.json"),
    }
}

/// Build the `run.json` body (pure; the IO wrapper is [`write_run_json`]).
fn run_json_string(config: &CaptureConfig, toggles: Option<&DebugToggles>) -> String {
    let frames = config
        .frames
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let scenario = config
        .scenario
        .as_deref()
        .map_or_else(|| "null".to_string(), |s| format!("\"{}\"", json_escape(s)));
    let commit = config
        .commit
        .as_deref()
        .map_or_else(|| "null".to_string(), |c| format!("\"{}\"", json_escape(c)));
    format!(
        "{{\"scenario\":{scenario},\"frames\":[{frames}],\"dt_secs\":{},\"settle\":{},\"app_version\":\"{}\",\"commit\":{commit},\"toggles\":{}}}\n",
        config.dt.as_secs_f64(),
        config.settle,
        env!("CARGO_PKG_VERSION"),
        toggles_json(toggles),
    )
}

/// Serialize the active `WC_DEBUG_*` toggles as a JSON object. Only toggles that
/// are set appear; an absent resource (or all-off) yields `{}`.
fn toggles_json(toggles: Option<&DebugToggles>) -> String {
    let Some(t) = toggles else {
        return "{}".to_string();
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(g) = t.force_g {
        parts.push(format!("\"force_g\":{g}"));
    }
    if t.disable_smear {
        parts.push("\"disable_smear\":true".to_string());
    }
    if t.disable_explode {
        parts.push("\"disable_explode\":true".to_string());
    }
    if t.disable_bloom {
        parts.push("\"disable_bloom\":true".to_string());
    }
    if t.disable_heatmap_refine {
        parts.push("\"disable_heatmap_refine\":true".to_string());
    }
    if t.disable_bone_composite {
        parts.push("\"disable_bone_composite\":true".to_string());
    }
    if t.disable_bone_camera {
        parts.push("\"disable_bone_camera\":true".to_string());
    }
    if let Some([r, g, b, a]) = t.solid_particles {
        parts.push(format!("\"solid_particles\":[{r},{g},{b},{a}]"));
    }
    if t.force_screensaver {
        parts.push("\"force_screensaver\":true".to_string());
    }
    if let Some(tier) = t.force_tier {
        // Lower-case the tier name to match the `WC_DEBUG_FORCE_TIER` input form.
        parts.push(format!(
            "\"force_tier\":\"{}\"",
            format!("{tier:?}").to_lowercase()
        ));
    }
    if t.force_cymatics_interaction {
        parts.push("\"force_cymatics_interaction\":true".to_string());
    }
    if t.force_flame_warp {
        parts.push("\"force_flame_warp\":true".to_string());
    }
    if t.force_flame_camera_pose {
        parts.push("\"force_flame_camera_pose\":true".to_string());
    }
    if t.force_radiance_synthetic_body {
        parts.push("\"force_radiance_synthetic_body\":true".to_string());
    }
    format!("{{{}}}", parts.join(","))
}

/// Minimal JSON string escaper for the few free-form values in `run.json`
/// (scenario name, commit hash). Escapes quotes, backslashes, and control chars.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::config::CaptureConfig;
    use bevy::time::TimeUpdateStrategy;
    use std::path::PathBuf;
    use std::time::Duration;

    fn cfg() -> CaptureConfig {
        CaptureConfig {
            dir: PathBuf::from("target/capture/__test"),
            frames: vec![0, 2],
            dt: Duration::from_millis(10),
            settle: 1,
            scenario: None,
            commit: None,
        }
    }

    #[test]
    fn installs_manual_duration_when_capturing() {
        let mut app = bevy::app::App::new();
        app.add_plugins(bevy::time::TimePlugin);
        app.insert_resource(cfg());
        app.init_resource::<CaptureState>();
        // Force the gate to treat assets as ready (headless has no sketch).
        app.world_mut().resource_mut::<CaptureState>().assets_ready = true;
        app.add_systems(bevy::app::Update, pin_capture_timestep);
        app.update();
        let strat = app.world().resource::<TimeUpdateStrategy>();
        assert!(
            matches!(strat, TimeUpdateStrategy::ManualDuration(d) if *d == Duration::from_millis(10))
        );
    }

    #[test]
    fn settle_then_frame_zero_advances_counter() {
        let mut state = CaptureState {
            assets_ready: true,
            ..CaptureState::default()
        };
        // settle = 1: first armed tick consumes the settle frame, next is sim-frame 0.
        assert_eq!(state.advance_and_current_frame(1), None); // settle frame
        assert_eq!(state.advance_and_current_frame(1), Some(0)); // sim-frame 0
        assert_eq!(state.advance_and_current_frame(1), Some(1)); // sim-frame 1
    }

    #[test]
    fn not_ready_does_not_advance() {
        let mut state = CaptureState::default(); // assets_ready = false
        assert_eq!(state.advance_and_current_frame(2), None);
        assert_eq!(state.sim_frame, 0);
    }

    #[test]
    fn zero_settle_yields_frame_zero_immediately() {
        let mut state = CaptureState {
            assets_ready: true,
            ..CaptureState::default()
        };
        assert_eq!(state.advance_and_current_frame(0), Some(0));
        assert_eq!(state.advance_and_current_frame(0), Some(1));
    }

    #[test]
    fn run_json_includes_scenario_commit_and_active_toggles() {
        let config = CaptureConfig {
            dir: PathBuf::from("target/capture/__test"),
            frames: vec![0, 2],
            dt: Duration::from_millis(10),
            settle: 1,
            scenario: Some("line-synthetic".to_string()),
            commit: Some("abc1234".to_string()),
        };
        let toggles = DebugToggles {
            force_g: Some(8000.0),
            disable_smear: true,
            disable_explode: false,
            disable_bloom: false,
            disable_heatmap_refine: false,
            disable_bone_composite: false,
            disable_bone_camera: false,
            solid_particles: None,
            force_screensaver: false,
            force_tier: None,
            force_cymatics_interaction: false,
            force_flame_warp: false,
            force_flame_camera_pose: true,
            force_radiance_synthetic_body: true,
        };
        let json = run_json_string(&config, Some(&toggles));
        assert!(json.contains("\"scenario\":\"line-synthetic\""), "{json}");
        assert!(json.contains("\"commit\":\"abc1234\""), "{json}");
        assert!(json.contains("\"frames\":[0,2]"), "{json}");
        assert!(json.contains("\"force_g\":8000"), "{json}");
        assert!(json.contains("\"disable_smear\":true"), "{json}");
        assert!(json.contains("\"force_flame_camera_pose\":true"), "{json}");
        assert!(
            json.contains("\"force_radiance_synthetic_body\":true"),
            "{json}"
        );
        // Off toggles must not appear.
        assert!(!json.contains("disable_bloom"), "{json}");
        assert!(!json.contains("force_flame_warp"), "{json}");
    }

    #[test]
    fn run_json_omits_absent_provenance() {
        let json = run_json_string(&cfg(), None);
        assert!(json.contains("\"scenario\":null"), "{json}");
        assert!(json.contains("\"commit\":null"), "{json}");
        assert!(json.contains("\"toggles\":{}"), "{json}");
    }

    /// Regression: the last scheduled frame must NOT exit in the same tick it is
    /// dispatched. Dispatching the screenshot and tearing the window down with
    /// `AppExit` in one tick drops the final PNG (`Unknown window for
    /// screenshot`). After the last frame, `drive_capture` arms a grace
    /// countdown and only requests `AppExit` once it elapses.
    #[test]
    fn last_frame_holds_for_grace_window_before_exit() {
        let mut app = bevy::app::App::new();
        app.add_message::<AppExit>();
        // Single scheduled frame (index 0), zero settle: the first armed tick is
        // both sim-frame 0 AND the last scheduled frame.
        app.insert_resource(CaptureConfig {
            dir: std::env::temp_dir().join("wc_capture_grace_test"),
            frames: vec![0],
            dt: Duration::from_millis(10),
            settle: 0,
            scenario: None,
            commit: None,
        });
        app.insert_resource(CaptureState {
            assets_ready: true,
            ..CaptureState::default()
        });
        app.add_systems(bevy::app::Update, drive_capture);

        // Tick 1: dispatches frame 0 (the last frame) and arms the grace window.
        // No exit yet; the screenshot still needs to flush.
        app.update();
        assert_eq!(
            app.world().resource::<CaptureState>().exit_grace,
            Some(EXIT_GRACE_FRAMES)
        );
        assert!(!app.world().resource::<CaptureState>().exit_requested);
        assert!(no_app_exit(&mut app));

        // Tick through the grace window: still no exit until the countdown hits 0.
        for _ in 0..EXIT_GRACE_FRAMES {
            app.update();
            assert!(!app.world().resource::<CaptureState>().exit_requested);
            assert!(no_app_exit(&mut app));
        }

        // The next tick (grace == 0) requests AppExit exactly once.
        app.update();
        assert!(app.world().resource::<CaptureState>().exit_requested);
        assert_eq!(drain_app_exit(&mut app), 1);

        // Subsequent ticks do not re-request exit.
        app.update();
        assert_eq!(drain_app_exit(&mut app), 0);
    }

    /// True when no `AppExit` message is currently buffered (does not drain).
    fn no_app_exit(app: &mut bevy::app::App) -> bool {
        drain_app_exit(app) == 0
    }

    /// Drain and count buffered `AppExit` messages.
    fn drain_app_exit(app: &mut bevy::app::App) -> usize {
        let mut messages = app.world_mut().resource_mut::<Messages<AppExit>>();
        let count = messages.drain().count();
        count
    }
}
