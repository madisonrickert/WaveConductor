//! Flame sketch: a name-seeded IFS fractal flame, evaluated level-parallel on
//! the GPU and drawn as an additive point cloud with a fake depth of field.
//!
//! Modules are added stage by stage (see the 2026-07-02 flame port plan).

pub mod branches;
pub mod levels;
