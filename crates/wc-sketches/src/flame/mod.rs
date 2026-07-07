//! Flame sketch: a name-seeded IFS fractal flame, evaluated level-parallel on
//! the GPU and drawn as an additive point cloud with a fake depth of field.
//!
//! ## Data flow (grows stage by stage; see the 2026-07-02 flame port plan)
//!
//! 1. `OnEnter(AppState::Flame)` swaps the clear color to v4's `#10101f`
//!    (stashing the previous value) — the fog fades points into this color.
//! 2. Settings register with the shared panel/persistence system; the
//!    `RenderProfile` applier drives the main camera's tonemapping/bloom
//!    while Flame is active.
//! 3. `OnExit` restores the clear color and resets the render profile.
//!
//! Simulation, rendering, interaction, audio, and the attract performer are
//! wired in later stages of the port plan.

pub mod audio_coupling;
pub mod branches;
pub mod compute;
pub mod levels;
pub mod render;
pub mod screensaver;
pub mod settings;
pub mod systems;
pub mod ui;

use bevy::prelude::*;
use wc_core::lifecycle::state::AppState;
use wc_core::lifecycle::RegisterIdleVetoExt;
use wc_core::settings::RegisterSketchSettingsExt;

/// Plugin that registers the Flame sketch.
pub struct FlamePlugin;

