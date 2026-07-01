//! Overlay buttons — Home, Settings, Volume.
//!
//! Floating `egui::Area`-positioned widgets that match v4's
//! `.overlay-button` SCSS rules. Each button reads [`OverlayStyle`] for its
//! palette and [`UiOpacity`] for its alpha; touch devices flip
//! [`PointerCoarse`] which scales button size from 32→44 px.
//!
//! ## Data flow
//!
//! `TouchInput` messages → [`update_pointer_coarse`] → [`PointerCoarse`] resource.
//! [`overlay_icon_button`] reads [`OverlayStyle`] constants and the egui
//! animation clock to produce a hover-animated, alpha-scaled button widget.
//! Task 14 adds `SettingsPanelVisible`, `draw_home_button`, and
//! `draw_settings_button`. Task 15 adds `VolumeMuted`,
//! `draw_volume_button`, and `sync_volume_muted`. This module provides the
//! shared primitive, touch-detection resource, and all draw systems.
//! Plan 11.5 Bug 1 refactored all three draw systems from `&mut World` to
//! typed `SystemParam` signatures so click events are processed correctly.

use std::time::Duration;

use bevy::input::touch::TouchInput;
use bevy::prelude::*;
use bevy::window::{PrimaryWindow, Window};
use bevy_egui::render::EguiBevyPaintCallback;
use bevy_egui::{egui, EguiContexts};

use super::auto_fade::UiOpacity;
use super::blur::callback::BackdropBlurPaintCallback;
use super::frame::should_paint_backdrop_blur;
use super::style::OverlayStyle;
use crate::audio::command::AudioCommand;
use crate::audio::state::AudioState;
use crate::lifecycle::state::AppState;

// Re-export so Tasks 14/15 can reach the icon glyph constants via this module.
pub use egui_phosphor::regular as phosphor;

/// `true` while a touch has been seen in the last second; `false` otherwise.
///
/// Buttons read this resource to choose between fine (32 px) and coarse
/// (44 px) sizes. Matches v4's CSS `@media (pointer: coarse)` rule.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct PointerCoarse(pub bool);

/// Tracks the [`Time::elapsed`] value at which the last touch event arrived.
///
/// Crate-private — only [`update_pointer_coarse`] reads and writes it.
#[derive(Resource, Debug, Default)]
pub(crate) struct LastTouchAt(Duration);

/// How long after the last touch event [`PointerCoarse`] stays `true`.
///
/// Matches the hold duration implied by v4's pointer-coarse media query
/// (CSS doesn't time out, but we want a graceful revert on hybrid devices).
const TOUCH_COARSE_HOLD: Duration = Duration::from_secs(1);

/// Plugin: inserts [`PointerCoarse`] + [`LastTouchAt`] resources and
/// registers [`update_pointer_coarse`].
///
/// `OverlayButtonsPlugin::build` also adds egui draw systems once Tasks 14
/// and 15 land; for now only the touch-detection half ships.
pub struct OverlayButtonsPlugin;

impl Plugin for OverlayButtonsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PointerCoarse>();
        app.init_resource::<LastTouchAt>();
        app.init_resource::<SettingsPanelVisible>();
        app.init_resource::<VolumeMuted>();
        // Register the message type if it isn't already present (idempotent).
        // `bevy::input::InputPlugin` handles this in production; tests that
        // use `MinimalPlugins` need explicit registration.
        app.add_message::<TouchInput>();
        app.add_systems(Update, update_pointer_coarse);
        // Keep VolumeMuted in sync with the audio thread's echo so the button
        // icon always reflects the authoritative mute state (the keyboard
        // shortcut `v` toggles audio independently of the UI button).
        app.add_systems(
            PreUpdate,
            sync_volume_muted.after(crate::audio::state::pump_audio_messages),
        );
        app.add_systems(
            bevy_egui::EguiPrimaryContextPass,
            // Chain ensures deterministic draw order (Home → Settings → Volume,
            // left-to-right matching v4's layout). All three share EguiContexts
            // so they cannot run in parallel regardless; `.chain()` makes the
            // ordering explicit rather than relying on Bevy's conflict resolution.
            // `draw_leap_status_led` runs last so it is always on top (overlaid
            // on the other buttons' z-order in the same pass).
            (
                draw_home_button,
                draw_settings_button,
                draw_volume_button,
                draw_leap_status_led,
            )
                .chain(),
        );
    }
}

