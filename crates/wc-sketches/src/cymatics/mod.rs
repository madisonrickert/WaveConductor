//! Cymatics sketch: a 2D wave-field simulation (ping-pong storage-texture
//! compute) rendered fullscreen.
//!
//! ## Data flow (wired so far)
//!
//! 1. `OnEnter(AppState::Cymatics)` runs [`init_cymatics_state`] (insert the
//!    CPU-side [`CymaticsState`] defaults) then [`spawn_cymatics`] (read
//!    [`settings::CymaticsSettings`] → derive the sim resolution from the window
//!    aspect → allocate the two ping-pong textures
//!    ([`compute::create_cymatics_textures`]) → spawn the fullscreen quad
//!    ([`render::spawn_cymatics_quad`], sampling texture A) → tag the texture
//!    handles onto a [`CymaticsRoot`] entity → insert the initial
//!    [`compute::CymaticsSimParams`]).
//! 2. Every `Update` while the sketch is `Active` **or** showing its
//!    screensaver, [`update_cymatics_sim_params`] packs [`CymaticsState`] into
//!    the extracted [`compute::CymaticsSimParams`] (centres, alive radius, and
//!    the per-iteration phase times) and advances the phase clock once.
//! 3. The render world extracts `CymaticsSimParams`;
//!    [`compute::CymaticsComputePlugin`] advances the wave field on the GPU
//!    (`assets/shaders/cymatics/simulate.wgsl`), and [`render::CymaticsMaterial`]
//!    samples texture A directly (`assets/shaders/cymatics/render.wgsl`).
//! 4. `OnExit(AppState::Cymatics)` despawns the [`CymaticsRoot`] entity tree
//!    (frees VRAM) and drops [`compute::CymaticsSimParams`] + [`CymaticsState`].
//!
//! The shared [`compute::CymaticsComputePlugin`] and the
//! `Material2dPlugin::<CymaticsMaterial>` are registered once by the
//! [`crate::SketchesPlugin`] umbrella, not here.
//!
//! Mouse/hand interaction (which drives the two wave centres), the faithful
//! audio coupling derived from the same `CymaticsState`, and the wandering
//! attract mode arrive in later stages; their systems will slot into the
//! lifecycle and `Update` chain established here.
//!
//! The shared bloomed hand-mesh overlay is registered in
//! [`CymaticsPlugin::build`] via [`crate::hand_mesh::HandMeshPlugin`]. Cymatics
//! renders in the main 2D pass with no post-process node, so no
//! [`crate::hand_mesh::HandMeshCompositeSet`] ordering edge is needed; the
//! composite runs in `EarlyPostProcess` after the 2D pass by default.

pub mod compute;
pub mod render;
pub mod screensaver;
pub mod settings;
pub mod systems;

use bevy::prelude::*;
use bevy::sprite_render::MeshMaterial2d;
use wc_core::audio::state::AudioState;
use wc_core::lifecycle::reload::SketchReloadState;
use wc_core::lifecycle::screensaver::in_screensaver;
use wc_core::lifecycle::state::AppState;
use wc_core::lifecycle::RegisterIdleVetoExt;
use wc_core::settings::{RegisterSketchSettingsExt, SketchSettings};
use wc_core::sketch::{despawn_with, sketch_active, RegisterSketchManifestExt};

use compute::{create_cymatics_textures, CymaticsSimParams, SimParamsGpu, MAX_ITERATIONS};
use settings::CymaticsSettings;

/// Debounce window before a `requires_restart` settings change triggers the
/// reload fade. Matches the Dots sketch (`RESTART_DEBOUNCE` in `dots/mod.rs`).
const RESTART_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

/// Resting alive-mask radius (v4 `MINIMUM_ACTIVE_RADIUS`). At rest the wave
/// sources oscillate inside a small mask of this radius; interaction (C9) grows
/// it. Defined here for C8; the interaction systems consume it when they land.
pub const MINIMUM_ACTIVE_RADIUS: f32 = 0.1;

