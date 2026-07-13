# Windows hardware session

Everything in the alpha.5 program that **cannot be done on the Mac**, gathered so it can be
worked through in one sitting on the Windows box. Check this branch out there and work top to
bottom.

Branch: `windows-hardware` (off `v5-alpha`).

## Why these are here and not already done

Two different reasons, and it matters which:

- **Uncompilable here.** Plan 05's COM/WMI code is Windows FFI. `cargo check --target
  x86_64-pc-windows-msvc` dies in `blake3`'s build script (it wants a C compiler for the target),
  and the mingw/GNU route only approximates the MSVC ABI. Writing FFI that has never once seen a
  compiler produces a pile of small errors to grind through. It is cheaper to write it on the box.
- **Unobservable here.** The DirectML fusion crash, the WDDM-silent APU, and the thermal throttle
  do not reproduce on an M-series Mac. No amount of care substitutes for the real hardware.

## Setup

```powershell
git checkout windows-hardware
cargo build -p waveconductor        # first build is long; Bevy from scratch
```

The CI gate, same as everywhere (`AGENTS.md`):

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
$env:RUSTDOCFLAGS="-D warnings"; cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
```

Note the doc gate: run it **exactly** as above. Do not add `--all-features` and do not narrow it
to `-p <crate>` — both produce phantom `unresolved link to crate::input::providers::leap_native`
errors that are artifacts of the command, not real defects. Six agents on this program have been
fooled by that.

---

## 1. Plan 05 — Windows thermal sensor (Tasks 2, 3, 4)

**Plan:** `docs/superpowers/plans/2026-07-09-alpha5-05-windows-thermal-sensor.md`

**Why it matters.** The deployment hardware class (Vega 10 / Radeon 780M APUs) reports **no WDDM
GPU temperature at all**, so the app pins `ThermalTier::Cool` forever and the thermal throttle
never engages across an eight-hour unattended soak. The fix adds a WMI ACPI thermal-zone rung
between the existing WDDM sensor and the schedule fallback.

**Already landed (and tested on macOS):** Task 1 — `crates/wc-core/src/lifecycle/thermal/wmi_zone.rs`.
The pure zone-filtering and hottest-zone-selection logic, 11 tests, no `cfg(windows)` anywhere.
That separation is deliberate: it is the half with actual *logic*, so it is testable where the FFI
is not.

**What Task 3 must know about the API it consumes** (this changed from the plan):

```rust
select_hottest(&[ZoneSample]) -> Option<HottestZone<'_>>
```

`HottestZone` **borrows** the zone name — it is `Copy` and allocation-free. The plan's version
cloned a `String` per sample, on the thermal sampler's continuously-running background thread,
which `AGENTS.md` forbids over a multi-hour soak. Do not reintroduce the clone.

**All-implausible zones return `None`, never the least-bad reading.** A wrong temperature is worse
than no temperature: `None` falls through to the schedule fallback, whereas a garbage value would
drive the throttle from nonsense.

### THE ONE THING TO VERIFY FIRST (Task 4)

> **The unit is an unverified assumption.** The code assumes ACPI thermal zones report
> **tenths of a Kelvin**. If the property actually reports **whole Kelvin**, the converted value
> lands *inside* the plausibility band — it will look completely reasonable and be completely
> wrong, and it will drive the thermal throttle. That is precisely the failure the
> "`None`, never least-bad" rule exists to prevent, and a unit error walks straight past it.

Confirm the property **and its unit** against a real reading before trusting anything downstream:

```powershell
# What zones exist, and what do they actually report?
Get-CimInstance -Namespace root\CIMV2 -ClassName Win32_PerfFormattedData_Counters_ThermalZoneInformation |
  Select-Object Name, Temperature, HighPrecisionTemperature

# Cross-check against a known-good reading (HWiNFO64, or the BIOS temp page).
```

- [ ] Zones enumerate at all on the deployment box (some OEM firmware exposes none — if so, this
      whole rung is dead there and we need a different sensor; say so before writing more code)
- [ ] The property name in the code matches what actually exists
- [ ] **The unit is confirmed** — a zone reading converts to a plausible Celsius value that agrees
      with an independent tool, not merely a value that *looks* plausible
- [ ] Under load, the reading actually *moves* (a zone pinned to a constant is useless)
- [ ] `ThermalTier` transitions Cool → Warm → Hot as the box heats, and the screensaver present-rate
      throttle engages

---

## 2. Plan 08 — DirectML remediation

**Plan:** `docs/superpowers/plans/2026-07-09-alpha5-08-directml-remediation.md`
**Hardware:** needs the **RX 6900 XT** for the probe run.

Runs on its own branch (`windows-directml-prelu-rank` per the program index). Blocked purely on the
box. See the plan for the probe procedure.

---

## 3. Plan 06 — verify the EP fallback against a real DirectML crash

**Merged, gate-green, but never seen fail.** Plan 06 makes a GPU execution provider that crashes at
graph-commit time degrade **that one model** to CPU, instead of costing all hand tracking. On the
Mac there is no DirectML fusion crash to trigger it, so the fallback path has only ever been
exercised by unit tests against pure decision functions.

- [ ] Reproduce the DirectML fusion crash on `palm_detection.onnx` (the failure being remediated)
- [ ] Confirm hand tracking **survives**: palm on CPU, `hand_landmark.onnx` still on DirectML
- [ ] Confirm the combined label reads `ort/DirectML+CPU` — a *mixed* state, not a laundered success
- [ ] **Confirm the amber row appears in the user settings panel**, under "Inference backend".
      This is the whole point: without it, a kiosk running hand tracking on the CPU for eight hours
      looks *identical* to a healthy one — green, no badge, thermals quietly creeping, and the only
      evidence a `warn!` line eight hours up-scroll that nobody tails.
- [ ] Set `ForceGpu` and confirm it now **fails loudly** rather than silently committing an
      unaccelerated session (it is the A/B control; it used to lie)
- [ ] Set `ForceCpu` and confirm hand tracking runs, on CPU, both models
- [ ] Confirm `ForceGpu` is still **reversible from the panel** with hand tracking fully dead — no
      config-file edit

---

## 4. The soak

Blocked on §1 (thermal has to work on the deployment hardware before an eight-hour run tells you
anything about thermals).

`AGENTS.md` requires an 8-hour soak before any release tag. It is currently a manual procedure —
`cargo xtask soak-test` is planned but **does not exist**; do not cite it as if it does.

- [ ] Representative load: hand tracking + audio active, sketch cycling
- [ ] Watch RSS, GPU memory, FPS for drift or a thermal stall
- [ ] Capture the thermal log — **thermal threshold tuning is gated on a real soak log from this
      hardware**, and cannot be done on the Mac at all

---

## Notes carried from the Mac work

- **`cargo xtask capture` works from an agent session.** The old "returns black frames when
  backgrounded" observation did not reproduce; the harness produced correct frames and was used to
  find a real Cymatics defect that no unit test could have caught. If it *does* go black on Windows,
  that is worth recording rather than assuming.
- **Never `git add -A`.** Stage named paths only.
- **`git commit -F <file>`, never `-m`** — backticks in a `-m` string get command-substituted.
