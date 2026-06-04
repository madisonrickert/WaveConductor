//! ONNX inference behind a runtime-agnostic trait.
//!
//! `tract` (pure Rust, single self-contained binary) is the primary
//! implementation — the verification spike confirmed it runs both `MediaPipe`
//! hand models (the landmark model bit-exactly; the palm model after the
//! committed `Resize`→`scales` graph surgery). An `OrtInference` could replace
//! it behind this same trait if the palm-ROI fidelity gate ever demands it (see
//! the design spec's *Spike results*); the rest of the pipeline never names a
//! concrete runtime.
//!
//! Pre/post-processing (anchor decode, NMS, ROI affine) lives in the sibling
//! `palm`/`landmark` modules, not here, so this trait stays runtime-agnostic:
//! it runs one model stage on one pre-shaped input tensor and returns the raw
//! output tensors.
//!
//! Foundation module: the production caller (the worker pipeline) lands in a
//! later phase, so the public items here are exercised by tests for now.
#![allow(dead_code)]

use thiserror::Error;
use tract_onnx::prelude::*;

/// Error from loading or running an inference model.
#[derive(Debug, Error)]
pub enum InferenceError {
    /// The model failed to parse, type-check, or optimize.
    #[error("model load failed: {0}")]
    Load(String),
    /// A forward pass failed.
    #[error("inference run failed: {0}")]
    Run(String),
    /// The input tensor's element count did not match its declared shape.
    #[error("input shape {shape:?} does not match {len} elements")]
    ShapeMismatch {
        /// The declared shape.
        shape: Vec<usize>,
        /// The actual element count.
        len: usize,
    },
}

/// A dense row-major `f32` tensor plus its shape.
#[derive(Debug, Clone, PartialEq)]
pub struct Tensor {
    /// Row-major elements; `data.len()` must equal the product of `shape`.
    pub data: Vec<f32>,
    /// Tensor dimensions.
    pub shape: Vec<usize>,
}

impl Tensor {
    /// Build a tensor, validating that `data` matches `shape`.
    ///
    /// # Errors
    /// Returns [`InferenceError::ShapeMismatch`] if `data.len()` is not the
    /// product of `shape`.
    pub fn new(data: Vec<f32>, shape: Vec<usize>) -> Result<Self, InferenceError> {
        let expected: usize = shape.iter().product();
        if expected != data.len() {
            return Err(InferenceError::ShapeMismatch {
                shape,
                len: data.len(),
            });
        }
        Ok(Self { data, shape })
    }

    /// Zero-filled tensor of the given shape.
    #[must_use]
    pub fn zeros(shape: Vec<usize>) -> Self {
        let len = shape.iter().product();
        Self {
            data: vec![0.0; len],
            shape,
        }
    }
}

/// Runs one ONNX model stage.
pub trait HandInference: Send {
    /// Run the model on `input`, returning the raw output tensors in the model's
    /// output order.
    ///
    /// # Errors
    /// Returns [`InferenceError::Run`] if the forward pass fails.
    fn run(&mut self, input: &Tensor) -> Result<Vec<Tensor>, InferenceError>;
}

/// `tract`-backed inference for a single model with a fixed input shape.
pub struct TractInference {
    plan: TypedSimplePlan<TypedModel>,
    input_shape: Vec<usize>,
}

impl TractInference {
    /// Load and optimize an ONNX model from its bytes, fixing the input to
    /// `input_shape` so tract can fully constant-fold shapes.
    ///
    /// # Errors
    /// Returns [`InferenceError::Load`] if the model cannot be parsed,
    /// type-checked, or optimized.
    pub fn load(model_bytes: &[u8], input_shape: &[usize]) -> Result<Self, InferenceError> {
        let load_err = |e: TractError| InferenceError::Load(e.to_string());
        let mut reader = model_bytes;
        let typed = tract_onnx::onnx()
            .model_for_read(&mut reader)
            .map_err(load_err)?
            .with_input_fact(0, f32::fact(input_shape).into())
            .map_err(load_err)?
            .into_optimized()
            .map_err(load_err)?;
        let plan = typed.into_runnable().map_err(load_err)?;
        Ok(Self {
            plan,
            input_shape: input_shape.to_vec(),
        })
    }
}

impl HandInference for TractInference {
    fn run(&mut self, input: &Tensor) -> Result<Vec<Tensor>, InferenceError> {
        if input.shape != self.input_shape {
            return Err(InferenceError::Run(format!(
                "expected input shape {:?}, got {:?}",
                self.input_shape, input.shape
            )));
        }
        // Fully-qualified: tract's `Tensor` is shadowed in this module by our
        // own public `Tensor` type (the trait's I/O type).
        let tensor = tract_onnx::prelude::Tensor::from_shape(&input.shape, &input.data)
            .map_err(|e| InferenceError::Run(e.to_string()))?;
        let outputs = self
            .plan
            .run(tvec!(tensor.into()))
            .map_err(|e| InferenceError::Run(e.to_string()))?;

        outputs
            .into_iter()
            .map(|o| {
                let view = o
                    .to_array_view::<f32>()
                    .map_err(|e| InferenceError::Run(e.to_string()))?;
                Ok(Tensor {
                    shape: view.shape().to_vec(),
                    data: view.iter().copied().collect(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn model_path(name: &str) -> PathBuf {
        // Models are vendored at the workspace root; tests run from the crate
        // manifest dir (crates/wc-core).
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/models/hand")
            .join(name)
    }

    fn load_model(name: &str, input_shape: &[usize]) -> TractInference {
        let bytes = std::fs::read(model_path(name)).expect("read vendored model");
        TractInference::load(&bytes, input_shape).expect("load vendored model")
    }

    #[test]
    fn tensor_new_validates_shape() {
        assert!(Tensor::new(vec![1.0, 2.0], vec![2]).is_ok());
        assert!(matches!(
            Tensor::new(vec![1.0, 2.0], vec![3]),
            Err(InferenceError::ShapeMismatch { .. })
        ));
    }

    #[test]
    fn palm_model_runs_and_emits_raw_box_and_score_tensors() {
        // The graph-surgeried palm detector: input [1,192,192,3] → raw
        // [1,2016,18] boxes + [1,2016,1] scores (anchor decode + NMS are done
        // in Rust, not in the graph). Proves tract runs it in-crate.
        let mut model = load_model("palm_detection.onnx", &[1, 192, 192, 3]);
        let out = model
            .run(&Tensor::zeros(vec![1, 192, 192, 3]))
            .expect("palm forward pass");
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
    fn landmark_model_runs_and_emits_landmarks_handedness_world() {
        // Landmark model: input [1,224,224,3] → [1,63] image landmarks,
        // [1,1] presence, [1,1] handedness, [1,63] world landmarks.
        let mut model = load_model("hand_landmark.onnx", &[1, 224, 224, 3]);
        let out = model
            .run(&Tensor::zeros(vec![1, 224, 224, 3]))
            .expect("landmark forward pass");
        let shapes: Vec<&[usize]> = out.iter().map(|t| t.shape.as_slice()).collect();
        assert_eq!(out.len(), 4, "shapes={shapes:?}");
        // Two 63-element landmark tensors (image + world) and two scalars.
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

    #[test]
    fn run_rejects_wrong_input_shape() {
        let mut model = load_model("hand_landmark.onnx", &[1, 224, 224, 3]);
        let err = model
            .run(&Tensor::zeros(vec![1, 192, 192, 3]))
            .expect_err("shape mismatch should error");
        assert!(matches!(err, InferenceError::Run(_)));
    }
}
