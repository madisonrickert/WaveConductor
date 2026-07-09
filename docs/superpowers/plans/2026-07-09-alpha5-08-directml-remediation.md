# DirectML Remediation Ladder Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Determine, with a self-contained probe tool and a falsifiable experiment, whether the tester's DirectML `commit` crash is caused by the palm model's CoreML-motivated rank-3 `PRelu` slopes, and land the cheapest fix that keeps the field tester's iGPU accelerated. CPU is the floor (Plan 06 already guarantees it); this plan tries to avoid settling for it.

**Architecture:** A new `cargo xtask probe-ep` subcommand builds an ONNX Runtime session against a chosen execution provider (DirectML on Windows, CoreML on macOS, CPU everywhere) at a chosen graph-optimization level, and reports — as JSON — whether registration and `commit_from_memory` succeed, the exact error string on failure, the model's I/O metadata, and mean inference latency over N runs. `xtask` depends on neither Bevy nor `wc-core` (its deps are `clap`, `ignore`, `regex`, `image`, `serde`, `serde_json`, `toml`, `naga`), so it builds in minutes on a fresh Windows clone. The tool is written and exercised on macOS (CoreML + CPU) until known-good, then run once on Windows against all three vendored models. A three-way decision gate on the rung-0 result selects the fix.

**Tech Stack:** Rust, `clap` (derive), `ort` (`=2.0.0-rc.12`, the workspace pin — reused, not a new dependency), ONNX Runtime C++ backend (pyke's `download-binaries` build), DirectML (Windows), CoreML (macOS).

**Branch:** `windows-directml-prelu-rank`. Nothing speculative merges to `v5-alpha` (or the `windows-remediation` line) until the probe returns data from a real DirectML device. The model swap could regress Windows further, and rank-4 slopes would definitely regress CoreML if they leaked to macOS, so the whole investigation — probe tool, model override, graph-opt flags, and any candidate fix — stays quarantined on this branch until the experiment decides.

## Global Constraints

Copied verbatim from `AGENTS.md` and Part 1 of the program index (`docs/superpowers/plans/2026-07-09-alpha5-program-index.md`). Every task's requirements implicitly include this section.

- **CI gates, all of which must pass before a task is complete** (`.github/workflows/ci.yml`):
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features --workspace -- -D warnings`
  - `cargo nextest run --workspace --all-features` (nextest skips doctests; cover those with `cargo test --doc --workspace`)
  - `cargo test --doc --workspace`
  - `cargo doc --no-deps --workspace --document-private-items` (CI runs with `RUSTDOCFLAGS="-D warnings"`; broken intra-doc links are hard errors; **no `--all-features`**, and a **public** item's rustdoc linking to a `pub(crate)` item trips `rustdoc::private_intra_doc_links` — demote to a plain code span)
  - `cargo deny check`
  - `cargo xtask check-secrets` (blocks developer home-dir paths `/Users/...`, `/home/...`, `C:\Users\...`, email addresses, and the AWS/GitHub/`sk-`/bearer-token secret prefixes; scans the whole tree except `vendor/`, `target/`, `.git/`, and `docs/superpowers/`)
- **The per-task clippy gate must use `--all-targets`.** `cargo clippy -p xtask --lib` skips the test target; CI runs `--all-targets`. Always `cargo clippy -p xtask --all-targets --all-features -- -D warnings`.
- **Clippy is `-D warnings` over `pedantic`, including inside `#[cfg(test)]`.** `Cargo.toml:206-211` sets `pedantic = warn` plus `unwrap_used`, `expect_used`, `panic`, and `as_conversions` at `warn`; CI escalates all warnings to errors. In your own example code:
  - No `.unwrap()` / `.expect()` in non-test code. In tests, add a module-level `#[allow(clippy::expect_used, reason = "…")]` (the existing xtask test modules in `bundle/windows.rs`, `manifest.rs`, and `tests/manifest.rs` all do this) or use `let … else` / `assert!(matches!(…))`.
  - `assert_eq!(x.is_some(), true)` → `clippy::bool_assert_comparison`. Use `assert!(x.is_some())`.
  - `0..(N + 1)` → `clippy::range_plus_one`. Use `0..=N`.
  - **No `as` casts on numeric types** where `From` / `TryFrom` / `u32::try_from` would work (`clippy::as_conversions` is denied). Use `usize::try_from`, `f64::from`, `Duration::as_secs_f64`.
- **`///` rustdoc on every public item** (struct, enum, trait, fn, module). Module-level `//!` on every module root describing role and data flow. **Never strip comments during refactors.**
- **Public API at the top, private helpers at the bottom, tests in a `#[cfg(test)] mod tests` block at the file footer.** One concept per file (~300 lines is a guideline, not a hard cap).
- **No new dependencies.** Reuse the workspace `ort` pin (`=2.0.0-rc.12`); adding per-target `ort` features to `xtask` is not a new dependency — it mirrors exactly what `crates/wc-core/Cargo.toml` already does (`coreml` under the macOS target, `directml` under the Windows target). Avoid pulling in any other crate (`libc`, `os_pipe`, `gag`, etc.); the pure work here needs only `std` + `regex` (already an xtask dep).
- **No hardcoded developer home paths.** Paths come from `CARGO_MANIFEST_DIR` resolution or CLI arguments. `cargo xtask check-secrets` blocks `/Users/...` etc.
- **Agent-first xtask CLI conventions.** Every subcommand accepts `--json`, appears in `cargo xtask manifest` (and its `xtask/tests/manifest.rs` cross-check), and supports `--help`. Follow the existing subcommand shape (`validate_shaders.rs` is the closest precedent: `#[derive(clap::Args)] pub struct Args`, `pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>>`, `--json` branch, hand-emitted JSON via `crate::util::json_escape`).
- **Commit messages: `git commit -F <file>`, never `-m`** (backticks in a `-m` string are command-substituted by zsh). **Stage named paths only** (`git add <path> …`); **never `git add -A`**. After each commit, `git show --stat HEAD` to confirm the stage.

### DirectML claims are hypotheses, not facts

The plan author cannot run DirectML (macOS dev box). **Every statement in this plan about what DirectML accepts or rejects is a hypothesis the probe exists to test.** Nobody has demonstrated that DirectML rejects rank-3 `PRelu` slopes. What is *established* (verified by static analysis, below) is only that the shipped palm model differs from its unmodified upstream original in exactly one respect — a `PRelu` slope-rank change made for CoreML — and that the model with no `PRelu` (`hand_landmark.onnx`) carries none of the suspect ops. Write every DirectML assertion as "the probe will show whether…", never "DirectML does…".

### Verified static analysis of the vendored models (macOS, `onnx` via `uv`)

Independently confirmed for this plan (`uv run --with onnx python -c …` over `assets/models/hand/`). The ONNX op is spelled **`PRelu`**, not `PReLU`; grepping the wrong spelling returns zero and nearly killed this hypothesis once.

| Model | `PRelu` nodes | Slope shapes (count) | Rank | Pad / Resize / Concat | Conv |
| --- | --- | --- | --- | --- | --- |
| `palm_detection.onnx` (shipped) | 26 | `(32,1,1)`×4, `(64,1,1)`×4, `(128,1,1)`×7, `(256,1,1)`×11 | **3** | 3 / 2 / 2 | 53 |
| `palm_detection_original.onnx` | 26 | `(1,32,1,1)`×4, `(1,64,1,1)`×4, `(1,128,1,1)`×7, `(1,256,1,1)`×11 | **4** | 3 / 2 / 2 | 53 |
| `hand_landmark.onnx` | 0 | — | — | 0 / 0 / 0 | 47 |

Declared I/O (both palm variants identical): input `input_1` `[1,192,192,3]` f32 → outputs `Identity` `[1,2016,18]`, `Identity_1` `[1,2016,1]`. Landmark: input `input_1` `[1,224,224,3]` f32 → four outputs (`[1,63]`, `[1,1]`, `[1,1]`, `[1,63]`).

The sole delta between the two palm variants is the slope rank, introduced in commit `d2369f4f` **for CoreML**, whose NeuralNetwork EP requires `[C,1,1]` or a scalar and rejects `[1,C,1,1]` (`docs/runbooks/onnx-coreml-model-surgery.md:120-129`). That surgery predates any Windows GPU-inference build. `crates/wc-core/src/input/providers/mediapipe/mod.rs:276-277` loads palm **before** landmark, and the tester's log shows exactly one initialization exception before the provider bails — so the model that threw is the surgically altered palm detector, and the `PRelu`-free landmark was never reached.

### Anchor corrections (verified while writing this plan)

- The program index and spec cite `xtask/src/bundle/windows.rs:244` as "the staged `DirectML.dll`." **Line 244 is a test literal** (`runtime_dlls: vec!["DirectML.dll".to_string()]`). The actual DLL-staging logic is `xtask/src/bundle/windows.rs:136-164` — a `read_dir` loop that copies any `onnxruntime*.dll` (`is_ort`, line 155-156) or `directml.dll` (`is_directml`, line 157) that pyke's `ort` build script dropped next to the release binary. Rung 1 (swap a newer `DirectML.dll`) targets that staged file, not line 244.
- `mediapipe/mod.rs:276-277` (palm before landmark) — confirmed.
- Workspace `ort` pin `= { version = "=2.0.0-rc.12", features = ["download-binaries"] }` — confirmed at `Cargo.toml:180`.
- `xtask/Cargo.toml` has no Bevy and no `wc-core` dependency — confirmed.

---

## Task 1: Create the quarantine branch

**Files:** none (git only).

**Interfaces:**
- Consumes: the current `windows-remediation` line (or `v5-alpha` if that branch does not yet exist locally).
- Produces: a checked-out `windows-directml-prelu-rank` branch.

This is the first task deliberately: the whole investigation is self-contained here and nothing merges out until the probe returns data.

- [ ] **Step 1: Branch from the remediation line**

The DirectML branch descends from `windows-remediation` per the spec's branch diagram. That branch may not exist yet (Plans 01–09 may still be landing on `v5-alpha`). Branch from `windows-remediation` if present, else from `v5-alpha`:

```bash
git fetch --all --quiet
if git show-ref --verify --quiet refs/heads/windows-remediation; then
  git checkout windows-remediation
elif git show-ref --verify --quiet refs/remotes/origin/windows-remediation; then
  git checkout -b windows-remediation origin/windows-remediation
else
  git checkout v5-alpha
fi
git checkout -b windows-directml-prelu-rank
git rev-parse --abbrev-ref HEAD   # expect: windows-directml-prelu-rank
```

- [ ] **Step 2: Record the quarantine rule where the next agent will see it**

No commit yet (nothing changed). The rule "nothing speculative merges until the probe returns data" is enforced by keeping every subsequent task on this branch and by the decision gate below. Do not open a PR against `v5-alpha` at any point in Tasks 2–6.

---

## Task 2: Wire `ort` into `xtask` and register the `probe-ep` subcommand skeleton

**Files:**
- Modify: `xtask/Cargo.toml` (add per-target `ort`)
- Create: `xtask/src/probe_ep.rs` (module skeleton — args + a stub `run` that compiles)
- Modify: `xtask/src/main.rs` (declare `mod probe_ep;`, add the `ProbeEp` variant, dispatch it)
- Modify: `xtask/src/manifest.rs` (add the `probe-ep` entry to `SUBCOMMANDS`)
- Modify: `xtask/tests/manifest.rs` (add `"probe-ep"` to `EXPECTED_SUBCOMMANDS`)

**Interfaces:**
- Consumes: nothing.
- Produces: `probe_ep::Args` (clap), `pub fn probe_ep::run(args: probe_ep::Args) -> Result<(), Box<dyn std::error::Error>>`.

**Why per-target `ort` features.** The workspace pin already carries `download-binaries`. Inheriting it with `ort = { workspace = true }` (base) links pyke's prebuilt ONNX Runtime + CPU EP everywhere. The GPU EPs are opt-in per target, exactly as `crates/wc-core/Cargo.toml` does it: `directml` only under `cfg(target_os = "windows")`, `coreml` only under `cfg(target_os = "macos")`. Cargo unions features across the three tables, so a macOS build gets `download-binaries` + `coreml`, a Windows build gets `download-binaries` + `directml`, and neither pulls the other's EP.

- [ ] **Step 1: Add `ort` to `xtask/Cargo.toml`**

Append to the `[dependencies]` table (after `naga`):

```toml
# ONNX Runtime, reusing the workspace pin (=2.0.0-rc.12 + download-binaries).
# Not a new dependency: it is the same crate/version wc-core already links.
# Used only by `probe-ep` to build sessions against a chosen execution provider
# and measure whether commit succeeds + inference latency. The GPU execution
# providers are added per-target below (mirroring crates/wc-core/Cargo.toml):
# DirectML on Windows, CoreML on macOS, CPU everywhere via the base dep.
ort = { workspace = true }
```

Then add the two target-specific tables (place them after the `[dev-dependencies]` table):

```toml
# Windows GPU inference EP for `probe-ep --ep directml`. Unions with the base
# `ort` dep. Mirrors crates/wc-core/Cargo.toml's windows-target ort entry.
[target.'cfg(target_os = "windows")'.dependencies]
ort = { workspace = true, features = ["directml"] }

# macOS GPU inference EP for `probe-ep --ep coreml`, so the probe is exercised
# known-good on the dev box before it ever sees Windows. Mirrors wc-core.
[target.'cfg(target_os = "macos")'.dependencies]
ort = { workspace = true, features = ["coreml"] }
```

- [ ] **Step 2: Create the module skeleton**

Create `xtask/src/probe_ep.rs`. This first version only needs to compile and dispatch; the real logic lands in Tasks 3–5.

```rust
//! `cargo xtask probe-ep` — build an ONNX Runtime session against a chosen
//! execution provider and report, as JSON, whether it commits, the exact error
//! on failure, the model's I/O metadata, and mean inference latency.
//!
//! ## Why this exists
//!
//! Field testing of v5.0.0-alpha.4 on an AMD Vega 10 iGPU crashed the DirectML
//! execution provider inside `DmlGraphFusionHelper` (`0x80004005`) at session
//! commit, taking all hand tracking with it. This probe isolates that failure
//! from the rest of the app: `xtask` depends on neither Bevy nor `wc-core`, so
//! it builds in minutes on a fresh Windows clone and the field tester runs one
//! command instead of iterating on a full app build.
//!
//! ## What it measures
//!
//! For one model and one execution provider, at one graph-optimization level:
//! whether the EP *registers*, whether `commit_from_memory` *succeeds*, the
//! exact error string if it does not, the model's declared input/output names
//! and shapes, and the mean wall-clock latency of `session.run` over N runs
//! (only when commit succeeds). Node-placement and partition counts are
//! best-effort, parsed from an ONNX Runtime verbose capability log the operator
//! captures out-of-band (see `--capability-log`); ONNX Runtime does not expose
//! them through `ort`'s Rust API, and capturing the C library's stderr in-process
//! would require a new dependency this crate deliberately avoids.
//!
//! The rung-0 decision (`docs/superpowers/plans/2026-07-09-alpha5-08-directml-remediation.md`)
//! turns solely on the `commit` verdict of the surgered vs. original palm model,
//! which is always in-band and reliable; partition counts are diagnostic colour.

use std::path::PathBuf;

use clap::Args as ClapArgs;

/// Arguments for the probe-ep subcommand.
#[derive(ClapArgs)]
pub struct Args {
    /// Path to the `.onnx` model to probe.
    #[arg(long)]
    pub model: PathBuf,

    /// Execution provider to build the session against.
    #[arg(long, value_enum)]
    pub ep: Ep,

    /// Graph optimization level (default: level3, matching production).
    #[arg(long, value_enum, default_value_t = GraphOpt::Level3)]
    pub graph_opt: GraphOpt,

    /// Comma-separated list of named ONNX Runtime optimizers to disable
    /// (rung 3: `optimization.disable_specified_optimizers`).
    #[arg(long)]
    pub disable_optimizers: Option<String>,

    /// Number of timed inference runs for the latency mean (after warmup).
    #[arg(long, default_value_t = 50)]
    pub runs: u32,

    /// Optional path to a previously captured ONNX Runtime verbose log
    /// (`ORT_LOG=verbose RUST_LOG=ort=trace … 2> cap.log`). When given, the
    /// probe parses partition and node-placement counts from it into the JSON.
    #[arg(long)]
    pub capability_log: Option<PathBuf>,

    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}

/// Execution provider selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Ep {
    /// Windows DirectML (DX12). Only registerable on a Windows build.
    Directml,
    /// macOS CoreML (ANE/GPU/CPU). Only registerable on a macOS build.
    Coreml,
    /// ONNX Runtime CPU EP. Registerable on every platform.
    Cpu,
}

/// Graph optimization level selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum GraphOpt {
    /// Disable all graph optimizations.
    Disable,
    /// Basic (level-1) optimizations only.
    Level1,
    /// All optimizations (ONNX Runtime default).
    Level3,
}

/// Execute the probe-ep subcommand.
///
/// # Errors
/// Returns `Err` only for genuine probe failures (unreadable model, an EP not
/// available on this platform). A model that *registers* but fails to *commit*
/// is a valid, expected result reported in the output with exit 0 — that is the
/// whole point of the tool.
pub fn run(_args: Args) -> Result<(), Box<dyn std::error::Error>> {
    // Real implementation lands in Tasks 3-5.
    Err("probe-ep: not yet implemented".into())
}
```

- [ ] **Step 3: Register the subcommand in `main.rs`**

In `xtask/src/main.rs`, add `mod probe_ep;` to the module list (after `mod msi;`), add the enum variant to `Command` (after `ValidateShaders`), and dispatch it in `main`:

```rust
    /// Probe an ONNX execution provider against a model (DirectML/CoreML/CPU).
    ProbeEp(probe_ep::Args),
```

and in the `match cli.command` block, after the `ValidateShaders` arm:

```rust
        Command::ProbeEp(args) => probe_ep::run(args),
```

- [ ] **Step 4: Add the manifest entry**

In `xtask/src/manifest.rs`, append to the `SUBCOMMANDS` table (after the `validate-shaders` entry):

```rust
    Entry {
        name: "probe-ep",
        description: "Probe an ONNX execution provider (DirectML/CoreML/CPU) against a model.",
    },
```

In `xtask/tests/manifest.rs`, append `"probe-ep"` to `EXPECTED_SUBCOMMANDS` (after `"validate-shaders"`). The JSON test asserts *ordered* equality, so it must be last in both lists, matching the `Command` enum order.

- [ ] **Step 5: Build, and run the manifest cross-check**

```bash
cargo build -p xtask
cargo test -p xtask --test manifest
cargo run -p xtask -- probe-ep --help
cargo tree -p xtask -i ort 2>&1 | head -5    # expect: ort v2.0.0-rc.12
```

Expected: builds; the three manifest cross-check tests pass; `--help` shows `--model`, `--ep`, `--graph-opt`, `--disable-optimizers`, `--runs`, `--capability-log`, `--json`; `ort` resolves to `2.0.0-rc.12`.

> If `cargo build -p xtask` fails on macOS because the base `ort = { workspace = true }` inherits only `download-binaries` (no EP) — that is fine; CPU is built into `download-binaries`. If it fails resolving `ort::ep::CoreML`, confirm the macOS target table in Step 1 was added.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p xtask --all-targets --all-features -- -D warnings
git add xtask/Cargo.toml xtask/src/probe_ep.rs xtask/src/main.rs xtask/src/manifest.rs xtask/tests/manifest.rs
git commit -F - <<'EOF'
feat(xtask): scaffold probe-ep subcommand + wire ort per-target

Adds the probe-ep subcommand skeleton (args, enums, stub run) and links
ort (=2.0.0-rc.12, the workspace pin) into xtask with per-target GPU EP
features: directml under the Windows target, coreml under the macOS
target, CPU everywhere via download-binaries. xtask depends on neither
Bevy nor wc-core, so this stays a minutes-long build on a fresh clone.

Registered in the Command enum, the manifest table, and the manifest
cross-check test. Real session-build/commit/latency logic follows.
EOF
git show --stat HEAD
```

---

## Task 3: Pure probe helpers — enums, shape math, and JSON serialization (TDD)

**Files:**
- Modify: `xtask/src/probe_ep.rs` (add pure helpers + a `#[cfg(test)] mod tests` footer)

**Interfaces:**
- Consumes: `crate::util::json_escape`.
- Produces:
  - `fn Ep::as_str(self) -> &'static str`
  - `fn GraphOpt::as_str(self) -> &'static str`
  - `fn concrete_dims(dims: &[i64]) -> Vec<i64>`
  - `fn element_count(dims: &[i64]) -> Result<usize, String>`
  - `struct ProbeReport` (plain fields only — no `ort` types) + `fn probe_report_json(r: &ProbeReport) -> String`

Everything here is `ort`-free so it unit-tests without a session or a GPU. The session code in Task 4 only *populates* a `ProbeReport`; serialization and shape math stay pure.

- [ ] **Step 1: Write the failing tests**

Add this test module to the footer of `xtask/src/probe_ep.rs`:

```rust
#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "expect is appropriate in xtask test code (matches the other xtask test modules)"
)]
mod tests {
    use super::*;

    #[test]
    fn ep_and_graph_opt_render_stable_lowercase_tokens() {
        assert_eq!(Ep::Directml.as_str(), "directml");
        assert_eq!(Ep::Coreml.as_str(), "coreml");
        assert_eq!(Ep::Cpu.as_str(), "cpu");
        assert_eq!(GraphOpt::Disable.as_str(), "disable");
        assert_eq!(GraphOpt::Level1.as_str(), "level1");
        assert_eq!(GraphOpt::Level3.as_str(), "level3");
    }

    #[test]
    fn concrete_dims_replaces_non_positive_dims_with_one() {
        // A declared Outlet may carry -1 (dynamic) dims; the probe needs a
        // concrete shape to allocate a zeroed input. Static hand-model shapes
        // pass through unchanged.
        assert_eq!(concrete_dims(&[1, 192, 192, 3]), vec![1, 192, 192, 3]);
        assert_eq!(concrete_dims(&[-1, 224, 224, 3]), vec![1, 224, 224, 3]);
        assert_eq!(concrete_dims(&[0, 5]), vec![1, 5]);
    }

    #[test]
    fn element_count_multiplies_concrete_dims() {
        assert_eq!(element_count(&[1, 192, 192, 3]).expect("count"), 110_592);
        assert_eq!(element_count(&[1, 1]).expect("count"), 1);
    }

    #[test]
    fn element_count_rejects_a_non_concrete_dim() {
        let err = element_count(&[-1, 3]).expect_err("dynamic dim must be rejected");
        assert!(err.contains("dim"), "error should name the bad dim: {err}");
    }

    #[test]
    fn json_reports_a_successful_commit_with_metadata_and_latency() {
        let report = ProbeReport {
            model: "palm_detection.onnx".to_string(),
            model_path: "assets/models/hand/palm_detection.onnx".to_string(),
            ep: "directml".to_string(),
            graph_opt: "level3".to_string(),
            disable_optimizers: None,
            platform: "windows".to_string(),
            register: "ok".to_string(),
            register_error: None,
            commit: "ok".to_string(),
            commit_error: None,
            input_name: Some("input_1".to_string()),
            input_shape: Some(vec![1, 192, 192, 3]),
            output_names: Some(vec!["Identity".to_string(), "Identity_1".to_string()]),
            latency_ms_mean: Some(7.5),
            latency_runs: 50,
            partitions: None,
            node_placement: Vec::new(),
        };
        let json = probe_report_json(&report);
        assert!(json.contains(r#""commit":"ok""#), "{json}");
        assert!(json.contains(r#""commit_error":null"#), "{json}");
        assert!(json.contains(r#""input_shape":[1,192,192,3]"#), "{json}");
        assert!(json.contains(r#""latency_ms_mean":7.5"#), "{json}");
        assert!(json.contains(r#""partitions":null"#), "{json}");
    }

    #[test]
    fn json_reports_a_commit_error_verbatim_and_escaped() {
        // The rung-0 signal: DirectML registers, then throws at commit. The
        // exact error string must survive into JSON (quotes/backslashes escaped).
        let report = ProbeReport {
            model: "palm_detection.onnx".to_string(),
            model_path: r"C:\models\palm_detection.onnx".to_string(),
            ep: "directml".to_string(),
            graph_opt: "level3".to_string(),
            disable_optimizers: None,
            platform: "windows".to_string(),
            register: "ok".to_string(),
            register_error: None,
            commit: "error".to_string(),
            commit_error: Some(r#"DmlGraphFusionHelper: "0x80004005""#.to_string()),
            input_name: None,
            input_shape: None,
            output_names: None,
            latency_ms_mean: None,
            latency_runs: 50,
            partitions: None,
            node_placement: Vec::new(),
        };
        let json = probe_report_json(&report);
        assert!(json.contains(r#""commit":"error""#), "{json}");
        assert!(json.contains(r#"0x80004005"#), "{json}");
        assert!(json.contains(r#""latency_ms_mean":null"#), "{json}");
        // Backslash in the Windows path is doubled; the result is valid JSON.
        assert!(json.contains(r"C:\\models\\palm_detection.onnx"), "{json}");
    }

    #[test]
    fn json_lists_node_placement_pairs() {
        let report = ProbeReport {
            model: "m.onnx".to_string(),
            model_path: "m.onnx".to_string(),
            ep: "directml".to_string(),
            graph_opt: "level3".to_string(),
            disable_optimizers: Some("ConstantFolding".to_string()),
            platform: "windows".to_string(),
            register: "ok".to_string(),
            register_error: None,
            commit: "ok".to_string(),
            commit_error: None,
            input_name: Some("input_1".to_string()),
            input_shape: Some(vec![1, 3]),
            output_names: Some(vec!["out".to_string()]),
            latency_ms_mean: Some(1.0),
            latency_runs: 1,
            partitions: Some(6),
            node_placement: vec![
                ("DmlExecutionProvider".to_string(), 117),
                ("CPUExecutionProvider".to_string(), 7),
            ],
        };
        let json = probe_report_json(&report);
        assert!(json.contains(r#""partitions":6"#), "{json}");
        assert!(json.contains(r#""disable_optimizers":"ConstantFolding""#), "{json}");
        assert!(
            json.contains(r#"{"ep":"DmlExecutionProvider","nodes":117}"#),
            "{json}"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p xtask --lib probe_ep 2>&1 | head -20
```

Expected: FAIL to compile — `cannot find function concrete_dims` / `cannot find type ProbeReport` / `no function or associated item named as_str`.

- [ ] **Step 3: Write the helpers**

In `xtask/src/probe_ep.rs`, add `use crate::util::json_escape;` to the imports, add `use std::fmt::Write as _;`, and insert these items above the `#[cfg(test)]` module (below `run`):

```rust
impl Ep {
    /// Stable lowercase token for JSON output and diagnostics.
    fn as_str(self) -> &'static str {
        match self {
            Ep::Directml => "directml",
            Ep::Coreml => "coreml",
            Ep::Cpu => "cpu",
        }
    }
}

impl GraphOpt {
    /// Stable lowercase token for JSON output and diagnostics.
    fn as_str(self) -> &'static str {
        match self {
            GraphOpt::Disable => "disable",
            GraphOpt::Level1 => "level1",
            GraphOpt::Level3 => "level3",
        }
    }
}

/// Replace non-positive (dynamic/unknown) dimensions with 1 so a concrete
/// zeroed input tensor can be allocated. The vendored hand models declare fully
/// static shapes, so this is a no-op for them; it guards a future model swap.
fn concrete_dims(dims: &[i64]) -> Vec<i64> {
    dims.iter().map(|&d| if d > 0 { d } else { 1 }).collect()
}

/// Total element count of a concrete shape, or an error naming a non-concrete
/// dimension. Overflow is an error, not a wrap (a probe must fail loudly).
fn element_count(dims: &[i64]) -> Result<usize, String> {
    let mut n: usize = 1;
    for &d in dims {
        let d = usize::try_from(d).map_err(|_| format!("non-concrete dim {d}"))?;
        n = n
            .checked_mul(d)
            .ok_or_else(|| "element count overflow".to_string())?;
    }
    Ok(n)
}

/// One probe result, in plain types only (no `ort` types) so it serializes and
/// unit-tests without a session. The session code populates it; the decision
/// gate reads `commit` + `commit_error`.
pub struct ProbeReport {
    /// Model file basename.
    pub model: String,
    /// Model path as supplied on the command line.
    pub model_path: String,
    /// Requested execution provider (`directml` / `coreml` / `cpu`).
    pub ep: String,
    /// Requested graph optimization level.
    pub graph_opt: String,
    /// Comma-separated disabled optimizers, if any (rung 3).
    pub disable_optimizers: Option<String>,
    /// Host platform (`std::env::consts::OS`).
    pub platform: String,
    /// EP registration outcome: `ok` / `error` / `n/a` (CPU needs no register).
    pub register: String,
    /// Exact EP registration error, if registration failed.
    pub register_error: Option<String>,
    /// Session commit outcome: `ok` / `error` / `skipped`.
    pub commit: String,
    /// Exact `commit_from_memory` error, if commit failed. The rung-0 signal.
    pub commit_error: Option<String>,
    /// Declared input tensor name (present only when commit succeeded).
    pub input_name: Option<String>,
    /// Declared input shape (present only when commit succeeded).
    pub input_shape: Option<Vec<i64>>,
    /// Declared output tensor names (present only when commit succeeded).
    pub output_names: Option<Vec<String>>,
    /// Mean `session.run` latency in milliseconds over `latency_runs` runs
    /// (present only when commit succeeded).
    pub latency_ms_mean: Option<f64>,
    /// Number of timed runs behind `latency_ms_mean`.
    pub latency_runs: u32,
    /// Partition count parsed from a verbose capability log, if supplied.
    pub partitions: Option<u32>,
    /// Per-EP node-placement counts parsed from a verbose capability log.
    pub node_placement: Vec<(String, u32)>,
}

/// Serialize a [`ProbeReport`] to a single-line JSON object.
///
/// Hand-emitted (matching the other xtask subcommands) rather than via a derive,
/// so `xtask` keeps its small dependency set. Strings are escaped with
/// [`json_escape`]; absent fields render as `null`.
pub fn probe_report_json(r: &ProbeReport) -> String {
    let opt_str = |v: &Option<String>| match v {
        Some(s) => format!("\"{}\"", json_escape(s)),
        None => "null".to_string(),
    };
    let opt_f64 = |v: Option<f64>| match v {
        Some(x) => format!("{x}"),
        None => "null".to_string(),
    };
    let opt_u32 = |v: Option<u32>| match v {
        Some(x) => format!("{x}"),
        None => "null".to_string(),
    };
    let i64_array = |v: &Option<Vec<i64>>| match v {
        Some(xs) => {
            let inner: Vec<String> = xs.iter().map(ToString::to_string).collect();
            format!("[{}]", inner.join(","))
        }
        None => "null".to_string(),
    };
    let str_array = |v: &Option<Vec<String>>| match v {
        Some(xs) => {
            let inner: Vec<String> = xs
                .iter()
                .map(|s| format!("\"{}\"", json_escape(s)))
                .collect();
            format!("[{}]", inner.join(","))
        }
        None => "null".to_string(),
    };
    let placement: Vec<String> = r
        .node_placement
        .iter()
        .map(|(ep, n)| format!("{{\"ep\":\"{}\",\"nodes\":{n}}}", json_escape(ep)))
        .collect();

    let mut out = String::new();
    let _ = write!(
        out,
        concat!(
            "{{\"model\":\"{}\",\"model_path\":\"{}\",\"ep\":\"{}\",",
            "\"graph_opt\":\"{}\",\"disable_optimizers\":{},\"platform\":\"{}\",",
            "\"register\":\"{}\",\"register_error\":{},",
            "\"commit\":\"{}\",\"commit_error\":{},",
            "\"input_name\":{},\"input_shape\":{},\"output_names\":{},",
            "\"latency_ms_mean\":{},\"latency_runs\":{},",
            "\"partitions\":{},\"node_placement\":[{}]}}"
        ),
        json_escape(&r.model),
        json_escape(&r.model_path),
        json_escape(&r.ep),
        json_escape(&r.graph_opt),
        opt_str(&r.disable_optimizers),
        json_escape(&r.platform),
        json_escape(&r.register),
        opt_str(&r.register_error),
        json_escape(&r.commit),
        opt_str(&r.commit_error),
        opt_str(&r.input_name),
        i64_array(&r.input_shape),
        str_array(&r.output_names),
        opt_f64(r.latency_ms_mean),
        r.latency_runs,
        opt_u32(r.partitions),
        placement.join(","),
    );
    out
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p xtask --lib probe_ep
```

Expected: PASS, 7 tests.

- [ ] **Step 5: Scoped gate and commit**

```bash
cargo fmt --all
cargo clippy -p xtask --all-targets --all-features -- -D warnings
cargo test -p xtask --lib probe_ep
git add xtask/src/probe_ep.rs
git commit -F - <<'EOF'
feat(xtask/probe-ep): pure helpers — enums, shape math, JSON report

ProbeReport holds plain types only (no ort types), so serialization and
input-shape math unit-test without a session or a GPU. The rung-0 signal
(commit ok/error + exact error string) round-trips through probe_report_json
with quotes and Windows-path backslashes escaped.
EOF
git show --stat HEAD
```

---

## Task 4: Build the session, register the EP, commit, and measure latency

**Files:**
- Modify: `xtask/src/probe_ep.rs` (replace the stub `run`; add `ort`-backed private helpers)

**Interfaces:**
- Consumes: `Ep`, `GraphOpt`, `concrete_dims`, `element_count`, `ProbeReport`, `probe_report_json` (Task 3).
- Produces: the working `run`, plus cfg-gated `register_directml` / `register_coreml`.

**ort rc.12 API (verified against the vendored crate source and `inference_ort.rs`):**
- `ort::session::Session::builder()?` → `SessionBuilder`.
- `.with_optimization_level(ort::session::builder::GraphOptimizationLevel::{Disable,Level1,Level3})?`, `.with_intra_threads(2)?`, `.with_intra_op_spinning(false)?`.
- DirectML requires `.with_parallel_execution(false)?.with_memory_pattern(false)?` (see `inference_ort::configure_accelerator_session`).
- Rung 3: `.with_disabled_optimizers("Comma,List")?`.
- `ort::ep::DirectML::default().register(&mut builder) -> Result<(), RegisterError>` (Windows); `ort::ep::CoreML::default().with_compute_units(ort::ep::coreml::ComputeUnits::All).register(&mut builder)` (macOS).
- `builder.commit_from_memory(&bytes)` consumes the builder → `Session`.
- `session.inputs() -> &[Outlet]`, `session.outputs() -> &[Outlet]`; `outlet.name() -> &str`; `outlet.dtype() -> &ValueType`; `ValueType::tensor_shape() -> Option<&Shape>`; `Shape: Deref<Target=[i64]>`.
- `ort::value::TensorRef::from_array_view((dims: &[i64], data: &[f32]))?`; `session.run(ort::inputs![name => tensor])?`.
- `ort::Error<R>` stringifies with `.to_string()` (generic over the recovery type `R`, as in `inference_ort::load_err`).

> **No GPU tests in CI, and the probe cannot be unit-tested end-to-end** — building a real ONNX Runtime session needs the downloaded native binary and (for latency) a live EP. So `run` has no new `#[cfg(test)]` assertions; it is verified by the human-run smoke in Task 5's macOS step and Task 6. This mirrors the repo's rule that a human runs the real thing (`cargo rund` / `cargo xtask capture`), never an agent asserting on rendered/inference output.

- [ ] **Step 1: Replace the stub `run` and add the session helpers**

Add these imports at the top of `xtask/src/probe_ep.rs` (below the existing `use` lines):

```rust
use std::time::Instant;

use ort::session::builder::{GraphOptimizationLevel, SessionBuilder};
use ort::session::Session;
use ort::value::TensorRef;
```

Replace the entire stub `pub fn run(...)` body with:

```rust
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let model_bytes = std::fs::read(&args.model)
        .map_err(|e| format!("probe-ep: cannot read model {}: {e}", args.model.display()))?;
    let model_name = args
        .model
        .file_name()
        .map_or_else(|| args.model.display().to_string(), |n| n.to_string_lossy().into_owned());

    let mut report = ProbeReport {
        model: model_name,
        model_path: args.model.display().to_string(),
        ep: args.ep.as_str().to_string(),
        graph_opt: args.graph_opt.as_str().to_string(),
        disable_optimizers: args.disable_optimizers.clone(),
        platform: std::env::consts::OS.to_string(),
        register: "n/a".to_string(),
        register_error: None,
        commit: "skipped".to_string(),
        commit_error: None,
        input_name: None,
        input_shape: None,
        output_names: None,
        latency_ms_mean: None,
        latency_runs: args.runs,
        partitions: None,
        node_placement: Vec::new(),
    };

    // Build the session options (graph-opt level, thread caps, EP-specific
    // session flags, and any rung-3 disabled optimizers).
    let mut builder = build_builder(&args)?;

    // Register the requested EP. A registration error is a *result*, not a probe
    // failure, EXCEPT when the EP is not compiled for this platform (that is an
    // operator error worth exiting non-zero for).
    match register_ep(args.ep, &mut builder) {
        Ok(RegisterOutcome::Cpu) => report.register = "n/a".to_string(),
        Ok(RegisterOutcome::Registered) => report.register = "ok".to_string(),
        Ok(RegisterOutcome::Failed(msg)) => {
            report.register = "error".to_string();
            report.register_error = Some(msg);
        }
        Err(unavailable) => return Err(unavailable.into()),
    }

    // Commit. This is the rung-0 signal for DirectML (registers, then throws in
    // DmlGraphFusionHelper). Capture success or the exact error as data.
    match builder.commit_from_memory(&model_bytes) {
        Ok(mut session) => {
            report.commit = "ok".to_string();
            fill_metadata(&session, &mut report);
            match measure_latency(&mut session, &report, args.runs) {
                Ok(mean_ms) => report.latency_ms_mean = Some(mean_ms),
                Err(e) => report.commit_error = Some(format!("latency run failed: {e}")),
            }
        }
        Err(e) => {
            report.commit = "error".to_string();
            report.commit_error = Some(e.to_string());
        }
    }

    // Best-effort partition/node-placement from an out-of-band verbose log.
    if let Some(ref log_path) = args.capability_log {
        let log = std::fs::read_to_string(log_path)
            .map_err(|e| format!("probe-ep: cannot read capability log {}: {e}", log_path.display()))?;
        report.partitions = parse_partition_count(&log);
        report.node_placement = parse_node_placement(&log);
    }

    if args.json {
        println!("{}", probe_report_json(&report));
    } else {
        print_human(&report);
    }
    Ok(())
}

/// Registration outcome, distinct from a probe error.
enum RegisterOutcome {
    /// CPU EP: nothing to register.
    Cpu,
    /// The GPU EP attached to the session options.
    Registered,
    /// The GPU EP failed to register (e.g. no DX12 device); its error string.
    Failed(String),
}

/// Build a `SessionBuilder` with the requested graph-opt level, the two-thread
/// spin-free CPU pool wc-core uses, DirectML's required session flags, and any
/// rung-3 disabled optimizers.
fn build_builder(args: &Args) -> Result<SessionBuilder, String> {
    let level = match args.graph_opt {
        GraphOpt::Disable => GraphOptimizationLevel::Disable,
        GraphOpt::Level1 => GraphOptimizationLevel::Level1,
        GraphOpt::Level3 => GraphOptimizationLevel::Level3,
    };
    let mut builder = Session::builder()
        .map_err(|e| e.to_string())?
        .with_optimization_level(level)
        .map_err(|e| e.to_string())?
        .with_intra_threads(2)
        .map_err(|e| e.to_string())?
        .with_intra_op_spinning(false)
        .map_err(|e| e.to_string())?;

    // DirectML rejects memory-pattern optimization and parallel graph execution
    // (mirrors inference_ort::configure_accelerator_session). Apply only for the
    // DirectML EP so CoreML/CPU probes stay on their defaults.
    if args.ep == Ep::Directml {
        builder = builder
            .with_parallel_execution(false)
            .map_err(|e| e.to_string())?
            .with_memory_pattern(false)
            .map_err(|e| e.to_string())?;
    }

    if let Some(ref list) = args.disable_optimizers {
        builder = builder
            .with_disabled_optimizers(list)
            .map_err(|e| e.to_string())?;
    }
    Ok(builder)
}

/// Register the requested EP on `builder`.
///
/// Returns `Err` only when the EP is not compiled for this platform — an
/// operator mistake (e.g. `--ep directml` on macOS) worth a non-zero exit.
fn register_ep(ep: Ep, builder: &mut SessionBuilder) -> Result<RegisterOutcome, String> {
    match ep {
        Ep::Cpu => Ok(RegisterOutcome::Cpu),
        Ep::Directml => register_directml(builder),
        Ep::Coreml => register_coreml(builder),
    }
}

#[cfg(target_os = "windows")]
fn register_directml(builder: &mut SessionBuilder) -> Result<RegisterOutcome, String> {
    use ort::ep::ExecutionProvider as _; // `.register` is a trait method
    match ort::ep::DirectML::default().register(builder) {
        Ok(()) => Ok(RegisterOutcome::Registered),
        Err(e) => Ok(RegisterOutcome::Failed(e.to_string())),
    }
}

#[cfg(not(target_os = "windows"))]
fn register_directml(_builder: &mut SessionBuilder) -> Result<RegisterOutcome, String> {
    Err("probe-ep: DirectML EP is compiled only on Windows; run --ep directml on the Windows box".to_string())
}

#[cfg(target_os = "macos")]
fn register_coreml(builder: &mut SessionBuilder) -> Result<RegisterOutcome, String> {
    use ort::ep::coreml::ComputeUnits;
    use ort::ep::ExecutionProvider as _;
    match ort::ep::CoreML::default()
        .with_compute_units(ComputeUnits::All)
        .register(builder)
    {
        Ok(()) => Ok(RegisterOutcome::Registered),
        Err(e) => Ok(RegisterOutcome::Failed(e.to_string())),
    }
}

#[cfg(not(target_os = "macos"))]
fn register_coreml(_builder: &mut SessionBuilder) -> Result<RegisterOutcome, String> {
    Err("probe-ep: CoreML EP is compiled only on macOS; run --ep coreml on the macOS box".to_string())
}

/// Fill declared I/O metadata from a committed session.
fn fill_metadata(session: &Session, report: &mut ProbeReport) {
    if let Some(input) = session.inputs().first() {
        report.input_name = Some(input.name().to_string());
        if let Some(shape) = input.dtype().tensor_shape() {
            report.input_shape = Some(shape.to_vec());
        }
    }
    report.output_names = Some(
        session
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect(),
    );
}

/// Time `session.run` over `runs` iterations (after 5 warmup runs) against a
/// zeroed input of the model's declared shape; return the mean in milliseconds.
fn measure_latency(session: &mut Session, report: &ProbeReport, runs: u32) -> Result<f64, String> {
    let (Some(name), Some(shape)) = (report.input_name.as_ref(), report.input_shape.as_ref())
    else {
        return Err("model has no declared input".to_string());
    };
    let dims = concrete_dims(shape);
    let n = element_count(&dims)?;
    let data = vec![0.0_f32; n];

    let mut run_once = |session: &mut Session| -> Result<(), String> {
        let tensor = TensorRef::from_array_view((dims.as_slice(), data.as_slice()))
            .map_err(|e| e.to_string())?;
        let _outputs = session
            .run(ort::inputs![name.as_str() => tensor])
            .map_err(|e| e.to_string())?;
        Ok(())
    };

    for _ in 0..5 {
        run_once(session)?;
    }
    let mut total_ms = 0.0_f64;
    for _ in 0..runs {
        let t0 = Instant::now();
        run_once(session)?;
        total_ms += t0.elapsed().as_secs_f64() * 1000.0;
    }
    let divisor = f64::from(runs.max(1));
    Ok(total_ms / divisor)
}

/// Human-readable one-block summary (default when `--json` is absent).
fn print_human(r: &ProbeReport) {
    println!("model      {}", r.model);
    println!("platform   {}", r.platform);
    println!("ep         {}  graph-opt {}", r.ep, r.graph_opt);
    println!("register   {}", r.register);
    if let Some(ref e) = r.register_error {
        println!("  register error: {e}");
    }
    println!("commit     {}", r.commit);
    if let Some(ref e) = r.commit_error {
        println!("  commit error: {e}");
    }
    if let Some(ms) = r.latency_ms_mean {
        println!("latency    {ms:.3} ms mean over {} runs", r.latency_runs);
    }
    if let Some(p) = r.partitions {
        println!("partitions {p}");
    }
    for (ep, n) in &r.node_placement {
        println!("  {ep}: {n} nodes");
    }
}
```

- [ ] **Step 2: Add the capability-log parsers (still pure — TDD)**

These parse ONNX Runtime's verbose `GetCapability` output. They are pure over a `&str`, so they unit-test with a captured log snippet. Add to the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn parse_partition_count_reads_the_getcapability_line() {
        let log = "\
[V:onnxruntime:, graph_partitioner.cc] GetCapability: number of partitions supported by DmlExecutionProvider: 1
other noise
";
        assert_eq!(parse_partition_count(log), Some(1));
    }

    #[test]
    fn parse_partition_count_is_none_when_absent() {
        assert_eq!(parse_partition_count("no capability line here"), None);
    }

    #[test]
    fn parse_node_placement_counts_nodes_placed_on_each_ep() {
        let log = "\
[V] Node placements
[V]  Node(s) placed on [DmlExecutionProvider]. Number of nodes: 117
[V]  Node(s) placed on [CPUExecutionProvider]. Number of nodes: 7
";
        let placement = parse_node_placement(log);
        assert_eq!(
            placement,
            vec![
                ("DmlExecutionProvider".to_string(), 117),
                ("CPUExecutionProvider".to_string(), 7),
            ]
        );
    }
