//! Settings subsystem: typed per-sketch settings, persistence, restart
//! events, user panel, and dev inspector.
//!
//! ## Data flow
//!
//! 1. A sketch crate defines `MySettings` with `#[derive(SketchSettings)]`.
//!    The macro emits `Default` and the [`SketchSettings`] impl.
//! 2. The sketch's plugin calls
//!    `app.register_sketch_settings::<MySettings>()`. This loads any
//!    persisted value (or `Default`), inserts it as a Bevy `Resource`, and
//!    appends an entry to [`SettingsRegistry`].
//! 3. Each frame, [`registry::emit_restart_events`] diffs every registered
//!    resource against its [`registry::PreviousSnapshot`]; any change to a
//!    `requires_restart` field writes a [`event::SketchRestart`] message.
//! 4. The user panel (`panel_user`, private) iterates the registry and draws
//!    only `category = User` fields. The dev panel ([`panel_dev`]) opens a
//!    `bevy-inspector-egui` window when [`panel_dev::DevPanelVisible`] is
//!    true, exposing every Reflect-registered resource (including the
//!    sketch settings types, which `register_sketch_settings` registers
//!    automatically).
//! 5. (Phase B) A debounced save system will write the current resource back
//!    to disk shortly after the last mutation. Phase A does not wire this
//!    automatically — call [`persistence::save::<S>`] manually when you need
//!    to persist a change.

pub mod def;
pub mod event;
pub mod panel_dev;
pub mod persistence;
pub mod registry;
pub mod test_settings;
pub mod trait_def;

mod panel_user;

pub use def::{NumberRange, SettingDef, SettingKind, SettingsCategory};
pub use event::SketchRestart;
pub use panel_dev::DevPanelVisible;
pub use registry::{RegisterSketchSettingsExt, SettingsRegistry};
pub use trait_def::SketchSettings;

use bevy::prelude::*;

/// Plugin that wires the settings subsystem into a Bevy [`App`].
///
/// Registered by [`crate::CorePlugin`]. Sketches register their concrete
/// settings types separately via
/// [`registry::RegisterSketchSettingsExt::register_sketch_settings`].
pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SettingsRegistry>()
            .init_resource::<DevPanelVisible>()
            .add_message::<SketchRestart>()
            // Always-on registration of the synthetic test settings so the
            // panels have at least one struct to render even before any
            // sketches exist. Sketches will register their own structs
            // additionally in Plan 6+.
            .register_sketch_settings::<test_settings::TestSketchSettings>()
            .add_systems(
                Update,
                (
                    panel_dev::handle_dev_panel_toggle,
                    registry::emit_restart_events,
                )
                    .chain(),
            );
        // TODO Phase B: wire save_fn as a debounced system so mutations are
        // persisted automatically without callers invoking save::<S> manually.
        // egui-based UI systems are wired below.
        panel_user::add_systems(app);
        panel_dev::add_systems(app);
    }
}
