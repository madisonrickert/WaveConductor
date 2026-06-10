//! Derived per-hand signals from the 21 landmarks.
//!
//! The landmark model gives positions, presence, handedness, and world
//! landmarks; the [`crate::input::hand::Hand`] fields the rest of the app
//! consumes (`pinch_strength`, `grab_strength`, `palm_normal`, `palm_velocity`,
//! stable `id`) are derived here with documented, deterministic geometry.
//!
//! [`hand_scale`], [`pinch_strength`], [`grab_strength`], and [`palm_center`]
//! are **space-agnostic** distance geometry — they hold in whatever coordinate
//! space the caller's 21 landmarks are expressed in. The `MediaPipe` pipeline
//! feeds the gesture magnitudes the model's **world** landmarks (metric metres,
//! wrist/hand-centred, orthographic), making them invariant to perspective
//! foreshortening: a hand tilted toward the camera no longer reads as partially
//! grabbed, which it did when these ran on projected image landmarks.
//! (`palm_center` is additionally used on square-norm *image* landmarks for the
//! positional palm path — same math, different space.) The pinch/grab
//! magnitudes are normalized by [`hand_scale`] so they are also
//! distance-invariant; their exact thresholds are tuned against real hands
//! during hardware validation.
//!
//! [`palm_normal`] is the exception: it is **orientation-sensitive**, so its
//! caller must supply landmarks already expressed in the Leap orientation
//! convention (the pipeline maps world axes into it first — see
//! `world_to_leap_orientation` in [`super::pipeline`]).
//!
//! Foundation module: consumed by the pipeline (plan Phase 8); exercised by
//! tests until then.
#![allow(dead_code)]

use std::time::Duration;

use bevy::math::Vec3;

use crate::input::hand::{Chirality, LandmarkIndex, LANDMARK_COUNT};

/// Reference hand scale: wrist → middle-finger MCP distance. Used to normalize
/// pinch/grab so they don't change with the hand's distance from the camera.
/// Space-agnostic pure distance; the pipeline passes metric world landmarks,
/// where this is ~0.09 m for an adult hand.
#[must_use]
pub fn hand_scale(lm: &[Vec3; LANDMARK_COUNT]) -> f32 {
    let wrist = lm[LandmarkIndex::Wrist.as_index()];
    let middle_mcp = lm[LandmarkIndex::MiddleMcp.as_index()];
    wrist.distance(middle_mcp).max(f32::EPSILON)
}

/// Pinch strength in `[0, 1]`: thumb-tip ↔ index-tip proximity, normalized by
/// hand scale. `1.0` when the tips touch, falling to `0.0` once they are about
/// half a hand-scale apart. Space-agnostic ratio of distances; the pipeline
/// passes world landmarks so the value is pose-invariant.
#[must_use]
pub fn pinch_strength(lm: &[Vec3; LANDMARK_COUNT]) -> f32 {
    let thumb = lm[LandmarkIndex::ThumbTip.as_index()];
    let index = lm[LandmarkIndex::IndexTip.as_index()];
    let dist = thumb.distance(index) / hand_scale(lm);
    // dist 0 → 1.0; dist >= 0.5 → 0.0.
    (1.0 - dist / 0.5).clamp(0.0, 1.0)
}

/// Grab strength in `[0, 1]`: mean fingertip closure toward the palm centre,
/// normalized by hand scale. `0.0` for an open hand (tips extended ~one
/// hand-scale out), approaching `1.0` as the four fingers curl into a fist.
/// Space-agnostic ratio of distances; the pipeline passes world landmarks —
/// on perspective-projected image landmarks a hand tilted toward the camera
/// foreshortens its tip-to-palm distances and falsely reads as grabbed.
#[must_use]
pub fn grab_strength(lm: &[Vec3; LANDMARK_COUNT]) -> f32 {
    let palm = palm_center(lm);
    let scale = hand_scale(lm);
    let tips = [
        LandmarkIndex::IndexTip,
        LandmarkIndex::MiddleTip,
        LandmarkIndex::RingTip,
        LandmarkIndex::PinkyTip,
    ];
    let count = f32::from(u8::try_from(tips.len()).unwrap_or(1));
    let mean: f32 = tips
        .iter()
        .map(|t| lm[t.as_index()].distance(palm) / scale)
        .sum::<f32>()
        / count;
    // Open hand: mean ≈ 1.0 (tips a hand-scale out) → 0.0.
    // Fist: mean ≈ 0.3 (tips near palm) → ~1.0.
    ((1.0 - mean) / 0.7).clamp(0.0, 1.0)
}