```

Then add the implementations above the test module (below `print_human`):

```rust
/// Parse the partition count from an ONNX Runtime verbose capability log.
///
/// Matches lines like `GetCapability: number of partitions supported by
/// DmlExecutionProvider: N`. Best-effort: the log format is an ONNX Runtime
/// internal, not a stable API, so `None` (log absent, or format changed) is a
/// normal result and never fails the probe.
fn parse_partition_count(log: &str) -> Option<u32> {
    let re = regex::Regex::new(r"number of partitions supported by \w+:\s*(\d+)").ok()?;
    re.captures_iter(log)
        .filter_map(|c| c.get(1))
        .filter_map(|m| m.as_str().parse::<u32>().ok())
        .last()
}

/// Parse per-EP node-placement counts from an ONNX Runtime verbose log.
///
/// Matches lines like `Node(s) placed on [DmlExecutionProvider]. Number of
/// nodes: N`, in log order. Best-effort, same caveat as
/// [`parse_partition_count`].
fn parse_node_placement(log: &str) -> Vec<(String, u32)> {
    let Ok(re) = regex::Regex::new(r"placed on \[(\w+)\]\. Number of nodes:\s*(\d+)") else {
        return Vec::new();
    };
    re.captures_iter(log)
        .filter_map(|c| {
            let ep = c.get(1)?.as_str().to_string();
            let n = c.get(2)?.as_str().parse::<u32>().ok()?;
            Some((ep, n))
        })
        .collect()
}
```

- [ ] **Step 3: Run the unit tests**

```bash
cargo test -p xtask --lib probe_ep
```

Expected: PASS, 10 tests (7 from Task 3 + 3 parser tests). The `run`/session helpers are not unit-tested (they need the native ONNX Runtime binary and a live EP); they are covered by the human smoke below.

- [ ] **Step 4: Build and confirm the CLI wiring**

```bash
cargo build -p xtask
cargo run -p xtask -- probe-ep --ep directml --model assets/models/hand/palm_detection.onnx --json
```

On **macOS**, expect a clean non-zero exit with the message `DirectML EP is compiled only on Windows…` (the platform guard). This proves `--ep directml` fails loudly on the wrong platform rather than silently mis-registering.

- [ ] **Step 5: Scoped gate and commit**

```bash
cargo fmt --all
cargo clippy -p xtask --all-targets --all-features -- -D warnings
cargo test -p xtask --lib probe_ep
git add xtask/src/probe_ep.rs
git commit -F - <<'EOF'
feat(xtask/probe-ep): build session, register EP, commit, measure latency

