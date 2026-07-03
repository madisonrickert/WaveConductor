//! Screensaver / attract-mode framework (Plan 11.8, Seam 2).
//!
//! Promotes the old behavioral placeholder into the framework each sketch plugs
//! its attract visual into. It provides:
//!
//! - The [`in_screensaver`] run-condition (parallel to
//!   [`crate::sketch::sketch_active`]) so a sketch gates its attract systems on
//!   "this sketch is up AND the screensaver is showing".
//! - The core [`settings::ScreensaverSettings`] resource (the FPS cap; the
//!   former operator caption was cut 2026-06-10 — see the settings module).
//! - The [`fade::ScreensaverFade`] envelope attract layers can cross-fade
//!   against.
//! - Per-tier **present-rate throttling** via `bevy::winit::WinitSettings`
//!   (`UpdateMode::Reactive { wait }`) — the thermal lever that actually lowers
//!   the unattended idle frame rate. Capped at the operator's "Screensaver FPS
//!   cap" setting (default 15 fps) regardless of temperature (the Cool tier),
//!   with Warm ≈ 15 fps and Hot ≈ 3 fps floored at that cap so heat only ever
//!   lowers the rate. The reactive loop drives the whole schedule, so the cap
//!   also throttles the particle compute dispatch and smear post pass against
//!   an uncapped display. The prior winit modes are snapshotted on entry
//!   (`SavedPresentMode`) and restored *exactly* on any exit. No new
//!   dependency; built into `bevy_winit`. Only `Screensaver` throttles — the
//!   pre-screensaver `Idle` window stays at full rate (the sketch is still
//!   fully visible then).
//! - The capture overrides (`WC_DEBUG_FORCE_SCREENSAVER`, `WC_DEBUG_FORCE_TIER`)
//!   so the visual harness can land in attract mode at a chosen tier
//!   deterministically.
//!
//! ## Effective tier
//!
//! Systems should read [`effective_tier`] rather than `ThermalState` directly:
//! it honours `WC_DEBUG_FORCE_TIER` in debug builds (capture), and falls back to
//! the live thermal tier otherwise. The Line attract driver and the present-rate
//! throttle both go through it so a forced capture tier drives the whole stack
//! consistently.
//!
//! ## What lives here vs. in the sketch
//!
//! The framework owns the *lifecycle* (enter/exit, fade, present rate,
//! tier signal). Each sketch owns its *content* — its attract choreography runs
//! as systems gated on `in_screensaver(AppState::That)` (Seam 3 for Line). A
//! sketch that registers no performer is **not** a black screen: because the
//! render pipeline stays resident through the screensaver (Idle/Screensaver are
//! sub-states of the sketch), its last frame keeps drawing under the throttled
//! present rate. Authoring richer attract visuals for the other sketches is
//! deferred (spec §6); the framework already supports them via [`in_screensaver`].

pub mod fade;
pub mod run_condition;
pub mod settings;

use std::time::Duration;

use bevy::prelude::*;
use bevy::winit::{UpdateMode, WinitSettings};

#[cfg(debug_assertions)]
use crate::debug::DebugToggles;
use crate::lifecycle::state::SketchActivity;
use crate::lifecycle::thermal::{ThermalState, ThermalTier};
use crate::settings::RegisterSketchSettingsExt;

pub use run_condition::in_screensaver;
pub use settings::ScreensaverSettings;

/// Marker resource present iff the screensaver is currently shown. Retained from
/// the Plan 2 placeholder so any existing reader keeps working; the framework
/// now does much more around it.
#[derive(Resource, Default, Debug)]
pub struct ScreensaverActive;

/// Plugin wiring the screensaver framework. Registered by
/// [`crate::lifecycle::LifecyclePlugin`].
pub struct ScreensaverPlugin;

