//! Dots sketch ("Fabric") — a grid of dots that ripple and deform under
//! pointer and hand interaction.
//!
//! ## Data flow
//!
//! 1. `OnEnter(AppState::Dots)` runs [`systems::spawn_dots`]: allocates the
//!    particle storage buffer (full-screen grid), spawns the render entity under
//!    [`systems::DotsRoot`], installs
//!    [`crate::particles::compute::ParticleSimParams`], and seeds
//!    [`crate::particles::sim_cpu::CpuMirror`] with the initial grid layout.
//! 2. Every `Update` while `sketch_active(AppState::Dots)` is true:
//!    - a. [`systems::update_dots_sim_params`] writes the current
//!      [`DotsSettings`] values into `ParticleSimParams` (drag, stationary
//!      spring, size scale — no attractor in D2).
//!    - b. [`systems::update_dots_post_params`] writes [`post_process::DotsPostParams`]
//!      from the live cursor (v4 UV convention), window resolution, and
//!      [`settings::DotsSettings::gamma`].
//! 3. The render world extracts `ParticleSimParams` and dispatches the compute
//!    pipeline (`assets/shaders/particles/simulate.wgsl`), which updates the
//!    storage buffer in place.
//! 4. Bevy's 2D render path consumes the same buffer through
//!    [`crate::particles::material::ParticleMaterial`] and draws one quad per
//!    particle via the vertex-index-driven
//!    `assets/shaders/particles/render.wgsl`.
//! 5. `OnExit(AppState::Dots)` runs `despawn_with::<DotsRoot>` and
//!    `remove_dots_sim_params` to free the entity tree, drop
//!    `ParticleSimParams` (releases the GPU buffer ref-count), and drop
//!    `CpuMirror` (frees the per-particle `Vec`).
//!
//! Mouse interaction (pointer/touch attractor) is wired in
//! [`systems::update_dots_mouse_attractor`] / [`systems::decay_dots_mouse_attractor`]
//! (Task 3). Hand attractors are wired in Plan D5.
//!
//! Audio coupling is wired in [`audio_coupling::drive_dots_audio`]: an
//! attack/release activity envelope driven by [`systems::DotsMouseAttractorState::power`]
//! maps to [`wc_core::audio::dots_synth::DotsSynth`] volume and filter cutoff each frame (ENVELOPE-PRIMARY
//! approach; no GPU readback or CPU particle mirror). See [`audio_coupling`]
//! for the approximation rationale, param mapping, and known gap (LFO rate).
//!
//! The shared [`crate::particles::compute::ParticleComputePlugin`] and
//! [`crate::particles::material::ParticleMaterial`] are registered once by
//! the [`crate::SketchesPlugin`] umbrella, not here.

pub mod audio_coupling;
pub mod bone_composite;
pub mod bone_wireframe;
pub mod hand_attractors;
pub mod hand_mesh;
pub mod hash;
pub mod post_process;
pub mod screensaver;
pub mod settings;
pub mod systems;

pub use systems::DotsRoot;

use bevy::prelude::*;
use wc_core::audio::state::AudioState;
use wc_core::lifecycle::reload::SketchReloadState;
use wc_core::lifecycle::state::AppState;
use wc_core::lifecycle::RegisterIdleVetoExt;
use wc_core::settings::{RegisterSketchSettingsExt, SketchSettings};
use wc_core::sketch::{despawn_with, sketch_active, RegisterSketchManifestExt};

/// Plugin that registers the Dots (Fabric) sketch.
pub struct DotsPlugin;