run() builds a session at the requested graph-opt level (DirectML's
memory-pattern/parallel-exec flags applied only for --ep directml, mirroring
inference_ort), registers the requested EP, and captures commit success or
the exact error as data (exit 0 — a commit failure is the result, not a
probe error). Latency is the mean of N session.run calls over a zeroed input
of the declared shape. --ep directml on non-Windows exits non-zero loudly.

Partition and node-placement counts are parsed best-effort from an
out-of-band ONNX Runtime verbose log (--capability-log); the parsers are
pure and unit-tested.
EOF
git show --stat HEAD
```

---

## Task 5: Exercise the probe known-good on macOS (CoreML + CPU) — human-run

**Files:** none (validation only; may append a short note to the branch's scratch, not committed).

**Interfaces:**
- Consumes: the built `probe-ep`.
- Produces: a recorded known-good baseline JSON for CoreML and CPU, proving the tool works before it sees Windows.

**Why a human runs this.** Building a real ONNX Runtime session needs the downloaded native binary and, for CoreML, a live Apple GPU/ANE. There are no GPU/inference tests in CI. An agent cannot assert on this; Madison (or the operator) runs it and reads the JSON.

- [ ] **Step 1: CoreML — surgered palm (the shipped model)**

```bash
cargo run -p xtask -- probe-ep --ep coreml --model assets/models/hand/palm_detection.onnx --json
```

Expected: `"commit":"ok"`, `"register":"ok"`, `"input_shape":[1,192,192,3]`, two output names, a finite `latency_ms_mean`. This is the model CoreML was surgered *for*, so it must commit cleanly. If it does not, the probe (not the model) is wrong — stop and fix Task 4 before trusting any Windows result.

- [ ] **Step 2: CoreML — original palm (rank-4)**

```bash
cargo run -p xtask -- probe-ep --ep coreml --model assets/models/hand/palm_detection_original.onnx --json
```

Expected: this **also** commits under ONNX Runtime's CoreML EP (the EP places unsupported ops on CPU; only *partition count* differs — the rank-4 slopes fragment more). The point of running it is to confirm the probe treats both models and to capture a CoreML latency baseline for the latency reckoning later. Record both latencies.

- [ ] **Step 3: CPU — all three models**

```bash
for m in palm_detection palm_detection_original hand_landmark; do
  cargo run -p xtask -- probe-ep --ep cpu --model assets/models/hand/$m.onnx --json
