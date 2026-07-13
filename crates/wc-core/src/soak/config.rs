//! `WC_SOAK` env parsing and the parsed [`SoakConfig`] resource.
//!
//! Mirrors [`crate::capture::config`]: the soak systems read only this
//! pre-parsed resource and never touch `std::env` per frame (project rule:
//! parse env once at startup).
//!
//! Format (`;`-separated `key=value`, same grammar as `WC_CAPTURE`):
//! `dir=<path>;duration=<secs>[;health=<secs>][;cycle=<secs>][;activity=active|natural]`
//! - `dir`: output directory for the `health.json` snapshot the launcher polls.
//! - `duration`: total soak length in seconds; the app requests `AppExit` once
//!   its own wall clock passes it (the launcher keeps an independent timeout as
//!   a safety net for the case where the app is wedged and never gets there).
//! - `health`: how often the app republishes `health.json` (default 1 s). This
//!   is deliberately much finer than the launcher's own sample interval so a
//!   *stale* snapshot is unambiguous evidence of a frozen app rather than a
//!   scheduling race between two similar-rate timers.
//! - `cycle`: seconds between automatic sketch advances (`0` / absent = never).
//!   Cycling is what exercises the sketch enter/exit lifecycle over hours —
//!   historically where this project's GPU-resource leaks lived.
//! - `activity`: `active` (default) keeps the interaction timer marked so the
//!   sketch stays `SketchActivity::Active` under representative load; `natural`
//!   lets the idle timer run through `Idle` -> `Screensaver` the way an
//!   untouched kiosk would.

use std::path::PathBuf;
use std::time::Duration;

use bevy::prelude::Resource;

/// How the soak run treats the idle timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SoakActivity {
    /// Mark the interaction timer every frame so the sketch stays `Active` —
    /// the "hand tracking + audio active" representative load the release gate
    /// calls for.
    #[default]
    Active,
    /// Leave the idle timer alone: the sketch will drift `Active` -> `Idle` ->
    /// `Screensaver` exactly as an untouched kiosk does.
    Natural,
}

/// Parsed `WC_SOAK` schedule. Inserted once at startup; read each frame by the
/// soak systems. Absent when `WC_SOAK` is unset (i.e. every normal run).
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct SoakConfig {
    /// Output directory; the app writes `health.json` here.
    pub dir: PathBuf,
    /// Total soak duration; the app self-exits once it elapses.
    pub duration: Duration,
    /// Interval between `health.json` republishes.
    pub health: Duration,
    /// Interval between automatic sketch advances. `None` = no cycling.
    pub cycle: Option<Duration>,
    /// Whether to hold the sketch `Active` or let the idle path run.
    pub activity: SoakActivity,
}

/// Default `health.json` republish interval.
const DEFAULT_HEALTH: Duration = Duration::from_secs(1);

/// Parse a `WC_SOAK` value into a [`SoakConfig`].
///
/// # Errors
///
/// Returns a human-readable `String` when `dir` or `duration` is missing, when
/// a numeric field fails to parse, when `duration` / `health` is zero, or when
/// an unknown key or `activity` value appears.
pub fn parse_wc_soak(raw: &str) -> Result<SoakConfig, String> {
    let mut dir: Option<PathBuf> = None;
    let mut duration: Option<Duration> = None;
    let mut health = DEFAULT_HEALTH;
    let mut cycle: Option<Duration> = None;
    let mut activity = SoakActivity::Active;

    for pair in raw.split(';').filter(|s| !s.trim().is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("WC_SOAK: malformed pair (no '='): {pair:?}"))?;
        let key = key.trim();
        let value = value.trim();
        match key {
            "dir" => dir = Some(PathBuf::from(value)),
            "duration" => duration = Some(parse_secs(key, value)?),
            "health" => health = parse_secs(key, value)?,
            "cycle" => {
                let d = parse_secs(key, value)?;
                // `cycle=0` is the explicit "never cycle" spelling, so the
                // launcher can always emit the key.
                cycle = (!d.is_zero()).then_some(d);
            }
            "activity" => {
                activity = match value.to_ascii_lowercase().as_str() {
                    "active" => SoakActivity::Active,
                    "natural" => SoakActivity::Natural,
                    other => {
                        return Err(format!(
                            "WC_SOAK: bad activity {other:?} (expected 'active' or 'natural')"
                        ))
                    }
                };
            }
            other => return Err(format!("WC_SOAK: unknown key {other:?}")),
        }
    }

    let dir = dir.ok_or_else(|| "WC_SOAK: missing required key 'dir'".to_string())?;
    let duration =
        duration.ok_or_else(|| "WC_SOAK: missing required key 'duration'".to_string())?;
    if duration.is_zero() {
        return Err("WC_SOAK: 'duration' must be greater than zero".to_string());
    }
    if health.is_zero() {
        return Err("WC_SOAK: 'health' must be greater than zero".to_string());
    }

    Ok(SoakConfig {
        dir,
        duration,
        health,
        cycle,
        activity,
    })
}