/// Flip [`PointerCoarse`] `true` on any incoming touch message; auto-revert
/// to `false` after [`TOUCH_COARSE_HOLD`] of no touch activity.
///
/// Reads [`TouchInput`] messages (not consumed — other systems can still read
/// them in the same frame). The revert check uses saturating subtraction so a
/// fresh app with `elapsed == 0` never underflows.
pub(crate) fn update_pointer_coarse(
    time: Res<'_, Time>,
    mut touches: MessageReader<'_, '_, TouchInput>,
    mut coarse: ResMut<'_, PointerCoarse>,
    mut last_touch_at: ResMut<'_, LastTouchAt>,
) {
    let now = time.elapsed();
    if touches.read().next().is_some() {
        last_touch_at.0 = now;
        coarse.0 = true;
        return;
    }
    if coarse.0 && now.saturating_sub(last_touch_at.0) >= TOUCH_COARSE_HOLD {
        coarse.0 = false;
    }
}

/// Visibility flag for the user-facing settings panel.
///
/// Toggled by [`draw_settings_button`] when the cog icon is clicked. Task 18
/// reads this resource to decide whether to draw `panel_user`'s frosted-glass
/// frame. Defaults `false` so the panel starts hidden.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct SettingsPanelVisible(pub bool);

/// Top-left home button. Hidden when [`AppState`] is already [`AppState::Home`].
///
/// Runs in [`bevy_egui::EguiPrimaryContextPass`]. On click, sets
/// `NextState<AppState>` to `Home`. Button size scales with [`PointerCoarse`]
/// (32 px fine / 44 px coarse). Icon: [`egui_phosphor::regular::HOUSE`].
///
/// Uses the standard `SystemParam` signature (not `&mut World`) so that
/// [`bevy_egui::EguiContexts`] and [`NextState`] are held as ordinary
/// typed borrows, avoiding any potential exclusive-world ordering issues that
/// could prevent click events from being processed correctly.
pub fn draw_home_button(
    state: Res<'_, State<AppState>>,
    mut contexts: EguiContexts<'_, '_>,
    style: Res<'_, OverlayStyle>,
    opacity: Res<'_, UiOpacity>,
    coarse: Res<'_, PointerCoarse>,
    mut next: ResMut<'_, NextState<AppState>>,
) {
    // Hidden on the Home screen itself — no point navigating home from home.
    if **state == AppState::Home {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let size = if coarse.0 {
        style.button_size_coarse
    } else {
        style.button_size_fine
    };

    let mut clicked = false;
    egui::Area::new(egui::Id::new("wc-home-button"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(12.0, 12.0))
        .show(ctx, |ui| {
            let response = overlay_icon_button(
                ui,
                &style,
                egui_phosphor::regular::HOUSE,
                size,
                opacity.current,
            );
            if response.clicked() {
                clicked = true;
            }
        });

    if clicked {
        next.set(AppState::Home);
    }
}

/// Top-right settings cog. Toggles [`SettingsPanelVisible`].
///
/// Runs in [`bevy_egui::EguiPrimaryContextPass`]. Position is
/// `(window_width - 12 - size, 12)` so it stays flush with the right edge
/// regardless of window size. Button size scales with [`PointerCoarse`]
/// (32 px fine / 44 px coarse). Icon: [`egui_phosphor::regular::GEAR`].
///
/// Hidden on [`AppState::Home`] — sketch chrome is not shown on the picker page,
/// matching v4's behaviour where only active sketch pages show the cog.
///
/// Uses the standard `SystemParam` signature (not `&mut World`) so that
/// [`bevy_egui::EguiContexts`] and [`SettingsPanelVisible`] are held as ordinary
/// typed borrows, avoiding any potential exclusive-world ordering issues that
/// could prevent click events from being processed correctly.
pub fn draw_settings_button(
    state: Res<'_, State<AppState>>,
    mut contexts: EguiContexts<'_, '_>,
    style: Res<'_, OverlayStyle>,
    opacity: Res<'_, UiOpacity>,
    coarse: Res<'_, PointerCoarse>,
    mut visible: ResMut<'_, SettingsPanelVisible>,
    windows: Query<'_, '_, &Window, With<PrimaryWindow>>,
) {
    // Hidden on the Home screen — same guard as `draw_home_button`.
    if **state == AppState::Home {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    // Read window width; fall back to 1280 if no primary window is present yet.
    let window_width = windows.single().map_or(1280.0, Window::width);
    let size = if coarse.0 {
        style.button_size_coarse
    } else {
        style.button_size_fine
    };

    let mut clicked = false;
    egui::Area::new(egui::Id::new("wc-settings-button"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(window_width - 12.0 - size, 12.0))
        .show(ctx, |ui| {
            let response = overlay_icon_button(
                ui,
                &style,
                egui_phosphor::regular::GEAR,
                size,
                opacity.current,
            );
            if response.clicked() {
                clicked = true;
            }
        });

    if clicked {
        visible.0 = !visible.0;
    }
}

/// Draw a round-cornered icon button with hover colour transition and frosted
/// glass background.
///
/// Allocates a `size × size` rect with click sense, animates the background
/// fill between [`OverlayStyle::button_fill_inactive`] and
/// [`OverlayStyle::button_fill_hovered`] via `ctx.animate_value_with_time`,
/// paints a [`backdrop_blur_frame`](super::frame::backdrop_blur_frame) (frosted glass + tint + stroke), then
/// centres the icon glyph at `size * 0.5` font points. All colours are
/// alpha-scaled by `opacity_mul` so the auto-fade system can dim the whole
/// chrome surface uniformly.
///
/// The frosted blur background is an intentional deviation from v4's CSS
/// (v4's `.overlay-button` did not have `backdrop-filter`), added at Madison's
/// request to match the settings-panel frosted look. The blur callback is a
/// no-op when the blur texture or pipeline is not yet ready, so the tint still
/// shows through as a fallback.
///
/// `icon` should be a UTF-8 glyph string from [`egui_phosphor::regular`]
/// (or any other icon font registered with the egui context).
///
/// Returns the [`egui::Response`] so callers can wire `.clicked()` handlers.
pub fn overlay_icon_button(
    ui: &mut egui::Ui,
    style: &OverlayStyle,
    icon: &str,
    size: f32,
    opacity_mul: f32,
) -> egui::Response {
    // Allocate the full click-sense rect first so hover/click detection works
    // before the blur frame is painted underneath.
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::click());
    // Pointer cursor: matching v4's `cursor: pointer` on `.overlay-button`.
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let hovered = response.hovered();

    // Lerp fill colour by hover state. `animate_value_with_time` uses the
    // response's widget id as the animation key so each button animates
    // independently. The 0.2 s duration matches v4's `transition: 0.2s ease`.
    let t =
        ui.ctx()
            .animate_value_with_time(response.id, if hovered { 1.0_f32 } else { 0.0_f32 }, 0.2);

    // Paint the frosted blur background + tint using backdrop_blur_frame.
    // The tint alpha lerps between inactive and hovered fills via the hover
    // animation value `t`. This matches the panel chrome's frosted appearance.
    // Note: backdrop_blur_frame calls ui.allocate_exact_size internally, so we
    // paint into a child UI constrained to the already-allocated rect.
    // Use a zero-padding frame since the button icon is painted separately below.
    {
        let fill = lerp_color(style.button_fill_inactive, style.button_fill_hovered, t);
        let fill = scale_color_alpha(fill, opacity_mul);
        let stroke = scale_color_alpha(style.button_stroke, opacity_mul);

        let painter = ui.painter();
        // Blur callback: composites the blurred backdrop behind the button.
        // Match the render node's opacity gate so faded buttons cannot keep
        // painting a stale frosted-glass sample after the blur texture stops
        // refreshing.
        if should_paint_backdrop_blur(opacity_mul) {
            painter.add(EguiBevyPaintCallback::new_paint_callback(
                rect,
                BackdropBlurPaintCallback {
                    corner_radius: f32::from(style.button_corner_radius),
                    rect,
                },
            ));
        }

        // Tint + stroke over the blur. Stroke uses Outside so it remains
        // visible in egui's compositing — Inside can be occluded by the fill.
        painter.add(egui::Shape::Rect(egui::epaint::RectShape::new(
            rect,
            egui::CornerRadius::same(style.button_corner_radius),
            fill,
            egui::Stroke::new(1.0, stroke),
            egui::epaint::StrokeKind::Outside,
        )));
    }

    // Lerp text colour with the same animation value `t` so icon brightness
    // transitions smoothly (matching v4's unified hover transition).
    // Previously this was a hard switch (dim ↔ bright); the lerp gives the
    // same smooth feel as the background fill transition.
    let text_color = lerp_color(style.text_color_dim, style.text_color_bright, t);
    let text_color = scale_color_alpha(text_color, opacity_mul);
    // Paint the icon glyph centred in the button. Use the named "phosphor"
    // font family explicitly — the Proportional chain has Inter at position 0,
    // which maps some PUA codepoints (U+E1E2–E2C7) that overlap with Phosphor
    // icon glyphs including HOUSE (E2C2) and GEAR (E270). Using the named
    // family bypasses that clash and always uses the correct Phosphor glyph.
    // Font size = half the button size so the glyph fills ~50% of the area
    // (matching v4's icon sizing).
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::new(size * 0.5, egui::FontFamily::Name("phosphor".into())),
        text_color,
    );

    response
}

/// Linearly interpolate each RGBA channel of `a` toward `b` by `t ∈ [0, 1]`.
///
/// Used to blend [`OverlayStyle::button_fill_inactive`] →
/// [`OverlayStyle::button_fill_hovered`] based on the egui animation clock.
///
/// The `as u8` cast inside the closure is safe: `x` and `y` are already
/// `u8`, so `f32::from(x) + diff * t.clamp(0, 1)` lies in `[0.0, 255.0]`
/// and `round()` cannot produce a value outside that range. Truncation is
/// the correct rounding mode here (match CSS colour math).
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "interpolated channel lies in [0.0, 255.0] by construction; truncation is intentional"
)]
fn lerp_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let lerp_u8 = |x: u8, y: u8| -> u8 {
        (f32::from(x) + (f32::from(y) - f32::from(x)) * t.clamp(0.0, 1.0)).round() as u8
    };
    egui::Color32::from_rgba_unmultiplied(
        lerp_u8(a.r(), b.r()),
        lerp_u8(a.g(), b.g()),
        lerp_u8(a.b(), b.b()),
        lerp_u8(a.a(), b.a()),
    )
}

