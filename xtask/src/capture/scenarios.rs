//! Scenario table loaded from `tests/visual/scenarios.toml`.
//!
//! A scenario names a deterministic launch: which sketch, hand provider, config
//! isolation, `WC_DEBUG_*` toggles, captured frame indices, and optional `dt`.
//! Baselines key off the scenario name.

use std::collections::BTreeMap;

use serde::Deserialize;

/// Top-level `scenarios.toml` document: `[scenarios.<name>]` tables.
#[derive(Debug, Deserialize)]
pub struct Scenarios {
    /// Map of scenario name -> definition. `BTreeMap` so `names()` is sorted.
    pub scenarios: BTreeMap<String, Scenario>,
}

impl Scenarios {
    /// Look up a scenario by name.
    pub fn get(&self, name: &str) -> Option<&Scenario> {
        self.scenarios.get(name)
    }

    /// All scenario names, sorted (for `--list`).
    pub fn names(&self) -> Vec<String> {
        self.scenarios.keys().cloned().collect()
    }
}

/// One named capture scenario.
#[derive(Debug, Deserialize)]
pub struct Scenario {
    /// Sketch name -> `WAVECONDUCTOR_START_SKETCH` (`home` captures the Home
    /// picker screen; the app stays in `AppState::Home` for the whole run).
    pub sketch: String,
    /// Hand provider -> `WAVECONDUCTOR_HAND_PROVIDER` (`synthetic`/`mock`
    /// fixtures for deterministic captures; the env var sets the app's
    /// launch provider).
    pub provider: String,
    /// `"clean"` (fresh temp config dir) or a path pinned via
    /// `WAVECONDUCTOR_CONFIG_DIR`.
    pub config: String,
    /// `WC_DEBUG_*` toggles as `KEY = "value"` (KEY without the `WC_DEBUG_`
    /// prefix; the launcher re-prefixes).
    #[serde(default)]
    pub debug: BTreeMap<String, String>,
    /// Sim-frame indices to capture.
    pub frames: Vec<u32>,
    /// Optional fixed timestep in seconds (default `1/60` in the app).
    #[serde(default)]
    pub dt: Option<f64>,
    /// Optional window resolution `[width, height]` in physical pixels ->
    /// `WC_CAPTURE_RESOLUTION=WxH` (debug builds only; the app defaults to
    /// 1280x720 when absent). Lets portrait/rotated-display scenarios capture
    /// at the deployment aspect ratio.
    #[serde(default)]
    pub resolution: Option<[u32; 2]>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is appropriate in test code")]
mod tests {
    use super::*;

    #[test]
    fn parses_scenarios_toml() {
        let toml = r#"
            [scenarios.line-synthetic]
            sketch = "line"
            provider = "synthetic"
            config = "clean"
            frames = [30, 60, 120]
            dt = 0.016666667

            [scenarios.line-synthetic.debug]
            FORCE_G = "8000"
            DISABLE_BLOOM = "1"
        "#;
        let scenarios: Scenarios = toml::from_str(toml).unwrap();
        let s = scenarios.get("line-synthetic").unwrap();
        assert_eq!(s.sketch, "line");
        assert_eq!(s.provider, "synthetic");
        assert_eq!(s.config, "clean");
        assert_eq!(s.frames, vec![30, 60, 120]);
        assert_eq!(s.debug.get("FORCE_G").map(String::as_str), Some("8000"));
        // No `resolution` key -> None (the app keeps its 1280x720 default).
        assert_eq!(s.resolution, None);
    }

    #[test]
    fn parses_optional_resolution() {
        let toml = r#"
            [scenarios.home-portrait]
            sketch = "home"
            provider = "mock"
            config = "clean"
            frames = [30, 60]
            resolution = [1080, 1920]
        "#;
        let scenarios: Scenarios = toml::from_str(toml).unwrap();
        let s = scenarios.get("home-portrait").unwrap();
        assert_eq!(s.sketch, "home");
        assert_eq!(s.resolution, Some([1080, 1920]));
    }

    #[test]
    fn names_are_listed_sorted() {
        let toml = r#"
            [scenarios.zebra]
            sketch = "line"
            provider = "mock"
            config = "clean"
            frames = [1]
            [scenarios.alpha]
            sketch = "line"
            provider = "mock"
            config = "clean"
            frames = [1]
        "#;
        let scenarios: Scenarios = toml::from_str(toml).unwrap();
        assert_eq!(
            scenarios.names(),
            vec!["alpha".to_string(), "zebra".to_string()]
        );
    }
}
