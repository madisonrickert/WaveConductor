//! Platform selection for the temperature sensor (Plan 11.8, Seam 1).
//!
//! Per AGENTS.md, platform-specific code lives behind a `platform` module; the
//! portable [`super::sensor`] loop never contains `cfg` blocks. [`create_sensor`]
//! returns the boxed [`super::sensor::TemperatureSensor`] for the current target,
//! or `None` when no usable sensor is available.
//!
//! ## Feature gating
//!
//! Real sensing is enabled by the `thermal-sensor` cargo feature. It pulls in NO
//! dependency: the Linux reader uses `std::fs` directly. When the feature is
//! **off** (the wc-core default), the reader modules are not compiled and
//! [`create_sensor`] always returns `None`, so the thermal state holds
//! Cool/Schedule — the design's intended no-sensor degradation. When the feature
//! is **on**:
//!
//! - **Linux** → [`native::SysfsThermalSensor`] reads the hottest CPU/SoC
//!   temperature from the kernel `hwmon` / `thermal_zone` sysfs (zero deps).
//! - **Windows** → compiles, but the sysfs paths are absent so it returns
//!   `None` (Windows is not a deployment target; a future WMI reader could slot
//!   in here).
//! - **wasm** → `None` (no thermal API in the browser).
//! - **macOS** → `None` *unless* the separate `thermal-sensor-macos` feature is
//!   also enabled, in which case [`macos::MacmonSensor`] reads the Apple-Silicon
//!   `SoC` temperature via `macmon` (Apple `IOReport`, no sudo). That feature is
//!   **dormant**: `macmon`'s MSRV exceeds the pinned rustc 1.89, so it is not a
//!   declared dependency and the feature pulls nothing today. Plain
//!   `thermal-sensor` therefore compiles on macOS and falls through to `None`.
//!
//! Exactly one `create_sensor` body is compiled, selected by `cfg`, so there are
//! no unreachable-expression warnings across the feature/target matrix.

#[cfg(all(feature = "thermal-sensor-macos", target_os = "macos"))]
mod macos;

#[cfg(all(
    feature = "thermal-sensor",
    not(target_os = "macos"),
    not(target_arch = "wasm32")
))]
mod native;

use super::sensor::TemperatureSensor;

/// macOS + `thermal-sensor-macos`: try the `macmon` Apple-Silicon sensor.
#[cfg(all(feature = "thermal-sensor-macos", target_os = "macos"))]
#[must_use]
pub fn create_sensor() -> Option<Box<dyn TemperatureSensor>> {
    let sensor = macos::MacmonSensor::new()?;
    let boxed: Box<dyn TemperatureSensor> = Box::new(sensor);
    Some(boxed)
}

/// Linux/Windows + `thermal-sensor`: use the zero-dependency sysfs reader.
/// `None` if the OS exposes no readable CPU/SoC temperature (e.g. Windows, where
/// the Linux sysfs paths are absent).
#[cfg(all(
    feature = "thermal-sensor",
    not(target_os = "macos"),
    not(target_arch = "wasm32")
))]
#[must_use]
pub fn create_sensor() -> Option<Box<dyn TemperatureSensor>> {
    let sensor = native::SysfsThermalSensor::new()?;
    let boxed: Box<dyn TemperatureSensor> = Box::new(sensor);
    Some(boxed)
}

/// No compiled sensor for this build: feature off, wasm, or macOS without the
/// dormant `thermal-sensor-macos` feature. Degrade to the schedule fallback.
#[cfg(not(any(
    all(
        feature = "thermal-sensor",
        not(target_os = "macos"),
        not(target_arch = "wasm32")
    ),
    all(feature = "thermal-sensor-macos", target_os = "macos")
)))]
#[must_use]
pub fn create_sensor() -> Option<Box<dyn TemperatureSensor>> {
    None
}
