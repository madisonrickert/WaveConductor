//! ONNX inference behind a runtime-agnostic trait.
//!
//! Defines the shared types used by all inference backends: [`Tensor`] (a dense
//! row-major `f32` buffer with a shape), [`InferenceError`], and [`HandInference`]
//! (run one ONNX model stage, input tensor → raw output tensors). The concrete
//! implementation is [`super::inference_ort::OrtInference`] (`ort`/ONNX Runtime
//! with `CoreML` acceleration on macOS).
//!
//! Pre/post-processing (anchor decode, NMS, ROI affine) lives in the sibling
//! `palm`/`landmark` modules, not here, so this trait stays runtime-agnostic:
//! it runs one model stage on one pre-shaped input tensor and returns the raw
//! output tensors.
//!
//! [`Tensor::new`], [`Tensor::zeros`], and [`InferenceError::ShapeMismatch`]
//! are testing/construction helpers: part of this module's API surface and
//! used by tests across the pipeline, but never called on the production hot
//! path — hence the per-item `#[allow(dead_code)]` each carries.

use thiserror::Error;

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
    #[allow(
        dead_code,
        reason = "constructed only by the test/construction helper Tensor::new"
    )]
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
    #[allow(
        dead_code,
        reason = "test/construction helper; off the production hot path"
    )]
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
    #[allow(
        dead_code,
        reason = "test/construction helper; off the production hot path"
    )]
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;

    #[test]
    fn tensor_new_validates_shape() {
        assert!(Tensor::new(vec![1.0, 2.0], vec![2]).is_ok());
        assert!(matches!(
            Tensor::new(vec![1.0, 2.0], vec![3]),
            Err(InferenceError::ShapeMismatch { .. })
        ));
    }
}
