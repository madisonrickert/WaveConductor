//! Cymatics sketch: a 2D wave-field simulation (ping-pong storage-texture
//! compute) rendered fullscreen.
//!
//! ## Data flow (wired so far)
//!
//! 1. `OnEnter(AppState::Cymatics)` runs `init_cymatics_state` (insert a
//!    warm-started [`CymaticsState`] — see `warm_start_state`'s doc — so the
//!    field shows two distinct blobs immediately instead of a blank one) then
//!    `spawn_cymatics` (read
//!    [`settings::CymaticsSettings`] → derive the sim resolution from the window
//!    aspect → allocate the two ping-pong textures
//!    ([`compute::create_cymatics_textures`]) → spawn the fullscreen quad
//!    ([`render::spawn_cymatics_quad`], sampling texture A) → tag the texture
//!    handles onto a [`CymaticsRoot`] entity → insert the initial
//!    [`compute::CymaticsSimParams`]).
//! 2. Every `Update` while the sketch is `Active`, through the `Idle` pre-roll,
//!    **or** showing its screensaver, `update_cymatics_sim_params` packs
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
use wc_core::lifecycle::screensaver::in_screensaver;
use wc_core::lifecycle::screensaver::ScreensaverActive;
use wc_core::lifecycle::state::{AppState, SketchActivity};
use wc_core::lifecycle::RegisterIdleVetoExt;
use wc_core::settings::RegisterSketchSettingsExt;
use wc_core::sketch::{despawn_with, in_idle, sketch_active};

use compute::{create_cymatics_textures, CymaticsSimParams, SimParamsGpu, MAX_ITERATIONS};
use settings::CymaticsSettings;

/// Resting alive-mask radius (v4 `MINIMUM_ACTIVE_RADIUS`). At rest the wave
/// sources oscillate inside a small mask of this radius; interaction (C9) grows
/// it. Defined here for C8; the interaction systems consume it when they land.
pub const MINIMUM_ACTIVE_RADIUS: f32 = 0.1;

/// Resting wave-frequency control (v4 default `numCycles`). Interaction lowers
/// the effective cycle count via `slowDown`; see `update_cymatics_sim_params`.
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
/// defaults; `update_cymatics_sim_params` packs them into the GPU uniform and
/// advances [`CymaticsState::simulation_time`] each frame.
#[derive(Resource, Debug, Clone)]
pub struct CymaticsState {
    /// Primary wave centre, sim UV `[0, 1]`, top-left origin (Bevy-native).
    pub center: Vec2,
    /// Secondary wave centre, sim UV `[0, 1]`, top-left origin (Bevy-native).
    pub center2: Vec2,
    /// Alive-mask radius (v4 `activeRadius`), in window heights — the shader
    /// measures it in a height-normalized frame so the disc renders circular
    /// at any window aspect (vertically it covers the same span v4's raw-UV
    /// radius did).
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
    /// [`Self::simulation_time`] but **capped at `RAMP_TIME_CAP`** rather than
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
        // cycle. The listener is the shared generic in
        // `wc_core::sketch::lifecycle`, monomorphised on `CymaticsSettings`
        // (which supplies the storage key + `AppState::Cymatics` via its
        // `SketchLifecycle` impl).
        app.add_systems(
            Update,
            wc_core::sketch::restart_on_settings_change::<CymaticsSettings>,
        );

