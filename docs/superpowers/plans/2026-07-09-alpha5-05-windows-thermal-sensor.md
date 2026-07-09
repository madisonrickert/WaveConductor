# Windows Thermal Sensor Chain Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a WMI ACPI thermal-zone rung between the existing WDDM D3DKMT sensor and the Schedule fallback so the deployment hardware class (Vega 10 / Radeon 780M APUs, whose iGPU reports no WDDM temperature) stops pinning `ThermalTier::Cool` forever and can actually engage the screensaver present-rate throttle.

**Architecture:** The portable thermal sampler already runs on a dedicated OS thread and calls `platform::create_sensor()` once at startup (inside that thread). On Windows we extend the Windows arm of `create_sensor` into a chain: WDDM `D3DKMTQueryAdapterInfo` (existing, direct GPU-die temp) → **new** WMI `Win32_PerfFormattedData_Counters_ThermalZoneInformation` read from `root\CIMV2` → `None` (Schedule fallback, now logged at WARN). The zone-filtering and hottest-zone selection is a pure, platform-independent function in a new `thermal/wmi_zone.rs` module so it is unit-tested on the macOS/Linux CI runners where the Windows FFI never compiles; only the COM/WMI plumbing lives behind the `target_os = "windows"` gate. `ThermalTier` and its hysteresis are untouched.

**Tech Stack:** Rust, Bevy 0.19, the `windows` crate 0.62 (COM + WMI FFI, already optional behind `thermal-sensor-windows`), `tracing`.

## Author's honesty note (read before implementing)

This plan was written on macOS. **Every `target_os = "windows"` code block in Task 3 is unverified — the author cannot compile or run it.** windows-rs 0.62 API shapes (COM/WMI signatures, VARIANT-reading helpers, constant module paths) can drift from what is written here. Task 1 (the pure selection logic) is where the real TDD lives and it compiles and tests on every platform. Tasks 2–3 are verified only by Madison on a Windows box in Task 4. Task 3's code steps therefore do **not** follow a red-green loop on the implementing agent's machine; they cannot. Where a signature is uncertain it is called out inline as `// UNVERIFIED`. Do not "fix" a compile error you cannot reproduce by guessing — record it for Task 4.

## Global Constraints

Copied from `AGENTS.md` and the program index's Part 1. Every task's requirements implicitly include this section.

- **CI gates**, all of which must pass before a task is complete:
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features --workspace -- -D warnings` (note `--all-targets`, not `--lib`; `--lib` skips the test target and hides `range_plus_one` / `used_underscore_binding` in test code)
  - `cargo nextest run --workspace --all-features` (nextest skips doctests; also run `cargo test --doc --workspace`)
  - `cargo doc --no-deps --workspace --document-private-items` (CI adds `RUSTDOCFLAGS="-D warnings"`; **no `--all-features`** — reproduce it exactly; a **public** item's rustdoc linking to a `pub(crate)` item trips the denied `rustdoc::private_intra_doc_links`, so demote to a plain code span)
  - `cargo deny check`
  - `cargo xtask check-secrets`
- **Clippy is `-D warnings` over `pedantic`, including inside `#[cfg(test)]`.** `Cargo.toml` sets `pedantic`, `unwrap_used`, `expect_used`, `panic`, `as_conversions` at `warn`, and CI escalates all warnings to errors. So in test code too:
  - No `.unwrap()` / `.expect()` / `panic!` / `unreachable!` unless the test module carries an explicit `#[allow(clippy::expect_used, reason = "...")]` (the sanctioned pattern — Plan 01 used it verbatim).
  - `assert_eq!(x.is_some(), true)` → use `assert!(x.is_some())`.
  - `0..(N + 1)` → `0..=N`.
- **No `unwrap()` / `expect()` in non-test code** unless the panic is a documented invariant violation.
- **No `as` casts on numeric types** where `From` / `TryFrom` / `u16::try_from` would work.
- `///` rustdoc on every public item; module-level `//!` on every module root. **Never strip comments during refactors — update stale ones instead.**
- Public API at the top, private helpers at the bottom, tests in a `#[cfg(test)] mod tests` block at the file footer.
- **Platform-specific code lives in `platform/` submodules.** The portable sampler loop stays `cfg`-free.
- **No new dependencies.** The `windows` crate is already optional behind `thermal-sensor-windows`; add feature modules to it, never a new crate.
- **A type with no non-test caller is `dead_code` on the lib target and fails `-D warnings`.** The pure `wmi_zone` module's only production caller is the Windows sensor, which does not compile on macOS/Linux — so it carries a *conditional* `allow(dead_code)` scoped to exactly the targets where the caller is absent (Task 1). This is not a blanket allow; it is correct because the code is genuinely exercised (by tests everywhere, by the sensor on Windows).
- **There are no Windows GPU/FFI tests in CI** (CI runs macOS and Linux). An assertion that only runs on a Windows box we do not have never runs — the same lesson as "no GPU tests in CI." This is *why* the selection logic must be a portable pure function.
- **Commit messages: `git commit -F <file>`, never `-m`.** Backticks in a `-m` string are command-substituted by zsh. **Stage named paths only — never `git add -A`**, then `git show --stat HEAD` to confirm.
- **Do not** put `bevy/dynamic_linking` in any manifest `[features]` table. Use `cargo rund` for manual smoke tests.
- **Branch:** all work lands on `windows-remediation`. Plan 05 is independent of every other alpha.5 plan and touches nobody else's files.

---

### Task 1: Pure, portable WMI zone-selection logic (`wmi_zone.rs`)

The one piece that compiles and is tested on every platform. Convert a raw tenths-of-a-Kelvin zone reading to °C, reject implausible values, take the hottest surviving zone. No `windows` dependency, so its tests run on the macOS and Linux CI runners.

**Files:**
- Create: `crates/wc-core/src/lifecycle/thermal/wmi_zone.rs`
- Modify: `crates/wc-core/src/lifecycle/thermal/mod.rs:47` (add `mod wmi_zone;` after `mod platform;`)

