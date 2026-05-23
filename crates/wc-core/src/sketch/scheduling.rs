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