        // Plan 02: re-run the spawn path at the new window size when a resize
        // settles, via a silent/instant reload. This is also what re-inits the
        // sim grid, which `spawn_cymatics` derives from the window aspect.
        // Registered ALWAYS-ON (no run_if), mirroring `restart_on_settings_change`
        // above — a resize during idle/screensaver (a display re-enumerating
        // after sleep) must still respawn; the listener gates internally.
        // Defensive `add_message` so a test that builds this plugin without
        // wc-core's LifecyclePlugin still has the message (Bevy dedups;
        // LifecyclePlugin is canonical).
        app.add_message::<wc_core::lifecycle::window_resize::WindowResizeSettled>();
        app.add_systems(
            Update,
            wc_core::sketch::reload_on_resize_settled::<CymaticsSettings>,
        );

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
                wc_core::sketch::reset_render_profile,
            ),
        );
        app.add_systems(
            OnEnter(SketchActivity::Screensaver),
            systems::audio_coupling::enter_cymatics_screensaver_audio
                .run_if(in_state(AppState::Cymatics)),
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
        // skew_curve setting), brightness, and gamma into the render material
        // each frame. Runs through Idle too so changes track immediately across
        // the screensaver transition.
        app.add_systems(
            Update,
            update_cymatics_material.run_if(
                sketch_active(AppState::Cymatics)
                    .or_else(in_idle(AppState::Cymatics))
                    .or_else(in_screensaver(AppState::Cymatics)),
            ),
        );

        // Camera render profile: write tonemapping + bloom settings onto the
        // main camera each frame while Cymatics is active, through idle, or
        // during the screensaver so the profile is consistent across the full
        // lifecycle. Change-gated inside `set_camera_render_profile`.
        app.add_systems(
            Update,
            wc_core::sketch::apply_render_profile::<CymaticsSettings>.run_if(
                sketch_active(AppState::Cymatics)
                    .or_else(in_idle(AppState::Cymatics))
                    .or_else(in_screensaver(AppState::Cymatics)),
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
    // Delegates to the shared `register_sketch_tile` helper (async PNG load +
    // manifest append). v4 calls this sketch "Cymatics" in HomePage.tsx.
    // `STORAGE_KEY` binds this tile to `CymaticsSettings` so the settings dock's
    // Sketch tab resolves to Cymatics automatically (no per-sketch match arm).
    use wc_core::settings::SketchSettings as _;
    wc_core::sketch::register_sketch_tile(
        app,
        AppState::Cymatics,
        "Cymatics",
        settings::CymaticsSettings::STORAGE_KEY,
        "sketches/cymatics/screenshot.png",
    );
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
/// attract driver (`screensaver::drive_cymatics_attract`) writes
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

/// Alive-bloom ramp-clock seed for `warm_start_state`'s `ramp_time` field
/// (see [`RAMP_TIME_CAP`] and the shader's `(iter.time - 500.0) / 500.0` ramp,
/// `assets/shaders/cymatics/simulate.wgsl`). Below `500.0` that ramp term
/// is negative and the alive mask clamps to `0.0` everywhere, regardless of
/// `active_radius` or how far apart the two centres are — the third,
/// independent cause of the blank field.
///
/// Chosen by reviewing rendered frames — `cargo xtask capture
/// cymatics-synthetic`, whose PNGs the operating agent inspects directly — not
/// derived from the shader formula on paper: there are no GPU tests in CI, so
/// only a look at the captured field can judge whether the seeded state reads
/// as "already blooming" versus an ugly instantaneous snap. `900.0` is the
/// shipped value: `min(0.8, (900.0 - 500.0) / 500.0) == 0.8`, i.e. the ramp
/// term is already fully saturated, matching how the field looks after about 15
/// seconds of normal play at the default cadence (900 phase units). If a later
/// capture review changes this value, update it here and nowhere else — the
/// tests read this constant rather than duplicating it.
const WARM_START_RAMP_TIME: f32 = 900.0;

/// Phase clock (seconds) at which `warm_start_state` samples
/// `screensaver::wander_centers` to place the two wave sources.
///
/// **Not `0.0`, and the reason is compositional.** `wander_centers` traces
/// each centre on `0.5 + 0.3 * sin/cos(omega * t)`, and at `t == 0.0` the
/// cosine terms are at their maximum: both centres land at the top of their
/// Y range (`c1 = (0.50, 0.80)`, `c2 = (0.80, 0.75)`), stacked against one
/// edge and only `0.30` apart. Rendered, the two blobs are clipped by the
/// frame edge and two thirds of the field sits empty.
///
/// At this phase the same pure function yields `c1 ~= (0.72, 0.70)` and
/// `c2 ~= (0.30, 0.43)`: `0.50` apart (a 1.7x wider spread), both comfortably
/// inside the field, and spread diagonally so the interference band between
/// them crosses the middle of the screen. Verified by rendering it -- see
/// `WARM_START_RAMP_TIME`'s note on how these two constants were judged.
///
/// The exact value is not load-bearing: any phase that keeps both centres
/// away from the edges and well separated will do. It is a constant rather
/// than a literal so the two seeded centres and the tests that pin their
/// separation cannot drift apart.
const WARM_START_WANDER_PHASE: f32 = 224.0;

/// Compute the [`CymaticsState`] a fresh `OnEnter(AppState::Cymatics)` should
/// seed, in place of [`CymaticsState::default()`]'s all-zero, overlapping
/// resting state.
///
/// Fixes three independent causes of the field's blank "blue screen of death"
/// look, which the field tester reported happening every time he cycled
/// through the picker into Cymatics — not just once at app boot:
///
/// 1. **`center` == `center2`**: both default to `(0.5, 0.5)`, so a bloomed
///    mask shows one blob, not the two the tester asked for. Seeded from
///    [`screensaver::wander_centers`] at `elapsed = 0.0`, which is already pure
///    and unit-tested and returns two separated points ((0.5, 0.8) and
///    approximately (0.80, 0.75)).
/// 2. **`active_radius` at its resting floor**: [`MINIMUM_ACTIVE_RADIUS`] =
///    `0.1` is, per `CymaticsSettings::attract_radius`'s own doc, "a nearly
///    invisible mask." Seeded to `settings.attract_radius` — the same live
///    Dev knob the screensaver's attract driver already treats as its calm-
///    pond target — so this warm start needs no tunable constant of its own
///    for this field. (If an operator has dragged `attract_radius` all the
///    way down to its own `0.1` floor, the seeded radius equals
///    [`MINIMUM_ACTIVE_RADIUS`] exactly rather than exceeding it; that operator
///    has already opted into a near-invisible mask everywhere else the
///    setting applies, so a matching warm start is consistent, not a
///    regression.)
/// 3. **`ramp_time` below the shader's bloom-ramp foot**: see
///    `WARM_START_RAMP_TIME`.
///
/// Pure: identical `settings` in always produces an identical [`CymaticsState`]
/// out — no `Time`, no RNG, no mutable global state is read — which is what
/// makes repeated `OnEnter(AppState::Cymatics)` cycles (the field tester's
/// exact repro: cycling through the picker four times in under a minute)
/// seed identically every time rather than drifting.
///
/// `num_cycles`, `slow_down`, `simulation_time`, and `center_speed` are left
/// at their [`CymaticsState::default()`] resting values: none of the three
/// causes above implicates them, and `simulation_time` in particular must
/// stay owned solely by [`update_cymatics_sim_params`] (see the module's design
/// note on the single-owner invariant). This function only chooses the
/// *starting* value that system reads on its first frame — exactly the
/// relationship [`CymaticsState::default()`] already had to it.
fn warm_start_state(settings: &CymaticsSettings) -> CymaticsState {
    let speeds = screensaver::LissajousSpeeds::from_settings(settings);
    let (center, center2) = screensaver::wander_centers(WARM_START_WANDER_PHASE, &speeds);
    CymaticsState {
        center,
        center2,
        active_radius: settings.attract_radius,
        ramp_time: WARM_START_RAMP_TIME,
        ..CymaticsState::default()
    }
}

/// `OnEnter(AppState::Cymatics)` — insert a warm-started [`CymaticsState`]
/// (see `warm_start_state`) instead of [`CymaticsState::default()`]'s
/// all-zero, overlapping-centres resting state.
///
/// Every entry into Cymatics — not just the app's first boot — allocates a
/// fresh ping-pong texture pair and re-inserts this resource (`spawn_cymatics`
/// runs immediately after, chained in the same `OnEnter`), so the old
/// [`CymaticsState::default()`] seed reproduced the field tester's "blue screen
/// of death" on every navigation into the sketch, not once at boot.
fn init_cymatics_state(mut commands: Commands<'_, '_>, settings: Res<'_, CymaticsSettings>) {
    commands.insert_resource(warm_start_state(&settings));
}

/// Derive the sim grid size (texels) from the window size and the
/// `vertical_resolution` setting: `vertical_resolution` texels tall ×
/// `round(vertical_resolution · aspect)` texels wide, using the window aspect
/// (v4's derivation).
///
/// The load-bearing property is that each texel covers a **square** region of
/// the window (equal physical width and height, up to the ±0.5-texel
/// rounding): square texels are what make the GPU wave propagation isotropic
/// in physical pixels at any window aspect. The shader completes the picture
/// by measuring its source/alive-mask distances in a height-normalized frame
/// (see `assets/shaders/cymatics/simulate.wgsl`), so ripples and discs are
/// circles on screen in both landscape and portrait.
///
/// Pure (no world access) so the aspect math is unit-testable; called by
/// [`spawn_cymatics`] on every enter — including the silent reload that
/// re-runs it after a `WindowResizeSettled`.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "f32 setting → u32 texel grid: rounding then converting is intentional, and the \
              `.max(1)` keeps the value non-negative before `u32::try_from`"
)]
fn derive_sim_grid(win: Vec2, vertical_resolution: f32) -> UVec2 {
    let aspect = win.x / win.y;
    // f32 settings → u32 texel grid: round, floor at 1, fall back to 480 if the
    // (always non-negative) value somehow fails the conversion.
    let vy = u32::try_from((vertical_resolution.round() as i64).max(1)).unwrap_or(480);
    let vx = u32::try_from(((vertical_resolution * aspect).round() as i64).max(1)).unwrap_or(480);
    UVec2::new(vx, vy)
}

/// `OnEnter(AppState::Cymatics)` — allocate the two ping-pong textures, spawn
/// the fullscreen quad (sampling texture A), and insert the initial
/// [`CymaticsSimParams`].
///
/// The sim grid resolution comes from [`derive_sim_grid`] (v4's window-aspect
/// derivation; square texels in window space).
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "the iterations setting is f32; rounding then converting to u32 is intentional, and \
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
    let UVec2 { x: vx, y: vy } = derive_sim_grid(win, settings.vertical_resolution);
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
        // Raindrop ping state: active mode (0) at spawn; overwritten each frame
        // by `update_cymatics_sim_params` (mode 1 only while the screensaver shows).
        ping_mode: 0,
        ping_base: [0.0, 0.0],
        ping_amp: [0.0, 0.0],
        ping_duration: 0.0,
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
pub(crate) fn update_cymatics_sim_params(
    mut state: ResMut<'_, CymaticsState>,
    mut sim: ResMut<'_, CymaticsSimParams>,
    settings: Res<'_, CymaticsSettings>,
    // Raindrop hand-off: both present only while the screensaver shows.
    // `screensaver_active` marks the screensaver; `ping_state` carries the
    // per-centre Hann-window ticks the scheduler advanced this frame.
    screensaver_active: Option<Res<'_, ScreensaverActive>>,
    ping_state: Option<Res<'_, screensaver::CymaticsPingState>>,
) {
    sim.params.center = state.center.to_array();
    sim.params.center2 = state.center2.to_array();
    sim.params.active_radius = state.active_radius;

    // Source mode hand-off. While the screensaver shows, each centre is driven
    // by its own intermittent raindrop Hann pulse (mode 1): copy the scheduler's
    // current per-centre window ticks into `ping_base`, and the fixed
    // strength/duration into `ping_amp`/`ping_duration` (the compute prepare loop
    // evaluates `ping_envelope` per sub-step from these). In active play and the
    // idle pre-roll the mode is 0 — the byte-identical shared-oscillator path.
    match (screensaver_active, ping_state) {
        (Some(_), Some(ping)) => {
            sim.ping_mode = 1;
            sim.ping_base = ping.envelope_tick;
            sim.ping_amp = [settings.ping_strength, settings.ping_strength];
            sim.ping_duration = settings.ping_duration;
        }
        _ => {
            sim.ping_mode = 0;
        }
    }

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
/// `master_brightness`, and `gamma`. The screensaver brightness lift and
/// saturation compensation have been removed; `skew.w` (formerly screensaver
/// saturation) is now pinned to `1.0` and ignored by the shader.
///
/// Runs under `sketch_active OR in_idle OR in_screensaver` so the material
/// reflects the live state during active play, the idle pre-roll, and the
/// attract screensaver.
///
/// v4 `skewIntensity = pow(max(0, (numCycles - 1.002) / 2 - 0.5), 2)`.
/// The `skew_curve` Dev knob applies an exponent to this raw value before
/// packing into the uniform, allowing a wider or narrower push range.
///
/// ## Change-gated upload
///
/// `materials.get_mut` marks the material asset `Changed`, which forces the
/// render world to re-extract and re-upload its 32-byte uniform. Taking that
/// borrow unconditionally every frame would re-upload an identical uniform on
/// every frame of the multi-hour at-rest screensaver. So this reads the
/// current packed `skew` via `materials.get` first and only mutates when the
/// freshly-packed `Vec4` differs. Every input is pinned when the settings are
/// unchanged, so the packed `Vec4` is bit-stable frame to frame and the exact
/// compare holds (no epsilon needed). Any real knob change flips a bit and
/// triggers the upload.
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
    // Brightness is the plain master setting now — the screensaver brightness
    // lift was a workaround for the AgX-bypass bug (fixed in the blur node) and
    // for AgX's muting (gone now that the operator-chosen tonemap is applied
    // consistently). `skew.w` (formerly screensaver saturation) is pinned to the
    // identity 1.0 and ignored by the shader.
    let brightness = settings.master_brightness;
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
        //   .y = brightness       (plain master_brightness)
        //   .z = gamma            (per-channel display gamma; 1.0 = identity)
        //   .w = 1.0              (unused — formerly screensaver saturation; removed)
        let new_skew = Vec4::new(skew_intensity, brightness, settings.gamma, 1.0);

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

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected None is the correct behaviour"
)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use wc_core::sketch::SketchManifest;

    /// `derive_sim_grid` produces square texels in window space: the grid
    /// aspect matches the window aspect (up to the ±0.5-texel rounding), in
    /// both landscape and portrait. Square texels are what keep the GPU wave
    /// propagation isotropic in physical pixels.
    #[test]
    #[allow(
        clippy::as_conversions,
        clippy::cast_precision_loss,
        reason = "test arithmetic: u32 texel counts → f32 physical texel sizes"
    )]
    fn derive_sim_grid_texels_are_square_in_window_space() {
        for (win, expected) in [
            // 16:9 landscape at the default 480 rows: 853 columns.
            (Vec2::new(1920.0, 1080.0), UVec2::new(853, 480)),
            // 9:16 portrait: 270 columns.
            (Vec2::new(1080.0, 1920.0), UVec2::new(270, 480)),
            // Square window: square grid.
            (Vec2::new(1000.0, 1000.0), UVec2::new(480, 480)),
        ] {
            let grid = derive_sim_grid(win, 480.0);
            assert_eq!(grid, expected, "grid for window {win:?}");
            // Square-texel invariant: texel width == texel height within the
            // half-texel rounding of the column count.
            let texel_w = win.x / grid.x as f32;
            let texel_h = win.y / grid.y as f32;
            assert!(
                (texel_w - texel_h).abs() / texel_h < 0.01,
                "texels must be square in window space for {win:?} \
                 (w {texel_w}, h {texel_h})"
            );
        }
    }

    /// Degenerate inputs floor at a 1×1 grid instead of a zero-sized texture.
    #[test]
    fn derive_sim_grid_floors_at_one_texel() {
        let grid = derive_sim_grid(Vec2::new(1.0, 4000.0), 1.0);
        assert!(grid.x >= 1 && grid.y >= 1, "grid must be at least 1x1");
    }

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
            ping_mode: 0,
            ping_base: [0.0, 0.0],
            ping_amp: [0.0, 0.0],
            ping_duration: 0.0,
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
            ping_mode: 0,
            ping_base: [0.0, 0.0],
            ping_amp: [0.0, 0.0],
            ping_duration: 0.0,
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
            ping_mode: 0,
            ping_base: [0.0, 0.0],
            ping_amp: [0.0, 0.0],
            ping_duration: 0.0,
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

    /// `warm_start_state`: `center` and `center2` land at distinct points
    /// inside `[0,1]^2`, rather than the overlapping `(0.5, 0.5)` pair
    /// [`CymaticsState::default()`] produces (which makes a bloomed mask show
    /// only one blob, not two).
    #[test]
    fn warm_start_centers_are_distinct_and_in_unit_square() {
        let settings = CymaticsSettings::default();
        let state = warm_start_state(&settings);

        assert!(
            (0.0..=1.0).contains(&state.center.x) && (0.0..=1.0).contains(&state.center.y),
            "center must stay inside the sim UV field, got {:?}",
            state.center
        );
        assert!(
            (0.0..=1.0).contains(&state.center2.x) && (0.0..=1.0).contains(&state.center2.y),
            "center2 must stay inside the sim UV field, got {:?}",
            state.center2
        );
        assert!(
            state.center.distance(state.center2) > 0.1,
            "center and center2 must be visibly separated, not overlapping at (0.5, 0.5) each \
             like CymaticsState::default() (distance was {})",
            state.center.distance(state.center2)
        );
    }

    /// The seeded centres must sit **away from the frame edge**, not merely
    /// inside the unit square.
    ///
    /// This is the assertion with teeth, and it is why `warm_start_state`
    /// samples the wander at [`WARM_START_WANDER_PHASE`] rather than at
    /// `0.0`. At `0.0` the wander's cosine terms are at their maximum, so both
    /// centres pin to the top of their Y range — `(0.50, 0.80)` and
    /// `(0.80, 0.75)`. Both are legal points inside the unit square, so
    /// `warm_start_centers_are_distinct_and_in_unit_square` above passes
    /// happily; but rendered, the two blobs are clipped by the frame edge and
    /// most of the field is empty. Reverting the phase to `0.0` fails *this*
    /// test, which is the point of it.
    #[test]
    fn warm_start_centers_are_clear_of_the_frame_edge_and_widely_separated() {
        let settings = CymaticsSettings::default();
        let state = warm_start_state(&settings);

        // 0.25..=0.75 — comfortably off every edge, so a blob of the seeded
        // radius is fully on screen rather than half-cropped.
        for (label, c) in [("center", state.center), ("center2", state.center2)] {
            assert!(
                (0.25..=0.75).contains(&c.x) && (0.25..=0.75).contains(&c.y),
                "{label} must sit clear of the frame edge (0.25..=0.75 in both axes), got {c:?} \
                 -- a centre pinned to the edge renders as a clipped blob"
            );
        }

        // The t=0 seed separated the centres by only ~0.30. Rendering both
        // showed the wider spread reads as two distinct sources rather than
        // one lopsided smear, so hold the line above the old value.
        let separation = state.center.distance(state.center2);
        assert!(
            separation > 0.4,
            "the two wave sources must be widely separated so the interference band \
             crosses the middle of the field (separation was {separation})"
        );
    }

    /// Seeding `active_radius` from `attract_radius` (well above the resting
    /// floor) means [`cymatics_idle_veto`] returns `true` on the very first
    /// frame of every entry — the sketch declares itself "busy" immediately.
    ///
    /// That is only safe because two numbers happen to be in the right order,
    /// and **nothing else in the tree pins their relationship**:
    ///
    /// - the non-interacting branch of `systems::interaction::step_centers`
    ///   decays the radius geometrically toward `min_radius` at
    ///   `decay_factor` (0.005/frame), which from the seeded `attract_radius`
    ///   (1.0) drops below the veto threshold after ~898 frames, i.e. **~15 s**;
    /// - `ScreensaverSettings::attract_mode_timeout_secs` is **60 s**.
    ///
    /// 15 < 60, so by the time the idle timer fires the veto has long since
    /// cleared and the attract screensaver arrives on schedule. If someone
    /// later raises `attract_radius`, drops `decay_factor`, or shortens the
    /// attract timeout far enough to invert that, an untouched kiosk would
    /// **veto its own screensaver forever** — a visitor-facing regression that
    /// no other test would catch, because every other test here asserts on the
    /// seeded state alone and never steps it. This one steps it.
    #[test]
    fn the_warm_started_radius_clears_the_idle_veto_well_before_attract_is_due() {
        use crate::cymatics::systems::interaction::{step_centers, CenterInput, CenterTuning};

        let settings = CymaticsSettings::default();
        let mut state = warm_start_state(&settings);
        let tuning = CenterTuning::from_settings(&settings);

        // The veto's own threshold, mirroring the expression `cymatics_idle_veto` uses.
        let veto_threshold = MINIMUM_ACTIVE_RADIUS + 1e-2;
        assert!(
            state.active_radius > veto_threshold,
            "precondition: the warm start seeds a radius that DOES veto idle \
             (got {}, threshold {veto_threshold})",
            state.active_radius
        );

        // Nobody touches the installation: no mouse, no hands.
        let idle = CenterInput {
            mouse_pressed: false,
            mouse_uv: Vec2::splat(0.5),
            c1_held: false,
            c1_uv: Vec2::splat(0.5),
            c2_held: false,
            c2_uv: Vec2::splat(0.5),
        };

        let mut frames_until_clear = None;
        for frame in 1..=3_600_u32 {
            step_centers(&mut state, idle, tuning);
            if state.active_radius <= veto_threshold {
                frames_until_clear = Some(frame);
                break;
            }
        }

        assert!(
            frames_until_clear.is_some(),
            "an untouched Cymatics field never decayed below the idle veto in a full minute \
             of frames — the attract screensaver would never arrive (radius stuck at {})",
            state.active_radius
        );
        let frames = frames_until_clear.unwrap_or(u32::MAX);

        // Read the real attract timeout rather than hardcoding 60.
        let attract_due_secs =
            wc_core::lifecycle::screensaver::settings::ScreensaverSettings::default()
                .attract_mode_timeout_secs;
        let clear_secs = f64::from(frames) / 60.0;

        assert!(
            clear_secs < f64::from(attract_due_secs),
            "the seeded radius must clear the idle veto BEFORE attract is due, or an \
             untouched kiosk vetoes its own screensaver forever. Cleared at {clear_secs:.1}s \
             (frame {frames}); attract is due at {attract_due_secs}s."
        );
    }

    /// `warm_start_state`: the seeded `active_radius` clears the resting
    /// floor ([`MINIMUM_ACTIVE_RADIUS`] = 0.1) at default settings, and is
    /// sourced from the live `attract_radius` Dev knob rather than a new
    /// invented constant.
    #[test]
    fn warm_start_active_radius_clears_the_resting_floor() {
        let settings = CymaticsSettings::default();
        let state = warm_start_state(&settings);

        assert!(
            state.active_radius > MINIMUM_ACTIVE_RADIUS,
            "seeded active_radius ({}) must exceed the resting floor ({MINIMUM_ACTIVE_RADIUS}) \
             at default settings",
            state.active_radius
        );
        assert!(
            (state.active_radius - settings.attract_radius).abs() < f32::EPSILON,
            "active_radius must be seeded from the live attract_radius Dev knob, not a \
             hardcoded value"
        );
    }

    /// `warm_start_state`: `ramp_time` is seeded to `WARM_START_RAMP_TIME`
    /// (the constant chosen by capture review in Task 1), which is above
    /// [`CymaticsState::default()`]'s resting `0.0` and within the bounds
    /// [`update_cymatics_sim_params`] maintains for the rest of the sketch's
    /// life ([`RAMP_TIME_CAP`]). This test intentionally reads
    /// `WARM_START_RAMP_TIME` rather than hardcoding a second copy of the
    /// number, so it stays correct no matter what the visual review lands on.
    #[test]
    fn warm_start_ramp_time_clears_default_and_stays_within_the_clock_cap() {
        let settings = CymaticsSettings::default();
        let state = warm_start_state(&settings);

        assert!(
            state.ramp_time > CymaticsState::default().ramp_time,
            "warm-started ramp_time must exceed the CymaticsState::default() resting value (0.0)"
        );
        assert!(
            state.ramp_time <= RAMP_TIME_CAP,
            "warm-started ramp_time ({}) must not exceed RAMP_TIME_CAP ({RAMP_TIME_CAP})",
            state.ramp_time
        );
        assert!(
            (state.ramp_time - WARM_START_RAMP_TIME).abs() < f32::EPSILON,
            "warm_start_state must seed exactly WARM_START_RAMP_TIME, the constant the visual \
             review landed on"
        );
    }

    /// `warm_start_state` is pure: identical settings in always produce a
    /// bit-identical [`CymaticsState`] out. No `Time`, no RNG, no mutable
    /// global state is read.
    #[test]
    fn warm_start_state_is_pure() {
        let settings = CymaticsSettings::default();
        let a = warm_start_state(&settings);
        let b = warm_start_state(&settings);

        assert!(a.center.distance(b.center) < f32::EPSILON);
        assert!(a.center2.distance(b.center2) < f32::EPSILON);
        assert!((a.active_radius - b.active_radius).abs() < f32::EPSILON);
        assert!((a.ramp_time - b.ramp_time).abs() < f32::EPSILON);
    }

    /// Repeated `OnEnter(AppState::Cymatics)` cycles — the field tester's
    /// exact reproduction ("cycling thru" the picker four times in under a
    /// minute) — seed an identical [`CymaticsState`] every time, not a
    /// drifting or progressively-blanker one. Runs the real
    /// `init_cymatics_state` system (not just `warm_start_state` directly)
    /// through the same remove-then-reinsert cycle `OnExit`/`OnEnter`
    /// perform in the real app.
    #[test]
    fn repeated_on_enter_seeds_identical_state_each_time() {
        let mut world = World::new();
        world.insert_resource(CymaticsSettings::default());

        world
            .run_system_once(init_cymatics_state)
            .expect("init_cymatics_state run (first entry)");
        let first = world.resource::<CymaticsState>().clone();

        // Mirrors OnExit's remove_cymatics_sim_params dropping CymaticsState,
        // then a second OnEnter re-inserting it.
        let _ = world.remove_resource::<CymaticsState>();
        world
            .run_system_once(init_cymatics_state)
            .expect("init_cymatics_state run (second entry)");
        let second = world.resource::<CymaticsState>().clone();

        assert!(first.center.distance(second.center) < f32::EPSILON);
        assert!(first.center2.distance(second.center2) < f32::EPSILON);
        assert!((first.active_radius - second.active_radius).abs() < f32::EPSILON);
        assert!((first.ramp_time - second.ramp_time).abs() < f32::EPSILON);
    }
}
