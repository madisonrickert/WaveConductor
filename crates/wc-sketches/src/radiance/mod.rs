//! Radiance sketch: a webcam-tracked dancer's silhouette rendered as a dark
//! glassy form with an emissive rim, wrapped in an aura of additive HDR
//! particles born on the silhouette edge and driven by curl-noise flow,
//! buoyancy, limb motion, and live audio input. Radiance does not generate
//! audio; it listens (Plan A's input analysis) and watches (Plan B's body
//! tracking).
//!
//! ## Data flow
//!
//! `OnEnter`: suspend the `MediaPipe` *hand* camera → ensure the mask + edge
//! surfaces exist → spawn buffers/quads + sim resources → insert the mic +
//! body activation requests. Per frame while Active: `update_radiance_sim`
//! bakes `AudioAnalysis` + `BodyTrackingState` + `SilhouetteEdges` into
//! `RadianceSimParams`; the render world extracts it, uploads edges
//! generation-gated, and dispatches the aura kernel before the 2D pass draws
//! the billboards over the silhouette quad. `drive_radiance_materials` runs
//! through Idle/Screensaver (`in_state`) so the ember blend keeps rendering.
//! Activity seams pause/resume the mic + body requests; Idle also zeroes
//! emission (the freeze idiom); `OnExit` tears everything down and schedules
//! the deferred hand-camera restore. Settings register with the shared
//! panel/persistence system; the `RenderProfile` applier drives the main
//! camera's tonemapping/bloom while Radiance is active. The screensaver
//! phantom and debug/capture drivers arrive in Plan C Tasks 12–13.
//!
//! Everything above except camera arbitration (`systems::arbitration`, which
//! consumes only the unconditional `wc_core::input::provider`) is gated
//! behind the `body-tracking-mediapipe` feature: it consumes
//! `wc_core::input::body`, which wc-core gates behind the same name. `build`
//! below gates each registration identically and stays the single source of
//! truth for wiring order.

pub mod compute;
pub mod render;
pub mod settings;
pub mod systems;

use bevy::prelude::*;
use wc_core::lifecycle::state::AppState;
use wc_core::settings::RegisterSketchSettingsExt;

/// Plugin that registers the Radiance sketch.
pub struct RadiancePlugin;

