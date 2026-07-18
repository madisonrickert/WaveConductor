//! Pure, platform-independent selection logic for the Windows WMI ACPI
//! thermal-zone rung of the thermal sensor chain (Plan 05, alpha.5).
//!
//! ## Why this is its own portable module
//!
//! The WMI query itself is Windows-only COM FFI and lives in
//! [`super::platform`]'s Windows submodule, compiled only under
//! `all(target_os = "windows", feature = "thermal-sensor-windows")`. But the
//! *decisions* it makes — convert each raw zone reading from tenths of a Kelvin
//! to Celsius, reject anything implausible, and take the hottest surviving zone
//! — are arithmetic. Keeping them here, unconditionally compiled and free of any
//! `windows` dependency, is what lets them be unit-tested on the macOS and Linux
//! CI runners, where the Windows FFI never builds. An assertion that only runs
//! on a Windows box we do not own in CI never runs.
//!
//! ## Unit contract (IMPORTANT — verify on hardware)
//!
//! [`deci_kelvin_to_celsius`] assumes the raw WMI value is in **tenths of a
//! Kelvin** (deci-Kelvin): `°C = raw / 10 - 273.15`. That matches
//! `MSAcpi_ThermalZoneTemperature::CurrentTemperature` and the
//! `HighPrecisionTemperature` property of
//! `Win32_PerfFormattedData_Counters_ThermalZoneInformation`. The perf class
//! *also* exposes a whole-Kelvin `Temperature`; that one must NOT be read with
//! this function. Both the property name and its unit MUST be confirmed on the
//! target box before the readings are trusted — ACPI/WMI unit conventions are
//! not uniform across providers. See Plan 05, Task 4 for the PowerShell dump
//! that confirms it empirically.
//!
//! ## Behaviour-change caveat
//!
//! A box that previously reported no temperature at all (WDDM returns 0, no WMI
//! rung) pinned [`super::ThermalTier::Cool`] forever. Once this rung reports a
//! real temperature it will classify true tiers against the *placeholder*
//! [`super::ThermalThresholds`], which alpha.5 does not tune. That is the
//! intended, safe direction — throttling a cold machine is a safer failure than
//! baking a hot one — but it is a behaviour change to watch in the first soak.
//! Threshold tuning is explicitly out of Plan 05's scope.

// This module is Plan 05 Task 1: the pure thermal-zone selection logic. Its only
// production caller — the Windows WMI sensor, compiled solely for
// `all(target_os = "windows", feature = "thermal-sensor-windows")` — is Plan 05
// Tasks 2-3 and is not written yet. So on every target, including a native
// Windows `--all-features` build and CI's macOS/Linux ones, these `pub(crate)`
// items have no non-test caller and would trip `dead_code` under `-D warnings`.
// They are not dead: this file's tests exercise them on every platform, and the
// Windows sensor consumes them once Tasks 2-3 land. `allow`, not `expect`,
// because there is currently no target on which the caller exists.
#![allow(
    dead_code,
    reason = "Plan 05 Task 1 pure thermal-zone logic; consumed by the WMI sensor wired in Tasks 2-3"
)]

use std::ops::RangeInclusive;

/// Plausible `SoC` / package / chassis-skin temperature band in °C. A reading
/// outside this, after conversion, is a bogus or unpopulated channel and is
/// skipped. Matches the WDDM sensor's band (`platform::windows`'s
/// `PLAUSIBLE_C`): a powered machine never reads below 1 °C, and above 150 °C is
/// past any throttle limit.
pub(crate) const PLAUSIBLE_C: RangeInclusive<f32> = 1.0..=150.0;

/// One raw ACPI thermal-zone reading harvested from WMI, before unit conversion
/// or plausibility filtering.
///
/// Owns its name because the WMI query it comes from must copy the zone name out
/// of a COM `BSTR` anyway; that copy happens once per zone inside the query, not
/// in this module's selection path.
pub(crate) struct ZoneSample {
    /// Zone instance name (the `Name` column, e.g. `\_TZ.TZ00`). Carried only for
    /// the provenance log line; [`select_hottest`] ignores it when ranking.
    pub name: String,
    /// Raw temperature counter in tenths of a Kelvin. See the module unit
    /// contract.
    pub deci_kelvin: u32,
}

