//! Optional frame-rate cap, applied by sleeping the main loop between frames.
//!
//! ## Why
//!
//! Diagnosed in `docs/runbooks/dots-explode-gpu-saturation.md`: a sketch can
//! saturate the GPU at maximum clock (no headroom), which makes frame times
//! spike under any variance. Capping the frame rate gives the GPU idle time
//! each frame so it drops to a lower clock — the spikes flatten and power/heat
//! drop (better for the multi-hour soak target). For slow ambient visuals a
//! lower rate (30-40 fps) reads as smooth.
//!
//! ## How
//!
//! The `frame_limiter` system runs in [`Last`] (after the frame's work is
//! queued) and, when [`FrameLimiterSettings::target_fps`] is positive, sleeps the main
//! thread until the next frame's deadline. Pacing is drift-free (the next
//! deadline is the previous deadline plus the frame duration, not "now plus one
//! frame"), so sleep overshoot self-corrects rather than accumulating. It is
//! sleep-only, with no spin loop: a busy-wait would burn power and defeat the
//! soak benefit, and with vsync on the present already supplies the precise
//! cadence (a cap to a refresh divisor, e.g. 30 of 60, aligns cleanly).
//!
//! Native only: the web build is paced by the browser's `requestAnimationFrame`
//! loop and `std::thread::sleep` cannot block it, so the system is registered
//! behind `cfg(not(wasm))` and the cap is a no-op there.
//!
//! ## Configuring
//!
//! `target_fps` is a `category = User` setting; the default is `60.0` (a steady
//! 60 fps with GPU headroom — even on a 60 Hz display the sleep hands the GPU
//! idle time that pure vsync does not, which de-saturates it). `0` disables the
//! cap. The [`FPS_CAP_ENV`] env var overrides it at launch (consistent with
//! `WAVECONDUCTOR_START_SKETCH`) for kiosk launch scripts and the capture
//! harness; the panel value takes over once changed.

use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

use crate::settings::RegisterSketchSettingsExt;

/// Env var that pins the frame-rate cap at launch, overriding the saved/default
/// [`FrameLimiterSettings::target_fps`]. Parsed as `f32`; `0`, unset, or
/// unparseable = no cap.
pub const FPS_CAP_ENV: &str = "WAVECONDUCTOR_FPS_CAP";

/// Global frame-rate cap, persisted across sessions.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "frame_limiter")]
pub struct FrameLimiterSettings {
    /// Target frames per second. The default `60` holds the main loop to at most
    /// 60 fps by sleeping, which gives the GPU idle time each frame (lower clock,
    /// fewer frame-time spikes, lower soak power) even on a 60 Hz display where
    /// vsync alone would keep it busier. `0` disables the cap (run at the
    /// vsync/native rate). 30-40 trades smoothness of motion for even more
    /// headroom and lower power.
    #[setting(
        default = 60.0_f32,
        min = 0.0,
        max = 120.0,
        step = 5.0,
        category = User,
        section = "Display",
        label = "Frame rate cap (0 = off)",
        unit = "fps"
    )]
    #[serde(default = "default_target_fps")]
    pub target_fps: f32,
}

/// Serde fallback so a config saved before this field existed loads at the
/// default 60 fps cap.
fn default_target_fps() -> f32 {
    60.0
}

/// Plugin: registers [`FrameLimiterSettings`], applies the launch env override,
/// and (native only) adds the `frame_limiter` pacing system to [`Last`].
pub struct FrameLimiterPlugin;

impl Plugin for FrameLimiterPlugin {
    fn build(&self, app: &mut App) {
        app.register_sketch_settings::<FrameLimiterSettings>();
        app.add_systems(Startup, apply_fps_cap_env_override);
        // Native only: `std::thread::sleep` cannot pace the browser rAF loop.
        #[cfg(not(target_arch = "wasm32"))]
        app.add_systems(Last, frame_limiter);
    }
}

