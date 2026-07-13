//! `SketchActivity` → activation-request sync.
//!
//! The Plan A/B request resources carry live pause knobs (`paused` on the
//! audio capture, `idle_throttle` on body tracking). Radiance flips both on
//! the activity seams: Idle and Screensaver pause the mic analysis (the
//! attract mode is not audio-reactive) and drop the body worker to its
//! detector-only idle rate (so a person walking up still re-activates the
//! sketch via the presence → `InteractionTimer` path Plan B owns); Active
//! restores both. Registered on `OnEnter` of each activity, gated
//! `in_state(AppState::Radiance)` — zero per-frame cost.

use bevy::prelude::*;
use wc_core::audio::input::AudioCaptureRequest;
use wc_core::input::body::BodyTrackingRequest;

/// `OnEnter(SketchActivity::Idle)` / `OnEnter(SketchActivity::Screensaver)`:
/// pause capture, throttle tracking. Both resources are optional — the
/// synthetic capture path never inserts them.
pub fn pause_tracking_requests(
    mut audio: Option<ResMut<'_, AudioCaptureRequest>>,
    mut body: Option<ResMut<'_, BodyTrackingRequest>>,
) {
    if let Some(audio) = audio.as_mut() {
        if !audio.paused {
            audio.paused = true;
        }
    }
    if let Some(body) = body.as_mut() {
        if !body.idle_throttle {
            body.idle_throttle = true;
        }
    }
}

/// `OnEnter(SketchActivity::Active)`: resume capture + full-rate tracking.
pub fn resume_tracking_requests(
    mut audio: Option<ResMut<'_, AudioCaptureRequest>>,
    mut body: Option<ResMut<'_, BodyTrackingRequest>>,
) {
    if let Some(audio) = audio.as_mut() {
        if audio.paused {
            audio.paused = false;
        }
    }
    if let Some(body) = body.as_mut() {
        if body.idle_throttle {
            body.idle_throttle = false;
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn pause_and_resume_flip_both_requests() {
        let mut world = World::new();
        world.insert_resource(AudioCaptureRequest {
            device_name: None,
            paused: false,
        });
        world.insert_resource(BodyTrackingRequest {
            idle_throttle: false,
            mask_ema: 0.6,
            one_euro_min_cutoff: 1.0,
            one_euro_beta: 0.05,
        });
        world
            .run_system_once(pause_tracking_requests)
            .expect("pause");
        assert!(world.resource::<AudioCaptureRequest>().paused);
        assert!(world.resource::<BodyTrackingRequest>().idle_throttle);
        world
            .run_system_once(resume_tracking_requests)
            .expect("resume");
        assert!(!world.resource::<AudioCaptureRequest>().paused);
        assert!(!world.resource::<BodyTrackingRequest>().idle_throttle);
    }

    #[test]
    fn absent_requests_are_a_no_op() {
        let mut world = World::new();
        world
            .run_system_once(pause_tracking_requests)
            .expect("pause");
        world
            .run_system_once(resume_tracking_requests)
            .expect("resume");
    }
}
