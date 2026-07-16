//! Windows temperature sensor via the WDDM `D3DKMT` adapter-perf-data query.
//!
//! Reliable, accurate, no-admin *CPU-die* temperature is not achievable on
//! consumer Windows: every accurate CPU source (Intel DTS/MSR, AMD SMU) sits
//! behind a ring-0 driver, which is the entire reason tools like
//! `LibreHardwareMonitor` exist. The one genuinely no-admin, in-process, both-vendor
//! signal is the **GPU/SoC die temperature the WDDM stack exposes** through
//! `D3DKMTQueryAdapterInfo(KMTQAITYPE_ADAPTERPERFDATA)` — the same sensor Task
//! Manager's GPU-temperature readout uses. On the deployment mini-PCs (AMD Ryzen
//! + Radeon 780M, Intel Core Ultra + Arc/Iris Xe) the iGPU and CPU share a die, so
//!   this is an adequate coarse "is the `SoC` getting hot, throttle now" proxy, which
//!   is exactly what [`super::super::ThermalState`] needs (not per-core telemetry).
//!
//! ## No new dependency
//!
//! The bindings live in the `windows` crate already resolved in the graph; only
//! the `Wdk_Graphics_Direct3D` + `Win32_Foundation` feature modules are new. No
//! admin, no extra process, no kernel driver.
//!
//! ## Degradation (important)
//!
//! Many WDDM drivers report `Temperature == 0` for *integrated* GPUs. That is
//! treated as "unsupported": [`WddmThermalSensor::new`] probes every adapter once
//! and keeps only those returning a nonzero reading; if none do, it returns `None`
//! so the sampler thread is never spawned and the monitor holds its Cool/Schedule
//! no-sensor fallback (a frozen bogus reading would be worse than none). Because
//! of this, the sensor must be **validated per hardware model at deployment**:
//! confirm it reads nonzero under load on each target box during provisioning.
//!
//! Units: `D3DKMT_ADAPTER_PERFDATA::Temperature` is deci-Celsius (`°C = raw / 10`).

// The workspace lint `unsafe_code = "deny"` is lifted for this FFI module (the
// same pattern as `capture::avfoundation` and `providers::leap_native`). Every
// `unsafe` block below is a raw D3DKMT syscall and carries its own SAFETY comment.
#![allow(unsafe_code)]

use std::ffi::c_void;

use windows::Wdk::Graphics::Direct3D::{
    D3DKMTEnumAdapters2, D3DKMTQueryAdapterInfo, D3DKMT_ADAPTERINFO, D3DKMT_ADAPTER_PERFDATA,
    D3DKMT_ENUMADAPTERS2, D3DKMT_QUERYADAPTERINFO, KMTQAITYPE_ADAPTERPERFDATA,
};

use crate::lifecycle::thermal::sensor::TemperatureSensor;

/// Plausible SoC/GPU die temperature band in °C. `0` is special-cased earlier as
/// the "unsupported" sentinel; anything below `1 °C` or above `150 °C` on a
/// powered machine is a bogus/unpopulated channel and is skipped.
const PLAUSIBLE_C: std::ops::RangeInclusive<f32> = 1.0..=150.0;

/// WDDM adapter temperature reader. Holds the handles of the adapters that
/// reported a usable temperature at construction; each read re-queries them and
/// takes the hottest, with no per-sample heap allocation.
pub struct WddmThermalSensor {
    /// Adapter handles (`D3DKMT_ADAPTERINFO::hAdapter`) that reported a nonzero,
    /// in-band temperature when probed in [`Self::new`].
    adapters: Vec<u32>,
}

impl WddmThermalSensor {
    /// Build the sensor, or `None` when no WDDM adapter exposes a usable
    /// temperature (the common integrated-GPU `Temperature == 0` case). A `Some`
    /// means at least one adapter read nonzero at probe time; later reads may still
    /// transiently return `None`, which the sampler tolerates.
    #[must_use]
    pub fn new() -> Option<Self> {
        let adapters: Vec<u32> = enumerate_adapters()
            .into_iter()
            .filter(|&h| read_adapter_celsius(h).is_some())
            .collect();
        if adapters.is_empty() {
            tracing::info!(
                "thermal(windows): no WDDM adapter reports a temperature; \
                 degrading to the Cool/Schedule fallback"
            );
            return None;
        }
        Some(Self { adapters })
    }
}

impl TemperatureSensor for WddmThermalSensor {
    fn read_celsius(&mut self) -> Option<f32> {
        // Hottest reporting adapter. Allocation-free: iterates the small handle
        // Vec and queries each into stack structs.
        let mut hottest: Option<f32> = None;
        for &h in &self.adapters {
            if let Some(celsius) = read_adapter_celsius(h) {
                hottest = Some(hottest.map_or(celsius, |best| best.max(celsius)));
            }
        }
        hottest
    }
}

