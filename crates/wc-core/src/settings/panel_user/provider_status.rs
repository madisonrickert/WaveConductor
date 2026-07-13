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
//!   whenever the session is degraded ([`backend_degradation`]), so a degraded
//!   kiosk *looks* degraded where the operator is already looking.
//!
//! All three inputs to that verdict are read from `Copy` / `&'static str` state
//! (see [`crate::input::provider::HandTrackingProvider::backend_label`] and
//! [`crate::input::provider::HandTrackingProvider::backend_request`]) — no
//! allocation, no lock — because the panel re-snapshots them every frame it is
//! open.

use bevy::prelude::World;
use bevy_egui::egui;

use crate::settings::HandTrackingBackend;
use crate::ui::OverlayStyle;

/// The Hand Tracking tab's two live status rows, snapshotted out of the `World`
/// before the egui closure borrows it (see [`hand_tracking_status_snapshot`]).
///
/// `Copy`: `Option`s over fieldless enums and a `&'static str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct HandTrackingStatus {
    /// Row under the "Tracking provider" dropdown. `None` = no row (live, or off).
    pub(super) provider: Option<ProviderStatusLine>,
    /// Row under the "Inference backend" dropdown: the backend label the primary
    /// provider actually registered. `None` = no row (no inference provider, or
    /// its sessions have not started).
    pub(super) backend: Option<&'static str>,
    /// How that label reads against what was *asked for* on *this* platform —
    /// the amber verdict, resolved once at snapshot time by
    /// [`backend_degradation`] so the render half stays a pure style choice.
    pub(super) degradation: BackendDegradation,
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
    let registry = world.get_resource::<crate::input::provider::ProviderRegistry>();
    let backend =
        registry.and_then(crate::input::provider::ProviderRegistry::primary_backend_label);
    // The request that produced that label, taken from the provider (what its
    // running sessions were *built* with), never from the live dropdown — see
    // `HandTrackingProvider::backend_request`.
    let request =
        registry.and_then(crate::input::provider::ProviderRegistry::primary_backend_request);
    let degradation = match (request, backend) {
        (Some(request), Some(backend)) => backend_degradation(
            request,
            crate::input::provider::platform_has_gpu_execution_provider(),
            backend,
        ),
        _ => BackendDegradation::None,
    };
    HandTrackingStatus {
        provider,
        backend,
        degradation,
    }
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

/// How far a hand-tracking session has fallen from the accelerator it asked for.
///
/// Two degraded states, not one, because the thermal cost differs by roughly a
/// factor of two and so does the urgency: one conv-heavy model at 30 Hz on the CPU
/// EP is a warm kiosk, both of them is the "240% CPU" fanless-hardware symptom.
/// The operator must be able to tell them apart from the row alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum BackendDegradation {
    /// Not degraded: the session got what was asked for (or CPU *was* what was
    /// asked for, or there was never an accelerator on this host to get).
    #[default]
    None,
    /// One model runs on the accelerator, the other fell back to the CPU EP
    /// (a mixed `"ort/CoreML+CPU"` / `"ort/DirectML+CPU"` label).
    OneModelOnCpu,
    /// **Both** models run on the CPU EP on a host that has an accelerator and was
    /// asked to use it (a whole-session `"ort/CPU"` label under `Auto`/`ForceGpu`).
    /// The most expensive degradation, and the one that used to be invisible: it
    /// carries no `+CPU` suffix, so a suffix test read it as a healthy CPU-only
    /// host and stayed silent.
    BothModelsOnCpu,
}

/// The amber verdict, as a pure function of the only three things it depends on —
/// so it is unit-testable with no GPU EP present (there are none in CI), exactly
/// like `MediaPipe`'s own `ep_plan` and `should_demote_to_cpu`.
///
/// Degradation is **not** "the label ends in `+CPU`". It is *the operator asked for
/// an accelerator, this platform can provide one, and we ended up on the CPU
/// anyway*:
///
/// - `request`: [`HandTrackingBackend::ForceCpu`] can never be degraded — a CPU
///   session is precisely what it asked for.
/// - `platform_has_accelerator`: on a host with no GPU EP compiled in (Linux),
///   `"ort/CPU"` is the only outcome that ever existed. Flagging it would cry wolf
///   on every Linux dev machine, and an amber row that is always on is an amber row
///   nobody reads.
/// - `label`: the result. Matched on the `+CPU` suffix and the `"ort/CPU"` label
///   rather than on the provider's constants, which live behind the
///   `hand-tracking-mediapipe` feature gate and so are not nameable from this
///   always-compiled panel module.
fn backend_degradation(
    request: HandTrackingBackend,
    platform_has_accelerator: bool,
    label: &str,
) -> BackendDegradation {
    let accelerator_requested = match request {
        HandTrackingBackend::Auto | HandTrackingBackend::ForceGpu => true,
        HandTrackingBackend::ForceCpu => false,
    };
    if !accelerator_requested || !platform_has_accelerator {
        return BackendDegradation::None;
    }
    if label.ends_with("+CPU") {
        BackendDegradation::OneModelOnCpu
    } else if label == CPU_ONLY_LABEL {
        BackendDegradation::BothModelsOnCpu
    } else {
        BackendDegradation::None
    }
}

