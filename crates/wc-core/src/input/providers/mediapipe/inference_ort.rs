//! ONNX Runtime (`ort`) inference backend for the MediaPipe hand-tracking pipeline.
//!
//! `OrtInference` is the sole concrete [`HandInference`] implementation. It
//! registers a platform GPU execution provider so the conv-heavy palm and landmark
//! models run off the CPU: `CoreML` on macOS (GPU/Neural Engine; measured ~164 ms
//! CPU-only down to well under the 33 ms/frame budget at 30 Hz) and `DirectML` on
//! Windows (vendor-neutral DX12, covering AMD/Intel integrated GPUs). Other targets
//! (Linux) run on ONNX Runtime's CPU EP. ONNX Runtime partitions the graph and
//! places any op the EP cannot support back on the CPU — but that is *per-op
//! placement* fallback, not a safety net against an EP that fails at *commit*: a
//! GPU EP can register cleanly and then throw while fusing the graph (observed as
//! `DirectML` `DmlGraphFusionHelper` `0x80004005` on some AMD drivers), aborting
//! the whole load. `load` therefore retries on a fresh CPU-only session when the
//! accelerated commit fails, so a broken GPU EP degrades one model to CPU instead
//! of losing hand tracking entirely (see `load_with_ep_fallback`).
//!
//! (Plain code span, not an intra-doc link: `mod inference_ort;` in the parent
//! carries an outer doc comment, so rustdoc resolves this module's merged doc
//! fragments in the *parent's* scope, where a private item of this module is not
//! nameable. The links inside `load`'s own doc resolve normally.)
//!
//! `ort` ships the C++ ONNX Runtime as a prebuilt native binary downloaded at build
//! time (`download-binaries` feature). The binary is subject to the
//! CDLA-Permissive-2.0 license, already allowed in `deny.toml`.
//!
//! The same vendored `.onnx` models used throughout the pipeline work without
//! conversion; only the backend changes.

use std::fmt::Display;
#[cfg(target_os = "macos")]
use std::path::PathBuf;

#[cfg(target_os = "macos")]
use ort::ep::coreml::ComputeUnits;
#[cfg(target_os = "macos")]
use ort::ep::CoreML;
#[cfg(target_os = "windows")]
use ort::ep::DirectML;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use ort::ep::ExecutionProvider;
use ort::session::builder::{GraphOptimizationLevel, SessionBuilder};
use ort::session::Session;
use ort::value::TensorRef;

use super::inference::{HandInference, InferenceError, Tensor};
use crate::settings::HandTrackingBackend;

/// Backend label when the session runs on ONNX Runtime's CPU execution provider
/// (no GPU EP for this target, or the EP failed to register and fell back).
pub(super) const BACKEND_CPU: &str = "ort/CPU";
/// Backend label when the macOS `CoreML` execution provider registered.
pub(super) const BACKEND_COREML: &str = "ort/CoreML";
/// Backend label when the Windows `DirectML` execution provider registered.
pub(super) const BACKEND_DIRECTML: &str = "ort/DirectML";

/// `ort`-backed inference for one ONNX model stage.
///
/// Output tensors are read back in the model's **declared output order** (not the
/// map iteration order), because the landmark stage's downstream selection is
/// index-based on that order: 0 image landmarks, 1 presence, 2 handedness,
/// 3 world landmarks.
pub struct OrtInference {
    session: Session,
    input_name: String,
    output_names: Vec<String>,
    backend: &'static str,
    /// Reused `i64` shape buffer for the input tensor view. The input shape is
    /// fixed per model, so refilling this in place each frame (rather than
    /// `collect`ing a fresh `Vec`) keeps `run` off the per-frame allocator.
    input_shape: Vec<i64>,
}

impl OrtInference {
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
    ///
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

    /// The inference backend this session registered: `"ort/CoreML"` (macOS) or
    /// `"ort/DirectML"` (Windows) when the platform GPU EP attached, or
    /// [`BACKEND_CPU`] (`"ort/CPU"`) when there is no GPU EP for the target or it
    /// fell back.
    ///
    /// This reflects registration success, not whole-graph placement: the EP may
    /// still partition unsupported ops back onto the CPU at commit time. To
    /// confirm what actually ran where on a given host, run with
    /// `ORT_LOG=verbose RUST_LOG=ort=trace` and read the node-placement dump.
    pub fn backend(&self) -> &'static str {
        self.backend
    }
}

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
/// On Windows this also applies the `DirectML` session options
/// (`configure_accelerator_session`); on Linux [`register_accelerator`] is a
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

