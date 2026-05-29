//! `WC_CAPTURE` env parsing and the parsed [`CaptureConfig`] resource.
//!
//! The capture system reads only this pre-parsed resource — it never touches
//! `std::env` per frame (project rule: parse env once at startup).
//!
//! Format (`;`-separated `key=value`):
//! `dir=<path>;frames=<n,n,...>[;dt=<secs>][;settle=<n>][;scenario=<name>][;commit=<hash>]`
//! - `dir`: output directory for `frame_NNNN.png` + `run.json`.
//! - `frames`: sim-frame indices to screenshot (frame 0 = first fully-loaded
//!   sketch frame, after assets-ready + settle).
//! - `dt`: fixed virtual-time delta in seconds (default `1/60`).
//! - `settle`: frames to wait after assets-ready before frame 0 (default `2`).
//! - `scenario`: optional scenario name, recorded verbatim in `run.json` for
//!   provenance (set by the xtask launcher).
//! - `commit`: optional short git commit hash, recorded in `run.json` for
//!   provenance (set by the xtask launcher).

use std::path::PathBuf;
use std::time::Duration;

use bevy::prelude::Resource;

/// Parsed `WC_CAPTURE` schedule + output target. Inserted once at startup;
/// read each frame by the capture system. Absent when `WC_CAPTURE` is unset.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct CaptureConfig {
    /// Output directory for `frame_NNNN.png` and `run.json`.
    pub dir: PathBuf,
    /// Sim-frame indices to screenshot, ascending and deduplicated.
    pub frames: Vec<u32>,
    /// Fixed virtual-time delta pinned during capture.
    pub dt: Duration,
    /// Frames to wait after assets-ready before counting frame 0.
    pub settle: u32,
    /// Optional scenario name, recorded in `run.json` for provenance. `None`
    /// when `WC_CAPTURE` carries no `scenario=` key.
    pub scenario: Option<String>,
    /// Optional short git commit hash, recorded in `run.json` for provenance.
    /// `None` when `WC_CAPTURE` carries no `commit=` key.
    pub commit: Option<String>,
}

/// Default fixed timestep: 1/60 s, expressed in whole nanoseconds so the value
/// is exact and equality-comparable in tests.
const DEFAULT_DT: Duration = Duration::from_nanos(16_666_667);

/// Default settle window: a small constant number of frames after assets-ready.
const DEFAULT_SETTLE: u32 = 2;

