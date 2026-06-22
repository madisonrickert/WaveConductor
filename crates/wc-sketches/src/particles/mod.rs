//! Shared GPU particle engine: GPU/CPU simulation, rendering material, and a
//! CPU reference integrator. This module was extracted from Line's original
//! `line/particles` implementation during Dots Plan D1 to give Dots (and future
//! sketches) the same particle foundation without duplication.
//!
//! # Submodules
//!
//! - [`compute`] — Bevy render-world plugin that dispatches the WGSL compute
//!   kernel each frame, manages the `SimParams` uniform buffer, and owns the
//!   GPU-side bind group.
//! - [`particle`] — POD types shared between Rust and the GPU: [`particle::Particle`],
//!   [`particle::Attractor`], [`particle::SimParams`], and [`particle::MAX_ATTRACTORS`].
//! - [`material`] — [`material::ParticleMaterial`] specialization that reads the
//!   shared particle buffer and renders billboarded star sprites.
//! - [`sim_cpu`] — CPU-mirror integrator running the same math as the WGSL kernel;
//!   used by Line's audio coupling and as a reference fixture for tests.

pub mod compute;
pub mod material;
pub mod particle;
pub mod sim_cpu;

pub use material::ParticleMaterial;
pub use particle::{Attractor, Particle, SimParams, MAX_ATTRACTORS};
