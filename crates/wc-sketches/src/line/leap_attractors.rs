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
use wc_core::input::engagement;
use wc_core::input::entity::{
    CameraDistance, GrabStrength, PalmPosition, PalmVelocity, TrackedHand,
};
use wc_core::input::projection::palm_to_world;
use wc_core::settings::HandTrackingSettings;
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
///
/// **Fallback band only**: when the provider supplies a physical camera
/// distance ([`wc_core::input::hand::Hand::camera_distance_mm`] > 0 — the
/// `MediaPipe` provider with its depth estimator on), the drive uses the
/// kiosk distance band from `LineSettings` (`synth_full_volume_mm` /
/// `synth_silence_mm`) instead, which keeps fading out to several feet. This
/// Leap-z band covers providers with no physical estimate: Leap itself, the
/// mock fixtures, and the `MediaPipe` `k = 0` rollback pin.
pub const HAND_DRIVE_Z_NEAR_MM: f32 = 40.0;

/// Farthest calibrated hand depth in Leap-device millimetres. A palm at this
/// Z or farther gets distance attenuation 0.0 (silent). Fallback band only —
/// see [`HAND_DRIVE_Z_NEAR_MM`].
///
/// Numerically this matches the 350 mm far plane of the visual power model's
/// depth modulator in `update_line_hand_attractors`, but the boundary
/// semantics differ: the visual modulator `5^((−z + 350) / 160)` evaluates to
/// 1× (neutral, not zero) at z = 350, while the audio drive reaches 0 there.
/// So at the very edge of the band a gripping hand still moves particles but
/// makes no sound — deliberate: silence, not a residual hum, is the cue that
/// the hand is about to leave the tracked volume. (On the kiosk band the
/// relation flips: the visual power fades to 1× by ~1 m while the sound
/// carries out to `synth_silence_mm`.)
pub const HAND_DRIVE_Z_FAR_MM: f32 = 350.0;

/// Guard floor (mm) on the kiosk band's width, `synth_silence_mm −
/// synth_full_volume_mm`: a hand-edited config with the rails inverted or
/// equal must not divide by zero or flip the fade's direction.
const HAND_DRIVE_MIN_BAND_MM: f32 = 1.0;

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
/// — see [`crate::hand_mesh`], which fixes the
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

/// Stickiness margin for the focal-hand pick: a rival hand must exceed the
/// current focal hand's engagement score by this factor (×1.4) before it can
/// start earning focus. Prevents per-frame flapping between two similarly
/// scored hands — focus is a latch, not an argmax.
pub const FOCAL_SWITCH_MARGIN: f32 = 1.4;

/// How long (seconds) a rival must *continuously* hold the margin before
/// focus actually switches — a sustained beat, so a single-frame velocity
/// spike (landmark jitter) can never steal the focal point mid-gesture.
pub const FOCAL_SWITCH_SUSTAIN_S: f32 = 0.5;

/// Max simultaneously scored hands. Providers cap at 2 tracked hands; 4
/// leaves headroom for a provider handover frame. A hand beyond the cap is
/// simply not scored this frame (it can never become focal until a slot
/// frees) — fixed-size storage keeps the per-frame system allocation-free.
const FOCAL_SCORE_CAP: usize = 4;

/// Per-hand render-rate engagement state for the focal pick: EMAs of palm
/// speed and grab articulation (τ = [`engagement::ENGAGEMENT_TAU_S`], same
/// constants as the provider-side score so the two lanes agree).
#[derive(Debug, Clone, Copy)]
struct FocalHandScore {
    /// The tracked-hand entity this entry follows.
    entity: Entity,
    /// EMA of |palm velocity| (mm/s) — the motion term.
    motion_mm_s: f32,
    /// EMA of |Δgrab|/dt (1/s) — the articulation term. Grab only (no pinch
    /// here): Line's interaction is grab-driven, and grab is the component
    /// the task of picking "the hand that is actually playing" cares about.
    articulation_per_s: f32,
    /// Previous frame's grab, for the articulation finite difference;
    /// `None` until the entry's second frame.
    prev_grab: Option<f32>,
    /// The engagement score computed this frame ([`engagement::engagement`]).
    score: f32,
    /// Seen-this-frame flag for pruning despawned hands.
    seen: bool,
}

