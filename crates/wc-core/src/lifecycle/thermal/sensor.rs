//! Temperature-sensor abstraction and the background sampler thread.
//!
//! The [`TemperatureSensor`] trait is the seam between the platform-specific
//! reader (a zero-dependency `std::fs` sysfs reader on Linux; `macmon` on Apple
//! Silicon when its macOS feature is enabled — see [`super::platform`]) and the
//! platform-agnostic sampler loop here. Sampling runs on a dedicated OS thread
//! so a blocking reader (the macOS `macmon` sampler) never stalls the Bevy main
//! thread; readings are published over a `std::sync::mpsc` channel the
//! main-thread drain system consumes.

use std::thread;
use std::time::Duration;

/// One temperature observation produced by the sampler thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThermalReading {
    /// Representative temperature in degrees Celsius (CPU/SoC package).
    pub temp_c: f32,
}

/// Receiver end of the sampler channel, held as a Bevy non-send resource (the
/// `Receiver` is `!Sync`; only the main-thread drain system touches it).
pub struct ThermalReadingReceiver(pub std::sync::mpsc::Receiver<ThermalReading>);

/// A platform temperature source. Returns `None` from [`Self::read_celsius`]
/// whenever the sensor is unavailable or produced no usable value, so the
/// caller can degrade gracefully (never panic — the deployment must survive a
/// missing/erroring sensor for days).
///
/// Sensors are constructed inside the sampler thread, so platform backends that
/// wrap thread-affine handles do not need to be [`Send`].
pub trait TemperatureSensor {
    /// Read the current representative temperature in Celsius, or `None` if the
    /// sensor is unavailable this sample. May block (the macOS sampler does);
    /// always called from the background thread, never the main thread.
    fn read_celsius(&mut self) -> Option<f32>;
}

/// Spawn the background sampler thread for the current platform, returning the
/// channel receiver to hold as a resource. Returns `None` when the platform has
/// no temperature sensor at all (web), in which case the caller leaves
/// [`super::ThermalState`] at its Cool/Schedule default.
///
/// The thread constructs the platform sensor, then loops forever: read → (maybe)
/// send → sleep `interval`. It exits silently when the receiver is dropped (app
/// shutdown), because `send` returns `Err` and we break the loop. A failed read
/// is skipped (no send), so a transiently-erroring sensor simply produces gaps
/// rather than bogus values.
#[must_use]
pub fn spawn_sampler(interval: Duration) -> Option<ThermalReadingReceiver> {
    let (tx, rx) = std::sync::mpsc::channel::<ThermalReading>();
    let spawned = thread::Builder::new()
        .name("wc-thermal-sampler".to_owned())
        .spawn(move || {
            let Some(mut sensor) = super::platform::create_sensor() else {
                return;
            };
            loop {
                if let Some(temp_c) = sensor.read_celsius() {
                    // `send` errors only once the receiver is dropped (shutdown).
                    if tx.send(ThermalReading { temp_c }).is_err() {
                        break;
                    }
                }
                thread::sleep(interval);
            }
        });
    match spawned {
        Ok(_handle) => Some(ThermalReadingReceiver(rx)),
        Err(err) => {
            tracing::warn!(?err, "thermal: failed to spawn sampler thread; degrading");
            None
        }
    }
}

/// Drain helper used by tests to assert channel semantics without a real sensor.
/// Returns the most recent reading, discarding older ones, or `None` if empty.
#[cfg(test)]
pub(crate) fn drain_latest(
    rx: &std::sync::mpsc::Receiver<ThermalReading>,
) -> Option<ThermalReading> {
    let mut latest = None;
    while let Ok(r) = rx.try_recv() {
        latest = Some(r);
    }
    latest
}

// `ThermalReadingReceiver` is held via `App::insert_non_send`, not the
// `Resource` derive: the inner `Receiver` is `!Sync`, which the `Resource`
// derive forbids. Bevy's non-send resource storage requires no trait impl. Do
// NOT add `#[derive(Resource)]` here — it will not compile against the `!Sync`
// `Receiver` and is unnecessary for non-send insertion.

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic in-test sensor producing a fixed ramp.
    struct RampSensor {
        next: f32,
    }

    impl TemperatureSensor for RampSensor {
        fn read_celsius(&mut self) -> Option<f32> {
            let v = self.next;
            self.next += 1.0;
            Some(v)
        }
    }

    #[test]
    fn drain_latest_keeps_only_most_recent() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(ThermalReading { temp_c: 10.0 }).unwrap_or(());
        tx.send(ThermalReading { temp_c: 20.0 }).unwrap_or(());
        tx.send(ThermalReading { temp_c: 30.0 }).unwrap_or(());
        let latest = drain_latest(&rx);
        assert_eq!(latest, Some(ThermalReading { temp_c: 30.0 }));
        // Channel now empty.
        assert!(drain_latest(&rx).is_none());
    }

    #[test]
    fn ramp_sensor_increments() {
        let mut s = RampSensor { next: 40.0 };
        assert_eq!(s.read_celsius(), Some(40.0));
        assert_eq!(s.read_celsius(), Some(41.0));
    }
}