impl Plugin for ScreensaverPlugin {
    fn build(&self, app: &mut App) {
        // Attract-mode settings (persisted, User panel).
        app.register_sketch_settings::<settings::ScreensaverSettings>();

        // Fade envelope (consumed by attract layers that want a smooth
        // appear/disappear; the caption overlay that used to read it is gone).
        app.init_resource::<fade::ScreensaverFade>();
        app.add_systems(Update, fade::drive_screensaver_fade);

        // Enter/exit lifecycle.
        app.add_systems(
            OnEnter(SketchActivity::Screensaver),
            (show, close_settings_panels),
        );
        app.add_systems(OnExit(SketchActivity::Screensaver), hide);

        // Per-tier present-rate throttle + capture overrides (kept out of this
        // fn so `build` stays short and the present-rate wiring reads as a unit).
        register_present_rate_systems(app);
        register_capture_overrides(app);
    }
}

/// Register the per-tier present-rate throttle: snapshot the prior winit modes
/// and start throttling on entry, restore the snapshot on exit.
fn register_present_rate_systems(app: &mut App) {
    app.add_systems(OnEnter(SketchActivity::Screensaver), save_present_mode);
    app.add_systems(
        Update,
        apply_present_rate.run_if(resource_exists::<ScreensaverActive>),
    );
    app.add_systems(OnExit(SketchActivity::Screensaver), restore_present_rate);
}

/// Register the capture-only force-into-screensaver system (debug builds).
///
/// Ordered BEFORE the idle state machine so the zeroed idle thresholds it sets
/// take effect in `advance_activity` the same frame — `advance_activity` then
/// becomes the single writer of `NextState<SketchActivity>` and naturally holds
/// `Screensaver` (no two-writer flapping). No-op (and not registered) in release.
#[cfg(debug_assertions)]
fn register_capture_overrides(app: &mut App) {
    app.add_systems(
        Update,
        apply_force_screensaver.before(crate::lifecycle::idle::advance_activity),
    );
}

/// Release stub: no capture overrides compiled in.
#[cfg(not(debug_assertions))]
fn register_capture_overrides(_app: &mut App) {}

/// `OnEnter(Screensaver)` — insert the [`ScreensaverActive`] marker.
fn show(mut commands: Commands<'_, '_>) {
    tracing::info!("screensaver: showing — attract mode engaged");
    commands.insert_resource(ScreensaverActive);
}

/// `OnEnter(Screensaver)` — close the user-facing settings panel so attract
/// mode is a clean kiosk surface. Optional resources keep this lifecycle hook
/// usable in tests that install the screensaver framework without the UI plugin.
fn close_settings_panels(user_panel: Option<ResMut<'_, crate::ui::buttons::SettingsPanelVisible>>) {
    if let Some(mut user_panel) = user_panel {
        if user_panel.0 {
            tracing::info!("screensaver: closing settings panel");
        }
        user_panel.0 = false;
    }
}

/// `OnExit(Screensaver)` — remove the [`ScreensaverActive`] marker. The log
/// is the operator's unambiguous signal that the screensaver has woken back to
/// normal interaction.
fn hide(mut commands: Commands<'_, '_>) {
    tracing::info!("screensaver: woke — interaction resumed");
    commands.remove_resource::<ScreensaverActive>();
}

/// The tier the screensaver should render at this frame: the
/// `WC_DEBUG_FORCE_TIER` override when set (debug capture), else the live
/// [`ThermalState`] tier. Centralised so the present-rate throttle and each
/// sketch's attract driver agree on the same tier.
///
/// In release the override path compiles out, so this is just the live tier.
#[must_use]
pub fn effective_tier(
    thermal: &ThermalState,
    #[cfg(debug_assertions)] toggles: Option<&DebugToggles>,
) -> ThermalTier {
    #[cfg(debug_assertions)]
    if let Some(forced) = toggles.and_then(|t| t.force_tier) {
        return forced;
    }
    thermal.tier
}