/// Parse a `WC_CAPTURE` value into a [`CaptureConfig`].
///
/// # Errors
///
/// Returns a human-readable `String` when `dir` or `frames` is missing, when
/// `frames` is empty, or when a numeric field fails to parse.
pub fn parse_wc_capture(raw: &str) -> Result<CaptureConfig, String> {
    let mut dir: Option<PathBuf> = None;
    let mut frames: Option<Vec<u32>> = None;
    let mut dt = DEFAULT_DT;
    let mut settle = DEFAULT_SETTLE;
    let mut scenario: Option<String> = None;
    let mut commit: Option<String> = None;

    for pair in raw.split(';').filter(|s| !s.trim().is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("WC_CAPTURE: malformed pair (no '='): {pair:?}"))?;
        let key = key.trim();
        let value = value.trim();
        match key {
            "dir" => dir = Some(PathBuf::from(value)),
            "frames" => {
                let mut parsed: Vec<u32> = value
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .map(|n| {
                        n.trim()
                            .parse::<u32>()
                            .map_err(|e| format!("WC_CAPTURE: bad frame index {n:?}: {e}"))
                    })
                    .collect::<Result<_, _>>()?;
                parsed.sort_unstable();
                parsed.dedup();
                frames = Some(parsed);
            }
            "dt" => {
                let secs = value
                    .parse::<f64>()
                    .map_err(|e| format!("WC_CAPTURE: bad dt {value:?}: {e}"))?;
                dt = Duration::from_secs_f64(secs);
            }
            "settle" => {
                settle = value
                    .parse::<u32>()
                    .map_err(|e| format!("WC_CAPTURE: bad settle {value:?}: {e}"))?;
            }
            "scenario" => scenario = Some(value.to_string()),
            "commit" => commit = Some(value.to_string()),
            other => return Err(format!("WC_CAPTURE: unknown key {other:?}")),
        }
    }

    let dir = dir.ok_or_else(|| "WC_CAPTURE: missing required key 'dir'".to_string())?;
    let frames = frames.ok_or_else(|| "WC_CAPTURE: missing required key 'frames'".to_string())?;
    if frames.is_empty() {
        return Err("WC_CAPTURE: 'frames' must list at least one index".to_string());
    }

    Ok(CaptureConfig {
        dir,
        frames,
        dt,
        settle,
        scenario,
        commit,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is appropriate in test code")]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_required_fields_with_defaults() {
        let cfg = parse_wc_capture("dir=target/capture/line;frames=30,60,120").unwrap();
        assert_eq!(cfg.dir, std::path::PathBuf::from("target/capture/line"));
        assert_eq!(cfg.frames, vec![30, 60, 120]);
        assert_eq!(cfg.dt, Duration::from_nanos(16_666_667)); // ~1/60
        assert_eq!(cfg.settle, 2);
    }

    #[test]
    fn parses_optional_dt_and_settle() {
        let cfg = parse_wc_capture("dir=out;frames=1;dt=0.05;settle=5").unwrap();
        assert_eq!(cfg.dt, Duration::from_secs_f64(0.05));
        assert_eq!(cfg.settle, 5);
    }

    #[test]
    fn frames_are_sorted_and_deduped() {
        let cfg = parse_wc_capture("dir=out;frames=120,30,60,30").unwrap();
        assert_eq!(cfg.frames, vec![30, 60, 120]);
    }

    #[test]
    fn parses_scenario_and_commit() {
        let cfg =
            parse_wc_capture("dir=out;frames=1;scenario=line-synthetic;commit=abc1234").unwrap();
        assert_eq!(cfg.scenario.as_deref(), Some("line-synthetic"));
        assert_eq!(cfg.commit.as_deref(), Some("abc1234"));
    }

    #[test]
    fn scenario_and_commit_default_to_none() {
        let cfg = parse_wc_capture("dir=out;frames=1").unwrap();
        assert_eq!(cfg.scenario, None);
        assert_eq!(cfg.commit, None);
    }

    #[test]
    fn missing_dir_is_error() {
        assert!(parse_wc_capture("frames=1,2").is_err());
    }

    #[test]
    fn missing_frames_is_error() {
        assert!(parse_wc_capture("dir=out").is_err());
    }

    #[test]
    fn empty_frames_is_error() {
        assert!(parse_wc_capture("dir=out;frames=").is_err());
    }

    #[test]
    fn unknown_key_is_error() {
        assert!(parse_wc_capture("dir=out;frames=1;bogus=2").is_err());
    }

    #[test]
    fn malformed_pair_without_equals_is_error() {
        assert!(parse_wc_capture("dir=out;frames").is_err());
    }

    #[test]
    fn whitespace_around_pairs_is_tolerated() {
        let cfg = parse_wc_capture(" dir = out ; frames = 1, 2 ").unwrap();
        assert_eq!(cfg.dir, std::path::PathBuf::from("out"));
        assert_eq!(cfg.frames, vec![1, 2]);
    }

    #[test]
    fn trailing_semicolon_is_ignored() {
        let cfg = parse_wc_capture("dir=out;frames=1;").unwrap();
        assert_eq!(cfg.frames, vec![1]);
    }

    #[test]
    fn bad_frame_index_is_error() {
        assert!(parse_wc_capture("dir=out;frames=1,x,3").is_err());
    }

    #[test]
    fn bad_dt_is_error() {
        assert!(parse_wc_capture("dir=out;frames=1;dt=fast").is_err());
    }
}