impl FocalHandScore {
    /// A cold entry for a newly seen hand: no motion/articulation history —
    /// like the provider-side tracker, a hand must *demonstrate* activity.
    fn new(entity: Entity) -> Self {
        Self {
            entity,
            motion_mm_s: 0.0,
            articulation_per_s: 0.0,
            prev_grab: None,
            score: 0.0,
            seen: true,
        }
    }

    /// Advance the EMAs one frame and recompute the score.
    ///
    /// `speed_mm_s` is this frame's |`PalmVelocity`|; `grab` this frame's
    /// grab strength; `distance_mm` the physical camera distance (`0` =
    /// unknown → neutral proximity); `motion_weight` the live
    /// `HandTrackingSettings::engagement_motion_weight`.
    fn step(
        &mut self,
        distance_mm: f32,
        speed_mm_s: f32,
        grab: f32,
        dt_s: f32,
        motion_weight: f32,
    ) {
        // Framerate-independent EMA step (alpha = 1 − e^(−dt/τ)).
        let alpha = engagement::ema_alpha(dt_s, engagement::ENGAGEMENT_TAU_S);
        if speed_mm_s.is_finite() {
            self.motion_mm_s += alpha * (speed_mm_s - self.motion_mm_s);
        }
        // Articulation rate |Δgrab|/dt: the drink-holder discriminator — a
        // static grip has a high grab LEVEL but zero grab CHANGE.
        if let Some(prev) = self.prev_grab {
            if dt_s > 0.0 {
                let rate = (grab - prev).abs() / dt_s;
                if rate.is_finite() {
                    self.articulation_per_s += alpha * (rate - self.articulation_per_s);
                }
            }
        }
        self.prev_grab = Some(grab);
        self.score = engagement::engagement(
            engagement::proximity(distance_mm),
            engagement::activity(self.motion_mm_s, self.articulation_per_s),
            motion_weight,
        );
        self.seen = true;
    }
}

/// `Local` state for `pick_line_focal_hand` (plain code span: the system is
/// private, so an intra-doc link would break without
/// `--document-private-items`): fixed-capacity per-hand score
/// entries (no allocation in the per-frame system) plus the rival latch that
/// implements the sustained-beat switch.
#[derive(Debug, Default)]
pub struct FocalPickState {
    /// Score entries, one per live tracked hand (`None` = free slot).
    entries: [Option<FocalHandScore>; FOCAL_SCORE_CAP],
    /// The rival currently accumulating switch time, if any.
    rival: Option<Entity>,
    /// How long the rival has continuously held the switch margin.
    rival_hold_s: f32,
}

/// Query row for the focal pick: each tracked Line hand with the three
/// engagement inputs already maintained on its entity.
type FocalHandQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static CameraDistance,
        &'static PalmVelocity,
        &'static GrabStrength,
    ),
    (With<TrackedHand>, With<LineHandAttractor>),
>;

impl FocalPickState {
    /// The entry for `entity`, creating one in a free slot if needed
    /// (`None` when the fixed capacity is full — that hand goes unscored).
    fn entry_mut(&mut self, entity: Entity) -> Option<&mut FocalHandScore> {
        // Two passes (find, then insert) keep the borrow checker happy
        // without unsafe or allocation; N is at most 4.
        let found = self
            .entries
            .iter()
            .position(|e| e.is_some_and(|e| e.entity == entity));
        let index = if let Some(i) = found {
            i
        } else {
            let free = self.entries.iter().position(Option::is_none)?;
            self.entries[free] = Some(FocalHandScore::new(entity));
            free
        };
        self.entries[index].as_mut()
    }

    /// This frame's best-scored seen hand.
    fn best(&self) -> Option<(Entity, f32)> {
        self.entries
            .iter()
            .flatten()
            .filter(|e| e.seen)
            .max_by(|a, b| a.score.total_cmp(&b.score))
            .map(|e| (e.entity, e.score))
    }

    /// The current score of `entity`, if it was seen this frame.
    fn score_of(&self, entity: Entity) -> Option<f32> {
        self.entries
            .iter()
            .flatten()
            .find(|e| e.seen && e.entity == entity)
            .map(|e| e.score)
    }
}

