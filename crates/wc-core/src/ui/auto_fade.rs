//! Auto-fading overlay chrome.
//!
//! Reads the existing [`crate::lifecycle::idle::InteractionTimer`] each
//! `Update` and drives a single [`UiOpacity`] f32 that every chrome element
//! multiplies into its alpha. The exponential approach toward the target
//! gives a CSS-`transition: opacity 0.6s ease`-equivalent feel.
//!
//! ## Data flow
//!
//! 1. [`update_opacity_target`] compares `InteractionTimer::idle_for(now)`
//!    against `OverlayUiSettings::idle_fade_threshold_seconds` each frame
//!    and sets `UiOpacity::target` to 0 or 1.
//! 2. [`lerp_opacity`] moves `UiOpacity::current` toward `target` via an
//!    exponential approach whose time constant is derived from
//!    `OverlayUiSettings::idle_fade_duration_seconds`.
//! 3. Every chrome widget reads `UiOpacity::current` and multiplies it into
//!    its alpha channel before rendering.

use std::time::Duration;

use bevy::prelude::*;

use crate::lifecycle::idle::InteractionTimer;
use crate::settings::RegisterSketchSettingsExt;

/// Current and target chrome opacity. `current` is what every overlay
/// element multiplies into its alpha; `target` is set by
/// [`update_opacity_target`] from the idle timer.
#[derive(Resource, Debug, Clone, Copy)]
pub struct UiOpacity {
    /// 0.0 = invisible, 1.0 = fully opaque.
    pub current: f32,
    /// Where `current` is lerping toward this frame.
    pub target: f32,
}

impl Default for UiOpacity {
    fn default() -> Self {
        Self {
            current: 1.0,
            target: 1.0,
        }
    }
}

/// User-facing overlay tuning. Surfaces in the dev panel via the
/// `SketchSettings` derive so kiosk operators can live-tune the idle
/// threshold and disable the blur as a perf escape hatch.
#[derive(
    wc_core_macros::SketchSettings,
    Resource,
    bevy::reflect::Reflect,
    serde::Serialize,
    serde::Deserialize,
    Clone,
    Debug,
)]
#[reflect(Resource, Default)]
#[settings(storage_key = "overlay_ui")]
pub struct OverlayUiSettings {
    /// Seconds of pointer inactivity before chrome fades out. v4 default: 30.
    #[setting(default = 30.0_f32, min = 5.0_f32, max = 600.0_f32, step = 1.0_f32, category = Dev)]
    #[serde(default = "default_idle_fade_threshold")]
    pub idle_fade_threshold_seconds: f32,

    /// Time constant for the opacity ease. v4 default: 0.6.
    #[setting(default = 0.6_f32, min = 0.0_f32, max = 5.0_f32, step = 0.1_f32, category = Dev)]
    #[serde(default = "default_idle_fade_duration")]
    pub idle_fade_duration_seconds: f32,

    /// Master toggle for the backdrop-blur pass. Dev escape hatch.
    #[setting(default = true, category = Dev)]
    #[serde(default = "default_backdrop_blur_enabled")]
    pub backdrop_blur_enabled: bool,
}

// Per-field serde defaults. Values MUST match the `#[setting(default = ...)]`
// values above. This mirrors the pattern from `LineSettings` so persisted
// TOML missing a field falls back to the design default instead of zeroing
// the whole section.
fn default_idle_fade_threshold() -> f32 {
    30.0
}
fn default_idle_fade_duration() -> f32 {
    0.6
}
fn default_backdrop_blur_enabled() -> bool {
    true
}

/// Plugin: registers resources, registers `OverlayUiSettings` with the
/// settings registry so it surfaces in the dev panel, and runs both fade
/// systems each `Update`.
///
/// Added by [`super::WaveConductorUiPlugin`]. Depends on `InteractionTimer`
/// being present, which `LifecyclePlugin` provides.
pub struct AutoFadePlugin;

impl Plugin for AutoFadePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiOpacity>();
        // Register as a settings type so the existing dev-panel
        // reflection walker surfaces the three knobs automatically.
        app.register_sketch_settings::<OverlayUiSettings>();
        app.add_systems(Update, (update_opacity_target, lerp_opacity).chain());
    }
}

/// Read `InteractionTimer::idle_for` and set `UiOpacity::target` to 0 or 1
/// based on the configured threshold. The chained `lerp_opacity` then
/// moves `current` toward the new target over the configured duration.
pub fn update_opacity_target(
    time: Res<'_, Time>,
    timer: Res<'_, InteractionTimer>,
    settings: Res<'_, OverlayUiSettings>,
    mut opacity: ResMut<'_, UiOpacity>,
) {
    let idle = timer.idle_for(time.elapsed());
    let threshold = Duration::from_secs_f32(settings.idle_fade_threshold_seconds);
    opacity.target = if idle > threshold { 0.0 } else { 1.0 };
}

