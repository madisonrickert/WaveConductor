//! Dev panel state.
//!
//! Toggled by [`crate::lifecycle::actions::WaveConductorAction::ToggleDevPanel`]
//! (bound to Shift+D in Plan 2). The actual `bevy-inspector-egui` integration
//! is wired in [`super::SettingsPlugin`]; this module owns only the boolean
//! state resource and its toggle system so the rest of the codebase can
//! depend on `DevPanelVisible` without dragging in egui.
//!
//! ## Task 19: v4 chrome
//!
//! The `egui::Window` is replaced by `egui::Area` + [`crate::ui::backdrop_blur_frame`]
//! for the translucent frosted-glass look. A `ScrollArea` wraps the world
//! inspector so it remains usable on shorter displays. Shift+D is the only
//! toggle — there is no click-outside dismiss for this developer tool.

use bevy::prelude::*;
use leafwing_input_manager::prelude::ActionState;

use crate::lifecycle::actions::WaveConductorAction;
use crate::ui::auto_fade::UiOpacity;
use crate::ui::{backdrop_blur_frame, letter_spaced_label, FrameOptions, OverlayStyle};

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

/// Plugin assembly hook called by [`super::SettingsPlugin::build`].
///
/// Adds [`draw_dev_panel`] to `bevy_egui::EguiPrimaryContextPass`, gated by
/// [`DevPanelVisible::0`]. The egui pass schedule is required (not `Update`)
/// because `Window::show` panics with "Called `available_rect()` before
/// `Context::run()`" when invoked outside an active egui pass.
pub(super) fn add_systems(app: &mut App) {
    app.add_systems(
        bevy_egui::EguiPrimaryContextPass,
        draw_dev_panel.run_if(dev_panel_visible),
    );
}

fn dev_panel_visible(visible: Res<'_, DevPanelVisible>) -> bool {
    visible.0
}

/// Exclusive `&mut World` system that draws the world inspector with v4 chrome.
///
/// Uses `egui::Area` for fixed top-left positioning (below where the Home
/// button sits) and wraps content in [`backdrop_blur_frame`] for the
/// translucent frosted-glass look. A `ScrollArea` wraps the world inspector
/// so it remains usable when the inspector content exceeds the window height.
///
/// Only runs when [`DevPanelVisible`] is `true` (gated by the
/// `dev_panel_visible` run condition in [`add_systems`]).
fn draw_dev_panel(world: &mut World) {
    // Guard: EguiPlugin must be initialized. In test harnesses that use
    // MinimalPlugins without EguiPlugin the resource won't exist and
    // SystemState::new would panic when initializing EguiContexts.
    if !world.contains_resource::<bevy_egui::EguiUserTextures>() {
        return;
    }

    let style = *world.resource::<OverlayStyle>();
    let opacity_mul = world.resource::<UiOpacity>().current;
    let window_height = {
        let mut q = world
            .query_filtered::<&bevy::window::Window, With<bevy::window::PrimaryWindow>>();
        q.single(world)
            .map(bevy::window::Window::height)
            .unwrap_or(720.0)
    };

    let mut state: bevy::ecs::system::SystemState<bevy_egui::EguiContexts<'_, '_>> =
        bevy::ecs::system::SystemState::new(world);
    let mut contexts = state.get_mut(world);
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    // `EguiContext` is `Arc<Mutex<…>>` internally, so `.clone()` is a refcount
    // bump. Cloning here lets us release the `EguiContexts` SystemParam borrow
    // before handing `world` to `ui_for_world` inside the `show` closure.
    let ctx = ctx.clone();
    state.apply(world);

    bevy_egui::egui::Area::new(bevy_egui::egui::Id::new("wc-settings-dev-panel"))
        .order(bevy_egui::egui::Order::Foreground)
        .fixed_pos(bevy_egui::egui::pos2(16.0, 60.0))
        .show(&ctx, |ui| {
            ui.set_max_width(480.0);
            ui.set_max_height((window_height - 100.0).max(200.0));
            backdrop_blur_frame(
                ui,
                &style,
                FrameOptions {
                    corner_radius: style.panel_corner_radius,
                    padding: bevy_egui::egui::vec2(20.0, 16.0),
                    opacity_mul,
                },
                |ui| {
                    // Title row: "DEV INSPECTOR" with v4 letter-spacing.
                    // v4 reference: overlayPanel.scss:25 `.overlay-panel-title {
                    //   letter-spacing: 0.04em }` — 13 pt × 0.04 = 0.52 pt gap.
                    letter_spaced_label(
                        ui,
                        "DEV INSPECTOR",
                        bevy_egui::egui::FontId::proportional(13.0),
                        style.text_color_dim,
                        13.0 * 0.04,
                    );
                    ui.separator();
                    bevy_egui::egui::ScrollArea::vertical()
                        .max_height((window_height - 200.0).max(100.0))
                        .show(ui, |ui| {
                            bevy_inspector_egui::bevy_inspector::ui_for_world(world, ui);
                        });
                },
            );
        });
}

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