/// Map an `ort` error to a model-load failure. Generic over the recovery
/// context `R` because rc.12's `SessionBuilder` error-recovery API parameterizes
/// `ort::Error<R>` by the value `.recover()` would hand back (`SessionBuilder`,
/// `Session`, or `()`), so a single non-generic closure can't span the call
/// sites here.
fn load_err<R>(e: ort::Error<R>) -> InferenceError {
    InferenceError::Load(e.to_string())
}

/// Map an `ort` error to an inference-run failure. Generic over the recovery
/// context for the same reason as [`load_err`].
fn run_err<R>(e: ort::Error<R>) -> InferenceError {
    InferenceError::Run(e.to_string())
}

/// Apply session options required by the platform accelerator before registering
/// the execution provider.
///
/// Windows `DirectML` rejects memory-pattern optimization and parallel graph
/// execution, so both are disabled explicitly here. Parallel execution is
/// already ONNX Runtime's default-off state, but spelling it out keeps the
/// DirectML contract close to the registration site and catches future builder
/// default changes. `CoreML` and CPU targets need no special session options.
#[cfg(target_os = "windows")]
fn configure_accelerator_session(
    builder: SessionBuilder,
) -> Result<SessionBuilder, InferenceError> {
    builder
        .with_parallel_execution(false)
        .map_err(load_err)?
        .with_memory_pattern(false)
        .map_err(load_err)
}

/// Register the macOS `CoreML` execution provider on `builder`, returning the
/// backend label (`CoreML` on success, CPU on a graceful fallback).
///
/// `CoreML` runs in its default `NeuralNetwork` model format. The newer
/// `MLProgram` format covers a few more ops in principle, but its stricter parser
/// rejects these vendored `MediaPipe` graphs at compile time: the build fails on a
/// fused `Conv` op with `Required param 'pad' is missing`. Even patched it only
/// reaches 27 `CoreML` partitions — worse than `NeuralNetwork`'s 6 once the palm
/// model's `PReLU` slopes are reshaped to the `[C, 1, 1]` shape the EP accepts — so
/// we stay on `NeuralNetwork`. `Core ML` places each segment on ANE/GPU/CPU itself,
/// and the compiled artifact is cached on disk per model ([`coreml_cache_dir`]) to
/// skip recompiling every launch.
#[cfg(target_os = "macos")]
fn register_accelerator(builder: &mut SessionBuilder, model_bytes: &[u8]) -> &'static str {
    let mut coreml = CoreML::default()
        // ALL lets Core ML place each segment on ANE/GPU/CPU as it sees fit (the
        // default, set explicitly). NeuralNetwork model format is kept deliberately
        // — see the doc comment: MLProgram fails to compile these vendored models.
        .with_compute_units(ComputeUnits::All);
    // Core ML compiles each model to a native artifact on first load; caching it on
    // disk avoids paying that compile every launch. A missing cache dir is
    // non-fatal — we just recompile each run.
    if let Some(cache) = coreml_cache_dir(model_bytes) {
        coreml = coreml.with_model_cache_dir(cache.display());
    }
    match coreml.register(builder) {
        Ok(()) => BACKEND_COREML,
        Err(e) => {
            tracing::warn!("CoreML EP registration failed; running on CPU: {e}");
            BACKEND_CPU
        }
    }
}

/// Register the Windows `DirectML` execution provider on `builder`, returning the
/// backend label (`DirectML` on success, CPU on a graceful fallback).
///
/// DirectML is the vendor-neutral DX12 EP, so one path accelerates AMD and Intel
/// integrated GPUs alike. Registration fails gracefully to the CPU EP on a host
/// with no DX12 device (e.g. a GPU-less CI runner).
#[cfg(target_os = "windows")]
fn register_accelerator(builder: &mut SessionBuilder, _model_bytes: &[u8]) -> &'static str {
    match DirectML::default().register(builder) {
        Ok(()) => BACKEND_DIRECTML,
        Err(e) => {
            tracing::warn!("DirectML EP registration failed; running on CPU: {e}");
            BACKEND_CPU
        }
    }
}

