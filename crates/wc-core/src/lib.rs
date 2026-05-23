//! # wc-core
//!
//! Shared infrastructure for `WaveConductor` v5: lifecycle, audio, input, settings,
//! and math helpers. Sketches consume this crate via [`CorePlugin`]; the binary
//! crate registers `CorePlugin` once at app startup.
//!
//! In Plan 1, `CorePlugin` is an empty placeholder. Subsystems are filled in by
//! Plan 2 (Core Scaffolding) and beyond.

#![warn(missing_docs)]

use bevy::prelude::*;

/// Single plugin that bundles every wc-core subsystem.
///
/// Registered once by the binary crate. As subsystems land in Plan 2 and later,
/// they are added as sub-plugins inside this `build()` method (audio, input,
/// lifecycle, settings, ui).
pub struct CorePlugin;

impl Plugin for CorePlugin {
    fn build(&self, _app: &mut App) {
        // Plan 2 fills this in. Intentionally empty in Plan 1 so the crate
        // compiles and the binary can wire it up end-to-end.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_plugin_builds_without_panicking() {
        let mut app = App::new();
        app.add_plugins(CorePlugin);
        // No assertion beyond "did not panic during plugin construction".
    }
}
