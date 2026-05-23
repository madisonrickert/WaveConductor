//! Dev panel state.
//!
//! Toggled by [`crate::lifecycle::actions::WaveConductorAction::ToggleDevPanel`]
//! (bound to Shift+D in Plan 2). The actual `bevy-inspector-egui` integration
//! is wired in [`super::SettingsPlugin`]; this module owns only the boolean
//! state resource and its toggle system so the rest of the codebase can
//! depend on `DevPanelVisible` without dragging in egui.

use bevy::prelude::*;
use leafwing_input_manager::prelude::ActionState;

use crate::lifecycle::actions::WaveConductorAction;

/// True when the dev inspector window should be drawn.
///
/// Defaults to `false` — production deployments and casual users never see
/// the panel. The Plan-5 binding (Shift+D) flips it.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevPanelVisible(pub bool);

/// System that listens for `WaveConductorAction::ToggleDevPanel` and flips
/// [`DevPanelVisible`]. Scheduled in `Update` by `SettingsPlugin`.
pub fn handle_dev_panel_toggle(
    actions: Res<'_, ActionState<WaveConductorAction>>,
    mut visible: ResMut<'_, DevPanelVisible>,
) {
    if actions.just_pressed(&WaveConductorAction::ToggleDevPanel) {
        visible.0 = !visible.0;
        tracing::debug!(visible = visible.0, "dev panel toggled");
    }
}

/// Plugin assembly hook called by [`super::SettingsPlugin::build`]. Empty in
/// Phase A; real implementation lands in Task 15.
pub(super) fn add_systems(_app: &mut bevy::prelude::App) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_app() -> App {
        // Deliberately omit `InputManagerPlugin` so `update_action_state` does not
        // overwrite direct `ActionState::press()` calls each frame. We are testing
        // only that `handle_dev_panel_toggle` reacts to a `just_pressed` state,
        // not the full physical-input pipeline (which is covered by
        // `tests/settings_plugin.rs`).
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<ActionState<WaveConductorAction>>();
        app.init_resource::<DevPanelVisible>();
        app.add_systems(Update, handle_dev_panel_toggle);
        app
    }

    #[test]
    fn toggle_flips_visibility() {
        let mut app = make_app();

        // Press toggles on. Without `InputManagerPlugin`, `JustPressed` is not
        // advanced to `Pressed` by a tick system, so `just_pressed()` returns true
        // for every frame until `release()` is called.
        app.world_mut()
            .resource_mut::<ActionState<WaveConductorAction>>()
            .press(&WaveConductorAction::ToggleDevPanel);
        app.update();
        assert!(
            app.world().resource::<DevPanelVisible>().0,
            "first press should make panel visible"
        );

        // Release clears the just-pressed state. Then press again toggles off.
        app.world_mut()
            .resource_mut::<ActionState<WaveConductorAction>>()
            .release(&WaveConductorAction::ToggleDevPanel);
        app.update();
        app.world_mut()
            .resource_mut::<ActionState<WaveConductorAction>>()
            .press(&WaveConductorAction::ToggleDevPanel);
        app.update();
        assert!(
            !app.world().resource::<DevPanelVisible>().0,
            "second press should hide panel"
        );
    }
}
