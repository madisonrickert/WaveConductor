//! Platform selection for the temperature sensor (Plan 11.8, Seam 1).
//!
//! Per AGENTS.md, platform-specific code lives behind a `platform` module; the
//! portable [`super::sensor`] loop never contains `cfg` blocks. [`create_sensor`]
//! returns the boxed [`super::sensor::TemperatureSensor`] for the current target,
//! or `None` when no usable sensor is available.
//!
//! ## Feature gating
//!
//! Real sensing is enabled by the `thermal-sensor` cargo feature (plus the
//! per-OS `thermal-sensor-macos` / `thermal-sensor-windows` features for the
//! platforms whose readers need an optional dependency). When no real reader is
//! compiled, [`create_sensor`] returns `None`, so the thermal state holds
//! Cool/Schedule — the design's intended no-sensor degradation.
//!
//! - **Linux** (`thermal-sensor`) → [`native::SysfsThermalSensor`] reads the
//!   hottest CPU/SoC temperature from the kernel `hwmon` / `thermal_zone` sysfs
//!   (zero deps).
//! - **Windows** (`thermal-sensor-windows`) → [`windows::WddmThermalSensor`] reads
//!   the iGPU/SoC die temperature via the no-admin WDDM `D3DKMT` adapter-perf-data
//!   query as a coarse throttle proxy (reliable no-admin CPU-die temps are not
//!   available on consumer Windows). Without the feature, Windows falls to the
//!   `native` reader, whose sysfs paths are absent, so it returns `None`.
//! - **wasm** → `None` (no thermal API in the browser).
//! - **macOS** → `None` *unless* `thermal-sensor-macos` is enabled, in which case
//!   [`macos::MacmonSensor`] reads the Apple-Silicon `SoC` temperature via
//!   `macmon` (Apple `IOReport`, no sudo). Plain `thermal-sensor` compiles on
//!   macOS and falls through to `None`.
//!
//! Exactly one `create_sensor` body is compiled, selected by `cfg`, so there are
//! no unreachable-expression warnings across the feature/target matrix. The
//! Linux/`native` arm explicitly yields to the Windows arm (`not(all(windows +
//! thermal-sensor-windows))`) so the two never both define `create_sensor`.

#[cfg(all(feature = "thermal-sensor-macos", target_os = "macos"))]
mod macos;

#[cfg(all(
    feature = "thermal-sensor",
    not(target_os = "macos"),
    not(target_arch = "wasm32"),
    not(all(feature = "thermal-sensor-windows", target_os = "windows"))
))]
mod native;

#[cfg(all(feature = "thermal-sensor-windows", target_os = "windows"))]
mod windows;

use super::sensor::TemperatureSensor;

/// macOS + `thermal-sensor-macos`: try the `macmon` Apple-Silicon sensor.
#[cfg(all(feature = "thermal-sensor-macos", target_os = "macos"))]
#[must_use]
pub fn create_sensor() -> Option<Box<dyn TemperatureSensor>> {
    let sensor = macos::MacmonSensor::new()?;
    let boxed: Box<dyn TemperatureSensor> = Box::new(sensor);
    Some(boxed)
}

/// Windows + `thermal-sensor-windows`: try the no-admin WDDM `D3DKMT` sensor.
/// `None` when no adapter exposes a temperature (common on integrated GPUs), so
/// the monitor degrades to its schedule fallback.
#[cfg(all(feature = "thermal-sensor-windows", target_os = "windows"))]
#[must_use]
pub fn create_sensor() -> Option<Box<dyn TemperatureSensor>> {
    let sensor = windows::WddmThermalSensor::new()?;
    let boxed: Box<dyn TemperatureSensor> = Box::new(sensor);
    Some(boxed)
}

/// Linux (and Windows without `thermal-sensor-windows`) + `thermal-sensor`: use
/// the zero-dependency sysfs reader. `None` if the OS exposes no readable CPU/SoC
/// temperature (e.g. Windows, where the Linux sysfs paths are absent).
#[cfg(all(
    feature = "thermal-sensor",
    not(target_os = "macos"),
    not(target_arch = "wasm32"),
    not(all(feature = "thermal-sensor-windows", target_os = "windows"))
))]
#[must_use]
pub fn create_sensor() -> Option<Box<dyn TemperatureSensor>> {
    let sensor = native::SysfsThermalSensor::new()?;
    let boxed: Box<dyn TemperatureSensor> = Box::new(sensor);
    Some(boxed)
}

/// No compiled sensor for this build: feature off, wasm, or macOS without the
/// `thermal-sensor-macos` feature. Degrade to the schedule fallback.
#[cfg(not(any(
    all(
        feature = "thermal-sensor",
        not(target_os = "macos"),
        not(target_arch = "wasm32"),
        not(all(feature = "thermal-sensor-windows", target_os = "windows"))
    ),
    all(feature = "thermal-sensor-macos", target_os = "macos"),
    all(feature = "thermal-sensor-windows", target_os = "windows")
)))]
#[must_use]
pub fn create_sensor() -> Option<Box<dyn TemperatureSensor>> {
    None
}
