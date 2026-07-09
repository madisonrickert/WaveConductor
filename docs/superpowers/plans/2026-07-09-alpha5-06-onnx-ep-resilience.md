# ONNX Execution-Provider Resilience Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A GPU execution provider that fails at commit time must degrade the affected model to the CPU execution provider instead of costing the app all hand tracking, per model, on both Windows (DirectML) and macOS (CoreML).

**Architecture:** `OrtInference::load` currently registers the platform GPU EP and then commits the graph in one shot; a DirectML fusion crash inside `commit_from_memory` propagates as a fatal `InferenceError::Load`. We split load into two pure decision points — `ep_plan` (backend preference → whether to attempt the accelerator and whether a commit failure may fall back to CPU) and `load_with_ep_fallback` (a generic "try accelerated, on error rebuild CPU-only" combinator) — both unit-testable with no GPU EP. `load` becomes: build a fresh accelerated `SessionBuilder` and commit it; on error, warn (naming the model and the failing node), rebuild a fresh CPU-only `SessionBuilder`, and commit that. Because `load_model` is called once per model and `combined_backend` already understands mixed states, a failure in `palm_detection.onnx` leaves `hand_landmark.onnx` on DirectML. A new `HandTrackingBackend { Auto | ForceGpu | ForceCpu }` setting lets the field tester A/B without recompiling.

**Tech Stack:** Rust, `ort` (pyke ONNX Runtime) `=2.0.0-rc.12`, ONNX Runtime C++ backend (CoreML EP on macOS, DirectML EP on Windows, CPU EP elsewhere), Bevy 0.19 settings/reflection, `tracing`.

## Global Constraints

Copied from `AGENTS.md` and the program index's Part 1. Every task's requirements implicitly include this section.

- **CI gates**, all of which must pass before a task is complete:
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features --workspace -- -D warnings`
  - `cargo nextest run --workspace --all-features` (nextest skips doctests; also run `cargo test --doc --workspace`)
  - `cargo test --doc --workspace`
  - `cargo doc --no-deps --workspace --document-private-items` (CI runs it with `RUSTDOCFLAGS="-D warnings"`)
  - `cargo deny check`
  - `cargo xtask check-secrets`
- **The per-task clippy gate MUST use `--all-targets`**, not `--lib`. `--lib` skips the test target; CI runs `--all-targets`. Use `cargo clippy -p wc-core --all-targets --all-features -- -D warnings`.
- **Clippy is `-D warnings` over `pedantic`, including inside `#[cfg(test)]`.** `unwrap_used`, `expect_used`, `panic`, and `as_conversions` are `warn` and escalate to errors. In test code, either put `#[allow(clippy::expect_used, reason = "…")]` on the `mod tests` block (the existing `inference_ort.rs` / `mod.rs` test blocks already carry it) or use `let … else` / `assert!(matches!(…))` / destructuring. Never `assert_eq!(x.is_some(), true)` (→ `bool_assert_comparison`; use `assert!(x.is_some())`). Never `0..(N + 1)` (→ `range_plus_one`; use `0..=N`). No `as` casts where `From`/`TryFrom` works.
- **No `unwrap()` / `expect()` / `panic!` in non-test code** unless a panic is a documented invariant violation.
- **`///` rustdoc on every public item** (struct, enum, trait, fn, module); `//!` on module roots. Never strip comments during refactors — update stale ones. A **public** item's rustdoc linking to a `pub(crate)`/private item trips `rustdoc::private_intra_doc_links` (denied); demote such references to a plain code span.
- **Never allocate in a hot path.** The **inference worker loop** (`wc-core/src/input/providers/mediapipe/worker.rs`) and `OrtInference::run` are hot paths. `OrtInference::load` and everything this plan adds to it run **once per model at startup** — allocation there is fine. Keep the retry cost on the error path only.
- **Platform code:** a handful of lines gets an inline `#[cfg(target_os = "…")]` (AGENTS.md names `inference_ort.rs` as a sanctioned inline-`cfg` site); do not force a `platform/` submodule split here.
- **No new dependencies.** Reuse what is already in the graph (`ort`, `tracing`, `bevy`, `serde`). Do not bump the `ort` pin.
- **Public API at the top, private helpers at the bottom, tests in a `#[cfg(test)] mod tests` block at the file footer.** Function bodies fit on one screen; extract if not.
- **Commits:** stage **named paths only** (never `git add -A`), and use `git commit -F <file>` (never `-m` — backticks in a message are shell-substituted). After committing, `git show --stat HEAD` to confirm only the intended paths landed.
- **Do not** put `bevy/dynamic_linking` in any manifest. Manual smoke tests use `cargo rund`.

## What this plan does *not* do, and cannot verify here

- **It does not chase the DirectML root cause.** Keeping the iGPU accelerated (the PRelu-rank experiment, DirectML.dll bump, graph-opt flags) is **Plan 08**, on branch `windows-directml-prelu-rank`. Plan 06 is the safety net that ships regardless of what Plan 08 finds; the two are independent.
- **The DirectML failure path is not reachable on the dev machine.** The implementer is on macOS; there is no DirectML and there are no GPU EP tests in CI (the program index, Part 1: "There are no GPU tests in CI"). Every test in this plan therefore exercises the retry **decision logic** through the pure, injectable `ep_plan` and `load_with_ep_fallback` functions, which take simulated `Ok`/`Err` closures and never touch a GPU. The macOS CoreML commit path is exercised only incidentally by the existing `inference_ort` tests, which continue to pass `HandTrackingBackend::Auto` and expect CoreML.

---

### Task 1: Add the `HandTrackingBackend` preference setting

