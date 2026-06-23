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

use crate::lifecycle::action_map::{ActionInput, ActionPhase};
use crate::lifecycle::actions::WaveConductorAction;
use crate::ui::auto_fade::UiOpacity;
use crate::ui::{backdrop_blur_frame, hairline, FrameOptions, OverlayStyle};

/// True when the dev inspector window should be drawn.
///
/// Defaults to `false` — production deployments and casual users never see
/// the panel. The Plan-5 binding (Shift+D) flips it.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevPanelVisible(pub bool);

/// Listens for `ToggleDevPanel` presses and flips [`DevPanelVisible`].
/// Scheduled in `Update` by `SettingsPlugin`.
pub fn handle_dev_panel_toggle(
    mut actions: MessageReader<'_, '_, ActionInput>,
    mut visible: ResMut<'_, DevPanelVisible>,
) {
    let toggled = actions.read().any(|a| {
        a.action == WaveConductorAction::ToggleDevPanel && a.phase == ActionPhase::Pressed
    });
    if toggled {
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
#[allow(clippy::too_many_lines)]
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
    let Ok(mut contexts) = state.get_mut(world) else {
        return;
    };
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

    // Live grab/pinch readout (drives the tuning calibration hint), snapshotted
    // before the egui closure borrows `world`. The tunable sliders themselves
    // moved to the settings dock (Advanced); this panel only reads.
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

    // Snapshot recent log records (cloned out under a brief lock, never held
    // across rendering — see `diagnostics::LogBuffer`). Absent when the binary
    // did not install the capture layer (headless tests). Dev-panel-only, so
    // the per-open-frame allocation is acceptable.
    let mut log_lines: Vec<crate::diagnostics::LogLine> = Vec::new();
    if let Some(buf) = world.get_resource::<crate::diagnostics::LogBuffer>() {
        buf.snapshot_recent(200, &mut log_lines);
    }

    // Smoothed frame-rate readout for the pinned Performance row. `get_resource`
    // (not `resource`) keeps the panel safe under the MinimalPlugins test harness,
    // where DiagnosticsStore is absent. Dev-panel-only; the stack copy is trivial.
    let frame_stats: Option<(f64, f64)> = world
        .get_resource::<bevy::diagnostic::DiagnosticsStore>()
        .map(|store| {
            let fps = store
                .get(&bevy::diagnostic::FrameTimeDiagnosticsPlugin::FPS)
                .and_then(bevy::diagnostic::Diagnostic::smoothed)
                .unwrap_or(0.0);
            let frame_ms = store
                .get(&bevy::diagnostic::FrameTimeDiagnosticsPlugin::FRAME_TIME)
                .and_then(bevy::diagnostic::Diagnostic::smoothed)
                .unwrap_or(0.0);
            (fps, frame_ms)
        });

    // Left-docked, mirroring the settings dock's frame discipline so the two sit
    // side-by-side as matching leaves: same top (y = 60), same bottom inset (16),
    // same side inset (16). Fixed 420 px wide — this is diagnostics-only, narrower
    // than the settings dock — with the artwork visible in the corridor between.
    let dock_height = (window_height - 60.0 - 16.0).max(200.0);
    bevy_egui::egui::Area::new(bevy_egui::egui::Id::new("wc-settings-dev-panel"))
        .order(bevy_egui::egui::Order::Foreground)
        .fixed_pos(bevy_egui::egui::pos2(16.0, 60.0))
        .show(&ctx, |ui| {
            ui.set_min_size(bevy_egui::egui::vec2(DEBUG_DOCK_WIDTH, dock_height));
            ui.set_max_size(bevy_egui::egui::vec2(DEBUG_DOCK_WIDTH, dock_height));
            backdrop_blur_frame(
                ui,
                &style,
                FrameOptions {
                    corner_radius: style.panel_corner_radius,
                    padding: bevy_egui::egui::vec2(20.0, 16.0),
                    opacity_mul,
                },
                |ui| {
                    // Accent drives selection / inspector highlights; the rest of
                    // the overlay keeps its existing palette (scoped override).
                    let v = ui.visuals_mut();
                    v.selection.bg_fill = style.accent_weak;
                    v.selection.stroke = bevy_egui::egui::Stroke::new(1.0, style.accent);

                    // Header: a single quiet section title + hairline baseline
                    // (retires the bright "DEV INSPECTOR" + `ui.separator()`).
                    ui.label(
                        bevy_egui::egui::RichText::new("DIAGNOSTICS")
                            .color(style.text_secondary)
                            .size(11.5)
                            .strong(),
                    );
                    ui.add_space(4.0);
                    hairline(ui, &style);
                    ui.add_space(8.0);

                    draw_frame_rate_row(ui, &style, frame_stats);
                    ui.add_space(8.0);

                    // One outer scroll area fills the fixed dock height; sections
                    // are collapsible so the long diagnostics grid can be folded.
                    bevy_egui::egui::ScrollArea::vertical()
                        .id_salt("wc-debug-scroll")
                        .auto_shrink([false, false])
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

                            // Live MediaPipe feel readout — the calibration aid
                            // you read while adjusting the sliders on the settings
                            // dock (Advanced); the sliders themselves moved there.
                            bevy_egui::egui::CollapsingHeader::new("Hand tuning (readout)")
                                .default_open(true)
                                .show(ui, |ui| {
                                    draw_hand_tuning_readout(ui, &style, hand_readout);
                                });

                            // Captured log records (newest at the bottom),
                            // colour-coded by level.
                            bevy_egui::egui::CollapsingHeader::new("Log")
                                .default_open(true)
                                .show(ui, |ui| {
                                    crate::diagnostics::render_log_view(ui, &log_lines, &style);
                                });

                            // Collapsed by default: the curated diagnostics grid
                            // above is the day-to-day view; the full world
                            // inspector is opened on demand.
                            bevy_egui::egui::CollapsingHeader::new("World inspector")
                                .default_open(false)
                                .show(ui, |ui| {
                                    bevy_inspector_egui::bevy_inspector::ui_for_world(world, ui);
                                });
                        });
                },
            );
        });
}