/// The hottest plausible zone selected from a batch of [`ZoneSample`]s.
///
/// Borrows the winner's name from the sample slice rather than cloning it: the
/// sampler runs on a continuously-running background thread for the life of a
/// multi-hour session, and AGENTS.md's no-allocation-in-a-hot-path rule covers
/// exactly that thread. `Copy`, so selection returns a value and allocates
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HottestZone<'a> {
    /// The winning zone's instance name, for the provenance log. Borrowed from
    /// the `samples` slice passed to [`select_hottest`].
    pub name: &'a str,
    /// The winning zone's temperature in °C.
    pub celsius: f32,
}

/// Convert a raw tenths-of-a-Kelvin thermal-zone reading to °C, or `None` when
/// the result falls outside [`PLAUSIBLE_C`].
///
/// `°C = raw / 10 - 273.15`. `u16::try_from` both avoids a lossy `as` cast and
/// rejects absurd raw values: any real reading is well under 4500 deci-Kelvin,
/// far inside `u16`, so a value that does not fit is a bogus channel and yields
/// `None`.
pub(crate) fn deci_kelvin_to_celsius(raw: u32) -> Option<f32> {
    // A real machine reports a few thousand deci-Kelvin; absolute zero is 0.
    let tenths = f32::from(u16::try_from(raw).ok()?);
    // 273.15 K is 0 °C. Scale tenths -> whole Kelvin, then shift to Celsius.
    let celsius = tenths / 10.0 - 273.15;
    PLAUSIBLE_C.contains(&celsius).then_some(celsius)
}

