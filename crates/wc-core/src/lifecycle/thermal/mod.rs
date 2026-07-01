//! Adaptive thermal signal for the screensaver / attract mode (Plan 11.8, Seam 1).
//!
//! ## Role
//!
//! Maintains a single [`ThermalState`] resource that classifies the machine's
//! current thermal headroom into one of three tiers — [`ThermalTier::Cool`],
//! [`ThermalTier::Warm`], [`ThermalTier::Hot`]. The screensaver reads this tier
//! to ratchet its rendering richness down only when the hardware is actually
//! getting hot, keeping the attract visual rich while there is headroom and
//! shedding heat (a hard-dropped present rate, down to the ≈3 fps "resting
//! ember" at Hot — spec §10.1's "Low-Rate Ember") when there is not. The whole
//! rationale of the v5 rewrite is unattended
//! multi-day thermal stability; this is the signal that drives it.
//!
//! ## Data flow
//!
//! 1. A background OS thread ([`sensor::spawn_sampler`]) polls the platform
//!    temperature sensor on a fixed interval. Each reading is sent over a
//!    `std::sync::mpsc` channel — sampling never blocks the Bevy main thread
//!    (the macOS `macmon` sampler blocks for its window; the Linux sysfs read is
//!    cheap but still kept off-thread for uniformity).
//! 2. `drain_thermal_readings` runs once per main-schedule frame, draining all
//!    pending readings, taking the most recent, and applying **asymmetric
//!    hysteresis** ([`ThermalThresholds`]) so the tier never flaps at a
//!    boundary.
//! 3. Anyone can read `Res<ThermalState>`; only the screensaver consumes it now
//!    (design-for-but-defer — D9).
//!
//! ## Signal-source degradation chain
//!
//! Real sensor → (future) GPU-time throttle proxy → conservative time schedule.
//! Each reading is tagged with its [`ThermalSource`] for the dev panel / logs.
//! When no sensor is available (Apple-Silicon falls through to no sensor, a
//! Linux box with no exposed `hwmon`/`thermal_zone`, or web), the state holds
//! [`ThermalTier::Cool`] tagged
//! [`ThermalSource::Schedule`]; a future phase plugs the GPU-time proxy and a
//! time-based floor into the same drain system without reshaping the resource.
//!
//! ## Release safety
//!
//! This module is always compiled (it is a production thermal lever, not a
//! debug scaffold). The sampler thread is spawned once at startup and parked on
//! a channel between samples — negligible idle cost, no per-frame allocation.

pub mod sensor;

mod platform;

use std::time::Duration;

use bevy::prelude::*;

use self::sensor::ThermalReading;

/// Coarse thermal headroom classification consumed by the screensaver.
///
/// Ordered coolest → hottest so `tier as u8` and `PartialOrd` both reflect
/// "hotter" as "greater". The screensaver maps each tier to a present rate —
/// the only thermal lever shipped (spec §10.1's "Low-Rate Ember"); per-tier
/// particle budgets and a Hot dispatch freeze remain deferred, soak-gated
/// options (spec §10.4).
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum ThermalTier {
    /// Full headroom: present at the operator's screensaver FPS cap
    /// (`ScreensaverSettings::screensaver_fps`, default 15).
    #[default]
    Cool,
    /// Warming: drop the present rate (≈15 fps), same choreography.
    Warm,
    /// Hot: drop the present rate hard (≈3 fps "resting ember"); no dispatch
    /// freeze — that remains a deferred escalation (spec §10.1).
    Hot,
}

/// Where the current [`ThermalState`] reading came from. Surfaced in the dev
/// panel and logs so an operator can tell a real sensor reading from a
/// degraded fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThermalSource {
    /// A real OS temperature sensor (Linux sysfs `hwmon`/`thermal_zone`, or
    /// macmon on Apple Silicon).
    Sensor,
    /// A GPU-time throttle-detection proxy (designed-for; not built yet).
    GpuTimeProxy,
    /// The conservative time-based schedule floor (no sensor available).
    #[default]
    Schedule,
}

/// The shared thermal signal. One resource anyone may read; the screensaver is
/// its only consumer today (D9 — built minimal-but-general for later sketches
/// and in-sketch auto-adaptation).
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct ThermalState {
    /// Current classified tier (after hysteresis).
    pub tier: ThermalTier,
    /// Most recent raw temperature in Celsius, if a sensor produced one.
    /// `None` while degraded to the schedule/proxy source.
    pub last_temp_c: Option<f32>,
    /// Provenance of the current reading.
    pub source: ThermalSource,
}

impl Default for ThermalState {
    fn default() -> Self {
        // Start Cool/Schedule: until the first real reading arrives we assume
        // there is headroom (the safe direction is to render rich, not to
        // throttle a cold machine). The drain system upgrades the source to
        // `Sensor` on the first reading.
        Self {
            tier: ThermalTier::Cool,
            last_temp_c: None,
            source: ThermalSource::Schedule,
        }
    }
}