/// Renders the live `MediaPipe` hand-tuning *readout* inside the dev panel: a
/// grab/pinch readout (so the operator can see the open-hand grab *floor*) plus
/// the calibration hint. The tunable sliders themselves now live on the
/// settings dock's Hand Tracking tab under the Advanced toggle (they are
/// `Dev`-category settings, surfaced there). This panel keeps only the live aid
/// you read while adjusting them in the dock side-by-side.
fn draw_hand_tuning_readout(
    ui: &mut bevy_egui::egui::Ui,
    style: &OverlayStyle,
    readout: Option<(usize, f32, f32)>,
) {
    if let Some((count, grab, pinch)) = readout {
        ui.label(format!(
            "Live:  {count} hand(s)  ·  grab {grab:.2}  ·  pinch {pinch:.2}"
        ));
        // Calibration reads the PRE-deadzone signal directly ("Grab raw (‰)" in
        // the Hand tracking grid above), so the raw readout is unaffected by the
        // deadzone slider and there is no need to zero it first.
        hint_label(
            ui,
            style,
            "Open-hand calibration: hold your hand open and relaxed, read the rest \
             floor from \"Grab raw (‰)\" above, then set \"Grab rest deadzone\" just \
             above it on the Settings dock (Advanced). Slider value = ‰ ÷ 1000, e.g. \
             raw 60‰ → deadzone 0.07.",
        );
    } else {
        hint_label(
            ui,
            style,
            "Hand-tracking feel sliders live on the Settings dock's Hand Tracking \
             tab under Advanced. Connect a provider to see the live grab/pinch \
             readout here.",
        );
    }
}

/// Fixed width of the left debug dock's `Area`, applied via `set_min_size` /
/// `set_max_size` in [`draw_dev_panel`]. [`HINT_WRAP_WIDTH`] is derived from
/// this — change them together by changing only this one.
const DEBUG_DOCK_WIDTH: f32 = 420.0;

