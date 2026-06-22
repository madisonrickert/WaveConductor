//! Line sketch — particles attracted to a pointer-driven gravity well.
//!
//! ## Data flow
//!
//! 1. `OnEnter(AppState::Line)` runs [`systems::spawn_line`]: allocates the
//!    particle storage buffer, spawns the render entity under [`LineRoot`],
//!    installs [`crate::particles::compute::ParticleSimParams`], and seeds [`crate::particles::sim_cpu::CpuMirror`]
//!    with the initial particle layout (spawn-time snapshot for heatmap tests).
//! 2. Every `Update` while `sketch_active(AppState::Line)` is true:
//!    - a. [`systems::update_sim_params`] writes the current pointer position +
//!      `LineSettings` values into `ParticleSimParams`.
//!    - b. [`particle_stats::update_particle_stats`] reads
//!      [`systems::MouseAttractorState`] and [`Time`], populating
//!      [`particle_stats::ParticleStats`] via smoothed CPU envelopes (Plan 11
//!      Phase F; no per-particle reduction, no CPU mirror step in production).
//!    - c. [`audio_coupling::drive_audio_and_shader`] reads `ParticleStats`
//!      plus the grab/depth loudness drive
//!      [`leap_attractors::HandAudioDrive`] (maintained in the
//!      `LineLeapAttractorsPlugin` chain) and drives the Line synth voice +
//!      `LinePostParams` shader uniforms. A held mouse press pins the drive
//!      to full; after any release (click or grab) the tail decays through
//!      both the drive and the `grouped_upness` envelope — a deliberately
//!      somewhat faster post-release tail than the envelope alone.
//! 3. The render world extracts `ParticleSimParams` and dispatches the compute
//!    pipeline (`assets/shaders/particles/simulate.wgsl`) which updates the
//!    storage buffer in place.
//! 4. Bevy's 2D render path consumes the same storage buffer through
//!    [`crate::particles::material::ParticleMaterial`] and draws a quad per particle via the
//!    vertex-index-driven `assets/shaders/particles/render.wgsl`.
//! 5. `OnExit(AppState::Line)` runs `despawn_with::<LineRoot>` and
//!    `remove_sim_params` to free the entity tree and drop the
//!    `ParticleSimParams` resource so its `Handle<ShaderBuffer>` clone is
//!    released, allowing the GPU storage buffer ref-count to reach zero.

pub mod attractor_visuals;
pub mod audio_coupling;
pub mod bone_composite;
pub mod bone_wireframe;
pub mod hand_mesh;
pub mod hash;
pub mod heatmap;
pub mod leap_attractors;
pub mod particle_stats;
pub mod post_process;
pub mod screensaver;
pub mod settings;
pub mod systems;
pub mod template_adjustments;
#[cfg(feature = "templates")]
pub mod template_adjustments_panel;
#[cfg(feature = "templates")]
pub mod template_adjustments_store;

pub use systems::LineRoot;

use bevy::prelude::*;
use bevy::sprite_render::Material2dPlugin;
use wc_core::audio::state::AudioState;
#[cfg(debug_assertions)]
use wc_core::debug::DebugToggles;
use wc_core::lifecycle::reload::SketchReloadState;
use wc_core::lifecycle::state::AppState;
use wc_core::lifecycle::RegisterIdleVetoExt;
#[cfg(feature = "templates")]
use wc_core::settings::RegisterDockSectionExt;
use wc_core::settings::{RegisterSketchSettingsExt, SketchSettings};
use wc_core::sketch::{despawn_with, sketch_active, RegisterSketchManifestExt};

/// Plugin that registers the Line sketch.
pub struct LinePlugin;