/// Palm centre: centroid of the wrist and the index/pinky MCP knuckles.
/// Space-agnostic; the pipeline uses it in two spaces — on world landmarks
/// (inside [`grab_strength`]) and on square-norm image landmarks for the
/// positional palm path.
#[must_use]
pub fn palm_center(lm: &[Vec3; LANDMARK_COUNT]) -> Vec3 {
    let wrist = lm[LandmarkIndex::Wrist.as_index()];
    let index_mcp = lm[LandmarkIndex::IndexMcp.as_index()];
    let pinky_mcp = lm[LandmarkIndex::PinkyMcp.as_index()];
    (wrist + index_mcp + pinky_mcp) / 3.0
}

/// Unit normal to the palm plane, from the wrist→index-MCP and wrist→pinky-MCP
/// edges. Points out of the palm. Chirality flips the sign so both hands' normals
/// agree with the Leap convention (away from the back of the hand).
///
/// **Orientation-sensitive** (unlike the distance signals above): `lm` must
/// already be in the Leap orientation convention — x mirrored to match the
/// positional mirror, y up, z on the camera axis. The pipeline maps the metric
/// world landmarks through `world_to_leap_orientation` before calling this.
#[must_use]
pub fn palm_normal(lm: &[Vec3; LANDMARK_COUNT], chirality: Chirality) -> Vec3 {
    let wrist = lm[LandmarkIndex::Wrist.as_index()];
    let index_mcp = lm[LandmarkIndex::IndexMcp.as_index()];
    let pinky_mcp = lm[LandmarkIndex::PinkyMcp.as_index()];
    let a = index_mcp - wrist;
    let b = pinky_mcp - wrist;
    let n = a.cross(b);
    let n = n.normalize_or_zero();
    match chirality {
        Chirality::Right => n,
        Chirality::Left => -n,
    }
}

/// Smoothed palm velocity (NDC-or-mm units per second), a finite difference of
/// successive palm positions over `dt`.
#[must_use]
pub fn palm_velocity(prev: Vec3, cur: Vec3, dt: Duration) -> Vec3 {
    let secs = dt.as_secs_f32();
    if secs <= 0.0 {
        Vec3::ZERO
    } else {
        (cur - prev) / secs
    }
}

/// Consecutive frames an observed chirality must *disagree* with a track's held
/// chirality before the track flips to it. `MediaPipe`'s per-frame handedness
/// classification can flicker; holding the value across a few frames keeps the
/// track stable through a transient flip while still adapting if the hand really
/// is the other chirality. See [`HandTracker::assign`].
const CHIRALITY_FLIP_FRAMES: u8 = 4;

/// Time constant τ (seconds) of the per-track depth EMA in
/// [`HandTracker::assign`]. The raw size-estimated depth jitters with landmark
/// noise on the measured segment; a τ = 0.4 s exponential smoother
/// (`alpha = 1 − exp(−dt/τ)`) settles a step ~63 % in 0.4 s and ~95 % in 1.2 s —
/// slow enough to swallow per-frame noise, fast enough that a deliberate
/// push toward the camera reads within a beat. The render-rate One-Euro filter
/// ([`super::smoothing`]) adds its own, lighter pass downstream.
const DEPTH_EMA_TAU_S: f32 = 0.4;

/// The result of [`HandTracker::assign`]: a stable track id, the track's
/// held (hysteresis-smoothed) [`Chirality`], the palm position this track
/// held on the *previous* frame (for velocity), and the track's EMA-smoothed
/// depth.
// Not `Eq`: `prev_pos`/`depth_mm` carry `f32`s.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Assigned {
    /// Stable per-hand id, reused only after the hand leaves.
    pub id: u32,
    /// Chirality held across brief per-frame handedness flips.
    pub chirality: Chirality,
    /// The palm position this track held on the previous frame, or `None` on
    /// the hand's first sighting. Pair with the current palm position and the
    /// inter-frame `dt` in [`palm_velocity`] for a finite-difference velocity;
    /// without it velocity is undefined (a fresh track has no history). Its z
    /// is the previous frame's smoothed [`Self::depth_mm`], matching the palm
    /// position the pipeline emitted that frame.
    pub prev_pos: Option<Vec3>,
    /// EMA-smoothed depth (mm, Leap z convention; τ = [`DEPTH_EMA_TAU_S`]).
    /// Seeded with the raw estimate on the hand's first sighting. The pipeline
    /// writes this into the emitted palm position's z.
    pub depth_mm: f32,
    /// EMA-smoothed **physical** camera distance (mm; same τ as
    /// [`Self::depth_mm`]); `0.0` = estimator off (the `k <= 0` pin). Unlike
    /// the Leap-remapped depth this is unclamped, so it keeps tracking past
    /// the 1 m far rail — the value the pipeline emits as
    /// [`crate::input::hand::Hand::camera_distance_mm`]. The on/off boundary
    /// snaps instead of smoothing (see `ema_distance`): easing 0 ↔ positive
    /// would sweep through small values that read as "hand at the lens".
    pub distance_mm: f32,
}