impl Plugin for DotsPlugin {
    fn build(&self, app: &mut App) {
        // Register DotsSettings with the settings system (panel + persistence).
        app.register_sketch_settings::<settings::DotsSettings>();

        // Explode post-process: render node + uniform extract.
        app.add_plugins(post_process::DotsPostProcessPlugin);

        // Register the picker-tile manifest entry (async screenshot load).
        register_dots_manifest(app);

        // Lifecycle: spawn the grid on enter, despawn and release VRAM on exit.
        // Audio lifecycle joins the same schedules — `enter_dots_audio` builds
        // the synth voice graph on the audio thread; `exit_dots_audio` tears it
        // down so audio resources are released between sketch entries (project
        // performance rule: per-sketch resources despawned on `OnExit` to
        // release resources). v4 Dots has NO background OGG, so only the synth
        // voice itself is managed here (no background mixer command).
        app.add_systems(
            OnEnter(AppState::Dots),
            (
                systems::spawn_dots,
                insert_dots_post_params,
                enter_dots_audio,
            )
                .chain(),
        );
        app.add_systems(
            OnExit(AppState::Dots),
            (
                despawn_with::<DotsRoot>,
                remove_dots_sim_params,
                exit_dots_audio,
            ),
        );

        // Mouse attractor state (persists across frames; updated each frame in
        // the `Update` chain below). The idle veto below keeps Dots `Active`
        // while the attractor has non-zero power so the decay system continues
        // to fire until the pull fully releases.
        app.init_resource::<systems::DotsMouseAttractorState>();
        // Register an idle veto that keeps Dots `Active` while the mouse
        // attractor's power is still decaying — otherwise the sketch would
        // transition to `Idle` mid-decay and the (gated) decay system would
        // never finish releasing the pull.
        app.register_idle_veto(dots_idle_veto);

        // Activity envelope resource (ENVELOPE-PRIMARY audio coupling). Starts
        // at 0.0 and is advanced each frame by `drive_dots_audio`. Persists
        // across enter/exit cycles (see `DotsAudioEnvelope` docs).
        app.init_resource::<audio_coupling::DotsAudioEnvelope>();

        // Per-frame: update mouse state, decay the attractor, write sim params,
        // then drive the audio envelope (audio reads the attractor state, so it
        // comes after the mouse/decay step). All systems run inside the
        // `sketch_active` gate so they do not execute while Dots is idle.
        //
        // `update_dots_post_params` writes DotsPostParams (cursor → UV, window
        // resolution, gamma). It only writes a resource that the render world
        // extracts; it has no ordering dependency on the mouse/sim chain, so it
        // runs as an independent system in the same gate.
        app.add_systems(
            Update,
            (
                systems::update_dots_mouse_attractor,
                systems::decay_dots_mouse_attractor,
                systems::update_dots_sim_params,
                audio_coupling::drive_dots_audio,
            )
                .chain()
                .run_if(sketch_active(AppState::Dots)),
        );
        app.add_systems(
            Update,
            systems::update_dots_post_params.run_if(sketch_active(AppState::Dots)),
        );

        // Hand attractors (D5) wired here.
        app.add_plugins(hand_attractors::DotsLeapAttractorsPlugin);
        // Screensaver attract driver (D6a).
        app.add_plugins(screensaver::DotsScreensaverPlugin);
        // Wireframe bone visualization (D6b Task 1): off-screen bone Camera3d
        // + 20 icosphere children per TrackedHand while Dots is active.
        app.add_plugins(hand_mesh::DotsHandMeshPlugin);
        // Additive bone-glow composite (D6b Task 2): blends the off-screen bone
        // image into the Dots scene before bloom + tonemapping. No-ops cleanly
        // when `DotsHandMeshTarget` is absent (outside Dots or before the image
        // first uploads). Carry-forward: add a `WC_DEBUG_DISABLE_BONE_COMPOSITE`
        // gate here (mirroring Line's `should_register_bone_composite`) if Dots
        // needs per-stage render isolation in debug builds.
        app.add_plugins(bone_composite::DotsBoneCompositePlugin);

        // Restart listener: begins the FadeOut phase of the reload overlay when
        // a requires_restart setting changes (e.g. `dot_spacing`, which sizes
        // the compute storage buffer at spawn). The overlay's `drive_reload_state`
        // system (in wc-core) drives the full FadeOut → Switch → FadeIn cycle.
        // Mirrors `LinePlugin`'s `restart_on_settings_change` registration.
        app.add_systems(Update, restart_on_dots_settings_change);
    }
}

