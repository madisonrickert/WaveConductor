//! Radiance sketch: webcam-tracked dancers rendered as dark glassy forms
//! with emissive per-body-colored rims, each wrapped in a turbulent particle
//! FLAME — additive HDR grains born on the silhouette edge, white-hot at
//! birth, cooling through the body's hue identity into dim embers, rising in
//! tongue-modulated buoyant plumes, streaking outward as fast ejecta on
//! audio onsets, and driven by curl-noise flow, limb motion, and live audio
//! input. Up to four dancers burn simultaneously, each with a distinct hue
//! derived from the active palette; the particle budget is shared (fade-
//! weighted) so density stays constant as people come and go. Radiance does
//! not generate audio; it listens (Plan A's input analysis) and watches
//! (Plan B's body tracking).
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
//! camera's tonemapping/bloom while Radiance is active. During the
//! screensaver, `screensaver::RadianceScreensaverPlugin` takes over the mask
//! and edge writes with a synthetic phantom performer and bakes a
//! fade-scaled ember frame through the same `bake_radiance_sim` baker the
//! live writer uses (Task 12). `WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_BODY`
//! (Task 13, debug builds) drives the same mask/edge/body/audio surfaces
//! from the deterministic synthetic dancer instead, and `spawn`'s
//! `insert_tracking_requests` skips the mic/camera activation requests
//! entirely under that toggle so a capture run never opens hardware; the
//! edge-point gizmo overlay and the egui inference readout are always-on
//! dev overlays, self-gated on `RadianceSettings`'s Dev bools.
//!
//! Everything above except camera arbitration (`systems::arbitration`, which
//! consumes only the unconditional `wc_core::input::provider`) is gated
//! behind the `body-tracking-mediapipe` feature: it consumes
//! `wc_core::input::body`, which wc-core gates behind the same name. `build`
//! below gates each registration identically and stays the single source of
//! truth for wiring order.

pub mod compute;
// Silhouette chamfer distance field (feeds the beat-wave shader). Consumes
// `wc_core::input::body` (mask + edge generation), so it is gated like
// `synthetic`/`screensaver` below (same `cargo doc` default-features-only
// rationale).
#[cfg(feature = "body-tracking-mediapipe")]
pub mod distance_field;
// Beat-pulse layer (waves of light radiating from the silhouette's edge on
// detected beats). Consumes `distance_field` above, gated identically.
#[cfg(feature = "body-tracking-mediapipe")]
pub mod pulse;
pub mod render;
pub mod settings;
// Extremity sparkle motes (per-body constellations riding the
// fastest-oscillating limbs). Consumes `wc_core::input::body` landmarks,
// gated identically.
#[cfg(feature = "body-tracking-mediapipe")]
pub mod sparkle;
// Consumes `wc_core::input::body` (`EdgePoint`/`MASK_SIZE`/`MAX_EDGE_POINTS`),
// which wc-core gates behind this feature (camera-independent, CI-testable
// headless). One generator, three consumers (unit tests here, the Task 12
// screensaver phantom, and the Task 13 capture dancer), so it lives at the
// sketch root beside `systems` rather than inside it. The `cargo doc` gate
// builds default features only, so this module must be absent there — see
// `Cargo.toml`'s `body-tracking-mediapipe` forwarding feature, and
// `radiance::systems::mod`/`radiance::compute::mod` for the identical
// precedent.
#[cfg(feature = "body-tracking-mediapipe")]
pub mod synthetic;
pub mod systems;
// Attract-mode phantom performer: consumes `wc_core::input::body`
// (`MaskTexture`/`SilhouetteEdges`) plus `synthetic` and `systems::sim_params`
// above, both of which are already gated behind this feature. Same
// `cargo doc` default-features-only rationale as `synthetic`/`systems::spawn`.
#[cfg(feature = "body-tracking-mediapipe")]
pub mod screensaver;

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

        // Silhouette distance field: recomputed per body frame
        // (generation-gated), consumed by the beat-wave shader. Ordered
        // before the pulse driver so a fresh mask's field is live the same
        // frame its wave brightness updates.
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_systems(
            Update,
            distance_field::update_distance_field
                .before(pulse::update_radiance_pulses)
                .run_if(in_state(AppState::Radiance)),
        );

        // Beat-pulse driver: spawns silhouette-contour waves on the analysis
        // engine's beat lane and packs the wave uniform. Gated `in_state`
        // like the material driver so residual waves keep fading through
        // Idle/Screensaver (no new waves spawn there — the mic is paused
        // and beat_confidence holds 0).
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_systems(
            Update,
            pulse::update_radiance_pulses.run_if(in_state(AppState::Radiance)),
        );

        // Extremity-sparkle driver: oscillation tracker + mirrored star
        // pair. Same gating rationale as the pulse driver.
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_systems(
            Update,
            sparkle::update_radiance_sparkles.run_if(in_state(AppState::Radiance)),
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

        // Attract performer: phantom silhouette + ember sim writer, both
        // gated in_screensaver (zero systems otherwise). Gated: consumes
        // `wc_core::input::body` via `screensaver`.
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_plugins(screensaver::RadianceScreensaverPlugin);

        // Dev overlays: edge gizmos (settings-gated internally) + readouts
        // (self-gated egui pass system, flame's overlay idiom). Gated:
        // both consume `wc_core::input::body` via `systems::debug`.
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_systems(
            Update,
            systems::debug::draw_edge_debug
                .run_if(wc_core::sketch::sketch_active(AppState::Radiance)),
        );
        // Person-cycle hotkey (KeyN): cycle the tracked dancer. Active only
        // while the Radiance sketch is running (not the screensaver).
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_systems(
            Update,
            systems::debug::cycle_person_hotkey
                .run_if(wc_core::sketch::sketch_active(AppState::Radiance)),
        );
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_systems(
            bevy_egui::EguiPrimaryContextPass,
            systems::debug::radiance_inference_readout,
        );

        // Capture dancer: debug builds, only under the synthetic-body toggle,
        // ordered before the live baker so its resources win this frame.
        #[cfg(all(feature = "body-tracking-mediapipe", debug_assertions))]
        app.add_systems(
            Update,
            systems::debug::drive_synthetic_body
                .before(systems::sim_params::update_radiance_sim)
                .run_if(wc_core::sketch::sketch_active(AppState::Radiance))
                .run_if(systems::debug::synthetic_body_forced),
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
