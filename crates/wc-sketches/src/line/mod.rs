//! Line sketch — particles attracted to a pointer-driven gravity well.
//!
//! ## Data flow
//!
//! 1. `OnEnter(AppState::Line)` runs [`systems::spawn_line`]: allocates the
//!    particle storage buffer, spawns the render entity under [`LineRoot`],
//!    installs [`compute::LineSimParams`], and seeds [`sim_cpu::LineCpuMirror`]
//!    with the initial particle layout (spawn-time snapshot for heatmap tests).
//! 2. Every `Update` while `sketch_active(AppState::Line)` is true:
//!    - a. [`systems::update_sim_params`] writes the current pointer position +
//!      `LineSettings` values into `LineSimParams`.
//!    - b. [`particle_stats::update_particle_stats`] reads
//!      [`systems::MouseAttractorState`] and [`Time`], populating
//!      [`particle_stats::ParticleStats`] via smoothed CPU envelopes (Plan 11
//!      Phase F; no per-particle reduction, no CPU mirror step in production).
//!    - c. [`audio_coupling::drive_audio_and_shader`] reads `ParticleStats` and
//!      drives the Line synth voice + `LinePostParams` shader uniforms.
//! 3. The render world extracts `LineSimParams` and dispatches the compute
//!    pipeline (`assets/shaders/line/simulate.wgsl`) which updates the
//!    storage buffer in place.
//! 4. Bevy's 2D render path consumes the same storage buffer through
//!    [`material::LineMaterial`] and draws a quad per particle via the
//!    vertex-index-driven `assets/shaders/line/render.wgsl`.
//! 5. `OnExit(AppState::Line)` runs `despawn_with::<LineRoot>` and
//!    `remove_sim_params` to free the entity tree and drop the
//!    `LineSimParams` resource so its `Handle<ShaderStorageBuffer>` clone is
//!    released, allowing the GPU storage buffer ref-count to reach zero.

pub mod attractor_visuals;
pub mod audio_coupling;
pub mod compute;
pub mod heatmap;
pub mod material;
pub mod particle;
pub mod particle_stats;
pub mod post_process;
pub mod settings;
pub mod sim_cpu;
pub mod systems;

pub use systems::LineRoot;

use bevy::prelude::*;
use bevy::sprite_render::Material2dPlugin;
use wc_core::lifecycle::state::AppState;
use wc_core::lifecycle::RegisterIdleVetoExt;
use wc_core::settings::{RegisterSketchSettingsExt, SketchSettings};
use wc_core::sketch::{despawn_with, sketch_active, RegisterSketchManifestExt};

/// Plugin that registers the Line sketch.
pub struct LinePlugin;

impl Plugin for LinePlugin {
    fn build(&self, app: &mut App) {
        // Register LineSettings with the settings system (panel + persistence).
        app.register_sketch_settings::<settings::LineSettings>();

        // Register the picker-tile manifest entry (async screenshot load).
        register_line_manifest(app);

        // Register the Material2d for LineMaterial.
        app.add_plugins(Material2dPlugin::<material::LineMaterial>::default());

        // Wire the compute pipeline.
        app.add_plugins(compute::LineComputePlugin);

        // Wire the gravity-smear post-process render-graph node.
        app.add_plugins(post_process::LinePostProcessPlugin);

        // Lifecycle: spawn on enter, despawn on exit. Audio lifecycle joins
        // the same `OnEnter`/`OnExit` schedules — `enter_line_audio` builds
        // the synth voice graph on the audio thread; `exit_line_audio` tears
        // it down so VRAM/audio resources are released between sketch entries
        // (project performance rule: per-sketch resources are owned by an
        // entity tagged with the sketch's marker component, despawned on
        // `OnExit` to release resources).
        app.add_systems(
            OnEnter(AppState::Line),
            (systems::spawn_line, enter_line_audio),
        );
        app.add_systems(
            OnExit(AppState::Line),
            (despawn_with::<LineRoot>, remove_sim_params, exit_line_audio),
        );

        // Mouse attractor state (independent of sketch active/idle so the
        // attractor's decay continues during the screensaver-fade window).
        app.init_resource::<systems::MouseAttractorState>();
        #[cfg(feature = "hand-tracking-gestures")]
        app.init_resource::<systems::mouse::LastPinchState>();
        // Register an idle veto that keeps Line `Active` while the mouse
        // attractor's power is still decaying — otherwise the sketch would
        // transition to `Idle` mid-decay and the (gated) `decay_mouse_attractor`
        // system would never finish releasing the pull.
        app.register_idle_veto(line_idle_veto);
        app.init_resource::<particle_stats::ParticleStats>();
        app.add_systems(
            Update,
            (
                systems::update_mouse_attractor,
                systems::decay_mouse_attractor,
                systems::update_sim_params,
                particle_stats::update_particle_stats,
                // `drive_audio_and_shader` reads `ParticleStats` and overrides
                // the placeholder `g_constant` + `i_mouse_factor` written by
                // `update_sim_params` earlier in this chain. The `.chain()`
                // ordering below makes the override deterministic.
                audio_coupling::drive_audio_and_shader,
                attractor_visuals::spawn_attractor_visual,
                attractor_visuals::animate_attractor_visual,
                attractor_visuals::despawn_attractor_visual,
            )
                .chain()
                .run_if(sketch_active(AppState::Line)),
        );

        // Restart listener: cycles Line → Home → Line when particle_density changes.
        app.add_systems(Update, restart_on_settings_change);
    }
}