/// Assigns stable per-hand IDs across frames.
///
/// `MediaPipe`'s landmark stage has no notion of track identity, so the provider
/// keeps its own: a detection inherits the id of the nearest previous-frame hand
/// whose palm is within [`Self::gate`] — matched by **position alone**.
/// Chirality is deliberately *not* part of the match key: `MediaPipe`'s
/// per-frame handedness can flip spuriously, and keying identity on it would
/// spawn a fresh id (resetting that hand's render-rate smoothing bank, which is
/// keyed on the id) on every flicker. Instead each track *holds* a chirality
/// that only flips after [`CHIRALITY_FLIP_FRAMES`] consecutive disagreements.
/// Tracks not seen in a frame age out, so IDs are reused only after a hand
/// leaves.
#[derive(Debug)]
pub struct HandTracker {
    tracks: Vec<Track>,
    next_id: u32,
    /// Max palm-distance (same units as the positions) for two frames to count
    /// as the same hand.
    gate: f32,
    /// Cumulative track lifecycle events (ids created + tracks aged out) since
    /// construction. A churn signal for diagnostics: a stable pair of hands
    /// leaves this flat, while flicker (acquire/lose loops, id swaps) makes it
    /// climb. See [`Self::churn`].
    churn: u64,
}

#[derive(Debug, Clone, Copy)]
struct Track {
    id: u32,
    /// The track's held chirality (see [`CHIRALITY_FLIP_FRAMES`]).
    chirality: Chirality,
    /// Consecutive frames the observed chirality disagreed with `chirality`.
    chirality_disagrees: u8,
    /// Palm position last frame; z holds [`Self::depth_mm`] (the smoothed
    /// depth), matching the palm position the pipeline emits.
    pos: Vec3,
    /// EMA-smoothed depth (mm); see [`DEPTH_EMA_TAU_S`].
    depth_mm: f32,
    /// EMA-smoothed physical camera distance (mm); `0.0` = estimator off.
    /// See [`Assigned::distance_mm`].
    distance_mm: f32,
    seen_this_frame: bool,
}

impl Default for HandTracker {
    fn default() -> Self {
        // 90 mm in the Leap-device-mm convention: wide enough that one hand's
        // inter-frame motion stays inside the gate, while two distinct hands
        // (typically hundreds of mm apart in this convention) stay outside it.
        // Raised from 60 mm after a hardware session: the letterbox
        // unprojection (ContentRect::to_content_norm) stretched a 720p
        // camera's vertical span onto the full Leap Y range, making vertical
        // motion ~1.78× faster in mm than when 60 was calibrated — fast waves
        // out-ran the old gate between inference frames and churned track ids
        // (resetting the per-id smoothing bank mid-gesture).
        Self {
            tracks: Vec::new(),
            next_id: 0,
            gate: 90.0,
            churn: 0,
        }
    }
}

impl HandTracker {
    /// Construct a tracker with a custom association gate.
    #[must_use]
    pub fn with_gate(gate: f32) -> Self {
        Self {
            gate,
            ..Self::default()
        }
    }