impl Plugin for RadiancePlugin {
    // The registration list is the single source of truth for wiring order
    // (flame's convention); splitting it would scatter it. Each block below
    // is gated `#[cfg(feature = "body-tracking-mediapipe")]` exactly where
    // the systems it registers are (see the module doc); camera arbitration
    // and the four settings/profile listeners are unconditional.
    #[allow(clippy::too_many_lines)]
    fn build(&self, app: &mut App) {
        // Settings: panel + persistence (storage key "radiance").
        app.register_sketch_settings::<settings::RadianceSettings>();

        // Picker-tile manifest entry (async screenshot load).
        register_radiance_manifest(app);

        // OnEnter, part 1: arbitration (release the webcam). Unconditional —
        // it only consumes `wc_core::input::provider`.
        app.add_systems(
            OnEnter(AppState::Radiance),
            systems::arbitration::suspend_mediapipe_hand_camera,
        );
        // OnEnter, part 2: surfaces, then spawn (reads MaskTexture), then
        // requests. Gated: all three consume `wc_core::input::body`.
        // `.after(...)` keeps entry ordering (webcam released before the
        // body worker needs it) without merging the two registrations into
        // one schedule addition — `suspend_mediapipe_hand_camera` is still
        // added to `OnEnter(AppState::Radiance)` exactly once, above.
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_systems(
            OnEnter(AppState::Radiance),
            (
                systems::spawn::ensure_body_surfaces,
                systems::spawn::spawn_radiance,
                systems::spawn::insert_tracking_requests,
            )
                .chain()
                .after(systems::arbitration::suspend_mediapipe_hand_camera),
        );

        // OnExit, part 1: despawn the sketch's entities and drop its
        // resources. Gated: `RadianceRoot` and `remove_radiance_resources`
        // live in `systems::spawn`.
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_systems(
            OnExit(AppState::Radiance),
            (
                wc_core::sketch::despawn_with::<systems::spawn::RadianceRoot>,
                systems::spawn::remove_radiance_resources,
            ),
        );
        // OnExit, part 2: schedule the deferred hand-camera restore and
        // reset the render profile. Unconditional.
        app.add_systems(
            OnExit(AppState::Radiance),
            (
                systems::arbitration::schedule_hand_camera_restore,
                wc_core::sketch::reset_render_profile,
            ),
        );
        // Deferred hand-camera restore: always-on, one Option branch when
        // idle (see its module docs for the sanctioned-listener rationale).
        app.add_systems(Update, systems::arbitration::resume_hand_camera_when_due);

        // Live writer: the per-frame baker, Active only. Gated: consumes
        // `RadianceSimParams`/`RadianceState` from `systems::sim_params`.
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_systems(
            Update,
            systems::sim_params::update_radiance_sim
                .run_if(wc_core::sketch::sketch_active(AppState::Radiance)),
        );

        // Idle freeze: zero emission so the aura fades out and the throttled
        // last frames hold (flame's freeze idiom); pause the mic + body
        // requests on the same seam. Gated: both systems consume
        // body-tracking types.
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_systems(
            OnEnter(wc_core::lifecycle::state::SketchActivity::Idle),
            (
                systems::sim_params::freeze_radiance_emission,
                systems::activity::pause_tracking_requests,
            )
                .run_if(in_state(AppState::Radiance)),
        );
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_systems(
            OnEnter(wc_core::lifecycle::state::SketchActivity::Screensaver),
            systems::activity::pause_tracking_requests.run_if(in_state(AppState::Radiance)),
        );
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_systems(
            OnEnter(wc_core::lifecycle::state::SketchActivity::Active),
            systems::activity::resume_tracking_requests.run_if(in_state(AppState::Radiance)),
        );

        // Material driver: runs through Idle and the screensaver (in_state,
        // flame's drive_flame_material gating) so the ember blend and held
        // envelopes keep rendering. Gated: `drive_radiance_materials`
        // consumes `RadianceState` + `RadianceSilhouetteMaterial`.
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_systems(
            Update,
            render::drive_radiance_materials.run_if(in_state(AppState::Radiance)),
        );

        // Restart listener (requires_restart fields fade out/in via the
        // shared reload overlay). Always-on sanctioned listener.
        app.add_systems(
            Update,
            wc_core::sketch::restart_on_settings_change::<settings::RadianceSettings>,
        );

        // Re-run the spawn path at the new window size when a resize settles
        // (silent/instant reload). Always-on sanctioned listener; defensive
        // add_message mirrors FlamePlugin (Bevy dedups; LifecyclePlugin is
        // canonical).
        app.add_message::<wc_core::lifecycle::window_resize::WindowResizeSettled>();
        app.add_systems(
            Update,
            wc_core::sketch::reload_on_resize_settled::<settings::RadianceSettings>,
        );

        // Tonemapping + bloom profile onto the main camera while Radiance is
        // up (live dev-panel tuning), via the shared generic applier.
        app.add_systems(
            Update,
            wc_core::sketch::apply_render_profile::<settings::RadianceSettings>
                .run_if(in_state(AppState::Radiance)),
        );
    }
}

/// Register Radiance's picker-tile metadata. Factored out of
/// `RadiancePlugin::build` so it is unit-testable without rendering plugins
/// (mirrors `register_flame_manifest`).
pub(crate) fn register_radiance_manifest(app: &mut App) {
    use wc_core::settings::SketchSettings as _;
    wc_core::sketch::register_sketch_tile(
        app,
        AppState::Radiance,
        "Radiance",
        settings::RadianceSettings::STORAGE_KEY,
        "sketches/radiance/screenshot.png",
    );
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use wc_core::sketch::SketchManifest;

    /// Mirrors `register_flame_manifest_appends_entry`: the free-function path
    /// registers a Radiance tile without needing a `RenderApp`.
    #[test]
    fn register_radiance_manifest_appends_entry() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::asset::AssetPlugin::default());
        app.add_plugins(bevy::image::ImagePlugin::default());
        register_radiance_manifest(&mut app);
        let manifest = app.world().resource::<SketchManifest>();
        let entry = manifest
            .get(AppState::Radiance)
            .expect("Radiance manifest entry should be registered");
        assert_eq!(entry.display_name, "Radiance");
        assert_eq!(entry.settings_key, "radiance");
    }
}