/// Exponential approach from `current` toward `target`. The time constant
/// is chosen so that ~99% of the remaining gap is closed in
/// `idle_fade_duration_seconds` (TAU = duration / ln(100)).
///
/// When `tau` is zero or negative the transition is instantaneous to avoid
/// a division-by-zero in the exponent.
pub fn lerp_opacity(
    time: Res<'_, Time>,
    settings: Res<'_, OverlayUiSettings>,
    mut opacity: ResMut<'_, UiOpacity>,
) {
    let dt = time.delta_secs();
    // ln(100) ≈ 4.6051702 — 99% threshold.
    let tau = settings.idle_fade_duration_seconds / 4.605_170_2;
    if tau <= 0.0 {
        opacity.current = opacity.target;
        return;
    }
    let blend = 1.0 - (-dt / tau).exp();
    opacity.current += (opacity.target - opacity.current) * blend;
    opacity.current = opacity.current.clamp(0.0, 1.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::time::TimeUpdateStrategy;

    /// Build a minimal app with `AutoFadePlugin` wired in.
    ///
    /// Uses `TimeUpdateStrategy::ManualDuration` so tests control elapsed time
    /// deterministically. In Bevy 0.18, `Time<()>::elapsed` is derived from
    /// `Time<Virtual>` / `Time<Real>` each frame; directly mutating `Time`
    /// is overwritten by `update_virtual_time`, so `TimeUpdateStrategy` is
    /// the correct handle.
    fn make_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<InteractionTimer>();
        app.add_plugins(AutoFadePlugin);
        app
    }

    #[test]
    fn opacity_target_is_one_when_recently_interacted() {
        let mut app = make_app();
        // Default InteractionTimer last_interaction == Duration::ZERO; Time
        // elapsed at app construction is also ~0, so idle_for ≈ 0 which is
        // below the 30 s threshold.
        app.update();
        assert_eq!(app.world().resource::<UiOpacity>().target, 1.0);
    }

    #[test]
    fn opacity_target_drops_to_zero_past_threshold() {
        let mut app = make_app();
        // Time<Virtual> clamps delta to DEFAULT_MAX_DELTA (250 ms) by default.
        // Raise it so ManualDuration(60 s) actually advances elapsed by 60 s
        // in a single tick.
        app.world_mut()
            .resource_mut::<bevy::time::Time<bevy::time::Virtual>>()
            .set_max_delta(Duration::from_secs(120));
        // Use ManualDuration(60 s) — one warmup tick has delta=0, the second
        // tick advances elapsed by 60 s.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(60)));
        app.update(); // warmup: delta=0, elapsed=0
        app.update(); // actual: elapsed += 60 s → idle_for(60 s) > 30 s threshold
        let elapsed = app.world().resource::<Time>().elapsed();
        let target = app.world().resource::<UiOpacity>().target;
        assert_eq!(
            target, 0.0,
            "opacity target should be 0 after 60 s of idle (threshold = 30 s), elapsed={elapsed:?}"
        );
    }

    #[test]
    fn lerp_converges_to_target_within_duration() {
        let mut app = make_app();

        // Retrieve the configured fade duration (default 0.6 s).
        let duration = app
            .world()
            .resource::<OverlayUiSettings>()
            .idle_fade_duration_seconds;

        // Time<Virtual> clamps delta to 250 ms by default. The fade duration
        // (0.6 s) exceeds that cap, so we raise max_delta first.
        app.world_mut()
            .resource_mut::<bevy::time::Time<bevy::time::Virtual>>()
            .set_max_delta(Duration::from_secs_f32(duration * 2.0));

        // Install ManualDuration so each update advances delta by exactly
        // `duration` seconds. First update is a warmup (delta=0); second
        // bakes the full duration into Time::delta_secs().
        app.insert_resource(TimeUpdateStrategy::ManualDuration(
            Duration::from_secs_f32(duration),
        ));
        app.update(); // warmup: delta=0
        app.update(); // bakes delta = duration into Time

        // Force current=1, target=0 and call lerp_opacity directly via
        // SystemState. update_opacity_target sees idle_for ≈ duration < 30 s
        // and would reset target back to 1 — calling lerp directly avoids that.
        app.world_mut().resource_mut::<UiOpacity>().current = 1.0;
        app.world_mut().resource_mut::<UiOpacity>().target = 0.0;

        let mut state: bevy::ecs::system::SystemState<(
            Res<'_, Time>,
            Res<'_, OverlayUiSettings>,
            ResMut<'_, UiOpacity>,
        )> = bevy::ecs::system::SystemState::new(app.world_mut());
        let (time, settings, opacity) = state.get_mut(app.world_mut());
        lerp_opacity(time, settings, opacity);
        state.apply(app.world_mut());

        let current = app.world().resource::<UiOpacity>().current;
        assert!(
            current <= 0.01,
            "expected current to converge to ≤ 1% of target within one duration tick, got {current}"
        );
    }
}
