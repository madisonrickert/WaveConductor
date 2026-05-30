//! Global hand-tracking settings, persisted across sessions.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// Hand-tracking-wide settings (not per-sketch).
///
/// `leap_background`: should the Leap provider request the
/// `BackgroundFrames` policy at start? When `true`, tracking frames keep
/// arriving even when the `WaveConductor` window is not focused. Default
/// `false` per v4.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "hand_tracking")]
pub struct HandTrackingSettings {
    /// Whether the Leap provider should request the `BackgroundFrames` policy
    /// at start. When `true`, tracking frames keep arriving even when the
    /// `WaveConductor` window is not focused. Default `false` per v4.
    #[setting(
        default = false,
        ty = Boolean,
        category = User,
        section = "Hand Tracking",
        label = "Receive Leap frames when window is not focused"
    )]
    #[serde(default)]
    pub leap_background: bool,
}