impl Plugin for LinePlugin {
    fn build(&self, app: &mut App) {
        // Register LineSettings with the settings system (panel + persistence).
        app.register_sketch_settings::<settings::LineSettings>();
        // Per-image template adjustments ride the same settings rails (empty
        // settings_def; rendered by the custom dock section, persisted centrally).
        #[cfg(feature = "templates")]
        app.register_sketch_settings::<template_adjustments_store::LineTemplateAdjustments>();
        // The per-image adjustment sliders render under the template picker via
        // the settings dock's custom-section hook (render-only; state lives in
        // the registered resource above).
        #[cfg(feature = "templates")]
        app.register_dock_section(
            "line",
            template_adjustments_panel::render_template_adjustments,
        );

        // Register the picker-tile manifest entry (async screenshot load).
        register_line_manifest(app);

        // Register the Material2d for ParticleMaterial.
        app.add_plugins(Material2dPlugin::<
            crate::particles::material::ParticleMaterial,
        >::default());

        // Wire the compute pipeline.
        app.add_plugins(crate::particles::compute::ParticleComputePlugin);

        // Wire the gravity-smear post-process render-graph node. In debug
        // builds, `WC_DEBUG_DISABLE_SMEAR` skips it for render-stage isolation.
        // `DebugPlugin` (in `CorePlugin`) ran earlier, so `DebugToggles` is
        // present iff a `WC_DEBUG_*` var was set; absence means "all toggles
        // off" (and release has no `DebugToggles` at all).
        #[cfg(debug_assertions)]
        let toggles = app.world().get_resource::<DebugToggles>().copied();
        #[cfg(debug_assertions)]
        let register_smear = should_register_smear(toggles.as_ref());
        #[cfg(not(debug_assertions))]
        let register_smear = true;
        if register_smear {
            app.add_plugins(post_process::LinePostProcessPlugin);
        }

        // Wire the additive bone-glow composite node here (hoisted out of
        // `LineHandMeshPlugin` so `WC_DEBUG_DISABLE_BONE_COMPOSITE` can gate it
        // at the Line render-stage level). The composite reads `HandMeshTarget`
        // (extracted from the main world) and no-ops cleanly when that resource
        // is absent, so registering it independently of the bone camera is safe.
        #[cfg(debug_assertions)]
        let register_bone_composite = should_register_bone_composite(toggles.as_ref());
        #[cfg(not(debug_assertions))]
        let register_bone_composite = true;
        if register_bone_composite {
            app.add_plugins(bone_composite::LineBoneCompositePlugin);
        }

        // Wire per-hand attractors (Plan 11.6 Phase 11.1).
        app.add_plugins(leap_attractors::LineLeapAttractorsPlugin);

        // Wire the attract-mode / screensaver driver (Plan 11.8, Seam 3). Its
        // systems are gated on `in_screensaver(AppState::Line)`, so they run
        // only while Line's screensaver is showing — zero cost otherwise.
        app.add_plugins(screensaver::LineScreensaverPlugin);

        // Wire wireframe bone visualization (Plan 11.6 Phase 13).
        app.add_plugins(hand_mesh::LineHandMeshPlugin);

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

        // In-place re-seed when the active template's position-shaping
        // adjustments change (no fade reload); colour influence is a live
        // uniform handled by `drive_color_influence`.
        #[cfg(feature = "templates")]
        app.add_systems(
            Update,
            systems::reseed::reseed_on_adjustments_change.run_if(sketch_active(AppState::Line)),
        );
        // Live colour-influence uniform: tints particles toward their source
        // image colour with no re-seed (the blend strength is a shader uniform).
        #[cfg(feature = "templates")]
        app.add_systems(
            Update,
            systems::color_influence::drive_color_influence.run_if(sketch_active(AppState::Line)),
        );
        // Live palette-uniform driver: maps the LineSettings palette knobs into
        // the ParticleMaterial::palette_params uniform. Registered under both the
        // active and screensaver gates (mirrors drive_attract_color) so the
        // palette applies live AND in attract while running zero systems when
        // idle. Change-gated internally, so it is a single float compare per
        // frame in the settled state.
        app.add_systems(
            Update,
            systems::palette::drive_palette.run_if(sketch_active(AppState::Line)),
        );
        app.add_systems(
            Update,
            systems::palette::drive_palette.run_if(
                wc_core::lifecycle::screensaver::in_screensaver(AppState::Line),
            ),
        );
        // Prune per-image adjustments when the template library changes (delete,
        // out-of-band removal, startup reconcile), keeping the persisted map in
        // sync with the store.
        #[cfg(feature = "templates")]
        app.add_systems(
            Update,
            systems::prune_adjustments::prune_orphan_adjustments.run_if(
                bevy::ecs::schedule::common_conditions::resource_exists_and_changed::<
                    wc_core::templates::resource::TemplateLibrary,
                >,
            ),
        );

        // Restart listener: begins the FadeOut phase of the reload overlay when
        // a requires_restart setting changes. The overlay's `drive_reload_state`
        // system (in wc-core) drives the full FadeOut → Switch → FadeIn cycle.
        app.add_systems(Update, restart_on_settings_change);
    }
}

