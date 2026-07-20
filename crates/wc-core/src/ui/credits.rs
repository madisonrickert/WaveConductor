//! Full-screen credits & open-source-licenses overlay, reachable from Home.
//!
//! v4 had a dedicated `/licenses` route listing its web-stack dependencies
//! (leapjs, the Ultraleap WebSocket bridge, assorted MIT/Apache web deps).
//! v5's equivalent is this overlay, listing v5's **own** stack: the Rust/Bevy
//! engine dependencies, the vendored `MediaPipe` model lineage, the Ultraleap
//! attribution required by the Enterprise Tracking Licence §5(b)
//! (`vendor/leapc/ATTRIBUTION.md`), the OFL fonts, and the CC0 audio fixture.
//!
//! ## Data flow
//!
//! [`CreditsVisible`] (default `false`) is flipped `true` when the picker's
//! credits tile "Open Source Licenses" link is clicked
//! (`super::picker::draw_sketch_picker`). [`draw_credits_overlay`] runs in
//! [`bevy_egui::EguiPrimaryContextPass`] gated on [`AppState::Home`] and the
//! shared `picker_not_reloading` condition; it is ordered after the picker
//! system so a click opens the overlay in the same frame. While the flag is
//! set the picker skips its own draw entirely, making this the only
//! Home-state surface. Closed by the top-right X button or `Esc` (safe to
//! read `Esc` here: the key maps to `NavigateHome`, which is a no-op when the
//! app is already Home — see `crate::lifecycle::nav`). `OnExit(AppState::Home)`
//! resets the flag so a number-key sketch select can never strand the overlay
//! open for the next visit to Home.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use super::buttons::{overlay_icon_button, PointerCoarse};
use super::picker::PICKER_BACKGROUND;
use super::style::OverlayStyle;
use super::text::letter_spaced_label;
use crate::lifecycle::state::AppState;

// Phosphor icon glyph for the close button (same crate as `buttons.rs`).
use egui_phosphor::regular as phosphor;

/// Visibility flag for the full-screen credits/licenses overlay.
///
/// Mirrors the [`super::buttons::SettingsPanelVisible`] pattern: a plain bool
/// resource toggled by its entry-point widget (the credits tile's
/// "Open Source Licenses" link) and read by the draw system. Defaults `false`
/// so the overlay starts hidden.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct CreditsVisible(pub bool);

/// Plugin: registers [`CreditsVisible`] and [`draw_credits_overlay`] in
/// [`bevy_egui::EguiPrimaryContextPass`], gated on [`AppState::Home`] and the
/// picker's shared not-reloading condition, ordered after the picker draw so
/// a tile click opens the overlay without a one-frame flash of the grid.
pub struct CreditsPlugin;

impl Plugin for CreditsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CreditsVisible>();
        app.add_systems(
            bevy_egui::EguiPrimaryContextPass,
            draw_credits_overlay
                .run_if(in_state(AppState::Home))
                .run_if(super::picker::picker_not_reloading)
                .after(super::picker::draw_sketch_picker),
        );
        // A sketch can still be selected while the overlay is open (number
        // keys); reset on leaving Home so the overlay never greets the next
        // Home visit already open.
        app.add_systems(OnExit(AppState::Home), reset_credits_visible);
    }
}

/// `OnExit(AppState::Home)`: hide the credits overlay.
pub(crate) fn reset_credits_visible(mut visible: ResMut<'_, CreditsVisible>) {
    visible.0 = false;
}

/// Maximum width of the credits text column, in logical points. Keeps line
/// lengths readable on wide displays; narrower viewports use their full width.
const CONTENT_MAX_WIDTH: f32 = 720.0;

