//! Dots sketch main-world systems.
//!
//! Split into focused submodules:
//!
//! - [`spawn`] — `OnEnter(AppState::Dots)` grid spawn + [`DotsRoot`] marker.
//! - [`sim_params`] — Per-frame writer for
//!   [`crate::particles::compute::ParticleSimParams`].
//! - [`mouse`] — Pointer/touch attractor state and decay systems.
//! - [`post_params`] — Per-frame writer for
//!   [`crate::dots::post_process::DotsPostParams`]: resolution, gamma,
//!   `shrink_factor`, and the explode hue-split centre (`i_mouse`) driven by
//!   the cursor or, when a hand is grabbing, an eased [`post_params::DotsExplodeFocal`].

pub mod mouse;
pub mod post_params;
pub mod sim_params;
pub mod spawn;

pub use mouse::{decay_dots_mouse_attractor, update_dots_mouse_attractor, DotsMouseAttractorState};
pub use post_params::{update_dots_post_params, DotsExplodeFocal};
pub use sim_params::update_dots_sim_params;
pub use spawn::{spawn_dots, DotsRoot};