/// Register Dots's picker-tile metadata into [`wc_core::sketch::SketchManifest`].
///
/// Factored out of [`DotsPlugin::build`] so it is independently unit-testable
/// without `DotsPlugin`'s rendering dependencies (the shared
/// `ParticleComputePlugin` and `Material2dPlugin::<ParticleMaterial>` both
/// require a full `RenderApp` that `MinimalPlugins` does not provide).
///
/// The `AssetServer` load is async; the picker renders the tile as soon as
/// the image asset finishes loading. Before then the tile shows the dark
/// placeholder fill defined in `OverlayStyle`. This mirrors the behavior of
/// [`crate::line::register_line_manifest`].
pub(crate) fn register_dots_manifest(app: &mut App) {
    let asset_server = app.world().resource::<AssetServer>();
    // Load the picker-tile screenshot as PNG. Bevy's default features include
    // the `png` image loader; JPEG requires the separate `bevy/jpeg` feature
    // which is not enabled in this workspace.
    // v4 calls this sketch "Fabric" in HomePage.tsx.
    let screenshot = asset_server.load("sketches/dots/screenshot.png");
    app.register_sketch_manifest(wc_core::sketch::SketchManifestEntry {
        state: AppState::Dots,
        display_name: "Fabric",
        screenshot,
    });
}

/// Idle veto for the Dots sketch. Returns `true` while the mouse attractor's
/// power is non-zero (i.e., active or still decaying) — keeps the sketch in
/// `SketchActivity::Active` so [`systems::decay_dots_mouse_attractor`] continues
/// to fire until the attractor is fully released.
///
/// Mirrors [`crate::line::line_idle_veto`] for the Line sketch.
fn dots_idle_veto(world: &World) -> bool {
    world
        .get_resource::<systems::DotsMouseAttractorState>()
        .is_some_and(|s| s.power > 0.0)
}

/// `OnEnter(AppState::Dots)` — push `AddDotsSynth` to build the Dots synth
/// voice graph on the audio thread.
///
/// v4 Dots has NO background OGG sample, so only the synth voice itself is
/// started here (no background-volume restore, unlike `enter_line_audio`).
///
/// Drops the command silently with a `warn` if the ring is full — the synth
/// will be set up correctly on the next successful command delivery.
///
/// Early-returns cleanly when `AudioCommandSender` is absent (headless tests:
/// no cpal device). Mirrors [`crate::line::enter_line_audio`].
fn enter_dots_audio(
    audio_cmd: Option<bevy::ecs::system::NonSendMut<'_, wc_core::audio::ring::AudioCommandSender>>,
) {
    // The audio engine is not started in headless integration tests (no cpal
    // device). Skip cleanly when the sender is not present.
    let Some(mut audio_cmd) = audio_cmd else {
        return;
    };
    if let Err(_dropped) = audio_cmd.push(wc_core::audio::command::AudioCommand::AddDotsSynth) {
        tracing::warn!("audio command ring full on Dots entry; AddDotsSynth dropped");
    }
}

/// `OnExit(AppState::Dots)` — push `RemoveDotsSynth` to tear down the Dots
/// synth voice graph and release its audio allocations.
///
/// Idempotent: a second `RemoveDotsSynth` while no synth is active is a no-op
/// (handled by the audio engine). Ring-full failures are logged as warnings and
/// dropped — the synth will be cleaned up on the next successful command.
///
/// Early-returns cleanly when `AudioCommandSender` is absent (headless tests).
/// Mirrors [`crate::line::exit_line_audio`].
fn exit_dots_audio(
    audio_cmd: Option<bevy::ecs::system::NonSendMut<'_, wc_core::audio::ring::AudioCommandSender>>,
) {
    let Some(mut audio_cmd) = audio_cmd else {
        return;
    };
    if let Err(_dropped) = audio_cmd.push(wc_core::audio::command::AudioCommand::RemoveDotsSynth) {
        tracing::warn!("audio command ring full on Dots exit; RemoveDotsSynth dropped");
    }
}

/// `OnEnter(AppState::Dots)` — insert [`post_process::DotsPostParams`] with
/// static seed values. [`systems::update_dots_post_params`] overwrites these
/// every frame with live cursor, resolution, and gamma; the values here are
/// only visible on the first frame before the `Update` systems run.
///
/// Seed values:
/// - `shrink_factor = 0.98` — v4 default.
/// - `gamma = 1.0` — identity; the Update driver reads `DotsSettings` each frame.
/// - `i_mouse = [0.5, 0.5]` — screen centre (normalised UV); prevents a corner
///   explode on the first frame before any cursor is known.
/// - `i_resolution` — from the primary window; falls back to `[1920.0, 1080.0]`.
fn insert_dots_post_params(mut commands: Commands<'_, '_>, window: Query<'_, '_, &Window>) {
    let (w, h) = window
        .single()
        .map_or((1920.0, 1080.0), |win| (win.width(), win.height()));
    commands.insert_resource(post_process::DotsPostParams {
        i_resolution: [w, h],
        i_mouse: [0.5, 0.5],
        shrink_factor: 0.98,
        gamma: 1.0,
    });
}

