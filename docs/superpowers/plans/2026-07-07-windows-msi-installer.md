# Windows MSI Installer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship WaveConductor on Windows as an MSI installer (primary) plus the existing portable zip (fallback), built in CI, behaving like a real GUI app (no console, on-disk logs, icon, self-contained DLLs).

**Architecture:** `cargo xtask bundle-windows` remains the single source of truth for the staged app directory. The zip archives it (unchanged) and a new `cargo xtask package-windows-msi` harvests the same folder into an MSI via the WiX Toolset. App-level changes (console suppression, file logging, icon/version resource) live in the `waveconductor` crate; bundle/DLL/CRT changes live in `xtask` and `.cargo/config.toml`; packaging lives in a new xtask subcommand + a committed WiX source.

**Tech Stack:** Rust, Bevy, `tracing` / `tracing-subscriber` / `tracing-appender`, `winresource` (Windows build-dep), `dirs`, `clap` (xtask), WiX Toolset v3 + `cargo-wix`, GitHub Actions.

## Global Constraints

- **Rust toolchain:** 1.96.0 (matches CI).
- **Only `x86_64` Windows** is supported/vendored. Non-x86_64 Windows hosts error in the bundler.
- **No new CI jobs** without cost review — extend existing jobs only (added steps on the existing `windows-latest` runner).
- **Avoid new dependencies** unless justified; `tracing-appender` is the one sanctioned addition here (non-blocking writer, sibling of the already-present `tracing-subscriber`).
- **No hardcoded home paths / secrets** — `cargo xtask check-secrets` scans the tree; log paths resolve at runtime via `dirs`, never compile-time home dirs.
- **User-facing copy has no em dashes** (`RUN.txt`, any installer strings). En dashes in ranges are fine.
- **CI gates that must stay green:** `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features --workspace -- -D warnings`, `cargo nextest run --workspace --all-features`, `cargo test --doc --workspace`, `cargo doc --no-deps --workspace --document-private-items` (with `RUSTDOCFLAGS="-D warnings"`), `cargo deny check`, `cargo xtask check-secrets`.
- **`///` rustdoc on every public item; `//!` on every module root.** Windows-only runtime behavior (console suppression, MSI install) is verified in CI / on the deployment machine, not on the macOS dev host — call these out explicitly in each affected task.

---

### Task 1: On-disk logging module + panic hook

Adds a `tracing-appender` non-blocking rolling-file log layer and a synchronous panic hook. This is a prerequisite for Task 2 (console suppression), which otherwise sends all stderr output into the void.

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`, after line 95)
- Modify: `crates/waveconductor/Cargo.toml` (`[dependencies]`, after line 52)
- Create: `crates/waveconductor/src/logging.rs`
- Modify: `crates/waveconductor/src/main.rs` (add `mod logging;`, rework `init_tracing`, hold the guard in `main`)

**Interfaces:**
- Produces:
  - `logging::log_dir() -> std::path::PathBuf` — resolved log directory.
  - `logging::file_writer() -> Option<(tracing_appender::non_blocking::NonBlocking, tracing_appender::non_blocking::WorkerGuard)>`
  - `logging::install_panic_hook()`
  - `init_tracing() -> (wc_core::diagnostics::LogBuffer, Option<tracing_appender::non_blocking::WorkerGuard>)` (return type changes; `main` must hold the guard).

- [ ] **Step 1: Add the workspace dependency**

In `Cargo.toml`, after line 95 (`tracing-subscriber = ...`), add:

```toml
tracing-appender = "0.2"
```

- [ ] **Step 2: Add the crate dependency**

In `crates/waveconductor/Cargo.toml`, after line 52 (`tracing-subscriber = { workspace = true }`), add:

```toml
tracing-appender = { workspace = true }
```

- [ ] **Step 3: Write the failing test for the logging module**

Create `crates/waveconductor/src/logging.rs` with the tests first (module body added in Step 5):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn log_dir_in_nests_under_app_and_logs() {
        let got = log_dir_in(Path::new("/base"));
        assert!(got.ends_with("WaveConductor/logs"), "got {got:?}");
        assert!(got.starts_with("/base"), "got {got:?}");
    }

    #[test]
    fn format_panic_includes_location_when_present() {
        let line = format_panic("boom", Some("src/main.rs:12:5"));
        assert_eq!(line, "PANIC at src/main.rs:12:5: boom");
    }

    #[test]
    fn format_panic_omits_location_when_absent() {
        let line = format_panic("boom", None);
        assert_eq!(line, "PANIC: boom");
    }
}
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cargo test -p waveconductor --lib logging 2>&1 | tail -20`
Expected: FAIL to compile — `cannot find function log_dir_in` / `format_panic` in this scope.

- [ ] **Step 5: Write the module implementation**

Prepend to `crates/waveconductor/src/logging.rs` (above the `#[cfg(test)] mod tests` block):

