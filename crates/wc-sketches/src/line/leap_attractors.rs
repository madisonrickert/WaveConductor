//! Per-hand attractor for the Line sketch.
//!
//! Ports v4's `computeLeapAttractorPower` continuous-power model
//! (`.worktrees/v4/src/particles/leapAttractorPower.ts`) onto v5's
//! `TrackedHand` entity model: each tracked hand gets its own
//! [`LineHandAttractor`] component while Line is the active sketch,
//! holding the current power + projected world position. Line's particle
//! stepping collects attractors from this query alongside the singleton
//! `MouseAttractorState`.
//!
//! Also owns [`HandAudioDrive`]: a continuous `[0, 1]` loudness drive derived
//! from grab strength × hand-depth attenuation (max over tracked hands),
//! pinned to full while a mouse press is held. The audio coupling
//! ([`super::audio_coupling::drive_audio_and_shader`]) multiplies it into the
//! synth `volume` param so a partial grab or a far hand sounds proportionally
//! quieter — previously the synth volume tracked only the particle-field
//! envelope, which saturates and made hand audio feel binary on/off. After
//! *any* release (grab opened, hand lost, mouse button up) the tail decays
//! through both this drive and the `grouped_upness` envelope — see
//! [`drive_target`] for why that stacking is deliberate.

use bevy::prelude::*;
use wc_core::input::entity::{GrabStrength, PalmPosition, TrackedHand};
use wc_core::input::projection::palm_to_world;
use wc_core::sketch::sketch_active;

use wc_core::lifecycle::state::AppState;

use super::settings::LineSettings;
use super::systems::MouseAttractorState;

/// v4 attack-speed for Line's grab-to-power smoothing.
/// (`.worktrees/v4/src/sketches/line/index.ts` `LEAP_POWER_CONFIG`.)
pub const LINE_HAND_ATTACK_SPEED: f32 = 0.005;

/// v4 decay-speed: when grab is below threshold, `power *= 0.5` per frame.
pub const LINE_HAND_DECAY_SPEED: f32 = 0.5;

/// v4 grab threshold: Line responds to any non-zero grab.
pub const LINE_HAND_GRAB_THRESHOLD: f32 = 0.0;

/// Nearest calibrated hand depth in Leap-device millimetres (`PalmPosition` Z).
/// A palm at this Z or closer gets distance attenuation 1.0 (loudest).
pub const HAND_DRIVE_Z_NEAR_MM: f32 = 40.0;

/// Farthest calibrated hand depth in Leap-device millimetres. A palm at this
/// Z or farther gets distance attenuation 0.0 (silent).
///
/// Numerically this matches the 350 mm far plane of the visual power model's
/// depth modulator in `update_line_hand_attractors`, but the boundary
/// semantics differ: the visual modulator `5^((−z + 350) / 160)` evaluates to
/// 1× (neutral, not zero) at z = 350, while the audio drive reaches 0 there.
/// So at the very edge of the band a gripping hand still moves particles but
/// makes no sound — deliberate: silence, not a residual hum, is the cue that
/// the hand is about to leave the tracked volume.
pub const HAND_DRIVE_Z_FAR_MM: f32 = 350.0;

/// Floor (seconds) on the [`HandAudioDrive`] release time constant.
///
/// The live τ is derived per frame by [`hand_drive_release_tau_s`] from
/// `LineSettings::synth_release_ms` — the same setting the production
/// envelope path turns into its release rate via
/// [`super::particle_stats::EnvelopeRates::from_settings`] — so the drive's
/// decay can never undercut the envelope's tail at any slider position
/// (100–3000 ms). This floor keeps short envelope settings (default 350 ms)
/// on the hand-tuned 670 ms drive feel instead of letting the drive collapse
/// with them. Rising targets are applied instantly — closing the fist must
/// be audible at once, mirroring the envelope's fast attack.
pub const HAND_DRIVE_RELEASE_TAU_FLOOR_S: f32 = 0.67;

