//! Name-input overlay, debounced carousel admission, and the screensaver
//! ghost seed label.
//!
//! Three independent pieces of glue:
//!
//! 1. [`flame_name_input_overlay`] draws a top-centered frosted `egui::TextEdit`
//!    bound to `FlameSettings::name` while the sketch is `Active` (hidden in
//!    `Idle`/`Screensaver`, where the fractal keeps morphing but nobody is
//!    typing at it, and hidden entirely by the `show_name_overlay` toggle).
//!    Every keystroke marks the setting changed, so the existing name-change
//!    watcher (`systems::name_change`) rebuilds the fractal live, per letter —
//!    the v4 joy of watching the shape reform as a name lands.
//! 2. [`debounce_name_admission`] watches that same setting from the *other*
//!    side: rather than admitting every keystroke into the persisted
//!    carousel list (which would fill it with half-typed garbage), it waits
//!    for [`NAME_SETTLE_SECS`] of no further edits before calling
//!    `admit_name`. The carousel (driven by [`super::screensaver`]) only
//!    ever cycles through names someone actually finished typing.
//! 3. [`flame_seed_ghost_label`] draws the same top-center anchor while the
//!    sketch is `Screensaver`, naming the seed currently on screen in a muted,
//!    drop-shadowed label that cross-fades as the carousel advances — the
//!    fractal is always attributed, even in attract mode (unless the
//!    `show_name_overlay` toggle hides it).
//!
//! `admit_name` itself is the pure debounced-admission core: reject too
//! short / the default placeholder, case-insensitive dedupe (moving the
//! existing entry to the front rather than duplicating), otherwise insert at
//! the front and cap the list length.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use super::branches::{normalize_name, DEFAULT_NAME};
use super::settings::FlameSettings;
use wc_core::lifecycle::state::{AppState, SketchActivity};
use wc_core::ui::auto_fade::UiOpacity;
use wc_core::ui::{backdrop_blur_frame, FrameOptions, OverlayStyle, PointerCoarse};

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
#[allow(
    clippy::too_many_arguments,
    reason = "a Bevy system's parameters are its data dependencies; the frosted \
              button-family styling pulls in OverlayStyle/UiOpacity/PointerCoarse \
              /Window alongside the state gates and the egui context"
)]
pub fn flame_name_input_overlay(
    app_state: Res<'_, State<AppState>>,
    activity: Option<Res<'_, State<SketchActivity>>>,
    mut settings: ResMut<'_, FlameSettings>,
    style: Res<'_, OverlayStyle>,
    opacity: Res<'_, UiOpacity>,
    coarse: Res<'_, PointerCoarse>,
    window: Single<'_, '_, &Window>,
    mut contexts: EguiContexts<'_, '_>,
) {
    if **app_state != AppState::Flame {
        return;
    }
    if !activity.is_some_and(|a| *a.get() == SketchActivity::Active) {
        return;
    }
    // Hidden by the "Show name overlay" toggle: name entry is settings-only.
    if !settings.show_name_overlay {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    // Match the overlay buttons (Madison's v4 ask): same height (32 fine / 44
    // coarse), frosted-glass backdrop instead of an opaque panel, top-centered
    // between the corner buttons — v4's `.flame-input` was centered and pinned
    // near the top. Width tracks the window (v4's `33% / min 240px`), clamped so
    // it stays wide on small windows and doesn't run into the buttons on large.
    let height = if coarse.0 {
        style.button_size_coarse
    } else {
        style.button_size_fine
    };
    let field_width = (window.width() * 0.32).clamp(280.0, 480.0);

    egui::Area::new(egui::Id::new("wc-flame-name-input"))
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 12.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            // Pin the box to exactly (field_width × button height) so the frosted
            // frame allocates that size and the text sits centered inside it.
            ui.set_width(field_width);
            ui.set_height(height);
            let options = FrameOptions {
                corner_radius: style.button_corner_radius,
                padding: egui::vec2(14.0, 4.0),
                opacity_mul: opacity.current,
            };
            backdrop_blur_frame(ui, &style, options, |ui| {
                ui.centered_and_justified(|ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut settings.bypass_change_detection().name)
                            .char_limit(20)
                            .hint_text("who are you?")
                            .font(egui::FontId::proportional((height * 0.44).max(15.0)))
                            .horizontal_align(egui::Align::Center)
                            // No egui frame: only the frosted-glass backdrop shows.
                            .frame(egui::Frame::NONE),
                    );
                    if response.changed() {
                        settings.set_changed();
                    }
                });
            });
        });
}