/// The whole-session CPU backend label (`MediaPipe`'s `inference_ort::BACKEND_CPU`,
/// re-stated here because that const is behind the `hand-tracking-mediapipe`
/// feature gate; a test in that module pins the two together).
const CPU_ONLY_LABEL: &str = "ort/CPU";

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
/// sessions actually registered on, amber when the session is degraded
/// ([`backend_degradation`]) and in the panel's secondary text colour otherwise.
///
/// A degraded row also carries a static note naming *how many* models are on the
/// CPU, because `"ort/CPU"` alone cannot say it: the label of a both-models
/// degradation is byte-identical to the label of a healthy CPU-only host, and the
/// two are hours of extra heat apart.
///
/// Every string here is `&'static` and rendered as its own egui label beside a
/// static `"Running:"` — deliberately not `format!`ed into one string, which would
/// allocate on every frame the panel is open.
pub(super) fn render_backend_status_row(
    ui: &mut egui::Ui,
    backend: &'static str,
    degradation: BackendDegradation,
    style: &OverlayStyle,
) {
    let (color, note, hover) = match degradation {
        BackendDegradation::None => (
            style.text_secondary,
            "",
            "The execution provider the hand-tracking models actually loaded on \
             (the dropdown above is the request; this is the result).",
        ),
        BackendDegradation::OneModelOnCpu => (
            style.warn_amber,
            "· 1 of 2 models degraded",
            "One model's GPU execution provider failed and that model fell back to the CPU; the \
             other still runs on the accelerator. Hand tracking works, but hotter and slower than \
             a healthy GPU session. See the log (or the dev panel, Shift+D) for which model \
             degraded.",
        ),
        BackendDegradation::BothModelsOnCpu => (
            style.warn_amber,
            "· BOTH models degraded",
            "Both hand-tracking models are running on the CPU even though this machine has a GPU \
             execution provider and was asked to use it. This is the most expensive degradation: \
             two conv-heavy models at 30 Hz on the CPU is a sustained thermal load on fanless \
             hardware. See the log (or the dev panel, Shift+D) for why the accelerator was lost, \
             and relaunch to retry it.",
        ),
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Running:").size(11.0).weak());
        ui.label(egui::RichText::new(backend).size(11.0).color(color));
        if !note.is_empty() {
            ui.label(egui::RichText::new(note).size(11.0).color(color));
        }
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

    /// The amber verdict across every (request × platform × result) combination
    /// that can occur.
    ///
    /// The load-bearing row is `Auto` + an accelerator-capable host + `"ort/CPU"`:
    /// **both** models on the CPU. It carries no `+CPU` suffix, so the old
    /// suffix-only test read it as a healthy CPU-only host and showed nothing —
    /// leaving the *most* thermally expensive failure (two conv-heavy models at
    /// 30 Hz on the CPU EP of a fanless kiosk) looking identical to a Linux box
    /// that never had a GPU EP. The three must-not-be-amber rows guard the
    /// opposite error: an amber row that cries wolf is one nobody reads.
    #[test]
    fn backend_degradation_across_request_platform_and_result() {
        use BackendDegradation::{BothModelsOnCpu, None, OneModelOnCpu};
        use HandTrackingBackend::{Auto, ForceCpu, ForceGpu};
        // (request, platform has an accelerator, resulting label, verdict, why)
        let cases = [
            // The bug: both models degraded on a machine that has an accelerator.
            (Auto, true, "ort/CPU", BothModelsOnCpu, "both models on CPU"),
            (
                ForceGpu,
                true,
                "ort/CPU",
                BothModelsOnCpu,
                "GPU forced, CPU delivered",
            ),
            // One model degraded (the mixed state that already worked).
            (
                Auto,
                true,
                "ort/CoreML+CPU",
                OneModelOnCpu,
                "one model on CPU",
            ),
            (
                Auto,
                true,
                "ort/DirectML+CPU",
                OneModelOnCpu,
                "one model on CPU",
            ),
            // Healthy: the accelerator was asked for and delivered.
            (Auto, true, "ort/CoreML", None, "healthy CoreML"),
            (Auto, true, "ort/DirectML", None, "healthy DirectML"),
            (ForceGpu, true, "ort/CoreML", None, "healthy forced GPU"),
            // Must NOT be amber: the operator asked for the CPU.
            (
                ForceCpu,
                true,
                "ort/CPU",
                None,
                "ForceCpu got what it asked",
            ),
            (ForceCpu, false, "ort/CPU", None, "ForceCpu, no accelerator"),
            // Must NOT be amber: no accelerator exists on this host (Linux), so
            // the CPU is not a degradation, it is the only outcome there was.
            (Auto, false, "ort/CPU", None, "no GPU EP on this platform"),
            (
                ForceGpu,
                false,
                "ort/CPU",
                None,
                "no GPU EP on this platform",
            ),
        ];
        for (request, has_accel, label, expected, why) in cases {
            assert_eq!(
                backend_degradation(request, has_accel, label),
                expected,
                "{request:?} + accelerator={has_accel} + {label} ({why})"
            );
        }
    }

    /// The backend row must survive a real egui `Grid` — the same
    /// grid-safety trap `render_reset_cell` hit (`ui.add_space` panics inside a
    /// grid) — in the normal style and in both amber styles.
    #[test]
    fn backend_status_row_renders_inside_a_grid() {
        let ctx = egui::Context::default();
        let style = OverlayStyle::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::Grid::new("backend_row_test")
                .num_columns(3)
                .show(ui, |ui| {
                    for (backend, degradation) in [
                        ("ort/CoreML", BackendDegradation::None),
                        ("ort/CoreML+CPU", BackendDegradation::OneModelOnCpu),
                        ("ort/CPU", BackendDegradation::BothModelsOnCpu),
                    ] {
                        ui.label("");
                        render_backend_status_row(ui, backend, degradation, &style);
                        ui.label("");
                        ui.end_row();
                    }
                });
        });
        // Reaching here without a panic is the assertion.
    }
}