```rust
//! On-disk logging for release builds.
//!
//! Release builds suppress the console (`windows_subsystem = "windows"`), so
//! stderr goes nowhere; combined with `panic = "abort"` a field crash would be
//! silent. This module adds a rolling on-disk log under the per-user local-data
//! dir and a panic hook, so a deployed build is diagnosable. The writer is
//! non-blocking (file I/O runs on a background thread) to keep disk flushes off
//! the frame/render path, per the "never block in a hot path" rule in AGENTS.md.
//!
//! Cross-platform on purpose: an on-disk log is useful on the macOS deployment
//! too, and keeping one code path means it is covered by the normal test surface
//! rather than a Windows-only branch.

use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};

/// Join the log directory (`<base>/WaveConductor/logs`) onto a data-root base.
///
/// Pure so it can be unit-tested without touching the real `dirs::data_local_dir`.
fn log_dir_in(base: &Path) -> PathBuf {
    base.join("WaveConductor").join("logs")
}

/// Resolve the on-disk log directory: `<data_local_dir>/WaveConductor/logs`,
/// falling back to `./logs` when the platform exposes no local-data dir.
pub fn log_dir() -> PathBuf {
    dirs::data_local_dir().map_or_else(|| PathBuf::from("logs"), |base| log_dir_in(&base))
}

/// Build the non-blocking rolling-file writer and its flush guard.
///
/// Returns `None` (logging to a file is best-effort; stderr and the in-memory
/// buffer still work) if the directory can't be created or the appender can't be
/// built. Rotation is daily with a 7-file retention window.
///
/// The returned [`WorkerGuard`] MUST be held for the process lifetime; dropping
/// it flushes and stops the background writer.
pub fn file_writer() -> Option<(NonBlocking, WorkerGuard)> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("waveconductor")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir)
        .ok()?;
    Some(tracing_appender::non_blocking(appender))
}

/// Format one panic log line. Pure for testability.
fn format_panic(payload: &str, location: Option<&str>) -> String {
    match location {
        Some(loc) => format!("PANIC at {loc}: {payload}"),
        None => format!("PANIC: {payload}"),
    }
}

/// Install a panic hook that appends the panic to `<log_dir>/panic.log`
/// synchronously (before the default hook runs and, in release, the process
/// aborts), then delegates to the previous hook.
pub fn install_panic_hook() {
    let dir = log_dir();
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_owned());
        let location = info.location().map(ToString::to_string);
        let line = format_panic(&payload, location.as_deref());
        if std::fs::create_dir_all(&dir).is_ok() {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("panic.log"))
            {
                let _ = writeln!(f, "{line}");
            }
        }
        default(info);
    }));
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p waveconductor --lib logging 2>&1 | tail -20`
Expected: PASS (3 tests).

- [ ] **Step 7: Wire the module into `main.rs`**

In `crates/waveconductor/src/main.rs`, add after line 22 (`mod hand_providers;`):

```rust
mod logging;
```

Replace the `init_tracing` function (lines 484-509) with:

```rust
/// Initialize the tracing subscriber: env-filtered fmt to stderr, a capture
/// layer feeding the in-app [`wc_core::diagnostics::LogBuffer`], and (best
/// effort) a non-blocking rolling on-disk log. Also installs the panic hook.
///
/// Returns the log buffer (inserted as a resource for the dev panel) and the
/// file writer's [`WorkerGuard`], which `main` must hold for the process
/// lifetime so buffered log lines are flushed.
fn init_tracing() -> (
    wc_core::diagnostics::LogBuffer,
    Option<tracing_appender::non_blocking::WorkerGuard>,
) {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let buffer = wc_core::diagnostics::LogBuffer::new(500);
    // `ort=warn`: the `ort` crate creates the ONNX Runtime environment at VERBOSE
    // and bridges every ORT message into `tracing` under the `ort` target, relying
    // on this filter to gate it. At `info` the graph-transformer / initializer /
    // model-cache chatter (hundreds of lines per session init) floods the log;
    // `warn` keeps the meaningful ORT warnings (partition counts, EP assignment)
    // and drops the noise. Overridable: `RUST_LOG=ort=trace` restores the full
    // node-placement dump for debugging (see `inference_ort::backend`).
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,waveconductor=info,wc_core=info,ort=warn"));

    // Best-effort on-disk layer. `.with(Option<Layer>)` is a no-op when `None`.
    let (file_layer, guard) = match logging::file_writer() {
        Some((writer, guard)) => {
            let layer = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_target(false)
                .with_writer(writer);
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(LogCaptureLayer {
            buffer: buffer.clone(),
        })
        .with(file_layer)
        .init();

    logging::install_panic_hook();
    (buffer, guard)
}
```

Update the call site at lines 25-29. Replace:

```rust
    let log_buffer = init_tracing();
    let mut app = App::new();
    app.insert_resource(log_buffer)
```

with:

```rust
    // `_log_guard` keeps the non-blocking file-log writer alive for the whole
    // process; dropping it would flush and stop on-disk logging.
    let (log_buffer, _log_guard) = init_tracing();
    let mut app = App::new();
    app.insert_resource(log_buffer)
```

- [ ] **Step 8: Verify build, format, and clippy**

Run: `cargo build -p waveconductor 2>&1 | tail -20`
Expected: builds clean (a real log file appears at `<data_local_dir>/WaveConductor/logs/waveconductor.<date>.log` on next run — verified at runtime, not here).

Run: `cargo fmt --all -- --check && cargo clippy -p waveconductor --all-features -- -D warnings 2>&1 | tail -20`
Expected: no output / no warnings.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml crates/waveconductor/Cargo.toml crates/waveconductor/src/logging.rs crates/waveconductor/src/main.rs
git commit -F- <<'EOF'
feat(logging): non-blocking on-disk log + panic hook

Adds a tracing-appender rolling file layer (daily, 7-file retention) under
the per-user local-data dir plus a synchronous panic hook, so release
builds remain diagnosable once the console is suppressed. Writer is
non-blocking to keep disk I/O off the frame path.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 2: Suppress the console window in release builds

**Files:**
- Modify: `crates/waveconductor/src/main.rs:1` (crate-level attribute)

**Interfaces:**
- Consumes: nothing.
- Produces: no API; a build-attribute behavior change (Windows release detaches from console).

- [ ] **Step 1: Add the attribute**