/// Register Line's picker-tile metadata into [`wc_core::sketch::SketchManifest`].
///
/// Factored out of [`LinePlugin::build`] so it is independently unit-testable
/// without `LinePlugin`'s rendering dependencies (`Material2dPlugin`,
/// `LineComputePlugin`, `LinePostProcessPlugin` all require a full `RenderApp`
/// that `MinimalPlugins` does not provide).
///
/// The `AssetServer` load is async; the picker renders the tile as soon as the
/// image asset finishes loading. Before then the tile shows the dark placeholder
/// fill defined in `OverlayStyle`.
pub(crate) fn register_line_manifest(app: &mut App) {
    let asset_server = app.world().resource::<AssetServer>();
    // Load the picker-tile screenshot as PNG. Bevy's default features include
    // the `png` image loader; JPEG requires the separate `bevy/jpeg` feature
    // which is not enabled in this workspace. The PNG at this path is the
    // 1280×720 screenshot that was always present; the JPG copy (loaded in the
    // previous commit) has been removed.
    // v4 calls this sketch "Gravity" in HomePage.tsx:44.
    let screenshot = asset_server.load("sketches/line/screenshot.png");
    app.register_sketch_manifest(wc_core::sketch::SketchManifestEntry {
        state: AppState::Line,
        display_name: "Gravity",
        screenshot,
    });
}

/// `OnExit(AppState::Line)` companion to [`systems::spawn_line`].
///
/// Drops the `LineSimParams` resource so its `Handle<ShaderStorageBuffer>`
/// clone is freed and the GPU storage buffer's ref-count reaches zero,
/// releasing VRAM on each Enter/Exit cycle. Also drops the CPU mirror so its
/// per-particle `Vec` is freed; `spawn_line` re-inserts a fresh snapshot on
/// the next `OnEnter`. The mirror is not stepped in production (Plan 11 Phase
/// F); it is only a spawn-time snapshot for heatmap test coverage.
///
/// Resets [`post_process::LinePostParams`] to its `Default` (which has
/// `g_constant = 0.0`) so the gravity-smear post-process is visually no-op
/// outside `AppState::Line`. The `update_sim_params` system that writes the
/// real per-frame uniform is gated by `sketch_active(AppState::Line)`, so
/// without this reset the resource would retain its last in-Line value and
/// the post-process would keep applying smear after leaving Line.
fn remove_sim_params(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<compute::LineSimParams>();
    commands.remove_resource::<sim_cpu::LineCpuMirror>();
    commands.insert_resource(post_process::LinePostParams::default());
}

/// Listens for `SketchRestart { storage_key == LineSettings::STORAGE_KEY }`
/// and forces a same-frame `Line → Home → Line` cycle so the `OnExit`/`OnEnter`
/// systems rebuild the sketch with the new settings.
///
/// Uses a one-frame `LineRestartPending` resource as a self-clearing trampoline:
/// on the frame the restart message arrives, we set `NextState::Home` *and*
/// insert `LineRestartPending`. On the following frame's update, the resource is
/// observed → `NextState::Line`, then the resource is removed.
fn restart_on_settings_change(
    mut events: MessageReader<'_, '_, wc_core::settings::SketchRestart>,
    current: Res<'_, State<AppState>>,
    mut next: ResMut<'_, NextState<AppState>>,
    mut commands: Commands<'_, '_>,
    pending: Option<Res<'_, LineRestartPending>>,
) {
    if pending.is_some() {
        // Second frame: complete the cycle by re-entering Line.
        next.set(AppState::Line);
        commands.remove_resource::<LineRestartPending>();
        // `debug!` rather than `info!` — the trampoline has been stable since
        // Plan 7; firing on every settings restart is noise in release builds.
        tracing::debug!("LineSettings restart cycle: re-entering Line");
        return;
    }
    let want_restart = events
        .read()
        .any(|e| e.storage_key == settings::LineSettings::STORAGE_KEY);
    if want_restart && **current == AppState::Line {
        next.set(AppState::Home);
        commands.insert_resource(LineRestartPending);
        tracing::debug!("LineSettings changed — cycling Line via Home for one frame");
    }
}

