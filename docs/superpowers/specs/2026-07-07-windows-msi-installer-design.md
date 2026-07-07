# Windows MSI Installer + "proper Windows app" polish — Design

Date: 2026-07-07
Status: Approved (brainstorm), pending implementation plan
Scope: single implementation plan

## Problem

WaveConductor's Windows build today is a *console-subsystem* binary shipped as a
portable `.zip` (`cargo xtask bundle-windows` → `Compress-Archive`). Consequences:

1. Double-clicked from Explorer, it opens a stray console window alongside the app.
2. Logs go only to stderr and an in-memory ring buffer; nothing is written to disk.
   With `panic = "abort"` in the release profile, a field crash is completely silent.
3. The staged bundle is **not actually self-contained**: default features include
   `hand-tracking-mediapipe`, which on Windows compiles ONNX Runtime with the
   DirectML execution provider (a dynamic `onnxruntime.dll` + `DirectML.dll`). Those
   DLLs are dropped into `target/release/` by ORT's build script at build time, so
   the app works when launched from `target/release/`, but `bundle-windows` never
   copies them into the staged dist folder. The camera/MediaPipe path would fail to
   load on an end-user machine.
4. No app icon or version metadata on the exe.
5. No installer — no Start Menu entry, no clean upgrade/uninstall story.

## Goals

- Ship a **Windows MSI installer** as the primary release artifact (full feature
  parity: Ultraleap `LeapC.dll` + camera/MediaPipe via ONNX Runtime DirectML).
- Keep the existing **portable `.zip`** as a no-install / no-admin fallback.
- Make the shipped bundle genuinely self-contained (no missing DLLs, no external
  VC++ redistributable dependency).
- Make the app behave like a real Windows GUI app: no console window, an icon +
  version resource, and an on-disk log file for field diagnosis.
- Build and release the MSI from **GitHub Actions CI** (no local Windows box needed
  for release), extending the existing `release.yml` (no new CI jobs).

## Non-goals

- **Code signing / Authenticode.** Roadmap, not now. Unsigned MSI triggers a
  SmartScreen "unknown publisher" warning on first launch; acceptable for the alpha.
  The xtask and CI are structured with an explicit seam where a `signtool` step
  slots in later.
- Single-file self-extracting exe / embedding assets into the binary. Assets stay
  loose next to the exe (the runtime asset resolver already supports this).
- Non-MSI installers (NSIS, Inno Setup).
- Windows-on-ARM. Only `x86_64` is vendored (matches the current bundle constraint).

## Decisions (from brainstorm)

| Decision | Choice |
| --- | --- |
| Feature scope | Full parity: Leap + camera/MediaPipe (DirectML) |
| Primary artifact | MSI installer |
| Portable zip | Keep it, as a secondary fallback |
| Build/release location | GitHub Actions CI (`windows-latest`) |
| MSI generation | `cargo-wix` / WiX Toolset v3, harvesting the staged folder |
| Code signing | Roadmap, not now (design a slot-in seam) |
| File logging | `tracing-appender` (non-blocking writer) |
| CRT | `+crt-static` (primary); VC++ redist merge module as fallback |

## Architecture

`cargo xtask bundle-windows` remains the **single source of truth** for "what
ships." Both distribution artifacts consume its staged output; the MSI step never
re-decides file contents.

```
cargo xtask bundle-windows
   └─ target/dist/windows-x86_64/WaveConductor/     ← the staged app dir
        waveconductor.exe        (release; console-suppressed; icon + version resource)
        LeapC.dll                (already staged)
        onnxruntime.dll          NEW: ORT DirectML runtime (staged from target/release/)
        DirectML.dll             NEW: ORT DirectML runtime
        onnxruntime_providers_shared.dll   NEW (if present — pin via verification)
        assets/                  (already staged, recursive)
        RUN.txt                  (zip only; MSI uses Start Menu shortcut instead)
              │
        ┌─────┴──────────────────────────────┐
   Compress-Archive                  cargo xtask package-windows-msi
   → WaveConductor-windows-x86_64.zip    → heat-harvest of the staged dir
     (portable, unchanged)                 + wix/waveconductor.wxs (product/shortcut/icon)
                                          → WaveConductor-<version>-x86_64.msi
```

Because the MSI harvests the staged folder rather than re-deriving it, the zip and
MSI can never drift, and the ORT-DLL fix benefits both automatically.

## Components

### 1. App-level Windows polish (in `crates/waveconductor/`)