/// Pick the hand entity that drives the gravity focal point this frame.
///
/// v4 (and v5 until the kiosk deployment) used "the first hand the
/// controller reported" — the lowest entity index, i.e. the *oldest-acquired*
/// hand. Beside a busy road that is exactly wrong: a bystander's static
/// drink-holding hand that grabbed a slot first held the focal point forever
/// while the actual player waved unheeded.
///
/// Now the pick is engagement-scored ([`engagement::engagement`]) from
/// components already on the entity — [`CameraDistance`] (closer wins),
/// [`PalmVelocity`] (moving wins), [`GrabStrength`] *change* (articulating
/// wins; a static grip scores zero) — with stickiness: the current focal hand
/// keeps focus unless a rival beats its score by [`FOCAL_SWITCH_MARGIN`] for
/// a sustained [`FOCAL_SWITCH_SUSTAIN_S`] ([`choose_focal`]), so focus never
/// flaps frame to frame. Per-hand EMA state lives in the `Local`
/// ([`FocalPickState`], fixed capacity — no per-frame allocation).
fn pick_line_focal_hand(
    hands: FocalHandQuery<'_, '_>,
    time: Res<'_, Time>,
    tracking_settings: Option<Res<'_, HandTrackingSettings>>,
    mut state: Local<'_, FocalPickState>,
    mut focal: ResMut<'_, LineFocalHand>,
) {
    let dt_s = time.delta_secs();
    // Same live knob as the provider-side eviction score, so one on-site
    // adjustment shapes both; default when the settings resource is absent
    // (tests, headless harnesses).
    let motion_weight = tracking_settings
        .as_ref()
        .map_or(engagement::DEFAULT_MOTION_WEIGHT, |settings| {
            settings.engagement_motion_weight
        });

    // Mark-and-sweep the fixed entry set: score every live hand, then drop
    // entries whose entity vanished (hand left / sketch exit).
    for entry in state.entries.iter_mut().flatten() {
        entry.seen = false;
    }
    for (entity, distance, velocity, grab) in &hands {
        if let Some(entry) = state.entry_mut(entity) {
            entry.step(distance.0, velocity.0.length(), grab.0, dt_s, motion_weight);
        }
    }
    for slot in &mut state.entries {
        if slot.is_some_and(|e| !e.seen) {
            *slot = None;
        }
    }

    let best = state.best();
    // The incumbent only counts while it is still a live, scored hand.
    let current = focal
        .0
        .and_then(|entity| state.score_of(entity).map(|score| (entity, score)));
    let FocalPickState {
        rival,
        rival_hold_s,
        ..
    } = &mut *state;
    let next = choose_focal(current, best, rival, rival_hold_s, dt_s);
    // set_if_neq semantics by hand: only write on change (avoid ticking the
    // resource every frame while focus is steady).
    if focal.0 != next {
        focal.0 = next;
    }
}