/// Live release time constant τ (seconds) for the drive:
/// `max(synth_release_ms / 1000, HAND_DRIVE_RELEASE_TAU_FLOOR_S)`.
///
/// Term by term:
/// - `synth_release_ms / 1000` — the envelope's release time constant in
///   seconds (`EnvelopeRates::from_settings` computes the release *rate* as
///   `1000 / synth_release_ms`, so its time constant is the reciprocal).
///   `τ_drive ≥ τ_envelope` guarantees the envelope, never the drive, is the
///   bottleneck shaping the release tail — the drive snapping ahead of a
///   long envelope tail would audibly clip it.
/// - `max(…, floor)` — see [`HAND_DRIVE_RELEASE_TAU_FLOOR_S`]: envelope
///   settings shorter than 670 ms keep the hand-tuned drive release feel.
pub fn hand_drive_release_tau_s(synth_release_ms: f32) -> f32 {
    (synth_release_ms / 1000.0).max(HAND_DRIVE_RELEASE_TAU_FLOOR_S)
}

/// Per-hand attractor state. Lives on each [`TrackedHand`] entity while
/// `AppState::Line` is active.
#[derive(Component, Debug, Default, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct LineHandAttractor {
    /// Current attractor power.
    pub power: f32,
    /// World-space position derived from `palm_to_world`.
    pub position: Vec2,
}

/// Marker resource pointing at the entity whose [`LineHandAttractor`]
/// should drive the gravity focal point this frame. Set by
/// `pick_line_focal_hand`; read by particle / post-process code.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct LineFocalHand(pub Option<Entity>);

/// Continuous loudness drive for the Line synth voice, in `[0, 1]`.
///
/// Written each frame by `update_hand_audio_drive` while Line is active;
/// multiplied into the synth `volume` param by
/// [`super::audio_coupling::drive_audio_and_shader`]. Hand interaction maps
/// grab strength × distance attenuation into the drive (partial grabs and
/// far hands are proportionally quieter); a held mouse press is pinned to
/// full drive. After release the tail decays through *both* this drive's
/// release lag and the `grouped_upness` envelope, so the post-click tail is
/// somewhat faster than the envelope alone produced before the drive existed
/// — deliberate, and pinned by the `post_release_tail_decays_through_drive_too`
/// test.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct HandAudioDrive(pub f32);

impl Default for HandAudioDrive {
    /// `1.0` = no attenuation: at startup and in mouse-only sessions the
    /// synth behaves exactly as it did before the drive existed.
    fn default() -> Self {
        Self(1.0)
    }
}

/// Plugin wiring: attaches the [`LineHandAttractor`] component when Line
/// is active and a new [`TrackedHand`] spawns, removes it on exit, runs
/// the per-frame power + position update system, and maintains the
/// [`HandAudioDrive`] loudness resource consumed by the audio coupling.
pub struct LineLeapAttractorsPlugin;

impl Plugin for LineLeapAttractorsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LineFocalHand>()
            .init_resource::<HandAudioDrive>()
            .register_type::<LineHandAttractor>()
            .add_systems(
                Update,
                (
                    ensure_line_attractors,
                    update_line_hand_attractors,
                    pick_line_focal_hand,
                    update_hand_audio_drive,
                )
                    .chain()
                    .run_if(sketch_active(AppState::Line)),
            )
            .add_systems(OnExit(AppState::Line), detach_all_line_attractors);
    }
}

/// Reconcile pass (runs while Line is the active sketch): attach
/// [`LineHandAttractor`] to every [`TrackedHand`] that doesn't already have it.
///
/// Replaces an earlier `Add<TrackedHand>` observer gated on `AppState::Line`.
/// That observer missed hands that were already being tracked when Line began —
/// hand-tracking runs in `PreUpdate`, *before* the `StateTransition` into Line,
/// so those hands were added while the state was still `Home` and never got an
/// attractor (no gravity pull from a hand held up as you entered the sketch).
/// A `Without<LineHandAttractor>` reconcile is timing-independent and idempotent
/// — see [`crate::line::hand_mesh::ensure_bone_meshes`], which fixes the
/// identical issue for the bone visuals.
fn ensure_line_attractors(
    mut commands: Commands<'_, '_>,
    new_hands: Query<'_, '_, Entity, (With<TrackedHand>, Without<LineHandAttractor>)>,
) {
    for hand in &new_hands {
        commands.entity(hand).insert(LineHandAttractor::default());
    }
}

