//! Cymatics sketch: a 2D wave-field simulation (ping-pong storage-texture
//! compute) rendered fullscreen.
//!
//! ## Data flow (wired so far)
//!
//! 1. `OnEnter(AppState::Cymatics)` runs [`init_cymatics_state`] (insert the
//!    CPU-side [`CymaticsState`] defaults) then [`spawn_cymatics`] (read
//!    [`settings::CymaticsSettings`] → derive the sim resolution from the window
//!    aspect → allocate the ping-pong + display textures
//!    ([`compute::create_cymatics_textures`]) → spawn the fullscreen quad
//!    ([`render::spawn_cymatics_quad`]) → tag the texture handles onto a
//!    [`CymaticsRoot`] entity → insert the initial [`compute::CymaticsSimParams`]).
//! 2. Every `Update` while the sketch is `Active` **or** showing its
//!    screensaver, [`update_cymatics_sim_params`] packs [`CymaticsState`] into
//!    the extracted [`compute::CymaticsSimParams`] (centres, alive radius, and
//!    the per-iteration phase times) and advances the phase clock once.
//! 3. The render world extracts `CymaticsSimParams`;
//!    [`compute::CymaticsComputePlugin`] advances the wave field on the GPU
//!    (`assets/shaders/cymatics/simulate.wgsl`), and [`render::CymaticsMaterial`]
//!    samples the display texture (`assets/shaders/cymatics/render.wgsl`).
//! 4. `OnExit(AppState::Cymatics)` despawns the [`CymaticsRoot`] entity tree
//!    (frees VRAM) and drops [`compute::CymaticsSimParams`] + [`CymaticsState`].
//!
//! The shared [`compute::CymaticsComputePlugin`] and the
//! `Material2dPlugin::<CymaticsMaterial>` are registered once by the
//! [`crate::SketchesPlugin`] umbrella, not here.
//!
//! Mouse/hand interaction (which drives the two wave centres), the faithful
//! audio coupling derived from the same `CymaticsState`, the wandering attract
//! mode, and the shared bloomed hand-mesh overlay arrive in later stages; their
//! systems will slot into the lifecycle and `Update` chain established here.

pub mod compute;
pub mod render;
pub mod settings;

use bevy::prelude::*;
use wc_core::lifecycle::screensaver::in_screensaver;
use wc_core::lifecycle::state::AppState;
use wc_core::lifecycle::RegisterIdleVetoExt;
use wc_core::settings::RegisterSketchSettingsExt;
use wc_core::sketch::{despawn_with, sketch_active, RegisterSketchManifestExt};

use compute::{create_cymatics_textures, CymaticsSimParams, SimParamsGpu, MAX_ITERATIONS};
use settings::CymaticsSettings;

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
/// tagged with it via [`wc_core::sketch::despawn_with`], releasing the
/// ping-pong/display textures' VRAM each enter/exit cycle.
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

        // Lifecycle: allocate the textures + spawn the quad on enter, despawn
        // and release VRAM on exit. Audio + interaction systems join these
        // schedules in later stages.
        app.add_systems(
            OnEnter(AppState::Cymatics),
            (init_cymatics_state, spawn_cymatics).chain(),
        );
        app.add_systems(
            OnExit(AppState::Cymatics),
            (despawn_with::<CymaticsRoot>, remove_cymatics_sim_params),
        );

        // Idle veto: stay `Active` while the field is still energised (the mask
        // radius is above rest), so the wave keeps integrating until it settles
        // rather than freezing the instant the idle timer trips. Vetoes read
        // `World` and return `false` when `CymaticsState` is absent, which is
        // why `OnExit` drops the resource (see `remove_cymatics_sim_params`).
        app.register_idle_veto(cymatics_idle_veto);

        // Per-frame CPU→GPU bridge. Runs while the sketch is `Active` OR while
        // its screensaver is showing, so the attract/screensaver mode keeps the
        // field animating instead of freezing. This is the single system that
        // advances `CymaticsState::simulation_time` (the audio-coupling system
        // takes over that advance in a later stage; exactly one system must).
        app.add_systems(
            Update,
            update_cymatics_sim_params.run_if(
                // `.or_else` is the non-deprecated equivalent of the run-condition
                // `or` combinator in this Bevy version (same truth table): run while
                // `Active` OR while the screensaver is showing.
                sketch_active(AppState::Cymatics).or_else(in_screensaver(AppState::Cymatics)),
            ),
        );
    }
}

/// Register Cymatics's picker-tile metadata into [`wc_core::sketch::SketchManifest`].
///
/// Minimal C8 registration: the display name only, with a default (unloaded)
/// screenshot handle so the picker renders the placeholder fill. The real
/// screenshot asset is loaded in the manifest-tile stage; registration is
/// idempotent on `state`, so that later registration cleanly overwrites this one.
pub(crate) fn register_cymatics_manifest(app: &mut App) {
    app.register_sketch_manifest(wc_core::sketch::SketchManifestEntry {
        state: AppState::Cymatics,
        display_name: "Cymatics",
        screenshot: Handle::default(),
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

/// `OnEnter(AppState::Cymatics)` — allocate the ping-pong/display textures,
/// spawn the fullscreen quad, and insert the initial [`CymaticsSimParams`].
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
    let vx =
        u32::try_from(((settings.vertical_resolution * aspect).round() as i64).max(1)).unwrap_or(480);
    let textures = create_cymatics_textures(vx, vy, &mut images);
    let sim_resolution = Vec2::new(vx as f32, vy as f32);

    render::spawn_cymatics_quad(
        &mut commands,
        &mut meshes,
        &mut materials,
        textures.display.clone(),
        win,
        sim_resolution,
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
        display: textures.display,
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

/// Pack [`CymaticsState`] into the extracted [`CymaticsSimParams`] each frame
/// and fill the per-iteration phase times.
///
/// v4: the effective cycle count is `numCycles / (1 + slowDown·3)`, and each of
/// the `N` sub-steps advances the phase by `dt = cycles·2π/N`. The sub-step
/// times are `base + i·dt` for `i in 0..N`. After filling them this advances the
/// phase clock by `N·dt` exactly once — this is the **only** system that mutates
/// [`CymaticsState::simulation_time`] (the audio-coupling stage later takes over
/// the advance; the invariant is that exactly one system owns it).
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
) {
    sim.params.center = state.center.to_array();
    sim.params.center2 = state.center2.to_array();
    sim.params.active_radius = state.active_radius;

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

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected None is the correct behaviour"
)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

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
            display: Handle::default(),
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
        world.insert_resource(CymaticsSimParams {
            params: SimParamsGpu::with_resting_physics([640, 480], MINIMUM_ACTIVE_RADIUS),
            iter_times: Vec::with_capacity(MAX_ITERATIONS),
            iterations: 20,
            tex_a: Handle::default(),
            tex_b: Handle::default(),
            display: Handle::default(),
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
}