/// The focal-hand latch: keep `current` unless `best` (the top-scored rival)
/// has beaten it by [`FOCAL_SWITCH_MARGIN`] continuously for
/// [`FOCAL_SWITCH_SUSTAIN_S`]. Pure and generic over the id type so the
/// decision is unit-testable without an ECS world.
///
/// `rival` / `rival_hold_s` are the caller-owned latch state: which candidate
/// is currently accumulating switch time and for how long. Any break in the
/// rival's dominance (score dips under the margin, a different hand becomes
/// best, the incumbent vanishes) resets the hold — the beat must be
/// *sustained*, not cumulative.
fn choose_focal<T: Copy + PartialEq>(
    current: Option<(T, f32)>,
    best: Option<(T, f32)>,
    rival: &mut Option<T>,
    rival_hold_s: &mut f32,
    dt_s: f32,
) -> Option<T> {
    let clear = |rival: &mut Option<T>, hold: &mut f32| {
        *rival = None;
        *hold = 0.0;
    };
    let Some((best_id, best_score)) = best else {
        // No scored hands at all: no focal point.
        clear(rival, rival_hold_s);
        return None;
    };
    let Some((current_id, current_score)) = current else {
        // No (live) incumbent: adopt the best immediately — first hand in
        // gets focus with no waiting period.
        clear(rival, rival_hold_s);
        return Some(best_id);
    };
    if best_id == current_id {
        // The incumbent is still the best hand: nothing to latch.
        clear(rival, rival_hold_s);
        return Some(current_id);
    }
    if best_score > current_score * FOCAL_SWITCH_MARGIN {
        // The rival holds the margin: accumulate its beat (restarting if the
        // rival identity changed since last frame).
        if *rival == Some(best_id) {
            *rival_hold_s += dt_s;
        } else {
            *rival = Some(best_id);
            *rival_hold_s = dt_s;
        }
        if *rival_hold_s >= FOCAL_SWITCH_SUSTAIN_S {
            clear(rival, rival_hold_s);
            return Some(best_id);
        }
    } else {
        // Margin lost: the beat must be continuous, so the latch resets.
        clear(rival, rival_hold_s);
    }
    Some(current_id)
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
    hands: Query<'_, '_, (&PalmPosition, &GrabStrength, &CameraDistance), With<TrackedHand>>,
    mouse: Res<'_, MouseAttractorState>,
    settings: Res<'_, LineSettings>,
    time: Res<'_, Time>,
    mut drive: ResMut<'_, HandAudioDrive>,
) {
    let hand_max = hands
        .iter()
        .map(|(palm, grab, distance)| {
            hand_audio_drive(
                grab.0,
                distance.0,
                palm.0.z,
                settings.synth_grab_gamma,
                settings.synth_distance_falloff,
                settings.synth_full_volume_mm,
                settings.synth_silence_mm,
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
/// `drive = clamp(grab, 0, 1)^grab_gamma × proximity^distance_falloff`.
///
/// Proximity comes from one of two bands:
///
/// - **Kiosk band** (when `camera_distance_mm > 0`, i.e. the provider
///   estimated a physical camera distance): `proximity = clamp((silence_mm −
///   d) / (silence_mm − full_volume_mm), 0, 1)` — full drive at or inside
///   `full_volume_mm` (default 500 mm, a kiosk visitor's standing distance),
///   silent at `silence_mm` (default 2400 mm ≈ 8 ft). Unlike the Leap z this
///   distance is unclamped past 1 m, so the fade genuinely spans feet.
/// - **Leap-z fallback** (`camera_distance_mm == 0`, the "unknown" sentinel):
///   the original `clamp((Z_FAR − z) / (Z_FAR − Z_NEAR), 0, 1)` band on the
///   palm's Leap-convention z — preserving the pre-kiosk feel for Leap
///   hardware, the mock fixtures, and `MediaPipe`'s `k = 0` depth-pin
///   rollback (whose pinned z = 120 mm lands at the documented ≈ 0.74 cap;
///   the `synth_volume_scale` master fader compensates live).
#[allow(
    clippy::too_many_arguments,
    reason = "pure tuning function: grab + two distance inputs + four knobs; bundling them into a \
              params struct at the single call site would only move the argument list"
)]
pub fn hand_audio_drive(
    grab: f32,
    camera_distance_mm: f32,
    z_mm: f32,
    grab_gamma: f32,
    distance_falloff: f32,
    full_volume_mm: f32,
    silence_mm: f32,
) -> f32 {
    // grab^gamma — how fist closure maps to loudness. The clamp guards
    // against over-range provider values (powf on >1 would exceed full
    // drive; on <0 it would be NaN). gamma = 1 is linear; gamma > 1 demands
    // a more deliberate fist before the synth opens up.
    let grab_term = grab.clamp(0.0, 1.0).powf(grab_gamma);
    let proximity = if camera_distance_mm > 0.0 {
        // Kiosk band on the physical distance. The band width is floored
        // (HAND_DRIVE_MIN_BAND_MM) so inverted/equal rails — reachable from
        // the overlapping dev-panel sliders, not just a hand-edited config —
        // degrade to a hard near/silent step instead of a division by zero
        // or a backwards fade.
        let band = (silence_mm - full_volume_mm).max(HAND_DRIVE_MIN_BAND_MM);
        ((silence_mm - camera_distance_mm) / band).clamp(0.0, 1.0)
    } else {
        // Leap-z fallback band: (Z_FAR − z) / (Z_FAR − Z_NEAR), clamped so
        // hands outside the calibrated band saturate rather than
        // over/under-shooting.
        ((HAND_DRIVE_Z_FAR_MM - z_mm) / (HAND_DRIVE_Z_FAR_MM - HAND_DRIVE_Z_NEAR_MM))
            .clamp(0.0, 1.0)
    };
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

    /// Default kiosk band rails (mirror `LineSettings` defaults).
    const FULL_MM: f32 = 500.0;
    const SILENCE_MM: f32 = 2400.0;

    /// [`hand_audio_drive`] through the **Leap-z fallback band**: camera
    /// distance unknown (0), kiosk rails passed but unused on this path.
    fn legacy_drive(grab: f32, z_mm: f32, gamma: f32, falloff: f32) -> f32 {
        hand_audio_drive(grab, 0.0, z_mm, gamma, falloff, FULL_MM, SILENCE_MM)
    }

    #[test]
    fn zero_grab_is_silent_even_at_nearest_depth() {
        assert_eq!(legacy_drive(0.0, HAND_DRIVE_Z_NEAR_MM, GAMMA, FALLOFF), 0.0);
    }

    #[test]
    fn full_grab_at_nearest_depth_is_full_drive() {
        let d = legacy_drive(1.0, HAND_DRIVE_Z_NEAR_MM, GAMMA, FALLOFF);
        assert!((d - 1.0).abs() < 1e-6, "expected 1.0, got {d}");
    }

    #[test]
    fn full_grab_at_farthest_depth_is_silent() {
        let d = legacy_drive(1.0, HAND_DRIVE_Z_FAR_MM, GAMMA, FALLOFF);
        assert!(d.abs() < 1e-6, "expected 0.0, got {d}");
    }

    #[test]
    fn out_of_band_inputs_clamp() {
        // Nearer than the near plane clamps to full attenuation, not > 1.
        assert!(legacy_drive(1.0, 0.0, GAMMA, FALLOFF) <= 1.0);
        // Farther than the far plane clamps to silence, not negative.
        assert_eq!(legacy_drive(1.0, 1000.0, GAMMA, FALLOFF), 0.0);
        // Over-range grab clamps to 1 before the exponent.
        assert!(legacy_drive(2.0, HAND_DRIVE_Z_NEAR_MM, GAMMA, FALLOFF) <= 1.0);
    }

    #[test]
    fn drive_monotonic_in_grab() {
        let z = 150.0; // arbitrary mid-band depth
        let mut prev = legacy_drive(0.0, z, GAMMA, FALLOFF);
        for i in 1..=10 {
            #[allow(
                clippy::cast_precision_loss,
                clippy::as_conversions,
                reason = "test loop counter"
            )]
            let grab = i as f32 / 10.0;
            let d = legacy_drive(grab, z, GAMMA, FALLOFF);
            assert!(
                d > prev,
                "drive must rise with grab: grab={grab} d={d} prev={prev}"
            );
            prev = d;
        }
    }

    #[test]
    fn drive_monotonic_decreasing_in_depth() {
        let mut prev = legacy_drive(1.0, HAND_DRIVE_Z_NEAR_MM, GAMMA, FALLOFF);
        for i in 1..=10 {
            #[allow(
                clippy::cast_precision_loss,
                clippy::as_conversions,
                reason = "test loop counter"
            )]
            let z = HAND_DRIVE_Z_NEAR_MM
                + (HAND_DRIVE_Z_FAR_MM - HAND_DRIVE_Z_NEAR_MM) * (i as f32 / 10.0);
            let d = legacy_drive(1.0, z, GAMMA, FALLOFF);
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
        let linear = legacy_drive(0.5, HAND_DRIVE_Z_NEAR_MM, 1.0, FALLOFF);
        let steep = legacy_drive(0.5, HAND_DRIVE_Z_NEAR_MM, 2.0, FALLOFF);
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
        let linear = legacy_drive(1.0, z_mid, GAMMA, 1.0);
        let steep = legacy_drive(1.0, z_mid, GAMMA, 2.0);
        assert!(
            steep < linear,
            "falloff 2 ({steep}) must be below falloff 1 ({linear})"
        );
        assert!((linear - 0.5).abs() < 1e-6);
        assert!((steep - 0.25).abs() < 1e-6);
    }

    // --- kiosk distance band (physical camera mm) --------------------------

    /// [`hand_audio_drive`] through the **kiosk band**: physical distance
    /// known, default rails, z passed as an obviously-wrong junk value to
    /// prove the kiosk path ignores it.
    fn kiosk_drive(grab: f32, distance_mm: f32) -> f32 {
        hand_audio_drive(
            grab,
            distance_mm,
            9999.0,
            GAMMA,
            FALLOFF,
            FULL_MM,
            SILENCE_MM,
        )
    }

    #[test]
    fn kiosk_band_is_full_volume_at_standing_distance() {
        // A kiosk visitor at the 500 mm rail (or nearer) gets full drive —
        // standing distance is not a "lean in" penalty.
        assert!((kiosk_drive(1.0, FULL_MM) - 1.0).abs() < 1e-6);
        assert!((kiosk_drive(1.0, 250.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn kiosk_band_fades_to_silence_at_the_far_rail() {
        // Silent at 2400 mm (~8 ft) and beyond; the old Leap band would have
        // been silent past 350 — this is the whole point of the kiosk band.
        assert_eq!(kiosk_drive(1.0, SILENCE_MM), 0.0);
        assert_eq!(kiosk_drive(1.0, 3000.0), 0.0);
        // Midpoint of the band → proximity 0.5 with linear falloff.
        let mid = f32::midpoint(FULL_MM, SILENCE_MM);
        assert!((kiosk_drive(1.0, mid) - 0.5).abs() < 1e-6);
        // Monotonic: farther is never louder.
        let mut prev = kiosk_drive(1.0, FULL_MM);
        for step in 1..=10 {
            #[allow(
                clippy::cast_precision_loss,
                clippy::as_conversions,
                reason = "test loop counter"
            )]
            let d = FULL_MM + (SILENCE_MM - FULL_MM) * (step as f32 / 10.0);
            let drive = kiosk_drive(1.0, d);
            assert!(drive < prev, "must fall with distance: {d} mm → {drive}");
            prev = drive;
        }
    }

    #[test]
    fn kiosk_band_ignores_leap_z() {
        // With a physical distance present, the Leap-z argument must have no
        // effect (it is clamped/meaningless past 1 m anyway).
        let a = hand_audio_drive(1.0, 800.0, 40.0, GAMMA, FALLOFF, FULL_MM, SILENCE_MM);
        let b = hand_audio_drive(1.0, 800.0, 350.0, GAMMA, FALLOFF, FULL_MM, SILENCE_MM);
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
    }

    #[test]
    fn unknown_distance_falls_back_to_the_leap_z_band() {
        // distance 0.0 = "unknown" sentinel (Leap, mock fixtures, k = 0 depth
        // pin): the original Leap-z band must apply unchanged — including the
        // documented ≈ 0.74 cap at the pinned z = 120 mm.
        let pinned = hand_audio_drive(1.0, 0.0, 120.0, GAMMA, FALLOFF, FULL_MM, SILENCE_MM);
        assert!((pinned - (350.0 - 120.0) / 310.0).abs() < 1e-6, "{pinned}");
    }

    #[test]
    fn degenerate_band_rails_do_not_nan_or_invert() {
        // Inverted rails in a hand-edited config: the band floor turns the
        // fade into a hard step, never a NaN or a backwards (louder-when-
        // farther) ramp.
        let near = hand_audio_drive(1.0, 500.0, 0.0, GAMMA, FALLOFF, 2000.0, 600.0);
        let far = hand_audio_drive(1.0, 2500.0, 0.0, GAMMA, FALLOFF, 2000.0, 600.0);
        assert!(near.is_finite() && (0.0..=1.0).contains(&near), "{near}");
        assert!(far.is_finite() && (0.0..=1.0).contains(&far), "{far}");
        assert!(near >= far, "never louder when farther: {near} vs {far}");
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

    // --- engagement-based focal-hand pick ----------------------------------

    /// Drive [`choose_focal`] one step with fresh latch state helpers.
    struct FocalLatch {
        rival: Option<u32>,
        hold: f32,
    }

    impl FocalLatch {
        fn new() -> Self {
            Self {
                rival: None,
                hold: 0.0,
            }
        }

        fn step(
            &mut self,
            current: Option<(u32, f32)>,
            best: Option<(u32, f32)>,
            dt: f32,
        ) -> Option<u32> {
            choose_focal(current, best, &mut self.rival, &mut self.hold, dt)
        }
    }

    #[test]
    fn focal_is_none_without_hands_and_adopts_the_first_hand_instantly() {
        let mut latch = FocalLatch::new();
        assert_eq!(latch.step(None, None, 0.016), None);
        // First hand in: focus with no waiting period.
        assert_eq!(latch.step(None, Some((7, 0.1)), 0.016), Some(7));
    }

    #[test]
    fn focal_sticks_when_the_rival_is_not_decisively_better() {
        // Rival at 1.3× the incumbent — under the 1.4× margin — must never
        // steal focus no matter how long it persists (no flapping between
        // two similar hands).
        let mut latch = FocalLatch::new();
        for _ in 0..600 {
            assert_eq!(
                latch.step(Some((1, 0.5)), Some((2, 0.65)), 0.016),
                Some(1),
                "sub-margin rival must never take focus"
            );
        }
    }

    #[test]
    fn decisive_rival_takes_focus_only_after_a_sustained_beat() {
        // Rival at 2× the incumbent: switches, but only after
        // FOCAL_SWITCH_SUSTAIN_S of continuous dominance — never on the
        // first frame (a velocity spike must not steal focus).
        let mut latch = FocalLatch::new();
        let dt = 0.016;
        let mut elapsed = 0.0;
        let mut switched_at = None;
        for _ in 0..120 {
            let now = latch.step(Some((1, 0.4)), Some((2, 0.8)), dt);
            elapsed += dt;
            if now == Some(2) {
                switched_at = Some(elapsed);
                break;
            }
        }
        assert!(
            switched_at.is_some(),
            "a decisively better rival must eventually win"
        );
        let at = switched_at.unwrap_or(0.0);
        assert!(
            at >= FOCAL_SWITCH_SUSTAIN_S,
            "switched after {at}s — before the sustained beat"
        );
        assert!(at < FOCAL_SWITCH_SUSTAIN_S + 0.1, "switched late: {at}s");
    }

    #[test]
    fn interrupted_dominance_resets_the_switch_beat() {
        // The rival holds the margin for a while, dips under it for one
        // frame, then holds again: the beat restarts, so no switch happens
        // in less than a full sustained window after the dip.
        let mut latch = FocalLatch::new();
        let dt = 0.016;
        // 20 frames (~0.32 s) of dominance — not enough to switch.
        for _ in 0..20 {
            assert_eq!(latch.step(Some((1, 0.4)), Some((2, 0.8)), dt), Some(1));
        }
        // One frame under the margin resets the latch.
        assert_eq!(latch.step(Some((1, 0.4)), Some((2, 0.5)), dt), Some(1));
        // 20 more frames of dominance: still under the sustain window
        // because the hold restarted.
        for _ in 0..20 {
            assert_eq!(
                latch.step(Some((1, 0.4)), Some((2, 0.8)), dt),
                Some(1),
                "beat must restart after the dip"
            );
        }
    }

    #[test]
    fn vanished_incumbent_hands_focus_to_the_best_hand_immediately() {
        let mut latch = FocalLatch::new();
        // Incumbent gone (current = None) but hands remain: no dead focal.
        assert_eq!(latch.step(None, Some((3, 0.2)), 0.016), Some(3));
    }

    #[test]
    fn static_grip_score_stays_low_while_a_mover_overtakes_it() {
        // Component-level scenario: the drink-holder (near, strong CONSTANT
        // grab, zero velocity) vs the player (farther, waving). After ~1 s of
        // frames the player's score must exceed the holder's by more than the
        // switch margin.
        let w = engagement::DEFAULT_MOTION_WEIGHT;
        let dt = 1.0 / 60.0;
        let mut holder = FocalHandScore::new(Entity::PLACEHOLDER);
        let mut player = FocalHandScore::new(Entity::PLACEHOLDER);
        for _ in 0..60 {
            // Holder: 600 mm away, motionless, grab pinned at 0.9 — high
            // grab LEVEL, zero grab CHANGE.
            holder.step(600.0, 0.0, 0.9, dt, w);
            // Player: 1200 mm away, waving at 400 mm/s, light grab.
            player.step(1200.0, 400.0, 0.2, dt, w);
        }
        assert!(
            player.score > holder.score * FOCAL_SWITCH_MARGIN,
            "player {} must decisively beat static holder {}",
            player.score,
            holder.score
        );
    }

    #[test]
    fn articulating_hand_scores_activity_without_moving() {
        // A hand opening/closing in place (playing Line without waving):
        // grab toggling drives the articulation term even at zero velocity.
        let w = engagement::DEFAULT_MOTION_WEIGHT;
        let dt = 1.0 / 60.0;
        let mut still_player = FocalHandScore::new(Entity::PLACEHOLDER);
        let mut still_holder = FocalHandScore::new(Entity::PLACEHOLDER);
        for n in 0..60 {
            let grab = if n % 2 == 0 { 0.1 } else { 0.6 };
            still_player.step(800.0, 0.0, grab, dt, w);
            still_holder.step(800.0, 0.0, 0.9, dt, w);
        }
        assert!(
            still_player.score > still_holder.score * FOCAL_SWITCH_MARGIN,
            "articulation alone must win focus: {} vs {}",
            still_player.score,
            still_holder.score
        );
    }

    /// System-level: the exact deployment failure. The drink-holder hand is
    /// spawned FIRST (lower entity index — the old pick's winner) and sits
    /// static with a strong grip; the player spawns second, farther away but
    /// waving. Focus must migrate to the player within ~2 s and stay there.
    #[test]
    fn system_focal_hand_migrates_from_static_holder_to_moving_player() {
        let mut app = App::new();
        app.init_resource::<LineFocalHand>();
        app.init_resource::<Time>();
        app.add_systems(Update, pick_line_focal_hand);

        let holder = app
            .world_mut()
            .spawn((
                TrackedHand,
                LineHandAttractor::default(),
                CameraDistance(600.0),
                PalmVelocity(Vec3::ZERO),
                GrabStrength(0.9),
            ))
            .id();
        let player = app
            .world_mut()
            .spawn((
                TrackedHand,
                LineHandAttractor::default(),
                CameraDistance(1200.0),
                PalmVelocity(Vec3::new(400.0, 0.0, 0.0)),
                GrabStrength(0.2),
            ))
            .id();

        // Frame 1: both cold — the (nearer) holder wins the initial pick,
        // reproducing the pre-fix state of the world.
        tick(&mut app, 1.0 / 60.0);
        assert_eq!(
            app.world().resource::<LineFocalHand>().0,
            Some(holder),
            "cold start: proximity alone favours the holder"
        );

        // ~2 s of frames: the player's motion EMA fills in, beats the margin
        // for the sustained window, and takes focus.
        for _ in 0..120 {
            tick(&mut app, 1.0 / 60.0);
        }
        assert_eq!(
            app.world().resource::<LineFocalHand>().0,
            Some(player),
            "the waving player must take focus from the static grip"
        );

        // And keeps it (no flapping back).
        for _ in 0..60 {
            tick(&mut app, 1.0 / 60.0);
            assert_eq!(app.world().resource::<LineFocalHand>().0, Some(player));
        }

        // Player leaves: focus falls back to the remaining hand immediately.
        app.world_mut().entity_mut(player).despawn();
        tick(&mut app, 1.0 / 60.0);
        assert_eq!(
            app.world().resource::<LineFocalHand>().0,
            Some(holder),
            "focus must not point at a despawned entity"
        );
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

    /// System-level: a known camera distance routes the drive through the
    /// kiosk band (settings rails), not the palm's Leap z. The palm sits at
    /// the Leap-z FAR rail (target 0 on the fallback band) while its physical
    /// distance sits inside the kiosk full-volume rail — after ~3 s of frames
    /// the drive must still be pinned at full, which only the kiosk path
    /// produces (the fallback path would have decayed toward 0).
    #[test]
    fn system_prefers_kiosk_band_over_leap_z_when_distance_known() {
        let mut app = App::new();
        app.init_resource::<HandAudioDrive>();
        app.init_resource::<MouseAttractorState>();
        app.init_resource::<LineSettings>();
        app.init_resource::<Time>();
        app.add_systems(Update, update_hand_audio_drive);

        app.world_mut().spawn((
            TrackedHand,
            PalmPosition(Vec3::new(0.0, 0.0, HAND_DRIVE_Z_FAR_MM)),
            GrabStrength(1.0),
            CameraDistance(400.0), // inside the 500 mm full-volume rail
        ));
        for _ in 0..180 {
            tick(&mut app, 1.0 / 60.0);
        }
        let d = app.world().resource::<HandAudioDrive>().0;
        assert!(
            (d - 1.0).abs() < 1e-6,
            "kiosk band must hold full drive at 400 mm regardless of Leap z, got {d}"
        );
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
                // Unknown distance → Leap-z fallback band (this test pins the
                // pre-kiosk behaviour end to end).
                CameraDistance(0.0),
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
