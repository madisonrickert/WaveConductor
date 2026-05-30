//! `macmon`-backed temperature sensor for Apple Silicon (the dev MacBook Pro).
//!
//! `sysinfo` returns an empty `Components` list on Apple Silicon, so the macOS
//! path reads the SoC temperature through `macmon`'s IOReport/IOKit sampler (no
//! sudo). `macmon` is a young library wrapping a *private* Apple API, so it is
//! pinned (`=0.7.0` in the workspace manifest) and fully wrapped behind the
//! portable [`TemperatureSensor`] trait: any construction or sampling error
//! degrades to `None` (→ `ThermalState` holds Cool/Schedule) rather than
//! panicking. This is a dev-platform convenience; the deployment NUC uses
//! [`super::native`].
//!
//! ## Blocking
//!
//! `Sampler::get_metrics(duration_ms)` blocks for its measurement window. This
//! reader is only ever called on the background sampler thread (see
//! [`crate::lifecycle::thermal::sensor::spawn_sampler`]), never the Bevy main
//! thread, so the block is harmless.
//!
//! ## Dormant: `macmon` is not yet a dependency
//!
//! This module is compiled only under `feature = "thermal-sensor-macos"`, a
//! documented placeholder feature. `macmon`'s MSRV exceeds the pinned rustc
//! 1.89, so it is intentionally NOT declared in `Cargo.toml` and the feature
//! pulls nothing. The `compile_error!` below turns an accidental
//! `--features thermal-sensor-macos` into a clear, actionable message instead of
//! a cryptic "unresolved crate `macmon`". The rest of this file is preserved
//! verbatim so re-enabling Apple-Silicon sensing once the toolchain advances is
//! a one-spot change: declare the optional `macmon` dep, point the feature at
//! `["dep:macmon"]`, and delete this guard.

// `macmon` is not a declared dependency (see the module docs). Fail loudly with
// a hint rather than emitting a confusing missing-crate error.
compile_error!(
    "feature `thermal-sensor-macos` is a dormant placeholder: the `macmon` \
     dependency is not declared because its MSRV exceeds the pinned rustc 1.89. \
     To enable Apple-Silicon sensing, add `macmon` as an optional dependency, \
     change the feature to `[\"dep:macmon\"]`, and remove this guard. Until \
     then use plain `thermal-sensor` on macOS (it falls through to the \
     synthetic Cool/Schedule model)."
);

use macmon::metrics::Sampler;

use crate::lifecycle::thermal::sensor::TemperatureSensor;

/// Measurement window for one `macmon` sample, in milliseconds. Short enough to
/// keep the background thread responsive, long enough for IOReport to produce a
/// stable reading.
const SAMPLE_WINDOW_MS: u32 = 200;

/// Apple-Silicon temperature reader backed by `macmon`'s [`Sampler`].
pub struct MacmonSensor {
    sampler: Sampler,
}

impl MacmonSensor {
    /// Build the sensor, or `None` if `macmon` cannot initialize its IOReport /
    /// SMC sources (e.g. an unsupported macOS build or a future API break).
    /// `Sampler::new` returns a boxed-error `Result`; we map any error to `None`
    /// and log once so the deployment degrades silently instead of crashing.
    #[must_use]
    pub fn new() -> Option<Self> {
        match Sampler::new() {
            Ok(sampler) => Some(Self { sampler }),
            Err(err) => {
                tracing::warn!(%err, "thermal: macmon Sampler::new failed; no macOS sensor");
                None
            }
        }
    }
}

impl TemperatureSensor for MacmonSensor {
    /// Sample one measurement window and return the average CPU temperature.
    ///
    /// `macmon` reports `cpu_temp_avg` and `gpu_temp_avg`; we use the hotter of
    /// the two so the screensaver reacts to whichever subsystem is under load
    /// (the SoC shares a thermal envelope). Any sampling error or a non-finite /
    /// zero reading (macmon reports `0.0` when a sensor channel is unavailable)
    /// yields `None`.
    fn read_celsius(&mut self) -> Option<f32> {
        let metrics = match self.sampler.get_metrics(SAMPLE_WINDOW_MS) {
            Ok(m) => m,
            Err(err) => {
                tracing::debug!(%err, "thermal: macmon get_metrics failed this sample");
                return None;
            }
        };
        let cpu = metrics.temp.cpu_temp_avg;
        let gpu = metrics.temp.gpu_temp_avg;
        let hottest = cpu.max(gpu);
        // macmon yields 0.0 for an unavailable channel; treat <=0 or non-finite
        // as "no reading" so a half-populated metrics struct never reads as a
        // freezing machine (which would wrongly hold the richest tier).
        if hottest.is_finite() && hottest > 0.0 {
            Some(hottest)
        } else {
            None
        }
    }
}
