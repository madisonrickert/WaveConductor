//! Camera arbitration: body replaces hands while Radiance is active.
//!
//! `OnEnter(AppState::Radiance)`: if a `MediaPipe` (webcam) hand provider is
//! registered, stop it synchronously — the worker joins and releases the
//! camera — so Plan B's body worker can open the same device. The provider
//! stays *registered* (its `NotStarted` status is the honest dev-panel
//! signal that it is suspended). Leap is untouched; Radiance never reads
//! hand data.
//!
//! `OnExit`: restart is *deferred* ~0.75 s via [`PendingHandCameraRestore`]
//! so the body worker (torn down by the request removal, observed by Plan
//! B's watcher on the following frame) has released the camera before the
//! hand provider re-opens it. The restore listener is registered always-on
//! and early-outs on a `None` resource in one branch — the same
//! cheap-no-op contract as the sanctioned reload listeners; it self-removes
//! after firing.

use std::time::Duration;

use bevy::prelude::*;
use wc_core::input::provider::{ProviderId, ProviderRegistry};

/// Marker: Radiance stopped the `MediaPipe` hand provider on entry and owes a
/// restart on exit.
#[derive(Resource)]
pub struct SuspendedHandCamera;

/// Deferred restore: restart the suspended `MediaPipe` hand provider once
/// `Time::elapsed` passes `at`.
#[derive(Resource)]
pub struct PendingHandCameraRestore {
    /// Instant (Bevy `Time::elapsed`) after which the restart runs.
    pub at: Duration,
}

/// How long after exit to wait before re-opening the hand camera, giving the
/// body worker's teardown time to release the device.
pub const RESTORE_DELAY: Duration = Duration::from_millis(750);

/// `OnEnter(AppState::Radiance)`: stop a registered `MediaPipe` hand provider
/// (releasing the webcam) and remember to restore it.
pub fn suspend_mediapipe_hand_camera(
    registry: Option<ResMut<'_, ProviderRegistry>>,
    mut commands: Commands<'_, '_>,
) {
    let Some(mut registry) = registry else {
        return; // headless / hand tracking not installed
    };
    let Some(slot) = registry.iter_mut().find(|p| p.id == ProviderId::MediaPipe) else {
        return; // Leap / mock / Off: nothing to arbitrate
    };
    slot.inner.stop();
    tracing::info!(
        "radiance: suspended the MediaPipe hand provider (webcam handed to body tracking)"
    );
    commands.insert_resource(SuspendedHandCamera);
}

/// `OnExit(AppState::Radiance)`: schedule the deferred restore (only if we
/// actually suspended on entry).
pub fn schedule_hand_camera_restore(
    suspended: Option<Res<'_, SuspendedHandCamera>>,
    time: Res<'_, Time>,
    mut commands: Commands<'_, '_>,
) {
    if suspended.is_none() {
        return;
    }
    commands.remove_resource::<SuspendedHandCamera>();
    commands.insert_resource(PendingHandCameraRestore {
        at: time.elapsed() + RESTORE_DELAY,
    });
}

