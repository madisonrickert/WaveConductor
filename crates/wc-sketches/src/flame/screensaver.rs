//! Flame attract-mode performer: name carousel + ember complexity decay.
//!
//! Two independent drivers, both gated `in_screensaver(AppState::Flame)` (zero
//! systems outside the screensaver — AGENTS.md "zero systems when idle"):
//!
//! - [`drive_flame_carousel`] slowly cycles [`FlameSettings::name`] through the
//!   editable carousel list (or [`BUILTIN_SEEDS`] when it's empty). Because
//!   `name` IS the setting the F7 name-change watcher rebuilds on, writing it
//!   here is enough to reseed the fractal — **and** because wake re-enters
//!   `Active` reading the same settings resource, the visitor sees whichever
//!   carousel name is currently showing adopted into the name box for free, no
//!   extra wiring required.
//! - [`drive_flame_attract_sim`] is the screensaver's [`bake_flame_sim`] writer
//!   (Condition A1 — one baker, two writers, so the warp/dispatch derivation
//!   cannot drift between the live and attract paths). It keeps `cX`
//!   oscillating from virtual time (the fractal keeps morphing while idle,
//!   matching v4), leaves the pointer/hand warp untouched, and lowers
//!   `complexity` via [`super::systems::sim_params::ember_complexity`] so the
//!   dispatch prefix (and therefore the drawn point count) thins toward
//!   `ember_fraction` as [`ScreensaverFade`] ramps in.
//!
//! The corresponding brightness lift lives in
//! [`super::render::drive_flame_material`] (unconditionally gated on
//! `AppState::Flame`, so it already runs during the screensaver) and the ghost
//! seed label lives in [`super::ui::flame_seed_ghost_label`].

use bevy::prelude::*;
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;
use wc_core::lifecycle::screensaver::in_screensaver;
use wc_core::lifecycle::state::AppState;

use super::compute::sim_params::FlameSimParams;
use super::settings::FlameSettings;
use super::systems::sim_params::{bake_flame_sim, ember_complexity, flame_cx, FlameState};

/// Fallback seed names cycled when [`FlameSettings::carousel_names`] is empty
/// (a fresh install, or every carousel entry deleted in the dock).
///
/// Word choices to be reviewed with Madison before the release tag.
pub const BUILTIN_SEEDS: &[&str] = &[
    "Xiaohan",
    "wave conductor",
    "ember",
    "aurora",
    "who are you?",
];

/// Carousel driver state: elapsed time since the last advance, and the index
/// of the next name to show.
#[derive(Resource, Default)]
pub struct FlameCarousel {
    /// Seconds since the last carousel advance.
    pub elapsed: f32,
    /// Index into whichever list [`next_carousel_name`] is currently reading
    /// (custom or builtin) of the name that will show next.
    pub index: usize,
}

/// Plugin wiring the Flame attract performer: the carousel driver and the
/// ember-decay sim writer, both gated `in_screensaver(AppState::Flame)`.
pub struct FlameScreensaverPlugin;

impl Plugin for FlameScreensaverPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FlameCarousel>();
        app.add_systems(
            Update,
            drive_flame_carousel.run_if(in_screensaver(AppState::Flame)),
        );
        app.add_systems(
            Update,
            drive_flame_attract_sim.run_if(in_screensaver(AppState::Flame)),
        );
    }
}

/// Pure carousel pick: `custom` if non-empty, else `builtin`. Returns the name
/// at `index % len` and the wrapped next index.
///
/// No allocation: both inputs are borrowed and the returned name borrows from
/// whichever list was chosen.
#[must_use]
pub(crate) fn next_carousel_name<'a>(
    custom: &'a [String],
    builtin: &'a [&'a str],
    index: usize,
) -> (&'a str, usize) {
    if custom.is_empty() {
        let len = builtin.len();
        let i = index % len;
        (builtin[i], (i + 1) % len)
    } else {
        let len = custom.len();
        let i = index % len;
        (custom[i].as_str(), (i + 1) % len)
    }
}

/// `Update` (`in_screensaver(AppState::Flame)`): advance the carousel clock and,
/// every [`FlameSettings::carousel_period_secs`], write the next name into
/// [`FlameSettings::name`].
///
/// The write allocates once per ~2 minutes (event-driven, not per-frame): it
/// triggers the F7 name-change watcher's rebuild, F14's audio config push, and
/// autosave, and because `name` IS the setting the wake transition adopts it
/// for free.
pub fn drive_flame_carousel(
    time: Res<'_, Time>,
    mut carousel: ResMut<'_, FlameCarousel>,
    mut settings: ResMut<'_, FlameSettings>,
) {
    carousel.elapsed += time.delta_secs();
    if carousel.elapsed < settings.carousel_period_secs {
        return;
    }
    carousel.elapsed = 0.0;
    let (name, next_index) =
        next_carousel_name(&settings.carousel_names, BUILTIN_SEEDS, carousel.index);
    settings.name = name.to_string();
    carousel.index = next_index;
}