/// Multiply the alpha channel of `color` by `mul`, clamped to `[0, 1]`.
///
/// Used to apply [`UiOpacity::current`] uniformly to every chrome element.
///
/// The `as u8` cast is safe: `f32::from(u8) * mul` where `mul ∈ [0, 1]`
/// produces a value in `[0.0, 255.0]`; truncation is the intended rounding.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "product is in [0.0, 255.0] by construction; truncation is intentional"
)]
fn scale_color_alpha(color: egui::Color32, mul: f32) -> egui::Color32 {
    let a = (f32::from(color.a()) * mul.clamp(0.0, 1.0)) as u8;
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

/// Local mirror of mute state. Flipped on each volume-button click and
/// synced from [`crate::audio::state::AudioState::muted`] each `PreUpdate`
/// so the icon stays accurate when mute is toggled via the `v` keyboard
/// shortcut as well as via the button.
///
/// The audio ring is the authoritative consumer; this resource exists so the
/// button icon doesn't re-read the audio engine each frame.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct VolumeMuted(pub bool);

/// `PreUpdate` system that keeps [`VolumeMuted`] in sync with
/// [`AudioState::muted`].
///
/// Runs after [`crate::audio::state::pump_audio_messages`] has drained the
/// audio→main message ring, so `AudioState::muted` reflects the latest echo
/// from the audio thread. This ensures the button icon tracks the `v`
/// keyboard shortcut as well as direct button clicks.
pub(crate) fn sync_volume_muted(
    audio_state: Option<Res<'_, AudioState>>,
    mut muted: ResMut<'_, VolumeMuted>,
) {
    if let Some(state) = audio_state {
        muted.0 = state.muted;
    }
}

/// Top-right volume button. Toggles mute; icon flips between
/// [`egui_phosphor::regular::SPEAKER_HIGH`] and
/// [`egui_phosphor::regular::SPEAKER_X`].
///
/// Runs in [`bevy_egui::EguiPrimaryContextPass`]. Position is
/// `(window_width - 12 - size - 8 - size, 12)` so it sits 8 px left of the
/// Settings button. Button size scales with [`PointerCoarse`]
/// (32 px fine / 44 px coarse).
///
/// On click: flips [`VolumeMuted`] and pushes
/// [`AudioCommand::SetMuted`] to the audio ring. Ring-full failures are
/// silently dropped — the audio thread is severely backlogged in that case
/// and will process the eventual echo correctly.
///
/// Hidden on [`AppState::Home`] — sketch chrome is not shown on the picker page,
/// matching v4's behaviour where only active sketch pages show the volume control.
pub fn draw_volume_button(
    state: Res<'_, State<AppState>>,
    mut contexts: EguiContexts<'_, '_>,
    style: Res<'_, OverlayStyle>,
    opacity: Res<'_, UiOpacity>,
    coarse: Res<'_, PointerCoarse>,
    mut muted: ResMut<'_, VolumeMuted>,
    sender: Option<NonSendMut<'_, crate::audio::ring::AudioCommandSender>>,
    windows: Query<'_, '_, &Window, With<PrimaryWindow>>,
) {
    // Hidden on the Home screen — same guard as `draw_home_button`.
    if **state == AppState::Home {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    // Read window width; fall back to 1280 if no primary window is present yet.
    let window_width = windows.single().map_or(1280.0, Window::width);
    let size = if coarse.0 {
        style.button_size_coarse
    } else {
        style.button_size_fine
    };

    // Volume sits just left of Settings, with an 8 px gap between them.
    let pos_x = window_width - 12.0 - size - 8.0 - size;
    let current_muted = muted.0;

    let mut clicked = false;
    egui::Area::new(egui::Id::new("wc-volume-button"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(pos_x, 12.0))
        .show(ctx, |ui| {
            let icon = if current_muted {
                egui_phosphor::regular::SPEAKER_X
            } else {
                egui_phosphor::regular::SPEAKER_HIGH
            };
            let response = overlay_icon_button(ui, &style, icon, size, opacity.current);
            if response.clicked() {
                clicked = true;
            }
        });

    if clicked {
        // Flip the local mirror first so the icon updates this frame without
        // waiting for the audio-thread echo.
        let new_muted = !current_muted;
        muted.0 = new_muted;
        // Push the command to the audio ring. Ring-full failure is non-fatal.
        if let Some(mut s) = sender {
            let _ = s.push(AudioCommand::SetMuted(new_muted));
        }
    }
}

/// Maps [`crate::input::state::PrimaryState`] to a dot color and one-line
/// tooltip string for the status LED.
///
/// Color semantics mirror the v4 connection-status conventions:
/// - Green → fully operational (Streaming, clean health)
/// - Yellow-green → soft degradation (smudged/robust/low-resource / low FPS)
/// - Blue → device present but idle (attached, not yet streaming)
/// - Amber `#f39c12` → service reachable but no device attached
/// - Orange `#e67e22` → device wedged (frozen stream; service alive but not delivering frames)
/// - Red → not reachable (service missing, disconnected, device error)
/// - Dark gray → not started (LED is present but muted)
fn leap_led_color_and_tooltip(
    state: crate::input::state::PrimaryState,
) -> (bevy_egui::egui::Color32, &'static str) {
    use crate::input::state::PrimaryState;
    use bevy_egui::egui::Color32;
    match state {
        PrimaryState::NotStarted => (Color32::DARK_GRAY, "Not started"),
        PrimaryState::ServiceMissing => (Color32::RED, "Ultraleap service not running"),
        PrimaryState::Disconnected => (Color32::RED, "Connection lost"),
        PrimaryState::ServiceOnly => (
            Color32::from_rgb(0xf3, 0x9c, 0x12),
            "Service up, no device attached",
        ),
        PrimaryState::DeviceAttached => (
            Color32::from_rgb(0x34, 0x98, 0xdb),
            "Device attached, not streaming",
        ),
        PrimaryState::Streaming => (Color32::from_rgb(0x2e, 0xcc, 0x71), "Streaming"),
        PrimaryState::DeviceDegraded => (Color32::from_rgb(0xf1, 0xc4, 0x0f), "Tracking degraded"),
        PrimaryState::DeviceWedged => (
            Color32::from_rgb(0xe6, 0x7e, 0x22),
            "Tracking frozen (service wedged)",
        ),
        PrimaryState::DeviceFailed => (Color32::from_rgb(0xc0, 0x39, 0x2b), "Device error"),
    }
}

/// Draws a small status LED in the top-right corner reflecting the
/// primary hand-tracking provider's coarse state. Hover for tooltip.
///
/// Runs in `EguiPrimaryContextPass` so the egui context is active. The
/// registry is `Option<Res<...>>` so tests without `HandTrackingPlugin`
/// (e.g., the `MinimalPlugins`-based UI test harness) don't panic.
pub fn draw_leap_status_led(
    mut contexts: EguiContexts<'_, '_>,
    registry: Option<Res<'_, crate::input::provider::ProviderRegistry>>,
) {
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let Some(registry) = registry else { return };

    let status = registry.primary_status();
    let (color, tooltip) = leap_led_color_and_tooltip(status.primary());

    egui::Area::new(egui::Id::new("leap_status_led"))
        .anchor(egui::Align2::RIGHT_TOP, egui::Vec2::new(-16.0, 16.0))
        .show(ctx, |ui| {
            let (rect, response) =
                ui.allocate_exact_size(egui::Vec2::splat(12.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 6.0, color);
            response.on_hover_text(tooltip);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(OverlayButtonsPlugin);
        app
    }

    #[test]
    fn pointer_coarse_defaults_to_false() {
        let app = make_app();
        assert!(!app.world().resource::<PointerCoarse>().0);
    }

    #[test]
    fn pointer_coarse_flips_true_on_touch() {
        let mut app = make_app();
        app.world_mut().write_message(TouchInput {
            phase: bevy::input::touch::TouchPhase::Started,
            position: Vec2::new(100.0, 200.0),
            window: Entity::PLACEHOLDER,
            force: None,
            id: 0,
        });
        app.update();
        assert!(app.world().resource::<PointerCoarse>().0);
    }

    #[test]
    fn settings_panel_visible_defaults_false() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<SettingsPanelVisible>();
        assert!(!app.world().resource::<SettingsPanelVisible>().0);
    }

    #[test]
    fn settings_panel_visible_toggles_with_resource_change() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<SettingsPanelVisible>();
        app.world_mut().resource_mut::<SettingsPanelVisible>().0 = true;
        assert!(app.world().resource::<SettingsPanelVisible>().0);
    }

    #[test]
    fn pointer_coarse_reverts_after_hold_duration() {
        let mut app = make_app();
        // Send a touch message so PointerCoarse flips to true.
        app.world_mut().write_message(TouchInput {
            phase: bevy::input::touch::TouchPhase::Started,
            position: Vec2::new(100.0, 200.0),
            window: Entity::PLACEHOLDER,
            force: None,
            id: 0,
        });
        app.update();
        assert!(app.world().resource::<PointerCoarse>().0);

        // In Bevy 0.18, `Time<()>` is overwritten each frame by
        // `update_virtual_time` which derives from `Time<Virtual>` and
        // `Time<Real>`. Direct `Time::advance_by` is therefore NOT the right
        // way to control elapsed time in tests. Use
        // `TimeUpdateStrategy::ManualDuration` so each `app.update()` advances
        // `Time::elapsed()` by a fixed step.
        //
        // TOUCH_COARSE_HOLD = 1 s. Twelve 100 ms steps = 1.2 s total, which
        // surpasses the hold threshold by 200 ms with no further touch events.
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_millis(100),
        ));
        for _ in 0..12_u32 {
            app.update();
        }
        assert!(!app.world().resource::<PointerCoarse>().0);
    }

    #[test]
    fn volume_muted_defaults_false() {
        let app = make_app();
        assert!(!app.world().resource::<VolumeMuted>().0);
    }

    #[test]
    fn sync_volume_muted_follows_audio_state() {
        let mut app = make_app();
        // Insert a mock AudioState with muted = true.
        app.world_mut().insert_resource(AudioState {
            muted: true,
            ..AudioState::default()
        });
        app.update();
        assert!(
            app.world().resource::<VolumeMuted>().0,
            "VolumeMuted should mirror AudioState::muted after update"
        );
    }
}
