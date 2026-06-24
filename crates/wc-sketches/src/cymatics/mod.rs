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
//! 2. Every `Update` while the sketch is `Active`, through the `Idle` pre-roll,
//!    **or** showing its screensaver, [`update_cymatics_sim_params`] packs
//!    [`CymaticsState`] into the extracted [`compute::CymaticsSimParams`]
//!    (centres, alive radius, and the per-iteration phase times) and advances
//!    the phase clock once — so the resting field never freezes during the
//!    pre-screensaver `Idle` window.
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
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;
use wc_core::lifecycle::screensaver::in_screensaver;
use wc_core::lifecycle::state::{AppState, SketchActivity};
use wc_core::lifecycle::RegisterIdleVetoExt;
use wc_core::settings::{RegisterSketchSettingsExt, SketchSettings};
use wc_core::sketch::{despawn_with, in_idle, sketch_active, RegisterSketchManifestExt};

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

/// Upper bound (in phase units) on [`CymaticsState::ramp_time`], the alive-bloom
/// ramp clock fed to the shader's `(time-500)/500` ramp. That ramp saturates at
/// `+0.8` once the clock reaches `900`, so capping just past it keeps the clock
/// bounded over a multi-hour soak (an unbounded f32 would lose precision) while
/// leaving the already-saturated bloom unchanged. Distinct from the oscillator
/// phase, which is wrapped mod TAU instead (see [`update_cymatics_sim_params`]).
const RAMP_TIME_CAP: f32 = 1000.0;

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
    /// Oscillator phase clock (v4 `simulationTime`), advanced `N·dt` per frame
    /// and **wrapped to `[0, TAU)`** each frame. Only the wave source's `sin`
    /// reads it, and `sin` is periodic, so wrapping is mathematically exact while
    /// keeping the argument small enough that f32 holds full precision over a
    /// multi-hour soak (an unbounded clock reaches ~10.9M rad in 8 h, far past
    /// where f32's 24-bit mantissa loses sub-radian precision). Distinct from
    /// [`Self::ramp_time`], which the alive-bloom ramp needs unwrapped.
    pub simulation_time: f32,
    /// Alive-bloom ramp clock — the elapsed-time value fed to the shader's
    /// `(time-500)/500` bloom ramp. Advanced `N·dt` per frame like
    /// [`Self::simulation_time`] but **capped at [`RAMP_TIME_CAP`]** rather than
    /// wrapped: the bloom needs real elapsed time, not phase, and saturates at
    /// `+0.8` by `900`, so the cap keeps it bounded without changing the
    /// saturated bloom. Equals `simulation_time` until the phase first wraps.
    pub ramp_time: f32,
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
            ramp_time: 0.0,
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

        // Per-frame CPU→GPU bridge. Runs while the sketch is `Active`, through
        // the 30–60 s `Idle` pre-roll, AND while its screensaver is showing, so
        // the field keeps animating instead of freezing. This is the single
        // system that advances `CymaticsState::simulation_time` (exactly one
        // system owns it).
        //
        // The `Idle` leg is a deliberate, narrow exception to "zero systems when
        // idle": without it the phase clock stops, the wave source
        // `source_amplitude·sin(time)` goes constant, and the resting field
        // visibly freezes for the 30 s
        // before the screensaver (the operator reads that as the screensaver
        // freezing). Only this bridge is extended into `Idle`; the attract
        // driver stays `in_screensaver`-only, so through `Idle` `active_radius`
        // keeps its decayed resting value and the idle veto does not flap.
        app.add_systems(
            Update,
            update_cymatics_sim_params.run_if(
                // `.or_else` is the non-deprecated equivalent of the run-condition
                // `or` combinator in this Bevy version (same truth table): run
                // while `Active`, OR through `Idle`, OR while the screensaver shows.
                sketch_active(AppState::Cymatics)
                    .or_else(in_idle(AppState::Cymatics))
                    .or_else(in_screensaver(AppState::Cymatics)),
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
///
/// ## Why it yields once the screensaver is showing
///
/// The veto's only legitimate job is to hold `Active` while the field rings
/// down from a *real* interaction. Once `SketchActivity::Screensaver` is up the
/// attract driver ([`screensaver::drive_cymatics_attract`]) writes
/// `active_radius` itself (≥ `attract_radius`), which the radius check below
/// would read as "still energised" and veto — bouncing the state straight back
/// to `Active`. `step_centers` then decays the radius for ~10 s until the veto
/// clears, the state re-enters `Screensaver`, the attract driver jumps the
/// radius again, and the alive-mask blooms: a periodic phantom pulse roughly
/// every 10 s, driven entirely by the veto reading the very field the attract
/// driver raised. Returning `false` in `Screensaver` breaks that loop; with no
/// input `idle_for` stays > 60 s, so the screensaver holds on its own.
fn cymatics_idle_veto(world: &World) -> bool {
    // Already in the screensaver: the attract driver owns `active_radius`, so a
    // veto here would only flap the state against itself (see the doc above).
    if world
        .get_resource::<State<SketchActivity>>()
        .is_some_and(|a| *a.get() == SketchActivity::Screensaver)
    {
        return false;
    }
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
        settings.gamma,
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
        // Phase/ramp scalars; overwritten each frame by `update_cymatics_sim_params`.
        // Carried as scalars (not a Vec) so the per-frame extract clone never
        // allocates — sub-step i's phase is `phase_base + i·phase_dt`, its ramp
        // time `ramp_base + i·phase_dt`.
        phase_base: 0.0,
        ramp_base: 0.0,
        phase_dt: 0.0,
        // Wave-source amplitude; overwritten each frame from the live setting.
        source_amplitude: settings.source_amplitude,
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
/// [`CymaticsSimParams`] each frame and store the per-iteration phase scalars.
///
/// v4: the effective cycle count is `numCycles / (1 + slowDown·3)`, and each of
/// the `N` sub-steps advances the phase by `dt = cycles·2π/N`. The sub-step
/// times are `base + i·dt` for `i in 0..N`; rather than materialise that as a
/// per-frame `Vec`, this stores `base`/`dt` into `phase_base`/`phase_dt` and the
/// render-world prepare step recomputes each slot's time. After storing them
/// this advances the phase clock by `N·dt` exactly once — this is the **only**
/// system that mutates [`CymaticsState::simulation_time`] (the audio-coupling
/// stage later takes over the advance; the invariant is that exactly one system
/// owns it).
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

    // Wave-source amplitude: live setting, applied CPU-side when the prepare
    // step precomputes each sub-step's `wave_signal = source_amplitude·sin(phase)`.
    sim.source_amplitude = settings.source_amplitude;

    // Defense-in-depth clamp to the compute pipeline's slot count (the dispatch
    // node also clamps); `spawn_cymatics` already clamps at insert time.
    let n = sim.iterations.clamp(1, MAX_ITERATIONS as u32);
    let cycles = state.num_cycles / (1.0 + state.slow_down * 3.0);
    let dt = cycles * std::f32::consts::TAU / n as f32;
    // Both clocks at frame start. They are equal until the phase first wraps.
    let phase_base = state.simulation_time;
    let ramp_base = state.ramp_time;

    // Store the two clock bases; the render-world prepare step recomputes each
    // sub-step's phase as `phase_base + i·phase_dt` and ramp time as
    // `ramp_base + i·phase_dt` (no per-frame Vec, so the extract clone never
    // allocates).
    sim.phase_base = phase_base;
    sim.ramp_base = ramp_base;
    sim.phase_dt = dt;

    // Advance both clocks once per frame (single-owner invariant above), each
    // bounded so it stays precise/finite over a multi-hour soak: the oscillator
    // phase wraps mod TAU (sin is periodic → exact, but the argument stays small)
    // and the alive-bloom clock caps at RAMP_TIME_CAP (the bloom is already
    // saturated by then). For the first ~900 phase units — before any wrap or cap
    // bites — both still equal the old unbounded `simulation_time`, so nothing
    // visible changes in normal use; this only bounds the long-run growth.
    let advance = n as f32 * dt;
    let (next_phase, next_ramp) = advance_clocks(phase_base, ramp_base, advance);
    state.simulation_time = next_phase;
    state.ramp_time = next_ramp;
}

/// Advance the two per-frame Cymatics clocks by `advance` (= `N·dt`), returning
/// `(next_phase, next_ramp)`.
///
/// The oscillator phase is wrapped to `[0, TAU)` via `rem_euclid` (sin is
/// periodic, so wrapping by whole turns is exact, yet the stored value stays
/// small enough that f32 keeps full precision over an 8-hour soak). The
/// alive-bloom ramp clock is capped at [`RAMP_TIME_CAP`] instead of wrapped — it
/// feeds the shader's `(time-500)/500` ramp, which needs real elapsed time and
/// saturates at `+0.8` by `900`, so capping just past that bounds it without
/// changing the saturated bloom.
///
/// Pure (no world access) so the long-soak boundedness/precision invariants are
/// unit-testable in a tight in-process loop without ECS overhead;
/// [`update_cymatics_sim_params`] calls it directly, so the test guards the real
/// path.
fn advance_clocks(phase: f32, ramp: f32, advance: f32) -> (f32, f32) {
    (
        (phase + advance).rem_euclid(std::f32::consts::TAU),
        (ramp + advance).min(RAMP_TIME_CAP),
    )
}

/// Update the [`render::CymaticsMaterial`] each frame with the current
/// `skew_intensity` (derived from `num_cycles` + `skew_curve` setting),
/// brightness (`master_brightness` × the screensaver lift), and `gamma`.
///
/// Runs under `sketch_active OR in_screensaver` so the material reflects the
/// live state during both active play and the attract screensaver. Unlike
/// [`update_cymatics_sim_params`], this is *not* extended into the `Idle`
/// pre-roll: its inputs (`skew_curve`, `master_brightness`, `gamma`) are all
/// pinned without interaction, so re-running it through `Idle` would only
/// re-pack an identical uniform.
///
/// v4 `skewIntensity = pow(max(0, (numCycles - 1.002) / 2 - 0.5), 2)`.
/// The `skew_curve` Dev knob applies an exponent to this raw value before
/// packing into the uniform, allowing a wider or narrower push range.
///
/// ## Screensaver brightness lift
///
/// The packed `master_brightness` channel is scaled by
/// `1 + fade.alpha() × (attract_brightness − 1)`. At fade = 0 (Active) the
/// factor is exactly `1.0`, so active rendering is byte-identical to before
/// this knob existed; at fade = 1 (Screensaver) it reaches `attract_brightness`,
/// lifting the gentle linear field up the `AgX` curve so it stays vivid rather
/// than landing in `AgX`'s dark, desaturated toe. See
/// [`CymaticsSettings::attract_brightness`].
///
/// ## Change-gated upload
///
/// `materials.get_mut` marks the material asset `Changed`, which forces the
/// render world to re-extract and re-upload its 32-byte uniform. Taking that
/// borrow unconditionally every frame would re-upload an identical uniform on
/// every frame of the multi-hour at-rest screensaver. So this reads the
/// current packed `skew` via `materials.get` first and only mutates when the
/// freshly-packed `Vec4` differs. The [`ScreensaverFade`] ramp moves
/// `fade.alpha()` each frame for ~1.5 s on screensaver enter/wake, so the
/// brightness channel flips a bit and the uniform re-uploads across the ramp;
/// once the fade settles (a constant `0` or `1`) every input is pinned, the
/// packed `Vec4` is bit-stable frame to frame, and the exact compare holds (no
/// epsilon needed). Any real knob change likewise flips a bit and triggers the
/// upload.
///
/// ## No-allocation guarantee
///
/// All arithmetic is on stack scalars; no per-frame heap allocation.
fn update_cymatics_material(
    quad_q: Query<'_, '_, &MeshMaterial2d<render::CymaticsMaterial>, With<CymaticsRoot>>,
    mut materials: ResMut<'_, Assets<render::CymaticsMaterial>>,
    settings: Res<'_, CymaticsSettings>,
    fade: Res<'_, ScreensaverFade>,
    state: Option<Res<'_, CymaticsState>>,
) {
    let Some(state) = state else { return };
    // Screensaver brightness lift: at fade = 0 (Active) the factor is ×1.0, so
    // the packed uniform — and therefore the rendered frame — is byte-identical
    // to before this knob existed. As the screensaver fades in, the factor
    // ramps toward ×attract_brightness, lifting the whole linear field up the
    // AgX curve (orange crests reach the vivid shoulder; navy lifts off pure
    // black) without sharpening the gentle waves — a uniform pre-AgX multiply.
    let brightness = settings.master_brightness
        * (1.0 + (settings.attract_brightness - 1.0) * fade.alpha());
    for handle in quad_q.iter() {
        // v4: skewIntensity = pow(max(0, (numCycles - 1.002) / 2 - 0.5), 2).
        // DEFAULT_NUM_CYCLES = 1.002; at rest, the clamp yields 0.
        let skew_raw = ((state.num_cycles - DEFAULT_NUM_CYCLES) / 2.0 - 0.5)
            .max(0.0)
            .powi(2);
        // skew_curve exponent: default 1.0 = linear (v4 parity). `powf` with a
        // non-negative base is always well-defined for positive exponents.
        let skew_intensity = skew_raw.powf(settings.skew_curve);
        // Pack into the skew uniform:
        //   .x = skew_intensity   (body-colour push toward white)
        //   .y = brightness       (master_brightness × screensaver lift)
        //   .z = gamma            (per-channel display gamma; 1.0 = identity)
        //   .w = 0 (reserved)
        let new_skew = Vec4::new(skew_intensity, brightness, settings.gamma, 0.0);

        // Skip the mutation (and the Changed flag + re-extract/re-upload it
        // triggers) when the packed uniform is unchanged. The immutable `get`
        // borrow is confined to this expression, so the `get_mut` below is free
        // of a borrow conflict.
        if materials
            .get(&handle.0)
            .is_some_and(|mat| mat.skew == new_skew)
        {
            continue;
        }
        if let Some(mut mat) = materials.get_mut(&handle.0) {
            mat.skew = new_skew;
        }
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

    /// `cymatics_idle_veto`: `false` once `SketchActivity::Screensaver` is
    /// showing, even with a raised `active_radius`. The attract driver writes
    /// `active_radius` itself in the screensaver, so a veto there would bounce
    /// the state back to `Active` and flap (the periodic phantom pulse).
    #[test]
    fn idle_veto_is_false_in_screensaver() {
        let mut world = World::new();
        // A raised radius that would normally trip the veto…
        world.insert_resource(CymaticsState {
            active_radius: MINIMUM_ACTIVE_RADIUS + 0.2,
            ..CymaticsState::default()
        });
        assert!(
            cymatics_idle_veto(&world),
            "sanity: a raised radius vetoes while not in the screensaver"
        );

        // …must be suppressed once the screensaver is the current activity.
        world.insert_resource(State::new(SketchActivity::Screensaver));
        assert!(
            !cymatics_idle_veto(&world),
            "veto must be false in Screensaver so the attract driver doesn't flap the state"
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
            phase_base: 0.0,
            ramp_base: 0.0,
            phase_dt: 0.0,
            source_amplitude: 3.0,
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

    /// `update_cymatics_sim_params` stores the clock bases (`phase_base` =
    /// wrapped phase at frame start, `ramp_base` = ramp clock at frame start,
    /// `phase_dt` = dt) and advances both clocks exactly once: `simulation_time`
    /// (phase) by `N·dt` then wrapped mod TAU, and `ramp_time` by `N·dt` (here
    /// `N·dt ≈ 6.3 ≪ RAMP_TIME_CAP`, so the cap does not bite). Sub-step `i`'s
    /// phase is reconstructed as `phase_base + i·phase_dt` by the prepare step.
    #[test]
    #[allow(
        clippy::as_conversions,
        clippy::cast_precision_loss,
        reason = "test arithmetic mirrors the system's u32→f32 phase math"
    )]
    fn update_sets_phase_scalars_and_advances_time_once() {
        let mut world = World::new();
        world.insert_resource(CymaticsState::default());
        world.insert_resource(CymaticsSettings::default());
        world.insert_resource(CymaticsSimParams {
            params: SimParamsGpu::with_resting_physics([640, 480], MINIMUM_ACTIVE_RADIUS),
            phase_base: 0.0,
            ramp_base: 0.0,
            phase_dt: 0.0,
            source_amplitude: 3.0,
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
        assert!(
            sim.phase_base.abs() < 1e-6,
            "phase_base must be the base phase (0.0)"
        );
        assert!(
            sim.ramp_base.abs() < 1e-6,
            "ramp_base must be the base ramp clock (0.0)"
        );
        assert!(
            (sim.phase_dt - dt).abs() < 1e-4,
            "phase_dt must equal the per-sub-step spacing dt"
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
        // 20·dt = 1.002·TAU > TAU, so the phase wraps once: it equals
        // (20·dt) mod TAU ≈ 0.002·TAU, and stays inside [0, TAU).
        let wrapped = (20.0 * dt).rem_euclid(std::f32::consts::TAU);
        assert!(
            (state.simulation_time - wrapped).abs() < 1e-3,
            "simulation_time must advance N·dt then wrap mod TAU (expected {wrapped}, got {})",
            state.simulation_time
        );
        assert!(
            (0.0..std::f32::consts::TAU).contains(&state.simulation_time),
            "wrapped phase must stay in [0, TAU)"
        );
        // The ramp clock advances by the same N·dt but is NOT wrapped (and here
        // 20·dt ≈ 6.3 is far below RAMP_TIME_CAP, so it is uncapped).
        assert!(
            (state.ramp_time - 20.0 * dt).abs() < 1e-3,
            "ramp_time must advance by exactly N·dt once (unwrapped, uncapped here)"
        );
    }

    /// Soak invariant: over an 8-hour run (60 fps) `advance_clocks` — the
    /// per-frame core of `update_cymatics_sim_params` — keeps the oscillator
    /// phase bounded in `[0, TAU)` and full-precision, and the alive-bloom ramp
    /// clock bounded by `RAMP_TIME_CAP`. An unbounded phase would instead reach
    /// ~10.9M rad, where f32's 24-bit mantissa can't hold the sub-radian phase
    /// the wave source's `sin()` needs (it would quantize / go erratic). Runs the
    /// real helper in a tight in-process loop — no sleeping, no ECS overhead.
    #[test]
    fn phase_wraps_and_ramp_stays_bounded_over_long_soak() {
        let tau32 = std::f32::consts::TAU;
        // Resting cadence: 20 sub-steps/frame at the default cycle count.
        let dt = DEFAULT_NUM_CYCLES * tau32 / 20.0;
        let advance = 20.0 * dt; // N·dt per frame ≈ 1.002·TAU ≈ 6.296 rad.
        let frames = 60 * 60 * 60 * 8; // 8 h at 60 fps = 1,728,000 frames.

        let mut phase = 0.0_f32; // real path: wrapped via advance_clocks
        let mut ramp = 0.0_f32; // real path: capped via advance_clocks
        for _ in 0..frames {
            let (next_phase, next_ramp) = advance_clocks(phase, ramp, advance);
            phase = next_phase;
            ramp = next_ramp;
            // (1) phase stays bounded in [0, TAU); (2) ramp stays ≤ the cap.
            assert!(
                (0.0..tau32).contains(&phase),
                "phase escaped [0, TAU): {phase}"
            );
            assert!(ramp <= RAMP_TIME_CAP, "ramp exceeded RAMP_TIME_CAP: {ramp}");
        }

        // (1) final phase still bounded; (2) the ramp clock has saturated exactly
        // at the cap (it passed 900 long ago, so the bloom is pinned at +0.8).
        assert!(
            (0.0..tau32).contains(&phase),
            "final phase must be in [0, TAU), got {phase}"
        );
        assert!(
            (ramp - RAMP_TIME_CAP).abs() < 1e-3,
            "ramp must saturate at RAMP_TIME_CAP after a long run, got {ramp}"
        );

        // (3) no precision degradation. The wrapped phase stays small, so `sin()`
        // is evaluated at full f32 precision and the per-frame advance registers
        // intact — `(phase + advance) - phase` round-trips back to `advance`.
        // (Note: the *absolute* phase still drifts slowly over 8 h, as any f32
        // accumulation does; what matters is that the stored phase never enters
        // the magnitude where the increment itself is lost — see the contrast.)
        let s = phase.sin();
        assert!(
            s.is_finite() && (-1.0..=1.0).contains(&s),
            "sin(phase) must stay sane (finite, in [-1, 1]), got {s}"
        );
        let wrapped_step = (phase + advance) - phase;
        assert!(
            (wrapped_step - advance).abs() < 1e-3,
            "wrapped phase preserves the per-frame increment ({advance}), got {wrapped_step}"
        );

        // Contrast: an UNbounded clock reaches the 8-hour magnitude (~10.9M rad,
        // f64-exact below), where f32's ulp is ≥ 1 rad. The very same `+ advance`
        // then loses most of the sub-radian increment every frame, so the stored
        // phase quantizes and the wave source goes erratic — exactly the gap the
        // wrap closes.
        let true_phase = f64::from(frames) * f64::from(advance);
        assert!(
            true_phase > 1.0e7,
            "8 h of phase exceeds 10M rad, got {true_phase}"
        );
        let soak_mag = 1.088e7_f32; // representative late-soak magnitude; f32 ulp ≈ 1 rad here
        let corrupted_step = (soak_mag + advance) - soak_mag;
        assert!(
            (corrupted_step - advance).abs() > 0.1,
            "an unbounded f32 clock at soak magnitude corrupts the per-frame increment \
             (advance {advance}, got {corrupted_step})"
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
            phase_base: 0.0,
            ramp_base: 0.0,
            phase_dt: 0.0,
            source_amplitude: 3.0,
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