/// Fixed wrap width for multi-line hint labels in the dev panel.
///
/// Derived from [`DEBUG_DOCK_WIDTH`] minus the frame padding (2 × 20 px)
/// and a 40 px scrollbar allowance, so this width is always available. The
/// width must be a constant: a default-wrapped label re-measures against
/// `ui.available_width()` every frame, and inside the panel's `ScrollArea`
/// that width shifts slightly as live values (diagnostics, inspector floats)
/// change the content width — so the wrap points oscillate and the hint text
/// visibly flickers between layouts. Wrapping inside a fixed-width scope
/// makes the layout identical every frame.
const HINT_WRAP_WIDTH: f32 = DEBUG_DOCK_WIDTH - 80.0;

/// One always-visible frame-rate row pinned at the top of the dev panel. Green
/// at refresh-rate, amber mid, red when frames are clearly dropping. With Bevy's
/// default `VSync` the FPS caps at the display refresh, so green == hitting refresh.
fn draw_frame_rate_row(
    ui: &mut bevy_egui::egui::Ui,
    style: &OverlayStyle,
    stats: Option<(f64, f64)>,
) {
    ui.horizontal(|ui| {
        ui.label(
            bevy_egui::egui::RichText::new("FPS")
                .color(style.text_secondary)
                .size(11.5)
                .strong(),
        );
        if let Some((fps, frame_ms)) = stats {
            let color = if fps >= 55.0 {
                style.ok_green
            } else if fps >= 30.0 {
                style.warn_amber
            } else {
                style.error_red
            };
            ui.label(
                bevy_egui::egui::RichText::new(format!("{fps:.1}"))
                    .color(color)
                    .strong(),
            );
            ui.label(
                bevy_egui::egui::RichText::new(format!("({frame_ms:.1} ms)"))
                    .color(style.text_faint),
            );
        } else {
            ui.label(
                bevy_egui::egui::RichText::new("(diagnostics unavailable)").color(style.text_faint),
            );
        }
    });
}

/// Draw a small dim multi-line hint at a fixed wrap width (see
/// [`HINT_WRAP_WIDTH`] for why the width must not track the live
/// `available_width`). Use this for every multi-line hint added to the dev
/// panel so none of them reflow-flicker.
fn hint_label(ui: &mut bevy_egui::egui::Ui, style: &OverlayStyle, text: &str) {
    ui.scope(|ui| {
        ui.set_max_width(HINT_WRAP_WIDTH);
        ui.label(
            bevy_egui::egui::RichText::new(text)
                .size(10.0)
                .color(style.text_color_dim),
        );
    });
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
        // Cap the value column so long strings (streaming line, SDK version,
        // metric text) wrap within the 420px dock instead of widening it.
        .max_col_width(HINT_WRAP_WIDTH - 100.0)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            // Grid rows lay out horizontally, so labels default to Extend (no
            // wrap) and a long value widens the dock. Force Wrap so they fold
            // within the capped column width instead.
            ui.style_mut().wrap_mode = Some(bevy_egui::egui::TextWrapMode::Wrap);
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
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_message::<ActionInput>();
        app.init_resource::<DevPanelVisible>();
        app.add_systems(Update, handle_dev_panel_toggle);
        app
    }

    fn fire_toggle(app: &mut App) {
        app.world_mut().write_message(ActionInput {
            action: WaveConductorAction::ToggleDevPanel,
            phase: ActionPhase::Pressed,
        });
        app.update();
    }

    #[test]
    fn toggle_flips_visibility() {
        let mut app = make_app();
        fire_toggle(&mut app);
        assert!(
            app.world().resource::<DevPanelVisible>().0,
            "first press should make panel visible",
        );
        fire_toggle(&mut app);
        assert!(
            !app.world().resource::<DevPanelVisible>().0,
            "second press should hide panel",
        );
    }
}