/// Seconds for one carousel name to cross-fade to the next in the screensaver
/// ghost label. Snappier than the flame's own morph so the text settles while
/// the fractal keeps flowing into the new shape.
const GHOST_FADE_SECS: f32 = 1.2;

/// Ghost-label font size in points. Larger than the overlay buttons: during the
/// screensaver this attribution IS the on-screen title.
const GHOST_FONT_PX: f32 = 26.0;

/// Drop-shadow offset (points, down-right) behind the ghost label, so the muted
/// text stays legible over the bright flame without a harsh outline.
const GHOST_SHADOW_PX: f32 = 1.5;

/// `bevy_egui::EguiPrimaryContextPass`: draws a dim centered-top ghost label
/// naming the current seed while the Flame screensaver is showing, cross-fading
/// as the carousel advances.
///
/// Self-gated (not a `run_if`) on `AppState::Flame` AND
/// `SketchActivity::Screensaver`, the same pattern as
/// [`flame_name_input_overlay`] — the input box itself stays hidden there (it
/// gates on `Active`), so the fractal is never left unattributed. It sits at the
/// same top-center anchor as the input box (Madison: "where the non-screensaver
/// name entry goes"), never wraps, and animates each name change: the outgoing
/// name fades out over the first half of `GHOST_FADE_SECS`, the incoming name
/// fades in over the second, tracking the flame morph underneath.
///
/// `shown` / `prev` / `progress` are per-system [`Local`] state: `shown` is the
/// name currently settling in, `prev` the one fading out, `progress` the 0→1
/// cross-fade clock. Allocating into the `Local` strings only happens on an
/// actual name change (event-driven, not per frame).
#[allow(
    clippy::too_many_arguments,
    reason = "a Bevy system's parameters are its data dependencies; the cross-fade \
              adds the time source and three Local cross-fade fields"
)]
pub fn flame_seed_ghost_label(
    app_state: Res<'_, State<AppState>>,
    activity: Option<Res<'_, State<SketchActivity>>>,
    settings: Res<'_, FlameSettings>,
    time: Res<'_, Time>,
    mut shown: Local<'_, String>,
    mut prev: Local<'_, String>,
    mut progress: Local<'_, f32>,
    mut contexts: EguiContexts<'_, '_>,
) {
    if **app_state != AppState::Flame {
        return;
    }
    if !activity.is_some_and(|a| *a.get() == SketchActivity::Screensaver) {
        return;
    }
    // Advance the cross-fade state every screensaver frame — even while the
    // overlay is toggled off below — so a later toggle-on never resurfaces a
    // stale carousel name: the state must track the carousel continuously, not
    // freeze behind the visibility guard.
    let current = normalize_name(&settings.name);
    step_ghost_crossfade(
        &mut prev,
        &mut shown,
        &mut progress,
        current,
        time.delta_secs(),
    );

    // Hidden by the "Show name overlay" toggle: the name lives in settings only.
    // (State was advanced above, so re-showing picks up the live carousel name.)
    if !settings.show_name_overlay {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    // Old name fades out over the first half, new name fades in over the second.
    let (text, alpha) = ghost_crossfade(*progress, &prev, &shown);
    let color = egui::Color32::from_gray(150).gamma_multiply(alpha);
    let shadow = egui::Color32::from_black_alpha(160).gamma_multiply(alpha);
    let font = egui::FontId::proportional(GHOST_FONT_PX);

    // Soft drop shadow first (behind, offset down-right), then the label on top:
    // both are `Order::Foreground`, so the later draw wins the z-order.
    for (id, offset, col) in [
        (
            "wc-flame-seed-ghost-shadow",
            egui::vec2(GHOST_SHADOW_PX, 12.0 + GHOST_SHADOW_PX),
            shadow,
        ),
        ("wc-flame-seed-ghost", egui::vec2(0.0, 12.0), color),
    ] {
        egui::Area::new(egui::Id::new(id))
            .anchor(egui::Align2::CENTER_TOP, offset)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(text).color(col).font(font.clone()))
                        .wrap_mode(egui::TextWrapMode::Extend)
                        .selectable(false),
                );
            });
    }
}

