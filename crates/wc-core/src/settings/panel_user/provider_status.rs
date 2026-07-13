//! Hand-tracking status rows shown under the "Tracking provider" and
//! "Inference backend" dropdowns on the settings panel's Hand Tracking tab.
//!
//! Two read-only rows, both snapshotted together into a [`HandTrackingStatus`]
//! by [`hand_tracking_status_snapshot`] and drawn by [`super::fields`]:
//!
//! - [`ProviderStatusLine`] (under "Tracking provider") is derived from the live
//!   [`crate::input::activation::HandTrackingActivation`] cue (not the dropdown's
//!   *selected* enum value) by [`provider_status_line`], so the row reports
//!   whether tracking is actually live rather than what the operator last picked.
//! - The backend row (under "Inference backend") reports the EP the `MediaPipe`
//!   sessions actually registered on — `"ort/CoreML"`, `"ort/CPU"`, or a degraded
//!   mixed `"ort/CoreML+CPU"` when one model's GPU commit failed and that model
//!   alone fell back. Without it, a kiosk whose palm detector quietly degraded to
//!   the CPU at 09:00 looks *identical* to a healthy one for the rest of the
//!   soak: tracking is `Active`, so the provider row above is silent, and the only
//!   evidence is one `warn!` line in a log nobody tails. It is styled amber
//!   whenever it is a mixed `+CPU` label ([`backend_is_degraded`]), so a degraded
//!   kiosk *looks* degraded where the operator is already looking.
//!
//! Both are read from `&'static str` / `Copy` state (see
//! [`crate::input::provider::HandTrackingProvider::backend_label`]) — no
//! allocation, no lock — because the panel re-snapshots them every frame it is
//! open.

use bevy::prelude::World;
use bevy_egui::egui;

use crate::ui::OverlayStyle;

/// The Hand Tracking tab's two live status rows, snapshotted out of the `World`
/// before the egui closure borrows it (see [`hand_tracking_status_snapshot`]).
///
/// `Copy`: two `Option`s over a fieldless enum and a `&'static str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct HandTrackingStatus {
    /// Row under the "Tracking provider" dropdown. `None` = no row (live, or off).
    pub(super) provider: Option<ProviderStatusLine>,
    /// Row under the "Inference backend" dropdown: the backend label the primary
    /// provider actually registered. `None` = no row (no inference provider, or
    /// its sessions have not started).
    pub(super) backend: Option<&'static str>,
}

/// What the status row under the "Tracking provider" dropdown should show,
/// derived from the [`HandTrackingActivation`] cue by [`provider_status_line`]
/// (the dropdown's *selected* enum value is not consulted: the row reports
/// whether tracking is actually live, which is what the operator waits on).
///
/// [`HandTrackingActivation`]: crate::input::activation::HandTrackingActivation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProviderStatusLine {
    /// A provider is starting, or Auto is still probing a candidate (the
    /// `MediaPipe` model-load/camera-open window, or Auto's Leap device-grace).
    /// Rendered as a spinner + "starting…".
    Starting,
    /// Auto fell back to the silent mock: the app runs but there is no hand
    /// tracking. Rendered as a short amber note.
    NoTracking,
    /// The selected provider failed / is unreachable. Rendered as a short red
    /// note pointing at the dev panel, which has the full multi-axis status.
    Failed,
}

impl ProviderStatusLine {
    /// Storage key of the settings struct owning the provider dropdown
    /// ([`crate::settings::HandTrackingSettings`]). Shared with the backend row
    /// below, which lives on the same settings struct.
    pub(super) const STORAGE_KEY: &'static str =
        <crate::settings::HandTrackingSettings as crate::settings::SketchSettings>::STORAGE_KEY;
    /// Field name of the provider dropdown within that struct.
    pub(super) const FIELD_NAME: &'static str = "provider";
}

/// Field name of the inference-backend dropdown within
/// [`crate::settings::HandTrackingSettings`] — the row the live backend label is
/// rendered directly beneath.
pub(super) const BACKEND_FIELD_NAME: &str = "backend";