/// Guard rails on the persisted `screensaver_fps` setting: a hand-edited TOML
/// outside the slider's 5–60 range is clamped here rather than producing a
/// degenerate (zero/negative/absurd/NaN) present wait — NaN would otherwise
/// PANIC in `Duration::from_secs_f64` (TOML accepts `nan` as a float).
const SCREENSAVER_FPS_MIN: f64 = 1.0;
const SCREENSAVER_FPS_MAX: f64 = 240.0;

/// Target present interval (frame-to-frame wait) per tier while in the
/// screensaver, capped by the operator's "Screensaver FPS cap" setting
/// (`cap_fps`, see [`settings::ScreensaverSettings::screensaver_fps`];
/// default 15). Larger wait = lower fps = less heat. These are the
/// present-rate half of the thermal ladder; the particle-count / dispatch
/// half lives in each sketch's attract driver (Seam 3 for Line).
///
/// - Cool = the cap exactly: rich attract while there is headroom — this is
///   the temperature-independent screensaver rate.
/// - Warm ≈ 15 fps (66 ms): noticeably calmer, still animated.
/// - Hot ≈ 3 fps (333 ms): "resting ember" present rate. The ~10× drop from
///   an uncapped rate alone is the cooldown ("Low-Rate Ember", spec §10.1) —
///   there is no dispatch freeze; that remains a deferred, soak-gated
///   escalation.
///
/// Warm and Hot floor at the Cool wait (`.max(cap_wait)`) so heat only ever
/// *lowers* the present rate: a cap below 15 fps would otherwise make the
/// Warm tier render *faster* than the cool screensaver.
#[must_use]
#[allow(
    clippy::manual_clamp,
    reason = "max().min() is deliberate: clamp() passes NaN through, and a NaN wait panics in \
              Duration::from_secs_f64 — max/min sanitize a `screensaver_fps = nan` TOML to the rail"
)]
fn tier_present_wait(tier: ThermalTier, cap_fps: f32) -> Duration {
    // .max().min() instead of .clamp(): clamp passes NaN through, and a NaN
    // wait panics in Duration::from_secs_f64. f64::max/min return the other
    // operand for NaN, so a `screensaver_fps = nan` TOML lands on the MIN
    // rail instead of crashing the first throttled frame.
    let cap_wait = Duration::from_secs_f64(
        1.0 / f64::from(cap_fps)
            .max(SCREENSAVER_FPS_MIN)
            .min(SCREENSAVER_FPS_MAX),
    );
    match tier {
        ThermalTier::Cool => cap_wait,
        ThermalTier::Warm => Duration::from_millis(66).max(cap_wait),
        ThermalTier::Hot => Duration::from_millis(333).max(cap_wait),
    }
}

/// The reactive present `wait`: the tier's wait, floored to the Leap duty cycle's
/// requested wake so the gap ends on time and sample windows are polled fast
/// enough to catch a resume frame. `None` when the duty cycle is absent (the
/// `hand-tracking-gestures` feature is off, or no Leap is installed).
#[must_use]
fn effective_wait(tier_wait: Duration, duty_wake: Option<Duration>) -> Duration {
    match duty_wake {
        Some(w) => tier_wait.min(w),
        None => tier_wait,
    }
}

/// The `WinitSettings` modes in effect *before* the screensaver throttled them,
/// snapshotted on `OnEnter(Screensaver)` and written back on exit. Stored as a
/// resource (not assumed to be `Continuous`) so the restore is exact whatever
/// baseline the app was configured with — e.g. the `WinitSettings::default()`
/// (= `game()`) baseline keeps a `reactive_low_power` *unfocused* mode that a
/// hard-coded `Continuous` restore would clobber into an uncapped burn.
#[derive(Resource, Debug, Clone)]
struct SavedPresentMode {
    /// `WinitSettings::focused_mode` at screensaver entry.
    focused: UpdateMode,
    /// `WinitSettings::unfocused_mode` at screensaver entry.
    unfocused: UpdateMode,
}