/// Advance the ghost-label cross-fade [`Local`] state for the current carousel
/// name and tick the fade clock by `dt`.
///
/// On a name change the outgoing `shown` becomes `prev` and the cross-fade
/// restarts. A *first* appearance (no outgoing name — `prev` ends up empty)
/// starts the clock at the half-way point so the incoming seed fades straight
/// in, rather than spending the first `GHOST_FADE_SECS / 2` rendering a blank
/// `prev` label (the "unattributed for ~0.6 s on the first bloom" bug).
fn step_ghost_crossfade(
    prev: &mut String,
    shown: &mut String,
    progress: &mut f32,
    current: &str,
    dt: f32,
) {
    if shown.as_str() != current {
        *prev = std::mem::take(shown);
        shown.push_str(current);
        *progress = if prev.is_empty() { 0.5 } else { 0.0 };
    }
    *progress = (*progress + dt / GHOST_FADE_SECS).min(1.0);
}

/// Pick the `(text, alpha)` the ghost label draws for a given cross-fade
/// `progress`: the outgoing name fades out over the first half, the incoming
/// name fades in over the second.
fn ghost_crossfade<'a>(progress: f32, prev: &'a str, shown: &'a str) -> (&'a str, f32) {
    if progress < 0.5 {
        (prev, 1.0 - progress * 2.0)
    } else {
        (shown, (progress - 0.5) * 2.0)
    }
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

    /// First appearance (no outgoing name) must fade the seed straight in, never
    /// render the blank `prev` half — the "unattributed for ~0.6 s" regression.
    #[test]
    fn ghost_first_appearance_fades_in_the_seed_not_blank() {
        let (mut prev, mut shown, mut progress) = (String::new(), String::new(), 0.0_f32);
        step_ghost_crossfade(&mut prev, &mut shown, &mut progress, "madison", 1.0 / 60.0);
        assert!(
            progress >= 0.5,
            "first appearance skips the fade-out half; got progress {progress}"
        );
        let (text, alpha) = ghost_crossfade(progress, &prev, &shown);
        assert_eq!(text, "madison", "the seed is shown, not a blank prev label");
        assert!((0.0..=1.0).contains(&alpha));
    }

    /// A genuine name change restarts the cross-fade: the old name fades out over
    /// the first half before the new name fades in.
    #[test]
    fn ghost_name_change_restarts_crossfade_from_prev() {
        let (mut prev, mut shown, mut progress) = (String::new(), "madison".to_string(), 1.0_f32);
        step_ghost_crossfade(&mut prev, &mut shown, &mut progress, "ada", 1.0 / 60.0);
        assert_eq!(prev, "madison");
        assert_eq!(shown, "ada");
        assert!(
            progress < 0.5,
            "a real change restarts the fade-out half; got progress {progress}"
        );
        assert_eq!(
            ghost_crossfade(progress, &prev, &shown).0,
            "madison",
            "outgoing name shows during the first half"
        );
    }

    /// The cross-fade splits at the half-way point: `prev` over `[0, 0.5)`,
    /// `shown` over `[0.5, 1]`, at full alpha at each endpoint.
    #[test]
    fn ghost_crossfade_splits_at_half() {
        assert_eq!(ghost_crossfade(0.0, "old", "new"), ("old", 1.0));
        assert_eq!(ghost_crossfade(0.49, "old", "new").0, "old");
        assert_eq!(ghost_crossfade(0.5, "old", "new"), ("new", 0.0));
        assert_eq!(ghost_crossfade(1.0, "old", "new"), ("new", 1.0));
    }

    /// An unchanged name leaves `prev`/`shown` alone and only ticks the clock —
    /// no spurious restart while a name is settling in.
    #[test]
    fn ghost_unchanged_name_only_advances_clock() {
        let (mut prev, mut shown, mut progress) =
            ("madison".to_string(), "ada".to_string(), 0.6_f32);
        step_ghost_crossfade(&mut prev, &mut shown, &mut progress, "ada", 1.0 / 60.0);
        assert_eq!(prev, "madison");
        assert_eq!(shown, "ada");
        assert!(progress > 0.6, "clock advanced; got {progress}");
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