/// Cleanup: remove `LineHandAttractor` from all entities on Line exit.
fn detach_all_line_attractors(
    mut commands: Commands<'_, '_>,
    query: Query<'_, '_, Entity, (With<TrackedHand>, With<LineHandAttractor>)>,
) {
    for entity in &query {
        commands.entity(entity).remove::<LineHandAttractor>();
    }
}

/// Per-frame: compute the v4 continuous power model and projected world
/// position for each hand's [`LineHandAttractor`].
fn update_line_hand_attractors(
    mut hands: Query<
        '_,
        '_,
        (&PalmPosition, &GrabStrength, &mut LineHandAttractor),
        With<TrackedHand>,
    >,
    window: Single<'_, '_, &Window>,
) {
    let window_size = Vec2::new(window.width(), window.height());

    for (palm, grab, mut attractor) in &mut hands {
        attractor.position = palm_to_world(palm.0, window_size);

        if grab.0 > LINE_HAND_GRAB_THRESHOLD {
            // v4: wanted = grab^1.5 * 5^((-z + 350) / 160)
            let grab_component = grab.0.powf(1.5);
            let depth_modulator = 5.0_f32.powf((-palm.0.z + 350.0) / 160.0);
            let wanted = grab_component * depth_modulator;
            // EMA toward wanted at the attack rate.
            attractor.power =
                attractor.power * (1.0 - LINE_HAND_ATTACK_SPEED) + wanted * LINE_HAND_ATTACK_SPEED;
        } else {
            // v4: power *= decay (geometric decay, no floor for Line).
            attractor.power *= LINE_HAND_DECAY_SPEED;
        }
    }
}

/// Pick the hand entity that drives the gravity focal point this frame.
/// v4's choice was "the first hand the controller reported" — in our
/// entity model that's the lowest-index `Entity`, since Bevy assigns
/// entity ids monotonically.
fn pick_line_focal_hand(
    hands: Query<'_, '_, Entity, (With<TrackedHand>, With<LineHandAttractor>)>,
    mut focal: ResMut<'_, LineFocalHand>,
) {
    focal.0 = hands.iter().min_by_key(|e| e.index());
}

/// Per-frame: derive [`HandAudioDrive`] from tracked hands + mouse activity.
///
/// The loudest hand wins (max over hands) — a second, farther hand must not
/// duck a near fist. While the mouse attractor is active the target is pinned
/// to 1.0 (full loudness during the held press). Rising targets apply
/// instantly; falling targets decay with a τ coupled to the envelope release
/// setting ([`hand_drive_release_tau_s`]) so a just-released grab's volume
/// tail isn't clipped (see [`step_hand_audio_drive`]).
///
/// Runs at the end of the leap-attractor chain. The audio coupling
/// ([`super::audio_coupling::drive_audio_and_shader`]) reads the resource
/// from a separate `Update` chain with no explicit cross-chain ordering, so
/// it may see the previous frame's drive — one frame of staleness is
/// inaudible through the synth's own `follow(0.016)` volume smoothing
/// (same tolerance the particle-stats system already accepts when reading
/// [`LineHandAttractor`]).
fn update_hand_audio_drive(
    hands: Query<'_, '_, (&PalmPosition, &GrabStrength), With<TrackedHand>>,
    mouse: Res<'_, MouseAttractorState>,
    settings: Res<'_, LineSettings>,
    time: Res<'_, Time>,
    mut drive: ResMut<'_, HandAudioDrive>,
) {
    let hand_max = hands
        .iter()
        .map(|(palm, grab)| {
            hand_audio_drive(
                grab.0,
                palm.0.z,
                settings.synth_grab_gamma,
                settings.synth_distance_falloff,
            )
        })
        .fold(0.0_f32, f32::max);
    let target = drive_target(hand_max, mouse.power > 0.0);
    // τ tracks the live envelope-release setting so the drive can never clip
    // the tail, whatever the slider says (see `hand_drive_release_tau_s`).
    let tau_s = hand_drive_release_tau_s(settings.synth_release_ms);
    let next = step_hand_audio_drive(drive.0, target, time.delta_secs(), tau_s);
    // set_if_neq: skip the resource write (and its change tick) across the
    // long steady-state stretches where the drive is parked at 0.0 or 1.0.
    drive.set_if_neq(HandAudioDrive(next));
}