**Interfaces:**
- Consumes: nothing.
- Produces (all `pub(crate)`, consumed by Task 3's Windows sensor):
  - `const PLAUSIBLE_C: std::ops::RangeInclusive<f32>` = `1.0..=150.0`
  - `struct ZoneSample { name: String, deci_kelvin: u32 }`
  - `struct HottestZone { name: String, celsius: f32 }`
  - `fn deci_kelvin_to_celsius(raw: u32) -> Option<f32>`
  - `fn select_hottest(samples: &[ZoneSample]) -> Option<HottestZone>`

- [ ] **Step 1: Write the failing tests**

Create `crates/wc-core/src/lifecycle/thermal/wmi_zone.rs` containing *only* the test module for now, so it fails to compile against the missing items:

```rust
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
    fn select_hottest_takes_the_hottest_plausible_zone() {
        let samples = [
            sample("\\_TZ.TZ00", 3145), // 41.35 °C
            sample("\\_TZ.TZ01", 3345), // 61.35 °C  <- hottest plausible
            sample("\\_TZ.SKIN", 3045), // 31.35 °C
        ];
        let hottest = select_hottest(&samples).expect("a plausible zone exists");
        assert_eq!(hottest.name, "\\_TZ.TZ01");
        assert!((hottest.celsius - 61.35).abs() < 0.05, "{}", hottest.celsius);
    }

    #[test]
    fn select_hottest_skips_implausible_and_keeps_survivors() {
        let samples = [
            sample("cold", 0),      // -273.15 °C, rejected
            sample("impossible", 6000), // 326.85 °C, rejected (above band)
            sample("real", 3200),   // 46.85 °C, the only survivor
        ];
        let hottest = select_hottest(&samples).expect("one plausible zone survives");
        assert_eq!(hottest.name, "real");
        assert!((hottest.celsius - 46.85).abs() < 0.05, "{}", hottest.celsius);
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
}
```

- [ ] **Step 2: Register the module and confirm the tests fail to compile**

In `crates/wc-core/src/lifecycle/thermal/mod.rs`, immediately after the `mod platform;` line (currently line 47), add:

```rust
mod wmi_zone;
```

Run: `cargo test -p wc-core --lib lifecycle::thermal::wmi_zone 2>&1 | head -20`

Expected: FAIL to compile, `cannot find type ZoneSample` / `cannot find function deci_kelvin_to_celsius in this scope`.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/wc-core/src/lifecycle/thermal/wmi_zone.rs`, above the test module:

```rust
//! Pure, platform-independent selection logic for the Windows WMI ACPI
//! thermal-zone rung of the thermal sensor chain (Plan 05, alpha.5).
//!
//! ## Why this is its own portable module
//!
//! The WMI query itself is Windows-only COM FFI and lives in
//! [`super::platform`]'s `windows_wmi` submodule, compiled only under
//! `all(target_os = "windows", feature = "thermal-sensor-windows")`. But the
//! *decisions* it makes — convert each raw zone reading from tenths of a Kelvin
//! to Celsius, reject anything implausible, and take the hottest surviving zone
//! — are arithmetic. Keeping them here, unconditionally compiled and free of any
//! `windows` dependency, is what lets them be unit-tested on the macOS and Linux
//! CI runners, where the Windows FFI never builds. The program index's "there
//! are no GPU tests in CI" lesson applies equally to platform FFI: an assertion
//! that only runs on a Windows box we do not own in CI never runs.
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

// The only production caller of this module is the Windows WMI sensor, compiled
// solely for `all(target_os = "windows", feature = "thermal-sensor-windows")`.
// On every other target — including CI's macOS/Linux `--all-features` builds,
// where the sensor's own `target_os = "windows"` gate keeps it out — these items
// have no non-test caller and would trip `dead_code` under `-D warnings`. They
// are not dead: this file's tests exercise them on every platform, and the
// Windows sensor consumes them in production. A conditional allow, scoped to
// exactly the targets where the caller is absent, is the correct expression of
// that.
#![cfg_attr(
    not(all(target_os = "windows", feature = "thermal-sensor-windows")),
    allow(dead_code)
)]

use std::ops::RangeInclusive;

/// Plausible SoC / package / chassis-skin temperature band in °C. A reading
/// outside this, after conversion, is a bogus or unpopulated channel and is
/// skipped. Matches the WDDM sensor's band (`platform::windows`'s `PLAUSIBLE_C`):
/// a powered machine never reads below 1 °C, and above 150 °C is past any
/// throttle limit.
pub(crate) const PLAUSIBLE_C: RangeInclusive<f32> = 1.0..=150.0;

/// One raw ACPI thermal-zone reading harvested from WMI, before unit conversion
/// or plausibility filtering.
pub(crate) struct ZoneSample {
    /// Zone instance name (the `Name` column, e.g. `\_TZ.TZ00`). Carried only for
    /// the provenance log line; [`select_hottest`] ignores it when ranking.
    pub name: String,
    /// Raw temperature counter in tenths of a Kelvin. See the module unit
    /// contract.
    pub deci_kelvin: u32,
}

