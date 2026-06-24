//! Cymatics sketch main-world systems.
//!
//! Split into focused submodules:
//!
//! - [`interaction`] — Two-centre interaction state machine: `step_centers`
//!   (verbatim v4 `step()` port), `screen_to_sim_uv`, and the Bevy system that
//!   feeds them pointer and hand-grab input.
//! - [`hand`] — [`hand::CymaticsHandGrabs`] resource stub; populated by Task C10.
//! - [`audio_coupling`] — Wave-field sonification stub; implemented by Task C11.

pub mod audio_coupling;
pub mod hand;
pub mod interaction;

pub use hand::CymaticsHandGrabs;
pub use interaction::{update_cymatics_centers, CenterInput};
