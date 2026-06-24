//! Cymatics sketch — a wave-simulation visualiser driven by cymatic patterns.
//!
//! Stage 2 (compute) opens here: the GPU POD types, ping-pong textures, and
//! extract resource are in [`compute`]. The full sketch plugin (settings, audio,
//! lifecycle) arrives in Stage 4.

pub mod compute;
pub mod render;

use bevy::prelude::*;

/// Marker component placed on every entity owned by the Cymatics sketch.
///
/// `OnExit(AppState::Cymatics)` despawns everything tagged with this marker
/// via [`wc_core::sketch::despawn_with`].
#[derive(Component)]
pub struct CymaticsRoot;
