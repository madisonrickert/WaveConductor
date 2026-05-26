//! Interaction tracking and idle / screensaver state transitions.
//!
//! The `InteractionTimer` resource records the time of the last detected
//! interaction. Two systems drive its evolution:
//!
//! - [`reset_on_interaction`] resets the timer whenever any input event
//!   (mouse, keyboard, touch) is observed.
//! - [`advance_activity`] reads the timer each frame and transitions
//!   [`crate::lifecycle::state::SketchActivity`] through
//!   `Active → Idle → Screensaver` as the elapsed time crosses thresholds,
//!   unless a sketch-registered [`IdleVetoFn`] (via [`IdleVetoes`]) overrides
//!   the decision to keep the sketch `Active`.

use std::time::Duration;

use bevy::prelude::*;

use super::state::SketchActivity;

/// Tracks when the user last interacted with the app, plus the thresholds at
/// which the lifecycle plugin transitions the sketch activity state.
#[derive(Resource, Debug, Clone)]
pub struct InteractionTimer {
    /// Time of last detected interaction, in `Res<Time>::elapsed()` units.
    last_interaction: Duration,
    /// After this much idle time, transition `Active → Idle`.
    pub idle_threshold: Duration,
    /// Additional idle time (beyond [`Self::idle_threshold`]) before the screensaver
    /// overlay is shown. With the defaults of 30 s each, the screensaver appears
    /// after 60 s of total inactivity.
    pub screensaver_threshold: Duration,
}

impl Default for InteractionTimer {
    fn default() -> Self {
        Self {
            last_interaction: Duration::ZERO,
            // Both default to 30 s per v4 BaseSketch.ts.
            idle_threshold: Duration::from_secs(30),
            screensaver_threshold: Duration::from_secs(30),
        }
    }
}

impl InteractionTimer {
    /// Record that an interaction just happened.
    pub fn mark(&mut self, now: Duration) {
        self.last_interaction = now;
    }

    /// Seconds elapsed since the last interaction.
    #[must_use]
    pub fn idle_for(&self, now: Duration) -> Duration {
        now.saturating_sub(self.last_interaction)
    }
}

/// Function pointer type for idle vetoes. Receives a read-only `World` reference;
/// returning `true` keeps the sketch in `SketchActivity::Active` regardless of
/// elapsed idle time.
///
/// Must be a plain `fn`, not a closure — captures are unsupported so that
/// `IdleVetoes` remains a cheap value resource without `Arc<dyn Fn>` overhead.
/// Read sketch state inside the function via `world.get_resource::<...>()`.
pub type IdleVetoFn = fn(&World) -> bool;

/// List of registered veto callbacks. [`advance_activity`] consults this list
/// before transitioning out of `Active`; any veto returning `true` overrides
/// the timeout-based decision.
///
/// Sketches register their veto in `Plugin::build` via
/// [`RegisterIdleVetoExt::register_idle_veto`].
#[derive(Resource, Default)]
pub struct IdleVetoes {
    /// Registered veto callbacks.
    vetoes: Vec<IdleVetoFn>,
}

impl IdleVetoes {
    /// Iterate registered vetoes. Internal helper for `any_veto_active`.
    fn iter(&self) -> impl Iterator<Item = &IdleVetoFn> {
        self.vetoes.iter()
    }
}

/// Returns `true` if any registered veto fires for the current world state.
fn any_veto_active(world: &World) -> bool {
    let Some(vetoes) = world.get_resource::<IdleVetoes>() else {
        return false;
    };
    vetoes.iter().any(|f| f(world))
}

/// Extension trait that adds `register_idle_veto` to Bevy's [`App`].
pub trait RegisterIdleVetoExt {
    /// Register a closure that returns `true` while the sketch should stay
    /// `Active` regardless of the idle timer.
    ///
    /// Registrations accumulate; multiple sketches can each contribute a veto.
    /// Vetoes are not auto-removed when a sketch exits — they read `World`
    /// and gracefully return `false` if their resources are absent.
    fn register_idle_veto(&mut self, veto: IdleVetoFn) -> &mut Self;
}

