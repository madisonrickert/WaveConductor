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
//! Tasks 14 and 15 wire the actual `draw_*` systems; this module provides
//! only the shared primitive and the touch-detection resource.

use std::time::Duration;

use bevy::input::touch::TouchInput;
use bevy::prelude::*;
use bevy_egui::egui;

use super::auto_fade::UiOpacity;
use super::style::OverlayStyle;

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
        // Register the message type if it isn't already present (idempotent).
        // `bevy::input::InputPlugin` handles this in production; tests that
        // use `MinimalPlugins` need explicit registration.
        app.add_message::<TouchInput>();
        app.add_systems(Update, update_pointer_coarse);
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

/// Draw a round-cornered icon button with hover colour transition.
///
/// Allocates a `size × size` rect with click sense, animates the background
/// fill between [`OverlayStyle::button_fill_inactive`] and
/// [`OverlayStyle::button_fill_hovered`] via `ctx.animate_value_with_time`,
/// paints the rounded rect + stroke, then centres the icon glyph at
/// `size * 0.5` font points. All colours are alpha-scaled by `opacity_mul`
/// so the auto-fade system can dim the whole chrome surface uniformly.
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
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::click());
    let hovered = response.hovered();

    // Lerp fill colour by hover state. `animate_value_with_time` uses the
    // response's widget id as the animation key so each button animates
    // independently. The 0.2 s duration matches v4's `transition: 0.2s ease`.
    let t = ui
        .ctx()
        .animate_value_with_time(response.id, if hovered { 1.0_f32 } else { 0.0_f32 }, 0.2);
    let fill = lerp_color(style.button_fill_inactive, style.button_fill_hovered, t);
    let fill = scale_color_alpha(fill, opacity_mul);
    let stroke = scale_color_alpha(style.button_stroke, opacity_mul);

    let painter = ui.painter();
    painter.rect(
        rect,
        egui::CornerRadius::same(style.button_corner_radius),
        fill,
        egui::Stroke::new(1.0, stroke),
        egui::epaint::StrokeKind::Inside,
    );

    let text_color = scale_color_alpha(
        if hovered {
            style.text_color_bright
        } else {
            style.text_color_dim
        },
        opacity_mul,
    );
    // Paint the icon glyph centred in the button. Font size = half the button
    // size so the glyph fills ~50% of the available area (matching v4's
    // icon sizing).
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(size * 0.5),
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

// Suppress unused-import lint: UiOpacity is imported so Task 14 draw
// systems added to this module won't need an extra import.
const _: fn() = || {
    let _ = std::mem::size_of::<UiOpacity>();
};

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
}
