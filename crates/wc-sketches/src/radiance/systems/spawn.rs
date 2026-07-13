//! `OnEnter(AppState::Radiance)` spawn plus the `OnExit` teardown.
//! (Populated in Task 9; the marker lands first so the material driver
//! compiles.)

use bevy::prelude::*;

/// Marker component on every entity owned by the Radiance sketch;
/// `OnExit(AppState::Radiance)` despawns everything tagged with it.
#[derive(Component)]
pub struct RadianceRoot;
