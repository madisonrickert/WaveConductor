//! Sketch infrastructure.
//!
//! Cross-sketch helpers consumed by every sketch crate. The pattern is:
//!
//! 1. Each sketch defines a `*Root` marker component (e.g., `LineRoot`).
//! 2. The sketch's plugin spawns its entities under that marker on
//!    `OnEnter(AppState::X)`.
//! 3. The sketch's plugin schedules [`cleanup::despawn_with::<*Root>`] on
//!    `OnExit(AppState::X)` to free everything.
//! 4. Update systems are gated with `.run_if(sketch_active(AppState::X))`
//!    so they only run when the sketch is foregrounded AND
//!    `SketchActivity::Active`.

pub mod cleanup;
pub mod scheduling;

pub use cleanup::despawn_with;
pub use scheduling::sketch_active;