/// Snapshot both Hand Tracking status rows out of the `World`.
///
/// Each half fails soft to `None` (no row) when its source resource is absent
/// (headless tests, provider `Off` with an empty registry). Cheap enough to run
/// every frame the panel is open: one resource copy and one `&'static str` read
/// behind the registry's primary-slot lookup.
pub(super) fn hand_tracking_status_snapshot(world: &World) -> HandTrackingStatus {
    let provider = world
        .get_resource::<crate::input::activation::HandTrackingActivation>()
        .copied()
        .and_then(provider_status_line);
    let backend = world
        .get_resource::<crate::input::provider::ProviderRegistry>()
        .and_then(crate::input::provider::ProviderRegistry::primary_backend_label);
    HandTrackingStatus { provider, backend }
}

/// Map the [`HandTrackingActivation`] cue to the status row to display.
///
/// - `Settling` → [`ProviderStatusLine::Starting`]: a provider is coming up or
///   Auto is still probing — covers `MediaPipe`'s model-load/camera-open window
///   and Auto's Leap device-grace, both of which the raw service axis misreads.
/// - `FellBackToMock` → [`ProviderStatusLine::NoTracking`]: Auto exhausted its
///   candidates and is running the silent mock; the app works, hands do not.
/// - `Failed` → [`ProviderStatusLine::Failed`]: will not recover without
///   intervention — honest red beats a stuck spinner.
/// - `Active` / `Inactive` → `None`: tracking is live, or `Off`; stay quiet.
///
/// [`HandTrackingActivation`]: crate::input::activation::HandTrackingActivation
fn provider_status_line(
    activation: crate::input::activation::HandTrackingActivation,
) -> Option<ProviderStatusLine> {
    use crate::input::activation::HandTrackingActivation as A;
    match activation {
        A::Settling => Some(ProviderStatusLine::Starting),
        A::FellBackToMock => Some(ProviderStatusLine::NoTracking),
        A::Failed => Some(ProviderStatusLine::Failed),
        A::Active | A::Inactive => None,
    }
}

/// Whether a backend label reports a *degraded* session: the accelerator was
/// requested and at least one of the two models fell back to the CPU EP.
///
/// Keyed on the `+CPU` suffix the provider's `combined_backend` builds
/// (`"ort/CoreML+CPU"`, `"ort/DirectML+CPU"`) rather than on those constants
/// directly, which live behind the `hand-tracking-mediapipe` feature gate and so
/// are not nameable from this always-compiled panel module. A whole-session
/// `"ort/CPU"` is *not* degraded by this test: on `ForceCpu` it is exactly what
/// the operator asked for, and on `Auto` it is the honest state of a host with no
/// GPU EP — neither is the half-broken case this row exists to make visible.
fn backend_is_degraded(label: &str) -> bool {
    label.ends_with("+CPU")
}

/// Render the widget half (Grid column 2) of the provider status row.
///
/// Cheap and allocation-free: static strings only, one small spinner while
/// starting (egui spinners self-animate; the egui pass runs every frame).
pub(super) fn render_provider_status_row(
    ui: &mut egui::Ui,
    line: ProviderStatusLine,
    style: &OverlayStyle,
) {
    match line {
        ProviderStatusLine::Starting => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(12.0));
                ui.weak("starting…");
            });
        }
        ProviderStatusLine::NoTracking => {
            ui.label(
                egui::RichText::new("no hand tracking (running idle)")
                    .size(11.0)
                    .color(style.warn_amber),
            );
        }
        ProviderStatusLine::Failed => {
            ui.label(
                egui::RichText::new("failed: see dev panel (Shift+D)")
                    .size(11.0)
                    .color(style.error_red),
            );
        }
    }
}

