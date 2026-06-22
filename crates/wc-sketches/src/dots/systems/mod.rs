//! Dots sketch main-world systems.
//!
//! Split into focused submodules:
//!
//! - [`spawn`] — `OnEnter(AppState::Dots)` grid spawn + [`DotsRoot`] marker.
//! - [`sim_params`] — Per-frame writer for
//!   [`crate::particles::compute::ParticleSimParams`].

pub mod sim_params;
pub mod spawn;

pub use sim_params::update_dots_sim_params;
pub use spawn::{spawn_dots, DotsRoot};