impl Plugin for FlamePlugin {
    // The registration list grows one stage at a time as the port plan adds
    // sim/render/interaction/audio/attract sub-systems (see the module docs);
    // splitting it would scatter the single source of truth for wiring order.
    #[allow(clippy::too_many_lines)]
    fn build(&self, app: &mut App) {
        // Settings: panel + persistence (storage key "flame").
        app.register_sketch_settings::<settings::FlameSettings>();

        // Picker-tile manifest entry (async screenshot load).
        register_flame_manifest(app);

        // v4 scene background: #10101f. The whole sketch reads against it
        // (fog fades points toward it), so it is swapped at the state seam.
        // `spawn_flame` allocates the node buffer + inserts the sim resources
        // on entry; `remove_flame_resources` drops them (releasing VRAM) and
        // `despawn_with::<FlameRoot>` tears down the sketch's entities on exit.
        // `enter_flame_audio` reads the freshly-inserted `FlameState` to push
        // the initial name-derived audio config, so it must run after
        // `spawn_flame`'s command flush — hence `.chain()`.
        app.add_systems(
            OnEnter(AppState::Flame),
            (
                systems::spawn::spawn_flame,
                enter_flame_clear_color,
                audio_coupling::enter_flame_audio,
            )
                .chain(),
        );
        app.add_systems(
            OnExit(AppState::Flame),
            (
                wc_core::sketch::despawn_with::<systems::spawn::FlameRoot>,
                systems::spawn::remove_flame_resources,
                exit_flame_clear_color,
                wc_core::sketch::reset_render_profile,
                audio_coupling::exit_flame_audio,
            ),
        );

        // Name/point-budget watcher: rebuilds the fractal on change. Gated on
        // the state (not `sketch_active`) so the screensaver carousel's name
        // changes are picked up while the sketch is idle.
        app.add_systems(
            Update,
            systems::name_change::watch_flame_name.run_if(in_state(AppState::Flame)),
        );
        // Per-frame writer: virtual-time cX oscillation + pointer warp, then the
        // single baker. Ordered after the watcher so it bakes the fresh spec.
        app.add_systems(
            Update,
            systems::sim_params::update_flame_sim
                .after(systems::name_change::watch_flame_name)
                .run_if(wc_core::sketch::sketch_active(AppState::Flame)),
        );
        // Idle freeze: zero the dispatch count so the compute pass idles while
        // the sketch is frozen (v4 froze on idle too).
        app.add_systems(
            OnEnter(wc_core::lifecycle::state::SketchActivity::Idle),
            systems::sim_params::freeze_flame_sim.run_if(in_state(AppState::Flame)),
        );

        // Orbit camera: autorotate + drag + wheel zoom + fling momentum decay.
        // Runs while Active OR during the screensaver — autorotate is the
        // screensaver's motion, drag/zoom input is simply inert there. Registered
        // ONCE with a combined run condition (not twice): a system added more
        // than once has an ambiguous `SystemTypeSet`, which cannot be used as a
        // `.before`/`.after` ordering target — and `drive_flame_audio` and
        // `update_flame_hands` below order against this system. `.or_else` is the
        // non-deprecated run-condition `or` combinator in this Bevy version.
        app.init_resource::<systems::camera::FlameCamera>();
        // `update_flame_camera` reads `PointerOverUi` as its egui-vs-scene input
        // guard, but that resource is normally supplied by wc-core's overlay-
        // buttons plugin (inside the UI plugin), not by FlamePlugin. Defensively
        // init it here (idempotent — the UI plugin's own init and maintaining
        // system win in production) so a harness that adds FlamePlugin without
        // the UI plugin can't panic on the first tick.
        app.init_resource::<wc_core::input::pointer::PointerOverUi>();
        app.add_systems(
            Update,
            systems::camera::update_flame_camera.run_if(
                wc_core::sketch::sketch_active(AppState::Flame).or_else(
                    wc_core::lifecycle::screensaver::in_screensaver(AppState::Flame),
                ),
            ),
        );

        // Audio coupling: two per-frame scalars (morph-energy, camera
        // distance) drive the FlameSynth voice. Runs while Active OR during the
        // screensaver — the screensaver's autorotate + carousel keep the fractal
        // morphing and the audio should track it there too — ordered after the
        // camera update so `FlameCamera::distance` reflects this frame's
        // zoom/autorotate before it is pushed. Registered ONCE with a combined
        // run condition, for the same ambiguous-`SystemTypeSet` reason as the
        // camera above.
        app.init_resource::<audio_coupling::FlameMorphEnergy>();
        app.init_resource::<audio_coupling::FlameChordDegreeCache>();
        app.add_systems(
            Update,
            audio_coupling::drive_flame_audio
                .after(systems::camera::update_flame_camera)
                .run_if(wc_core::sketch::sketch_active(AppState::Flame).or_else(
                    wc_core::lifecycle::screensaver::in_screensaver(AppState::Flame),
                )),
        );

        // Hand grab-and-fling: gathers grabbing hands and drives the orbit
        // camera the way a mouse drag does, writing `FlameGrabState.warp_px`
        // (F7's `update_flame_sim` maps it into the fractal warp every frame).
        // Ordered before the camera update so this frame's grab delta lands
        // before autorotate/momentum apply it.
        app.init_resource::<systems::hands::FlameGrabState>();
        app.add_systems(
            Update,
            systems::hands::update_flame_hands
                .before(systems::camera::update_flame_camera)
                .run_if(wc_core::sketch::sketch_active(AppState::Flame)),
        );
        // Idle veto: stay Active through a released fling's coast-down and
        // while a hand is actively grabbing (mirrors `dots::dots_idle_veto`).
        app.register_idle_veto(systems::hands::flame_idle_veto);

        // Hand-mesh overlay: warm amber #ffb84d, the flame-palette
        // counterpart to Dots' ice blue.
        app.add_plugins(crate::hand_mesh::HandMeshPlugin {
            config: crate::hand_mesh::HandMeshConfig {
                app_state: AppState::Flame,
                bone_color: Color::srgb(
                    f32::from(0xff_u8) / 255.0,
                    f32::from(0xb8_u8) / 255.0,
                    f32::from(0x4d_u8) / 255.0,
                ),
                glow_intensity: 5.0,
                bone_radius: 10.0,
            },
        });

        // Per-frame material driver: packs settings + FlameState into the eight
        // render uniforms. Gated on the state (not `sketch_active`) so the
        // camera/material keep updating during Idle and the screensaver, like
        // `drive_dots_master_brightness`.
        app.add_systems(
            Update,
            render::drive_flame_material.run_if(in_state(AppState::Flame)),
        );

        // Tonemapping + bloom profile onto the main camera while Flame is
        // active (live dev-panel tuning), via the shared generic applier.
        app.add_systems(
            Update,
            wc_core::sketch::apply_render_profile::<settings::FlameSettings>
                .run_if(in_state(AppState::Flame)),
        );

        // Restart listener (requires_restart fields fade out/in via the
        // shared reload overlay). The name is NOT requires_restart — it
        // rebuilds live through the name-change watcher (later stage).
        app.add_systems(
            Update,
            wc_core::sketch::restart_on_settings_change::<settings::FlameSettings>,
        );

        // Name-input overlay + debounced carousel admission. The overlay
        // draws every egui pass (self-gated inside the function on
        // Active-only); the debounce watcher only runs while the sketch is
        // actually active (no point admitting names while it's idle/hidden).
        app.init_resource::<ui::FlameNameDebounce>();
        app.add_systems(
            Update,
            ui::debounce_name_admission.run_if(wc_core::sketch::sketch_active(AppState::Flame)),
        );
        app.add_systems(
            bevy_egui::EguiPrimaryContextPass,
            (ui::flame_name_input_overlay, ui::flame_seed_ghost_label),
        );

        // Attract performer: carousel driver + ember complexity decay, both
        // gated `in_screensaver(AppState::Flame)` (zero systems otherwise).
        // The brightness lift lives in `drive_flame_material` above (already
        // gated `in_state(AppState::Flame)`, so it runs through the
        // screensaver too); the ghost label is registered just above.
        app.add_plugins(screensaver::FlameScreensaverPlugin);
    }
}

