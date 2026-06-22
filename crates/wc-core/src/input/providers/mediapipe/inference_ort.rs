//! ONNX Runtime (`ort`) inference backend for the MediaPipe hand-tracking pipeline.
//!
//! [`OrtInference`] is the sole concrete [`HandInference`] implementation. On macOS
//! it registers the `CoreML` execution provider so the conv-heavy palm and landmark
//! models run on the GPU/Neural Engine (measured improvement from ~164 ms CPU-only
//! to well under the 33 ms/frame budget at 30 Hz). ONNX Runtime falls back to CPU
//! for any op CoreML cannot handle, so load never fails closed on an unsupported
//! operator.
//!
//! `ort` ships the C++ ONNX Runtime as a prebuilt native binary downloaded at build
//! time (`download-binaries` feature). The binary is subject to the
//! CDLA-Permissive-2.0 license, already allowed in `deny.toml`.
//!
//! The same vendored `.onnx` models used throughout the pipeline work without
//! conversion; only the backend changes.

use std::path::PathBuf;

use ort::ep::coreml::ComputeUnits;
use ort::ep::{CoreML, ExecutionProvider};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::TensorRef;

use super::inference::{HandInference, InferenceError, Tensor};

/// Backend label when the `CoreML` execution provider registered successfully.
const BACKEND_COREML: &str = "ort/CoreML";
/// Backend label when `CoreML` registration failed and the session runs on CPU.
const BACKEND_CPU: &str = "ort/CPU";

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
}