done
```

Expected: all three `"commit":"ok"` on the CPU EP, with latency means. **This is the floor.** The CPU latencies here are the number DirectML must beat on the Vega 10 to be worth shipping (see the latency reckoning after the gate).

- [ ] **Step 4: Optional — capture a CoreML verbose capability log and confirm parsing**

```bash
ORT_LOG=verbose RUST_LOG=ort=trace \
  cargo run -p xtask -- probe-ep --ep coreml --model assets/models/hand/palm_detection.onnx 2> coreml_cap.log
cargo run -p xtask -- probe-ep --ep coreml --model assets/models/hand/palm_detection.onnx \
  --capability-log coreml_cap.log --json
```

Expected: the second run's JSON now carries a non-null `partitions` and a populated `node_placement`, proving the log-parsing path end-to-end on real ONNX Runtime output. If the regex misses (ONNX Runtime's CoreML log phrasing differs from the DirectML phrasing the parser was written against), adjust `parse_partition_count` / `parse_node_placement` and re-run their unit tests. Delete `coreml_cap.log` afterward — it is a scratch artifact, never committed. **Node/partition counts remain diagnostic colour; the gate below does not depend on them.**

- [ ] **Step 5: No commit** (nothing changed unless Step 4 adjusted a regex, in which case commit that one file with the same scoped gate as Task 4).

---

## ▶ DECISION GATE — Rung 0: run the probe on a real DirectML device

**This is the whole reason the plan exists.** Everything above is macOS work; this step needs a Windows machine. Because the hypothesised trigger (a `PRelu` slope-rank mismatch) is device-independent, **Madison's RX 6900 XT (discrete RDNA2) is a valid test bed** even though it is not the field tester's Vega 10 (GCN5) iGPU. Only if rung 0 excludes the cheap causes does the field tester's box get involved.

- [ ] **Gate Step 1: On the Windows box — clone the branch and build `xtask` only**

```bash
git fetch --all
git checkout windows-directml-prelu-rank
cargo build -p xtask        # minutes: no Bevy, no wc-core
```

- [ ] **Gate Step 2: Probe all three models on DirectML**

```powershell
foreach ($m in "palm_detection","palm_detection_original","hand_landmark") {
  cargo run -p xtask -- probe-ep --ep directml --model assets/models/hand/$m.onnx --json
}
```

Read the `commit` field of each. `hand_landmark.onnx` (no `PRelu`) is the control: it should commit on DirectML regardless, confirming the EP itself works on this box.

- [ ] **Gate Step 3: Read the two palm results and branch**

| `palm_detection.onnx` (surgered, rank-3) | `palm_detection_original.onnx` (rank-4) | Conclusion | Go to |
| --- | --- | --- | --- |
| `commit:error` | `commit:ok` | **PReLU rank confirmed.** The rank-3 slope is what DirectML's `DmlGraphFusionHelper` rejects. Fix is free — the rank-4 original is already committed in-tree. | **Task 6A** |
| `commit:error` | `commit:error` | It is **not** the slope rank; the shared suspects are the channel-dim `Pad` / `half_pixel` `Resize` / 3-D `Concat`. Model-level, reproducible on this box, no field tester needed. | **Task 6B** |
| `commit:ok` | `commit:ok` | The cheap causes are **excluded**. The failure is GCN5- or driver-specific to the Vega 10. | **Task 6C** |

Record the exact `commit_error` strings and the DirectML latency of whichever palm variant commits (compare against the Task 5 CPU/CoreML baselines — feeds the latency reckoning). **Do not proceed past this gate on a guess; the branch stays quarantined until this table resolves to exactly one row.**

---

## Task 6A: PReLU rank confirmed — `cfg`-select the palm model per platform

*Only if the gate resolved to row 1.*

**Files:**
- Modify: `crates/wc-core/src/input/providers/mediapipe/mod.rs` (the `load_model(dir, "palm_detection.onnx")` call at `:276`)

**Interfaces:**
- Consumes: nothing.
- Produces: a `const PALM_MODEL` selected by target OS.

**Why this is safe.** `palm_detection_original.onnx` is already vendored and committed. Windows loading the rank-4 original keeps CoreML on macOS untouched (macOS keeps the rank-3 surgered model, which its NeuralNetwork EP requires). Both platforms therefore keep full GPU acceleration. This is a `wc-core` change, so it triggers the slow build — budget for it.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/wc-core/src/input/providers/mediapipe/mod.rs`:

```rust
    #[test]
    fn palm_model_is_rank4_original_on_windows_rank3_surgered_elsewhere() {
        // Rung 0 (Plan 08) established on real DirectML hardware that the CoreML
        // PReLU slope reshape [1,C,1,1] -> [C,1,1] is what DmlGraphFusionHelper
        // rejects at commit. Windows loads the unmodified rank-4 upstream; macOS
        // keeps the rank-3 model its CoreML NeuralNetwork EP requires. Both are
        // vendored in assets/models/hand/.
        #[cfg(target_os = "windows")]
        assert_eq!(PALM_MODEL, "palm_detection_original.onnx");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(PALM_MODEL, "palm_detection.onnx");
    }
```

- [ ] **Step 2: Run it to see it fail**

```bash
cargo test -p wc-core --features hand-tracking-mediapipe --lib input::providers::mediapipe 2>&1 | head -20
```

Expected: FAIL to compile — `cannot find value PALM_MODEL in this scope`.

- [ ] **Step 3: Implement the `cfg`-select**

Immediately above `build_pipeline` (near `mod.rs:274`), add:

```rust
/// Vendored palm-detection model filename, selected per platform.
///
/// The shipped `palm_detection.onnx` carries rank-3 `PRelu` slopes `[C,1,1]`,
/// reshaped from the upstream rank-4 `[1,C,1,1]` in commit `d2369f4f` so the
/// macOS CoreML NeuralNetwork EP accepts them (it rejects rank 4). Plan 08's
/// rung-0 probe established on real DirectML hardware that DirectML's
/// `DmlGraphFusionHelper` rejects the rank-3 slope at commit — the mirror-image
/// constraint — so Windows loads the unmodified rank-4 original. Both files are
/// vendored under `assets/models/hand/`; each platform keeps full GPU
/// acceleration. See `docs/runbooks/onnx-coreml-model-surgery.md` and
/// `docs/superpowers/plans/2026-07-09-alpha5-08-directml-remediation.md`.
const PALM_MODEL: &str = if cfg!(target_os = "windows") {
    "palm_detection_original.onnx"
} else {
    "palm_detection.onnx"
};
```