/// Always-on `Update` listener: one `Option` branch in the steady state;
/// restarts the `MediaPipe` hand provider once the delay passes, then removes
/// itself. A start failure is logged and stays visible as the provider's
/// honest `Errored` status (the house failure philosophy); re-picking the
/// provider in the tracking dropdown re-probes.
pub fn resume_hand_camera_when_due(
    pending: Option<Res<'_, PendingHandCameraRestore>>,
    time: Res<'_, Time>,
    registry: Option<ResMut<'_, ProviderRegistry>>,
    mut commands: Commands<'_, '_>,
) {
    let Some(pending) = pending else {
        return; // steady-state no-op
    };
    if time.elapsed() < pending.at {
        return;
    }
    commands.remove_resource::<PendingHandCameraRestore>();
    let Some(mut registry) = registry else {
        return;
    };
    let Some(slot) = registry.iter_mut().find(|p| p.id == ProviderId::MediaPipe) else {
        // The operator switched providers while Radiance ran; nothing owed.
        return;
    };
    match slot.inner.start() {
        Ok(()) => tracing::info!("radiance: restored the MediaPipe hand provider"),
        Err(err) => tracing::error!(
            ?err,
            "radiance: failed to restore the MediaPipe hand provider; its status \
             stays visible in the dev panel"
        ),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use wc_core::input::provider::{HandTrackingProvider, ProviderRole};
    use wc_core::input::state::{
        HandTrackingError, HandTrackingFrame, ProviderDiagnostics, ProviderStatus,
    };

    /// Scripted provider counting start/stop calls (mirrors the binary's
    /// `ServiceStub` test pattern).
    struct CountingStub {
        starts: Arc<AtomicUsize>,
        stops: Arc<AtomicUsize>,
    }

    impl HandTrackingProvider for CountingStub {
        fn start(&mut self) -> Result<(), HandTrackingError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn stop(&mut self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
        fn poll(&mut self, _now: Duration, _out: &mut Messages<HandTrackingFrame>) {}
        fn status(&self) -> ProviderStatus {
            ProviderStatus::default()
        }
        fn diagnostics(&self) -> ProviderDiagnostics {
            ProviderDiagnostics::default()
        }
    }

    fn registry_with(id: ProviderId) -> (ProviderRegistry, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let starts = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let mut registry = ProviderRegistry::default();
        registry.register(
            id,
            ProviderRole::Primary,
            Box::new(CountingStub {
                starts: Arc::clone(&starts),
                stops: Arc::clone(&stops),
            }),
        );
        (registry, starts, stops)
    }

    /// Suspend stops a registered `MediaPipe` provider and marks the debt;
    /// the deferred restore fires once the delay passes and self-removes.
    #[test]
    fn suspend_then_deferred_restore_round_trip() {
        use bevy::ecs::system::RunSystemOnce;
        let (registry, starts, stops) = registry_with(ProviderId::MediaPipe);
        // register() auto-starts once; ignore that baseline.
        let base_starts = starts.load(Ordering::SeqCst);

        let mut world = World::new();
        world.insert_resource(registry);
        world.insert_resource(Time::<()>::default());

        world
            .run_system_once(suspend_mediapipe_hand_camera)
            .expect("suspend");
        assert_eq!(stops.load(Ordering::SeqCst), 1, "provider stopped");
        assert!(world.get_resource::<SuspendedHandCamera>().is_some());

        world
            .run_system_once(schedule_hand_camera_restore)
            .expect("schedule");
        assert!(world.get_resource::<SuspendedHandCamera>().is_none());
        assert!(world.get_resource::<PendingHandCameraRestore>().is_some());

        // Before the delay: no restart.
        world
            .run_system_once(resume_hand_camera_when_due)
            .expect("early poll");
        assert_eq!(starts.load(Ordering::SeqCst), base_starts);

        // Advance past the delay: restart fires exactly once and clears.
        let mut time = Time::<()>::default();
        time.advance_by(RESTORE_DELAY + Duration::from_millis(10));
        world.insert_resource(time);
        world
            .run_system_once(resume_hand_camera_when_due)
            .expect("due poll");
        assert_eq!(starts.load(Ordering::SeqCst), base_starts + 1);
        assert!(world.get_resource::<PendingHandCameraRestore>().is_none());
    }

    /// A non-`MediaPipe` registry (Leap) is untouched: no suspend marker, no
    /// stop.
    #[test]
    fn leap_registry_is_untouched() {
        use bevy::ecs::system::RunSystemOnce;
        let (registry, _starts, stops) = registry_with(ProviderId::Leap);
        let mut world = World::new();
        world.insert_resource(registry);
        world
            .run_system_once(suspend_mediapipe_hand_camera)
            .expect("suspend");
        assert_eq!(stops.load(Ordering::SeqCst), 0);
        assert!(world.get_resource::<SuspendedHandCamera>().is_none());
    }

    /// No registry at all (headless): both systems are clean no-ops.
    #[test]
    fn missing_registry_is_a_no_op() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.insert_resource(Time::<()>::default());
        world
            .run_system_once(suspend_mediapipe_hand_camera)
            .expect("suspend");
        world
            .run_system_once(resume_hand_camera_when_due)
            .expect("resume");
        assert!(world.get_resource::<SuspendedHandCamera>().is_none());
    }
}
