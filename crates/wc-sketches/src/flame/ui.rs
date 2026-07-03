//! Name-input overlay, debounced carousel admission, and the screensaver
//! ghost seed label.
//!
//! Three independent pieces of glue:
//!
//! 1. [`flame_name_input_overlay`] draws a centered-bottom `egui::TextEdit`
//!    bound to `FlameSettings::name` while the sketch is `Active` (hidden in
//!    `Idle`/`Screensaver`, where the fractal keeps morphing but nobody is
//!    typing at it). Every keystroke marks the setting changed, so the
//!    existing name-change watcher (`systems::name_change`) rebuilds the
//!    fractal live, per letter — the v4 joy of watching the shape reform as
//!    a name lands.
//! 2. [`debounce_name_admission`] watches that same setting from the *other*
//!    side: rather than admitting every keystroke into the persisted
//!    carousel list (which would fill it with half-typed garbage), it waits
//!    for [`NAME_SETTLE_SECS`] of no further edits before calling
//!    [`admit_name`]. The carousel (driven by [`super::screensaver`]) only
//!    ever cycles through names someone actually finished typing.
//! 3. [`flame_seed_ghost_label`] draws the same centered-bottom anchor while
//!    the sketch is `Screensaver`, naming the seed currently on screen in a
//!    muted color — the fractal is always attributed, even in attract mode.
//!
//! [`admit_name`] itself is the pure debounced-admission core: reject too
//! short / the default placeholder, case-insensitive dedupe (moving the
//! existing entry to the front rather than duplicating), otherwise insert at
//! the front and cap the list length.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use super::branches::{normalize_name, DEFAULT_NAME};
use super::settings::FlameSettings;
use wc_core::lifecycle::state::{AppState, SketchActivity};

/// Cap on the persisted carousel list. Oldest entries are evicted once a new
/// name is admitted past this length.
pub const MAX_CAROUSEL_NAMES: usize = 16;

/// Seconds of no further name edits before the pending name is admitted into
/// the carousel list. Long enough that a still-typing visitor's half-formed
/// prefixes never land; short enough that walking away for a moment is
/// enough to be remembered.
pub const NAME_SETTLE_SECS: f32 = 4.0;

/// Debounce state for [`debounce_name_admission`]. `pending` tracks the last
/// observed `FlameSettings::name`; `settled_at` (in `Time::elapsed_secs`) is
/// the timestamp at which `pending` becomes eligible for admission, reset
/// every time the name changes again.
#[derive(Resource, Default)]
pub struct FlameNameDebounce {
    /// The most recently observed `FlameSettings::name` value.
    pub pending: String,
    /// Virtual-time timestamp (seconds) at which `pending` settles and may
    /// be admitted, or `None` if it has already been admitted (or is still
    /// empty / unset).
    pub settled_at: Option<f32>,
}

/// The debounced-admission core: try to insert `candidate` (trimmed) at the
/// front of `list`.
///
/// Rejects (returns `false`, `list` unchanged except possible reordering):
/// - fewer than 2 trimmed characters (matches v4: single-character names
///   never produced a meaningful fractal),
/// - exactly [`DEFAULT_NAME`] (the placeholder, not a real visitor name).
///
/// Case-insensitive dedupe: if an entry equal to `candidate` (ignoring case)
/// already exists, it is moved to the front (original casing preserved) and
/// this returns `false` — no duplicate is inserted.
///
/// Otherwise, `candidate` (trimmed) is inserted at the front and the list is
/// truncated to [`MAX_CAROUSEL_NAMES`], evicting the oldest entry if it was
/// already full. Returns `true` on this path.
pub(crate) fn admit_name(list: &mut Vec<String>, candidate: &str) -> bool {
    let trimmed = candidate.trim();
    if trimmed.chars().count() < 2 || trimmed == DEFAULT_NAME {
        return false;
    }
    let trimmed_lower = trimmed.to_lowercase();
    if let Some(index) = list.iter().position(|n| n.to_lowercase() == trimmed_lower) {
        if index != 0 {
            let existing = list.remove(index);
            list.insert(0, existing);
        }
        return false;
    }
    list.insert(0, trimmed.to_string());
    list.truncate(MAX_CAROUSEL_NAMES);
    true
}

/// `Update`, gated `sketch_active(Flame)`: watches `FlameSettings::name` and
/// admits it into `carousel_names` once it has settled (no edits for
/// [`NAME_SETTLE_SECS`]).
///
/// Runs every frame but the common case (name unchanged, already admitted)
/// is two cheap comparisons and an early return — no allocation. The `clone`
/// only fires on an actual keystroke-driven change, mirroring the rest of
/// the codebase's "event-driven allocation is fine, per-frame is not" rule.
pub fn debounce_name_admission(
    time: Res<'_, Time>,
    mut settings: ResMut<'_, FlameSettings>,
    mut debounce: ResMut<'_, FlameNameDebounce>,
) {
    let now = time.elapsed_secs();
    if settings.name != debounce.pending {
        debounce.pending.clone_from(&settings.name);
        debounce.settled_at = Some(now + NAME_SETTLE_SECS);
        return;
    }
    let Some(settled_at) = debounce.settled_at else {
        return;
    };
    if now >= settled_at {
        admit_name(&mut settings.carousel_names, &debounce.pending);
        debounce.settled_at = None;
    }
}