**1a. Console suppression** — top of `src/main.rs`:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
```

`not(debug_assertions)` (rather than bare `windows`) so `cargo rund`, the visual
capture harness, and any debug run keep their console/stderr. Only release builds
detach.

**1b. File log + panic hook** — a new module (`src/logging.rs` or
`lifecycle/logging/`) extending the existing `init_tracing()` in `main.rs`.

- Adds a `tracing-appender` file layer as a fourth layer alongside the existing
  fmt (stderr) and `LogCaptureLayer` (in-memory dev panel) layers. The in-memory
  layer is unchanged.
- Writer config: `RollingFileAppender` with `Rotation::DAILY`,
  `.max_log_files(7)`, writing to `dirs::data_local_dir()/WaveConductor/logs/`
  (`%LOCALAPPDATA%\WaveConductor\logs\` on Windows). An installed (Program Files)
  app cannot write next to its exe, so the per-user local-data dir is the correct
  location. The path is derived at runtime (no hardcoded home paths — honors
  `check-secrets`).
- **Non-blocking writer**: `tracing_appender::non_blocking` offloads file I/O to a
  background thread. Rationale: tracing events fire from Bevy systems on the main
  thread; a synchronous `File::write` could stall a frame on a slow disk flush,
  violating the "never block in a hot path / multi-hour soak" discipline in
  AGENTS.md. The returned `WorkerGuard` **must be held for the whole process
  lifetime** (drop = flush + lose buffered logs); `init_tracing()` returns it and
  `main` holds it (or it lives in a Bevy resource).
- **Panic hook**: `std::panic::set_hook` writes the panic message + location to the
  log file **synchronously** before delegating to the default hook (a crash path is
  not a hot path, and we want the flush to complete before `abort`). This is the
  only field-diagnosis channel once the console is gone and release aborts on panic.
- Applies on all platforms (not Windows-gated): a real log file is useful on the
  macOS deployment too, and keeping it cross-platform means it is covered by the
  normal test surface rather than a Windows-only code path.

Open question for `tracing-appender`, resolved: rotation is **time-based only**
(`DAILY`), no size cap; acceptable for the kiosk soak cadence. `max_log_files(7)`
bounds retention to a rolling week.

**1c. Icon + version metadata** — add `winresource` as a
`[target.'cfg(windows)'.build-dependencies]` in `crates/waveconductor/Cargo.toml`
and have `crates/waveconductor/build.rs` embed:

- An `.ico` (multi-resolution) for the exe icon, Add/Remove Programs, and shortcut.
- Version info: product name, version from `CARGO_PKG_VERSION`, company/copyright.

The repo has an app icon at `assets/app-icons/icon.png` (PNG, not ICO). A one-time
conversion to a multi-resolution `.ico` (e.g. `crates/waveconductor/assets/icon.ico`)
is required; produced with ImageMagick and committed. `winresource` is a
build-dependency only, Windows-gated, so it never touches the `--all-features`
compile surface on macOS/Linux.

### 2. Self-contained bundle fixes (in `xtask/src/bundle/`)

**2a. Stage the ORT DirectML DLLs** — extend `windows.rs`'s `assemble()` to copy
ORT's runtime DLLs from `target/release/` into the staging dir. Implementation:
copy a **known allowlist if present** (`onnxruntime.dll`, `DirectML.dll`,
`onnxruntime_providers_shared.dll`), and the bundle report (`--json`) lists exactly
which DLLs were staged so CI and the operator can verify coverage rather than trust
a silent copy. The staged-DLL list is a natural regression signal.

The **authoritative DLL set is pinned by a verification task** (see Verification
step 3), not guessed: a real Windows release build enumerates `target/release/*.dll`
and the allowlist is derived from that.

**2b. VC++ CRT** — add to `.cargo/config.toml`:

```toml
[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-feature=+crt-static"]
```

Statically links the CRT so **both** the zip and the MSI are self-contained with no
external VC++ redistributable. (A zip cannot carry an installer's redist merge
module, so crt-static is the option that makes the portable artifact self-contained
too.)

Caveat to verify on Windows: our exe would use a static CRT while `onnxruntime.dll`
/ `LeapC.dll` / `DirectML.dll` use the dynamic CRT. Safe as long as no CRT-owned
resource (heap allocations, `FILE*`) crosses the boundary — these are C-ABI
libraries with opaque handles and caller-owned buffers, so it should be fine, but
it is a real interaction and a verify point.

**Fallback** if crt-static misbehaves with ORT: drop crt-static and add Microsoft's
VC++ redist **merge module** to the WiX source (closes it for the MSI only; the zip
would then carry an "install VC++ redist" note in `RUN.txt`).

### 3. MSI packaging (new xtask + WiX source)

**New subcommand `cargo xtask package-windows-msi`** — agent-first: `--help`,
`--json`, registered in `cargo xtask manifest`. Behavior:

- Consumes the staged folder from `bundle-windows`. Errors with a hint to run
  `bundle-windows` first if the staged dir is absent (or invokes it).
- Drives the WiX Toolset (via `cargo-wix`, WiX v3 — the mature, best-documented
  path) to emit `WaveConductor-<version>-x86_64.msi`. If cargo-wix's cargo-centric
  model fights the pre-staged-folder approach, the xtask falls back to invoking
  `heat`/`candle`/`light` directly. Either way the staged folder stays the source of
  truth (WiX harvests it, never re-derives file contents).
- `--json` reports: MSI path, size, resolved MSI version, and the staged DLL list.

**Committed WiX source `wix/waveconductor.wxs`:**

- **Product**: name `WaveConductor`, a fixed `UpgradeCode` GUID (enables clean
  in-place upgrades across versions), manufacturer, icon referenced for Add/Remove
  Programs.
- **Install location**: `ProgramFiles64Folder\WaveConductor`.
- **Files**: a `heat`-harvested fragment for the staged tree (exe + all DLLs +
  `assets/`), so adding an asset or DLL requires no wxs edit.
- **Shortcuts**: Start Menu shortcut to `waveconductor.exe` with the icon. Desktop
  shortcut left off (kiosk).
- **Not installed by the MSI**: the log dir (created at runtime under
  `%LOCALAPPDATA%`; nothing to install, no Program Files write-permission problem).
- **Signing seam**: no sign step now; the xtask/CI has an explicit place where a
  `signtool` call slots in later.

**MSI version mapping** — MSI `ProductVersion` is numeric-only (`a.b.c.d`, each
field ≤ 255), so tags like `v5.0.0-alpha.4` are not legal MSI versions. Mapping rule
(pure function, unit-tested):

- Strip the leading `v`.
- Drop the pre-release number into the 4th field: `v5.0.0-alpha.4` → `5.0.0.4`.
- Non-prerelease `v5.0.0` → `5.0.0.0`.
- The full original tag remains the filename / release-note version; only the
  internal `ProductVersion` uses the numeric form.
- Constraint: the alpha number must stay ≤ 255 (fine for the alpha cadence).

### 4. CI wiring (`.github/workflows/release.yml`)

Extend the existing Windows entry of the `build` matrix (no new job):

- Install WiX Toolset v3 + `cargo-wix` explicitly on the `windows-latest` runner
  (do not rely on the image pre-baking WiX).
- Add a step: `cargo xtask package-windows-msi` → `actions/upload-artifact` for the
  `.msi`. The existing `Compress-Archive` zip step stays as-is.
- `publish` job: ensure its release-upload globs pick up the `.msi` alongside the
  existing archives. No new job (respects the "no new CI jobs without cost review"
  constraint — added steps on an existing runner).
- `+crt-static` in `.cargo/config.toml` also applies to the Windows
  `test-cross-platform` and `ci.yml` Windows jobs; those passing is itself a signal
  crt-static did not break the build.

## Verification

Constraint: primary dev machine is Apple Silicon; this is Windows-only, so the end
result cannot be run on the dev host. Layered verification:

1. **Host-independent unit tests** (normal `cargo nextest`, any host): extend the
   existing `assemble()` tests to assert ORT DLLs are staged when present in the
   source dir; add tests for the version-mapping function (`v5.0.0-alpha.4` →
   `5.0.0.4`, `v5.0.0` → `5.0.0.0`, the ≤ 255 boundary). These are the pieces
   provable from the Mac.
2. **CI Windows build as the compile/link signal**: the release workflow building
   exe + staging + MSI proves crt-static links, `winresource` embeds, and WiX
   assembles — real signal without a local Windows box.
3. **DLL-set pinning (first plan step)**: a Windows build (CI log or VM) enumerates
   `target/release/*.dll` to record the authoritative ORT DLL set before finalizing
   the staging allowlist. Not guessed.
4. **Manual Windows install-test on the deployment hardware** — checklist in the
   spec (only a human on Windows can confirm), analogous to the existing manual soak
   procedure:
   - a. Install the MSI; no error.
   - b. Launch from Start Menu; **no console window appears**.
   - c. Log file appears at `%LOCALAPPDATA%\WaveConductor\logs\` and receives lines.
   - d. Both Leap and camera/MediaPipe initialize (all DLLs resolve at runtime).
   - e. Uninstall is clean (files removed, shortcut removed).
   - f. Installing a newer version upgrades in place (UpgradeCode works).

## Risks / open items

- **ORT DLL set** is unconfirmed until a Windows build (verification step 3). The
  allowlist + tests are written against that result.
- **crt-static × ORT dynamic CRT** interaction — verify point 4d covers it; merge
  module is the documented fallback.
- **WiX v3 availability on the runner** — installed explicitly; if the GitHub image
  drops it entirely, cargo-wix + a pinned WiX download covers it.
- **Unsigned MSI** — SmartScreen warns; documented, signing is roadmap.

## Files touched (anticipated)

- `crates/waveconductor/src/main.rs` — console-suppression attribute; hold WorkerGuard.
- `crates/waveconductor/src/logging.rs` (new) — file layer + panic hook.
- `crates/waveconductor/build.rs` — `winresource` icon/version embedding.
- `crates/waveconductor/assets/icon.ico` (new) — converted from `assets/app-icons/icon.png`.
- `crates/waveconductor/Cargo.toml` — `winresource` Windows build-dep.
- `Cargo.toml` (workspace) — `tracing-appender` dependency.
- `xtask/src/bundle/windows.rs` — stage ORT DLLs; report them.
- `xtask/src/bundle/common.rs` — helper for ORT DLL discovery (if shared).
- `xtask/src/msi.rs` (new) + `xtask/src/main.rs` + `xtask/src/manifest.rs` —
  `package-windows-msi` subcommand + version-mapping function.
- `wix/waveconductor.wxs` (new) — WiX product source.
- `.cargo/config.toml` — `+crt-static` for `x86_64-pc-windows-msvc`.
- `.github/workflows/release.yml` — WiX install + MSI build/upload steps.
