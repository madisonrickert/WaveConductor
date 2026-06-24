//! Run-condition helpers for sketch update systems.

use bevy::prelude::*;

use crate::lifecycle::state::{AppState, SketchActivity};

/// Returns a run-condition that is `true` when `AppState == target` AND
/// `SketchActivity == Active`. Use it to gate every sketch update system:
///
/// ```ignore
/// app.add_systems(
///     Update,
///     update_particles.run_if(sketch_active(AppState::Line)),
/// );
/// ```
///
/// This is the single line that delivers AGENTS.md's "zero systems when
/// idle" guarantee for sketches — when the user navigates away or the idle
/// timer trips, every sketch system skips automatically.
pub fn sketch_active(
    target: AppState,
) -> impl FnMut(Res<'_, State<AppState>>, Option<Res<'_, State<SketchActivity>>>) -> bool + Clone {
    move |app: Res<'_, State<AppState>>, activity: Option<Res<'_, State<SketchActivity>>>| {
        **app == target && activity.is_some_and(|a| **a == SketchActivity::Active)
    }
}

/// Returns a run-condition that is `true` when `AppState == target` AND
/// `SketchActivity == Idle` — the 30–60 s window after the user goes quiet but
/// before the screensaver shows. The sketch is still fully visible then.
///
/// The analogue of [`sketch_active`] for the `Idle` activity, parallel to
/// [`crate::lifecycle::screensaver::in_screensaver`] for `Screensaver`. Use it
/// to keep a *minimal* slice of a sketch animating through the Idle pre-roll —
/// e.g. a phase clock whose value would otherwise freeze and make the resting
/// field look stuck:
///
/// ```ignore
/// app.add_systems(
///     Update,
///     advance_phase_clock.run_if(
///         sketch_active(AppState::Cymatics).or_else(in_idle(AppState::Cymatics)),
///     ),
/// );
/// ```
///
/// This is a deliberate, narrow exception to "zero systems when idle": gate only
/// the bridge a frozen value would visibly break on it, not a sketch's full
/// update set.
pub fn in_idle(
    target: AppState,
) -> impl FnMut(Res<'_, State<AppState>>, Option<Res<'_, State<SketchActivity>>>) -> bool + Clone {
    move |app: Res<'_, State<AppState>>, activity: Option<Res<'_, State<SketchActivity>>>| {
        **app == target && activity.is_some_and(|a| **a == SketchActivity::Idle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::SystemState;
    use bevy::state::app::StatesPlugin;

    /// Build a minimal app pinned to `app_state` / `activity` so a run-condition
    /// can be exercised through real state resources. Mirrors the helper in
    /// `screensaver::run_condition`'s tests.
    fn app_in_state(app_state: AppState, activity: SketchActivity) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(StatesPlugin);
        app.init_state::<AppState>();
        app.add_sub_state::<SketchActivity>();
        app.insert_resource(State::new(app_state));
        app.insert_resource(State::new(activity));
        app
    }

    /// Evaluate a capturing run-condition closure against the app's world by
    /// fetching the two `Res` params Bevy would inject (the closure captures
    /// `target`, so `run_system_cached` cannot be used).
    #[expect(
        clippy::type_complexity,
        reason = "test helper mirrors the exact SystemState tuple Bevy would inject"
    )]
    fn eval(
        app: &mut App,
        mut cond: impl FnMut(Res<'_, State<AppState>>, Option<Res<'_, State<SketchActivity>>>) -> bool,
    ) -> bool {
        let mut state: SystemState<(
            Res<'_, State<AppState>>,
            Option<Res<'_, State<SketchActivity>>>,
        )> = SystemState::new(app.world_mut());
        let Ok((app_state, activity)) = state.get(app.world()) else {
            unreachable!("test SystemState params are always present");
        };
        cond(app_state, activity)
    }

    #[test]
    fn in_idle_true_only_in_target_and_idle() {
        let mut app = app_in_state(AppState::Cymatics, SketchActivity::Idle);
        assert!(eval(&mut app, in_idle(AppState::Cymatics)));
    }

    #[test]
    fn in_idle_false_when_active_or_screensaver() {
        let mut active = app_in_state(AppState::Cymatics, SketchActivity::Active);
        assert!(!eval(&mut active, in_idle(AppState::Cymatics)));
        let mut saver = app_in_state(AppState::Cymatics, SketchActivity::Screensaver);
        assert!(!eval(&mut saver, in_idle(AppState::Cymatics)));
    }

    #[test]
    fn in_idle_false_for_other_sketch() {
        let mut app = app_in_state(AppState::Line, SketchActivity::Idle);
        assert!(!eval(&mut app, in_idle(AppState::Cymatics)));
    }
}