/// `OnEnter(Screensaver)` — snapshot the current winit update modes into
/// [`SavedPresentMode`] before [`apply_present_rate`] (Update, gated on
/// [`ScreensaverActive`]) first overwrites them, so [`restore_present_rate`]
/// can put back exactly what was there.
fn save_present_mode(mut commands: Commands<'_, '_>, winit: Res<'_, WinitSettings>) {
    commands.insert_resource(SavedPresentMode {
        focused: winit.focused_mode,
        unfocused: winit.unfocused_mode,
    });
}

/// While the screensaver is showing, set `WinitSettings` to a reactive
/// update-mode whose `wait` matches the effective tier, throttling the present
/// rate. Mouse / keyboard / touch interaction wakes the loop instantly
/// (`react_to_*` all true), so a visitor at the controls resumes at full rate
/// without perceptible lag. The *unfocused* mode gets the same reactive mode —
/// deliberately more device-event-responsive than the `game()` baseline's
/// unfocused `reactive_low_power` (`react_to_device_events: true` vs `false`),
/// so a passer-by can wake an unfocused window too.
///
/// **Hand-wake chain (webcam / MediaPipe):** camera frames are *not* winit
/// events, so a hand wakes the install through the polled path instead — and
/// nothing wakes the loop early, so each step costs a full reactive tick. The
/// inference worker (already capped at 4 Hz in Idle/Screensaver, commit
/// b3d6589a) emits a hand-bearing frame → tick N's `poll_all_providers`
/// (`PreUpdate`) drains it and `reset_on_interaction` / `advance_activity`
/// write `NextState` in tick N's `Update` — but `StateTransition` runs *before*
/// `Update` in the `Main` schedule, so the `Active` flip and the
/// `OnExit(Screensaver)` restore land in tick N+1's `StateTransition`, a
/// second full wait later. The throttle therefore adds ≤ 2 reactive ticks:
/// ~133 ms at the default 15 fps cap (~66 ms at a 30 fps cap), ≈ 433 ms total
/// against the ~300 ms worst-case wake documented on the inference throttle.
/// At hotter tiers the two ticks scale with the wait — ≤ 666 ms at Hot,
/// ≈ 0.97 s total worst case, an accepted trade in a thermal emergency.
///
/// Only writes `WinitSettings` when the desired mode changes (avoids churning a
/// resource every frame).
///
/// **Capture exception:** the visual-capture harness pins its own virtual clock
/// and drives frames deterministically via a screenshot schedule; a reactive
/// `wait` of hundreds of ms per frame (the Hot tier) would let the per-frame
/// wall-clock blow past the launcher's 90 s timeout. So when a `CaptureConfig`
/// is present this system is a no-op, leaving winit in `Continuous` — the
/// captured visual (particle state, choreography) is unaffected; only the
/// *present cadence* (which capture doesn't measure) is skipped. Debug-only
/// guard; compiled out of release.
fn apply_present_rate(
    thermal: Res<'_, ThermalState>,
    time: Res<'_, Time>,
    settings: Res<'_, settings::ScreensaverSettings>,
    duty: Option<Res<'_, crate::input::idle_pause::LeapIdlePause>>,
    #[cfg(debug_assertions)] toggles: Option<Res<'_, DebugToggles>>,
    #[cfg(debug_assertions)] capture: Option<Res<'_, crate::capture::config::CaptureConfig>>,
    mut winit: ResMut<'_, WinitSettings>,
) {
    // During a deterministic capture, never throttle the present rate (see docs).
    #[cfg(debug_assertions)]
    if capture.is_some() {
        return;
    }

    #[cfg(debug_assertions)]
    let tier = effective_tier(&thermal, toggles.as_deref());
    #[cfg(not(debug_assertions))]
    let tier = effective_tier(&thermal);

    // Known follow-up (pre-existing): during a Leap duty-cycle Paused countdown,
    // `requested_wake` shrinks every tick, so `desired` differs each frame and
    // `WinitSettings` is rewritten per frame (opt-in path only; harmless churn).
    let duty_wake = duty.as_deref().map(|d| d.requested_wake(time.elapsed()));
    let wait = effective_wait(tier_present_wait(tier, settings.screensaver_fps), duty_wake);
    let desired = UpdateMode::Reactive {
        wait,
        react_to_device_events: true,
        react_to_user_events: true,
        react_to_window_events: true,
    };
    if winit.focused_mode != desired {
        let fps = if wait.is_zero() {
            f64::INFINITY
        } else {
            1.0 / wait.as_secs_f64()
        };
        tracing::info!(
            tier = ?tier,
            wait_ms = wait.as_secs_f64() * 1000.0,
            fps,
            "screensaver: present-rate throttle active"
        );
        winit.focused_mode = desired;
        winit.unfocused_mode = desired;
    }
}