/// Startup: if [`FPS_CAP_ENV`] is a parseable `f32`, override the loaded
/// [`FrameLimiterSettings::target_fps`] for this launch (the panel takes over
/// once the user changes it). Mirrors the `WAVECONDUCTOR_START_SKETCH` launch
/// override.
fn apply_fps_cap_env_override(mut settings: ResMut<'_, FrameLimiterSettings>) {
    let Ok(raw) = std::env::var(FPS_CAP_ENV) else {
        return;
    };
    match raw.trim().parse::<f32>() {
        Ok(fps) => {
            tracing::info!(fps, "{}: frame-rate cap set at launch", FPS_CAP_ENV);
            settings.target_fps = fps;
        }
        Err(_) => {
            tracing::warn!(value = %raw, "{}: not a number; ignoring", FPS_CAP_ENV);
        }
    }
}

/// The target frame duration for a given (positive, finite) fps.
fn frame_duration(fps: f32) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(fps))
}

/// How long to sleep to hold the frame to `frame`, given the time elapsed since
/// the previous deadline. `None` means already at/over budget — run free.
fn sleep_for(frame: Duration, since_prev: Duration) -> Option<Duration> {
    frame.checked_sub(since_prev)
}

/// `Last`-schedule system that paces the main loop to
/// [`FrameLimiterSettings::target_fps`] by sleeping. No-op when the target is
/// not a positive, finite number. Native only. See the module docs for the
/// drift-free, sleep-only rationale.
#[cfg(not(target_arch = "wasm32"))]
fn frame_limiter(
    settings: Res<'_, FrameLimiterSettings>,
    mut deadline: Local<'_, Option<Instant>>,
) {
    let fps = settings.target_fps;
    // Uncapped for 0, negative, NaN, or infinity (the NaN guard also keeps
    // `frame_duration` from a `from_secs_f64(NaN)` panic). Reset so a later
    // re-enable starts a fresh cadence rather than snapping to a stale deadline.
    if !fps.is_finite() || fps <= 0.0 {
        *deadline = None;
        return;
    }
    let frame = frame_duration(fps);
    let now = Instant::now();
    let Some(prev) = *deadline else {
        // First capped frame: establish the cadence baseline, don't sleep.
        *deadline = Some(now);
        return;
    };
    match sleep_for(frame, now.saturating_duration_since(prev)) {
        Some(remaining) => {
            std::thread::sleep(remaining);
            // Pace from the intended deadline (drift-free): sleep overshoot
            // shortens the next interval instead of accumulating.
            *deadline = Some(prev + frame);
        }
        None => {
            // Behind schedule (app slower than the target, or just resumed):
            // run free and re-baseline so we don't burst to "catch up".
            *deadline = Some(now);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact literal defaults / round-tripped values are bit-exact"
)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;

    #[test]
    fn target_fps_defaults_to_60() {
        assert_eq!(FrameLimiterSettings::default().target_fps, 60.0);
    }

    #[test]
    fn frame_duration_matches_fps() {
        assert!((frame_duration(30.0).as_secs_f64() - 1.0 / 30.0).abs() < 1e-9);
        assert!((frame_duration(60.0).as_secs_f64() - 1.0 / 60.0).abs() < 1e-9);
    }

    #[test]
    fn sleep_for_when_ahead_returns_remaining() {
        let frame = Duration::from_millis(33);
        assert_eq!(
            sleep_for(frame, Duration::from_millis(20)),
            Some(Duration::from_millis(13))
        );
    }

    #[test]
    fn sleep_for_when_behind_runs_free() {
        let frame = Duration::from_millis(33);
        assert_eq!(sleep_for(frame, Duration::from_millis(40)), None);
    }

    #[test]
    fn settings_round_trip_through_toml() {
        let s = FrameLimiterSettings { target_fps: 30.0 };
        let text = toml::to_string(&s).expect("serialize");
        let back: FrameLimiterSettings = toml::from_str(&text).expect("parse");
        assert_eq!(back, s);
    }

    #[test]
    fn pre_field_settings_file_loads_with_60_default() {
        // A config saved before this field existed must load at the 60 fps cap.
        let parsed: FrameLimiterSettings = toml::from_str("").expect("empty settings loads");
        assert_eq!(parsed.target_fps, 60.0);
    }
}