/// Register Line's picker-tile metadata into [`wc_core::sketch::SketchManifest`].
///
/// Factored out of [`LinePlugin::build`] so it is independently unit-testable
/// without `LinePlugin`'s rendering dependencies (`Material2dPlugin`,
/// `ParticleComputePlugin`, `LinePostProcessPlugin` all require a full `RenderApp`
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
/// Drops the `ParticleSimParams` resource so its `Handle<ShaderBuffer>`
/// clone is freed and the GPU storage buffer's ref-count reaches zero,
/// releasing VRAM on each Enter/Exit cycle. Also drops the CPU mirror so its
/// per-particle `Vec` is freed; `spawn_line` re-inserts a fresh snapshot on
/// the next `OnEnter`. The mirror is not stepped in production (Plan 11 Phase
/// F); it is only a spawn-time snapshot for heatmap test coverage.
///
/// Also drops [`systems::sim_params::LineSmearFocal`] so the next `OnEnter`
/// re-seeds a centered focal instead of inheriting the last in-Line value.
///
/// Resets [`post_process::LinePostParams`] to its `Default` (which has
/// `g_constant = 0.0`) so the gravity-smear post-process is visually no-op
/// outside `AppState::Line`. The `update_sim_params` system that writes the
/// real per-frame uniform is gated by `sketch_active(AppState::Line)`, so
/// without this reset the resource would retain its last in-Line value and
/// the post-process would keep applying smear after leaving Line.
fn remove_sim_params(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<crate::particles::compute::ParticleSimParams>();
    commands.remove_resource::<crate::particles::sim_cpu::CpuMirror>();
    commands.remove_resource::<systems::sim_params::LineSmearFocal>();
    commands.insert_resource(post_process::LinePostParams::default());
}

/// How long the user must stop adjusting a `requires_restart` setting before
/// the sketch restarts. 500 ms quiescence prevents mid-drag sketch kills when
/// the user is still adjusting a slider.
const RESTART_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