/// Instantaneous loudness drive for one hand, in `[0, 1]`.
///
/// `drive = clamp(grab, 0, 1)^grab_gamma × proximity^distance_falloff` where
/// `proximity = clamp((Z_FAR − z) / (Z_FAR − Z_NEAR), 0, 1)`.
pub fn hand_audio_drive(grab: f32, z_mm: f32, grab_gamma: f32, distance_falloff: f32) -> f32 {
    // grab^gamma — how fist closure maps to loudness. The clamp guards
    // against over-range provider values (powf on >1 would exceed full
    // drive; on <0 it would be NaN). gamma = 1 is linear; gamma > 1 demands
    // a more deliberate fist before the synth opens up.
    let grab_term = grab.clamp(0.0, 1.0).powf(grab_gamma);
    // Normalised proximity: (Z_FAR − z) / (Z_FAR − Z_NEAR).
    //   z = Z_NEAR (40 mm, hand nearest the sensor) → 1.0 (loudest);
    //   z = Z_FAR (350 mm, hand at the tracking edge) → 0.0 (silent).
    // Clamped so hands outside the calibrated band saturate rather than
    // over/under-shooting.
    //
    // Providers that PIN depth instead of estimating it land at a fixed
    // proximity: MediaPipe's depth-estimator rollback (dev-panel
    // "Depth calibration k" = 0) pins z = 120 mm — `MEDIAPIPE_DEPTH_PROXY_MM`
    // in wc-core's mediapipe coords module — giving
    // (350 − 120) / 310 ≈ 0.74, capping hand audio below full drive however
    // close the hand really is. Accepted for the rollback path; the
    // `synth_volume_scale` master fader compensates live.
    let proximity = ((HAND_DRIVE_Z_FAR_MM - z_mm) / (HAND_DRIVE_Z_FAR_MM - HAND_DRIVE_Z_NEAR_MM))
        .clamp(0.0, 1.0);
    // proximity^falloff — distance-attenuation curve. falloff = 1 fades
    // linearly across the band; falloff > 1 drops loudness faster as the
    // hand retreats.
    grab_term * proximity.powf(distance_falloff)
}

/// Combine the per-hand maximum with mouse activity into this frame's drive
/// target: `max(hand_max, mouse_active ? 1.0 : 0.0)`.
///
/// Mouse interaction has no grab/depth axes, so a *held* press pins the
/// target to full drive — the synth is as loud under a click as under a near
/// full fist. On release (mouse or grab) the target drops and
/// [`step_hand_audio_drive`]'s release lag (not a snap) takes the drive down
/// while `grouped_upness` releases. The audible post-release tail is
/// therefore the product of *both* decays — somewhat faster than the
/// envelope alone, which is the pre-drive behaviour. Deliberate: the tail
/// stays shaped (never clipped, since `τ_drive ≥ τ_envelope`), and a single
/// stacked release reads as one gesture ending rather than two.
pub fn drive_target(hand_max: f32, mouse_active: bool) -> f32 {
    hand_max.max(if mouse_active { 1.0 } else { 0.0 })
}