/// Resting wave-frequency control (v4 default `numCycles`). Interaction lowers
/// the effective cycle count via `slowDown`; see [`update_cymatics_sim_params`].
pub const DEFAULT_NUM_CYCLES: f32 = 1.002;

/// Marker component placed on every entity owned by the Cymatics sketch.
///
/// `OnEnter(AppState::Cymatics)` tags the fullscreen quad and the texture-handle
/// holder with this marker; `OnExit(AppState::Cymatics)` despawns everything
/// tagged with it via [`wc_core::sketch::despawn_with`], releasing the two
/// ping-pong textures' VRAM each enter/exit cycle.
#[derive(Component)]
pub struct CymaticsRoot;

/// CPU-side interaction state (v4 `index.ts` instance vars). Units match v4.
///
/// At C8 the interaction systems are not yet wired, so these hold their resting
/// defaults; [`update_cymatics_sim_params`] packs them into the GPU uniform and
/// advances [`CymaticsState::simulation_time`] each frame.
#[derive(Resource, Debug, Clone)]
pub struct CymaticsState {
    /// Primary wave centre, sim UV `[0, 1]`, top-left origin (Bevy-native).
    pub center: Vec2,
    /// Secondary wave centre, sim UV `[0, 1]`, top-left origin (Bevy-native).
    pub center2: Vec2,
    /// Alive-mask radius (v4 `activeRadius`).
    pub active_radius: f32,
    /// Frequency control (v4 `numCycles`).
    pub num_cycles: f32,
    /// Decays toward 0 on interaction onset, lowering the effective cycle count
    /// (v4 `slowDownAmount`). Held at 0 until interaction lands (C9).
    pub slow_down: f32,
    /// Phase clock (v4 `simulationTime`), advanced `N·dt` per frame.
    pub simulation_time: f32,
    /// Last frame's primary-centre speed (for the audio coupling), v4
    /// `centerSpeed`. Held at 0 until interaction lands (C9).
    pub center_speed: f32,
}

impl Default for CymaticsState {
    fn default() -> Self {
        Self {
            // Top-left UV convention: both shaders are Bevy-native top-left
            // origin, so no v4-style `y = 1 - y` flip is applied here.
            center: Vec2::new(0.5, 0.5),
            center2: Vec2::new(0.5, 0.5),
            active_radius: MINIMUM_ACTIVE_RADIUS,
            num_cycles: DEFAULT_NUM_CYCLES,
            slow_down: 0.0,
            simulation_time: 0.0,
            center_speed: 0.0,
        }
    }
}

/// Cymatics sketch plugin.
///
/// Registers the settings + picker-tile manifest, the lifecycle schedules
/// (`OnEnter`/`OnExit`), the idle veto, and the per-frame CPU→GPU sim-params
/// bridge. The shared compute node and render material are registered once by
/// [`crate::SketchesPlugin`]; re-registering them here would trigger Bevy's
/// duplicate-plugin panic, so this plugin deliberately does not.
pub struct CymaticsPlugin;