/// Asymmetric hysteresis thresholds, in Celsius. A tier is *entered* at its
/// higher `enter_*` temperature and only *left* at the lower `leave_*`
/// temperature, so a reading hovering on a boundary cannot flap the tier.
///
/// ```text
///   Cool ──(≥ enter_warm)──▶ Warm ──(≥ enter_hot)──▶ Hot
///   Cool ◀──(< leave_warm)── Warm ◀──(< leave_hot)── Hot
/// ```
///
/// The invariant `leave_warm < enter_warm <= leave_hot < enter_hot` is checked
/// at construction; [`Self::default`] satisfies it. Defaults are placeholders
/// to be tuned against the 8-hour soak on real hardware (the M1 dev box throttles
/// around 100 °C package; the NUC's Intel iGPU package runs hotter under load).
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct ThermalThresholds {
    /// Cool → Warm boundary (entered at/above this temperature).
    pub enter_warm: f32,
    /// Warm → Cool boundary (left below this temperature). `< enter_warm`.
    pub leave_warm: f32,
    /// Warm → Hot boundary (entered at/above this temperature).
    pub enter_hot: f32,
    /// Hot → Warm boundary (left below this temperature). `< enter_hot`.
    pub leave_hot: f32,
}

impl Default for ThermalThresholds {
    fn default() -> Self {
        // Placeholder bands (°C). Tune against the soak; see struct docs.
        Self {
            enter_warm: 75.0,
            leave_warm: 70.0,
            enter_hot: 90.0,
            leave_hot: 85.0,
        }
    }
}

impl ThermalThresholds {
    /// Classify a temperature into a tier given the *current* tier, applying
    /// asymmetric hysteresis. `current` is the tier the state already holds; the
    /// returned tier is the new tier after this reading.
    ///
    /// The hysteresis is encoded by choosing the boundary based on the direction
    /// of travel: rising readings use the `enter_*` thresholds, falling readings
    /// use the `leave_*` thresholds. Equivalent to a Schmitt trigger per band.
    #[must_use]
    pub fn classify(self, current: ThermalTier, temp_c: f32) -> ThermalTier {
        match current {
            ThermalTier::Cool => {
                if temp_c >= self.enter_hot {
                    ThermalTier::Hot
                } else if temp_c >= self.enter_warm {
                    ThermalTier::Warm
                } else {
                    ThermalTier::Cool
                }
            }
            ThermalTier::Warm => {
                if temp_c >= self.enter_hot {
                    ThermalTier::Hot
                } else if temp_c < self.leave_warm {
                    ThermalTier::Cool
                } else {
                    ThermalTier::Warm
                }
            }
            ThermalTier::Hot => {
                if temp_c < self.leave_hot {
                    // Drop one band at a time so the visual ratchets back up
                    // smoothly rather than jumping Hot → Cool. Re-classify the
                    // intermediate as if we were Warm.
                    if temp_c < self.leave_warm {
                        ThermalTier::Cool
                    } else {
                        ThermalTier::Warm
                    }
                } else {
                    ThermalTier::Hot
                }
            }
        }
    }

    /// True iff the band ordering invariant holds.
    #[must_use]
    pub fn is_well_ordered(self) -> bool {
        self.leave_warm < self.enter_warm
            && self.enter_warm <= self.leave_hot
            && self.leave_hot < self.enter_hot
    }
}

/// How often the background thread samples the temperature sensor. A few seconds
/// is plenty: thermal mass changes slowly, and the screensaver only needs a
/// leading indicator, not a high-rate signal.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(3);

/// Plugin that maintains [`ThermalState`] from a background temperature sampler.
///
/// Inserts the default [`ThermalState`] and [`ThermalThresholds`] resources,
/// spawns the platform sampler thread, and registers the per-frame drain system
/// that applies hysteresis. Registered by [`crate::CorePlugin`] via
/// [`super::LifecyclePlugin`].
pub struct ThermalMonitorPlugin;

impl Plugin for ThermalMonitorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ThermalState>()
            .init_resource::<ThermalThresholds>();

        // Spawn the sampler thread and hold its receiver as a non-send resource
        // (the `Receiver` is `!Sync`; only the main-thread drain system reads
        // it). If the platform sensor cannot initialize inside the thread, no
        // readings are sent and the state simply holds Cool/Schedule forever.
        match sensor::spawn_sampler(SAMPLE_INTERVAL) {
            Some(receiver) => {
                app.insert_non_send(receiver);
                app.add_systems(Update, drain_thermal_readings);
                tracing::info!("thermal: sampler thread started");
            }
            None => {
                tracing::info!(
                    "thermal: no temperature sensor on this platform; \
                     ThermalState stays Cool/Schedule"
                );
            }
        }
    }
}