impl OrtInference {
    /// Load an ONNX model from its bytes, registering the `CoreML` execution
    /// provider (ONNX Runtime falls back to CPU for any unsupported op).
    ///
    /// `CoreML` runs in its default `NeuralNetwork` model format. The newer
    /// `MLProgram` format covers a few more ops in principle, but its stricter
    /// parser rejects these vendored `MediaPipe` graphs at compile time: the build
    /// fails on a fused `Conv` op with `Required param 'pad' is missing`. Even
    /// patched it only reaches 27 `CoreML` partitions — worse than
    /// `NeuralNetwork`'s 6 once the palm model's `PReLU` slopes are reshaped to the
    /// `[C, 1, 1]` shape the EP accepts — so we stay on `NeuralNetwork`. `Core ML`
    /// places each segment on ANE/GPU/CPU itself, and the compiled artifact is
    /// cached on disk per model ([`coreml_cache_dir`]) to skip recompiling every
    /// launch.
    ///
    /// The session's CPU thread pool is capped to two intra-op threads with
    /// spin-waiting disabled: two sessions (palm + landmark) each own a pool, and
    /// ONNX Runtime's default spin-wait kept whole cores busy between frames at
    /// our `<= 30 Hz` cadence even when most of the graph was on `Core ML`. This
    /// is independent of model format and is the main idle-CPU fix.
    ///
    /// # Errors
    /// Returns [`InferenceError::Load`] if the session cannot be built or the
    /// model has no input.
    pub fn load(model_bytes: &[u8]) -> Result<Self, InferenceError> {
        let mut coreml = CoreML::default()
            // ALL lets Core ML place each segment on ANE/GPU/CPU as it sees fit
            // (the default, set explicitly). The default NeuralNetwork model
            // format is kept deliberately — see the doc comment: MLProgram fails
            // to compile these vendored models.
            .with_compute_units(ComputeUnits::All);
        // Core ML compiles each model to a native artifact on first load; caching
        // it on disk avoids paying that compile every launch. A missing cache dir
        // is non-fatal — we just recompile each run.
        if let Some(cache) = coreml_cache_dir(model_bytes) {
            coreml = coreml.with_model_cache_dir(cache.display());
        }

        // Two-phase build: `ExecutionProvider::register` takes `&mut
        // SessionBuilder` (unlike the by-value builder methods), so the EP is
        // registered separately and its outcome recorded as an observable
        // backend label.
        let mut builder = Session::builder()
            .map_err(load_err)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(load_err)?
            // Two sessions (palm + landmark) each own a CPU thread pool; capping
            // intra-op threads and disabling spin-waiting stops idle inference
            // from burning whole cores between frames at our cadence.
            .with_intra_threads(2)
            .map_err(load_err)?
            .with_intra_op_spinning(false)
            .map_err(load_err)?;

        // `Ok(())` means the EP attached to the session options, NOT that every
        // node runs on CoreML — the graph is partitioned at commit and any
        // unsupported op still falls to the CPU. The label reflects registration
        // success, not whole-graph placement (see [`Self::backend`]).
        let backend = match coreml.register(&mut builder) {
            Ok(()) => BACKEND_COREML,
            Err(e) => {
                tracing::warn!("CoreML EP registration failed; running on CPU: {e}");
                BACKEND_CPU
            }
        };

        let session = builder.commit_from_memory(model_bytes).map_err(load_err)?;

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
            backend,
        })
    }

    /// The inference backend this session registered: [`BACKEND_COREML`]
    /// (`"ort/CoreML"`) when the `CoreML` execution provider attached, or
    /// [`BACKEND_CPU`] (`"ort/CPU"`) when it fell back.
    ///
    /// This reflects registration success, not whole-graph placement: `CoreML` may
    /// still partition unsupported ops back onto the CPU at commit time. To
    /// confirm what actually ran where on a given host, run with
    /// `ORT_LOG=verbose RUST_LOG=ort=trace` and read the node-placement dump.
    pub fn backend(&self) -> &'static str {
        self.backend
    }
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
    /// The input is **not** copied: it is bound as a borrowed [`TensorRef`] view
    /// over the pipeline's reused per-frame input buffer (see
    /// [`super::pipeline::Pipeline`]), so no per-frame input allocation happens on
    /// the hot path. Each frame previously cloned the whole input (≈0.4 MB palm /
    /// ≈0.6 MB landmark f32) into an owned tensor; `from_array_view` removes that.
    ///
    /// One copy remains, forced by the trait: each output is copied out of `ort`'s
    /// arena into our runtime-agnostic `Tensor` because the trait returns owned
    /// `Vec`s (largest is the ≈145 KB palm box tensor). Removing it too would need
    /// `ort` I/O binding with preallocated output buffers *and* a trait change so
    /// `run` writes into caller-owned storage. On Apple Silicon's unified memory
    /// there is no host/device transfer for I/O binding to save, so that is a
    /// profiling-gated follow-up, not done blind.
    fn run(&mut self, input: &Tensor) -> Result<Vec<Tensor>, InferenceError> {
        // ort tensor shapes are `i64`; our `usize` dims convert infallibly for
        // any realistic image/landmark tensor.
        let shape: Vec<i64> = input
            .shape
            .iter()
            .map(|&d| i64::try_from(d))
            .collect::<Result<_, _>>()
            .map_err(|e| InferenceError::Run(format!("input dim overflow: {e}")))?;
        let in_tensor =
            TensorRef::from_array_view((shape, input.data.as_slice())).map_err(run_err)?;

        let outputs = self
            .session
            .run(ort::inputs![self.input_name.as_str() => in_tensor])
            .map_err(run_err)?;

        // Re-materialize each output in declared order as our runtime-agnostic
        // `Tensor` (owned `f32` + `usize` shape). `Shape` derefs to `[i64]`.
        let mut result = Vec::with_capacity(self.output_names.len());
        for name in &self.output_names {
            let (shape, data) = outputs[name.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(run_err)?;
            let dims: Result<Vec<usize>, _> = shape.iter().map(|&d| usize::try_from(d)).collect();
            let shape = dims.map_err(|e| InferenceError::Run(format!("bad output dim: {e}")))?;
            result.push(Tensor {
                data: data.to_vec(),
                shape,
            });
        }
        Ok(result)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        let model = OrtInference::load(&model_bytes("palm_detection.onnx")).expect("load via ort");
        let backend = model.backend();
        assert!(
            backend == BACKEND_COREML || backend == BACKEND_CPU,
            "unexpected backend label {backend:?}"
        );
        // On macOS the CoreML EP is compiled in (the `coreml` ort feature) and
        // must register against these vendored models — load succeeded above, so
        // anything but CoreML here means a real registration regression.
        #[cfg(target_os = "macos")]
        assert_eq!(backend, BACKEND_COREML, "CoreML must register on macOS");
    }

    #[test]
    fn ort_palm_model_runs_and_emits_raw_box_and_score_tensors() {
        // The graph-surgeried palm detector: input [1,192,192,3] → raw
        // [1,2016,18] boxes + [1,2016,1] scores (anchor decode + NMS are done
        // in Rust, not in the graph). Proves ort loads and runs it in-crate.
        let mut model =
            OrtInference::load(&model_bytes("palm_detection.onnx")).expect("load via ort");
        let out = model
            .run(&Tensor::zeros(vec![1, 192, 192, 3]))
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
        let mut model =
            OrtInference::load(&model_bytes("hand_landmark.onnx")).expect("load via ort");
        let out = model
            .run(&Tensor::zeros(vec![1, 224, 224, 3]))
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
        let mut model =
            OrtInference::load(&model_bytes("hand_landmark.onnx")).expect("load via ort");
        let out = model
            .run(&Tensor::zeros(vec![1, 224, 224, 3]))
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
        let mut model =
            OrtInference::load(&model_bytes("hand_landmark.onnx")).expect("load via ort");
        // Landmark model expects [1,224,224,3]; supply a palm-sized input instead.
        let err = model
            .run(&Tensor::zeros(vec![1, 192, 192, 3]))
            .expect_err("shape mismatch should return an error");
        assert!(matches!(err, InferenceError::Run(_)));
    }
}