/// Listens for `SketchRestart { storage_key == LineSettings::STORAGE_KEY }`
/// and begins the reload fade-overlay transition so the `Line → Home → Line`
/// cycle is blacked out rather than flashing the picker page.
///
/// A 500 ms debounce (`RESTART_DEBOUNCE`) prevents the restart from firing
/// while the user is still dragging a slider. The debounce timestamp is tracked
/// in a `Local<Option<Duration>>` that is updated on every message and checked
/// each frame against `Time::elapsed`.
///
/// After the debounce window closes, calls `SketchReloadState::begin_fade_out`
/// which sets `phase = FadeOut`. The `drive_reload_state` system (registered in
/// `wc-core`'s `LifecyclePlugin`) owns all subsequent phase transitions:
/// `FadeOut` → Switch (sets `NextState::Home`) → `FadeIn` (sets `NextState::Line`).
fn restart_on_settings_change(
    mut events: MessageReader<'_, '_, wc_core::settings::SketchRestart>,
    time: Res<'_, bevy::prelude::Time>,
    current: Res<'_, State<AppState>>,
    mut reload_state: ResMut<'_, SketchReloadState>,
    // Optional: not present in headless (MinimalPlugins) test harnesses.
    audio_state: Option<Res<'_, AudioState>>,
    // Tracks the `Time::elapsed` of the last received restart message.
    // `None` means no message has been received since the last restart.
    mut last_change_at: Local<'_, Option<std::time::Duration>>,
) {
    // Absorb any new restart messages, updating the debounce timestamp.
    // Only arm when in Line (not during the Home/FadeIn return leg) and when
    // no reload is already in progress.
    let got_message = events
        .read()
        .any(|e| e.storage_key == settings::LineSettings::STORAGE_KEY);
    if got_message && **current == AppState::Line && reload_state.is_idle() {
        *last_change_at = Some(time.elapsed());
        tracing::debug!("LineSettings changed — debounce timer reset (500 ms)");
    }

    // Fire the FadeOut only after 500 ms of no further changes.
    if let Some(last) = *last_change_at {
        let elapsed_since = time.elapsed().saturating_sub(last);
        if elapsed_since >= RESTART_DEBOUNCE
            && **current == AppState::Line
            && reload_state.is_idle()
        {
            // Fall back to full volume (1.0) when the audio engine hasn't
            // started — headless tests and early startup before the cpal
            // stream is active.
            let pre_fade_volume = audio_state.as_ref().map_or(1.0, |s| s.volume);
            reload_state.begin_fade_out(time.elapsed(), pre_fade_volume);
            *last_change_at = None;
            tracing::debug!("LineSettings debounce elapsed — beginning reload FadeOut");
        }
    }
}

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
/// volume so the `line_background.ogg` sample resumes playing.
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
/// volume so the `line_background.ogg` sample stops playing when the user
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

/// Whether to register the gravity-smear post-process node. On unless
/// `WC_DEBUG_DISABLE_SMEAR` is set. Always on in release (no [`DebugToggles`]).
#[cfg(debug_assertions)]
fn should_register_smear(toggles: Option<&DebugToggles>) -> bool {
    !toggles.is_some_and(|t| t.disable_smear)
}

/// Whether to register the additive bone-composite node. On unless
/// `WC_DEBUG_DISABLE_BONE_COMPOSITE` is set. Always on in release.
#[cfg(debug_assertions)]
fn should_register_bone_composite(toggles: Option<&DebugToggles>) -> bool {
    !toggles.is_some_and(|t| t.disable_bone_composite)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected None is the correct behaviour"
)]
mod tests {
    use super::*;
    use wc_core::sketch::SketchManifest;

    /// `LinePlugin` decides whether to register the bone-composite + smear
    /// nodes by reading `DebugToggles`; this guards the gating predicate.
    #[test]
    #[cfg(debug_assertions)]
    fn render_stage_gating_predicate() {
        use wc_core::debug::DebugToggles;
        let all_off = DebugToggles {
            force_g: None,
            disable_smear: false,
            disable_bloom: false,
            disable_bone_composite: false,
            disable_bone_camera: false,
            solid_particles: None,
            force_screensaver: false,
            force_tier: None,
        };
        assert!(should_register_smear(Some(&all_off)));
        assert!(should_register_bone_composite(Some(&all_off)));
        assert!(should_register_smear(None)); // no toggles → everything on
        let no_smear = DebugToggles {
            disable_smear: true,
            ..all_off
        };
        assert!(!should_register_smear(Some(&no_smear)));
        let no_comp = DebugToggles {
            disable_bone_composite: true,
            ..all_off
        };
        assert!(!should_register_bone_composite(Some(&no_comp)));
    }

    /// `remove_sim_params` must drop `LineSmearFocal` on Line exit so a
    /// re-entry's `spawn_line` re-seeds a fresh centered focal rather than
    /// inheriting a stale off-center one (the resource-not-Local guarantee).
    #[test]
    fn remove_sim_params_drops_smear_focal() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.insert_resource(systems::sim_params::LineSmearFocal(Vec2::new(123.0, 45.0)));
        world
            .run_system_once(remove_sim_params)
            .expect("remove_sim_params run");
        assert!(
            world
                .get_resource::<systems::sim_params::LineSmearFocal>()
                .is_none(),
            "LineSmearFocal must be removed on Line exit"
        );
    }

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