**Files:**
- Modify: `crates/wc-core/src/settings/hand_tracking.rs` (add the enum, the field, and tests)
- Modify: `crates/wc-core/src/settings/mod.rs:44` (re-export the enum)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum HandTrackingBackend { Auto, ForceGpu, ForceCpu }` (derives mirror `HandProviderChoice`: `Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default`, default `Auto`).
  - `HandTrackingSettings::backend: HandTrackingBackend` (a `ty = Enum`, `category = User` setting).
  - Re-export `crate::settings::HandTrackingBackend`.

The `ty = Enum` `SettingKind` already handles a static unit-variant Rust enum (that is exactly what `HandProviderChoice` at `hand_tracking.rs:23` is). No runtime-enumerated widget (Plan 03a) is needed. `HandProviderChoice` is never explicitly `register_type`'d anywhere — the `SketchSettings` derive plus `register_sketch_settings::<HandTrackingSettings>()` (`settings/registry.rs:181`) is sufficient — so this enum needs no registration either.

- [ ] **Step 1: Write the failing tests**

Append these tests inside the existing `#[cfg(test)] mod tests` block at the footer of `crates/wc-core/src/settings/hand_tracking.rs` (it already carries `#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]`):

```rust
    /// The inference backend preference defaults to `Auto` when a settings file
    /// saved before the field existed is loaded — never erroring, never landing
    /// on a forced mode the operator did not choose.
    #[test]
    fn backend_defaults_to_auto_when_absent_from_saved_settings() {
        let parsed: HandTrackingSettings =
            toml::from_str("leap_background = true").expect("pre-backend settings file loads");
        assert_eq!(parsed.backend, HandTrackingBackend::Auto);
    }

    /// The persisted representation is the bare variant name, matching the
    /// dropdown's reflection write-back, so persistence and the panel can never
    /// disagree about an identifier.
    #[test]
    fn backend_persists_as_the_variant_name() {
        let settings = HandTrackingSettings {
            backend: HandTrackingBackend::ForceCpu,
            ..HandTrackingSettings::default()
        };
        let text = toml::to_string(&settings).expect("settings serialize");
        assert!(text.contains("backend = \"ForceCpu\""), "got: {text}");
    }

    /// Round-trip every variant through the persisted form.
    #[test]
    fn backend_choice_round_trips_through_toml() {
        for choice in [
            HandTrackingBackend::Auto,
            HandTrackingBackend::ForceGpu,
            HandTrackingBackend::ForceCpu,
        ] {
            let settings = HandTrackingSettings {
                backend: choice,
                ..HandTrackingSettings::default()
            };
            let text = toml::to_string(&settings).expect("serialize");
            let back: HandTrackingSettings = toml::from_str(&text).expect("parse back");
            assert_eq!(back.backend, choice);
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p wc-core --lib settings::hand_tracking 2>&1 | head -30`

Expected: FAIL to compile — `cannot find type HandTrackingBackend in this scope` and `no field backend on type HandTrackingSettings`.

- [ ] **Step 3: Add the enum**

In `crates/wc-core/src/settings/hand_tracking.rs`, immediately below the `HandProviderChoice` enum (after its closing brace at line 35), add:

```rust
/// How the `MediaPipe` inference sessions should choose an execution provider —
/// the operator's override for the GPU-vs-CPU decision, so a box whose GPU EP
/// crashes at graph fusion can be pinned to CPU (or forced back onto the GPU for
/// diagnosis) without a rebuild.
///
/// Variant identifiers double as the persisted strings *and* the dropdown labels
/// (see `wc_core_macros`: serde serializes unit variants as their name, and the
/// panel has no per-variant label mapping), so they are chosen to read in a
/// dropdown.
///
/// Applied at provider (re)start: `MediaPipeConfig::backend` is seeded from this
/// on registry build, and each ONNX model resolves its provider from it in
/// `OrtInference::load`. Changing it takes effect when the provider is next
/// rebuilt (relaunch, or a toggle of the "Tracking provider" dropdown) — there is
/// no live per-frame re-tune, because the choice is only read while a session is
/// being constructed.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HandTrackingBackend {
    /// Attempt the platform GPU EP (CoreML / DirectML); if it fails at commit,
    /// warn and rebuild that model on the CPU EP. The safety net — the default.
    #[default]
    Auto,
    /// Attempt the platform GPU EP and do **not** fall back: a commit failure is
    /// surfaced as a load error. Disables the safety net deliberately, so a
    /// broken EP is loud rather than silently degraded — a diagnosis lever, not a
    /// deployment default.
    ForceGpu,
    /// Never register a GPU EP; build a CPU-only session from the start. The
    /// fastest way to confirm CPU tracking works, and the operator's lever when a
    /// GPU EP is flaky.
    ForceCpu,
}
```

- [ ] **Step 4: Add the field**

In `crates/wc-core/src/settings/hand_tracking.rs`, add this field to `HandTrackingSettings` immediately after the `provider` field (after line 62), so the macro-generated `Default` and `settings_def()` include it:

```rust
    /// Which execution provider the `MediaPipe` ONNX sessions should use
    /// (`Auto` tries the platform GPU EP and falls back to CPU on a commit
    /// failure; `ForceGpu` disables that fallback; `ForceCpu` skips the GPU EP
    /// entirely). Applied when the provider is next (re)built — see
    /// [`HandTrackingBackend`]. Exposed as a `User` knob so a field tester can
    /// A/B GPU vs CPU inference without a new build.
    #[setting(
        default = HandTrackingBackend::Auto,
        ty = Enum,
        category = User,
        section = "Hand Tracking",
        label = "Inference backend"
    )]
    #[serde(default)]
    pub backend: HandTrackingBackend,
```

- [ ] **Step 5: Re-export the enum**

In `crates/wc-core/src/settings/mod.rs`, change line 44 from:

```rust
pub use hand_tracking::{HandProviderChoice, HandTrackingSettings};
```

to:

```rust
pub use hand_tracking::{HandProviderChoice, HandTrackingBackend, HandTrackingSettings};
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p wc-core --lib settings::hand_tracking`

Expected: PASS, including the three new `backend_*` tests alongside the existing `provider_*` ones.

- [ ] **Step 7: Run the scoped gate and commit**

The full workspace clippy gate is deliberately not run per-step (the controller runs it between tasks).

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --lib settings::hand_tracking
```

Write this message to a file (e.g. `"$TMPDIR/wc-msg"`) and commit with `-F`:

```
feat(settings): add HandTrackingBackend { Auto | ForceGpu | ForceCpu }

A User-category enum setting selecting how the MediaPipe ONNX sessions
pick an execution provider. Auto keeps the GPU EP with a CPU commit-time
fallback; ForceGpu disables that fallback for loud diagnosis; ForceCpu
skips the GPU EP entirely. Mirrors HandProviderChoice's derives and
persistence (variant name is the stored string and the dropdown label).
Consumed by OrtInference::load in the next task.
```

```bash
git add crates/wc-core/src/settings/hand_tracking.rs crates/wc-core/src/settings/mod.rs
git commit -F "$TMPDIR/wc-msg"
git show --stat HEAD
```

---

### Task 2: Commit-level EP fallback in `OrtInference::load`, wired end to end

**Files:**
- Modify: `crates/wc-core/src/input/providers/mediapipe/inference_ort.rs`
  - module doc `:6-9` and `load` doc `:62-63` (correct the false "load never fails closed" claim)
  - `load` (`:60-118`): new signature and retry body
  - add `base_builder`, `commit_accelerated`, `commit_cpu`, `EpPlan`, `ep_plan`, `load_with_ep_fallback`
  - the five `OrtInference::load(...)` test call sites (`:386, :414, :437, :469, :495`)
  - new unit tests for `ep_plan` and `load_with_ep_fallback`
- Modify: `crates/wc-core/src/input/providers/mediapipe/mod.rs`
  - `MediaPipeConfig` (`:74-113`) + its `Default` (`:115-129`): add `backend`
  - `build_pipeline` (`:274-289`) passes `self.config.backend`
  - `load_model` (`:491-506`): new signature + per-model startup log
- Modify: `crates/wc-core/src/input/providers/mediapipe/pipeline.rs:1428` (the `model(name)` test helper's `OrtInference::load` call)

**Interfaces:**
- Consumes: `crate::settings::HandTrackingBackend` (Task 1).
- Produces:
  - `pub fn OrtInference::load(model_bytes: &[u8], backend: HandTrackingBackend, model_name: &str) -> Result<Self, InferenceError>`
  - `fn base_builder() -> Result<SessionBuilder, InferenceError>`
  - `fn commit_accelerated(model_bytes: &[u8]) -> Result<(Session, &'static str), InferenceError>`
  - `fn commit_cpu(model_bytes: &[u8]) -> Result<(Session, &'static str), InferenceError>`
  - `struct EpPlan { try_accelerated: bool, allow_cpu_fallback: bool }` (`#[derive(Debug, Clone, Copy, PartialEq, Eq)]`)
  - `fn ep_plan(backend: HandTrackingBackend) -> EpPlan`
  - `fn load_with_ep_fallback<S, E: Display>(model_name: &str, allow_cpu_fallback: bool, try_accelerated: impl FnOnce() -> Result<(S, &'static str), E>, build_cpu: impl FnOnce() -> Result<(S, &'static str), E>) -> Result<(S, &'static str), E>`
  - `MediaPipeConfig::backend: HandTrackingBackend`
  - `fn load_model(dir: &Path, name: &str, backend: HandTrackingBackend) -> Result<(Box<dyn HandInference>, &'static str), HandTrackingError>`

**Why one task.** Changing `load`'s signature breaks `mod.rs::load_model` and `pipeline.rs`'s test helper, and `build_pipeline` can only pass `self.config.backend` once `MediaPipeConfig` has that field. These are one atomic compile unit; splitting them leaves the crate non-building at a commit boundary. `MediaPipeConfig::backend` defaults to `Auto`, so the binary's `register_mediapipe` still compiles via `..MediaPipeConfig::default()` (Task 3 seeds it from settings). With the default `Auto`, runtime behaviour is identical to today plus the new commit-time safety net.

- [ ] **Step 1: Write the failing tests for the pure decision logic**

Add these to the existing `#[cfg(test)] mod tests` block at the footer of `crates/wc-core/src/input/providers/mediapipe/inference_ort.rs` (it already carries `#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]`, so `.expect()` is permitted there; do not use bare `panic!`):

```rust
    #[test]
    fn ep_plan_maps_each_backend_preference() {
        // Auto: try the accelerator, allow a CPU rebuild if commit fails.
        assert_eq!(
            ep_plan(HandTrackingBackend::Auto),
            EpPlan {
                try_accelerated: true,
                allow_cpu_fallback: true
            }
        );
        // ForceGpu: try the accelerator, but never fall back (loud failure).
        assert_eq!(
            ep_plan(HandTrackingBackend::ForceGpu),
            EpPlan {
                try_accelerated: true,
                allow_cpu_fallback: false
            }
        );
        // ForceCpu: never register a GPU EP at all.
        assert_eq!(
            ep_plan(HandTrackingBackend::ForceCpu),
            EpPlan {
                try_accelerated: false,
                allow_cpu_fallback: false
            }
        );
    }

    #[test]
    fn ep_fallback_keeps_the_accelerated_result_on_success() {
        // On a successful accelerated commit the (session, label) pair is returned
        // unchanged and the CPU builder is never invoked. The GPU EP path is
        // unreachable in CI (no GPU tests), so the decision logic is exercised
        // with plain stand-in values instead of a real Session.
        let mut cpu_built = false;
        let (session, label) = load_with_ep_fallback::<u32, String>(
            "palm_detection.onnx",
            true,
            || Ok((42, BACKEND_DIRECTML)),
            || {
                cpu_built = true;
                Ok((0, BACKEND_CPU))
            },
        )
        .expect("accelerated commit succeeds");
        assert_eq!(session, 42);
        assert_eq!(label, BACKEND_DIRECTML);
        assert!(
            !cpu_built,
            "CPU builder must not run when the accelerated path commits"
        );
    }

    #[test]
    fn ep_fallback_rebuilds_on_cpu_when_the_accelerated_commit_fails() {
        // This is the regression for the shipped bug: a DirectML fusion crash at
        // commit must degrade to the CPU EP, not abort the whole load.
        let (session, label) = load_with_ep_fallback::<u32, String>(
            "palm_detection.onnx",
            true,
            || Err("80004005: DmlGraphFusionHelper".to_owned()),
            || Ok((7, BACKEND_CPU)),
        )
        .expect("cpu rebuild succeeds");
        assert_eq!(session, 7);
        assert_eq!(label, BACKEND_CPU);
    }

    #[test]
    fn ep_fallback_propagates_the_error_when_cpu_fallback_is_disallowed() {
        // ForceGpu semantics: a commit failure must surface as an error, and the
        // CPU builder must not run.
        let mut cpu_built = false;
        let result = load_with_ep_fallback::<u32, String>(
            "palm_detection.onnx",
            false,
            || Err("commit failed".to_owned()),
            || {
                cpu_built = true;
                Ok((0, BACKEND_CPU))
            },
        );
        assert!(result.is_err());
        assert!(
            !cpu_built,
            "no CPU rebuild is attempted when fallback is disallowed"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p wc-core --all-features --lib input::providers::mediapipe::inference_ort 2>&1 | head -30`

Expected: FAIL to compile — `cannot find function ep_plan`, `cannot find type EpPlan`, `cannot find function load_with_ep_fallback`, `cannot find type HandTrackingBackend` in this scope.

- [ ] **Step 3: Correct the false doc comments**

In `crates/wc-core/src/input/providers/mediapipe/inference_ort.rs`, the module doc currently ends (lines 8-9):

```rust
//! (Linux) run on ONNX Runtime's CPU EP. ONNX Runtime falls back to CPU for any op
//! the EP cannot place, so load never fails closed on an unsupported operator.
```

Replace those two lines with:

```rust
//! (Linux) run on ONNX Runtime's CPU EP. ONNX Runtime partitions the graph and
//! places any op the EP cannot support back on the CPU — but that is *per-op
//! placement* fallback, not a safety net against an EP that fails at *commit*: a
//! GPU EP can register cleanly and then throw while fusing the graph (observed as
//! DirectML `DmlGraphFusionHelper` `0x80004005` on some AMD drivers), aborting the
//! whole load. `load` therefore retries on a fresh CPU-only session when the
//! accelerated commit fails, so a broken GPU EP degrades one model to CPU instead
//! of losing hand tracking entirely (see [`load_with_ep_fallback`]).
```

The `load` doc currently reads (lines 61-63):

```rust
    /// Load an ONNX model from its bytes, registering the platform GPU execution
    /// provider (see [`register_accelerator`]). ONNX Runtime falls back to CPU for
    /// any op the EP cannot place, so load never fails closed.
```

Replace those three lines with:

```rust
    /// Load an ONNX model from its bytes, resolving its execution provider from
    /// `backend` (see [`ep_plan`]) and registering the platform GPU EP (see
    /// [`register_accelerator`]) when the plan calls for it.
    ///
    /// The EP is a *placement* preference, not a guarantee: ONNX Runtime moves
    /// individual unsupported ops to the CPU, but a GPU EP can still fail while
    /// fusing the graph at commit. On such a failure — unless the caller forced
    /// the GPU with [`HandTrackingBackend::ForceGpu`] — `load` rebuilds a fresh
    /// CPU-only session and returns [`BACKEND_CPU`], so one broken EP never costs
    /// all hand tracking. `model_name` names the failing model in the warning.
```

- [ ] **Step 4: Add the imports and the pure helpers**

In `crates/wc-core/src/input/providers/mediapipe/inference_ort.rs`, add to the imports (after the existing `use super::inference::{HandInference, InferenceError, Tensor};` at line 33):

```rust
use std::fmt::Display;

use crate::settings::HandTrackingBackend;
```

Then, immediately below the `impl OrtInference { … }` block (after its closing brace at line 132, before `fn load_err`), add the pure helpers:

```rust
/// How a [`HandTrackingBackend`] preference resolves into the two independent
/// load-time decisions: whether to attempt the platform GPU EP at all, and
/// whether a commit failure on that EP may rebuild on the CPU.
///
/// Split out as a plain data value so the mapping is unit-testable with no GPU EP
/// present (there are none in CI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EpPlan {
    /// Attempt the platform GPU EP (`false` only for
    /// [`HandTrackingBackend::ForceCpu`], which builds CPU-only from the start).
    try_accelerated: bool,
    /// On an accelerated commit failure, rebuild on the CPU EP (`false` for
    /// [`HandTrackingBackend::ForceGpu`], which surfaces the error instead).
    allow_cpu_fallback: bool,
}

/// Resolve a backend preference into an [`EpPlan`].
fn ep_plan(backend: HandTrackingBackend) -> EpPlan {
    match backend {
        HandTrackingBackend::Auto => EpPlan {
            try_accelerated: true,
            allow_cpu_fallback: true,
        },
        HandTrackingBackend::ForceGpu => EpPlan {
            try_accelerated: true,
            allow_cpu_fallback: false,
        },
        HandTrackingBackend::ForceCpu => EpPlan {
            try_accelerated: false,
            allow_cpu_fallback: false,
        },
    }
}

/// Try an accelerated session build+commit, and on error optionally rebuild on
/// the CPU EP, returning the committed session and the backend label that
/// actually took (the accelerated label on success, whatever `build_cpu` returns
/// on the fallback path).
///
/// Generic over the session type `S` and error `E: Display` so the retry decision
/// is unit-testable without any GPU EP: a test passes closures returning `Ok`/`Err`
/// to drive every branch. `E: Display` is used only to render the failing EP's
/// error — which carries the exact failing node — into the warning, so the field
/// tester's "upload the log" workflow captures the diagnostic.
fn load_with_ep_fallback<S, E: Display>(
    model_name: &str,
    allow_cpu_fallback: bool,
    try_accelerated: impl FnOnce() -> Result<(S, &'static str), E>,
    build_cpu: impl FnOnce() -> Result<(S, &'static str), E>,
) -> Result<(S, &'static str), E> {
    match try_accelerated() {
        Ok(loaded) => Ok(loaded),
        Err(err) if allow_cpu_fallback => {
            tracing::warn!(
                model = model_name,
                %err,
                "accelerated execution provider failed to commit the graph; \
                 rebuilding this model on the CPU execution provider"
            );
            build_cpu()
        }
        Err(err) => Err(err),
    }
}

/// Build a `SessionBuilder` with the CPU-thread-pool options shared by every
/// execution provider.
///
/// Two sessions (palm + landmark) each own a pool; capping intra-op threads and
/// disabling spin-waiting stops idle inference from burning whole cores between
/// frames at our `<= 30 Hz` cadence. This is independent of EP/model format, so
/// both the accelerated and CPU-only builders start from it.
fn base_builder() -> Result<SessionBuilder, InferenceError> {
    Session::builder()
        .map_err(load_err)?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(load_err)?
        .with_intra_threads(2)
        .map_err(load_err)?
        .with_intra_op_spinning(false)
        .map_err(load_err)
}

/// Build the platform-accelerated session and commit it, returning the committed
/// session and the registered backend label.
///
/// On Windows this also applies the DirectML session options
/// ([`configure_accelerator_session`]); on Linux [`register_accelerator`] is a
/// no-op and the label is [`BACKEND_CPU`]. `commit_from_memory` consumes the
/// builder, which is why the CPU fallback in [`OrtInference::load`] rebuilds a
/// fresh one rather than reusing this.
fn commit_accelerated(model_bytes: &[u8]) -> Result<(Session, &'static str), InferenceError> {
    let mut builder = base_builder()?;
    #[cfg(target_os = "windows")]
    {
        builder = configure_accelerator_session(builder)?;
    }
    // `Ok` registration means the EP attached to the session options, NOT that
    // every node runs on it — the graph is partitioned at commit and any
    // unsupported op still falls to the CPU. The label reflects registration, not
    // whole-graph placement (see [`OrtInference::backend`]).
    let label = register_accelerator(&mut builder, model_bytes);
    let session = builder.commit_from_memory(model_bytes).map_err(load_err)?;
    Ok((session, label))
}

/// Build a CPU-only session (no GPU EP registered) and commit it.
///
/// Used both for [`HandTrackingBackend::ForceCpu`] and as the fallback when an
/// accelerated commit fails. Starts from a fresh [`base_builder`] because the
/// accelerated builder was consumed by its failed commit.
fn commit_cpu(model_bytes: &[u8]) -> Result<(Session, &'static str), InferenceError> {
    let session = base_builder()?
        .commit_from_memory(model_bytes)
        .map_err(load_err)?;
    Ok((session, BACKEND_CPU))
}
```

- [ ] **Step 5: Rewrite `load`**

Replace the body of `OrtInference::load` (from the opening `{` at line 74 through its closing `}` at line 118) — i.e. everything between the doc comment you fixed in Step 3 and the end of the method — with the new signature and body. The full method (doc comment from Step 3 followed by this) becomes:

```rust
    /// The session's CPU thread pool is capped to two intra-op threads with
    /// spin-waiting disabled (see [`base_builder`]).
    ///
    /// # Errors
    /// Returns [`InferenceError::Load`] if the session cannot be built or
    /// committed (and, under [`HandTrackingBackend::ForceGpu`], if the GPU EP
    /// fails at commit), or if the model has no input.
    pub fn load(
        model_bytes: &[u8],
        backend: HandTrackingBackend,
        model_name: &str,
    ) -> Result<Self, InferenceError> {
        let plan = ep_plan(backend);
        let (session, backend_label) = if plan.try_accelerated {
            load_with_ep_fallback(
                model_name,
                plan.allow_cpu_fallback,
                || commit_accelerated(model_bytes),
                || commit_cpu(model_bytes),
            )?
        } else {
            commit_cpu(model_bytes)?
        };

        let input_name = session
            .inputs()
            .first()
            .ok_or_else(|| InferenceError::Load("model has no inputs".into()))?
            .name()
            .to_owned();
        let output_names = session
            .outputs()
            .iter()
            .map(|o| o.name().to_owned())
            .collect();
        Ok(Self {
            session,
            input_name,
            output_names,
            backend: backend_label,
            input_shape: Vec::new(),
        })
    }
```

> The old inline builder-and-commit (old lines 74-98) is now entirely inside `base_builder` / `commit_accelerated` / `commit_cpu`; do not leave a duplicate. Confirm `builder.commit_from_memory` no longer appears inside `load` itself.

- [ ] **Step 6: Update the five `OrtInference::load` call sites in this file's tests**

Each test that loads a model must pass `HandTrackingBackend::Auto` and the model's filename. Make these edits in the `#[cfg(test)] mod tests` block:

- `backend_label_is_one_of_the_known_values` (line 386):

```rust
        let model = OrtInference::load(
            &model_bytes("palm_detection.onnx"),
            HandTrackingBackend::Auto,
            "palm_detection.onnx",
        )
        .expect("load via ort");
```

- `ort_palm_model_runs_and_emits_raw_box_and_score_tensors` (line 414):

```rust
        let mut model = OrtInference::load(
            &model_bytes("palm_detection.onnx"),
            HandTrackingBackend::Auto,
            "palm_detection.onnx",
        )
        .expect("load via ort");
```

- `ort_landmark_model_runs_and_emits_expected_shapes` (line 437):

```rust
        let mut model = OrtInference::load(
            &model_bytes("hand_landmark.onnx"),
            HandTrackingBackend::Auto,
            "hand_landmark.onnx",
        )
        .expect("load via ort");
```

- `ort_landmark_presence_is_a_probability_from_the_graph` (line 469):

```rust
        let mut model = OrtInference::load(
            &model_bytes("hand_landmark.onnx"),
            HandTrackingBackend::Auto,
            "hand_landmark.onnx",
        )
        .expect("load via ort");
```

- `ort_run_rejects_wrong_input_shape` (line 495):

```rust
        let mut model = OrtInference::load(
            &model_bytes("hand_landmark.onnx"),
            HandTrackingBackend::Auto,
            "hand_landmark.onnx",
        )
        .expect("load via ort");
```

The existing macOS assertion in `backend_label_is_one_of_the_known_values` (`assert_eq!(backend, BACKEND_COREML, …)`) stays valid: with `Auto`, macOS registers CoreML and commits, returning `BACKEND_COREML`.

- [ ] **Step 7: Give `MediaPipeConfig` a `backend` field**

In `crates/wc-core/src/input/providers/mediapipe/mod.rs`, add the field to `MediaPipeConfig` (after `model_dir`, i.e. after line 112):

```rust
    /// Which execution provider the ONNX sessions should use. Seeded from
    /// [`crate::settings::HandTrackingBackend`] at registry build; applies on
    /// the next `start`. `Auto` (the default) tries the platform GPU EP with a
    /// CPU commit-time fallback.
    pub backend: crate::settings::HandTrackingBackend,
```

And in its `Default` impl (the returned struct literal at lines 117-128), add after `model_dir: …`:

```rust
            backend: crate::settings::HandTrackingBackend::Auto,
```

- [ ] **Step 8: Thread `backend` through `load_model` and log per-model backend at startup**

In `crates/wc-core/src/input/providers/mediapipe/mod.rs`, change `load_model` (lines 491-506). New signature and body:

```rust
fn load_model(
    dir: &Path,
    name: &str,
    backend: crate::settings::HandTrackingBackend,
) -> Result<(Box<dyn HandInference>, &'static str), HandTrackingError> {
    let path = dir.join(name);
    let bytes = std::fs::read(&path).map_err(|e| {
        HandTrackingError::Misconfigured(format!("read model {}: {e}", path.display()))
    })?;
    let model = inference_ort::OrtInference::load(&bytes, backend, name)
        .map_err(|e| HandTrackingError::Misconfigured(e.to_string()))?;
    // Read the backend before boxing — it lives on the concrete type, not the
    // `HandInference` trait object.
    let backend_label = model.backend();
    // Log the effective per-model backend at startup so the field tester's
    // "upload the log" workflow shows which model landed on GPU vs CPU (a commit
    // fallback affects one model, not both). Startup-only; not a hot path.
    tracing::info!(
        model = name,
        backend = backend_label,
        "loaded MediaPipe hand model"
    );
    let boxed: Box<dyn HandInference> = Box::new(model);
    Ok((boxed, backend_label))
}
```

And update `build_pipeline` (lines 275-277) to pass the configured backend:

```rust
        let (palm, palm_backend) = load_model(dir, "palm_detection.onnx", self.config.backend)?;
        let (landmark, landmark_backend) =
            load_model(dir, "hand_landmark.onnx", self.config.backend)?;
```

> The existing combined-backend `tracing::info!` in `start` (mod.rs:531) stays — it reports the merged `palm+landmark` label; the new per-model lines report each stage's actual EP. `combined_backend` (`mod.rs:512`) already folds a mixed state into `BACKEND_DIRECTML_CPU` / `BACKEND_COREML_CPU`, so a palm-only fallback surfaces there too. Do not change `combined_backend` — the tests at `mod.rs:792` and `mod.rs:800` pin its behaviour and remain green.

- [ ] **Step 9: Update the `pipeline.rs` test helper's call site**

In `crates/wc-core/src/input/providers/mediapipe/pipeline.rs`, the `model(name)` test helper (line 1421-1429, inside `#[cfg(test)] mod tests`) calls `OrtInference::load(&bytes)`. Change its final line (1428) from:

```rust
        Box::new(OrtInference::load(&bytes).expect("load model"))
```

to:

```rust
        Box::new(
            OrtInference::load(&bytes, crate::settings::HandTrackingBackend::Auto, name)
                .expect("load model"),
        )
```

- [ ] **Step 10: Run the tests to verify they pass**

```bash
cargo test -p wc-core --all-features --lib input::providers::mediapipe::inference_ort
cargo test -p wc-core --all-features --lib input::providers::mediapipe::pipeline
```

Expected: PASS. The four new decision-logic tests pass; the existing `inference_ort` model tests still load and run (CoreML on macOS via `Auto`); the pipeline tests still build a real two-stage pipeline.

- [ ] **Step 11: Run the scoped gate and commit**

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --all-features --lib input::providers::mediapipe
```

Confirm no stale duplicate remains and the false claim is gone:

```bash
rg -n "load never fails closed" crates/            # expect: no matches
rg -n "commit_from_memory" crates/wc-core/src/input/providers/mediapipe/inference_ort.rs
# expect: exactly two matches, inside commit_accelerated and commit_cpu — none inside `load`
```

Write this message to a file and commit with `-F`:

```
fix(mediapipe): fall back to the CPU EP when a GPU EP fails at commit

OrtInference::load registered the platform GPU EP (DirectML on Windows,
CoreML on macOS) and committed the graph in one shot; a DirectML fusion
crash inside commit_from_memory (0x80004005 in DmlGraphFusionHelper on
some AMD drivers) propagated as a fatal InferenceError::Load. With no
Leap device attached, Windows then had no hand tracking at all.

load now resolves an EpPlan from a HandTrackingBackend preference and,
on Auto, retries a fresh CPU-only session when the accelerated commit
fails, warning with the failing model and node. The retry decision lives
in two pure, GPU-free functions (ep_plan, load_with_ep_fallback) so it is
unit-tested in CI, which has no GPU execution provider. Because load_model
runs once per model and combined_backend already reports mixed states, a
palm-detection failure leaves hand_landmark on the GPU.

Corrects the module and load doc comments that claimed ONNX Runtime's
per-op placement fallback means load "never fails closed" — the false
assumption that produced the bug. Adds a per-model backend log at startup.
```

```bash
git add crates/wc-core/src/input/providers/mediapipe/inference_ort.rs crates/wc-core/src/input/providers/mediapipe/mod.rs crates/wc-core/src/input/providers/mediapipe/pipeline.rs
git commit -F "$TMPDIR/wc-msg"
git show --stat HEAD
```

---

### Task 3: Seed the backend preference from settings into the running provider

**Files:**
- Modify: `crates/waveconductor/src/hand_providers.rs` (the `register_mediapipe` config literal, ~lines 510-517)

**Interfaces:**
- Consumes: `HandTrackingSettings::backend` (Task 1), `MediaPipeConfig::backend` (Task 2).
- Produces: nothing new. Makes the User-facing setting actually control the provider.

**Why the binary.** `MediaPipeConfig` is constructed in exactly one settings-seeded place: `register_mediapipe` in the binary crate (`hand_providers.rs`), alongside the grab/depth/smoothing tunables. Without this line the field defaults to `Auto` and the setting has no effect. This is the only edit outside `input/providers/mediapipe/` and `settings/hand_tracking.rs`, and it is one field in a struct literal.

**Live-apply caveat (decision, not a defect).** `apply_provider_choice` (`hand_providers.rs:250-251`) rebuilds the registry **only when `settings.provider` changes** (`if choice == control.last_applied { … return; }`); the test `unrelated_settings_change_does_not_rebuild_registry` pins that. So changing `backend` alone does **not** rebuild the provider. The tester applies a new backend by relaunching, or by toggling the "Tracking provider" dropdown (e.g. `Auto` → `MediaPipe`), which forces a rebuild — **neither requires a new build**, which is what the requirement asked for. Extending the rebuild trigger to also fire on a `backend` change is deliberately **out of scope** (it would touch `HandProviderControl`'s change-tracking state and that pinned test); see Open Questions.

- [ ] **Step 1: Seed the field**

In `crates/wc-core/src/../../waveconductor/src/hand_providers.rs`, in `register_mediapipe`, add `backend: settings.backend,` to the `MediaPipeConfig` literal. It becomes:

```rust
    let config = MediaPipeConfig {
        smoothing,
        grab_rest_deadzone: settings.grab_rest_deadzone,
        depth_calibration_k: settings.depth_calibration_k,
        smoothing_min_cutoff: settings.smoothing_min_cutoff,
        smoothing_beta: settings.smoothing_beta,
        backend: settings.backend,
        ..MediaPipeConfig::default()
    };
```

- [ ] **Step 2: Run the scoped gate**

`register_mediapipe` is feature-gated (webcam MediaPipe); build the binary with the same features CI uses. There is no cheap unit test for this seed (it constructs a real provider), so verification is the compile plus clippy under `--all-features`:

```bash
cargo fmt --all
cargo clippy -p waveconductor --all-targets --all-features -- -D warnings
```

Expected: clean. (The existing `hand_providers` tests — `unrelated_settings_change_does_not_rebuild_registry` etc. — still pass unchanged, since `backend` is not a rebuild trigger.)

- [ ] **Step 3: Manual smoke test (human)**

There is no GPU EP test in CI and the DirectML path is unreachable on macOS, so a human confirms the knob:

```bash
cargo rund
```

Open the settings panel, find **Hand Tracking → Inference backend**, set it to `ForceCpu`, then toggle the **Tracking provider** dropdown off and back to `MediaPipe` (or relaunch). Confirm the log shows `loaded MediaPipe hand model … backend=ort/CPU` for both models. Set it back to `Auto` and confirm CoreML returns (`backend=ort/CoreML`). On the Windows field-test box, `Auto` should now log a DirectML commit warning naming the failing node followed by a CPU rebuild, and hand tracking should work — where alpha.4 had none.

- [ ] **Step 4: Commit**

Write this message to a file and commit with `-F`:

```
feat(hand-providers): seed MediaPipe backend preference from settings

register_mediapipe now copies HandTrackingSettings::backend into
MediaPipeConfig, so the User-facing Inference backend knob (Auto /
ForceGpu / ForceCpu) controls which execution provider the ONNX sessions
use. Applied at provider (re)start: a relaunch or a Tracking-provider
dropdown toggle picks it up, with no rebuild required. Changing backend
alone does not rebuild the registry (only the provider enum does), which
is intentional.
```

```bash
git add crates/waveconductor/src/hand_providers.rs
git commit -F "$TMPDIR/wc-msg"
git show --stat HEAD
```

---

## Self-Review

**Locked decisions, each mapped to a task.**
- Commit-level retry in `OrtInference::load` (try accelerated → warn → fresh CPU-only builder → `BACKEND_CPU`): **Task 2, Steps 4-5** (`load_with_ep_fallback`, `commit_accelerated`, `commit_cpu`, rewritten `load`).
- Same shape on macOS (CoreML can also fail at commit): the retry is **platform-agnostic** — `commit_accelerated` differs from `commit_cpu` only by the `register_accelerator` call, which is `cfg`-selected per platform, so CoreML gets the identical fallback. **Task 2.**
- DirectML still used wherever it commits (failure behaviour only): `commit_accelerated` is unchanged from today's registration path; only the *error arm* is new. **Task 2.**
- Per-model fallback exploited: `load_model` is per-model and now takes/forwards `backend` and logs per-model; `combined_backend` (unchanged) reports the mixed `BACKEND_DIRECTML_CPU` state. **Task 2, Step 8.**
- `backend: Auto | ForceGpu | ForceCpu` on the existing `hand_tracking.rs` section as a static `ty = Enum`: **Task 1**; wired live: **Task 3.**
- Log effective per-model backend at startup, and the failing node on EP failure: **Task 2** (`load_model`'s `info!`; `load_with_ep_fallback`'s `warn!(%err)` renders the ORT error, which carries the node).
- The false doc comment at `inference_ort.rs:6-9` and `:62-63`: **corrected in Task 2, Step 3** — verbatim replacements shown.

**Anchors re-verified against `v5-alpha` (read with `sed`/`rg`, not run):** `inference_ort.rs` fatal commit at `:98`; register-only guard at `:208-217`; false claims at `:6-9`/`:62-63`; five test `load` call sites at `:386,:414,:437,:469,:495`. `mod.rs` `load_model` per-model at `:276-277`, `combined_backend` at `:512`, `BACKEND_DIRECTML_CPU` at `:69`, mixed-state tests at `:792,:800`. `hand_tracking.rs` `HandProviderChoice` at `:23`, the `provider` `ty = Enum` setting at `:54-62`. `settings/mod.rs` re-export at `:44`. `pipeline.rs` test-helper `load` at `:1428`. `hand_providers.rs` config literal at `:510-517`, rebuild keying at `:250-251`. All confirmed; the extra `pipeline.rs:1428` and binary `hand_providers.rs` call sites (not named in the task brief) are folded into Tasks 2 and 3 so the crate compiles at every commit.

**Placeholder scan:** none. Every code step shows complete code; no "TBD" / "similar to Task N".

**Type consistency (Produces ↔ Consumes):** `load(model_bytes: &[u8], backend: HandTrackingBackend, model_name: &str)` is consumed identically by `load_model` (Task 2, forwarding `self.config.backend: HandTrackingBackend` and `name: &str`) and by all six test call sites. `load_with_ep_fallback`'s closures both return `Result<(S, &'static str), E>`; production instantiates `S = Session, E = InferenceError` (which impls `Display` via `thiserror`); tests instantiate `S = u32, E = String`. `ep_plan(HandTrackingBackend) -> EpPlan` with two `bool` fields; `EpPlan` derives `Debug, Copy, PartialEq, Eq` so the `assert_eq!` struct-literal comparisons compile.

**Tests run on macOS/Linux CI with no GPU EP:** yes. `ep_plan_*` and `ep_fallback_*` use only plain values and simulated closures. The `backend_*` settings tests are TOML round-trips. The pre-existing model-loading tests run on whatever EP the host has (CoreML on the macOS runner via `Auto`, CPU on Linux) and are unchanged in intent.

**Clippy-rule compliance in example code:** test blocks reuse the existing `#[allow(clippy::expect_used, …)]`; no bare `panic!`; no `assert_eq!(_.is_some(), true)`; no `0..(N+1)`; no `as` casts. `load_with_ep_fallback` uses the tracing `%err` field shorthand (Display), not an `as`/format hot-path cost, and it is off the hot path (load-time).

## Open Questions (could not be resolved by reading alone)

1. **Should a `backend`-setting change rebuild the provider live?** As written, it applies on next provider (re)start (relaunch or a Tracking-provider dropdown toggle) — which meets "A/B without a new build." Making it fully live means extending `apply_provider_choice`'s change trigger (`hand_providers.rs:250`) and `HandProviderControl`'s tracked state to include `backend`, plus updating `unrelated_settings_change_does_not_rebuild_registry`. Left out to keep Plan 06 self-contained; flag for the reviewer if a keyboard-free live toggle is wanted on the kiosk.
2. **`ForceGpu` on the deployment box yields no hand tracking if the EP is broken — is that the intended diagnostic behaviour?** This plan treats `ForceGpu` as a loud diagnosis lever (no fallback), with `Auto` as the safe default. Confirm that reading; if `ForceGpu` should instead mean "prefer GPU but still fall back," it collapses into `Auto` and the variant is redundant.
3. **`cargo`-gated facts I could not check (no builds allowed here):** that `tracing::warn!(model = …, %err, …)` compiles as written under this `tracing` version; that `commit_from_memory` consumes `SessionBuilder` by value (assumed from the current `let mut builder … commit_from_memory` usage and the brief's "the builder is consumed"); and that adding `use std::fmt::Display;` plus `use crate::settings::HandTrackingBackend;` raises no unused/ordering warning under `-D warnings`. All are standard, but the implementer should treat the first failing gate as the source of truth and adjust (e.g. field-shorthand form) rather than assuming the code is wrong.