/// Advance the drive one frame of `dt` seconds toward `target`: instant on
/// rise, exponential decay with the supplied time constant `tau_s` on fall
/// (production passes [`hand_drive_release_tau_s`] of the live
/// `synth_release_ms` setting).
///
/// The asymmetry mirrors the synth envelope: attack must be heard the same
/// frame the fist closes, while a falling drive (released grab, hand leaving
/// the tracking volume) must not snap to zero under the still-releasing
/// `grouped_upness` tail — that would clip the release audibly.
pub fn step_hand_audio_drive(current: f32, target: f32, dt: f32, tau_s: f32) -> f32 {
    if target >= current {
        // Rising edge: instantaneous attack.
        target
    } else {
        // Falling edge: first-order lag toward the target.
        //   next = current + (target − current) · min(dt/τ, 1)
        // dt/τ is the per-frame lerp fraction of the exponential decay
        // (Euler step of dx/dt = (target − x)/τ); the min(…, 1) guards a
        // long-frame hitch from overshooting past the target.
        current + (target - current) * (dt / tau_s).min(1.0)
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic float arithmetic is the test subject"
)]
mod tests {
    use super::*;

    /// Defaults: linear grab curve, linear distance falloff.
    const GAMMA: f32 = 1.0;
    const FALLOFF: f32 = 1.0;

    #[test]
    fn zero_grab_is_silent_even_at_nearest_depth() {
        assert_eq!(
            hand_audio_drive(0.0, HAND_DRIVE_Z_NEAR_MM, GAMMA, FALLOFF),
            0.0
        );
    }

    #[test]
    fn full_grab_at_nearest_depth_is_full_drive() {
        let d = hand_audio_drive(1.0, HAND_DRIVE_Z_NEAR_MM, GAMMA, FALLOFF);
        assert!((d - 1.0).abs() < 1e-6, "expected 1.0, got {d}");
    }

    #[test]
    fn full_grab_at_farthest_depth_is_silent() {
        let d = hand_audio_drive(1.0, HAND_DRIVE_Z_FAR_MM, GAMMA, FALLOFF);
        assert!(d.abs() < 1e-6, "expected 0.0, got {d}");
    }

    #[test]
    fn out_of_band_inputs_clamp() {
        // Nearer than the near plane clamps to full attenuation, not > 1.
        assert!(hand_audio_drive(1.0, 0.0, GAMMA, FALLOFF) <= 1.0);
        // Farther than the far plane clamps to silence, not negative.
        assert_eq!(hand_audio_drive(1.0, 1000.0, GAMMA, FALLOFF), 0.0);
        // Over-range grab clamps to 1 before the exponent.
        assert!(hand_audio_drive(2.0, HAND_DRIVE_Z_NEAR_MM, GAMMA, FALLOFF) <= 1.0);
    }