/// `OnExit(AppState::Dots)` companion to [`systems::spawn_dots`].
///
/// Drops `ParticleSimParams` so its `Handle<ShaderBuffer>` clone is freed and
/// the GPU storage buffer's ref-count reaches zero, releasing VRAM on each
/// Enter/Exit cycle. Also drops `CpuMirror` so its per-particle `Vec` is
/// freed; `spawn_dots` re-inserts a fresh snapshot on the next `OnEnter`.
/// Also drops [`post_process::DotsPostParams`] so the render system no-ops
/// outside Dots (the `Option<Res<DotsPostParams>>` gate returns `None`).
fn remove_dots_sim_params(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<crate::particles::compute::ParticleSimParams>();
    commands.remove_resource::<crate::particles::sim_cpu::CpuMirror>();
    commands.remove_resource::<post_process::DotsPostParams>();
}

/// How long the user must stop adjusting a `requires_restart` setting before
/// the sketch restarts. 500 ms quiescence prevents mid-drag sketch kills when
/// the user is still adjusting a slider. Mirrors [`crate::line`]'s debounce.
const RESTART_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

/// Listens for [`wc_core::settings::SketchRestart`] events targeted at
/// [`settings::DotsSettings::STORAGE_KEY`] ("dots") and begins the reload
/// fade-overlay transition so the `Dots → Home → Dots` cycle is blacked out
/// rather than flashing the picker page.
///
/// A 500 ms debounce prevents the restart from firing while the user is still
/// dragging a slider. The debounce timestamp is tracked in a
/// `Local<Option<Duration>>` that is updated on every matching message and
/// checked each frame against `Time::elapsed`.
///
/// After the debounce window closes, calls [`SketchReloadState::begin_fade_out`]
/// which sets `phase = FadeOut`. The `drive_reload_state` system (registered in
/// `wc-core`'s `LifecyclePlugin`) owns all subsequent phase transitions:
/// `FadeOut` → Switch (sets `NextState::Home`) → `FadeIn` (sets
/// `NextState::Dots`). Mirrors [`crate::line::restart_on_settings_change`].
fn restart_on_dots_settings_change(
    mut events: MessageReader<'_, '_, wc_core::settings::SketchRestart>,
    time: Res<'_, Time>,
    current: Res<'_, State<AppState>>,
    mut reload_state: ResMut<'_, SketchReloadState>,
    // Optional: not present in headless (MinimalPlugins) test harnesses.
    audio_state: Option<Res<'_, AudioState>>,
    // Tracks the `Time::elapsed` of the last received restart message.
    // `None` means no message has been received since the last restart.
    mut last_change_at: Local<'_, Option<std::time::Duration>>,
) {
    // Absorb any new restart messages, updating the debounce timestamp.
    // Only arm when in Dots (not during the Home/FadeIn return leg) and when
    // no reload is already in progress.
    let got_message = events
        .read()
        .any(|e| e.storage_key == settings::DotsSettings::STORAGE_KEY);
    if got_message && **current == AppState::Dots && reload_state.is_idle() {
        *last_change_at = Some(time.elapsed());
        tracing::debug!("DotsSettings changed — debounce timer reset (500 ms)");
    }

    // Fire the FadeOut only after 500 ms of no further changes.
    if let Some(last) = *last_change_at {
        let elapsed_since = time.elapsed().saturating_sub(last);
        if elapsed_since >= RESTART_DEBOUNCE
            && **current == AppState::Dots
            && reload_state.is_idle()
        {
            // Fall back to full volume (1.0) when the audio engine hasn't
            // started — headless tests and early startup before the cpal
            // stream is active.
            let pre_fade_volume = audio_state.as_ref().map_or(1.0, |s| s.volume);
            reload_state.begin_fade_out(time.elapsed(), pre_fade_volume, AppState::Dots);
            *last_change_at = None;
            tracing::debug!("DotsSettings debounce elapsed — beginning reload FadeOut");
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected None is the correct behaviour"
)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use wc_core::sketch::SketchManifest;

    /// Verifies that `register_dots_manifest` appends an entry for
    /// `AppState::Dots` with the correct display name.
    ///
    /// Uses the free-function path rather than constructing the full
    /// `DotsPlugin` because `DotsPlugin::build` adds rendering plugins that
    /// require a real `RenderApp` — unavailable in headless unit tests.
    /// Mirrors `register_line_manifest_appends_entry` in `crate::line::tests`.
    #[test]
    fn register_dots_manifest_appends_entry() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::asset::AssetPlugin::default());
        // `ImagePlugin` registers `Image` as an asset type so `AssetServer`
        // can allocate a `Handle<Image>` for the screenshot path.
        app.add_plugins(bevy::image::ImagePlugin::default());
        register_dots_manifest(&mut app);
        let manifest = app.world().resource::<SketchManifest>();
        let entry = manifest
            .get(AppState::Dots)
            .expect("Dots manifest entry should be registered");
        assert_eq!(entry.display_name, "Fabric");
    }

    /// `dots_idle_veto` returns `true` while power > 0 and `false` when power
    /// is zero — ensures the sketch stays `Active` during attractor decay.
    /// Mirrors `line_idle_veto` behavior.
    #[test]
    fn dots_idle_veto_true_while_power_nonzero() {
        use systems::DotsMouseAttractorState;

        let mut world = World::new();

        // No resource at all → veto returns false (no attractor in flight).
        assert!(
            !dots_idle_veto(&world),
            "veto must be false when DotsMouseAttractorState is absent"
        );

        // Power = 0.0 → veto false.
        world.insert_resource(DotsMouseAttractorState {
            power: 0.0,
            position: [0.0, 0.0],
        });
        assert!(
            !dots_idle_veto(&world),
            "veto must be false when power == 0.0"
        );

        // Power > 0.0 → veto true (attractor still active or decaying).
        world.insert_resource(DotsMouseAttractorState {
            power: 1.0,
            position: [0.0, 0.0],
        });
        assert!(dots_idle_veto(&world), "veto must be true when power > 0.0");
    }

    /// `remove_dots_sim_params` must drop `ParticleSimParams`, `CpuMirror`,
    /// and `DotsPostParams` on Dots exit so VRAM and CPU memory are released
    /// and the render system no-ops outside Dots.
    #[test]
    fn remove_dots_sim_params_drops_resources() {
        use crate::particles::compute::ParticleSimParams;
        use crate::particles::particle::SimParams;
        use crate::particles::sim_cpu::CpuMirror;
        use post_process::DotsPostParams;

        let mut world = World::new();
        world.insert_resource(ParticleSimParams {
            params: SimParams::default(),
            particles_handle: Handle::default(),
            particle_count: 0,
        });
        world.insert_resource(CpuMirror { particles: vec![] });
        world.insert_resource(DotsPostParams {
            i_resolution: [1920.0, 1080.0],
            i_mouse: [0.5, 0.5],
            shrink_factor: 0.98,
            gamma: 1.0,
        });

        world
            .run_system_once(remove_dots_sim_params)
            .expect("remove_dots_sim_params run");

        assert!(
            world.get_resource::<ParticleSimParams>().is_none(),
            "ParticleSimParams must be removed on Dots exit"
        );
        assert!(
            world.get_resource::<CpuMirror>().is_none(),
            "CpuMirror must be removed on Dots exit"
        );
        assert!(
            world.get_resource::<DotsPostParams>().is_none(),
            "DotsPostParams must be removed on Dots exit so render system no-ops"
        );
    }

    /// `insert_dots_post_params` must insert `DotsPostParams` on Dots enter
    /// with the static defaults: `shrink_factor=0.98`, `gamma=1.0`,
    /// `i_mouse=[0.5, 0.5]`, and `i_resolution` read from the window (or the
    /// fallback `[1920, 1080]` when no window entity is present).
    #[test]
    #[allow(clippy::float_cmp, reason = "comparing literal defaults")]
    fn insert_dots_post_params_inserts_resource() {
        use post_process::DotsPostParams;

        let mut world = World::new();
        // No window entity — the system falls back to [1920, 1080].
        world
            .run_system_once(insert_dots_post_params)
            .expect("insert_dots_post_params run");

        let params = world
            .get_resource::<DotsPostParams>()
            .expect("DotsPostParams must be present after OnEnter(Dots)");
        assert_eq!(params.shrink_factor, 0.98, "shrink_factor must be 0.98");
        assert_eq!(params.gamma, 1.0, "gamma must be 1.0");
        assert_eq!(params.i_mouse, [0.5, 0.5], "i_mouse must default to centre");
        // Fallback resolution when no window is present.
        assert_eq!(
            params.i_resolution,
            [1920.0, 1080.0],
            "i_resolution must fall back to [1920, 1080] when no window"
        );
    }

    /// `restart_on_dots_settings_change` must transition `SketchReloadState`
    /// to `FadeOut` after a `SketchRestart { storage_key: "dots" }` event
    /// arrives and the 500 ms debounce elapses while in `AppState::Dots`.
    ///
    /// This is the primary behavioral assertion for the Dots restart listener.
    /// It exercises the system end-to-end: event receipt, debounce arming,
    /// debounce expiry, and `begin_fade_out` invocation.
    #[test]
    fn restart_listener_begins_fade_out_on_dots_key() {
        use std::time::Duration;

        use bevy::state::app::StatesPlugin;
        use wc_core::lifecycle::reload::{ReloadPhase, SketchReloadState};
        use wc_core::settings::SketchRestart;

        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(StatesPlugin);
        app.init_state::<AppState>();
        app.insert_resource(SketchReloadState::default());
        // Register the SketchRestart message type so `MessageReader` resolves.
        app.add_message::<SketchRestart>();
        app.add_systems(Update, restart_on_dots_settings_change);

        // Transition to AppState::Dots so the listener gates correctly.
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Dots);
        app.update(); // Apply state transition.

        // Send a SketchRestart event for "dots".
        app.world_mut()
            .resource_mut::<Messages<SketchRestart>>()
            .write(SketchRestart {
                storage_key: "dots",
            });
        // First update: listener reads the message and arms the debounce timer.
        app.update();

        // Immediately after receipt the reload must still be idle (500 ms
        // debounce has not elapsed yet).
        assert_eq!(
            app.world().resource::<SketchReloadState>().phase,
            ReloadPhase::Idle,
            "reload must remain idle while debounce window is open"
        );

        // Advance time past the 500 ms debounce in 100 ms chunks.
        // `ManualDuration` advances `Time<()>.delta_secs()` by the given amount
        // each `app.update()`, which accumulates in `Time::elapsed()`.
        // 7 steps × 100 ms = 700 ms — ample headroom past the 500 ms window.
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_millis(100),
        ));
        for _ in 0..7_u32 {
            app.update();
        }

        assert_eq!(
            app.world().resource::<SketchReloadState>().phase,
            ReloadPhase::FadeOut,
            "reload must transition to FadeOut after debounce elapses"
        );
        // Confirm the return_state was set to Dots so the fade cycle navigates
        // back to the correct sketch.
        assert_eq!(
            app.world().resource::<SketchReloadState>().return_state,
            AppState::Dots,
            "return_state must be Dots so drive_reload_state navigates back correctly"
        );
    }

    /// `restart_on_dots_settings_change` must ignore `SketchRestart` events
    /// whose `storage_key` does not match `DotsSettings::STORAGE_KEY` ("dots").
    ///
    /// Verifies the filter predicate: a "line" event (wrong key) must leave
    /// `SketchReloadState` in `Idle` even after the debounce window passes.
    #[test]
    fn restart_listener_ignores_non_dots_key() {
        use std::time::Duration;

        use bevy::state::app::StatesPlugin;
        use wc_core::lifecycle::reload::{ReloadPhase, SketchReloadState};
        use wc_core::settings::SketchRestart;

        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(StatesPlugin);
        app.init_state::<AppState>();
        app.insert_resource(SketchReloadState::default());
        app.add_message::<SketchRestart>();
        app.add_systems(Update, restart_on_dots_settings_change);

        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Dots);
        app.update();

        // Send a restart event for a different sketch key ("line").
        app.world_mut()
            .resource_mut::<Messages<SketchRestart>>()
            .write(SketchRestart {
                storage_key: "line",
            });

        // Advance time past the debounce window in 100 ms chunks.
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_millis(100),
        ));
        for _ in 0..7_u32 {
            app.update();
        }

        // Reload state must remain idle: the "line" key was filtered out.
        assert_eq!(
            app.world().resource::<SketchReloadState>().phase,
            ReloadPhase::Idle,
            "listener must not fire for events targeting other sketches"
        );
    }
}