/// `bevy_egui::EguiPrimaryContextPass`: draws the centered-bottom name-input
/// box while the Flame sketch is `Active`.
///
/// Self-gated (not a `run_if`) on `AppState::Flame` AND
/// `SketchActivity::Active` so the box disappears during `Idle` and the
/// screensaver — the fractal keeps drifting there, but nobody should be
/// typing into it.
///
/// The `TextEdit` is bound directly to `settings.name` via
/// `bypass_change_detection` (egui needs a plain `&mut String`, not change
/// tracking on every render pass); `response.changed()` then explicitly
/// marks the setting changed so autosave and the name-change watcher fire
/// exactly on real edits, not every frame the box is drawn.
pub fn flame_name_input_overlay(
    app_state: Res<'_, State<AppState>>,
    activity: Option<Res<'_, State<SketchActivity>>>,
    mut settings: ResMut<'_, FlameSettings>,
    mut contexts: EguiContexts<'_, '_>,
) {
    if **app_state != AppState::Flame {
        return;
    }
    if !activity.is_some_and(|a| *a.get() == SketchActivity::Active) {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    egui::Area::new(egui::Id::new("wc-flame-name-input"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -64.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::default()
                .fill(egui::Color32::from_black_alpha(180))
                .inner_margin(egui::Margin::symmetric(16, 10))
                .corner_radius(8_u8)
                .show(ui, |ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut settings.bypass_change_detection().name)
                            .char_limit(20)
                            .hint_text("who are you?")
                            .desired_width(220.0),
                    );
                    if response.changed() {
                        settings.set_changed();
                    }
                });
        });
}

/// `bevy_egui::EguiPrimaryContextPass`: draws a dim centered-bottom ghost
/// label naming the current seed while the Flame screensaver is showing.
///
/// Self-gated (not a `run_if`) on `AppState::Flame` AND
/// `SketchActivity::Screensaver`, the same pattern as
/// [`flame_name_input_overlay`] — the input box itself stays hidden there (it
/// gates on `Active`), so the fractal is never left unattributed: the ghost
/// label fills the same anchor with the seed name the carousel is currently
/// showing, in a muted color that reads as "attract mode, not editable".
pub fn flame_seed_ghost_label(
    app_state: Res<'_, State<AppState>>,
    activity: Option<Res<'_, State<SketchActivity>>>,
    settings: Res<'_, FlameSettings>,
    mut contexts: EguiContexts<'_, '_>,
) {
    if **app_state != AppState::Flame {
        return;
    }
    if !activity.is_some_and(|a| *a.get() == SketchActivity::Screensaver) {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let seed = normalize_name(&settings.name);

    egui::Area::new(egui::Id::new("wc-flame-seed-ghost"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -64.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.label(egui::RichText::new(seed).color(egui::Color32::from_gray(140)));
        });
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use std::time::Duration;

    #[test]
    fn admit_rejects_short_and_default() {
        let mut list = vec![];
        assert!(!admit_name(&mut list, "a"));
        assert!(!admit_name(&mut list, "  x "));
        assert!(!admit_name(&mut list, DEFAULT_NAME));
        assert!(list.is_empty());
    }

    #[test]
    fn admit_inserts_at_front_and_truncates() {
        let mut list: Vec<String> = (0..16).map(|i| format!("name{i}")).collect();
        assert!(admit_name(&mut list, "madison"));
        assert_eq!(list.len(), MAX_CAROUSEL_NAMES);
        assert_eq!(list[0], "madison");
        assert!(!list.iter().any(|n| n == "name15"), "oldest evicted");
    }

    #[test]
    fn admit_dedupes_case_insensitively_move_to_front() {
        let mut list = vec!["Xiaohan".to_string(), "madison".to_string()];
        assert!(!admit_name(&mut list, "MADISON"));
        assert_eq!(list.len(), 2);
        assert_eq!(
            list[0], "madison",
            "existing entry moved to front, original casing kept"
        );
    }

    #[test]
    fn admit_trims_whitespace() {
        let mut list = vec![];
        assert!(admit_name(&mut list, "  ember  "));
        assert_eq!(list[0], "ember");
    }

    /// A name typed and then left alone past `NAME_SETTLE_SECS` is admitted;
    /// a subsequent run with the name unchanged admits nothing new.
    #[test]
    fn debounce_admits_after_settle_and_is_idempotent() {
        let mut world = World::new();
        world.insert_resource(FlameSettings {
            name: "madison".to_string(),
            ..Default::default()
        });
        world.insert_resource(FlameNameDebounce::default());
        world.insert_resource(Time::<()>::default());

        // First run: the name differs from `pending` ("") so this just
        // records it and schedules the settle time. Not admitted yet.
        world
            .run_system_once(debounce_name_admission)
            .expect("first run");
        assert!(
            world.resource::<FlameSettings>().carousel_names.is_empty(),
            "not admitted before settling"
        );

        // Advance virtual time past the settle window and run again: now
        // `pending == name` and the deadline has passed, so it admits.
        world
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f32(NAME_SETTLE_SECS + 0.1));
        world
            .run_system_once(debounce_name_admission)
            .expect("second run");
        assert_eq!(
            world.resource::<FlameSettings>().carousel_names,
            vec!["madison".to_string()]
        );

        // Third run with the name unchanged: no-op, no duplicate.
        world
            .run_system_once(debounce_name_admission)
            .expect("third run");
        assert_eq!(
            world.resource::<FlameSettings>().carousel_names,
            vec!["madison".to_string()]
        );
    }
}