/// `Update` (`in_screensaver(AppState::Flame)`): the screensaver's
/// [`bake_flame_sim`] writer (Condition A1 — one baker, two writers).
///
/// Advances `cX` from virtual time so the fractal keeps morphing while idle
/// (matching v4's screensaver, which never fully froze it), leaves
/// `warp_input` untouched (no pointer/hand input during attract), and lowers
/// `complexity` toward [`FlameSettings::ember_fraction`] as
/// [`ScreensaverFade::alpha`] ramps in — the graceful decay AND the roar-back
/// ride the same 1.5 s envelope in both directions.
pub fn drive_flame_attract_sim(
    time: Res<'_, Time>,
    settings: Res<'_, FlameSettings>,
    fade: Res<'_, ScreensaverFade>,
    mut state: ResMut<'_, FlameState>,
    mut sim: ResMut<'_, FlameSimParams>,
) {
    state.c_x = flame_cx(time.elapsed_secs_f64());
    state.complexity = ember_complexity(fade.alpha(), settings.ember_fraction);
    bake_flame_sim(&state, &mut sim);
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use crate::flame::branches::build_flame_spec;
    use crate::flame::compute::sim_params::{encode_branches, encode_levels, FlameLevelParamsGpu};
    use crate::flame::levels::{LevelLayout, MAX_LEVELS};
    use bevy::ecs::system::RunSystemOnce;
    use bytemuck::Zeroable;
    use std::time::Duration;

    #[test]
    fn next_carousel_prefers_custom_list() {
        let custom = vec!["madison".to_string(), "xiaohan".to_string()];
        let (name, next) = next_carousel_name(&custom, BUILTIN_SEEDS, 0);
        assert_eq!(name, "madison");
        assert_eq!(next, 1);
        let (name, next) = next_carousel_name(&custom, BUILTIN_SEEDS, 1);
        assert_eq!(name, "xiaohan");
        assert_eq!(next, 0, "wraps");
    }

    #[test]
    fn next_carousel_falls_back_to_builtin_when_empty() {
        let custom: Vec<String> = vec![];
        let (name, _) = next_carousel_name(&custom, BUILTIN_SEEDS, 0);
        assert_eq!(name, BUILTIN_SEEDS[0]);
    }

    #[test]
    fn next_carousel_index_out_of_range_wraps() {
        let custom = vec!["a2".to_string()];
        let (name, next) = next_carousel_name(&custom, BUILTIN_SEEDS, 99);
        assert_eq!(name, "a2");
        assert_eq!(next, 0);
    }

    /// Build a `FlameSimParams` from a spec/layout with a default node handle,
    /// mirroring `systems::sim_params`'s test helper.
    fn test_sim_params(
        spec: &crate::flame::branches::FlameSpec,
        layout: &LevelLayout,
    ) -> FlameSimParams {
        let mut levels = [FlameLevelParamsGpu::zeroed(); MAX_LEVELS];
        let level_count = encode_levels(layout, &mut levels);
        FlameSimParams {
            params: encode_branches(spec),
            levels,
            level_count,
            nodes: Handle::default(),
        }
    }

    /// `drive_flame_carousel` writes the builtin seed 0 and resets `elapsed`
    /// once the virtual clock crosses `carousel_period_secs`.
    #[test]
    fn drive_flame_carousel_advances_after_period() {
        let mut world = World::new();
        world.insert_resource(FlameSettings {
            carousel_period_secs: 0.1,
            ..Default::default()
        });
        world.insert_resource(FlameCarousel::default());
        let mut time = Time::<()>::default();
        time.advance_by(Duration::from_millis(200));
        world.insert_resource(time);

        world
            .run_system_once(drive_flame_carousel)
            .expect("drive_flame_carousel run");

        assert_eq!(world.resource::<FlameSettings>().name, "Xiaohan");
        assert!((world.resource::<FlameCarousel>().elapsed - 0.0).abs() < 1e-6);
    }

    /// `drive_flame_attract_sim` lowers `complexity` via `ember_complexity` and
    /// bakes it into the dispatch prefix, mirroring the live writer's baker.
    #[test]
    fn drive_flame_attract_sim_applies_ember_decay() {
        let spec = build_flame_spec("madison");
        let layout = LevelLayout::build(4, 100_000.0);
        let full_levels = u32::try_from(layout.levels.len()).expect("fits") - 1;
        let sim = test_sim_params(&spec, &layout);

        let mut world = World::new();
        // Branch-major layout is heavily back-loaded (the last level holds
        // most of the tree), so the minimum ember_fraction (0.2) is used
        // here to guarantee the live prefix actually crosses below the
        // final level's start and the dispatch count visibly shrinks; 0.5
        // (the default) is well within the same last level for this
        // branch-count/depth combination and would not move `level_count`.
        world.insert_resource(FlameSettings {
            ember_fraction: 0.2,
            ..Default::default()
        });
        let mut fade = ScreensaverFade::default();
        fade.set_target(1.0);
        let fade = fade.advanced(Duration::from_secs(10));
        world.insert_resource(fade);
        world.insert_resource(FlameState {
            spec,
            layout,
            last_name: "madison".into(),
            last_target_points: 100_000.0,
            c_x: 0.0,
            warp_input: Vec2::ZERO,
            complexity: 1.0,
        });
        world.insert_resource(sim);
        world.insert_resource(Time::<()>::default());

        world
            .run_system_once(drive_flame_attract_sim)
            .expect("drive_flame_attract_sim run");

        let state = world.resource::<FlameState>();
        assert!(
            (state.complexity - 0.2).abs() < 1e-6,
            "full fade -> ember_fraction"
        );
        let sim = world.resource::<FlameSimParams>();
        assert!(
            sim.level_count < full_levels,
            "ember decay must thin the dispatch prefix"
        );
    }
}
