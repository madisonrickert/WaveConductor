//! Screensaver overlay shown after sustained idle.
//!
//! In Plan 2 this is a behavioral placeholder: entering the screensaver state
//! logs a message and inserts a marker resource that future systems can read.
//! The actual full-screen overlay UI lands when bevy-egui is integrated in
//! Plan 5 (settings) and uses the same plumbing.

use bevy::prelude::*;

/// Marker resource present iff the screensaver overlay is currently shown.
#[derive(Resource, Default, Debug)]
pub struct ScreensaverActive;

/// `OnEnter(SketchActivity::Screensaver)` handler — inserts the [`ScreensaverActive`] marker resource and logs the transition.
pub fn show(mut commands: Commands<'_, '_>) {
    tracing::info!("screensaver: show");
    commands.insert_resource(ScreensaverActive);
}

/// `OnExit(SketchActivity::Screensaver)` handler — removes the [`ScreensaverActive`] marker resource and logs the transition.
pub fn hide(mut commands: Commands<'_, '_>) {
    tracing::info!("screensaver: hide");
    commands.remove_resource::<ScreensaverActive>();
}