/// The hottest plausible zone selected from a batch of [`ZoneSample`]s.
pub(crate) struct HottestZone {
    /// The winning zone's instance name, for the provenance log.
    pub name: String,
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
/// also present. Allocates only the winner's name (one `String` clone per call,
/// at the sampler's multi-second cadence — never on the render or audio hot
/// path).
pub(crate) fn select_hottest(samples: &[ZoneSample]) -> Option<HottestZone> {
    let mut best: Option<HottestZone> = None;
    for sample in samples {
        let Some(celsius) = deci_kelvin_to_celsius(sample.deci_kelvin) else {
            continue;
        };
        if best.as_ref().is_none_or(|current| celsius > current.celsius) {
            best = Some(HottestZone {
                name: sample.name.clone(),
                celsius,
            });
        }
    }
    best
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wc-core --lib lifecycle::thermal::wmi_zone`

Expected: PASS, 7 tests.

- [ ] **Step 5: Run the scoped gate and commit**

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --lib lifecycle::thermal::wmi_zone
git add crates/wc-core/src/lifecycle/thermal/wmi_zone.rs crates/wc-core/src/lifecycle/thermal/mod.rs
git commit -F <message-file>
```

Commit message (write to a file, then `git commit -F`; it contains backticks):

```
feat(thermal): add pure WMI thermal-zone selection logic

The Windows WMI ACPI thermal-zone rung (Plan 05) needs to convert raw
tenths-of-a-Kelvin zone readings to Celsius, reject implausible values,
and pick the hottest surviving zone. That logic is arithmetic with no
`windows` dependency, so it lives in a portable `wmi_zone` module and is
unit-tested on every CI runner -- including the macOS and Linux boxes where
the Windows COM/WMI FFI never compiles. Only the query plumbing will sit
behind the `target_os = "windows"` gate (next task).

The module carries a conditional `allow(dead_code)` scoped to the targets
where its sole production caller (the Windows sensor) is absent, so the
lib target stays clean under `-D warnings` while the tests still run.
```

---

### Task 2: Add the WMI/COM feature modules to the `windows` crate

No new dependency — the `windows` crate is already declared, optional, gated by `thermal-sensor-windows`. We add three feature modules it needs: COM bring-up, the WMI client interfaces, and the VARIANT-reading helpers.

**Files:**
- Modify: `crates/wc-core/Cargo.toml:185-188` (the Windows-target `windows` dependency's `features` list)

**Interfaces:**
- Consumes: nothing.
- Produces: `Win32_System_Com`, `Win32_System_Wmi`, `Win32_System_Variant` available to Task 3.

- [ ] **Step 1: Extend the features list**

In `crates/wc-core/Cargo.toml`, the dependency currently reads (lines 185-188):

```toml
windows = { version = "0.62", optional = true, features = [
    "Wdk_Graphics_Direct3D", # D3DKMTEnumAdapters2 / D3DKMTQueryAdapterInfo + structs
    "Win32_Foundation",      # NTSTATUS, LUID, BOOL referenced by those structs
] }
```

Replace it with:

```toml
windows = { version = "0.62", optional = true, features = [
    "Wdk_Graphics_Direct3D", # D3DKMTEnumAdapters2 / D3DKMTQueryAdapterInfo + structs (WDDM rung)
    "Win32_Foundation",      # NTSTATUS, LUID, BOOL referenced by those structs
    # WMI ACPI thermal-zone rung (Plan 05):
    "Win32_System_Com",     # CoInitializeEx/CoUninitialize/CoCreateInstance/CoTaskMemFree, CLSCTX, COINIT
    "Win32_System_Wmi",     # IWbemLocator/IWbemServices/IWbemClassObject/IEnumWbemClassObject, WBEM_FLAG_*
    "Win32_System_Variant", # VARIANT + VariantToUInt32/VariantToStringAlt readers
] }
```

- [ ] **Step 2: Confirm the manifest still parses**

On macOS the `windows` crate is under `[target.'cfg(target_os = "windows")'.dependencies]`, so it is not resolved for the host build and cannot be compiled here — this step only confirms the manifest is well-formed.

```bash
cargo fmt --all -- --check
cargo metadata --format-version 1 --no-deps >/dev/null && echo "manifest OK"
```

Expected: `manifest OK`, no TOML parse error. (A full build of these features happens on Windows in Task 4. **Do not** attempt `cargo build --target x86_64-pc-windows-msvc` on macOS — the MSVC toolchain and Windows SDK are absent.)

- [ ] **Step 3: Commit**

```bash
git add crates/wc-core/Cargo.toml
git commit -F <message-file>
```

Commit message (contains backticks; `git commit -F`):

```
build(thermal): add Win32 COM/WMI/Variant features to the windows crate

Plan 05's WMI ACPI thermal-zone rung needs COM bring-up, the WMI client
interfaces, and the VARIANT-reading helpers. These are feature modules of
the `windows` crate already declared optional behind `thermal-sensor-windows`
-- not a new dependency. The rung's code lands in the next task.
```

---

### Task 3: Windows WMI thermal-zone sensor and chain wiring

**UNVERIFIED ON THE IMPLEMENTING AGENT'S MACHINE.** Every file this task touches is `target_os = "windows"`-gated and does not compile on macOS/Linux. The implementing agent writes the code, runs `cargo fmt` and the standard non-Windows gate (which confirms nothing *else* broke, because these files are simply absent from that build), and then **stops**. The compile-and-fix loop is Task 4, on Madison's Windows box. This is unavoidable: there is no Windows compiler here.

**Files:**
- Create: `crates/wc-core/src/lifecycle/thermal/platform/windows_wmi.rs`
- Modify: `crates/wc-core/src/lifecycle/thermal/platform/mod.rs` (register `mod windows_wmi;` at `:47`; rewrite the Windows `create_sensor` at `:63-69` into the chain; update the module-doc Windows bullet at `:18-23`)
- Modify: `crates/wc-core/src/lifecycle/thermal/platform/windows.rs` (demote the "no WDDM adapter" log at `:72-76` from INFO to DEBUG and reword; update the stale "Degradation" module-doc paragraph at `:20-30`)

**Interfaces:**
- Consumes: `wmi_zone::{select_hottest, HottestZone, ZoneSample}` (Task 1); the `windows` features (Task 2); `sensor::TemperatureSensor`.
- Produces: `WmiThermalZoneSensor` implementing `TemperatureSensor`; a Windows `create_sensor` that chains WDDM → WMI → WARN+`None`; a `WC_THERMAL_FORCE_WMI` env hook to exercise the WMI rung on hardware where WDDM does report a temperature.

- [ ] **Step 1: Create the WMI sensor**

Create `crates/wc-core/src/lifecycle/thermal/platform/windows_wmi.rs` with the following. Every `// UNVERIFIED` marks a windows-rs 0.62 signature or module path the author could not compile-check; Task 4 confirms or corrects each.

```rust
//! Windows WMI ACPI thermal-zone temperature sensor — the second rung of the
//! thermal sensor chain (Plan 05, alpha.5).
//!
//! ## Role
//!
//! The first rung ([`super::windows`]'s WDDM `D3DKMT` sensor) returns the GPU/SoC
//! die temperature, but many integrated GPUs — including both deployment
//! candidates, the Vega 10 and the Radeon 780M — report `Temperature == 0`
//! there. Precisely those machines expose a usable *package* temperature through
//! the ACPI thermal zones surfaced by WMI. On a shared-die APU (one die, one
//! power budget, one thermal budget for CPU cores and iGPU alike) that package
//! temperature is a faithful proxy for the GPU the renderer is loading, which is
//! all [`super::super::ThermalState`] needs.
//!
//! ## What it reads
//!
//! `SELECT Name, HighPrecisionTemperature FROM
//! Win32_PerfFormattedData_Counters_ThermalZoneInformation` in the `root\CIMV2`
//! namespace, which is generally readable **without elevation** — in preference
//! to `MSAcpi_ThermalZoneTemperature` in `root\WMI`, which frequently requires
//! admin. `HighPrecisionTemperature` is in tenths of a Kelvin; the conversion and
//! the hottest-plausible-zone selection live in the portable, unit-tested
//! [`super::super::wmi_zone`] module. **The exact class, property, and unit must
//! be confirmed on the target box** (Plan 05, Task 4) — WMI unit conventions are
//! not uniform, and the perf class also carries a whole-Kelvin `Temperature`.
//!
//! ## OEM caveat (recorded deliberately)
//!
//! ACPI zone semantics are OEM-defined. Some boxes report a chipset or
//! chassis-skin temperature rather than the die, so a threshold tuned on one
//! machine does not transfer to another. Skin and package temperatures also lag
//! die temperature by tens of seconds. Both are acceptable here because the only
//! thermal lever is an attract-mode present rate, not frame-level control.
//!
//! ## Threading and COM
//!
//! This reader is constructed and used **only** on the background thermal
//! sampler thread ([`super::super::sensor::spawn_sampler`]), never the Bevy
//! main/render thread. COM is apartment-affine, so the sensor initialises COM on
//! that thread in [`WmiThermalZoneSensor::new`], holds the `IWbemServices` proxy
//! for the thread's life, and balances the init with `CoUninitialize` on drop
//! (fields drop in declaration order, so the proxy is released before COM is torn
//! down). The trait does not require `Send`; the thread-affine COM handles never
//! leave this thread.
//!
//! ## Allocation
//!
//! A WMI `ExecQuery` inherently allocates (BSTRs, an enumerator, per-row VARIANTs
//! and, for the zone name, a `CoTaskMem` string). That runs at the sampler's
//! multi-second cadence on a dedicated thread — never the render or audio hot
//! path — so per AGENTS.md this dependency-forced cost is documented rather than
//! eliminated. The one buffer we own, the `Vec<ZoneSample>` scratch, is reused
//! (`clear()` each sample) so at least the outer allocation is amortised. Tighter
//! elimination is a profiling-gated follow-up, not warranted for a 3 s cadence.

// The workspace lint `unsafe_code = "deny"` is lifted for this FFI module, the
// same pattern as `platform::windows` (WDDM) and `capture::avfoundation`. Every
// `unsafe` block below is a COM/WMI syscall and carries its own SAFETY comment.
#![allow(unsafe_code)]

use windows::core::{w, BSTR, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::{VariantToStringAlt, VariantToUInt32, VARIANT};
use windows::Win32::System::Wmi::{
    IEnumWbemClassObject, IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator,
    WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY,
};

use crate::lifecycle::thermal::wmi_zone::{select_hottest, ZoneSample};

/// `IEnumWbemClassObject::Next` timeout per batch, in milliseconds. Finite (not
/// `WBEM_INFINITE`) so a wedged WMI provider can never permanently stall the
/// sampler thread; a timeout simply ends the enumeration with whatever was
/// gathered.
const NEXT_TIMEOUT_MS: i32 = 2_000;

/// Defensive cap on zones harvested per sample. Real machines expose a handful;
/// this bounds the scratch `Vec` if a provider streams pathologically.
const MAX_ZONES: usize = 32;

/// Emit a provenance log line every N samples. At the 3 s sampler cadence that
/// is roughly once a minute — frequent enough that the next soak log carries the
/// data needed to tune thresholds later, sparse enough not to flood it.
const PROVENANCE_EVERY: u64 = 20;

/// WMI ACPI thermal-zone temperature reader. Holds a live `root\CIMV2` services
/// proxy and re-queries it each sample, taking the hottest plausible zone.
pub struct WmiThermalZoneSensor {
    /// Live WMI services proxy, thread-affine to the sampler thread. Declared
    /// first so it is released (COM `Release`) before [`ComGuard`] runs
    /// `CoUninitialize`.
    services: IWbemServices,
    /// Reused per-sample harvest buffer; `clear()`ed each read so the outer
    /// allocation is not repeated (per-zone `String`/VARIANT allocations are the
    /// documented WMI-forced residual cost).
    scratch: Vec<ZoneSample>,
    /// Sample counter, for periodic provenance logging.
    samples: u64,
    /// RAII balance for the per-thread `CoInitializeEx`. Declared last so it
    /// drops after `services`.
    _com: ComGuard,
}

impl WmiThermalZoneSensor {
    /// Initialise COM on this (sampler) thread, connect to `root\CIMV2`, and
    /// probe once. Returns `None` — degrading to the next rung / Schedule — when
    /// COM init fails, the connection fails, or no zone reads plausibly at probe
    /// time (a frozen bogus reading would be worse than none).
    #[must_use]
    pub fn new() -> Option<Self> {
        // SAFETY: called on our own freshly spawned sampler thread, which has no
        // prior COM apartment. COINIT_MULTITHREADED joins the MTA; this thread
        // never pumps a message loop. Returns S_OK on a fresh thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }; // UNVERIFIED: return type is HRESULT in 0.62
        if hr.is_err() {
            tracing::debug!(?hr, "thermal(windows/wmi): CoInitializeEx failed");
            return None;
        }
        // From here every early return must CoUninitialize; the guard does that.
        let guard = ComGuard;

        let services = connect_cimv2()?; // guard drops -> CoUninitialize on None

        // Probe: harvest once and require at least one plausible zone now, so the
        // chain falls through to Schedule rather than installing a dead reader.
        let mut probe = Vec::new();
        if harvest_zones(&services, &mut probe).is_none() || select_hottest(&probe).is_none() {
            tracing::debug!(
                "thermal(windows/wmi): connected but no plausible thermal zone at probe; \
                 degrading"
            );
            return None; // guard drops -> CoUninitialize
        }

        Some(Self {
            services,
            scratch: Vec::new(),
            samples: 0,
            _com: guard,
        })
    }
}

impl super::super::sensor::TemperatureSensor for WmiThermalZoneSensor {
    /// Re-query the thermal zones and return the hottest plausible one in °C, or
    /// `None` this sample on any WMI error (the sampler tolerates gaps).
    fn read_celsius(&mut self) -> Option<f32> {
        harvest_zones(&self.services, &mut self.scratch)?;
        let hottest = select_hottest(&self.scratch)?;

        let count = self.samples;
        self.samples = self.samples.wrapping_add(1);
        if count % PROVENANCE_EVERY == 0 {
            tracing::info!(
                source = "wmi-zone",
                zone = ?hottest.name,
                // One decimal: the raw f32 tail (e.g. 61.349998) adds no signal.
                temp_c = (hottest.celsius * 10.0).round() / 10.0,
                "thermal(windows): WMI ACPI thermal-zone reading"
            );
        }
        Some(hottest.celsius)
    }
}

/// Connect to the local `root\CIMV2` WMI namespace, returning a services proxy.
///
/// No `CoInitializeSecurity` / `CoSetProxyBlanket`: for an in-process **local**
/// query the default proxy blanket is sufficient, and `CoInitializeSecurity` is
/// process-global (calling it from a library worker thread after the app has
/// started risks `RPC_E_TOO_LATE`). See Plan 05, Task 4 — if the query returns
/// `WBEM_E_ACCESS_DENIED`, the fix is to add the `Win32_System_Rpc` feature and a
/// `CoSetProxyBlanket` call (sketch in the plan's appendix).
fn connect_cimv2() -> Option<IWbemServices> {
    // SAFETY: standard WMI locator bring-up. `CoCreateInstance` yields a checked
    // IWbemLocator; the BSTRs live for the ConnectServer call; the result is
    // checked before use.
    unsafe {
        let locator: IWbemLocator =
            CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).ok()?; // UNVERIFIED: CoCreateInstance generic + WbemLocator CLSID path
        locator
            .ConnectServer(
                &BSTR::from("root\\CIMV2"),
                &BSTR::new(), // user: current
                &BSTR::new(), // password: current
                &BSTR::new(), // locale: current
                0,            // security flags
                &BSTR::new(), // authority
                None,         // context
            )
            .ok() // UNVERIFIED: ConnectServer param `Param<BSTR>` coercions + lsecurityflags: i32
    }
}

/// Harvest every thermal zone's name + raw tenths-of-a-Kelvin temperature into
/// `out` (cleared first). `None` when the query itself fails; `Some(())` with a
/// possibly-empty `out` otherwise.
fn harvest_zones(services: &IWbemServices, out: &mut Vec<ZoneSample>) -> Option<()> {
    out.clear();
    // SAFETY: `services` is a live proxy; the WQL BSTRs live for ExecQuery; the
    // `row`/`returned` out-params are valid; each VARIANT is initialised by Get.
    unsafe {
        let enumerator: IEnumWbemClassObject = services
            .ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from(
                    "SELECT Name, HighPrecisionTemperature \
                     FROM Win32_PerfFormattedData_Counters_ThermalZoneInformation",
                ),
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            )
            .ok()?; // UNVERIFIED: ExecQuery flags type (WBEM_GENERIC_FLAG_TYPE) + pctx None

        loop {
            let mut row: [Option<IWbemClassObject>; 1] = [None];
            let mut returned: u32 = 0;
            // Next returns an HRESULT; WBEM_S_FALSE (success) at end-of-enum.
            if enumerator
                .Next(NEXT_TIMEOUT_MS, &mut row, &mut returned)
                .is_err()
            {
                break; // errored: return what we have
            }
            if returned == 0 {
                break; // WBEM_S_FALSE / timeout: enumeration complete
            }
            let Some(obj) = row[0].take() else {
                break;
            };
            let Some(deci_kelvin) = get_u32(&obj, w!("HighPrecisionTemperature")) else {
                continue; // no temperature on this row
            };
            let name = get_string(&obj, w!("Name")).unwrap_or_else(|| "?".to_owned());
            out.push(ZoneSample { name, deci_kelvin });
            if out.len() >= MAX_ZONES {
                break;
            }
        }
    }
    Some(())
}

/// Read a numeric WMI property as `u32` (VARIANT coercion handles VT_I4/VT_UI4).
fn get_u32(obj: &IWbemClassObject, name: PCWSTR) -> Option<u32> {
    let mut value = VARIANT::default();
    // SAFETY: `value` is a live, default-initialised VARIANT that Get fills or
    // errors on; VariantToUInt32 reads that initialised VARIANT by const pointer.
    unsafe {
        obj.Get(name, 0, &mut value, None, None).ok()?; // UNVERIFIED: Get signature (PCWSTR, i32, *mut VARIANT, Option, Option)
        VariantToUInt32(&value).ok() // UNVERIFIED: VariantToUInt32(*const VARIANT) -> Result<u32>
    }
}

/// Read a string WMI property as an owned `String`, best-effort (`None` on any
/// failure so a missing name never fails the whole read).
fn get_string(obj: &IWbemClassObject, name: PCWSTR) -> Option<String> {
    let mut value = VARIANT::default();
    // SAFETY: as `get_u32`. VariantToStringAlt returns a CoTaskMem-allocated
    // PWSTR we own and free with CoTaskMemFree after copying it into a String.
    unsafe {
        obj.Get(name, 0, &mut value, None, None).ok()?;
        let pwstr = VariantToStringAlt(&value).ok()?; // UNVERIFIED: VariantToStringAlt(*const VARIANT) -> Result<PWSTR>
        let owned = pwstr.to_string().ok();
        CoTaskMemFree(Some(pwstr.0.cast())); // UNVERIFIED: PWSTR.0 is *mut u16; free as *const c_void
        owned
    }
}

/// RAII balance for the per-thread `CoInitializeEx` in [`WmiThermalZoneSensor::new`].
struct ComGuard;

impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: balances the CoInitializeEx in `WmiThermalZoneSensor::new` on
        // the same (sampler) thread. Runs after `services` has been released,
        // because struct fields drop in declaration order and `_com` is last.
        unsafe { CoUninitialize() };
    }
}
```

> **No `#[cfg(test)] mod tests` here on purpose.** Every testable decision this file makes is in the pure `wmi_zone` module (Task 1), which is tested on all CI runners. A test in this file would be gated `target_os = "windows"` and never run in CI — the module-index "no tests that only run on a box we don't have" rule.