impl Plugin for CymaticsPlugin {
    fn build(&self, app: &mut App) {
        // Settings (panel + persistence) and the picker-tile manifest entry.
        app.register_sketch_settings::<CymaticsSettings>();
        register_cymatics_manifest(app);

        // Restart listener: debounces `requires_restart` settings changes
        // (vertical_resolution, iterations) and begins the fade-out/reload
        // cycle. Mirrors `restart_on_dots_settings_change` in `dots/mod.rs`.
        app.add_systems(Update, restart_on_cymatics_settings_change);

        // Shared wireframe bone overlay (mirrors Line/Dots). Cymatics renders in
        // the main 2D pass with no post-process node, so no
        // `HandMeshCompositeSet` ordering edge is needed — the composite runs
        // in `EarlyPostProcess` after the 2D pass by default (confirmed tolerant
        // of the absent edge in the hand-mesh integration tests).
        app.add_plugins(crate::hand_mesh::HandMeshPlugin {
            config: crate::hand_mesh::HandMeshConfig {
                app_state: AppState::Cymatics,
                // Orange `#eb5938` — v4 BASE_BODY_COL (235, 89, 56). Intentionally
                // matches the render shader's body colour so the bone overlay reads
                // as part of the same visual palette.
                bone_color: Color::srgb(
                    f32::from(0xeb_u8) / 255.0,
                    f32::from(0x59_u8) / 255.0,
                    f32::from(0x38_u8) / 255.0,
                ),
                glow_intensity: 5.0,
                bone_radius: 10.0,
            },
        });

        // Attract mode: two wave centres drift on slow incommensurate Lissajous
        // paths while the screensaver shows. Zero systems when not in screensaver.
        app.add_plugins(screensaver::CymaticsScreensaverPlugin);

        // Lifecycle: allocate the textures + spawn the quad on enter, despawn
        // and release VRAM on exit. Audio lifecycle joins the same schedules:
        // `enter_cymatics_audio` builds the synth voice bundle; `exit_cymatics_audio`
        // tears it down so audio allocations are released between sketch entries.
        app.add_systems(
            OnEnter(AppState::Cymatics),
            (
                init_cymatics_state,
                spawn_cymatics,
                systems::audio_coupling::enter_cymatics_audio,
            )
                .chain(),
        );
        app.add_systems(
            OnExit(AppState::Cymatics),
            (
                despawn_with::<CymaticsRoot>,
                remove_cymatics_sim_params,
                systems::audio_coupling::exit_cymatics_audio,
            ),
        );

        // Onset throttle state for kick/risingbass one-shots (persists across
        // enter/exit cycles so the throttle survives a fast sketch re-entry).
        app.init_resource::<systems::audio_coupling::CymaticsTriggerState>();

        // Idle veto: stay `Active` while the field is still energised (the mask
        // radius is above rest), so the wave keeps integrating until it settles
        // rather than freezing the instant the idle timer trips. Vetoes read
        // `World` and return `false` when `CymaticsState` is absent, which is
        // why `OnExit` drops the resource (see `remove_cymatics_sim_params`).
        app.register_idle_veto(cymatics_idle_veto);

        // Hand-grab resource: persists across enter/exit cycles (same pattern
        // as `DotsMouseAttractorState`). Task C10 sets the fields; until then
        // both slots are `None` and only mouse/touch drives the centres.
        app.init_resource::<systems::CymaticsHandGrabs>();

        // Hand grab → centres: reads TrackedHand entities, detects grabs, and
        // writes CymaticsHandGrabs. Must run before update_cymatics_centers so
        // the interaction state machine reads fresh grab positions this frame.
        // Runs only while Active (not screensaver — attract mode drives centres
        // itself in Task C13).
        app.add_systems(
            Update,
            systems::update_cymatics_hand_centers
                .before(systems::update_cymatics_centers)
                .run_if(sketch_active(AppState::Cymatics)),
        );

        // Interaction state machine: updates `CymaticsState` from pointer and
        // hand-grab input. Runs only while `Active` (not screensaver — attract
        // drives the centres itself in Task C13). Must run before
        // `update_cymatics_sim_params` so the updated centres are packed into
        // the GPU uniform this frame.
        app.add_systems(
            Update,
            systems::update_cymatics_centers
                .before(update_cymatics_sim_params)
                .run_if(sketch_active(AppState::Cymatics)),
        );

        // Per-frame CPU→GPU bridge. Runs while the sketch is `Active` OR while
        // its screensaver is showing, so the attract/screensaver mode keeps the
        // field animating instead of freezing. This is the single system that
        // advances `CymaticsState::simulation_time` (exactly one system owns it).
        app.add_systems(
            Update,
            update_cymatics_sim_params.run_if(
                // `.or_else` is the non-deprecated equivalent of the run-condition
                // `or` combinator in this Bevy version (same truth table): run while
                // `Active` OR while the screensaver is showing.
                sketch_active(AppState::Cymatics).or_else(in_screensaver(AppState::Cymatics)),
            ),
        );

        // Material knob update: pack skew_intensity (derived from num_cycles +
        // skew_curve setting) and master_brightness into the render material
        // each frame. Same run condition as the sim-params bridge.
        app.add_systems(
            Update,
            update_cymatics_material.run_if(
                sketch_active(AppState::Cymatics).or_else(in_screensaver(AppState::Cymatics)),
            ),
        );

        // Audio coupling: derive v4 audio params from CymaticsState and push
        // them to the ring each frame. Runs only while `Active` — silent in the
        // screensaver (attract mode drives centres but produces no audio).
        // Runs after `update_cymatics_centers` so it reads fresh active_radius
        // and slow_down values; the onset edge then increments slow_down, which
        // `update_cymatics_sim_params` (registered above) packs into the GPU
        // uniform this same frame.
        app.add_systems(
            Update,
            systems::audio_coupling::drive_cymatics_audio
                .after(systems::update_cymatics_centers)
                .run_if(sketch_active(AppState::Cymatics)),
        );
    }
}

