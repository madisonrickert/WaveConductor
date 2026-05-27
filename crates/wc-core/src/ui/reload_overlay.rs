//! Full-screen black overlay driven by [`SketchReloadState`].
//!
//! Paints an opaque `egui::Area` scaled by the current reload alpha so that
//! sketch-to-sketch transitions fade through black rather than flashing the
//! picker page.
//!
//! ## Data flow
//!
//! `drive_reload_state` (in `lifecycle/reload.rs`) updates `SketchReloadState`
//! each `Update` frame. This system runs in [`bevy_egui::EguiPrimaryContextPass`]
//! and reads the same resource to determine the overlay alpha.
//!
//! When `alpha == 0` (i.e., `phase == Idle`) the function returns immediately
//! without touching egui — zero overhead in the steady state.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use crate::lifecycle::reload::SketchReloadState;

/// System: paint the full-screen reload overlay.
///
/// Runs in [`bevy_egui::EguiPrimaryContextPass`] unconditionally (no state gate)
/// so it fires even during the one-frame `Switch` phase while `AppState == Home`.
/// No-ops when `alpha == 0` for zero per-frame cost in the steady state.
pub fn draw_reload_overlay(
    reload: Res<'_, SketchReloadState>,
    time: Res<'_, Time>,
    mut contexts: EguiContexts<'_, '_>,
) {
    let alpha = reload.overlay_alpha(time.elapsed());
    if alpha <= 0.0 {
        return;
    }

    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    // Use a large, stable id that won't collide with picker or settings areas.
    egui::Area::new(egui::Id::new("wc-reload-overlay"))
        .order(egui::Order::Tooltip) // above panels; below debug toasts
        .fixed_pos(egui::pos2(0.0, 0.0))
        .show(ctx, |ui| {
            // Fill the entire screen. `content_rect()` is the logical pixel
            // size of the primary window as reported by egui.
            let screen = ctx.content_rect();
            let (rect, _) = ui.allocate_exact_size(screen.size(), egui::Sense::hover());

            #[allow(
                clippy::as_conversions,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "alpha is clamped to [0, 1]; product is in [0, 255]; truncation is intentional"
            )]
            let overlay_alpha = (alpha.clamp(0.0, 1.0) * 255.0) as u8;
            ui.painter().rect_filled(
                rect,
                egui::CornerRadius::ZERO,
                egui::Color32::from_black_alpha(overlay_alpha),
            );
        });
}
