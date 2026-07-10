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
//!
//! The [`lifecycle`] submodule collects the per-sketch glue that used to be
//! copy-pasted verbatim across sketches — the `requires_restart` reload
//! listener, the camera render-profile applier/reset, and the shared restart
//! debounce window — as generic systems parameterised over each sketch's
//! settings type via the [`lifecycle::SketchLifecycle`] trait.

pub mod cleanup;
pub mod lifecycle;
pub mod manifest;
pub mod scheduling;

pub use cleanup::despawn_with;
pub use lifecycle::{
    apply_render_profile, reload_on_resize_settled, reset_render_profile,
    restart_on_settings_change, RenderProfile, SketchLifecycle, RESTART_DEBOUNCE,
};
pub use manifest::{
    register_sketch_tile, RegisterSketchManifestExt, SketchManifest, SketchManifestEntry,
};
pub use scheduling::{in_idle, sketch_active};