/// Draw the full-screen credits overlay when [`CreditsVisible`] is set.
///
/// Paints an opaque backdrop (same colour as the picker page) in a
/// `Foreground`-order [`egui::Area`] covering the whole viewport — an `Area`
/// rather than a second `CentralPanel` so it composes cleanly with the
/// picker's panel on the single frame where both draw (the click frame).
/// Content is a vertically scrollable, centre-aligned column; a top-right X
/// button and the `Esc` key close it.
pub fn draw_credits_overlay(
    mut contexts: EguiContexts<'_, '_>,
    style: Res<'_, OverlayStyle>,
    coarse: Res<'_, PointerCoarse>,
    mut visible: ResMut<'_, CreditsVisible>,
) {
    if !visible.0 {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    // Esc closes. NavigateHome (also bound to Esc) is a no-op on Home, so
    // consuming the press here cannot fight the lifecycle nav handler.
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        visible.0 = false;
        return;
    }

    // `content_rect` (not the deprecated `screen_rect`): the viewport area
    // available for UI content, which is what the overlay should cover.
    let screen = ctx.content_rect();
    let mut close_clicked = false;

    egui::Area::new(egui::Id::new("wc-credits-overlay"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.set_min_size(screen.size());
            // Opaque backdrop fully covering the picker grid beneath.
            ui.painter()
                .rect_filled(screen, egui::CornerRadius::ZERO, PICKER_BACKGROUND);

            egui::ScrollArea::vertical()
                .max_height(screen.height())
                .show(ui, |ui| {
                    ui.set_width(screen.width());
                    let column_width = screen.width().min(CONTENT_MAX_WIDTH);
                    ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                        // Cap paragraph wrap width to the readable column.
                        ui.set_max_width(column_width);
                        credits_content(ui, &style);
                    });
                });

            // Close button, painted last so it sits above the scroll content.
            // Same top-right position as the settings cog on sketch pages.
            let size = if coarse.0 {
                style.button_size_coarse
            } else {
                style.button_size_fine
            };
            let close_rect = egui::Rect::from_min_size(
                egui::pos2(screen.right() - 12.0 - size, screen.top() + 12.0),
                egui::Vec2::splat(size),
            );
            let mut close_ui = ui.new_child(egui::UiBuilder::new().max_rect(close_rect));
            if overlay_icon_button(&mut close_ui, &style, phosphor::X, size, 1.0).clicked() {
                close_clicked = true;
            }
        });

    if close_clicked {
        visible.0 = false;
    }
}

/// The scrollable credits body: title, lineage, contributors, and one section
/// per attribution surface (Ultraleap, models, engine crates, fonts, audio).
///
/// The vendored OBSBOT device-control SDK is proprietary (not open source)
/// and is deliberately **not** listed on this public credits surface.
fn credits_content(ui: &mut egui::Ui, style: &OverlayStyle) {
    ui.add_space(56.0);

    // Title — same Orbitron letter-spaced treatment as the credits tile,
    // v4 `.credits-content h2 { letter-spacing: 0.1em }`.
    letter_spaced_label(
        ui,
        "WaveConductor",
        egui::FontId::new(32.0, egui::FontFamily::Name("orbitron".into())),
        style.text_color_bright,
        32.0 * 0.1,
    );
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Credits & Open Source Licenses")
            .size(13.0)
            .color(style.text_secondary),
    );

    ui.add_space(20.0);
    body(ui, style, "based on hellochar by Xiaohan Zhang");
    link(
        ui,
        style,
        "github.com/hellochar/hellochar.com",
        "https://github.com/hellochar/hellochar.com",
    );

    section_header(ui, style, "CONTRIBUTORS");
    link(ui, style, "Madison Rickert", "https://madisonrickert.com");
    link(ui, style, "Rich Trapani | LoveTech", "https://lovetech.org");

    section_header(ui, style, "HAND TRACKING");
    // Long form from vendor/leapc/ATTRIBUTION.md, required by the Ultraleap
    // Enterprise Tracking Licence §5(b).
    body(
        ui,
        style,
        "WaveConductor includes hand-tracking technology from Ultraleap.",
    );
    body_dim(ui, style, "Ultraleap Tracking SDK 6.2.0");
    link(ui, style, "ultraleap.com", "https://www.ultraleap.com/");

    section_header(ui, style, "VISION MODELS & INFERENCE");
    // Lineage per assets/models/{hand,pose}/ATTRIBUTION.md.
    dep(
        ui,
        style,
        "MediaPipe Hands & BlazePose models — Google MediaPipe",
        "Apache-2.0",
    );
    dep(
        ui,
        style,
        "ONNX model conversions — OpenCV Zoo",
        "Apache-2.0",
    );
    dep(ui, style, "ONNX Runtime — Microsoft", "MIT");
    dep(
        ui,
        style,
        "ort (Rust ONNX Runtime bindings)",
        "MIT / Apache-2.0",
    );
    dep(ui, style, "nokhwa webcam capture", "Apache-2.0");

    section_header(ui, style, "ENGINE & LIBRARIES");
    dep(ui, style, "Bevy Engine", "MIT / Apache-2.0");
    dep(ui, style, "egui & bevy_egui", "MIT / Apache-2.0");
    dep(ui, style, "Phosphor Icons (egui-phosphor)", "MIT");
    dep(ui, style, "cpal audio I/O", "Apache-2.0");
    dep(ui, style, "FunDSP", "MIT / Apache-2.0");
    dep(ui, style, "Claxon FLAC decoder", "Apache-2.0");
    dep(ui, style, "Symphonia audio decoding", "MPL-2.0");
    dep(ui, style, "rtrb lock-free ring buffer", "MIT / Apache-2.0");

    section_header(ui, style, "FONTS");
    dep(ui, style, "Inter — Rasmus Andersson", "SIL OFL 1.1");
    dep(ui, style, "Fira Code", "SIL OFL 1.1");
    dep(ui, style, "Orbitron — Matt McInerney", "SIL OFL 1.1");

    section_header(ui, style, "AUDIO");
    // tests/fixtures/audio/README.md — CC0 drive-signal fixture.
    dep(
        ui,
        style,
        "\u{201c}Dance Robot, Activate!\u{201d} — Loyalty Freak Music",
        "CC0 1.0",
    );

    ui.add_space(32.0);
    ui.label(
        egui::RichText::new("WaveConductor v5 is released under the MIT License.")
            .size(12.0)
            .color(style.text_faint),
    );
    ui.add_space(64.0);
}