/// `OnExit(Screensaver)` — restore the [`SavedPresentMode`] snapshot so the
/// live sketch runs at its configured full rate again the instant a visitor
/// interacts. Restores *both* modes exactly as they were (no `Continuous`
/// assumption), then drops the snapshot.
///
/// If the snapshot is somehow absent (it is inserted on every `OnEnter`), fall
/// back to the app's `WinitSettings::default()` modes rather than leaving the
/// live sketch stranded at the throttled rate.
fn restore_present_rate(
    mut commands: Commands<'_, '_>,
    saved: Option<Res<'_, SavedPresentMode>>,
    mut winit: ResMut<'_, WinitSettings>,
) {
    let (focused, unfocused) = if let Some(saved) = saved {
        (saved.focused, saved.unfocused)
    } else {
        let default = WinitSettings::default();
        (default.focused_mode, default.unfocused_mode)
    };
    if winit.focused_mode != focused || winit.unfocused_mode != unfocused {
        tracing::info!("screensaver: present-rate throttle restored");
        winit.focused_mode = focused;
        winit.unfocused_mode = unfocused;
    }
    commands.remove_resource::<SavedPresentMode>();
}

/// Capture helper (debug only): when `WC_DEBUG_FORCE_SCREENSAVER` is set, pin
/// `SketchActivity::Screensaver` so the harness lands in (and stays in) attract
/// mode without waiting out the idle timer.
///
/// ## Why it drives the idle timer, not `NextState` directly
///
/// `advance_activity` writes `NextState<SketchActivity>` every frame from the
/// idle timer. Under the capture clock almost no virtual time elapses, so it
/// targets `Active` each frame; a force that *also* wrote `NextState` would fight
/// it and flap show↔hide every frame (which churns the throttle apply/restore
/// writes and can stall the capture's frame-advance — observed as a 90 s
/// timeout). Instead this
/// collapses both idle thresholds to zero, so `advance_activity` *itself* targets
/// `Screensaver` from the first frame and stays there: a single writer, no race.
/// Registered to run `before` `advance_activity` so the zeroed thresholds take
/// effect the same frame.
#[cfg(debug_assertions)]
fn apply_force_screensaver(
    toggles: Option<Res<'_, DebugToggles>>,
    mut timer: ResMut<'_, crate::lifecycle::idle::InteractionTimer>,
) {
    let Some(toggles) = toggles else {
        return;
    };
    if !toggles.force_screensaver {
        return;
    }
    if !timer.idle_threshold.is_zero() || !timer.screensaver_threshold.is_zero() {
        timer.idle_threshold = std::time::Duration::ZERO;
        timer.screensaver_threshold = std::time::Duration::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_wait_is_floored_to_duty_cycle() {
        // Hot tier present wait (333 ms) yields to a tighter duty-cycle wake.
        let tier = Duration::from_millis(333);
        assert_eq!(
            effective_wait(tier, Some(Duration::from_millis(16))),
            Duration::from_millis(16)
        );
        // No duty cycle → tier wait unchanged.
        assert_eq!(effective_wait(tier, None), tier);
        // Duty cycle slower than tier (long gap) → tier wait wins.
        assert_eq!(effective_wait(tier, Some(Duration::from_millis(350))), tier);
    }

    #[test]
    fn cool_tier_wait_enforces_the_screensaver_fps_cap() {
        // The temperature-independent screensaver cap: even at full thermal
        // headroom (Cool), presents never exceed the configured cap — here
        // the setting's default.
        let cap = settings::ScreensaverSettings::default().screensaver_fps;
        assert_eq!(
            tier_present_wait(ThermalTier::Cool, cap),
            Duration::from_secs_f64(1.0 / f64::from(cap))
        );
    }

    #[test]
    fn present_wait_never_decreases_with_heat() {
        // At a generous cap (30 fps) the tier ladder is strictly increasing…
        assert!(
            tier_present_wait(ThermalTier::Cool, 30.0) < tier_present_wait(ThermalTier::Warm, 30.0)
        );
        assert!(
            tier_present_wait(ThermalTier::Warm, 30.0) < tier_present_wait(ThermalTier::Hot, 30.0)
        );
        // …and at the default 15 fps cap, Warm's nominal ~15 fps floors at the
        // cap (equal wait) instead of presenting FASTER than the cool tier.
        let cap = settings::ScreensaverSettings::default().screensaver_fps;
        assert!(
            tier_present_wait(ThermalTier::Cool, cap) <= tier_present_wait(ThermalTier::Warm, cap)
        );
        assert!(
            tier_present_wait(ThermalTier::Warm, cap) <= tier_present_wait(ThermalTier::Hot, cap)
        );
        // An extreme low cap (5 fps, 200 ms) outwaits Warm's 66 ms everywhere
        // except Hot's 333 ms.
        assert_eq!(
            tier_present_wait(ThermalTier::Warm, 5.0),
            Duration::from_secs_f64(1.0 / 5.0)
        );
        assert_eq!(
            tier_present_wait(ThermalTier::Hot, 5.0),
            Duration::from_millis(333)
        );
    }

    #[test]
    fn degenerate_persisted_fps_is_clamped_not_divided_by() {
        // A hand-edited TOML with fps = 0 (or negative) must not produce an
        // infinite/zero wait — it clamps to the guard-rail minimum.
        let wait = tier_present_wait(ThermalTier::Cool, 0.0);
        assert_eq!(wait, Duration::from_secs_f64(1.0 / SCREENSAVER_FPS_MIN));
        let wait = tier_present_wait(ThermalTier::Cool, -10.0);
        assert_eq!(wait, Duration::from_secs_f64(1.0 / SCREENSAVER_FPS_MIN));
        // And an absurdly high value clamps to the max instead of busy-waiting.
        let wait = tier_present_wait(ThermalTier::Cool, 100_000.0);
        assert_eq!(wait, Duration::from_secs_f64(1.0 / SCREENSAVER_FPS_MAX));
        // NaN (TOML accepts `nan`) must not panic Duration::from_secs_f64;
        // it lands on the MIN rail like the other degenerate values.
        let wait = tier_present_wait(ThermalTier::Cool, f32::NAN);
        assert_eq!(wait, Duration::from_secs_f64(1.0 / SCREENSAVER_FPS_MIN));
    }

    #[test]
    #[cfg(debug_assertions)]
    fn effective_tier_honours_force_override() {
        let thermal = ThermalState {
            tier: ThermalTier::Cool,
            ..ThermalState::default()
        };
        let forced = DebugToggles {
            force_g: None,
            disable_smear: false,
            disable_explode: false,
            disable_bloom: false,
            disable_bone_composite: false,
            disable_bone_camera: false,
            solid_particles: None,
            force_screensaver: false,
            force_tier: Some(ThermalTier::Hot),
            force_cymatics_interaction: false,
            force_flame_warp: false,
            force_flame_camera_pose: false,
        };
        assert_eq!(effective_tier(&thermal, Some(&forced)), ThermalTier::Hot);
        assert_eq!(effective_tier(&thermal, None), ThermalTier::Cool);
    }
}