/// Enumerate the WDDM adapter handles via `D3DKMTEnumAdapters2`.
///
/// Returns an empty `Vec` on any failure (no adapters, or the call errored). The
/// two-call pattern (null buffer for the count, then a sized buffer) allocates the
/// handle `Vec` once; this runs only from [`WddmThermalSensor::new`], never the
/// sampling path.
fn enumerate_adapters() -> Vec<u32> {
    // First call with a null `pAdapters` asks the kernel for the adapter count.
    let mut desc = D3DKMT_ENUMADAPTERS2::default();
    // SAFETY: `desc` is a zeroed, correctly-typed `D3DKMT_ENUMADAPTERS2`; a null
    // `pAdapters` with `NumAdapters == 0` requests only the count per the
    // `D3DKMTEnumAdapters2` contract.
    let status = unsafe { D3DKMTEnumAdapters2(&raw mut desc) };
    // NT_SUCCESS is a non-negative NTSTATUS. `usize::try_from` avoids an `as` cast
    // (u32 always fits usize on our 64-bit targets; 0 on the impossible failure).
    let count = usize::try_from(desc.NumAdapters).unwrap_or(0);
    if status.0 < 0 || count == 0 {
        return Vec::new();
    }

    let mut infos = vec![D3DKMT_ADAPTERINFO::default(); count];
    desc.pAdapters = infos.as_mut_ptr();
    // SAFETY: `pAdapters` points at a buffer of exactly `count`
    // `D3DKMT_ADAPTERINFO` elements (the count the kernel just returned); pointer
    // and length are consistent for the duration of the call.
    let status = unsafe { D3DKMTEnumAdapters2(&raw mut desc) };
    if status.0 < 0 {
        return Vec::new();
    }

    // The second call may fill fewer entries than the first reported.
    let filled = usize::try_from(desc.NumAdapters)
        .unwrap_or(0)
        .min(infos.len());
    infos[..filled].iter().map(|info| info.hAdapter).collect()
}

/// Query one adapter's WDDM perf-data temperature, in °C.
///
/// Returns `None` when the query fails, the driver reports `0` (unsupported iGPU
/// temperature), or the value is outside [`PLAUSIBLE_C`].
fn read_adapter_celsius(h_adapter: u32) -> Option<f32> {
    let mut perf = D3DKMT_ADAPTER_PERFDATA::default();
    let perf_ptr: *mut D3DKMT_ADAPTER_PERFDATA = &raw mut perf;
    let mut query = D3DKMT_QUERYADAPTERINFO {
        hAdapter: h_adapter,
        Type: KMTQAITYPE_ADAPTERPERFDATA,
        pPrivateDriverData: perf_ptr.cast::<c_void>(),
        PrivateDriverDataSize: u32::try_from(std::mem::size_of::<D3DKMT_ADAPTER_PERFDATA>())
            .ok()?,
    };
    // SAFETY: `query` is fully initialized; `pPrivateDriverData` points at `perf`
    // (a live, correctly-sized `D3DKMT_ADAPTER_PERFDATA`) with a matching
    // `PrivateDriverDataSize`, as the `KMTQAITYPE_ADAPTERPERFDATA` query requires.
    // `perf` outlives the call and is not aliased elsewhere.
    let status = unsafe { D3DKMTQueryAdapterInfo(&raw mut query) };
    if status.0 < 0 {
        return None;
    }
    deci_celsius_to_celsius(perf.Temperature)
}

/// Convert a raw WDDM `Temperature` (deci-Celsius; `0` == unsupported) to a
/// plausible °C reading, or `None` when unsupported or out of band.
///
/// Split out from the FFI so it is unit-testable without a GPU.
fn deci_celsius_to_celsius(raw: u32) -> Option<f32> {
    if raw == 0 {
        return None;
    }
    // deci-Celsius: a real reading is well under u16 (0..=1500 for 0..=150 °C);
    // `u16::try_from` then `f32::from` avoids a lossy `as` cast.
    let deci = u16::try_from(raw).ok()?;
    let celsius = f32::from(deci) / 10.0;
    PLAUSIBLE_C.contains(&celsius).then_some(celsius)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construction must never panic on any host (CI may have no GPU or a driver
    /// that reports 0), and any successful read is plausible.
    #[test]
    fn construction_and_read_do_not_panic() {
        if let Some(mut sensor) = WddmThermalSensor::new() {
            if let Some(celsius) = sensor.read_celsius() {
                assert!(
                    PLAUSIBLE_C.contains(&celsius),
                    "implausible temperature reading: {celsius}"
                );
            }
        }
    }

    #[test]
    fn deci_celsius_zero_is_unsupported() {
        assert_eq!(
            deci_celsius_to_celsius(0),
            None,
            "0 is the unsupported sentinel"
        );
    }

    #[test]
    fn deci_celsius_converts_and_bounds() {
        assert_eq!(
            deci_celsius_to_celsius(350),
            Some(35.0),
            "350 deci-C -> 35 C"
        );
        assert_eq!(
            deci_celsius_to_celsius(725),
            Some(72.5),
            "725 deci-C -> 72.5 C"
        );
        // 250 °C is past any real throttle limit -> rejected as bogus.
        assert_eq!(
            deci_celsius_to_celsius(2500),
            None,
            "out-of-band high rejected"
        );
    }
}
