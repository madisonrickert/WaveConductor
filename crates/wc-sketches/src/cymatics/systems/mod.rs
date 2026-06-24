//! Cymatics sketch main-world systems.
//!
//! Split into focused submodules:
//!
//! - [`interaction`] — Two-centre interaction state machine: `step_centers`
//!   (verbatim v4 `step()` port), `screen_to_sim_uv`, and the Bevy system that
//!   feeds them pointer and hand-grab input.
//! - [`hand`] — [`hand::CymaticsHandGrabs`] resource + [`hand::update_cymatics_hand_centers`]
//!   system: maps up to two grabbing hands to the two wave-centre UV positions.
//! - [`audio_coupling`] — Wave-field sonification stub; implemented by Task C11.

pub mod audio_coupling;
pub mod hand;
pub mod interaction;

pub use hand::{update_cymatics_hand_centers, CymaticsHandGrabs};
pub use interaction::{update_cymatics_centers, CenterInput, CenterTuning};
