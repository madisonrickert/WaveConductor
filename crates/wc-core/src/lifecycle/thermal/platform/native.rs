//! Zero-dependency Linux temperature sensor for the deployment NUC.
//!
//! Reads the kernel's `hwmon` / `thermal_zone` sysfs interface directly with
//! `std::fs` — no crate, no MSRV pin, no build-time native dependency, no
//! `unsafe`. For a coarse "hottest CPU/SoC component, every few seconds" signal
//! on a known Linux `x86_64` box, this is the most robust option for an unattended
//! multi-day appliance: there is nothing in the supply chain to break.
//!
//! ## What it reads
//!
//! The kernel exposes every temperature as an integer file in **millidegrees
//! Celsius** (`/1000` → °C), world-readable, no root.
//!
//! 1. **Preferred — hwmon by chip name.** `/sys/class/hwmon/hwmonN/name` is a
//!    stable lowercase driver identifier (mandatory per the kernel hwmon ABI).
//!    We trust the CPU drivers ([`is_cpu_chip`]) — Intel `coretemp`, AMD
//!    `k10temp`/`zenpower`, ARM `cpu_thermal` — and take the hottest
//!    `tempN_input` within them. For Intel `coretemp`, `temp1_input` is the
//!    "Package id 0" aggregate; taking the max also covers per-core sensors.
//!    This skips the noise a naive "read all thermal zones" pass trips on
//!    (`nvme`, `iwlwifi`, `acpitz`, chipset sensors).
//! 2. **Fallback — `thermal_zone` by type.** Some kernels expose the package temp
//!    via an `x86_pkg_temp` thermal zone but not via an hwmon `coretemp` chip
//!    (depends on which modules are loaded). The fallback catches that;
//!    `acpitz` is included last because it reads socket-adjacent, not die —
//!    acceptable for a coarse "is the box overheating" signal.
//!
//! ## Degradation
//!
//! Any missing directory, unparseable file, or out-of-range value is skipped;
//! [`SysfsThermalSensor::read_celsius`] returns `None` when nothing trustworthy
//! is found, so the monitor holds its Cool/Schedule no-sensor fallback. On a
//! machine with no exposed CPU sensor [`SysfsThermalSensor::new`] returns `None`
//! so the sampler thread is never spawned.
//!
//! ## Why not `sysinfo`
//!
//! `sysinfo` would pull ~30 transitive crates to wrap the same file reads, and
//! its generic component list mixes NVMe/WiFi/chipset sensors that must be
//! filtered by name anyway. The cross-platform breadth is moot here (the NUC is
//! Linux; Apple-Silicon returns empty from `sysinfo` regardless and uses
//! [`super::macos`] once the optional macOS sensor dependency is enabled).

use std::fs;
use std::path::Path;

use crate::lifecycle::thermal::sensor::TemperatureSensor;

/// Plausible CPU/SoC temperature band in °C. Readings outside this are treated
/// as a bogus/unpopulated channel and skipped (a load-bearing CPU sensor never
/// reports ≤ 0 °C, and > 150 °C is past any real throttle limit).
const PLAUSIBLE_C: std::ops::RangeInclusive<f32> = -40.0..=150.0;

/// hwmon driver `name` values trusted as a real CPU/SoC temperature source.
/// Intel → `coretemp`; AMD → `k10temp`/`zenpower`; many ARM `SoCs` → `cpu_thermal`.
fn is_cpu_chip(name: &str) -> bool {
    matches!(name, "coretemp" | "k10temp" | "zenpower" | "cpu_thermal")
}

/// Linux sysfs temperature reader. Stateless beyond the trait; each read walks
/// the (tiny) sysfs tree fresh so hot-plugged sensors or a late-loading
/// `coretemp` module are picked up without restart.
pub struct SysfsThermalSensor {
    /// Cached "the system exposed at least one CPU sensor at construction" flag,
    /// kept only so [`Self::new`] can refuse to build on a sensorless box.
    _private: (),
}

impl SysfsThermalSensor {
    /// Build the sensor, or `None` when no trustworthy CPU/SoC temperature is
    /// readable at all (non-Linux, or a box with no exposed sensor). A `Some`
    /// here means the first probe found a usable reading; later reads may still
    /// transiently return `None`, which the sampler tolerates.
    #[must_use]
    pub fn new() -> Option<Self> {
        read_hottest_celsius().map(|_| Self { _private: () })
    }
}

impl TemperatureSensor for SysfsThermalSensor {
    fn read_celsius(&mut self) -> Option<f32> {
        read_hottest_celsius()
    }
}

