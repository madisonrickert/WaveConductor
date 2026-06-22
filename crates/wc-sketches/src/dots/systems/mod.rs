//! Dots sketch main-world systems.
//!
//! Split into focused submodules:
//!
//! - [`spawn`] — `OnEnter(AppState::Dots)` grid spawn + [`DotsRoot`] marker.
//! - [`sim_params`] — Per-frame writer for
//!   [`crate::particles::compute::ParticleSimParams`].
//! - [`mouse`] — Pointer/touch attractor state and decay systems.

pub mod mouse;
pub mod sim_params;
pub mod spawn;

pub use mouse::{decay_dots_mouse_attractor, update_dots_mouse_attractor, DotsMouseAttractorState};
pub use sim_params::update_dots_sim_params;
pub use spawn::{spawn_dots, DotsRoot};