/// Render the widget half (Grid column 2) of the live-backend row: the EP the
/// sessions actually registered on, amber when it is a degraded mixed `+CPU`
/// label and in the panel's secondary text colour otherwise.
///
/// The label is a `&'static str` rendered as its own egui label beside a static
/// `"Running:"` — deliberately not `format!`ed into one string, which would
/// allocate on every frame the panel is open.
pub(super) fn render_backend_status_row(
    ui: &mut egui::Ui,
    backend: &'static str,
    style: &OverlayStyle,
) {
    let (color, hover) = if backend_is_degraded(backend) {
        (
            style.warn_amber,
            "A model's GPU execution provider failed at load and that model fell back to the CPU. \
             Hand tracking still works, but it is running hotter and slower than a healthy GPU \
             session. See the log (or the dev panel, Shift+D) for which model degraded.",
        )
    } else {
        (
            style.text_secondary,
            "The execution provider the hand-tracking models actually loaded on \
             (the dropdown above is the request; this is the result).",
        )
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Running:").size(11.0).weak());
        ui.label(egui::RichText::new(backend).size(11.0).color(color));
    })
    .response
    .on_hover_text(hover);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status rows key on `HandTrackingSettings`'s actual storage key and
    /// the `provider` / `backend` field names; if any is renamed without
    /// updating these consts, the rows silently stop rendering.
    #[test]
    fn provider_status_row_keys_match_the_settings_struct() {
        use crate::settings::SketchSettings;
        assert_eq!(ProviderStatusLine::STORAGE_KEY, "hand_tracking");
        let defs = crate::settings::HandTrackingSettings::settings_def();
        for field in [ProviderStatusLine::FIELD_NAME, BACKEND_FIELD_NAME] {
            assert!(
                defs.iter().any(|d| d.field_name == field),
                "HandTrackingSettings has no `{field}` field — update the status-row consts"
            );
        }
    }

    /// Every activation state maps to the right row: settling spins, mock
    /// fallback warns amber, failure warns red, live/off show nothing.
    #[test]
    fn provider_status_line_maps_every_activation_state() {
        use crate::input::activation::HandTrackingActivation as A;
        use ProviderStatusLine::{Failed, NoTracking, Starting};
        for (activation, expected) in [
            (A::Settling, Some(Starting)),
            (A::FellBackToMock, Some(NoTracking)),
            (A::Failed, Some(Failed)),
            (A::Active, None),
            (A::Inactive, None),
        ] {
            assert_eq!(provider_status_line(activation), expected, "{activation:?}");
        }
    }

    /// An absent activation resource (headless test) and the `Inactive` default
    /// (provider `Off`) both render no status row — neither may read as an
    /// eternal "starting…" spinner. The backend half is likewise absent with no
    /// registry.
    #[test]
    fn provider_status_snapshot_is_none_when_inactive_or_absent() {
        use crate::input::activation::HandTrackingActivation;
        let mut world = World::new();
        assert_eq!(
            hand_tracking_status_snapshot(&world),
            HandTrackingStatus::default(),
            "absent resources"
        );
        world.insert_resource(HandTrackingActivation::Inactive);
        assert_eq!(
            hand_tracking_status_snapshot(&world).provider,
            None,
            "inactive"
        );
        world.insert_resource(HandTrackingActivation::Settling);
        assert_eq!(
            hand_tracking_status_snapshot(&world).provider,
            Some(ProviderStatusLine::Starting),
            "settling spins"
        );
    }

    /// The amber test is the mixed-session one: a model that fell back to the
    /// CPU while the other kept the accelerator. A uniform session — CPU-only
    /// (asked for, or no GPU EP on the host) or fully accelerated — is not
    /// degraded, and an all-day CPU soak masquerading as a healthy kiosk is
    /// exactly what the amber exists to prevent.
    #[test]
    fn only_a_mixed_cpu_backend_reads_as_degraded() {
        for degraded in ["ort/CoreML+CPU", "ort/DirectML+CPU"] {
            assert!(
                backend_is_degraded(degraded),
                "{degraded} must show amber — one model is on the CPU"
            );
        }
        for healthy in ["ort/CoreML", "ort/DirectML", "ort/CPU", "ort/mixed"] {
            assert!(
                !backend_is_degraded(healthy),
                "{healthy} is a uniform session, not a degraded one"
            );
        }
    }

    /// The backend row must survive a real egui `Grid` — the same
    /// grid-safety trap `render_reset_cell` hit (`ui.add_space` panics inside a
    /// grid) — in both the amber and normal styles.
    #[test]
    fn backend_status_row_renders_inside_a_grid() {
        let ctx = egui::Context::default();
        let style = OverlayStyle::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::Grid::new("backend_row_test")
                .num_columns(3)
                .show(ui, |ui| {
                    for backend in ["ort/CoreML", "ort/CoreML+CPU"] {
                        ui.label("");
                        render_backend_status_row(ui, backend, &style);
                        ui.label("");
                        ui.end_row();
                    }
                });
        });
        // Reaching here without a panic is the assertion.
    }
}
