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

use ort::execution_providers::coreml::{CoreMLComputeUnits, CoreMLExecutionProvider};
use ort::execution_providers::ExecutionProvider;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor as OrtTensor;

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
    /// `MLProgram` format covers more ops in principle, but its stricter parser
    /// rejects these vendored `MediaPipe` graphs at compile time (their `MaxPool`
    /// nodes omit the `pad` param `MLProgram` requires), failing the session
    /// build outright — so we stay on the format that has always loaded and
    /// accelerated them. `Core ML` places each segment on ANE/GPU/CPU itself, and
    /// the compiled artifact is cached on disk to skip recompiling every launch.
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
        let load_err = |e: ort::Error| InferenceError::Load(e.to_string());

        let mut coreml = CoreMLExecutionProvider::default()
            // ALL lets Core ML place each segment on ANE/GPU/CPU as it sees fit
            // (the default, set explicitly). The default NeuralNetwork model
            // format is kept deliberately — see the doc comment: MLProgram fails
            // to compile these vendored models.
            .with_compute_units(CoreMLComputeUnits::All);
        // Core ML compiles each model to a native artifact on first load; caching
        // it on disk avoids paying that compile every launch. A missing cache dir
        // is non-fatal — we just recompile each run.
        if let Some(cache) = coreml_cache_dir() {
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
            .inputs
            .first()
            .ok_or_else(|| InferenceError::Load("model has no inputs".into()))?
            .name
            .clone();
        let output_names = session.outputs.iter().map(|o| o.name.clone()).collect();
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

/// Resolve the on-disk `CoreML` model-cache directory
/// (`<cache>/waveconductor/coreml-cache`), creating it if absent.
///
/// Returns `None` when no cache dir is available or it cannot be created; the
/// caller then loads without a cache (recompiling the Core ML artifact each run)
/// rather than failing.
fn coreml_cache_dir() -> Option<PathBuf> {
    let dir = dirs::cache_dir()?
        .join("waveconductor")
        .join("coreml-cache");
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
    /// Two residual copies remain, both bound by the `ort`/trait APIs rather than
    /// the pipeline: the input is cloned into an owned `OrtTensor`
    /// (`from_array` takes ownership; ≈110 KB palm / ≈150 KB landmark f32), and
    /// each output is copied out of `ort`'s arena into our runtime-agnostic
    /// `Tensor` (the trait returns owned `Vec`s; the largest is the ≈145 KB palm
    /// box tensor). The pipeline's per-frame *input* buffer is already reused
    /// upstream (see [`super::pipeline::Pipeline`]); removing these last copies
    /// needs `ort` I/O binding with preallocated tensors — a narrow,
    /// profiling-gated follow-up tied to the `ort` upgrade path, not done blind.
    fn run(&mut self, input: &Tensor) -> Result<Vec<Tensor>, InferenceError> {
        let run_err = |e: ort::Error| InferenceError::Run(e.to_string());

        // ort tensor shapes are `i64`; our `usize` dims convert infallibly for
        // any realistic image/landmark tensor.
        let shape: Vec<i64> = input
            .shape
            .iter()
            .map(|&d| i64::try_from(d))
            .collect::<Result<_, _>>()
            .map_err(|e| InferenceError::Run(format!("input dim overflow: {e}")))?;
        let in_tensor = OrtTensor::from_array((shape, input.data.clone())).map_err(run_err)?;

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