/// Register Cymatics's picker-tile metadata into [`wc_core::sketch::SketchManifest`].
///
/// Factored out of [`CymaticsPlugin::build`] so it is independently
/// unit-testable without `CymaticsPlugin`'s rendering dependencies (the shared
/// `CymaticsComputePlugin` and `Material2dPlugin::<CymaticsMaterial>` both
/// require a full `RenderApp` that `MinimalPlugins` does not provide).
///
/// The `AssetServer` load is async; the picker renders the tile as soon as the
/// image asset finishes loading. Before then the tile shows the dark placeholder
/// fill defined in `OverlayStyle`. This mirrors the behavior of
/// [`crate::dots::register_dots_manifest`].
pub(crate) fn register_cymatics_manifest(app: &mut App) {
    let asset_server = app.world().resource::<AssetServer>();
    // Load the picker-tile screenshot as PNG. Bevy's default features include
    // the `png` image loader; JPEG requires the separate `bevy/jpeg` feature
    // which is not enabled in this workspace.
    // v4 calls this sketch "Cymatics" in HomePage.tsx.
    let screenshot = asset_server.load("sketches/cymatics/screenshot.png");
    app.register_sketch_manifest(wc_core::sketch::SketchManifestEntry {
        state: AppState::Cymatics,
        display_name: "Cymatics",
        screenshot,
    });
}

/// Idle veto for the Cymatics sketch. Returns `true` while the alive-mask
/// radius is meaningfully above its resting value — keeps the sketch `Active`
/// so the wave field finishes integrating instead of freezing mid-ripple.
///
/// Returns `false` when [`CymaticsState`] is absent (e.g. after exit), per the
/// registered-veto contract.
fn cymatics_idle_veto(world: &World) -> bool {
    world
        .get_resource::<CymaticsState>()
        .is_some_and(|s| s.active_radius > MINIMUM_ACTIVE_RADIUS + 1e-2)
}

/// `OnEnter(AppState::Cymatics)` — insert the resting [`CymaticsState`].
fn init_cymatics_state(mut commands: Commands<'_, '_>) {
    commands.insert_resource(CymaticsState::default());
}

