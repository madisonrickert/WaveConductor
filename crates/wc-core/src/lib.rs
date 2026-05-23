//! # wc-core
//!
//! Shared infrastructure for `WaveConductor` v5: lifecycle, input, audio,
//! settings, and math helpers. Sketches consume this crate via [`CorePlugin`];
//! the binary crate registers `CorePlugin` once at app startup.

pub mod input;
pub mod lifecycle;

use bevy::prelude::*;

/// Single plugin that bundles every wc-core subsystem.
///
/// Registered once by the binary crate. As subsystems land in later plans,
/// they are added as sub-plugins inside this `build()` method.
pub struct CorePlugin;

impl Plugin for CorePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(lifecycle::LifecyclePlugin);
        app.add_plugins(input::HandTrackingPlugin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::state::app::StatesPlugin;

    #[test]
    fn core_plugin_builds_without_panicking() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::input::InputPlugin);
        app.add_plugins(StatesPlugin);
        app.add_plugins(CorePlugin);
    }
}
