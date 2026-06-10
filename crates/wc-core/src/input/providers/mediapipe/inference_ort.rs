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

use ort::execution_providers::coreml::CoreMLExecutionProvider;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor as OrtTensor;

use super::inference::{HandInference, InferenceError, Tensor};

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
}

impl OrtInference {
    /// Load an ONNX model from its bytes, registering the `CoreML` execution
    /// provider (ONNX Runtime falls back to CPU for any unsupported op).
    ///
    /// # Errors
    /// Returns [`InferenceError::Load`] if the session cannot be built or the
    /// model has no input.
    pub fn load(model_bytes: &[u8]) -> Result<Self, InferenceError> {
        let load_err = |e: ort::Error| InferenceError::Load(e.to_string());
        let session = Session::builder()
            .map_err(load_err)?
            // CoreML first; ONNX Runtime falls back to CPU for any op CoreML
            // cannot run, so this never fails closed on an unsupported operator.
            .with_execution_providers([CoreMLExecutionProvider::default().build()])
            .map_err(load_err)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(load_err)?
            .commit_from_memory(model_bytes)
            .map_err(load_err)?;

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
        })
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
