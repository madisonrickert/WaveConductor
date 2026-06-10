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

    /// Raw timestamp of the last detected interaction.
    ///
    /// Primarily useful in tests that need to assert the timer was *not* reset
    /// (e.g. verifying that an empty Leap tracking frame is ignored).
    #[must_use]
    pub fn last_interaction(&self) -> Duration {
        self.last_interaction
    }

    /// Rewind the timer as if both idle thresholds had already elapsed, so
    /// [`advance_activity`] targets `Screensaver` on its next run. The
    /// `Shift+S` skip ([`skip_to_screensaver`]) calls this every armed frame.
    pub fn rewind_past_screensaver(&mut self, now: Duration) {
        self.last_interaction =
            now.saturating_sub(self.idle_threshold + self.screensaver_threshold);
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
/// Reads mouse, keyboard, touch, and hand-tracking message streams. A
/// hand-*bearing* frame (at least one hand in the tracking volume) counts as
/// user interaction; empty tracking frames emitted by a running-but-unoccupied
/// Leap device do not — otherwise the idle timer never reaches `Screensaver`
/// while a Leap is connected.
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
        // A *hand* in the tracking volume is interaction; the empty tracking
        // frames a running-but-unoccupied Leap streams continuously are not —
        // otherwise the idle timer never reaches Screensaver while a Leap is
        // connected. `.filter().count()` (not `.any()`) so the reader cursor
        // fully drains (see the note above about peeking).
        || hand_tracking
            .read()
            .filter(|frame| !frame.hands.is_empty())
            .count()
            > 0;
    if any_event {
        timer.mark(time.elapsed());
    }
}

/// `Shift+S` screensaver skip: while the chord is in flight, rewind the
/// [`InteractionTimer`] past both thresholds so [`advance_activity`] enters
/// `Screensaver` immediately instead of waiting out the ~60 s idle timer.
///
/// ## Why it stays armed until the keyboard is quiet
///
/// [`reset_on_interaction`] treats *every* keyboard event as interaction —
/// including the key-up events of this very chord (`S` up, then `Shift` up,
/// possibly frames apart). A one-shot rewind on `just_pressed` would be
/// cancelled by those releases and the screensaver would flash and wake.
/// Instead the skip **arms** on `just_pressed` and keeps re-rewinding (after
/// `reset_on_interaction`, before `advance_activity` — see the lifecycle
/// plugin's chain) every frame the keyboard is still active (any key held or
/// released this frame), disarming only once the keyboard has gone quiet.
/// From then on any interaction — mouse, touch, hand, the next keypress —
/// wakes the sketch exactly as after a natural timeout.
///
/// ## Egui keyboard capture
///
/// Unlike the other hotkey consumers this system is NOT gated on the
/// `egui_not_capturing_keyboard` run condition — a `run_if` would freeze the
/// `armed` `Local` for as long as a text field holds focus, and the stale
/// arm would rewind the timer (popping the screensaver mid-typing) on the
/// first uncaptured keyboard frame, minutes after the original chord.
/// Instead the system always runs and treats "egui owns the keyboard" as a
/// quiet keyboard: the chord can't arm, and an in-flight arm disarms
/// immediately (the skip simply stops fighting `reset_on_interaction`; if a
/// release event then wakes the screensaver, the operator is at the panel
/// anyway).
///
/// Outside a sketch (Home), `advance_activity` has no `SketchActivity` state
/// to drive this frame, so the skip shows nothing there (the timer rewind
/// itself is harmlessly overwritten by the next interaction).
pub fn skip_to_screensaver(
    time: Res<'_, Time>,
    actions: Res<
        '_,
        leafwing_input_manager::prelude::ActionState<super::actions::WaveConductorAction>,
    >,
    keys: Res<'_, ButtonInput<KeyCode>>,
    captured: Option<Res<'_, crate::settings::input_capture::EguiKeyboardCaptured>>,
    mut armed: Local<'_, bool>,
    mut timer: ResMut<'_, InteractionTimer>,
) {
    // Fail-open like egui_not_capturing_keyboard: absent resource (harnesses
    // without SettingsPlugin/EguiPlugin) = not capturing.
    let capturing = captured.is_some_and(|c| c.0);
    let just_pressed =
        !capturing && actions.just_pressed(&super::actions::WaveConductorAction::StartScreensaver);
    // "Keyboard active" = any key still held, or released this frame: both
    // produce events reset_on_interaction has already counted this frame.
    // While egui captures the keyboard this reads as quiet, so an armed skip
    // disarms instead of holding the screensaver against the operator's
    // typing (see the module docs above).
    let keyboard_active = !capturing
        && (keys.get_pressed().next().is_some() || keys.get_just_released().next().is_some());
    let (next_armed, rewind) = skip_step(*armed, just_pressed, keyboard_active);
    if rewind {
        timer.rewind_past_screensaver(time.elapsed());
    }
    if *armed && !next_armed {
        tracing::info!("screensaver: skip hotkey released; normal wake behavior resumes");
    }
    *armed = next_armed;
}

/// Pure per-frame decision for [`skip_to_screensaver`]: `(new_armed, rewind)`.
///
/// Arms on the chord press; stays armed (and keeps rewinding, swallowing the
/// chord's own key-up interactions) while the keyboard is active; disarms on
/// the first quiet frame without a rewind (nothing marked the timer that
/// frame, so the previous rewind stands).
fn skip_step(armed: bool, just_pressed: bool, keyboard_active: bool) -> (bool, bool) {
    let next = just_pressed || (armed && keyboard_active);
    (next, next)
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

    #[test]
    fn rewind_past_screensaver_crosses_both_thresholds() {
        let mut timer = InteractionTimer::default();
        let now = Duration::from_secs(100);
        timer.mark(now); // freshly interacted…
        timer.rewind_past_screensaver(now); // …then skipped
        assert!(
            timer.idle_for(now) >= timer.idle_threshold + timer.screensaver_threshold,
            "rewind must put the timer past Idle + Screensaver"
        );
        // Early in a session (now < thresholds) it saturates instead of
        // underflowing, still reading as maximally idle.
        let mut early = InteractionTimer::default();
        early.mark(Duration::from_secs(5));
        early.rewind_past_screensaver(Duration::from_secs(10));
        assert_eq!(early.last_interaction(), Duration::ZERO);
    }

    #[test]
    fn skip_step_arms_holds_through_chord_release_and_disarms_when_quiet() {
        // Frame 1: chord just pressed (keys held) → arm + rewind.
        assert_eq!(skip_step(false, true, true), (true, true));
        // Frames while Shift is still held after S released → keep rewinding,
        // swallowing the key-up marks reset_on_interaction just made.
        assert_eq!(skip_step(true, false, true), (true, true));
        // First quiet frame (no keys held, none released) → disarm, no rewind
        // needed (nothing marked the timer this frame).
        assert_eq!(skip_step(true, false, false), (false, false));
        // Disarmed + quiet → inert.
        assert_eq!(skip_step(false, false, false), (false, false));
        // Disarmed but other keys active (normal typing) → must NOT rewind.
        assert_eq!(skip_step(false, false, true), (false, false));
    }
}