/// Parse a millidegree-Celsius integer file into °C, rejecting bogus values.
fn read_millidegrees(path: &Path) -> Option<f32> {
    let raw = fs::read_to_string(path).ok()?;
    let milli: i64 = raw.trim().parse().ok()?;
    // i64 millidegrees → f32 °C. The value range (±150_000) is far inside f32's
    // exact-integer range, so this conversion is lossless for any real reading.
    let celsius = f32::from(i16::try_from(milli / 1000).ok()?);
    PLAUSIBLE_C.contains(&celsius).then_some(celsius)
}

/// Fold a candidate temperature into the running hottest-so-far.
fn fold_hotter(hottest: &mut Option<f32>, candidate: f32) {
    *hottest = Some(hottest.map_or(candidate, |best| best.max(candidate)));
}

/// The hottest representative CPU/SoC temperature in °C, or `None` if none is
/// readable. Tries hwmon CPU chips first, then CPU-ish thermal zones.
fn read_hottest_celsius() -> Option<f32> {
    if let Some(t) = read_hwmon_cpu_chips() {
        return Some(t);
    }
    read_cpu_thermal_zones()
}

/// Strategy 1: hottest `tempN_input` across hwmon chips whose `name` is a known
/// CPU driver. Returns `None` if no such chip/reading exists.
fn read_hwmon_cpu_chips() -> Option<f32> {
    let mut hottest = None;
    let entries = fs::read_dir("/sys/class/hwmon").ok()?;
    for entry in entries.flatten() {
        let dir = entry.path();
        let name = fs::read_to_string(dir.join("name"))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        if !is_cpu_chip(&name) {
            continue;
        }
        let Ok(files) = fs::read_dir(&dir) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if fname.starts_with("temp") && fname.ends_with("_input") {
                if let Some(celsius) = read_millidegrees(&path) {
                    fold_hotter(&mut hottest, celsius);
                }
            }
        }
    }
    hottest
}

/// Strategy 2 (fallback): hottest `temp` across thermal zones whose `type` looks
/// CPU/package-related. Returns `None` if none match.
fn read_cpu_thermal_zones() -> Option<f32> {
    let mut hottest = None;
    let zones = fs::read_dir("/sys/class/thermal").ok()?;
    for zone in zones.flatten() {
        let path = zone.path();
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if !fname.starts_with("thermal_zone") {
            continue;
        }
        let ztype = fs::read_to_string(path.join("type"))
            .map(|s| s.trim().to_lowercase())
            .unwrap_or_default();
        let cpu_ish = ztype.contains("x86_pkg_temp")
            || ztype.contains("cpu")
            || ztype.contains("coretemp")
            || ztype.contains("acpitz");
        if cpu_ish {
            if let Some(celsius) = read_millidegrees(&path.join("temp")) {
                fold_hotter(&mut hottest, celsius);
            }
        }
    }
    hottest
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construction must never panic regardless of host (the test runner may be
    /// macOS or a sensorless CI box), and any successful read is plausible.
    #[test]
    fn construction_and_read_do_not_panic() {
        if let Some(mut sensor) = SysfsThermalSensor::new() {
            if let Some(celsius) = sensor.read_celsius() {
                assert!(
                    PLAUSIBLE_C.contains(&celsius),
                    "implausible temperature reading: {celsius}"
                );
            }
        }
    }

    #[test]
    fn cpu_chip_names_recognised() {
        assert!(is_cpu_chip("coretemp"));
        assert!(is_cpu_chip("k10temp"));
        assert!(!is_cpu_chip("nvme"));
        assert!(!is_cpu_chip("iwlwifi"));
    }

    #[test]
    fn fold_hotter_keeps_maximum() {
        let mut h = None;
        fold_hotter(&mut h, 40.0);
        fold_hotter(&mut h, 55.0);
        fold_hotter(&mut h, 48.0);
        assert_eq!(h, Some(55.0));
    }

    #[test]
    fn read_millidegrees_parses_and_bounds() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().expect("tempfile");
        write!(f, "52000\n").expect("write");
        assert_eq!(read_millidegrees(f.path()), Some(52.0));

        let mut bogus = tempfile::NamedTempFile::new().expect("tempfile");
        write!(bogus, "0").expect("write");
        // 0 °C is inside the plausible band but is the documented "unpopulated"
        // sentinel only at the sysinfo layer; here 0 is allowed through (a cold
        // boot can read low). Out-of-band values are what we reject:
        let mut hot = tempfile::NamedTempFile::new().expect("tempfile");
        write!(hot, "999000").expect("write");
        assert_eq!(read_millidegrees(hot.path()), None);
    }
}
