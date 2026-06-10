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
//! for the translucent frosted-glass look. A single `ScrollArea` wraps the whole
//! panel body — diagnostics, tuning, and the world inspector — with each section
//! in a collapsible header, so nothing falls off the bottom on shorter displays.
//! Shift+D is the only toggle — there is no click-outside dismiss for this
//! developer tool. While a panel field has keyboard focus, Shift+D is
//! suppressed like every other app hotkey (the
//! [`crate::settings::input_capture::egui_not_capturing_keyboard`] gate, so a
//! capital D types into the field instead of dismissing the panel under it);
//! press Esc or click off the field first, allowing one frame of lag for the
//! focus mirror to catch up.

use bevy::prelude::*;
use leafwing_input_manager::prelude::ActionState;

use crate::lifecycle::actions::WaveConductorAction;
use crate::ui::auto_fade::UiOpacity;
use crate::ui::{backdrop_blur_frame, FrameOptions, OverlayStyle};

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
/// translucent frosted-glass look. A single `ScrollArea` wraps the whole panel
/// body and each section sits in a collapsible header, so the panel stays
/// on-screen when its content exceeds the window height.
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
        let mut q =
            world.query_filtered::<&bevy::window::Window, With<bevy::window::PrimaryWindow>>();
        q.single(world).map_or(720.0, bevy::window::Window::height)
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

    // Snapshot the provider registry before entering the egui closure so that
    // `world` is free for `ui_for_world`'s exclusive `&mut World` borrow later.
    // `Option<(ProviderId, ProviderStatus, ProviderDiagnostics)>` is `Clone` and
    // cheap to copy for the diagnostic strings that live on the stack.
    let registry_snapshot: Option<(
        Option<crate::input::provider::ProviderId>,
        crate::input::state::ProviderStatus,
        crate::input::state::ProviderDiagnostics,
    )> = world
        .get_resource::<crate::input::provider::ProviderRegistry>()
        .map(|r| (r.primary_id(), r.primary_status(), r.primary_diagnostics()));

    // Live grab/pinch readout (drives the tuning calibration hint) and a working
    // copy of the tunable settings — snapshotted before the egui closure borrows
    // `world`. Slider edits land in `tuning` and are committed back after `show`.
    let hand_readout: Option<(usize, f32, f32)> = world
        .get_resource::<crate::input::state::HandTrackingState>()
        .map(|s| {
            let first = s.iter().next();
            (
                s.active_hand_count(),
                first.map_or(0.0, |h| h.grab_strength),
                first.map_or(0.0, |h| h.pinch_strength),
            )
        });
    let mut tuning: Option<crate::settings::HandTrackingSettings> = world
        .get_resource::<crate::settings::HandTrackingSettings>()
        .cloned();

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
                    ui.label(
                        bevy_egui::egui::RichText::new("DEV INSPECTOR")
                            .color(style.text_color_dim)
                            .size(13.0),
                    );
                    ui.separator();

                    // One outer scroll area wraps every section so none can fall
                    // off the bottom of the screen — the curated diagnostics grid
                    // grew long enough to overflow on shorter displays. Sections
                    // are collapsible so the long grid can be folded away, and the
                    // world inspector no longer needs its own (nested) scroll.
                    // `auto_shrink([false, true])`: keep a stable width, but be only
                    // as tall as the content up to `max_height`, then scroll.
                    bevy_egui::egui::ScrollArea::vertical()
                        .auto_shrink([false, true])
                        .max_height((window_height - 140.0).max(120.0))
                        .show(ui, |ui| {
                            // Hand Tracking section — curated diagnostics from the
                            // multi-axis ProviderStatus + ProviderDiagnostics.
                            // Snapshotted into locals before the closure so `world`
                            // is free for `ui_for_world`'s `&mut World` borrow below.
                            if let Some((primary_id, status, diag)) = &registry_snapshot {
                                bevy_egui::egui::CollapsingHeader::new("Hand tracking")
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        draw_hand_tracking_section(ui, *primary_id, status, diag);
                                    });
                            }

                            // Live MediaPipe feel tuning — readout + sliders bound to
                            // the persisted settings (edits committed after the closure).
                            if let Some(t) = tuning.as_mut() {
                                bevy_egui::egui::CollapsingHeader::new("Hand tuning (MediaPipe)")
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        draw_hand_tuning_controls(ui, &style, t, hand_readout);
                                    });
                            }

                            bevy_egui::egui::CollapsingHeader::new("World inspector")
                                .default_open(true)
                                .show(ui, |ui| {
                                    bevy_inspector_egui::bevy_inspector::ui_for_world(world, ui);
                                });
                        });
                },
            );
        });

    // Commit slider edits back to the resource, but only when a value actually
    // moved — so `Changed<HandTrackingSettings>` (autosave + the apply-to-provider
    // system) fires on real edits, not every frame the panel is open.
    if let Some(edited) = tuning {
        if let Some(mut res) = world.get_resource_mut::<crate::settings::HandTrackingSettings>() {
            if *res != edited {
                *res = edited;
            }
        }
    }
}

