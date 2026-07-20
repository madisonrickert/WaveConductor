//! Custom settings-dock section: live OBSBOT camera status, rendered directly
//! under the reflected "Camera" section (registered after the `obsbot`
//! storage key by [`super::ObsbotControlPlugin`]).
//!
//! The reflection-driven panel cannot conditionally disable individual
//! sliders, so the framing sliders stay editable in every state; the apply
//! system (`super::framing`) simply no-ops outside
//! [`ObsbotStatus::InControl`], and this section is where the operator learns
//! *why* the sliders are inert (no camera / control disabled / take-control
//! failed) — or gets the product/serial/firmware confirmation that they are
//! live. Render-only, no state (see `settings::custom_section`'s contract);
//! the state it reads lives on [`super::ObsbotControl`].

use bevy::prelude::*;
use bevy_egui::egui;

use super::{ObsbotControl, ObsbotStatus};
use crate::ui::OverlayStyle;

/// Row text size, matching the panel's other status rows
/// (`settings::panel_user::provider_status`).
const ROW_SIZE: f32 = 11.0;

/// Render the OBSBOT status row(s). Only runs while the settings panel is
/// open (the dock walks custom sections per visible frame), so the small
/// per-frame egui text allocations here are panel-scoped, not steady-state.
pub(super) fn render_status_section(world: &mut World, ui: &mut egui::Ui, style: &OverlayStyle) {
    let Some(ctl) = world.get_resource::<ObsbotControl>() else {
        return;
    };
    match &ctl.status {
        ObsbotStatus::NoDevice => {
            ui.label(
                egui::RichText::new(
                    "No OBSBOT camera detected — gimbal/zoom/FOV controls are inactive.",
                )
                .size(ROW_SIZE)
                .color(style.text_faint),
            );
        }
        ObsbotStatus::TakingControl => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(12.0));
                ui.label(
                    egui::RichText::new("taking control of camera…")
                        .size(ROW_SIZE)
                        .color(style.text_secondary),
                );
            });
        }
        ObsbotStatus::InControl {
            sn,
            firmware,
            product,
        } => {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("In control:")
                        .size(ROW_SIZE)
                        .color(style.text_secondary),
                );
                ui.label(
                    egui::RichText::new(product.as_str())
                        .size(ROW_SIZE)
                        .color(style.text_primary),
                );
                ui.label(
                    egui::RichText::new(sn.as_str())
                        .size(ROW_SIZE)
                        .color(style.text_faint),
                );
                ui.label(
                    egui::RichText::new("fw")
                        .size(ROW_SIZE)
                        .color(style.text_faint),
                );
                ui.label(
                    egui::RichText::new(firmware.as_str())
                        .size(ROW_SIZE)
                        .color(style.text_faint),
                );
            })
            .response
            .on_hover_text(
                "The camera's on-device AI and gestures are off; the gimbal/zoom/FOV \
                 sliders above drive it live. Framing is re-applied automatically after \
                 every restart or re-plug.",
            );
        }
        ObsbotStatus::Failed { .. } => {
            ui.label(
                egui::RichText::new(
                    "Take-control FAILED — the camera's on-device AI may still be active. \
                     See the log, or disable AI/gestures in OBSBOT Center \
                     (docs/runbooks/obsbot.md).",
                )
                .size(ROW_SIZE)
                .color(style.error_red),
            );
        }
        ObsbotStatus::ControlDisabled { .. } => {
            ui.label(
                egui::RichText::new(
                    "Camera detected, control disabled — enable \"Take control\" above to \
                     use the gimbal/zoom/FOV controls.",
                )
                .size(ROW_SIZE)
                .color(style.warn_amber),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every status variant renders without panicking inside a real egui
    /// frame (the same headless-render smoke the other panel pieces pin).
    #[test]
    fn every_status_variant_renders() {
        let ctx = egui::Context::default();
        let style = OverlayStyle::default();
        let statuses = [
            ObsbotStatus::NoDevice,
            ObsbotStatus::TakingControl,
            ObsbotStatus::InControl {
                sn: "SN12345678901X".to_owned(),
                firmware: "6.2.7.1".to_owned(),
                product: "Tiny 2 Lite".to_owned(),
            },
            ObsbotStatus::Failed {
                achieved: super::super::ControlSteps::AI_OFF,
            },
            ObsbotStatus::ControlDisabled {
                sn: "SN12345678901X".to_owned(),
            },
        ];
        for status in statuses {
            let mut world = World::new();
            world.insert_resource(ObsbotControl {
                status,
                ..Default::default()
            });
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                render_status_section(&mut world, ui, &style);
            });
        }
        // Reaching here without a panic is the assertion.
    }

    /// An absent resource (headless harness) renders nothing and must not
    /// panic.
    #[test]
    fn absent_resource_is_a_noop() {
        let ctx = egui::Context::default();
        let style = OverlayStyle::default();
        let mut world = World::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            render_status_section(&mut world, ui, &style);
        });
    }
}