    #[test]
    fn drive_monotonic_in_grab() {
        let z = 150.0; // arbitrary mid-band depth
        let mut prev = hand_audio_drive(0.0, z, GAMMA, FALLOFF);
        for i in 1..=10 {
            #[allow(
                clippy::cast_precision_loss,
                clippy::as_conversions,
                reason = "test loop counter"
            )]
            let grab = i as f32 / 10.0;
            let d = hand_audio_drive(grab, z, GAMMA, FALLOFF);
            assert!(
                d > prev,
                "drive must rise with grab: grab={grab} d={d} prev={prev}"
            );
            prev = d;
        }
    }

    #[test]
    fn drive_monotonic_decreasing_in_depth() {
        let mut prev = hand_audio_drive(1.0, HAND_DRIVE_Z_NEAR_MM, GAMMA, FALLOFF);
        for i in 1..=10 {
            #[allow(
                clippy::cast_precision_loss,
                clippy::as_conversions,
                reason = "test loop counter"
            )]
            let z = HAND_DRIVE_Z_NEAR_MM
                + (HAND_DRIVE_Z_FAR_MM - HAND_DRIVE_Z_NEAR_MM) * (i as f32 / 10.0);
            let d = hand_audio_drive(1.0, z, GAMMA, FALLOFF);
            assert!(
                d < prev,
                "drive must fall with depth: z={z} d={d} prev={prev}"
            );
            prev = d;
        }
    }

    #[test]
    fn grab_gamma_exponent_bites() {
        // At half grab, a steeper curve (gamma 2) must be quieter than linear:
        // 0.5^2 = 0.25 < 0.5^1 = 0.5.
        let linear = hand_audio_drive(0.5, HAND_DRIVE_Z_NEAR_MM, 1.0, FALLOFF);
        let steep = hand_audio_drive(0.5, HAND_DRIVE_Z_NEAR_MM, 2.0, FALLOFF);
        assert!(
            steep < linear,
            "gamma 2 ({steep}) must be below gamma 1 ({linear})"
        );
        assert!((linear - 0.5).abs() < 1e-6);
        assert!((steep - 0.25).abs() < 1e-6);
    }

    #[test]
    fn distance_falloff_exponent_bites() {
        // Mid-band depth → proximity 0.5; falloff 2 squares it to 0.25.
        let z_mid = f32::midpoint(HAND_DRIVE_Z_NEAR_MM, HAND_DRIVE_Z_FAR_MM);
        let linear = hand_audio_drive(1.0, z_mid, GAMMA, 1.0);
        let steep = hand_audio_drive(1.0, z_mid, GAMMA, 2.0);
        assert!(
            steep < linear,
            "falloff 2 ({steep}) must be below falloff 1 ({linear})"
        );
        assert!((linear - 0.5).abs() < 1e-6);
        assert!((steep - 0.25).abs() < 1e-6);
    }

    #[test]
    fn mouse_active_pins_target_to_full() {
        // No hands: mouse alone keeps current loudness.
        assert_eq!(drive_target(0.0, true), 1.0);
        // Partial hand + mouse: mouse wins (max).
        assert_eq!(drive_target(0.3, true), 1.0);
        // No mouse: hand value passes through.
        assert_eq!(drive_target(0.3, false), 0.3);
        // Neither active: target is zero (the release smoothing below keeps
        // the tail from snapping).
        assert_eq!(drive_target(0.0, false), 0.0);
    }

    #[test]
    fn rising_target_applies_instantly() {
        // Attack must be immediate — a closing fist is audible the same frame.
        let tau = HAND_DRIVE_RELEASE_TAU_FLOOR_S;
        assert_eq!(step_hand_audio_drive(0.2, 0.9, 0.016, tau), 0.9);
        assert_eq!(step_hand_audio_drive(0.0, 1.0, 0.016, tau), 1.0);
    }

    #[test]
    fn falling_target_decays_without_snapping() {
        // One 60 fps frame after a full release: the drive must still be close
        // to 1.0 (exact value 1 − dt/τ), never clipped to the target.
        let dt = 0.016;
        let tau = HAND_DRIVE_RELEASE_TAU_FLOOR_S;
        let stepped = step_hand_audio_drive(1.0, 0.0, dt, tau);
        let expected = 1.0 - dt / tau;
        assert!(
            (stepped - expected).abs() < 1e-6,
            "expected {expected}, got {stepped}"
        );
        assert!(stepped > 0.9, "release must not clip the tail: {stepped}");
    }

    #[test]
    fn release_converges_to_target() {
        // Integrate ~3 s of 60 fps frames at the floor τ: drive must land
        // near zero (tail fully released) without going negative.
        let mut drive = 1.0;
        for _ in 0..180 {
            drive = step_hand_audio_drive(drive, 0.0, 1.0 / 60.0, HAND_DRIVE_RELEASE_TAU_FLOOR_S);
        }
        assert!(drive >= 0.0);
        assert!(
            drive < 0.02,
            "after ~4.5τ the drive should be near zero: {drive}"
        );
    }

    #[test]
    fn drive_tau_never_undercuts_envelope_release() {
        // Production envelope rates come from `EnvelopeRates::from_settings`
        // (τ_env = synth_release_ms / 1000), and the `synth_release_ms`
        // slider spans 100–3000 ms. The drive τ must be ≥ τ_env at every
        // slider position so the envelope — never the drive — is the
        // bottleneck shaping the release tail, and ≥ the floor so short
        // envelope settings keep the hand-tuned drive feel.
        let mut ms = 100.0_f32;
        while ms <= 3000.0 {
            let tau = hand_drive_release_tau_s(ms);
            let tau_env = ms / 1000.0;
            assert!(
                tau >= tau_env,
                "τ_drive ({tau}) must be ≥ τ_env ({tau_env}) at {ms} ms"
            );
            assert!(
                tau >= HAND_DRIVE_RELEASE_TAU_FLOOR_S,
                "τ_drive ({tau}) must respect the floor at {ms} ms"
            );
            ms += 50.0;
        }
    }

    #[test]
    fn post_release_tail_decays_through_drive_too() {
        // Documents a deliberate behaviour change: after a mouse release the
        // drive does NOT hold at 1.0 — it decays toward 0 alongside the
        // grouped_upness release, so the audible post-click tail is the
        // product of both decays (somewhat faster than the envelope alone,
        // the pre-drive behaviour). The lag (never a snap) still guarantees
        // the tail is shaped, not clipped.
        let tau = hand_drive_release_tau_s(350.0); // default release slider
        let dt = 1.0 / 60.0;
        // Held press: pinned at full drive.
        let held = step_hand_audio_drive(1.0, drive_target(0.0, true), dt, tau);
        assert_eq!(held, 1.0, "held press stays pinned at full drive");
        // Released: the drive starts decaying the same frame…
        let released = step_hand_audio_drive(1.0, drive_target(0.0, false), dt, tau);
        assert!(released < 1.0, "release must start the drive decay");
        // …but as a first-order lag, not a snap.
        assert!(released > 0.95, "decay is a lag, not a snap: {released}");
    }

    /// Advance the manually-driven `Time` resource by `dt` seconds and run one
    /// frame. The test app deliberately omits `TimePlugin` so the release
    /// decay integrates a deterministic fixed timestep instead of wall clock.
    fn tick(app: &mut App, dt: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(dt));
        app.update();
    }

    /// System-level: a tracked hand with a partial grab writes a partial
    /// drive; adding mouse power pins it back to 1.0.
    #[test]
    fn system_writes_drive_from_tracked_hand() {
        let mut app = App::new();
        app.init_resource::<HandAudioDrive>();
        app.init_resource::<MouseAttractorState>();
        app.init_resource::<LineSettings>();
        app.init_resource::<Time>();
        app.add_systems(Update, update_hand_audio_drive);

        // Full grab at the nearest depth → drive 1.0 (rising edge, instant).
        let hand = app
            .world_mut()
            .spawn((
                TrackedHand,
                PalmPosition(Vec3::new(0.0, 0.0, HAND_DRIVE_Z_NEAR_MM)),
                GrabStrength(1.0),
            ))
            .id();
        tick(&mut app, 1.0 / 60.0);
        let d = app.world().resource::<HandAudioDrive>().0;
        assert!(
            (d - 1.0).abs() < 1e-6,
            "full near grab should drive 1.0, got {d}"
        );

        // Half grab at the same depth: target 0.5 is below the current 1.0,
        // so the release path applies with τ = max(0.35, 0.67) = 0.67 s
        // (default `synth_release_ms` = 350 sits under the floor) — integrate
        // ~3 s of 60 fps frames (≈ 4.5τ) and require convergence near 0.5.
        app.world_mut().entity_mut(hand).insert(GrabStrength(0.5));
        for _ in 0..180 {
            tick(&mut app, 1.0 / 60.0);
        }
        let d = app.world().resource::<HandAudioDrive>().0;
        assert!(
            (d - 0.5).abs() < 0.05,
            "half grab should converge near 0.5, got {d}"
        );

        // Mouse activity pins the drive back to full instantly (rising edge).
        app.world_mut().resource_mut::<MouseAttractorState>().power = 10.0;
        tick(&mut app, 1.0 / 60.0);
        let d = app.world().resource::<HandAudioDrive>().0;
        assert!(
            (d - 1.0).abs() < 1e-6,
            "mouse-active should pin drive to 1.0, got {d}"
        );
    }
}
