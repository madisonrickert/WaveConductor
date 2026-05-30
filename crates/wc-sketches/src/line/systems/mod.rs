//! Line sketch main-world update systems.
//!
//! Split into focused submodules so each file stays under the ~300-line ceiling
//! enforced by `AGENTS.md` as the Phase C work extends per-particle state:
//!
//! - [`spawn`] — `OnEnter(AppState::Line)` spawn system plus the [`LineRoot`]
//!   marker component. Allocates the particle storage buffer, builds the quad
//!   mesh, installs [`crate::line::compute::LineSimParams`] for the render
//!   world, and seeds the CPU mirror.
//! - [`mouse`] — Pointer-driven attractor lifecycle. Tracks button transitions,
//!   updates [`mouse::MouseAttractorState`], and decays the attractor's power
//!   each frame so the pull fades smoothly after release.
//! - [`sim_params`] — Per-frame writer for [`crate::line::compute::LineSimParams`].
//!   Bakes the v4-parity drag constants, derives the size-scaled gravity
//!   multiplier from the window width, and publishes the attractor array
//!   alongside the constrain-to-box bounds. The param-baking core is factored
//!   into the shared `bake_sim_params` / `bake_post_base` fns (Plan 12 Condition
//!   A1) so the live writer here and the screensaver's phantom-hand writer
//!   (`crate::line::screensaver`) bake identically and cannot drift.

pub mod mouse;
pub mod sim_params;
pub mod spawn;

pub use mouse::{
    decay_mouse_attractor, update_mouse_attractor, MouseAttractorState, MOUSE_POWER_DECAY,
    MOUSE_POWER_FLOOR, MOUSE_POWER_PRESS,
};
pub use sim_params::{
    bake_post_base, bake_sim_params, update_sim_params, WindowGeom, V4_FADE_DURATION, V4_FIXED_DT,
    V4_INERTIAL_DRAG_CONSTANT, V4_PULLING_DRAG_CONSTANT,
};
pub use spawn::{spawn_line, LineRoot};
