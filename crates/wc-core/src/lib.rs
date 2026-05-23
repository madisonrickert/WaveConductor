//! # wc-core
//!
//! Shared infrastructure for `WaveConductor` v5: lifecycle, audio, input, settings,
//! and math helpers. Sketches consume this crate via [`CorePlugin`]; the binary
//! crate registers `CorePlugin` once at app startup.

pub mod lifecycle;

use bevy::prelude::*;

/// Single plugin that bundles every wc-core subsystem.
///
/// Registered once by the binary crate. As subsystems land in later plans, they
/// are added as sub-plugins inside this `build()` method.
pub struct CorePlugin;

impl Plugin for CorePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(lifecycle::LifecyclePlugin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_plugin_builds_without_panicking() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        // StatesPlugin is required for init_state (called by LifecyclePlugin).
        // MinimalPlugins does not include it; DefaultPlugins does.
        app.add_plugins(bevy::state::app::StatesPlugin);
        app.add_plugins(CorePlugin);
    }
}
