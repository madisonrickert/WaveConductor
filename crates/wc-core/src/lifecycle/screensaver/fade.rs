//! Screensaver fade envelope (Plan 11.8, Seam 2).
//!
//! A single 0..1 value that ramps toward `1.0` while the screensaver is active
//! and back toward `0.0` when it is not. An attract layer that wants to
//! appear/disappear gracefully multiplies its opacity by this value so the
//! transition into and out of attract mode is a smooth fade rather than a
//! snap. (The instruction-caption overlay that originally consumed it was cut
//! 2026-06-10; the envelope stays for the attract-mode color/visual layers.)
//!
//! Kept deliberately tiny and framework-level (not Line-specific): the Line
//! attract driver runs its own per-pulse choreography; this is only the
//! coarse-grained "are we showing attract content, and how strongly" envelope.
//!
//! ## Color correction for HDR sketches (read this before porting a new sketch)
//!
//! Sketches rendered through the global HDR + `AgX` camera (Cymatics, and any
//! future HDR sketch port) need a screensaver-only **brightness lift** driven by
//! this envelope, not just an opacity cross-fade. The reason: a gentle,
//! low-energy idle/attract field has low *linear* luminance, which lands in
//! `AgX`'s dark, desaturated toe — so it reads muted / near-black even while it is
//! animating, whereas active play drives the field bright into `AgX`'s vivid
//! range. The fix is **presentation, not physics** (raising wave energy to
//! compensate would fight the "gentle" feel and risk soak instability): multiply
//! the sketch's master brightness by `1.0 + (attract_brightness - 1.0) *
//! fade.alpha()` so the calm field lifts up `AgX`'s curve into the vivid range
//! during the screensaver while staying gentle. At `fade.alpha() == 0` (active)
//! the factor is `1.0`, so active rendering is unchanged. Dots, Line, and
//! Cymatics all do this via a per-sketch `attract_brightness` Dev setting; a new
//! HDR sketch's attract layer should too.

use std::time::Duration;

use bevy::prelude::*;

use crate::lifecycle::state::SketchActivity;

/// How long the fade takes to travel the full 0↔1 range, in seconds. Slow
/// enough to read as a deliberate dissolve, fast enough that the attract
/// content arrives promptly.
const FADE_DURATION_SECS: f32 = 1.5;

/// Coarse attract-mode opacity envelope. `0.0` = fully hidden (not in attract),
/// `1.0` = fully shown. Ramps linearly between the two as the screensaver state
/// toggles. Available to attract layers.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Default)]
pub struct ScreensaverFade {
    /// Current envelope value in `0..=1`.
    value: f32,
    /// Target the value is ramping toward (`1.0` in screensaver, else `0.0`).
    target: f32,
}

impl ScreensaverFade {
    /// Current envelope value in `0..=1`.
    #[must_use]
    pub fn alpha(&self) -> f32 {
        self.value
    }

    /// Set the ramp target (`1.0` to fade in, `0.0` to fade out).
    pub fn set_target(&mut self, target: f32) {
        self.target = target.clamp(0.0, 1.0);
    }

    /// Advance the envelope toward its target by `dt`, returning the new value.
    /// Pure helper so the ramp math is unit-testable without a `World`.
    #[must_use]
    pub fn advanced(mut self, dt: Duration) -> Self {
        if FADE_DURATION_SECS <= 0.0 {
            self.value = self.target;
            return self;
        }
        let step = dt.as_secs_f32() / FADE_DURATION_SECS;
        if self.value < self.target {
            self.value = (self.value + step).min(self.target);
        } else if self.value > self.target {
            self.value = (self.value - step).max(self.target);
        }
        self
    }
}

/// Set [`ScreensaverFade`]'s target from the current [`SketchActivity`] and
/// advance the envelope each frame.
///
/// Runs unconditionally (cheap): when not in a sketch state the sub-state is
/// absent and the target falls to `0.0`, so the fade settles to hidden. The
/// envelope's own ramp guard makes this a near-no-op once settled.
pub fn drive_screensaver_fade(
    time: Res<'_, Time>,
    activity: Option<Res<'_, State<SketchActivity>>>,
    mut fade: ResMut<'_, ScreensaverFade>,
) {
    let in_screensaver = activity.is_some_and(|a| *a.get() == SketchActivity::Screensaver);
    fade.set_target(if in_screensaver { 1.0 } else { 0.0 });
    let next = fade.advanced(time.delta());
    if *fade != next {
        *fade = next;
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "ramp endpoints are exact; equality is the intended comparison"
)]
mod tests {
    use super::*;

    #[test]
    fn default_is_hidden() {
        assert_eq!(ScreensaverFade::default().alpha(), 0.0);
    }

    #[test]
    fn ramps_up_toward_target() {
        let mut f = ScreensaverFade::default();
        f.set_target(1.0);
        // One full duration of dt reaches the target exactly.
        f = f.advanced(Duration::from_secs_f32(FADE_DURATION_SECS));
        assert_eq!(f.alpha(), 1.0);
    }

    #[test]
    fn ramps_down_toward_zero() {
        let mut f = ScreensaverFade {
            value: 1.0,
            target: 0.0,
        };
        f = f.advanced(Duration::from_secs_f32(FADE_DURATION_SECS));
        assert_eq!(f.alpha(), 0.0);
    }

    #[test]
    fn does_not_overshoot_on_large_dt() {
        let mut f = ScreensaverFade::default();
        f.set_target(1.0);
        f = f.advanced(Duration::from_secs(100));
        assert_eq!(f.alpha(), 1.0, "must clamp at target, not overshoot");
    }

    #[test]
    fn partial_ramp_is_proportional() {
        let mut f = ScreensaverFade::default();
        f.set_target(1.0);
        f = f.advanced(Duration::from_secs_f32(FADE_DURATION_SECS / 2.0));
        assert!((f.alpha() - 0.5).abs() < 1e-5, "half-duration → ~0.5");
    }
}
