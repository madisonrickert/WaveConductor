//! GPU-accelerated ONNX inference via `ort` (ONNX Runtime).
//!
//! The alternative [`HandInference`] backend to the pure-Rust `tract` path
//! ([`super::inference::TractInference`]), for when CPU inference is the
//! bottleneck. On macOS `ort`'s `CoreML` execution provider runs the conv-heavy
//! palm + landmark models on the GPU/Neural Engine; the measured tract CPU cost
//! (`palm.run` ≈ 164 ms) makes interaction stall, especially on the periodic
//! palm re-detect. Same models (ONNX, no conversion), same trait, so the
//! pipeline is unchanged — only the backend swaps.
//!
//! Compiled only under the `hand-tracking-mediapipe-ort` feature. Selection is
//! at runtime (see [`super::use_ort_backend`]): the feature defaults to `ort`,
//! and `WAVECONDUCTOR_HAND_INFERENCE=tract` forces the pure-Rust path for A/B.

use ort::execution_providers::coreml::CoreMLExecutionProvider;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor as OrtTensor;

use super::inference::{HandInference, InferenceError, Tensor};

/// `ort`-backed inference for one ONNX model stage.
///
/// Output tensors are read back in the model's **declared output order** (not the
/// map iteration order), because the landmark stage's downstream selection treats
/// the first `[1, 1]` output as presence and the second as handedness.
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
    fn ort_landmark_model_runs_and_emits_expected_shapes() {
        // The ort backend must yield the same output set the pipeline matches on
        // by shape, in declared order: two [1,63] landmark tensors and two [1,1]
        // scalars. On a host without CoreML, ort falls back to CPU — still
        // exercising load + run + the declared-order shape extraction.
        let mut model =
            OrtInference::load(&model_bytes("hand_landmark.onnx")).expect("load via ort");
        let out = model
            .run(&Tensor::zeros(vec![1, 224, 224, 3]))
            .expect("ort landmark forward pass");
        let shapes: Vec<&[usize]> = out.iter().map(|t| t.shape.as_slice()).collect();
        assert_eq!(out.len(), 4, "shapes={shapes:?}");
        assert_eq!(
            shapes.iter().filter(|s| **s == [1, 63]).count(),
            2,
            "shapes={shapes:?}"
        );
        assert_eq!(
            shapes.iter().filter(|s| **s == [1, 1]).count(),
            2,
            "shapes={shapes:?}"
        );
    }
}