- [ ] **Step 2: Register the module and rewrite the Windows `create_sensor` chain**

In `crates/wc-core/src/lifecycle/thermal/platform/mod.rs`, after the existing `mod windows;` (line 47), add the sibling declaration:

```rust
#[cfg(all(feature = "thermal-sensor-windows", target_os = "windows"))]
mod windows_wmi;
```

Then replace the entire Windows `create_sensor` (currently lines 63-69):

```rust
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
```

with the two-rung chain:

```rust
/// Windows + `thermal-sensor-windows`: the thermal sensor chain. WDDM `D3DKMT`
/// GPU-die temperature first; on the integrated-GPU class that reports 0 there
/// (Vega 10, Radeon 780M), fall through to the WMI ACPI thermal-zone rung, whose
/// package temperature is populated precisely where WDDM is not. `None` only when
/// neither exposes a temperature — logged at WARN, because a silent fallback here
/// is exactly how alpha.4 shipped thermally blind.
///
/// Setting the `WC_THERMAL_FORCE_WMI` environment variable skips the WDDM rung,
/// so the WMI path can be exercised on hardware where WDDM *does* report (e.g. a
/// discrete GPU) — see Plan 05, Task 4.
#[cfg(all(feature = "thermal-sensor-windows", target_os = "windows"))]
#[must_use]
pub fn create_sensor() -> Option<Box<dyn TemperatureSensor>> {
    // Rung 1: direct GPU/SoC die temperature via WDDM. Skipped when the operator
    // forces the WMI rung for validation.
    let force_wmi = std::env::var_os("WC_THERMAL_FORCE_WMI").is_some();
    if force_wmi {
        tracing::info!("thermal(windows): WC_THERMAL_FORCE_WMI set; skipping the WDDM rung");
    } else if let Some(sensor) = windows::WddmThermalSensor::new() {
        let boxed: Box<dyn TemperatureSensor> = Box::new(sensor);
        return Some(boxed);
    }

    // Rung 2: package/skin temperature via WMI ACPI thermal zones.
    if let Some(sensor) = windows_wmi::WmiThermalZoneSensor::new() {
        let boxed: Box<dyn TemperatureSensor> = Box::new(sensor);
        return Some(boxed);
    }

    // Both rungs failed: no temperature signal at all. WARN, not INFO — the only
    // thermal lever, the screensaver present-rate throttle, cannot engage, so
    // every tier runs as Cool. This is the failure that went unnoticed in
    // alpha.4.
    tracing::warn!(
        "thermal(windows): no temperature source (WDDM D3DKMT reports 0 and no readable \
         WMI ACPI thermal zone); the app is thermally blind and holds the Cool/Schedule \
         fallback -- the screensaver present-rate throttle will NOT engage. Verify the \
         WMI thermal-zone rung on this hardware."
    );
    None
}
```

