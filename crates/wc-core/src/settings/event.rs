//! The [`SketchRestart`] message.
//!
//! Fires when a setting with `requires_restart = true` changes value. The
//! sketch's plugin observes the message and re-runs its `OnEnter` setup
//! sequence so any size-dependent or one-time resources (particle counts,
//! VRAM buffers, etc.) are rebuilt against the new value.

use bevy::prelude::*;

/// Fired by `SettingsPlugin` when a `requires_restart` field changes.
///
/// Carries the [`crate::settings::SketchSettings::STORAGE_KEY`] of the
/// struct whose field triggered the restart so listeners can ignore
/// restarts targeting other sketches.
#[derive(Message, Debug, Clone)]
pub struct SketchRestart {
    /// Storage key of the settings struct that requested the restart.
    pub storage_key: &'static str,
}