/// `OnEnter(AppState::Cymatics)` — allocate the two ping-pong textures, spawn
/// the fullscreen quad (sampling texture A), and insert the initial
/// [`CymaticsSimParams`].
///
/// The sim grid resolution follows v4: `vertical_resolution` texels tall ×
/// `round(vertical_resolution · aspect)` texels wide, using the window aspect.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "settings are f32; rounding then converting to the u32 texel grid is intentional, \
              the `.max(1)` keeps the value non-negative before `u32::try_from`, and \
              `MAX_ITERATIONS` (120) trivially fits the clamp bound's integer type"
)]
fn spawn_cymatics(
    mut commands: Commands<'_, '_>,
    mut images: ResMut<'_, Assets<Image>>,
    mut meshes: ResMut<'_, Assets<Mesh>>,
    mut materials: ResMut<'_, Assets<render::CymaticsMaterial>>,
    settings: Res<'_, CymaticsSettings>,
    window: Single<'_, '_, &Window>,
) {
    let win = Vec2::new(window.width().max(1.0), window.height().max(1.0));
    let aspect = win.x / win.y;
    // f32 settings → u32 texel grid: round, floor at 1, fall back to 480 if the
    // (always non-negative) value somehow fails the conversion.
    let vy = u32::try_from((settings.vertical_resolution.round() as i64).max(1)).unwrap_or(480);
    let vx = u32::try_from(((settings.vertical_resolution * aspect).round() as i64).max(1))
        .unwrap_or(480);
    let textures = create_cymatics_textures(vx, vy, &mut images);
    let sim_resolution = Vec2::new(vx as f32, vy as f32);

    render::spawn_cymatics_quad(
        &mut commands,
        &mut meshes,
        &mut materials,
        // The material samples ping-pong texture A directly: the odd-N
        // continuity refresh keeps A holding the latest field at frame end, so
        // no separate display texture (or per-frame blit into it) is needed.
        textures.a.clone(),
        win,
        sim_resolution,
        settings.master_brightness,
    );
    // Tag the texture handles onto a CymaticsRoot entity so `OnExit` frees them.
    commands.spawn((textures.clone(), CymaticsRoot));

    // Sub-steps per frame, clamped to the compute pipeline's slot count.
    let iterations =
        u32::try_from((settings.iterations.round() as i64).clamp(1, MAX_ITERATIONS as i64))
            .unwrap_or(20);
    commands.insert_resource(CymaticsSimParams {
        // Resting v4 physics; centres/radius are overwritten each frame by
        // `update_cymatics_sim_params`. The constructor lives in `sim_params.rs`
        // because `SimParamsGpu`'s pad field is module-private.
        params: SimParamsGpu::with_resting_physics([vx, vy], MINIMUM_ACTIVE_RADIUS),
        // Pre-allocated to MAX_ITERATIONS; refilled each frame with `clear()` +
        // `push` (capacity preserved) so the steady-state path never reallocates.
        iter_times: Vec::with_capacity(MAX_ITERATIONS),
        iterations,
        tex_a: textures.a,
        tex_b: textures.b,
        resolution: UVec2::new(vx, vy),
    });
}

/// `OnExit(AppState::Cymatics)` — drop the per-frame [`CymaticsSimParams`] (so
/// its texture-handle clones are freed and the GPU ref-count reaches zero) and
/// the [`CymaticsState`] (so the idle veto reads `None` → `false` and no stale
/// alive-mask radius leaks into other sketches' idle decisions).
fn remove_cymatics_sim_params(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<CymaticsSimParams>();
    commands.remove_resource::<CymaticsState>();
}

/// Pack [`CymaticsState`] and live physics settings into the extracted
/// [`CymaticsSimParams`] each frame and fill the per-iteration phase times.
///
/// v4: the effective cycle count is `numCycles / (1 + slowDown·3)`, and each of
/// the `N` sub-steps advances the phase by `dt = cycles·2π/N`. The sub-step
/// times are `base + i·dt` for `i in 0..N`. After filling them this advances the
/// phase clock by `N·dt` exactly once — this is the **only** system that mutates
/// [`CymaticsState::simulation_time`] (the audio-coupling stage later takes over
/// the advance; the invariant is that exactly one system owns it).
///
/// Physics fields (`force_multiplier`, `velocity_decay`, `height_decay`,
/// `accumulated_height_decay`) are now read from `CymaticsSettings` each frame
/// so Dev knob changes take effect immediately without a restart.
#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "u32 sub-step index/count → f32 phase math, and the `MAX_ITERATIONS` (120) clamp \
              bound → u32; all values are <= MAX_ITERATIONS"
)]
fn update_cymatics_sim_params(
    mut state: ResMut<'_, CymaticsState>,
    mut sim: ResMut<'_, CymaticsSimParams>,
    settings: Res<'_, CymaticsSettings>,
) {
    sim.params.center = state.center.to_array();
    sim.params.center2 = state.center2.to_array();
    sim.params.active_radius = state.active_radius;

    // Physics knobs: read from settings each frame (live, no restart).
    // `with_resting_physics` seeds these at spawn; the bridge overwrites them
    // every frame so Dev changes are reflected on the next GPU dispatch.
    sim.params.force_multiplier = settings.force_multiplier;
    sim.params.velocity_decay = settings.velocity_decay;
    sim.params.height_decay = settings.height_decay;
    sim.params.accumulated_height_decay = settings.accumulated_height_decay;

    // Defense-in-depth clamp to the compute pipeline's slot count (the dispatch
    // node also clamps); `spawn_cymatics` already clamps at insert time.
    let n = sim.iterations.clamp(1, MAX_ITERATIONS as u32);
    let cycles = state.num_cycles / (1.0 + state.slow_down * 3.0);
    let dt = cycles * std::f32::consts::TAU / n as f32;
    let base = state.simulation_time;

    // `clear()` keeps the MAX_ITERATIONS capacity, so this never reallocates.
    sim.iter_times.clear();
    for i in 0..n {
        sim.iter_times.push(base + i as f32 * dt);
    }

    // Advance the phase clock once per frame (single-owner invariant above).
    state.simulation_time += n as f32 * dt;
}