Finally, update the module-doc Windows bullet (currently lines 18-23) so it describes the chain rather than WDDM-only. Replace:

```rust
//! - **Windows** (`thermal-sensor-windows`) → `windows::WddmThermalSensor` reads
//!   the iGPU/SoC die temperature via the no-admin WDDM `D3DKMT` adapter-perf-data
//!   query as a coarse throttle proxy (reliable no-admin CPU-die temps are not
//!   available on consumer Windows). Without the feature, Windows falls to the
//!   `native` reader, whose sysfs paths are absent, so it returns `None`.
```

with:

```rust
//! - **Windows** (`thermal-sensor-windows`) → a two-rung chain. First
//!   `windows::WddmThermalSensor` reads the iGPU/SoC die temperature via the
//!   no-admin WDDM `D3DKMT` adapter-perf-data query. Many integrated GPUs report
//!   0 there; those fall through to `windows_wmi::WmiThermalZoneSensor`, which
//!   reads the ACPI package temperature via a no-admin WMI query against
//!   `root\CIMV2` (a faithful proxy on a shared-die APU). Only when neither rung
//!   reports does Windows return `None` (logged at WARN). Without the feature,
//!   Windows falls to the `native` reader, whose sysfs paths are absent, so it
//!   returns `None`.
```

- [ ] **Step 3: Demote the WDDM "no adapter" log and fix its stale module doc**