/// No GPU execution provider for this target (e.g. Linux): the session runs on
/// ONNX Runtime's CPU EP. `builder` is left untouched.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn register_accelerator(_builder: &mut SessionBuilder, _model_bytes: &[u8]) -> &'static str {
    BACKEND_CPU
}

/// Compute a stable per-model cache key from the model bytes.
///
/// ONNX Runtime's `CoreML` EP names its compiled-artifact subdirectory by a
/// model hash that does **not** change when only our model's initializers change:
/// the palm model's `PReLU` slope reshape (`[1,C,1,1]` → `[C,1,1]`, which moves
/// `PReLU` onto `CoreML` and collapses the graph from 30 partitions to 6) leaves
/// that EP-side key identical to the pre-reshape model's. Without our own
/// namespacing, a model update therefore loads the *previous* model's stale
/// compiled partition and fails at inference with `output_features has no value`.
/// Hashing the model bytes here lands every distinct model in its own directory,
/// so a changed model can never collide with a prior compile.
///
/// The hash only needs to be stable within a single binary (the same build that
/// wrote the cache reads it back), so a `std` hasher suffices and adds no
/// dependency. A toolchain change that alters the hash merely forces a one-time
/// recompile, which is harmless.
///
/// macOS-only: the on-disk EP artifact cache is a `CoreML` concern.
#[cfg(target_os = "macos")]
fn model_cache_key(model_bytes: &[u8]) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    model_bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Resolve the on-disk `CoreML` model-cache directory for a specific model
/// (`<cache>/waveconductor/coreml-cache/<model-key>`), creating it if absent.
///
/// The per-model `<model-key>` ([`model_cache_key`]) is what makes reusing the
/// cache across model revisions safe — see that function for why a directory
/// shared between models corrupts after a model change.
///
/// Disabled under `cfg(test)`: the unit tests load the same model from many
/// parallel processes, and ONNX Runtime's `CoreML` EP is not safe against two of
/// them populating the shared cache directory at once (the loser of the
/// move-into-place race fails with "an item with the same name already exists").
/// A test loads each model once, so the cache buys nothing, and skipping it also
/// keeps tests from writing into the real user cache dir. Production (non-test)
/// keeps the cache for fast startup, where each model is loaded exactly once.
///
/// Returns `None` when caching is disabled, no cache dir is available, or it
/// cannot be created; the caller then loads without a cache (recompiling the
/// Core ML artifact each run) rather than failing.
#[cfg(target_os = "macos")]
fn coreml_cache_dir(model_bytes: &[u8]) -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let dir = dirs::cache_dir()?
        .join("waveconductor")
        .join("coreml-cache")
        .join(model_cache_key(model_bytes));
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(e) => {
            tracing::warn!("CoreML cache dir {} unavailable: {e}", dir.display());
            None
        }
    }
}