/// Update the [`render::CymaticsMaterial`] each frame with the current
/// `skew_intensity` (derived from `num_cycles` + `skew_curve` setting) and
/// `master_brightness` (User setting).
///
/// Runs under the same `sketch_active OR in_screensaver` condition as
/// [`update_cymatics_sim_params`] so the material reflects the live state
/// during both active play and the attract screensaver.
///
/// v4 `skewIntensity = pow(max(0, (numCycles - 1.002) / 2 - 0.5), 2)`.
/// The `skew_curve` Dev knob applies an exponent to this raw value before
/// packing into the uniform, allowing a wider or narrower push range.
///
/// ## No-allocation guarantee
///
/// All arithmetic is on stack scalars; no per-frame heap allocation.
fn update_cymatics_material(
    quad_q: Query<'_, '_, &MeshMaterial2d<render::CymaticsMaterial>, With<CymaticsRoot>>,
    mut materials: ResMut<'_, Assets<render::CymaticsMaterial>>,
    settings: Res<'_, CymaticsSettings>,
    state: Option<Res<'_, CymaticsState>>,
) {
    let Some(state) = state else { return };
    for handle in quad_q.iter() {
        let Some(mut mat) = materials.get_mut(&handle.0) else {
            continue;
        };
        // v4: skewIntensity = pow(max(0, (numCycles - 1.002) / 2 - 0.5), 2).
        // DEFAULT_NUM_CYCLES = 1.002; at rest, the clamp yields 0.
        let skew_raw = ((state.num_cycles - DEFAULT_NUM_CYCLES) / 2.0 - 0.5)
            .max(0.0)
            .powi(2);
        // skew_curve exponent: default 1.0 = linear (v4 parity). `powf` with a
        // non-negative base is always well-defined for positive exponents.
        let skew_intensity = skew_raw.powf(settings.skew_curve);
        // Pack into the skew uniform:
        //   .x = skew_intensity  (body-colour push toward white)
        //   .y = master_brightness  (post-render multiplier)
        //   .zw = 0
        mat.skew = Vec4::new(skew_intensity, settings.master_brightness, 0.0, 0.0);
    }
}