impl RegisterIdleVetoExt for App {
    fn register_idle_veto(&mut self, veto: IdleVetoFn) -> &mut Self {
        let mut vetoes = self
            .world_mut()
            .get_resource_or_insert_with(IdleVetoes::default);
        vetoes.vetoes.push(veto);
        self
    }
}

/// Resets [`InteractionTimer`] whenever any input event is observed.
///
/// Reads mouse, keyboard, touch, and hand-tracking message streams. A hand
/// entering the tracking volume (any [`crate::input::state::HandTrackingFrame`]
/// arriving) counts as user interaction.
///
/// Note: Bevy 0.18 renamed `EventReader`/`EventWriter` to `MessageReader`/`MessageWriter`.
/// The readers must consume (`.read()`) messages rather than merely peeking with
/// `.is_empty()`, because an unconsumed reader cursor never advances and would
/// incorrectly report messages present on every subsequent frame.
pub fn reset_on_interaction(
    time: Res<'_, Time>,
    mut timer: ResMut<'_, InteractionTimer>,
    mut mouse_motion: MessageReader<'_, '_, bevy::input::mouse::MouseMotion>,
    mut mouse_buttons: MessageReader<'_, '_, bevy::input::mouse::MouseButtonInput>,
    mut keyboard: MessageReader<'_, '_, bevy::input::keyboard::KeyboardInput>,
    mut touch: MessageReader<'_, '_, bevy::input::touch::TouchInput>,
    mut hand_tracking: MessageReader<'_, '_, crate::input::state::HandTrackingFrame>,
) {
    let any_event = mouse_motion.read().count() > 0
        || mouse_buttons.read().count() > 0
        || keyboard.read().count() > 0
        || touch.read().count() > 0
        || hand_tracking.read().count() > 0;
    if any_event {
        timer.mark(time.elapsed());
    }
}

/// Reads [`InteractionTimer`] and transitions [`SketchActivity`] when the
/// configured thresholds are crossed, unless a registered [`IdleVetoFn`]
/// in [`IdleVetoes`] keeps the sketch `Active`.
///
/// Runs as an exclusive system (`world: &mut World`) so the veto callbacks can
/// read arbitrary world state — Bevy 0.18 does not accept `&World` as a regular
/// system parameter alongside `Res<...>`. Skips cleanly when `SketchActivity`
/// doesn't exist (i.e. when `AppState` is `Home`): the sub-state resource is
/// absent outside sketch states.
pub fn advance_activity(world: &mut World) {
    let now = world.resource::<Time>().elapsed();
    let timer = world.resource::<InteractionTimer>().clone();
    let idle = timer.idle_for(now);
    let timeout_target = if idle >= timer.screensaver_threshold + timer.idle_threshold {
        SketchActivity::Screensaver
    } else if idle >= timer.idle_threshold {
        SketchActivity::Idle
    } else {
        SketchActivity::Active
    };
    let target = if timeout_target != SketchActivity::Active && any_veto_active(world) {
        SketchActivity::Active
    } else {
        timeout_target
    };
    let Some(current) = world.get_resource::<State<SketchActivity>>() else {
        return; // Not in a sketch state; nothing to do.
    };
    if *current.get() == target {
        return;
    }
    if let Some(mut next) = world.get_resource_mut::<NextState<SketchActivity>>() {
        next.set(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_for_handles_clock_resets() {
        let mut timer = InteractionTimer::default();
        timer.mark(Duration::from_secs(10));
        // Querying with an earlier "now" should saturate to zero, not panic.
        assert_eq!(timer.idle_for(Duration::from_secs(5)), Duration::ZERO);
    }

    #[test]
    fn idle_for_reports_elapsed() {
        let mut timer = InteractionTimer::default();
        timer.mark(Duration::from_secs(10));
        assert_eq!(
            timer.idle_for(Duration::from_secs(45)),
            Duration::from_secs(35)
        );
    }

    #[test]
    fn defaults_match_v4_thirty_second_idle() {
        let timer = InteractionTimer::default();
        assert_eq!(timer.idle_threshold, Duration::from_secs(30));
        assert_eq!(timer.screensaver_threshold, Duration::from_secs(30));
    }
}