/// Pick the hottest plausible zone from `samples`, or `None` when none convert
/// into [`PLAUSIBLE_C`].
///
/// Conservative by construction: the hottest surviving zone wins, so a genuinely
/// hot die still engages the throttle even when cooler chassis/skin zones are
/// also present. When *every* zone is implausible the answer is `None` — no
/// reading — never the least-bad garbage value: a wrong temperature would drive
/// the throttle from noise, whereas `None` lets the sensor chain fall through to
/// its schedule fallback. Ties keep the first zone in slice order, which is
/// stable for a fixed WMI enumeration and immaterial to the tier either way.
///
/// Allocation-free: iterates the slice and returns a `Copy` value borrowing the
/// winner's name.
pub(crate) fn select_hottest(samples: &[ZoneSample]) -> Option<HottestZone<'_>> {
    let mut best: Option<HottestZone<'_>> = None;
    for sample in samples {
        let Some(celsius) = deci_kelvin_to_celsius(sample.deci_kelvin) else {
            continue;
        };
        if best.is_none_or(|current| celsius > current.celsius) {
            best = Some(HottestZone {
                name: &sample.name,
                celsius,
            });
        }
    }
    best
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;

    fn sample(name: &str, deci_kelvin: u32) -> ZoneSample {
        ZoneSample {
            name: name.to_owned(),
            deci_kelvin,
        }
    }

    #[test]
    fn deci_kelvin_converts_room_and_load_temperatures() {
        // °C = raw/10 - 273.15. 2982 dK = (25.05 + 273.15) * 10.
        let room = deci_kelvin_to_celsius(2982).expect("2982 dK converts");
        assert!((room - 25.05).abs() < 0.05, "room: {room}");
        // 3345 dK = 61.35 °C, the temperature used in the provenance-log example.
        let load = deci_kelvin_to_celsius(3345).expect("3345 dK converts");
        assert!((load - 61.35).abs() < 0.05, "load: {load}");
    }

    #[test]
    fn deci_kelvin_rejects_below_band() {
        // 2732 dK = 0.05 °C, below the 1 °C floor.
        assert!(deci_kelvin_to_celsius(2732).is_none());
        // 0 raw = -273.15 °C.
        assert!(deci_kelvin_to_celsius(0).is_none());
    }

    #[test]
    fn deci_kelvin_rejects_above_band_and_non_u16() {
        // 4732 dK = 200.05 °C, above the 150 °C ceiling.
        assert!(deci_kelvin_to_celsius(4732).is_none());
        // 70_000 does not fit u16 — a bogus/unpopulated channel.
        assert!(deci_kelvin_to_celsius(70_000).is_none());
    }

    #[test]
    fn deci_kelvin_band_edges_are_inclusive() {
        // Just inside the 1 °C floor: 2742 dK = 1.05 °C.
        let floor = deci_kelvin_to_celsius(2742).expect("2742 dK is inside the floor");
        assert!((floor - 1.05).abs() < 0.05, "floor: {floor}");
        // Just outside it: 2741 dK = 0.95 °C.
        assert!(deci_kelvin_to_celsius(2741).is_none());

        // Just inside the 150 °C ceiling: 4231 dK = 149.95 °C.
        let ceiling = deci_kelvin_to_celsius(4231).expect("4231 dK is inside the ceiling");
        assert!((ceiling - 149.95).abs() < 0.05, "ceiling: {ceiling}");
        // Just outside it: 4232 dK = 150.05 °C.
        assert!(deci_kelvin_to_celsius(4232).is_none());
    }

    #[test]
    fn deci_kelvin_rejects_hotter_than_the_sun() {
        // u16::MAX fits the try_from but converts to ~6280 °C — the band, not the
        // integer width, is what rejects it.
        assert!(deci_kelvin_to_celsius(u32::from(u16::MAX)).is_none());
        assert!(deci_kelvin_to_celsius(u32::MAX).is_none());
    }

    #[test]
    fn select_hottest_takes_the_hottest_plausible_zone() {
        let samples = [
            sample("\\_TZ.TZ00", 3145), // 41.35 °C
            sample("\\_TZ.TZ01", 3345), // 61.35 °C  <- hottest plausible
            sample("\\_TZ.SKIN", 3045), // 31.35 °C
        ];
        let hottest = select_hottest(&samples).expect("a plausible zone exists");
        assert_eq!(hottest.name, "\\_TZ.TZ01");
        assert!(
            (hottest.celsius - 61.35).abs() < 0.05,
            "{}",
            hottest.celsius
        );
    }

    #[test]
    fn select_hottest_skips_implausible_and_keeps_survivors() {
        let samples = [
            sample("cold", 0),          // -273.15 °C, rejected
            sample("impossible", 6000), // 326.85 °C, rejected (above band)
            sample("real", 3200),       // 46.85 °C, the only survivor
        ];
        let hottest = select_hottest(&samples).expect("one plausible zone survives");
        assert_eq!(hottest.name, "real");
        assert!(
            (hottest.celsius - 46.85).abs() < 0.05,
            "{}",
            hottest.celsius
        );
    }

    #[test]
    fn select_hottest_is_none_when_all_implausible() {
        let samples = [sample("a", 0), sample("b", 70_000)];
        assert!(select_hottest(&samples).is_none());
    }

    #[test]
    fn select_hottest_is_none_for_empty() {
        assert!(select_hottest(&[]).is_none());
    }

    #[test]
    fn select_hottest_keeps_the_first_zone_on_a_tie() {
        // Equal temperatures must not flap between zones across samples: first in
        // slice order wins, which is stable for a fixed WMI enumeration.
        let samples = [sample("first", 3200), sample("second", 3200)];
        let hottest = select_hottest(&samples).expect("both zones are plausible");
        assert_eq!(hottest.name, "first");
    }

    #[test]
    fn select_hottest_ignores_an_implausible_zone_that_is_numerically_hottest() {
        // The junk zone is the hottest raw value; picking it would drive the
        // throttle from garbage. The real zone must win.
        let samples = [sample("junk", 60_000), sample("real", 3200)];
        let hottest = select_hottest(&samples).expect("the real zone survives");
        assert_eq!(hottest.name, "real");
        assert!(
            (hottest.celsius - 46.85).abs() < 0.05,
            "{}",
            hottest.celsius
        );
    }
}