/// Register Flame's picker-tile metadata. Factored out of `FlamePlugin::build`
/// so it is unit-testable without rendering plugins (mirrors
/// `register_dots_manifest`).
pub(crate) fn register_flame_manifest(app: &mut App) {
    // Delegates to the shared `register_sketch_tile` helper (async PNG load +
    // manifest append). v4 calls this sketch "You-niverse" in HomePage.tsx.
    // `STORAGE_KEY` binds this tile to `FlameSettings` so the settings dock's
    // Sketch tab resolves to Flame automatically (no per-sketch match arm).
    use wc_core::settings::SketchSettings as _;
    wc_core::sketch::register_sketch_tile(
        app,
        AppState::Flame,
        "You-niverse",
        settings::FlameSettings::STORAGE_KEY,
        "sketches/flame/screenshot.png",
    );
}

/// Stash for the pre-Flame clear color, restored on exit.
#[derive(Resource)]
struct SavedClearColor(ClearColor);

/// `OnEnter(AppState::Flame)`: stash the current clear color and swap in
/// v4's scene background `#10101f`.
fn enter_flame_clear_color(mut commands: Commands<'_, '_>, current: Res<'_, ClearColor>) {
    commands.insert_resource(SavedClearColor(current.clone()));
    commands.insert_resource(ClearColor(Color::srgb_u8(0x10, 0x10, 0x1f)));
}

/// `OnExit(AppState::Flame)`: restore the stashed clear color.
fn exit_flame_clear_color(mut commands: Commands<'_, '_>, saved: Option<Res<'_, SavedClearColor>>) {
    if let Some(saved) = saved {
        commands.insert_resource(saved.0.clone());
    }
    commands.remove_resource::<SavedClearColor>();
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use wc_core::sketch::SketchManifest;

    /// Mirrors `register_dots_manifest_appends_entry`: the free-function path
    /// registers a Flame tile without needing a `RenderApp`.
    #[test]
    fn register_flame_manifest_appends_entry() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::asset::AssetPlugin::default());
        app.add_plugins(bevy::image::ImagePlugin::default());
        register_flame_manifest(&mut app);
        let manifest = app.world().resource::<SketchManifest>();
        let entry = manifest
            .get(AppState::Flame)
            .expect("Flame manifest entry should be registered");
        assert_eq!(entry.display_name, "You-niverse");
    }

    /// `OnEnter` swaps the clear color to v4's #10101f and stashes the prior
    /// value; `OnExit` restores it and drops the stash.
    #[test]
    fn clear_color_swap_and_restore() {
        let mut world = World::new();
        world.insert_resource(ClearColor(Color::WHITE));

        world
            .run_system_once(enter_flame_clear_color)
            .expect("enter runs");
        let cc = world.resource::<ClearColor>();
        assert_eq!(cc.0, Color::srgb_u8(0x10, 0x10, 0x1f));
        assert!(world.get_resource::<SavedClearColor>().is_some());

        world
            .run_system_once(exit_flame_clear_color)
            .expect("exit runs");
        assert_eq!(world.resource::<ClearColor>().0, Color::WHITE);
        assert!(world.get_resource::<SavedClearColor>().is_none());
    }
}