At the very top of `crates/waveconductor/src/main.rs` (before the `//!` module doc at line 1), add:

```rust
// Release builds are a GUI app: detach from the console so double-clicking the
// installed exe doesn't spawn a stray console window. Gated on
// `not(debug_assertions)` (not bare `windows`) so `cargo rund`, the visual
// capture harness, and any debug run keep their stderr/console. Inert on
// non-Windows targets. Logs still land on disk via `logging` (Task 1).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
```

- [ ] **Step 2: Verify it compiles on the dev host**

Run: `cargo build -p waveconductor 2>&1 | tail -5`
Expected: builds clean. (The attribute is inert on macOS and in debug builds; its runtime effect — no console window — is verified on Windows in CI / on the deployment machine, per Task 7's manual checklist.)

- [ ] **Step 3: Verify format and clippy**

Run: `cargo fmt --all -- --check && cargo clippy -p waveconductor --all-features -- -D warnings 2>&1 | tail -5`
Expected: no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/waveconductor/src/main.rs
git commit -F- <<'EOF'
feat(windows): suppress console window in release builds

Adds `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`
so the installed release exe is a GUI app with no stray console. Debug
builds (cargo rund, capture harness) keep their console.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 3: App icon + version resource (winresource)

**Files:**
- Create: `crates/waveconductor/assets/icon.ico` (converted from `assets/app-icons/icon.png`)
- Modify: `crates/waveconductor/Cargo.toml` (add a Windows `[target.'cfg(windows)'.build-dependencies]` table)
- Modify: `crates/waveconductor/build.rs` (embed icon + version info on Windows)

**Interfaces:**
- Consumes: nothing.
- Produces: no Rust API; build-time embedding of icon + version resource into the Windows exe.

- [ ] **Step 1: Convert the PNG to a multi-resolution ICO**

Run (ImageMagick v7; installed via Homebrew on the dev host):

```bash
magick assets/app-icons/icon.png -background none -define icon:auto-resize=256,128,64,48,32,16 crates/waveconductor/assets/icon.ico
```

Verify it exists and is an ICO:

Run: `file crates/waveconductor/assets/icon.ico`
Expected: `... MS Windows icon resource - 6 icons ...`

- [ ] **Step 2: Add the Windows build-dependency**

In `crates/waveconductor/Cargo.toml`, add a new table (place it next to the existing `[target.'cfg(target_os = "windows")'.dependencies]` at line 62):

```toml
[target.'cfg(windows)'.build-dependencies]
# Embeds the app icon + version metadata into the Windows exe at build time.
# Build-dependency only + Windows-gated, so it never touches the macOS/Linux
# `--all-features` compile surface.
winresource = "0.1"
```

- [ ] **Step 3: Embed the resource in build.rs**

In `crates/waveconductor/build.rs`, inside the existing `fn main()`, add a new Windows-gated block (leave the existing `LeapC.dll` copy block intact). Add after the existing `println!("cargo:rerun-if-changed=...")` line:

```rust
    // Embed the app icon + version metadata into the Windows exe.
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "WaveConductor");
        res.set("FileDescription", "WaveConductor");
        res.set("CompanyName", "Madison Rickert");
        res.compile()
            .expect("embed Windows resources (icon + version) via winresource");
    }
```

- [ ] **Step 4: Verify the dev-host build is unaffected**

Run: `cargo build -p waveconductor 2>&1 | tail -5`
Expected: builds clean (the winresource block is `cfg(target_os = "windows")`, so it is compiled out on macOS; icon embedding is verified on Windows in CI / on the deployment machine).

Run: `cargo xtask check-secrets 2>&1 | tail -5`
Expected: passes (the ICO is binary; `assets/icon.ico` path is workspace-relative).

- [ ] **Step 5: Commit**

```bash
git add crates/waveconductor/assets/icon.ico crates/waveconductor/Cargo.toml crates/waveconductor/build.rs
git commit -F- <<'EOF'
feat(windows): embed app icon + version resource

Converts the app icon to a multi-resolution .ico and embeds it plus
version metadata into the Windows exe via winresource (Windows-only
build-dep). Makes the installed exe show a real icon in Explorer, the
Start Menu, and Add/Remove Programs.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 4: Stage the ONNX Runtime DLLs in the Windows bundle

Closes the "not actually self-contained" gap: ORT's DirectML build drops `onnxruntime*.dll` + `DirectML.dll` next to the release binary, but the staging step never copies them.

**Files:**
- Modify: `xtask/src/bundle/common.rs:27-34` (add `runtime_dlls` field to `StageReport`)
- Modify: `xtask/src/bundle/linux.rs:139` (set the new field to `Vec::new()`)
- Modify: `xtask/src/bundle/windows.rs` (`assemble`, `report_out`, and the test)

**Interfaces:**
- Consumes: `common::StageReport` (Task-local; adds a field).
- Produces: `StageReport.runtime_dlls: Vec<String>` — sorted filenames of staged runtime DLLs.

- [ ] **Step 1: Add the field to `StageReport`**

In `xtask/src/bundle/common.rs`, extend the struct (lines 27-34):

```rust
pub struct StageReport {
    /// Absolute path to the assembled staging directory.
    pub dir: PathBuf,
    /// Total size of the staging directory in bytes.
    pub size_bytes: u64,
    /// Number of regular files copied from `assets/`.
    pub asset_count: u64,
    /// Filenames of runtime DLLs staged next to the binary (Windows ORT
    /// DirectML runtime). Empty on platforms that stage none. Sorted.
    pub runtime_dlls: Vec<String>,
}
```

- [ ] **Step 2: Keep the Linux bundler compiling**

In `xtask/src/bundle/linux.rs`, in the `Ok(StageReport { ... })` at line 139, add the field:

```rust
        runtime_dlls: Vec::new(),
```

- [ ] **Step 3: Write the failing test in windows.rs**

In `xtask/src/bundle/windows.rs`, add this test to the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn assemble_stages_ort_dlls_from_binary_dir() {
        let tmp = unique_tmp();

        let binary = tmp.join("waveconductor.exe");
        std::fs::write(&binary, b"PE-ish binary bytes").expect("write fake exe");
        // ORT drops these next to the release binary; DirectML.dll casing varies.
        std::fs::write(tmp.join("onnxruntime.dll"), b"ort").expect("ort dll");
        std::fs::write(tmp.join("onnxruntime_providers_shared.dll"), b"ort2").expect("ort shared");
        std::fs::write(tmp.join("DirectML.dll"), b"dml").expect("dml dll");
        // An unrelated DLL that must NOT be staged.
        std::fs::write(tmp.join("random.dll"), b"nope").expect("random dll");
        let leap = tmp.join("LeapC.dll");
        std::fs::write(&leap, b"leap dll bytes").expect("write fake dll");
        let assets = tmp.join("assets");
        std::fs::create_dir_all(&assets).expect("mk assets");
        std::fs::write(assets.join("a.txt"), b"a").expect("asset a");

        let staging_root = tmp.join("dist");
        let report = assemble(&binary, &leap, &assets, &staging_root).expect("assemble");

        let app = staging_root.join("WaveConductor");
        assert!(app.join("onnxruntime.dll").is_file(), "ort staged");
        assert!(
            app.join("onnxruntime_providers_shared.dll").is_file(),
            "ort shared staged"
        );
        assert!(app.join("DirectML.dll").is_file(), "directml staged");
        assert!(!app.join("random.dll").exists(), "unrelated dll not staged");
        assert_eq!(
            report.runtime_dlls,
            vec![
                "DirectML.dll".to_string(),
                "onnxruntime.dll".to_string(),
                "onnxruntime_providers_shared.dll".to_string(),
            ],
            "sorted staged dll list"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cargo test -p xtask --lib bundle::windows 2>&1 | tail -20`
Expected: FAIL — `random.dll` staged / `runtime_dlls` field missing values (no staging logic yet).

- [ ] **Step 5: Implement DLL staging in `assemble`**

In `xtask/src/bundle/windows.rs`, in `assemble`, after the `common::copy_leap_lib(...)` call (line ~130) and before the assets copy, add:

```rust
    // Stage the ONNX Runtime DirectML DLLs that ORT's build script drops next to
    // the release binary (present only when `hand-tracking-mediapipe` is compiled
    // on Windows). Matched by known name so the exact provider-shared filename —
    // which varies by ORT build — is tolerated. Best-effort per file; the report
    // lists what was staged so CI can assert coverage. `LeapC.dll` is handled
    // above and deliberately excluded here.
    let bin_dir = binary.parent().unwrap_or_else(|| Path::new("."));
    let mut runtime_dlls = Vec::new();
    for entry in std::fs::read_dir(bin_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let lower = name.to_ascii_lowercase();
        let is_ort = lower.starts_with("onnxruntime") && lower.ends_with(".dll");
        let is_directml = lower == "directml.dll";
        if is_ort || is_directml {
            std::fs::copy(entry.path(), app_dir.join(file_name.as_os_str())).map_err(|e| {
                format!("bundle-windows: cannot stage runtime dll {name}: {e}")
            })?;
            runtime_dlls.push(name.into_owned());
        }
    }
    runtime_dlls.sort();
```

Then update the `Ok(StageReport { ... })` at line ~145 to include the field:

```rust
    Ok(StageReport {
        dir: app_dir,
        size_bytes,
        asset_count,
        runtime_dlls,
    })
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p xtask --lib bundle::windows 2>&1 | tail -20`
Expected: PASS (including the existing `assemble_lays_out_exe_dll_assets_and_notes`).

- [ ] **Step 7: Surface staged DLLs in the report output**

In `xtask/src/bundle/windows.rs`, update `report_out` so both branches report the DLLs. Replace the JSON `println!` with:

```rust
    if json {
        let dlls = report
            .runtime_dlls
            .iter()
            .map(|d| format!("\"{d}\""))
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "{{\"dir\":\"{dir}\",\"size_bytes\":{},\"asset_count\":{},\"runtime_dlls\":[{dlls}]}}",
            report.size_bytes, report.asset_count
        );
    } else {
```

And in the human branch, after the `asset files` line, add:

```rust
        println!("  runtime dlls  {}", report.runtime_dlls.join(", "));
```

- [ ] **Step 8: Verify build, format, clippy, and full xtask tests**

Run: `cargo fmt --all -- --check && cargo clippy -p xtask --all-features -- -D warnings 2>&1 | tail -10`
Expected: no warnings.

Run: `cargo test -p xtask 2>&1 | tail -15`
Expected: PASS.

> **CI verification note:** On the first CI Windows build after this task, inspect the `cargo xtask bundle-windows --json` output's `runtime_dlls` array against `target/release/*.dll`. This is the DLL-set pinning step from the spec: confirm the glob captured every ORT DLL and nothing stray. If ORT emits a DLL not matched by `onnxruntime*` / `DirectML.dll`, widen the match here.

- [ ] **Step 9: Commit**

```bash
git add xtask/src/bundle/common.rs xtask/src/bundle/linux.rs xtask/src/bundle/windows.rs
git commit -F- <<'EOF'
fix(bundle-windows): stage ONNX Runtime DirectML DLLs

The Windows bundle omitted onnxruntime*.dll / DirectML.dll that ORT drops
next to the release binary, so the camera/MediaPipe path would fail to
load outside target/release/. Stage them into the app dir and report the
staged set via --json so CI can assert coverage.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 5: Static CRT for the Windows target

Makes both the zip and the MSI self-contained with no external VC++ redistributable.

**Files:**
- Modify: `.cargo/config.toml` (add a `[target.x86_64-pc-windows-msvc]` table)

**Interfaces:**
- Consumes: nothing.
- Produces: no API; a link-time change (static CRT) for the MSVC target.

- [ ] **Step 1: Add the target rustflags**

In `.cargo/config.toml`, after the existing `[target.x86_64-unknown-linux-gnu]` block, add:

```toml
# Statically link the MSVC C runtime so the shipped exe needs no external
# "VC++ 2015-2022 Redistributable" install. Makes BOTH the portable zip and the
# MSI self-contained (a zip cannot carry an installer's redist merge module).
#
# Our static-CRT exe loads onnxruntime.dll / DirectML.dll / LeapC.dll, which use
# the dynamic CRT. Safe because these are C-ABI libraries with opaque handles and
# caller-owned buffers, so no CRT-owned resource (heap allocation, FILE*) crosses
# the boundary. If ORT ever regresses this, drop crt-static and add Microsoft's
# VC++ redist merge module to the WiX source (MSI-only) instead.
[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-feature=+crt-static"]
```

- [ ] **Step 2: Verify the config parses on the dev host**

Run: `cargo metadata --no-deps --format-version 1 >/dev/null 2>&1 && echo "config OK"`
Expected: `config OK` (the `[target.x86_64-pc-windows-msvc]` table is inert on macOS; crt-static linking is verified by the CI Windows build and the `test-cross-platform` Windows job going green).

- [ ] **Step 3: Commit**

```bash
git add .cargo/config.toml
git commit -F- <<'EOF'
build(windows): statically link the MSVC CRT

Adds `+crt-static` for x86_64-pc-windows-msvc so the shipped exe needs no
external VC++ redistributable, making both the portable zip and the MSI
self-contained.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 6: `package-windows-msi` xtask subcommand + WiX source

**Files:**
- Create: `xtask/src/msi.rs`
- Create: `wix/waveconductor.wxs`
- Modify: `xtask/src/main.rs` (register `mod msi;`, add the `Command` variant + match arm)
- Modify: `xtask/src/manifest.rs` (add the `SUBCOMMANDS` entry)
- Modify: `xtask/tests/manifest.rs` (add to `EXPECTED_SUBCOMMANDS`)

**Interfaces:**
- Consumes: the staged dir at `target/dist/windows-x86_64/WaveConductor` (from Task 4's `bundle-windows`).
- Produces:
  - `msi::run(args: msi::Args) -> Result<(), Box<dyn std::error::Error>>`
  - `msi::msi_version(tag: &str) -> Result<String, String>` (pure; used internally and unit-tested).

- [ ] **Step 1: Write the failing test for the version mapping**

Create `xtask/src/msi.rs` with the tests first:

```rust
#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test scaffolding")]
mod tests {
    use super::*;

    #[test]
    fn maps_prerelease_tag_to_four_field_numeric() {
        assert_eq!(msi_version("v5.0.0-alpha.4").expect("map"), "5.0.0.4");
    }

    #[test]
    fn maps_plain_release_tag_to_zero_fourth_field() {
        assert_eq!(msi_version("v5.0.0").expect("map"), "5.0.0.0");
        assert_eq!(msi_version("5.1.2").expect("map"), "5.1.2.0");
    }

    #[test]
    fn rejects_field_over_255() {
        assert!(msi_version("v5.0.0-alpha.300").is_err());
        assert!(msi_version("v5.0.256").is_err());
    }

    #[test]
    fn rejects_malformed_tag() {
        assert!(msi_version("v5.0").is_err());
        assert!(msi_version("nonsense").is_err());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p xtask --lib msi 2>&1 | tail -20`
Expected: FAIL to compile — `msi` module not declared / `msi_version` missing. (Add `mod msi;` per Step 4 to reach the compile error on `msi_version` itself; until then the module is unreferenced.)

- [ ] **Step 3: Implement the module**

Prepend to `xtask/src/msi.rs` (above the test block):

```rust
//! `cargo xtask package-windows-msi` — build a Windows MSI from the staged
//! Windows app directory produced by `bundle-windows`.
//!
//! The staged folder (`target/dist/windows-x86_64/WaveConductor`) is the single
//! source of truth for what ships; this subcommand harvests it into an MSI via
//! the WiX Toolset (`cargo wix`), never re-deriving file contents. It expects to
//! run on a Windows runner with WiX v3 + `cargo-wix` installed (that is where a
//! Windows release binary and MSI are produced); the version-mapping logic is
//! host-independent and unit-tested on any host.

use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;

use crate::bundle::common;

/// The staged Windows app directory, relative to the workspace root.
const STAGED_REL: &str = "target/dist/windows-x86_64/WaveConductor";

/// Arguments for the package-windows-msi subcommand.
#[derive(ClapArgs)]
pub struct Args {
    /// Release tag to stamp into the MSI (e.g. `v5.0.0-alpha.4`). Defaults to the
    /// crate version (`0.0.0` form) when omitted.
    #[arg(long)]
    pub version: Option<String>,

    /// Emit machine-readable JSON instead of the human summary.
    #[arg(long)]
    pub json: bool,
}

/// Map a release tag to a legal MSI `ProductVersion` (`a.b.c.d`, each ≤ 255).
///
/// - Strips a leading `v`.
/// - `MAJOR.MINOR.PATCH` maps to `MAJOR.MINOR.PATCH.0`.
/// - `MAJOR.MINOR.PATCH-<label>.N` maps to `MAJOR.MINOR.PATCH.N`.
///
/// Errors on malformed input or any field > 255.
pub fn msi_version(tag: &str) -> Result<String, String> {
    let tag = tag.strip_prefix('v').unwrap_or(tag);
    let (core, pre) = match tag.split_once('-') {
        Some((core, label)) => (core, Some(label)),
        None => (tag, None),
    };
    let mut fields: Vec<u32> = Vec::new();
    for part in core.split('.') {
        let n: u32 = part
            .parse()
            .map_err(|_| format!("msi_version: non-numeric field in '{tag}'"))?;
        fields.push(n);
    }
    if fields.len() != 3 {
        return Err(format!(
            "msi_version: expected MAJOR.MINOR.PATCH core, got '{core}'"
        ));
    }
    let fourth: u32 = match pre {
        Some(label) => label
            .rsplit('.')
            .next()
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| format!("msi_version: no numeric suffix in prerelease '{tag}'"))?,
        None => 0,
    };
    fields.push(fourth);
    if let Some(bad) = fields.iter().find(|&&f| f > 255) {
        return Err(format!("msi_version: field {bad} exceeds 255 in '{tag}'"));
    }
    Ok(fields
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join("."))
}

/// Resolve the tag to use: explicit `--version`, else the xtask crate version.
fn resolve_tag(explicit: Option<&str>) -> String {
    explicit.map_or_else(|| env!("CARGO_PKG_VERSION").to_owned(), ToOwned::to_owned)
}

/// Execute the package-windows-msi subcommand.
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let root = common::workspace_root();
    let staged = root.join(STAGED_REL);
    if !staged.is_dir() {
        return Err(format!(
            "package-windows-msi: staged dir not found at {}; run `cargo xtask bundle-windows` first",
            staged.display()
        )
        .into());
    }

    let tag = resolve_tag(args.version.as_deref());
    let version = msi_version(&tag)?;
    let out_msi = root.join(format!("WaveConductor-{tag}-x86_64.msi"));

    build_msi(&root, &staged, &version, &out_msi)?;

    let size = std::fs::metadata(&out_msi).map(|m| m.len()).unwrap_or(0);
    if args.json {
        println!(
            "{{\"msi\":\"{}\",\"version\":\"{version}\",\"size_bytes\":{size}}}",
            out_msi.display()
        );
    } else {
        println!("MSI written: {}", out_msi.display());
        println!("  version   {version}");
        println!("  size      {} bytes", size);
    }
    Ok(())
}

/// Invoke `cargo wix` against the committed WiX source, harvesting the staged
/// dir. `cargo-wix` + WiX v3 must be on PATH (CI installs them).
fn build_msi(
    root: &Path,
    staged: &Path,
    version: &str,
    out_msi: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let wxs = root.join("wix").join("waveconductor.wxs");
    let status = std::process::Command::new("cargo")
        .current_dir(root)
        .args(["wix", "--no-build", "--nocapture"])
        .args(["--install-version", version])
        .args(["--output".as_ref(), out_msi.as_os_str()])
        .args(["--include".as_ref(), wxs.as_os_str()])
        .env("WC_MSI_STAGED_DIR", staged)
        .status()?;
    if !status.success() {
        return Err(format!("package-windows-msi: `cargo wix` failed with {status}").into());
    }
    Ok(())
}
```

> **Implementer note on `build_msi`:** the exact `cargo wix` flag surface (harvesting the staged dir via `heat`, passing `WC_MSI_STAGED_DIR` into the wxs as a preprocessor variable) is finalized against the real tool on the CI Windows runner — `cargo wix` vs a direct `heat`/`candle`/`light` invocation is an implementation detail behind this function. The unit-tested contract (`msi_version`, the staged-dir existence guard, output naming) is fixed; the subprocess wiring is verified in CI (Task 7). Keep `msi_version` and `run`'s validation intact.

- [ ] **Step 4: Register the module and subcommand**

In `xtask/src/main.rs`, add `mod msi;` after line 13 (`mod manifest;` — keep alphabetical-ish grouping):

```rust
mod msi;
```

Add a `Command` variant after `BundleWindows` (line 39):

```rust
    /// Package the staged Windows app dir into an MSI installer.
    PackageWindowsMsi(msi::Args),
```

Add the match arm after the `BundleWindows` arm (line 56):

```rust
        Command::PackageWindowsMsi(args) => msi::run(args),
```

- [ ] **Step 5: Register in the manifest table**

In `xtask/src/manifest.rs`, add an `Entry` to `SUBCOMMANDS` after the `bundle-windows` entry (line 57):

```rust
    Entry {
        name: "package-windows-msi",
        description: "Package the staged Windows app dir into an MSI installer.",
    },
```

In `xtask/tests/manifest.rs`, add to `EXPECTED_SUBCOMMANDS` after `"bundle-windows"` (line 29):

```rust
    "package-windows-msi",
```

- [ ] **Step 6: Write the committed WiX source**

Create `wix/waveconductor.wxs`. This is a WiX v3 source; the file list under the app dir is harvested from `$(env.WC_MSI_STAGED_DIR)` via `heat` at build time (see the `build_msi` implementer note), so only product/shortcut/icon metadata is hand-authored here:

```xml
<?xml version="1.0" encoding="windows-1252"?>
<!--
  WiX v3 product source for the WaveConductor MSI.

  The file payload (exe + DLLs + assets/) is harvested from the staged app dir
  ($(env.WC_MSI_STAGED_DIR)) at build time, so this file only declares product
  metadata, the install location, the icon, and the Start Menu shortcut. The
  UpgradeCode is fixed so newer versions upgrade in place. Unsigned for the
  alpha; a signtool step slots in after `light` in CI later.
-->
<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">
  <Product Id="*"
           Name="WaveConductor"
           Language="1033"
           Version="$(var.Version)"
           Manufacturer="Madison Rickert"
           UpgradeCode="PUT-A-FIXED-GUID-HERE">
    <Package InstallerVersion="500" Compressed="yes" InstallScope="perMachine" />
    <MajorUpgrade DowngradeErrorMessage="A newer version of WaveConductor is already installed." />
    <MediaTemplate EmbedCab="yes" />

    <Icon Id="AppIcon" SourceFile="crates/waveconductor/assets/icon.ico" />
    <Property Id="ARPPRODUCTICON" Value="AppIcon" />

    <Directory Id="TARGETDIR" Name="SourceDir">
      <Directory Id="ProgramFiles64Folder">
        <Directory Id="INSTALLDIR" Name="WaveConductor" />
      </Directory>
      <Directory Id="ProgramMenuFolder" />
    </Directory>

    <!-- HarvestedComponents is a ComponentGroup emitted by heat from the staged
         dir; referenced here so the harvested files land under INSTALLDIR. -->
    <Feature Id="Main" Title="WaveConductor" Level="1">
      <ComponentGroupRef Id="HarvestedComponents" />
      <ComponentRef Id="StartMenuShortcut" />
    </Feature>

    <DirectoryRef Id="ProgramMenuFolder">
      <Component Id="StartMenuShortcut" Guid="*">
        <Shortcut Id="WaveConductorShortcut"
                  Name="WaveConductor"
                  Target="[INSTALLDIR]waveconductor.exe"
                  WorkingDirectory="INSTALLDIR"
                  Icon="AppIcon" />
        <RegistryValue Root="HKCU" Key="Software\WaveConductor"
                       Name="installed" Type="integer" Value="1" KeyPath="yes" />
      </Component>
    </DirectoryRef>
  </Product>
</Wix>
```

Replace `PUT-A-FIXED-GUID-HERE` with a generated GUID:

Run: `uuidgen | tr '[:lower:]' '[:upper:]'`
Paste the result into the `UpgradeCode`. This GUID is permanent — never change it, or upgrades break.

- [ ] **Step 7: Run the xtask tests**

Run: `cargo test -p xtask 2>&1 | tail -20`
Expected: PASS — `msi` unit tests plus the `manifest.rs` tests (which now agree that `package-windows-msi` exists across the `Command` enum, the `SUBCOMMANDS` table, and `EXPECTED_SUBCOMMANDS`).

- [ ] **Step 8: Verify format, clippy, and manifest wiring**

Run: `cargo fmt --all -- --check && cargo clippy -p xtask --all-features -- -D warnings 2>&1 | tail -10`
Expected: no warnings.

Run: `cargo run -p xtask -- manifest 2>&1 | grep package-windows-msi`
Expected: the new subcommand appears.

Run: `cargo run -p xtask -- package-windows-msi 2>&1 | tail -3`
Expected: errors with "staged dir not found ... run `cargo xtask bundle-windows` first" (correct on macOS — no staged dir, and no WiX; the actual MSI build is CI-verified).

- [ ] **Step 9: Commit**

```bash
git add xtask/src/msi.rs xtask/src/main.rs xtask/src/manifest.rs xtask/tests/manifest.rs wix/waveconductor.wxs
git commit -F- <<'EOF'
feat(xtask): package-windows-msi subcommand + WiX source

Adds `cargo xtask package-windows-msi`, which harvests the staged Windows
app dir into an MSI via WiX v3 (cargo-wix). Includes a unit-tested
tag->MSI-version mapping (v5.0.0-alpha.4 -> 5.0.0.4) and a committed WiX
product source with a fixed UpgradeCode, Program Files install, and a
Start Menu shortcut. Unsigned for the alpha; signing seam left open.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 7: CI — build and publish the MSI

**Files:**
- Modify: `.github/workflows/release.yml` (the `build` job's Windows steps + the `publish` job's upload glob)

**Interfaces:**
- Consumes: `cargo xtask bundle-windows` (staged dir) and `cargo xtask package-windows-msi` (Task 6).
- Produces: a `WaveConductor-<version>-x86_64.msi` release artifact.

- [ ] **Step 1: Install WiX + cargo-wix on the Windows runner**

In `.github/workflows/release.yml`, in the `build` job (after the `Swatinem/rust-cache@v2` step, around line 286), add a Windows-only step:

```yaml
      - name: Install WiX Toolset + cargo-wix (Windows)
        if: runner.os == 'Windows'
        shell: pwsh
        run: |
          choco install wixtoolset --no-progress -y
          cargo install cargo-wix --version ^0.3 --locked
```

- [ ] **Step 2: Confirm the DLL coverage from the staged bundle**

Immediately after the existing `Build + bundle` step (line ~303), add a Windows-only assertion step so a missing ORT DLL fails the release loudly (the DLL-set pinning gate from the spec):

```yaml
      - name: Verify staged runtime DLLs (Windows)
        if: runner.os == 'Windows'
        shell: pwsh
        run: |
          $report = cargo xtask bundle-windows --skip-build --json | ConvertFrom-Json
          Write-Host "Staged runtime DLLs: $($report.runtime_dlls -join ', ')"
          if (-not ($report.runtime_dlls -contains 'onnxruntime.dll')) {
            throw "onnxruntime.dll was not staged; ORT DLL glob needs widening (see xtask/src/bundle/windows.rs)"
          }
          if (-not ($report.runtime_dlls -contains 'DirectML.dll')) {
            throw "DirectML.dll was not staged"
          }
```

- [ ] **Step 3: Build the MSI and upload it**

After the existing `Archive (Windows)` step (line ~316), add:

```yaml
      - name: Package MSI (Windows)
        if: runner.os == 'Windows'
        shell: pwsh
        run: cargo xtask package-windows-msi --version "${{ needs.prepare.outputs.version }}"

      - name: Upload MSI artifact (Windows)
        if: runner.os == 'Windows'
        uses: actions/upload-artifact@v7
        with:
          name: WaveConductor-${{ needs.prepare.outputs.version }}-x86_64.msi
          path: WaveConductor-${{ needs.prepare.outputs.version }}-x86_64.msi
          if-no-files-found: error
```

> The `needs.prepare.outputs.version` reference matches the tag validated in the `prepare` job (`release.yml` line ~69). If that output has a different name, use it verbatim — the `msi_version` mapping normalizes whatever tag string is passed.

- [ ] **Step 4: Ensure the publish job uploads the MSI**

In the `publish` job's release-creation/upload step (around line 368-378, the `gh release create` invocation), confirm the artifact glob includes `*.msi`. If the command lists explicit files, add the MSI; if it globs a directory (e.g. `dist/*`), ensure the downloaded MSI artifact lands there. Concretely, after the `Download all artifacts` step, the MSI directory is `WaveConductor-<version>-x86_64.msi/`; include its contents in the `gh release create ... <files>` arguments alongside the existing `.zip` / `.tar.gz` archives.

- [ ] **Step 5: Validate the workflow YAML locally**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/release.yml')); print('yaml OK')"`
Expected: `yaml OK`.

> **CI/manual verification (cannot run on the dev host):** trigger the `release.yml` workflow (dispatch, `publish: false`) on a branch. Confirm: the Windows `build` job installs WiX, stages the ORT DLLs (the verify step passes), `package-windows-msi` produces the `.msi`, and it uploads as an artifact. Then, on the Windows deployment machine, run the manual install checklist below.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/release.yml
git commit -F- <<'EOF'
ci(release): build + publish the Windows MSI

Extends the existing Windows build job (no new job): installs WiX +
cargo-wix, asserts the ORT DirectML DLLs were staged, runs
package-windows-msi, and uploads the .msi alongside the portable zip. The
publish job attaches the MSI to the pre-release.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Manual Windows install checklist (deployment machine)

Not automatable from the dev host. Run once on the Windows deployment machine after CI produces an MSI:

- [ ] Install the MSI; completes without error, app lands in `C:\Program Files\WaveConductor`.
- [ ] Launch from the Start Menu shortcut; **no console window appears**.
- [ ] The exe shows the app icon in Explorer and the Start Menu; Add/Remove Programs shows the icon + version.
- [ ] A log file appears at `%LOCALAPPDATA%\WaveConductor\logs\waveconductor.<date>.log` and receives lines during a session.
- [ ] Hand tracking (Ultraleap) initializes; the camera/MediaPipe provider initializes (all DLLs resolve at runtime — no "DLL not found" error).
- [ ] Force a panic (or check after any crash): `%LOCALAPPDATA%\WaveConductor\logs\panic.log` captures it.
- [ ] Uninstall via Add/Remove Programs; files and the Start Menu shortcut are removed.
- [ ] Install a higher version number over the existing install; it upgrades in place (no duplicate entry).

---

## Self-Review

**Spec coverage:**
- Console suppression → Task 2. ✓
- On-disk log + panic hook (`tracing-appender`, `%LOCALAPPDATA%`, non-blocking, guard) → Task 1. ✓
- Icon + version resource (`winresource`, PNG→ICO) → Task 3. ✓
- Stage ORT DirectML DLLs (glob, report, verification pinning) → Task 4 + Task 7 Step 2. ✓
- `+crt-static` (primary) + merge-module fallback (documented) → Task 5. ✓
- `package-windows-msi` xtask (`--json`, staged-dir guard, agent-first registration) → Task 6. ✓
- WiX source (UpgradeCode, Program Files, Start Menu shortcut, harvested files, signing seam) → Task 6 Step 6. ✓
- MSI version mapping (numeric-only, ≤255, unit-tested) → Task 6 Steps 1-3. ✓
- Keep the portable zip → untouched (Task 7 leaves the existing Archive/upload steps in place). ✓
- CI-built, no new job → Task 7 (steps on the existing `build`/`publish` jobs). ✓
- Signing = roadmap (seam left open) → Task 6 wxs comment + Task 7 (no sign step). ✓
- Manual Windows verification checklist → dedicated section. ✓

**Placeholder scan:** The only intentional fill-in is the `UpgradeCode` GUID, which Task 6 Step 6 generates with `uuidgen` (not a plan placeholder — a per-project constant minted during implementation). No TBD/TODO/"handle edge cases" remain.

**Type consistency:** `StageReport.runtime_dlls: Vec<String>` defined in Task 4 Step 1, set in linux.rs (Task 4 Step 2) and windows.rs (Task 4 Step 5), read in Task 4 Step 7 and Task 7 Step 2. `msi_version(&str) -> Result<String, String>` defined and consumed within Task 6. `init_tracing` return type change (Task 1 Step 7) is matched at its sole call site in the same step.