    /// Assign (or reuse) a track for a hand observed with `chirality` at palm
    /// position `pos`, returning its stable id, held chirality, and smoothed
    /// depth.
    ///
    /// `raw_depth_mm` is this frame's unsmoothed size-estimated depth (Leap z
    /// mm, [`super::coords::estimate_depth`]); the track EMA-smooths it
    /// (τ = [`DEPTH_EMA_TAU_S`], hence `dt`, the time since the previous
    /// processed frame) and stores the smoothed value as its position's z.
    /// `pos.z` itself is ignored — the gate is xy-only and the stored z is the
    /// smoothed depth. `raw_distance_mm` is the matching unsmoothed *physical*
    /// distance ([`super::coords::DepthEstimate::distance_mm`]; `0.0` =
    /// estimator off), EMA-smoothed with the same τ into
    /// [`Assigned::distance_mm`].
    pub fn assign(
        &mut self,
        chirality: Chirality,
        pos: Vec3,
        raw_depth_mm: f32,
        raw_distance_mm: f32,
        dt: Duration,
    ) -> Assigned {
        // Nearest unclaimed track within the gate — by POSITION ALONE (chirality
        // is held per-track, not matched on; see the type docs), and by **xy
        // only**: the raw size-estimated depth is far noisier than the image
        // xy, and a single-frame z spike larger than the gate would otherwise
        // spawn a fresh id (resetting that hand's render-rate smoothing bank).
        // Zeroing z preserves the pre-estimator behaviour exactly, where every
        // track's z was the same 120 mm pin and so never affected the distance.
        let mut best: Option<(usize, f32)> = None;
        for (i, t) in self.tracks.iter().enumerate() {
            if t.seen_this_frame {
                continue;
            }
            let d = t.pos.truncate().distance(pos.truncate());
            if d <= self.gate && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        if let Some((i, _)) = best {
            let t = &mut self.tracks[i];
            // Capture last frame's position before overwriting, so the caller can
            // finite-difference it against `pos` for velocity.
            let prev_pos = Some(t.pos);
            // Depth EMA: alpha = 1 − e^(−dt/τ) is the exact discrete step of a
            // first-order low-pass with time constant τ, so smoothing strength
            // is framerate-independent (dt = 0 → alpha = 0 → unchanged).
            let alpha = 1.0 - (-dt.as_secs_f32() / DEPTH_EMA_TAU_S).exp();
            t.depth_mm = alpha.mul_add(raw_depth_mm - t.depth_mm, t.depth_mm);
            t.distance_mm = ema_distance(t.distance_mm, raw_distance_mm, alpha);
            // Store the smoothed depth as z so prev_pos finite-differences into
            // a velocity consistent with the emitted palm position.
            t.pos = Vec3::new(pos.x, pos.y, t.depth_mm);
            t.seen_this_frame = true;
            // Sticky chirality: only flip after CHIRALITY_FLIP_FRAMES consecutive
            // disagreements, so a one-frame handedness flicker is ignored.
            if chirality == t.chirality {
                t.chirality_disagrees = 0;
            } else {
                t.chirality_disagrees += 1;
                if t.chirality_disagrees >= CHIRALITY_FLIP_FRAMES {
                    t.chirality = chirality;
                    t.chirality_disagrees = 0;
                }
            }
            return Assigned {
                id: t.id,
                chirality: t.chirality,
                prev_pos,
                depth_mm: t.depth_mm,
                distance_mm: t.distance_mm,
            };
        }
        let id = self.next_id;
        self.next_id += 1;
        self.churn = self.churn.saturating_add(1); // a new id is a churn event
        self.tracks.push(Track {
            id,
            chirality,
            chirality_disagrees: 0,
            // First sighting: seed the depth EMA with the raw estimate (no
            // history to smooth against) and store it as the position's z.
            pos: Vec3::new(pos.x, pos.y, raw_depth_mm),
            depth_mm: raw_depth_mm,
            distance_mm: raw_distance_mm.max(0.0),
            seen_this_frame: true,
        });
        // A brand-new track has no previous position → velocity starts at zero.
        Assigned {
            id,
            chirality,
            prev_pos: None,
            depth_mm: raw_depth_mm,
            distance_mm: raw_distance_mm.max(0.0),
        }
    }

    /// Call once per frame after all `assign` calls: drop tracks not seen this
    /// frame and reset the per-frame flags.
    pub fn end_frame(&mut self) {
        let before = self.tracks.len();
        self.tracks.retain(|t| t.seen_this_frame);
        let dropped = u64::try_from(before - self.tracks.len()).unwrap_or(0);
        self.churn = self.churn.saturating_add(dropped); // aged-out tracks churn too
        for t in &mut self.tracks {
            t.seen_this_frame = false;
        }
    }

    /// Cumulative track churn (ids created + tracks aged out) since construction.
    /// Surfaced in pipeline diagnostics so hardware tests can tell stable
    /// tracking from acquire/lose flicker. Monotonic; never reset.
    #[must_use]
    pub fn churn(&self) -> u64 {
        self.churn
    }
}

/// One EMA step of a track's physical camera distance, with sentinel-aware
/// boundaries: `0.0` means "estimator off / unknown" (the `k <= 0` pin), and
/// smoothing *across* that boundary would sweep the value through small
/// positive readings that downstream consumers interpret as "hand at the
/// lens" (the audio band's loudest rail). So the off↔on transitions snap —
/// raw `<= 0` returns `0.0` immediately, and a track whose current value is
/// `0.0` re-seeds from the first positive raw estimate — while steady
/// positive readings smooth with the caller's `alpha` (the same
/// [`DEPTH_EMA_TAU_S`] step as the Leap-z depth).
/// (The conditions are written `!(x > 0.0)` rather than `x <= 0.0` so a NaN
/// — e.g. a degenerate world-landmark segment upstream — routes through the
/// snap branch, where `raw.max(0.0)` maps it to the 0.0 sentinel, instead of
/// entering the EMA and poisoning the track's distance for its lifetime.)
#[allow(
    clippy::neg_cmp_op_on_partial_ord,
    reason = "the negation is the point: `!(x > 0.0)` is true for NaN where `x <= 0.0` is not, \
              routing NaN through the sentinel-snap branch instead of poisoning the EMA"
)]
fn ema_distance(current: f32, raw: f32, alpha: f32) -> f32 {
    if !(raw > 0.0) || !(current > 0.0) {
        raw.max(0.0)
    } else {
        alpha.mul_add(raw - current, current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An open right hand roughly in the XY plane: wrist at origin, fingers
    /// extended along +Y, thumb out along +X. Units are arbitrary but
    /// self-consistent.
    fn open_hand() -> [Vec3; LANDMARK_COUNT] {
        let mut lm = [Vec3::ZERO; LANDMARK_COUNT];
        lm[LandmarkIndex::Wrist.as_index()] = Vec3::new(0.0, 0.0, 0.0);
        // MCP knuckles across the palm.
        lm[LandmarkIndex::IndexMcp.as_index()] = Vec3::new(-0.3, 1.0, 0.0);
        lm[LandmarkIndex::MiddleMcp.as_index()] = Vec3::new(0.0, 1.0, 0.0);
        lm[LandmarkIndex::RingMcp.as_index()] = Vec3::new(0.3, 1.0, 0.0);
        lm[LandmarkIndex::PinkyMcp.as_index()] = Vec3::new(0.6, 1.0, 0.0);
        // Fingertips extended out to ~2.0 (about one hand-scale beyond the palm).
        lm[LandmarkIndex::IndexTip.as_index()] = Vec3::new(-0.3, 2.0, 0.0);
        lm[LandmarkIndex::MiddleTip.as_index()] = Vec3::new(0.0, 2.0, 0.0);
        lm[LandmarkIndex::RingTip.as_index()] = Vec3::new(0.3, 2.0, 0.0);
        lm[LandmarkIndex::PinkyTip.as_index()] = Vec3::new(0.6, 2.0, 0.0);
        // Thumb out to the side, tip far from the index tip.
        lm[LandmarkIndex::ThumbTip.as_index()] = Vec3::new(-1.2, 0.6, 0.0);
        lm
    }

    #[test]
    fn open_hand_has_low_pinch_and_grab() {
        let lm = open_hand();
        assert!(pinch_strength(&lm) < 0.2, "pinch={}", pinch_strength(&lm));
        assert!(grab_strength(&lm) < 0.2, "grab={}", grab_strength(&lm));
    }

    #[test]
    fn touching_thumb_and_index_reads_full_pinch() {
        let mut lm = open_hand();
        let p = Vec3::new(0.0, 1.5, 0.0);
        lm[LandmarkIndex::ThumbTip.as_index()] = p;
        lm[LandmarkIndex::IndexTip.as_index()] = p;
        assert!(pinch_strength(&lm) > 0.9, "pinch={}", pinch_strength(&lm));
    }

    #[test]
    fn curled_fingers_read_high_grab() {
        let mut lm = open_hand();
        // Curl the four fingertips back toward the palm centre.
        let palm = palm_center(&lm);
        for t in [
            LandmarkIndex::IndexTip,
            LandmarkIndex::MiddleTip,
            LandmarkIndex::RingTip,
            LandmarkIndex::PinkyTip,
        ] {
            lm[t.as_index()] = palm + Vec3::new(0.0, 0.1, 0.0);
        }
        assert!(grab_strength(&lm) > 0.8, "grab={}", grab_strength(&lm));
    }

    #[test]
    fn palm_normal_is_perpendicular_to_a_planar_hand() {
        let lm = open_hand();
        let n = palm_normal(&lm, Chirality::Right);
        // Hand lies in the XY plane → normal along ±Z.
        assert!(n.x.abs() < 1e-5 && n.y.abs() < 1e-5, "n={n:?}");
        assert!((n.z.abs() - 1.0).abs() < 1e-4, "n={n:?}");
    }

    #[test]
    fn left_and_right_palm_normals_oppose() {
        let lm = open_hand();
        let r = palm_normal(&lm, Chirality::Right);
        let l = palm_normal(&lm, Chirality::Left);
        assert!((r + l).length() < 1e-5, "r={r:?} l={l:?}");
    }

    #[test]
    fn velocity_is_finite_difference() {
        let v = palm_velocity(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(10.0, 0.0, 0.0),
            Duration::from_millis(100),
        );
        assert!((v.x - 100.0).abs() < 1e-3, "v={v:?}");
        // Zero dt is safe.
        assert_eq!(
            palm_velocity(Vec3::ZERO, Vec3::ONE, Duration::ZERO),
            Vec3::ZERO
        );
    }

    /// [`HandTracker::assign`] with a constant raw depth (the old fixed pin),
    /// an unknown (0) physical distance, and a nominal 33 ms inter-frame dt —
    /// for tests exercising the id/chirality logic where depth is irrelevant.
    fn assign_at(t: &mut HandTracker, chirality: Chirality, pos: Vec3) -> Assigned {
        t.assign(chirality, pos, 120.0, 0.0, Duration::from_millis(33))
    }

    #[test]
    fn tracker_reports_prev_pos_for_velocity() {
        // First sighting has no history; the next frame reports last frame's
        // position so the caller can finite-difference it into a velocity.
        let mut t = HandTracker::default();
        let first = assign_at(&mut t, Chirality::Right, Vec3::new(0.0, 200.0, 0.0));
        assert_eq!(first.prev_pos, None, "fresh track has no previous position");
        t.end_frame();
        let moved = assign_at(&mut t, Chirality::Right, Vec3::new(10.0, 200.0, 0.0));
        assert_eq!(
            // The track stores its smoothed depth as z (here the constant 120
            // raw seed), so prev_pos carries it — consistent with the palm
            // position the pipeline emits after writing the smoothed depth.
            moved.prev_pos,
            Some(Vec3::new(0.0, 200.0, 120.0)),
            "prev_pos is the position held last frame (z = smoothed depth)",
        );
        // (10 mm − 0 mm) / 0.1 s = 100 mm/s in x.
        let v = palm_velocity(
            moved.prev_pos.unwrap_or(Vec3::ZERO),
            Vec3::new(10.0, 200.0, 0.0),
            Duration::from_millis(100),
        );
        assert!((v.x - 100.0).abs() < 1e-3, "v={v:?}");
    }

    #[test]
    fn tracker_keeps_id_for_nearby_hand() {
        let mut t = HandTracker::default();
        let a = assign_at(&mut t, Chirality::Right, Vec3::new(0.0, 200.0, 0.0));
        t.end_frame();
        let b = assign_at(&mut t, Chirality::Right, Vec3::new(5.0, 205.0, 0.0));
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn fast_wave_jump_inside_widened_gate_keeps_id() {
        // Hardware regression: a fast vertical wave can move the palm ~70 mm
        // between inference frames now that the letterbox unprojection maps
        // the full camera height onto the Leap Y range (~1.78× faster in mm
        // than when the old 60 mm gate was calibrated). 70 mm > 60 churned the
        // id (and reset the per-id smoothing bank) mid-wave; the 90 mm gate
        // must keep identity.
        let mut t = HandTracker::default();
        let a = assign_at(&mut t, Chirality::Right, Vec3::new(0.0, 200.0, 0.0));
        t.end_frame();
        let b = assign_at(&mut t, Chirality::Right, Vec3::new(0.0, 270.0, 0.0));
        assert_eq!(a.id, b.id, "a 70 mm inter-frame jump must not churn the id");
    }

    #[test]
    fn tracker_gives_new_id_for_far_hand() {
        let mut t = HandTracker::default();
        let a = assign_at(&mut t, Chirality::Right, Vec3::new(-200.0, 100.0, 0.0));
        t.end_frame();
        let b = assign_at(&mut t, Chirality::Right, Vec3::new(200.0, 300.0, 0.0));
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn tracker_keeps_id_and_held_chirality_through_a_one_frame_flip() {
        // A hand stays put while its handedness flickers for a single frame.
        // Identity is matched by position, so the id must NOT churn (which would
        // reset the smoothing bank), and the held chirality must not flip on a
        // lone disagreement.
        let mut t = HandTracker::default();
        let a = assign_at(&mut t, Chirality::Right, Vec3::new(0.0, 200.0, 0.0));
        t.end_frame();
        let b = assign_at(&mut t, Chirality::Left, Vec3::new(2.0, 201.0, 0.0));
        assert_eq!(a.id, b.id, "id stable across a chirality flicker");
        assert_eq!(
            b.chirality,
            Chirality::Right,
            "held chirality ignores a one-frame flip",
        );
    }

    #[test]
    fn tracker_flips_held_chirality_after_sustained_disagreement() {
        // Sustained disagreement (the hand really is the other chirality) flips
        // the held value after CHIRALITY_FLIP_FRAMES, keeping the same id.
        let mut t = HandTracker::default();
        let first = assign_at(&mut t, Chirality::Right, Vec3::new(0.0, 200.0, 0.0));
        let mut last = first;
        for _ in 0..CHIRALITY_FLIP_FRAMES {
            t.end_frame();
            last = assign_at(&mut t, Chirality::Left, Vec3::new(0.0, 200.0, 0.0));
        }
        assert_eq!(first.id, last.id, "id stays stable while chirality settles");
        assert_eq!(last.chirality, Chirality::Left, "flips after enough frames");
    }

    #[test]
    fn tracker_churn_counts_creations_and_drops() {
        let mut t = HandTracker::default();
        assert_eq!(t.churn(), 0);
        // Two fresh ids → churn 2.
        assign_at(&mut t, Chirality::Right, Vec3::new(-150.0, 200.0, 0.0));
        assign_at(&mut t, Chirality::Right, Vec3::new(150.0, 200.0, 0.0));
        assert_eq!(t.churn(), 2, "two new ids");
        // Next frame sees only one of them → the other ages out (+1 drop).
        t.end_frame();
        assign_at(&mut t, Chirality::Right, Vec3::new(-150.0, 200.0, 0.0));
        t.end_frame();
        assert_eq!(t.churn(), 3, "one track aged out");
        // A stable hand adds no churn.
        assign_at(&mut t, Chirality::Right, Vec3::new(-150.0, 200.0, 0.0));
        t.end_frame();
        assert_eq!(t.churn(), 3, "stable track does not churn");
    }

    // --- per-track depth EMA + xy-only gating (Phase P5) -------------------

    #[test]
    fn depth_seeds_raw_on_first_sighting() {
        // A fresh track has no depth history; the EMA seeds with the raw
        // estimate rather than easing in from some default.
        let mut t = HandTracker::default();
        let a = t.assign(
            Chirality::Right,
            Vec3::new(0.0, 200.0, 0.0),
            97.5,
            487.5,
            Duration::from_millis(33),
        );
        assert!((a.depth_mm - 97.5).abs() < 1e-6, "depth {}", a.depth_mm);
        // The physical distance seeds raw on first sighting too.
        assert!(
            (a.distance_mm - 487.5).abs() < 1e-6,
            "distance {}",
            a.distance_mm
        );
    }

    #[test]
    fn depth_ema_converges_with_tau_0_4s() {
        // τ = 0.4 s time-constant EMA: after a single dt = τ step toward a new
        // target, the smoothed depth is 1 − e⁻¹ ≈ 63.2 % of the way there.
        let mut t = HandTracker::default();
        let pos = Vec3::new(0.0, 200.0, 0.0);
        t.assign(
            Chirality::Right,
            pos,
            100.0,
            500.0,
            Duration::from_millis(33),
        );
        t.end_frame();
        let stepped = t.assign(
            Chirality::Right,
            pos,
            300.0,
            1500.0,
            Duration::from_secs_f32(0.4),
        );
        // 100 + (1 − e⁻¹) · (300 − 100) ≈ 226.4 mm.
        let want = 200.0f32.mul_add(1.0 - (-1.0f32).exp(), 100.0);
        assert!(
            (stepped.depth_mm - want).abs() < 0.5,
            "depth {} (want ≈ {want})",
            stepped.depth_mm
        );
        // The physical distance smooths with the SAME τ: one dt = τ step from
        // 500 toward 1500 lands ≈ 63.2 % of the way (≈ 1132 mm).
        let want_dist = 1000.0f32.mul_add(1.0 - (-1.0f32).exp(), 500.0);
        assert!(
            (stepped.distance_mm - want_dist).abs() < 1.0,
            "distance {} (want ≈ {want_dist})",
            stepped.distance_mm
        );
        // And it keeps converging on subsequent steps (monotonic toward 300).
        t.end_frame();
        let again = t.assign(
            Chirality::Right,
            pos,
            300.0,
            1500.0,
            Duration::from_secs_f32(0.4),
        );
        assert!(
            again.depth_mm > stepped.depth_mm && again.depth_mm < 300.0,
            "depth {} should keep approaching 300",
            again.depth_mm
        );
    }

    #[test]
    fn gate_compares_xy_only_so_depth_jumps_keep_identity() {
        // Two assigns differing ONLY in z (a full-range raw-depth jump, far
        // larger than the 90 mm gate) must keep the same track id: identity
        // association is xy-only so depth noise can never churn ids (which
        // would reset the render-rate smoothing bank keyed on the id).
        let mut t = HandTracker::default();
        let a = t.assign(
            Chirality::Right,
            Vec3::new(0.0, 200.0, 40.0),
            40.0,
            350.0,
            Duration::from_millis(33),
        );
        t.end_frame();
        let b = t.assign(
            Chirality::Right,
            Vec3::new(0.0, 200.0, 350.0),
            350.0,
            1000.0,
            Duration::from_millis(33),
        );
        assert_eq!(a.id, b.id, "a raw z jump must not break identity");
    }

    #[test]
    fn distance_ema_snaps_across_the_estimator_off_boundary() {
        // 0.0 is the "estimator off / unknown" sentinel, not a distance.
        // Smoothing across the boundary would sweep through small positive
        // values that read as "hand at the lens" (loudest audio rail), so
        // both transitions snap.
        let mut t = HandTracker::default();
        let pos = Vec3::new(0.0, 200.0, 0.0);
        t.assign(
            Chirality::Right,
            pos,
            100.0,
            800.0,
            Duration::from_millis(33),
        );
        t.end_frame();
        // Estimator turned off mid-track (k slider → 0): snap to 0 at once.
        let off = t.assign(Chirality::Right, pos, 120.0, 0.0, Duration::from_millis(33));
        assert!(
            off.distance_mm.abs() < f32::EPSILON,
            "off must snap, not decay through near-zero: {}",
            off.distance_mm
        );
        t.end_frame();
        // Turned back on: re-seed from the first positive raw, no ease-in from 0.
        let on = t.assign(
            Chirality::Right,
            pos,
            100.0,
            750.0,
            Duration::from_millis(33),
        );
        assert!((on.distance_mm - 750.0).abs() < 1e-6, "{}", on.distance_mm);
    }

    #[test]
    fn nan_raw_distance_snaps_to_unknown_and_recovers() {
        // A NaN raw distance (degenerate world landmarks upstream) must route
        // through the snap branch to the 0.0 sentinel — entering the EMA would
        // poison the track's distance for its remaining lifetime.
        let mut t = HandTracker::default();
        let pos = Vec3::new(0.0, 200.0, 0.0);
        t.assign(
            Chirality::Right,
            pos,
            100.0,
            800.0,
            Duration::from_millis(33),
        );
        t.end_frame();
        let bad = t.assign(
            Chirality::Right,
            pos,
            100.0,
            f32::NAN,
            Duration::from_millis(33),
        );
        assert!(
            bad.distance_mm.abs() < f32::EPSILON,
            "NaN must snap to the unknown sentinel, got {}",
            bad.distance_mm
        );
        t.end_frame();
        // And the track recovers on the next good estimate (re-seed).
        let ok = t.assign(
            Chirality::Right,
            pos,
            100.0,
            600.0,
            Duration::from_millis(33),
        );
        assert!((ok.distance_mm - 600.0).abs() < 1e-6, "{}", ok.distance_mm);
        // A NaN on a FRESH track seeds the sentinel too (max(0.0) maps NaN→0).
        let mut t2 = HandTracker::default();
        let fresh = t2.assign(
            Chirality::Right,
            Vec3::new(150.0, 200.0, 0.0),
            100.0,
            f32::NAN,
            Duration::from_millis(33),
        );
        assert!(
            fresh.distance_mm.abs() < f32::EPSILON,
            "{}",
            fresh.distance_mm
        );
    }

    #[test]
    fn tracker_separates_two_hands_by_position_regardless_of_chirality() {
        // Two hands far apart keep distinct ids even with the same observed
        // chirality (position-based association).
        let mut t = HandTracker::default();
        let left_hand = assign_at(&mut t, Chirality::Right, Vec3::new(-150.0, 200.0, 0.0));
        let right_hand = assign_at(&mut t, Chirality::Right, Vec3::new(150.0, 200.0, 0.0));
        assert_ne!(left_hand.id, right_hand.id);
    }
}