Then change `mod.rs:276` from `load_model(dir, "palm_detection.onnx")?` to `load_model(dir, PALM_MODEL)?`. Leave the landmark load (`:277`) unchanged (it has no `PRelu`).

- [ ] **Step 4: Run the test to see it pass, then verify on Windows**

```bash
cargo test -p wc-core --features hand-tracking-mediapipe --lib input::providers::mediapipe
```

Expected: PASS. Then, on the Windows box, confirm the app itself now loads palm on DirectML: build and run, and read the startup backend log line (Plan 06 logs the effective per-model backend). It must report DirectML for both palm and landmark, not a CPU fallback.

- [ ] **Step 5: Full gate and commit**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
git add crates/wc-core/src/input/providers/mediapipe/mod.rs
git commit -F - <<'EOF'
fix(mediapipe): load the rank-4 palm model on Windows for DirectML

Plan 08's rung-0 probe established on real DirectML hardware that the
CoreML-motivated PReLU slope reshape ([1,C,1,1] -> [C,1,1], commit d2369f4f)
is exactly what DirectML's DmlGraphFusionHelper rejects at commit — the
mirror image of the CoreML NeuralNetwork constraint. Windows now loads the
unmodified rank-4 palm_detection_original.onnx (already vendored); macOS
keeps the rank-3 palm_detection.onnx its CoreML EP requires. Both platforms
keep full GPU acceleration. No model surgery, no new asset.
EOF
git show --stat HEAD
```

- [ ] **Step 6: Merge decision** — this is the outcome that lets the branch graduate. Proceed to the **latency reckoning** below before merging; then finish the branch per `superpowers:finishing-a-development-branch` (a PR from `windows-directml-prelu-rank` into the `windows-remediation` line). The probe tool (Tasks 2–4) merges with it: it is the reproduction harness for any future model swap.

---

## Task 6B: Both palm variants fail — Pad / Resize / Concat, DirectML edition

*Only if the gate resolved to row 2.*

The slope rank is exonerated. The remaining shared suspects are exactly the ops the CoreML runbook already floors on (`docs/runbooks/onnx-coreml-model-surgery.md:196-213`): 3 × channel-dim `Pad`, 2 × `half_pixel` `Resize`, 2 × 3-D `Concat`. This is model-level and reproduces on Madison's box, so the field tester is not needed. Walk the remaining rungs on the RX 6900 XT, cheapest first — **each is one already-built flag, so no rebuild between them.**

- [ ] **Step 1: Rung 2 — lower the graph-optimization level.** ONNX Runtime issue #12538 suggests this may not rescue DML, but it is instantly falsifiable:

```powershell
cargo run -p xtask -- probe-ep --ep directml --model assets/models/hand/palm_detection.onnx --graph-opt level1 --json
cargo run -p xtask -- probe-ep --ep directml --model assets/models/hand/palm_detection.onnx --graph-opt disable --json
```

If either flips `commit` to `ok`, the fix is a session-option change in `inference_ort::configure_accelerator_session` (set the level explicitly for DirectML). Write it as a small TDD change there, mirroring Task 6A's shape, and measure latency (a lower opt level may cost throughput — feeds the reckoning).

- [ ] **Step 2: Rung 3 — disable named optimizers.** Capture the DirectML verbose capability log to learn which transform precedes the fusion crash:

```powershell
$env:ORT_LOG="verbose"; $env:RUST_LOG="ort=trace"
cargo run -p xtask -- probe-ep --ep directml --model assets/models/hand/palm_detection.onnx 2> dml_cap.log
cargo run -p xtask -- probe-ep --ep directml --model assets/models/hand/palm_detection.onnx --capability-log dml_cap.log --json
```

Then try disabling the suspect transformer(s):

```powershell
cargo run -p xtask -- probe-ep --ep directml --model assets/models/hand/palm_detection.onnx --disable-optimizers "ConstantFolding" --json
```

(Substitute the transformer name the log implicates.) If a named optimizer disables the crash, the fix is `builder.with_disabled_optimizers("…")` in the DirectML branch of `configure_accelerator_session`.

- [ ] **Step 3: Rung 4 — a different ONNX Runtime build.** `ort` statically links pyke's build; the documented 16.3 → 17.0 DML initialization regression (issue #21205) makes "is this build-specific?" worth one test. This one **does** need a rebuild, because `load-dynamic` and `download-binaries` are mutually exclusive linking strategies. On this quarantine branch only, make a throwaway edit to `xtask/Cargo.toml` — override the base `ort` dep to `ort = { version = "=2.0.0-rc.12", default-features = false, features = ["load-dynamic"] }` (same version pin) — then:

```powershell
$env:ORT_DYLIB_PATH="C:\path\to\microsoft\onnxruntime.dll"
cargo run -p xtask -- probe-ep --ep directml --model assets/models/hand/palm_detection.onnx --json
```

If Microsoft's official `onnxruntime.dll` commits where pyke's build crashes, the failure is build-specific and the fix is a runtime-linking or ORT-version change (escalate to a separate design — do **not** flip the whole app to `load-dynamic` here). **Revert the `Cargo.toml` edit** whatever the result; it is a probe experiment, not a shipped change.

- [ ] **Step 4: Rung 5 — model surgery, DirectML edition.** Only if rungs 2–4 all fail. Re-derive a DirectML-friendly palm graph with `tools/handtrack-oracle/graph_surgery.py`, targeting the channel-`Pad` / `half_pixel`-`Resize` / 3-D-`Concat` floor the CoreML runbook documents, and **verify every edit is bit-exact** with the runbook's CPU-EP numerical-diff recipe (`onnx-coreml-model-surgery.md:252-278`) before committing an asset. Probe the surgered model on DirectML to confirm `commit:ok` and record it + its SHA in `assets/models/hand/ATTRIBUTION.md`. This is a new surgery lineage — write it up in the runbook as the DirectML counterpart to Surgery 2.

Whichever rung succeeds, land it TDD-first with the full CI gate, then go to the latency reckoning.

---

## Task 6C: Both palm variants pass — GCN5 / driver-specific

*Only if the gate resolved to row 3.*

The cheap, device-independent causes are excluded: on RDNA2 both palm models commit on DirectML. The failure is specific to the Vega 10 (GCN5) or its driver `23.20.815.6656`. Now — and only now — the field tester's box is on the critical path.

- [ ] **Step 1: Field tester runs the probe on the Vega 10.** Same three-model DirectML sweep as the gate. Confirm the surgered palm reproduces `commit:error` there while it committed on the 6900 XT — that pins the failure to his hardware/driver class.

- [ ] **Step 2: Rung 1 — ship a newer `DirectML.dll`.** The DLL pyke's `ort` rc.12 build staged (see the anchor correction: `xtask/src/bundle/windows.rs:136-164`, the `is_directml` copy at `:157`) is whatever that build linked against. DirectML is independently redistributable and fusion fixes for older GCN hardware land in it. Have the tester drop a newer `DirectML.dll` (from the `Microsoft.AI.DirectML` NuGet redist) next to the `xtask` binary (adjacent-DLL resolution) and re-run the DirectML probe. If a newer DLL commits, the app-side fix is to stage that newer redist in `bundle-windows` instead of the one `ort` ships — a change to the staging loop at `windows.rs:136-164`.

- [ ] **Step 3: Rungs 2–4 on his box, as flags.** The single `xtask` build already carries `--graph-opt` and `--disable-optimizers`; the tester runs them exactly as Task 6B Steps 1–2. He validates outcomes; he does not iterate on code.

- [ ] **Step 4: If nothing rescues the Vega 10**, the honest outcome is that this hardware class runs hand tracking on CPU. That is not a failure of this plan — Plan 06's commit-level fallback already guarantees it, and the latency reckoning below may show CPU is faster on a shared-die APU anyway. Record the finding (which rungs were tried, the exact errors) in the branch and the runbook, and default the tester's box to CPU via Plan 06's `hand_tracking_backend` setting.

---

## Latency reckoning — DirectML can win the ladder and still lose

**Run this before merging any 6A/6B/6C fix.** On a shared-die APU (Vega 10, Radeon 780M), DirectML inference contends with the renderer for the same shader cores and the same memory pool, and the renderer is already the bottleneck. CPU inference on two ~4 MB models may be faster end-to-end than DirectML. Winning the commit ladder and then measuring a latency regression is a real, expected outcome — and the probe measures exactly this.

- [ ] **Step 1: Compare the numbers you already have.** For whichever palm model now commits on DirectML, put its `latency_ms_mean` next to the same model's CPU `latency_ms_mean` from Task 5 Step 3, measured **on the deployment-class hardware** (the field tester's box for a final call; the RX 6900 XT is not representative of APU contention).

- [ ] **Step 2: Decide the default, not the capability.**
  - If DirectML is **faster or comparable** end-to-end: ship the fix with DirectML as the default. Done.
  - If **CPU is faster** on the APU: still land the fix (it restores the *option* of GPU acceleration and is correct), but set the **default** `hand_tracking_backend` (Plan 06's `Auto | ForceGpu | ForceCpu` setting) to prefer CPU for this hardware class. This branch does not own that setting — Plan 06 does, on the `windows-remediation` line — so the deliverable here is a written recommendation and the measured latencies, coordinated with Plan 06, not an edit to `settings/hand_tracking.rs`.
  - The full DirectML-vs-CPU-default question on a shared-die APU is a **non-goal for a release gate** (spec §3, §8): the probe supplies the number; the default-selection policy is a follow-up. Record the measurement; do not block the release on tuning it.

---

## Self-Review

**Branch quarantine.** Task 1 creates `windows-directml-prelu-rank` before anything else. Tasks 2–5 (probe tool) touch only `xtask/` and its tests — nothing app-facing. The one speculative *product* change (Task 6A's `cfg`-select, or 6B/6C's fix) lands only after the gate resolves on real DirectML data. "Nothing speculative merges until the probe returns data" is enforced structurally: the gate sits between the tool and every candidate fix.

**Probe fully specified and exercised on macOS first.** `probe-ep` has `--model`, `--ep {directml,coreml,cpu}`, `--graph-opt {disable,level1,level3}`, `--disable-optimizers`, `--runs`, `--capability-log`, and `--json`; it appears in `SUBCOMMANDS` + `EXPECTED_SUBCOMMANDS` (Task 2 Step 4) and supports `--help`. Task 5 runs it against CoreML and CPU on macOS and captures a known-good baseline before any Windows step.

**Three-way gate is explicit.** The DECISION GATE section is a marked block with a three-row table; each row routes to a distinct task (6A / 6B / 6C). It is not a linear list.

**DirectML claims are hypotheses.** The "DirectML claims are hypotheses" subsection states it up front, and every rung is written as "the probe will show whether…". The gate table is the only place a DirectML conclusion is drawn, and only from measured `commit` results.

**Latency loss is handled.** The latency reckoning section is a required pre-merge step that can conclude "ship the fix but default to CPU," with the default-selection policy explicitly deferred to Plan 06 (spec non-goal).

**Placeholder scan.** No "TBD"/"similar to Task N"/"…". Every code step shows complete code. The one intentional operator-fill is the Windows-machine paths (`C:\path\to\…\onnxruntime.dll`, the newer `DirectML.dll` source) and the transformer name in rung 3 — those are runtime inputs, not code placeholders.

**Type consistency.** `ProbeReport` fields are plain types (`String`, `Option<String>`, `Option<Vec<i64>>`, `Option<f64>`, `u32`, `Option<u32>`, `Vec<(String, u32)>`) — no `ort` types, so `probe_report_json` and the shape helpers unit-test without a session. `Ep`/`GraphOpt` are `clap::ValueEnum` + `Copy` with `as_str`. `run` consumes `Args` and returns `Result<(), Box<dyn std::error::Error>>`, matching `validate_shaders::run`. `build_builder`/`register_ep`/`measure_latency` thread a `SessionBuilder`/`Session` and stringify `ort::Error<R>` via `.to_string()` (the generic-recovery pattern from `inference_ort::load_err`).

**Clippy rules in example code.** Test modules carry `#[allow(clippy::expect_used, reason = …)]` (matching the existing xtask test modules), so `.expect()` in tests is allowed. No `assert_eq!(x.is_some(), true)`; no `0..(N+1)` (loops use `0..5`, `0..runs`, or plain counts); no `as` casts — `usize::try_from`, `f64::from(runs.max(1))`, and `Duration::as_secs_f64()` are used instead. `register_directml`/`register_coreml` are `cfg`-paired so the "unavailable" arm exists on every platform (no dead code, no missing symbol).

**Anchor corrections recorded.** The index's `xtask/src/bundle/windows.rs:244` is a test literal; the real DLL staging is `:136-164` (`is_directml` at `:157`). `mediapipe/mod.rs:276-277`, the `=2.0.0-rc.12` pin, and xtask's no-Bevy/no-wc-core dependency set are all confirmed. The PRelu table was independently reconfirmed with `onnx` (op spelled `PRelu`; shipped model rank 3, original rank 4, landmark zero).

**Open questions carried forward.**
1. Whether `ort` rc.12's CoreML verbose log uses the same `GetCapability … number of partitions` / `Node(s) placed on […]` phrasing the DirectML parser was written against — resolved empirically in Task 5 Step 4 (adjust the regex if it misses; the gate does not depend on it).
2. Whether `RegisterOutcome::Failed` should still attempt `commit` (currently it does — registration failure is recorded but commit proceeds, so a graceful CPU fallback is still measured). The gate reads `commit`, so this is correct, but confirm the DirectML `register` genuinely succeeds on the tester's box (the spec says it does; the probe's `register` field verifies it).