/// Renders the live `MediaPipe` hand-tuning controls inside the dev panel: a
/// grab/pinch readout (so the operator can see the open-hand grab *floor*) plus
/// sliders bound to the persisted [`crate::settings::HandTrackingSettings`]
/// tunables. The caller commits edits back to the resource; the
/// `apply_mediapipe_tuning_settings` system forwards them to the live provider,
/// so feel changes apply with no restart.
fn draw_hand_tuning_controls(
    ui: &mut bevy_egui::egui::Ui,
    style: &OverlayStyle,
    settings: &mut crate::settings::HandTrackingSettings,
    readout: Option<(usize, f32, f32)>,
) {
    use bevy_egui::egui;

    if let Some((count, grab, pinch)) = readout {
        ui.label(format!(
            "Live:  {count} hand(s)  ·  grab {grab:.2}  ·  pinch {pinch:.2}"
        ));
        // Calibration now reads the PRE-deadzone signal directly ("Grab raw
        // (‰)" in the Hand tracking grid), so there is no need to zero the
        // deadzone first — the raw readout is unaffected by the slider.
        ui.label(
            egui::RichText::new(
                "Open-hand calibration: hold your hand open and relaxed, read the rest \
                 floor from \"Grab raw (‰)\" above, then set the deadzone just above it \
                 (slider value = ‰ ÷ 1000, e.g. raw 60‰ → deadzone 0.07).",
            )
            .size(10.0)
            .color(style.text_color_dim),
        );
        ui.add_space(2.0);
    }
    ui.add(
        egui::Slider::new(&mut settings.grab_rest_deadzone, 0.0..=0.6).text("Grab rest deadzone"),
    );
    // Size-estimated depth calibration. The "0 = off" in the label keeps the
    // escape hatch discoverable mid-set: dragging to 0 disables the estimator
    // and restores the fixed 120 mm depth pin (grab-only attractor control).
    ui.add(
        egui::Slider::new(&mut settings.depth_calibration_k, 0.0..=1.5)
            .text("Depth calibration k (0 = off)"),
    );
    ui.add(
        egui::Slider::new(&mut settings.smoothing_min_cutoff, 0.1..=20.0)
            .text("Smoothing min cutoff (Hz)"),
    );
    ui.add(egui::Slider::new(&mut settings.smoothing_beta, 0.0..=10.0).text("Smoothing beta"));
}

/// Renders the curated "HAND TRACKING" diagnostic grid inside the dev panel.
///
/// Called only when `ProviderRegistry` exists in the world. Surfaces every
/// axis of [`crate::input::state::ProviderStatus`] plus the string fields
/// from [`crate::input::state::ProviderDiagnostics`] so developers can
/// diagnose connection, device, and streaming issues without opening a
/// separate tool.
///
/// `primary_id` is `None` when the registry exists but has no providers
/// registered yet (e.g., early in a test harness setup).
fn draw_hand_tracking_section(
    ui: &mut bevy_egui::egui::Ui,
    primary_id: Option<crate::input::provider::ProviderId>,
    status: &crate::input::state::ProviderStatus,
    diag: &crate::input::state::ProviderDiagnostics,
) {
    use crate::input::state::{DevicePresence, ServiceConnection, TrackingFlow};

    bevy_egui::egui::Grid::new("hand_tracking_diag")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label("Provider:");
            ui.label(primary_id.map_or("(none)", |id| id.label()));
            ui.end_row();

            ui.label("Service:");
            let s = match status.service {
                ServiceConnection::NotStarted => "Not started",
                ServiceConnection::Connecting => "Connecting",
                ServiceConnection::Connected => "Connected",
                ServiceConnection::ServiceMissing => "Service not running",
                ServiceConnection::Disconnected => "Disconnected",
                ServiceConnection::Errored => "Errored",
            };
            ui.label(s);
            ui.end_row();

            ui.label("Device:");
            let d = match status.device {
                DevicePresence::NoDevice => "No device",
                DevicePresence::Attached => "Attached",
                DevicePresence::Lost => "Lost",
                DevicePresence::Failed => "Failed",
            };
            if matches!(status.device, DevicePresence::Attached) {
                if let Some(serial) = diag.device_serial.as_deref() {
                    ui.label(format!("{d} ({serial})"));
                } else {
                    ui.label(d);
                }
            } else {
                ui.label(d);
            }
            ui.end_row();

            ui.label("Health:");
            if status.health.is_empty() {
                ui.label("(none)");
            } else {
                ui.label(format!("{:?}", status.health));
            }
            ui.end_row();

            ui.label("Streaming:");
            match status.streaming {
                TrackingFlow::NotStreaming => {
                    ui.label("Not streaming");
                }
                TrackingFlow::Streaming {
                    last_frame_ago,
                    dropped_since_start,
                } => {
                    ui.label(format!(
                        "Streaming  ·  last frame {} ms ago  ·  {} dropped",
                        last_frame_ago.as_millis(),
                        dropped_since_start
                    ));
                }
            }
            ui.end_row();

            ui.label("Service health:");
            if status.service_health.is_empty() {
                ui.label("(none)");
            } else {
                ui.label(format!("{:?}", status.service_health));
            }
            ui.end_row();

            ui.label("SDK version:");
            ui.label(diag.sdk_version.as_deref().unwrap_or("(unknown)"));
            ui.end_row();

            ui.label("Active policies:");
            if diag.active_policies.is_empty() {
                ui.label("(none)");
            } else {
                ui.label(diag.active_policies.join(", "));
            }
            ui.end_row();

            if let Some(err) = diag.last_error.as_deref() {
                ui.label("Last error:");
                ui.label(err);
                ui.end_row();
            }

            for metric in &diag.metrics {
                draw_provider_metric_row(ui, metric);
            }
        });
}

/// Render one provider-specific diagnostic metric row.
fn draw_provider_metric_row(
    ui: &mut bevy_egui::egui::Ui,
    metric: &crate::input::state::ProviderMetric,
) {
    use crate::input::state::ProviderMetricValue;

    ui.label(metric.label);
    match metric.value {
        ProviderMetricValue::Duration(value) => {
            ui.label(format!("{} ms", value.as_millis()));
        }
        ProviderMetricValue::Count(value) => {
            ui.label(value.to_string());
        }
        ProviderMetricValue::Text(value) => {
            ui.label(value);
        }
    }
    ui.end_row();
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
