//! The [`in_screensaver`] run-condition (Plan 11.8, Seam 2).
//!
//! The attract-mode analogue of [`crate::sketch::sketch_active`], factored into
//! its own file (one concept per file) so each sketch gates its attract-driver
//! systems on "this sketch is up AND its screensaver is showing".

use bevy::prelude::*;

use crate::lifecycle::state::{AppState, SketchActivity};

/// Run-condition: `true` when `AppState == target` AND
/// `SketchActivity == Screensaver`. Each sketch gates its attract performer
/// systems on this so they run **only** while its own attract mode is showing
/// and nowhere else (AGENTS.md "zero systems when idle").
///
/// ```ignore
/// app.add_systems(
///     Update,
///     line_attract_step.run_if(in_screensaver(AppState::Line)),
/// );
/// ```
pub fn in_screensaver(
    target: AppState,
) -> impl FnMut(Res<'_, State<AppState>>, Option<Res<'_, State<SketchActivity>>>) -> bool + Clone {
    move |app: Res<'_, State<AppState>>, activity: Option<Res<'_, State<SketchActivity>>>| {
        **app == target && activity.is_some_and(|a| **a == SketchActivity::Screensaver)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::state::app::StatesPlugin;

    /// Build a minimal app pinned to `app_state` / `activity` so the run-condition
    /// can be exercised through real state resources.
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

    /// Evaluate the run-condition against the app's current world.
    ///
    /// `in_screensaver` returns a capturing `FnMut` (it closes over `target`),
    /// which Bevy's `run_system_cached` rejects ("Non-ZST systems cannot be
    /// cached"). We instead fetch the two `Res` params via a `SystemState` and
    /// call the closure directly — the same params Bevy would inject.
    #[expect(
        clippy::type_complexity,
        reason = "test helper mirrors the exact SystemState tuple Bevy would inject"
    )]
    fn eval(app: &mut App, target: AppState) -> bool {
        use bevy::ecs::system::SystemState;
        let mut cond = in_screensaver(target);
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
    fn true_only_in_target_and_screensaver() {
        let mut app = app_in_state(AppState::Line, SketchActivity::Screensaver);
        assert!(eval(&mut app, AppState::Line));
    }

    #[test]
    fn false_when_active() {
        let mut app = app_in_state(AppState::Line, SketchActivity::Active);
        assert!(!eval(&mut app, AppState::Line));
    }

    #[test]
    fn false_for_other_sketch() {
        let mut app = app_in_state(AppState::Flame, SketchActivity::Screensaver);
        assert!(!eval(&mut app, AppState::Line));
    }
}