/// Listen for [`wc_core::settings::SketchRestart`] events targeted at
/// `CymaticsSettings` and begin the fade-out/reload cycle after a 500 ms
/// debounce window.
///
/// Only arms when in `AppState::Cymatics` and no reload is already in
/// progress, preventing double-fires during the return leg. Mirrors
/// `restart_on_dots_settings_change` in `dots/mod.rs`.
fn restart_on_cymatics_settings_change(
    mut events: MessageReader<'_, '_, wc_core::settings::SketchRestart>,
    time: Res<'_, Time>,
    current: Res<'_, State<AppState>>,
    mut reload_state: ResMut<'_, SketchReloadState>,
    // Optional: absent in headless test harnesses (no cpal audio stream).
    audio_state: Option<Res<'_, AudioState>>,
    // Tracks the `Time::elapsed` of the last received restart message;
    // `None` means no pending restart since the last reload.
    mut last_change_at: Local<'_, Option<std::time::Duration>>,
) {
    // Absorb any new restart messages for our settings key and reset the
    // debounce timestamp. Only arm when in Cymatics and no reload in progress.
    let got_message = events
        .read()
        .any(|e| e.storage_key == CymaticsSettings::STORAGE_KEY);
    if got_message && **current == AppState::Cymatics && reload_state.is_idle() {
        *last_change_at = Some(time.elapsed());
        tracing::debug!("CymaticsSettings changed — debounce timer reset (500 ms)");
    }

    // Fire the FadeOut only after 500 ms of no further changes.
    if let Some(last) = *last_change_at {
        let elapsed_since = time.elapsed().saturating_sub(last);
        if elapsed_since >= RESTART_DEBOUNCE
            && **current == AppState::Cymatics
            && reload_state.is_idle()
        {
            let pre_fade_volume = audio_state.as_ref().map_or(1.0, |s| s.volume);
            reload_state.begin_fade_out(time.elapsed(), pre_fade_volume, AppState::Cymatics);
            *last_change_at = None;
            tracing::debug!("CymaticsSettings debounce elapsed — beginning reload FadeOut");
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

    /// `cymatics_idle_veto`: `false` when absent or at rest, `true` once the
    /// alive-mask radius is meaningfully above its resting value.
    #[test]
    fn idle_veto_tracks_active_radius() {
        let mut world = World::new();

        // No resource → veto false (nothing in flight).
        assert!(
            !cymatics_idle_veto(&world),
            "veto must be false when CymaticsState is absent"
        );

        // Resting radius → veto false (sketch may idle).
        world.insert_resource(CymaticsState::default());
        assert!(
            !cymatics_idle_veto(&world),
            "veto must be false at the resting active_radius"
        );

        // Raised radius (as interaction would) → veto true.
        world.resource_mut::<CymaticsState>().active_radius = MINIMUM_ACTIVE_RADIUS + 0.2;
        assert!(
            cymatics_idle_veto(&world),
            "veto must be true while active_radius is above rest"
        );
    }

    /// `remove_cymatics_sim_params` drops both `CymaticsSimParams` and
    /// `CymaticsState` on exit (VRAM release + no stale veto).
    #[test]
    fn remove_drops_sim_params_and_state() {
        let mut world = World::new();
        world.insert_resource(CymaticsState::default());
        world.insert_resource(CymaticsSimParams {
            params: SimParamsGpu::with_resting_physics([640, 480], MINIMUM_ACTIVE_RADIUS),
            iter_times: Vec::with_capacity(MAX_ITERATIONS),
            iterations: 20,
            tex_a: Handle::default(),
            tex_b: Handle::default(),
            resolution: UVec2::new(640, 480),
        });

        world
            .run_system_once(remove_cymatics_sim_params)
            .expect("remove_cymatics_sim_params run");

        assert!(
            world.get_resource::<CymaticsSimParams>().is_none(),
            "CymaticsSimParams must be removed on exit"
        );
        assert!(
            world.get_resource::<CymaticsState>().is_none(),
            "CymaticsState must be removed on exit so the idle veto reads None"
        );
    }

    /// `update_cymatics_sim_params` fills `iter_times` to `iterations` entries
    /// (`base + i·dt`), advances `simulation_time` exactly once by `N·dt`, and
    /// never reallocates `iter_times` (capacity preserved across `clear()`).
    #[test]
    #[allow(
        clippy::as_conversions,
        clippy::cast_precision_loss,
        reason = "test arithmetic mirrors the system's u32→f32 phase math"
    )]
    fn update_fills_iter_times_and_advances_time_once() {
        let mut world = World::new();
        world.insert_resource(CymaticsState::default());
        world.insert_resource(CymaticsSettings::default());
        world.insert_resource(CymaticsSimParams {
            params: SimParamsGpu::with_resting_physics([640, 480], MINIMUM_ACTIVE_RADIUS),
            iter_times: Vec::with_capacity(MAX_ITERATIONS),
            iterations: 20,
            tex_a: Handle::default(),
            tex_b: Handle::default(),
            resolution: UVec2::new(640, 480),
        });

        world
            .run_system_once(update_cymatics_sim_params)
            .expect("update_cymatics_sim_params run");

        // cycles = 1.002 / (1 + 0·3) = 1.002; dt = cycles·2π / 20.
        let cycles = DEFAULT_NUM_CYCLES;
        let dt = cycles * std::f32::consts::TAU / 20.0;

        let sim = world.resource::<CymaticsSimParams>();
        assert_eq!(sim.iter_times.len(), 20, "iter_times length must equal N");
        assert!(
            sim.iter_times.capacity() >= MAX_ITERATIONS,
            "capacity must be preserved (no reallocation)"
        );
        assert!(
            sim.iter_times[0].abs() < 1e-6,
            "first sub-step time must be the base phase (0.0)"
        );
        assert!(
            (sim.iter_times[1] - dt).abs() < 1e-4,
            "sub-step spacing must be dt"
        );
        // Centres packed through in top-left UV with no y-flip.
        assert!(
            (sim.params.center[0] - 0.5).abs() < 1e-6 && (sim.params.center[1] - 0.5).abs() < 1e-6,
            "center must pass through in top-left UV (no y-flip)"
        );
        assert!(
            (sim.params.center2[0] - 0.5).abs() < 1e-6
                && (sim.params.center2[1] - 0.5).abs() < 1e-6,
            "center2 must pass through in top-left UV (no y-flip)"
        );

        let state = world.resource::<CymaticsState>();
        assert!(
            (state.simulation_time - 20.0 * dt).abs() < 1e-3,
            "simulation_time must advance by exactly N·dt once"
        );
    }

    /// Changing `force_multiplier` in `CymaticsSettings` is reflected in the
    /// packed `SimParamsGpu` after one run of `update_cymatics_sim_params`.
    /// Confirms that physics knobs are live (not dead settings).
    #[test]
    fn force_multiplier_setting_is_packed_live() {
        let mut world = World::new();
        world.insert_resource(CymaticsState::default());
        world.insert_resource(CymaticsSettings {
            force_multiplier: 0.5,
            ..CymaticsSettings::default()
        });
        world.insert_resource(CymaticsSimParams {
            params: SimParamsGpu::with_resting_physics([640, 480], MINIMUM_ACTIVE_RADIUS),
            iter_times: Vec::with_capacity(MAX_ITERATIONS),
            iterations: 20,
            tex_a: Handle::default(),
            tex_b: Handle::default(),
            resolution: UVec2::new(640, 480),
        });

        world
            .run_system_once(update_cymatics_sim_params)
            .expect("update_cymatics_sim_params run");

        let sim = world.resource::<CymaticsSimParams>();
        assert!(
            (sim.params.force_multiplier - 0.5).abs() < f32::EPSILON,
            "force_multiplier must be read from settings (expected 0.5, got {})",
            sim.params.force_multiplier
        );
    }

    /// Verifies that `register_cymatics_manifest` appends an entry for
    /// `AppState::Cymatics` with the correct display name.
    ///
    /// Uses the free-function path rather than constructing the full
    /// `CymaticsPlugin` because `CymaticsPlugin::build` adds rendering plugins
    /// that require a real `RenderApp` — unavailable in headless unit tests.
    /// Mirrors `register_dots_manifest_appends_entry` in `crate::dots::tests`.
    #[test]
    fn register_cymatics_manifest_appends_entry() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::asset::AssetPlugin::default());
        // `ImagePlugin` registers `Image` as an asset type so `AssetServer`
        // can allocate a `Handle<Image>` for the screenshot path.
        app.add_plugins(bevy::image::ImagePlugin::default());
        register_cymatics_manifest(&mut app);
        let manifest = app.world().resource::<SketchManifest>();
        let entry = manifest
            .get(AppState::Cymatics)
            .expect("Cymatics manifest entry should be registered");
        assert_eq!(entry.display_name, "Cymatics");
    }
}