/// Trampoline marker for the same-frame Line→Home→Line cycle. Inserted on the
/// frame a restart is detected; the next frame's `restart_on_settings_change`
/// observes it, transitions back to `Line`, and removes the resource.
#[derive(Resource)]
struct LineRestartPending;

/// Idle veto for the Line sketch. Returns `true` while the mouse attractor's
/// power is non-zero (i.e., still decaying) — keeps the sketch in
/// `SketchActivity::Active` so [`systems::decay_mouse_attractor`] continues to
/// fire until the attractor is fully released.
fn line_idle_veto(world: &World) -> bool {
    world
        .get_resource::<systems::MouseAttractorState>()
        .is_some_and(|s| s.power > 0.0)
}

/// `OnEnter(AppState::Line)` — push `AddLineSynth` and restore the background
/// volume so the line_background.ogg sample resumes playing.
///
/// Two commands are pushed:
/// 1. `AddLineSynth` — builds the synth voice graph (idempotent: no-op if a
///    synth already exists from a dropped tear-down).
/// 2. `SetLineParam { key: "background_volume", value: 1.0 }` — restores the
///    DSP host's background mixer to full volume. `exit_line_audio` sets this
///    to 0.0 on exit; after `enter_line_audio` restores it to 1.0 the
///    `audio_coupling` system keeps updating it each frame while Line is active.
///
/// Drops commands silently with a `warn` if the ring is full — the synth and
/// background will be set up correctly on the next successful command delivery.
fn enter_line_audio(
    audio_cmd: Option<bevy::ecs::system::NonSendMut<'_, wc_core::audio::ring::AudioCommandSender>>,
) {
    // The audio engine isn't started in headless integration tests (no cpal
    // device). Skip cleanly when the sender isn't present.
    let Some(mut audio_cmd) = audio_cmd else {
        return;
    };
    if let Err(_dropped) = audio_cmd.push(wc_core::audio::command::AudioCommand::AddLineSynth) {
        tracing::warn!("audio command ring full on Line entry; AddLineSynth dropped");
    }
    if let Err(_dropped) = audio_cmd.push(wc_core::audio::command::AudioCommand::SetLineParam {
        key: "background_volume",
        value: 1.0,
    }) {
        tracing::warn!("audio command ring full on Line entry; background_volume restore dropped");
    }
}

/// `OnExit(AppState::Line)` — push `RemoveLineSynth` and mute the background
/// volume so the line_background.ogg sample stops playing when the user
/// navigates to Home.
///
/// Two commands are pushed:
/// 1. `RemoveLineSynth` — tears down the synth voice graph (idempotent: no-op
///    if no synth is active).
/// 2. `SetLineParam { key: "background_volume", value: 0.0 }` — silences the
///    DSP host's background mixer so the sample track does not continue playing
///    over the picker page. `enter_line_audio` restores this to 1.0 on the next
///    entry, after which the `audio_coupling` system keeps it updated each frame.
///
/// Ring-full failures are logged as warnings and dropped — the audio thread is
/// severely backlogged in that case and the param will be restored on the next
/// `OnEnter(Line)`.
fn exit_line_audio(
    audio_cmd: Option<bevy::ecs::system::NonSendMut<'_, wc_core::audio::ring::AudioCommandSender>>,
) {
    let Some(mut audio_cmd) = audio_cmd else {
        return;
    };
    if let Err(_dropped) = audio_cmd.push(wc_core::audio::command::AudioCommand::RemoveLineSynth) {
        tracing::warn!("audio command ring full on Line exit; RemoveLineSynth dropped");
    }
    if let Err(_dropped) = audio_cmd.push(wc_core::audio::command::AudioCommand::SetLineParam {
        key: "background_volume",
        value: 0.0,
    }) {
        tracing::warn!("audio command ring full on Line exit; background_volume mute dropped");
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected None is the correct behaviour"
)]
mod tests {
    use super::*;
    use wc_core::sketch::SketchManifest;

    /// Verifies that `register_line_manifest` appends an entry for
    /// `AppState::Line` with the correct display name.
    ///
    /// Uses the free-function path rather than constructing the full
    /// `LinePlugin` because `LinePlugin::build` adds rendering plugins that
    /// require a real `RenderApp` — unavailable in headless unit tests.
    #[test]
    fn register_line_manifest_appends_entry() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::asset::AssetPlugin::default());
        // `ImagePlugin` registers `Image` as an asset type so `AssetServer`
        // can allocate a `Handle<Image>` for the screenshot path.
        app.add_plugins(bevy::image::ImagePlugin::default());
        register_line_manifest(&mut app);
        let manifest = app.world().resource::<SketchManifest>();
        let entry = manifest
            .get(AppState::Line)
            .expect("Line manifest entry should be registered");
        assert_eq!(entry.display_name, "Gravity");
    }
}