In `crates/wc-core/src/lifecycle/thermal/platform/windows.rs`, the "no WDDM adapter" branch (currently lines 71-77) logs at INFO and claims it degrades straight to Schedule. That is no longer true — it now falls through to the WMI rung. Replace the `tracing::info!(...)` call (lines 72-75) with:

```rust
            tracing::debug!(
                "thermal(windows): no WDDM adapter reports a temperature (common on \
                 integrated GPUs); falling through to the WMI ACPI thermal-zone rung"
            );
```

(The surrounding `if adapters.is_empty() { ... return None; }` is unchanged — `None` from the WDDM sensor is what the chain in `mod.rs` catches to try WMI.)

Then update the stale sentence in the "## Degradation (important)" module-doc paragraph (lines 20-30). It currently reads:

```rust
//! so the sampler thread is never spawned and the monitor holds its Cool/Schedule
//! no-sensor fallback (a frozen bogus reading would be worse than none). Because
```

Replace those two lines with:

```rust
//! so [`super::mod`]'s `create_sensor` falls through to the WMI ACPI
//! thermal-zone rung (`windows_wmi`), and only if *that* also finds nothing does
//! the monitor hold its Cool/Schedule no-sensor fallback (a frozen bogus reading
//! would be worse than none). Because
```

> Rustdoc note: `[`super::mod`]` is not a valid intra-doc path. Write it as a plain code span ``` `super`'s `create_sensor` ``` to avoid a broken-link `-D warnings` failure. Concretely, use: `//! so `super`'s `create_sensor` falls through to the WMI ACPI`.

- [ ] **Step 4: Non-Windows gate (all the implementing agent can verify)**

These files are `target_os = "windows"`-gated and absent from the macOS/Linux build, so this only confirms nothing *else* regressed and the formatting is clean. It does **not** prove the Windows code compiles — that is Task 4.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
```

Expected: all pass. The `wmi_zone` tests from Task 1 run and pass; nothing references `windows_wmi` on this target. If clippy flags `dead_code` on any `wmi_zone` item, the conditional allow's cfg predicate is wrong — re-check it against Task 1, do not add a blanket allow.

- [ ] **Step 5: Commit**

```bash
git add \
  crates/wc-core/src/lifecycle/thermal/platform/windows_wmi.rs \
  crates/wc-core/src/lifecycle/thermal/platform/mod.rs \
  crates/wc-core/src/lifecycle/thermal/platform/windows.rs
git show --stat HEAD  # confirm only these three paths (after the commit below)
git commit -F <message-file>
```

Commit message (contains backticks; `git commit -F`):

```
feat(thermal): add WMI ACPI thermal-zone rung for Windows APUs

The WDDM D3DKMT sensor reports Temperature == 0 on the integrated GPUs the
installation deploys to (Vega 10, Radeon 780M), so the tier pinned Cool
forever and the screensaver present-rate throttle -- the only thermal lever
-- never engaged. Worse, that failure logged at INFO, so nobody noticed.

Add a second rung: a no-admin WMI query of the ACPI thermal zones in
root\CIMV2, whose package temperature is a faithful proxy on a shared-die
APU and is populated precisely where WDDM returns 0. The chain is now
WDDM -> WMI -> None, and the terminal "no sensor" case logs at WARN. The
zone conversion and hottest-zone selection are the pure, CI-tested wmi_zone
module; only the COM/WMI plumbing is Windows-gated. ThermalTier and its
hysteresis are unchanged; no GpuTimeProxy rung is built.