/// Orbitron letter-spaced section header with breathing room above and below.
fn section_header(ui: &mut egui::Ui, style: &OverlayStyle, text: &str) {
    ui.add_space(28.0);
    letter_spaced_label(
        ui,
        text,
        egui::FontId::new(14.0, egui::FontFamily::Name("orbitron".into())),
        style.text_secondary,
        14.0 * 0.1,
    );
    ui.add_space(10.0);
}

/// Primary body line (Inter 13 pt, bright-ish).
fn body(ui: &mut egui::Ui, style: &OverlayStyle, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(13.0)
            .color(style.text_primary),
    );
}

/// Secondary body line (Inter 12 pt, dim).
fn body_dim(ui: &mut egui::Ui, style: &OverlayStyle, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(12.0)
            .color(style.text_secondary),
    );
}

/// External hyperlink line (Inter 13 pt, accent-tinted like the dock links).
fn link(ui: &mut egui::Ui, style: &OverlayStyle, label: &str, url: &str) {
    ui.hyperlink_to(
        egui::RichText::new(label).size(13.0).color(style.accent),
        url,
    );
}

/// One dependency line: "name — license" (name in primary, licence dimmed).
fn dep(ui: &mut egui::Ui, style: &OverlayStyle, name: &str, license: &str) {
    // A single label per line keeps the centre alignment trivial; the licence
    // is visually separated by the em-dash and dimmer implied weight.
    ui.label(
        egui::RichText::new(format!("{name}   \u{00b7}   {license}"))
            .size(13.0)
            .color(style.text_primary),
    );
    ui.add_space(2.0);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is appropriate in test code")]
mod tests {
    use super::*;

    #[test]
    fn credits_visible_defaults_false() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<CreditsVisible>();
        assert!(!app.world().resource::<CreditsVisible>().0);
    }

    #[test]
    fn reset_credits_visible_hides_the_overlay() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.insert_resource(CreditsVisible(true));
        world.run_system_once(reset_credits_visible).unwrap();
        assert!(
            !world.resource::<CreditsVisible>().0,
            "OnExit(Home) reset must hide the credits overlay"
        );
    }
}