/// Per-frame: drain all pending [`ThermalReading`]s, apply hysteresis to the
/// most recent, and update [`ThermalState`]. Only writes the resource when the
/// tier or temperature actually changes, so change-detection consumers (the
/// screensaver) wake only on a real transition.
///
/// Draining (rather than reading one) keeps the channel from backing up if the
/// main loop stalls; we only care about the latest temperature.
fn drain_thermal_readings(
    receiver: Option<NonSend<'_, sensor::ThermalReadingReceiver>>,
    thresholds: Res<'_, ThermalThresholds>,
    mut state: ResMut<'_, ThermalState>,
) {
    let Some(receiver) = receiver else {
        return;
    };
    let mut latest: Option<ThermalReading> = None;
    while let Ok(reading) = receiver.0.try_recv() {
        latest = Some(reading);
    }
    let Some(reading) = latest else {
        return;
    };
    let new_tier = thresholds.classify(state.tier, reading.temp_c);
    let next = ThermalState {
        tier: new_tier,
        last_temp_c: Some(reading.temp_c),
        source: ThermalSource::Sensor,
    };
    if *state != next {
        if state.tier != new_tier {
            tracing::info!(
                from = ?state.tier,
                to = ?new_tier,
                // Round to one decimal: the sensor's f32 reading widens to a long
                // f64 tail (e.g. 76.68995666503906) that adds no signal to the log.
                temp_c = (reading.temp_c * 10.0).round() / 10.0,
                "thermal: tier transition"
            );
        }
        *state = next;
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "threshold constants are exact; equality is the intended comparison"
)]
mod tests {
    use super::*;

    #[test]
    fn default_thresholds_are_well_ordered() {
        assert!(ThermalThresholds::default().is_well_ordered());
    }

    #[test]
    fn rising_temperature_climbs_tiers() {
        let t = ThermalThresholds::default();
        assert_eq!(t.classify(ThermalTier::Cool, 50.0), ThermalTier::Cool);
        assert_eq!(t.classify(ThermalTier::Cool, 76.0), ThermalTier::Warm);
        assert_eq!(t.classify(ThermalTier::Warm, 91.0), ThermalTier::Hot);
    }

    #[test]
    fn falling_temperature_descends_tiers() {
        let t = ThermalThresholds::default();
        // From Hot, must drop below leave_hot (85) to leave Hot, then below
        // leave_warm (70) to reach Cool.
        assert_eq!(t.classify(ThermalTier::Hot, 84.0), ThermalTier::Warm);
        assert_eq!(t.classify(ThermalTier::Warm, 69.0), ThermalTier::Cool);
    }

    /// The decisive property: a temperature sitting *between* the enter and
    /// leave thresholds must not change the tier in either direction. This is
    /// what stops the tier from flapping when the temperature hovers on a band.
    #[test]
    fn hysteresis_band_does_not_flap() {
        let t = ThermalThresholds::default();
        // Between leave_warm (70) and enter_warm (75): Cool stays Cool, Warm
        // stays Warm — same input, tier preserved.
        assert_eq!(t.classify(ThermalTier::Cool, 72.0), ThermalTier::Cool);
        assert_eq!(t.classify(ThermalTier::Warm, 72.0), ThermalTier::Warm);
        // Between leave_hot (85) and enter_hot (90): Warm stays Warm, Hot stays Hot.
        assert_eq!(t.classify(ThermalTier::Warm, 87.0), ThermalTier::Warm);
        assert_eq!(t.classify(ThermalTier::Hot, 87.0), ThermalTier::Hot);
    }

    #[test]
    fn exact_enter_threshold_transitions() {
        let t = ThermalThresholds::default();
        // `>=` semantics: exactly enter_warm enters Warm.
        assert_eq!(
            t.classify(ThermalTier::Cool, t.enter_warm),
            ThermalTier::Warm
        );
        assert_eq!(t.classify(ThermalTier::Warm, t.enter_hot), ThermalTier::Hot);
    }

    #[test]
    fn hot_drops_at_most_to_warm_when_just_below_leave_hot() {
        let t = ThermalThresholds::default();
        // Just below leave_hot but above leave_warm → Warm, not Cool.
        let temp = f32::midpoint(t.leave_hot, t.leave_warm);
        assert_eq!(t.classify(ThermalTier::Hot, temp), ThermalTier::Warm);
    }

    #[test]
    fn default_state_is_cool_schedule() {
        let s = ThermalState::default();
        assert_eq!(s.tier, ThermalTier::Cool);
        assert_eq!(s.source, ThermalSource::Schedule);
        assert!(s.last_temp_c.is_none());
    }

    #[test]
    fn tier_ordering_reflects_heat() {
        assert!(ThermalTier::Cool < ThermalTier::Warm);
        assert!(ThermalTier::Warm < ThermalTier::Hot);
    }
}