/// Parse a non-negative, finite seconds value into a [`Duration`].
fn parse_secs(key: &str, value: &str) -> Result<Duration, String> {
    let secs = value
        .parse::<f64>()
        .map_err(|e| format!("WC_SOAK: bad {key} {value:?}: {e}"))?;
    if !secs.is_finite() || secs < 0.0 {
        return Err(format!(
            "WC_SOAK: {key} must be a finite, non-negative number of seconds (got {value:?})"
        ));
    }
    Ok(Duration::from_secs_f64(secs))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is appropriate in test code")]
mod tests {
    use super::*;

    #[test]
    fn parses_required_fields_with_defaults() {
        let cfg = parse_wc_soak("dir=target/soak/run;duration=1800").unwrap();
        assert_eq!(cfg.dir, PathBuf::from("target/soak/run"));
        assert_eq!(cfg.duration, Duration::from_mins(30));
        assert_eq!(cfg.health, DEFAULT_HEALTH);
        assert_eq!(cfg.cycle, None);
        assert_eq!(cfg.activity, SoakActivity::Active);
    }

    #[test]
    fn parses_optional_health_cycle_and_activity() {
        let cfg =
            parse_wc_soak("dir=out;duration=60;health=0.5;cycle=300;activity=natural").unwrap();
        assert_eq!(cfg.health, Duration::from_millis(500));
        assert_eq!(cfg.cycle, Some(Duration::from_mins(5)));
        assert_eq!(cfg.activity, SoakActivity::Natural);
    }

    #[test]
    fn zero_cycle_means_no_cycling() {
        let cfg = parse_wc_soak("dir=out;duration=60;cycle=0").unwrap();
        assert_eq!(cfg.cycle, None);
    }

    #[test]
    fn missing_dir_is_error() {
        assert!(parse_wc_soak("duration=60").is_err());
    }

    #[test]
    fn missing_duration_is_error() {
        assert!(parse_wc_soak("dir=out").is_err());
    }

    #[test]
    fn zero_duration_is_error() {
        assert!(parse_wc_soak("dir=out;duration=0").is_err());
    }

    #[test]
    fn zero_health_is_error() {
        assert!(parse_wc_soak("dir=out;duration=60;health=0").is_err());
    }

    #[test]
    fn negative_duration_is_error() {
        assert!(parse_wc_soak("dir=out;duration=-5").is_err());
    }

    #[test]
    fn unknown_key_is_error() {
        assert!(parse_wc_soak("dir=out;duration=60;bogus=1").is_err());
    }

    #[test]
    fn unknown_activity_is_error() {
        assert!(parse_wc_soak("dir=out;duration=60;activity=sideways").is_err());
    }

    #[test]
    fn malformed_pair_without_equals_is_error() {
        assert!(parse_wc_soak("dir=out;duration").is_err());
    }

    #[test]
    fn whitespace_and_trailing_semicolon_are_tolerated() {
        let cfg = parse_wc_soak(" dir = out ; duration = 60 ; ").unwrap();
        assert_eq!(cfg.dir, PathBuf::from("out"));
        assert_eq!(cfg.duration, Duration::from_mins(1));
    }
}
