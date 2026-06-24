//! Cymatics hand-grab state for the two wave centres.
//!
//! This stub provides the [`CymaticsHandGrabs`] resource consumed by
//! [`super::interaction::update_cymatics_centers`]. Task C10 wires the
//! Leap/MediaPipe gesture detection that populates `c1` and `c2`; until then
//! both slots stay `None` and the interaction system drives both centres
//! from mouse/touch input only.

use bevy::math::Vec2;
use bevy::prelude::Resource;

/// Hand-grip positions for the two Cymatics wave centres.
///
/// `None` = that centre is not held by a hand this frame. Task C10 fills
/// these from the hand-tracking gesture recogniser; the interaction system
/// (Task C9) reads them to decide whether a centre follows the mouse (free)
/// or a hand (held).
///
/// Coordinates are sim UV `[0, 1]`, top-left origin (Bevy-native).
#[derive(Resource, Default, Clone, Copy)]
pub struct CymaticsHandGrabs {
    /// Primary wave-centre grab position, or `None` if not held.
    pub c1: Option<Vec2>,
    /// Secondary wave-centre grab position, or `None` if not held.
    pub c2: Option<Vec2>,
}