WC_THERMAL_FORCE_WMI skips the WDDM rung to exercise WMI on discrete GPUs.

UNVERIFIED on macOS: the windows-rs FFI compiles only on Windows; validated
on hardware in the Plan 05 verification task.
```

> **STOP here if you are the implementing agent on macOS/Linux.** Task 4 is a human-operated procedure on a Windows machine. Report Tasks 1–3 done and hand off.

---

### Task 4: Windows hardware verification (human-operated, Madison)

Tasks 1–3 wrote code the author could not compile. This task compiles and validates it on Windows, in two stages: first on Madison's **RX 6900 XT** dev box (a discrete RDNA2 GPU that likely *does* report a WDDM temperature, so the WMI rung must be forced), then on the field tester's **Vega 10** box (where WMI is the rung that actually engages). These are different machines: the 6900 XT is NOT the tester's iGPU, so a WDDM temperature may well be reported there and the WMI rung would never engage on its own — hence `WC_THERMAL_FORCE_WMI`.

- [ ] **Step 1: Confirm the WMI class, property, and unit empirically (before trusting any reading)**

The whole conversion in `wmi_zone::deci_kelvin_to_celsius` rests on `HighPrecisionTemperature` being in tenths of a Kelvin. Confirm it against raw values in an elevated-or-not PowerShell:

```powershell
Get-CimInstance -Namespace root/cimv2 -ClassName Win32_PerfFormattedData_Counters_ThermalZoneInformation |
    Select-Object Name, Temperature, HighPrecisionTemperature
```

Sanity: for a zone at ~40 °C expect `HighPrecisionTemperature ≈ 3131` (tenths of K) and `Temperature ≈ 313` (whole K). If `HighPrecisionTemperature` is instead ~313 or ~40, the unit assumption is wrong — **stop and correct `deci_kelvin_to_celsius` before proceeding** (a whole-Kelvin source drops the `/10`; a Celsius source drops the `- 273.15`). Record the observed raw values in the commit or a runbook. If the class returns nothing without elevation, note it and compare the `root/wmi` fallback:

```powershell
Get-CimInstance -Namespace root/wmi -ClassName MSAcpi_ThermalZoneTemperature |
    Select-Object InstanceName, CurrentTemperature
```

(`MSAcpi_ThermalZoneTemperature::CurrentTemperature` is also tenths of a Kelvin — same conversion — but usually needs admin; the plan prefers the `root\CIMV2` perf class for that reason.)

- [ ] **Step 2: Build the crate for Windows and fix any windows-rs signature drift**

On the Windows box, with `thermal-sensor-windows` in the feature set:

```powershell
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
```

Every `// UNVERIFIED` marker in `windows_wmi.rs` is a candidate failure point. Likely corrections if the build fails, in decreasing order of probability:
- `VARIANT` import path: try `windows::Win32::System::Variant::VARIANT`, else `windows::core::VARIANT`.
- `VariantToUInt32` / `VariantToStringAlt` argument form: `&value` may need `&value as *const _` or a `Param` wrapper.
- `IWbemServices::ExecQuery` flags: the `WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY` union type may need an explicit `WBEM_GENERIC_FLAG_TYPE`.
- `IWbemClassObject::Get` trailing `Option` args and `CoTaskMemFree`'s pointer form.
- `CoInitializeEx` return: `.is_err()` on `HRESULT` — if it returns `Result<()>` in this build, use `.is_err()` on the `Result` instead.

Fix signatures to match the compiler; do **not** change the logic or the chain. Keep the `unsafe` SAFETY comments accurate. Re-run until clippy is clean, then run the full gate (`fmt`, `clippy`, `nextest`, `test --doc`, `doc`, `deny check`, `xtask check-secrets`). Amend the Task 3 commit or add a fixup commit with the corrections.

- [ ] **Step 3: Exercise the WMI rung on the RX 6900 XT (forced)**

```powershell
$env:WC_THERMAL_FORCE_WMI = "1"; cargo run -p waveconductor --features thermal-sensor-windows
```

Watch the log for the WARN vs. the provenance line:
- Expected on success: `thermal(windows): WC_THERMAL_FORCE_WMI set; skipping the WDDM rung` followed within a few seconds by `thermal(windows): WMI ACPI thermal-zone reading source="wmi-zone" zone="..." temp_c=NN.N`.
- If instead you see the `no temperature source ... thermally blind` WARN, the WMI query returned nothing: check for `WBEM_E_ACCESS_DENIED` (needs the `CoSetProxyBlanket` fallback in the appendix) or an empty zone set (the perf class may be unpopulated on this box — try the `MSAcpi_ThermalZoneTemperature` variant per the appendix).

Confirm the reported temperature tracks reality: put the machine under GPU load (run a sketch with hand tracking + audio) and confirm `temp_c` rises over a minute or two.

- [ ] **Step 4: Field-box validation on the Vega 10 (unforced)**

On the tester's box, run **without** `WC_THERMAL_FORCE_WMI` (normal chain). Because the Vega 10 reports 0 via WDDM, the WMI rung should engage automatically. Confirm the provenance line appears and no thermal-blind WARN. This is the machine the whole plan exists for.

- [ ] **Step 5: First-soak watch (behaviour-change flag)**

This box previously pinned `Cool` forever; it will now classify real tiers against the **placeholder** `ThermalThresholds` (`enter_warm 75 / enter_hot 90 °C`), which alpha.5 does not tune. During the first 8-hour soak, watch that:
- the tier does not sit at `Hot` indefinitely (would mean the placeholder bands are far too low for whatever zone won — e.g. a hot chipset zone), and
- the present-rate throttle engages and releases sensibly rather than flapping.

Capture the periodic `source="wmi-zone"` provenance lines from the soak log — they are the raw data that makes threshold tuning possible later (out of scope here). If the winning `zone` name suggests a skin/chassis sensor rather than die, note it: thresholds tuned against it will not transfer to the other deployment box.

- [ ] **Step 6: Final commit (only if Step 2 required fixups not already committed)**

```bash
git add crates/wc-core/src/lifecycle/thermal/platform/windows_wmi.rs
git show --stat HEAD
git commit -F <message-file>
```

---

## Appendix: `CoSetProxyBlanket` fallback (only if WMI returns access-denied)

If Step 3/4 shows `WBEM_E_ACCESS_DENIED` (or `E_ACCESSDENIED 0x80070005`) from `ExecQuery`/`Next`, the local proxy needs an explicit security blanket. This adds one feature and one call — apply it only if needed:

1. Add `"Win32_System_Rpc"` to the `windows` features in `Cargo.toml` (for `RPC_C_AUTHN_WINNT`, `RPC_C_AUTHZ_NONE`).
2. In `connect_cimv2`, after a successful `ConnectServer`, before returning the proxy:

```rust
use windows::Win32::System::Com::{
    CoSetProxyBlanket, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};

// SAFETY: `services` is a live proxy; the blanket applies default local
// Windows auth at call-level impersonation, the standard WMI pattern.
CoSetProxyBlanket(
    &services,
    RPC_C_AUTHN_WINNT,
    RPC_C_AUTHZ_NONE,
    None,
    RPC_C_AUTHN_LEVEL_CALL,
    RPC_C_IMP_LEVEL_IMPERSONATE,
    None,
    EOAC_NONE,
)
.ok()?;
```

If the perf class itself is empty on a box, switch the WQL to the `root\WMI` fallback (`SELECT InstanceName, CurrentTemperature FROM MSAcpi_ThermalZoneTemperature`, `CurrentTemperature` in tenths of a Kelvin — same `deci_kelvin_to_celsius`), noting it typically requires the app to run elevated.

---

## Self-Review

**Locked-decision coverage.**
- Sensor chain WDDM → WMI → Schedule, `ThermalTier`/hysteresis unchanged, no `GpuTimeProxy` rung: Task 3 Step 2 (`create_sensor` chain); nothing touches `ThermalTier`/`ThermalThresholds`/`ThermalSource`.
- `Win32_PerfFormattedData_Counters_ThermalZoneInformation` from `root\CIMV2`, preferred over `MSAcpi_ThermalZoneTemperature`; must verify on target: Task 3 Step 1 (WQL) + Task 4 Step 1 (empirical confirmation) + Appendix (fallback).
- New `windows` features only (`Win32_System_Com/_Wmi/_Variant`), no new dep: Task 2.
- Filter 1–150 °C after the Kelvin conversion (tenths of a Kelvin, documented), hottest plausible wins: Task 1 (`PLAUSIBLE_C`, `deci_kelvin_to_celsius`, `select_hottest`).
- COM per-thread, on the sampler thread never the render thread: Task 3 (`WmiThermalZoneSensor::new` inits COM; module doc "Threading and COM"; the sampler already calls `create_sensor` inside its thread — verified in `sensor.rs:55`).
- INFO → WARN for "no sensor", provenance log `source=wmi-zone zone="..." temp=NN.N`, periodic sampling: Task 3 (WARN in `create_sensor`; `PROVENANCE_EVERY` INFO line in `read_celsius`; WDDM's own INFO demoted to DEBUG since it is no longer terminal).
- Selection logic behind a testable seam (pure function over a slice), tests pass on macOS/Linux CI: Task 1 (`wmi_zone`, portable, 7 unit tests, conditional `allow(dead_code)` so it stays live-and-tested without a Windows caller).
- Threshold tuning explicitly out of scope, behaviour-change flagged: `wmi_zone` module doc + Task 4 Step 5.
- OEM-defined zone semantics / skin-lag caveat recorded in code: `windows_wmi.rs` module doc "OEM caveat".
- Force/exercise the WMI rung for testing: `WC_THERMAL_FORCE_WMI` (Task 3 Step 2) + Task 4 Step 3.

**Placeholder scan.** No "TBD"/"similar to Task N"/"...". Every code block is complete. `// UNVERIFIED` markers are deliberate and enumerated in Task 4 Step 2, not placeholders.

**Type consistency (Produces ↔ Consumes).** `wmi_zone` produces `ZoneSample { name: String, deci_kelvin: u32 }`, `select_hottest(&[ZoneSample]) -> Option<HottestZone>`, `HottestZone { name: String, celsius: f32 }`. `windows_wmi` consumes exactly those: `harvest_zones` pushes `ZoneSample`, `read_celsius` calls `select_hottest(&self.scratch)` and reads `.name` / `.celsius`. `WmiThermalZoneSensor` implements `TemperatureSensor::read_celsius(&mut self) -> Option<f32>` (matches `sensor.rs:36`). `create_sensor` returns `Option<Box<dyn TemperatureSensor>>` (matches the other arms).

**Tests genuinely decoupled from WMI.** All 7 tests exercise `deci_kelvin_to_celsius` / `select_hottest` with hand-built `ZoneSample` slices — no COM, no `windows` symbol. They compile and run under `all(target_os = "macos"/"linux")` because `wmi_zone` is not `cfg`-gated to Windows; the conditional `allow(dead_code)` keeps the non-test lib build clean on those targets.

**Clippy-rule check on the example code.** Test module carries `#[allow(clippy::expect_used, reason = ...)]`, so `.expect()` is sanctioned; no `.unwrap()`/`panic!`/`unreachable!`. No `assert_eq!(x.is_some(), true)` (uses `assert!(...is_none())` / destructure). No `0..(N+1)` ranges. No bare `as` casts anywhere — conversions use `u16::try_from` + `f32::from`. Float assertions use `.abs() < eps`, not `==`, so no `clippy::float_cmp`. Production code has no `unwrap`/`expect` (COM errors go through `.ok()?`; the one documented `panic`-free path returns `None`).

## Open questions (could not be resolved by reading code on macOS)

1. **The exact WMI unit and property.** `HighPrecisionTemperature` is assumed to be tenths of a Kelvin. This is the single load-bearing assumption and it cannot be confirmed without running WMI on Windows. Task 4 Step 1 confirms it empirically before any reading is trusted; the conversion is isolated in one function so a correction is a one-line change.
2. **Whether `root\CIMV2` reads without elevation on the target boxes, and whether the perf class is populated.** Preferred for the no-admin property, but not guaranteed per-OEM. Task 4 Step 1 + the Appendix cover the `CoSetProxyBlanket` and `MSAcpi_ThermalZoneTemperature` fallbacks.
3. **windows-rs 0.62 signature drift.** Every `// UNVERIFIED` marker (VARIANT import path, `VariantToUInt32`/`VariantToStringAlt` argument forms, `ExecQuery` flag type, `CoInitializeEx` return type, `CoTaskMemFree` pointer form). Enumerated with likely corrections in Task 4 Step 2; resolvable only by compiling on Windows.
4. **Whether the winning zone is die vs. chipset/skin on each box.** Determines whether a future tuned threshold transfers between the Vega 10 and the 780M. Not answerable now; the provenance `zone` name in the soak log is what will answer it (Task 4 Step 5).