impl HandInference for OrtInference {
    /// Run one stage.
    ///
    /// Allocation-free on the steady-state hot path. The input is bound as a
    /// borrowed [`TensorRef`] view over the pipeline's reused per-frame input
    /// buffer (no input copy; each frame previously cloned ≈0.4 MB palm / ≈0.6 MB
    /// landmark f32). Each output is written into `out`, a buffer the caller owns
    /// and reuses across frames: `out` is grown/truncated to the model's output
    /// count once, then each tensor's `data`/`shape` is refilled in place
    /// (`clear` keeps capacity). After the first call neither the input shape, the
    /// output container, nor the output data allocates.
    ///
    /// Outputs are written in the model's **declared output order** (see the
    /// struct doc), which the landmark stage selects by index.
    fn run(&mut self, input: &Tensor, out: &mut Vec<Tensor>) -> Result<(), InferenceError> {
        // Refill the reused i64 shape buffer in place (ort shapes are i64; our
        // usize dims convert infallibly for any realistic image/landmark tensor).
        self.input_shape.clear();
        for &d in &input.shape {
            self.input_shape.push(
                i64::try_from(d)
                    .map_err(|e| InferenceError::Run(format!("input dim overflow: {e}")))?,
            );
        }
        let in_tensor =
            TensorRef::from_array_view((self.input_shape.as_slice(), input.data.as_slice()))
                .map_err(run_err)?;

        let outputs = self
            .session
            .run(ort::inputs![self.input_name.as_str() => in_tensor])
            .map_err(run_err)?;

        // Reuse `out`: size it to the output count, then refill each tensor's
        // buffers in place, in declared order. `Shape` derefs to `[i64]`.
        out.truncate(self.output_names.len());
        while out.len() < self.output_names.len() {
            out.push(Tensor::default());
        }
        for (slot, name) in out.iter_mut().zip(&self.output_names) {
            let (shape, data) = outputs[name.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(run_err)?;
            slot.data.clear();
            slot.data.extend_from_slice(data);
            slot.shape.clear();
            for &d in shape.iter() {
                slot.shape.push(
                    usize::try_from(d)
                        .map_err(|e| InferenceError::Run(format!("bad output dim: {e}")))?,
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg(target_os = "macos")]
    #[test]
    fn coreml_cache_key_is_per_model_and_deterministic() {
        // Regression: ONNX Runtime's CoreML EP reuses one on-disk cache key
        // across our model revisions, so after a model change it would serve the
        // previous model's stale compiled partition and fail at inference with
        // "output_features has no value" (observed when the PReLU slope reshape
        // collapsed the palm graph 30 -> 6 partitions against a 30-partition
        // cache). The cache directory must be namespaced by model content:
        // distinct bytes -> distinct key, identical bytes -> identical key.
        let v1 = model_cache_key(b"palm-model-rev-1");
        let v2 = model_cache_key(b"palm-model-rev-2");
        assert_ne!(
            v1, v2,
            "different model bytes must namespace to different cache keys"
        );
        assert_eq!(
            v1,
            model_cache_key(b"palm-model-rev-1"),
            "the same model bytes must map to the same cache key"
        );
    }

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

    fn model_bytes(name: &str) -> Vec<u8> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/models/hand")
            .join(name);
        std::fs::read(path).expect("read vendored model")
    }

    #[test]
    fn backend_label_is_one_of_the_known_values() {
        // The backend label must be observable and one of the two known states,
        // so a silent CPU fallback (the 240% CPU symptom) is never hidden behind
        // an empty or bogus string in diagnostics.
        let model = OrtInference::load(
            &model_bytes("palm_detection.onnx"),
            HandTrackingBackend::Auto,
            "palm_detection.onnx",
        )
        .expect("load via ort");
        let backend = model.backend();
        // The label must be observable and one of the known states, so a silent
        // CPU fallback (the 240% CPU symptom) is never hidden behind a bogus string
        // in diagnostics. Which GPU EP is expected depends on the target: CoreML on
        // macOS (compiled in via the `coreml` ort feature; load succeeded above, so
        // anything else is a real registration regression), DirectML-or-CPU on
        // Windows (CPU when the runner has no DX12 device), CPU elsewhere.
        #[cfg(target_os = "macos")]
        assert_eq!(backend, BACKEND_COREML, "CoreML must register on macOS");
        #[cfg(target_os = "windows")]
        assert!(
            backend == BACKEND_DIRECTML || backend == BACKEND_CPU,
            "Windows backend must be DirectML or CPU fallback, got {backend:?}"
        );
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(
            backend, BACKEND_CPU,
            "non-accelerated targets use the CPU EP"
        );
    }

    #[test]
    fn ort_palm_model_runs_and_emits_raw_box_and_score_tensors() {
        // The graph-surgeried palm detector: input [1,192,192,3] → raw
        // [1,2016,18] boxes + [1,2016,1] scores (anchor decode + NMS are done
        // in Rust, not in the graph). Proves ort loads and runs it in-crate.
        let mut model = OrtInference::load(
            &model_bytes("palm_detection.onnx"),
            HandTrackingBackend::Auto,
            "palm_detection.onnx",
        )
        .expect("load via ort");
        let mut out = Vec::new();
        model
            .run(&Tensor::zeros(vec![1, 192, 192, 3]), &mut out)
            .expect("ort palm forward pass");
        let shapes: Vec<&[usize]> = out.iter().map(|t| t.shape.as_slice()).collect();
        assert!(
            shapes.contains(&[1, 2016, 18].as_slice()),
            "shapes={shapes:?}"
        );
        assert!(
            shapes.contains(&[1, 2016, 1].as_slice()),
            "shapes={shapes:?}"
        );
    }

    #[test]
    fn ort_landmark_model_runs_and_emits_expected_shapes() {
        // The ort backend must yield the output set the pipeline selects by
        // declared index order: two [1,63] landmark tensors and two [1,1]
        // scalars. On a host without CoreML, ort falls back to CPU — still
        // exercising load + run + the declared-order shape extraction.
        let mut model = OrtInference::load(
            &model_bytes("hand_landmark.onnx"),
            HandTrackingBackend::Auto,
            "hand_landmark.onnx",
        )
        .expect("load via ort");
        let mut out = Vec::new();
        model
            .run(&Tensor::zeros(vec![1, 224, 224, 3]), &mut out)
            .expect("ort landmark forward pass");
        let shapes: Vec<&[usize]> = out.iter().map(|t| t.shape.as_slice()).collect();
        assert_eq!(out.len(), 4, "shapes={shapes:?}");
        // Positional: the pipeline selects by declared index order, so each
        // index must carry its declared shape — not merely the right multiset.
        assert_eq!(out[0].shape, vec![1, 63], "output 0: image landmarks");
        assert_eq!(out[1].shape, vec![1, 1], "output 1: presence");
        assert_eq!(out[2].shape, vec![1, 1], "output 2: handedness");
        assert_eq!(out[3].shape, vec![1, 63], "output 3: world landmarks");
    }

    #[test]
    fn ort_landmark_presence_is_a_probability_from_the_graph() {
        // Premise lock: the vendored hand_landmark.onnx applies a Sigmoid op to
        // the presence head INSIDE the graph, so declared output 1 is already a
        // probability and the pipeline must NOT sigmoid it again. An all-zeros
        // input contains no hand, so presence must read low. If a future model
        // swap ships raw logits instead (no baked-in activation), an empty
        // input's logit would be strongly negative — outside what this asserts
        // only by luck — while a logit-positive model or a non-[0,1] head fails
        // here loudly before the pipeline silently misreads it.
        //
        // The handedness head's baked-in sigmoid (declared output 2) is NOT
        // separately pinned here: proving it needs a hand-shaped input (an
        // empty frame says nothing about handedness either way). It is covered
        // at the mock level by the pipeline test
        // `handedness_probability_below_half_reads_left`.
        let mut model = OrtInference::load(
            &model_bytes("hand_landmark.onnx"),
            HandTrackingBackend::Auto,
            "hand_landmark.onnx",
        )
        .expect("load via ort");
        let mut out = Vec::new();
        model
            .run(&Tensor::zeros(vec![1, 224, 224, 3]), &mut out)
            .expect("ort landmark forward pass");
        assert_eq!(
            out[1].shape,
            vec![1, 1],
            "declared output 1 must be the presence scalar"
        );
        let presence = *out[1].data.first().expect("presence scalar");
        assert!(
            (0.0..=1.0).contains(&presence),
            "presence {presence} outside [0, 1] — model head is not pre-activated"
        );
        assert!(
            presence < 0.5,
            "presence {presence} on an empty (all-zeros) input should be < 0.5"
        );
    }

    #[test]
    fn ort_run_rejects_wrong_input_shape() {
        // ONNX Runtime should return an error (not panic) when the input tensor
        // has a shape that disagrees with the model's declared input.
        let mut model = OrtInference::load(
            &model_bytes("hand_landmark.onnx"),
            HandTrackingBackend::Auto,
            "hand_landmark.onnx",
        )
        .expect("load via ort");
        // Landmark model expects [1,224,224,3]; supply a palm-sized input instead.
        let mut out = Vec::new();
        let err = model
            .run(&Tensor::zeros(vec![1, 192, 192, 3]), &mut out)
            .expect_err("shape mismatch should return an error");
        assert!(matches!(err, InferenceError::Run(_)));
    }
}
