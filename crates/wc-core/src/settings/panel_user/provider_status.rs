//! Hand-tracking provider status row shown under the "Tracking provider"
//! dropdown on the settings panel's Hand Tracking tab.
//!
//! [`ProviderStatusLine`] is derived from the live
//! [`crate::input::activation::HandTrackingActivation`] cue (not the
//! dropdown's *selected* enum value) by [`provider_status_line`], so the row
//! reports whether tracking is actually live rather than what the operator
//! last picked. [`provider_status_snapshot`] takes the pre-render snapshot
//! and [`render_provider_status_row`] draws it; both are called from
//! [`super::fields`].

use bevy::prelude::World;
use bevy_egui::egui;

use crate::ui::OverlayStyle;

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
    /// ([`crate::settings::HandTrackingSettings`]).
    pub(super) const STORAGE_KEY: &'static str =
        <crate::settings::HandTrackingSettings as crate::settings::SketchSettings>::STORAGE_KEY;
    /// Field name of the provider dropdown within that struct.
    pub(super) const FIELD_NAME: &'static str = "provider";
}

/// Snapshot the hand-tracking activation cue as a status-row verdict. `None`
/// (no row) when the activation resource is absent (headless tests), when
/// tracking is inactive (provider `Off`), or when a real provider is live.
pub(super) fn provider_status_snapshot(world: &World) -> Option<ProviderStatusLine> {
    let activation = world
        .get_resource::<crate::input::activation::HandTrackingActivation>()
        .copied()?;
    provider_status_line(activation)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The status row keys on `HandTrackingSettings`'s actual storage key and
    /// the `provider` field's actual name; if either is renamed without
    /// updating [`ProviderStatusLine`], the row silently stops rendering.
    #[test]
    fn provider_status_row_keys_match_the_settings_struct() {
        use crate::settings::SketchSettings;
        assert_eq!(ProviderStatusLine::STORAGE_KEY, "hand_tracking");
        assert!(
            crate::settings::HandTrackingSettings::settings_def()
                .iter()
                .any(|d| d.field_name == ProviderStatusLine::FIELD_NAME),
            "HandTrackingSettings has no `{}` field — update ProviderStatusLine::FIELD_NAME",
            ProviderStatusLine::FIELD_NAME
        );
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
    /// eternal "starting…" spinner.
    #[test]
    fn provider_status_snapshot_is_none_when_inactive_or_absent() {
        use crate::input::activation::HandTrackingActivation;
        let mut world = World::new();
        assert_eq!(provider_status_snapshot(&world), None, "absent resource");
        world.insert_resource(HandTrackingActivation::Inactive);
        assert_eq!(provider_status_snapshot(&world), None, "inactive");
        world.insert_resource(HandTrackingActivation::Settling);
        assert_eq!(
            provider_status_snapshot(&world),
            Some(ProviderStatusLine::Starting),
            "settling spins"
        );
    }
}
