//! Screensaver / attract-mode framework (Plan 11.8, Seam 2).
//!
//! Promotes the old behavioral placeholder into the framework each sketch plugs
//! its attract visual into. It provides:
//!
//! - The [`in_screensaver`] run-condition (parallel to
//!   [`crate::sketch::sketch_active`]) so a sketch gates its attract systems on
//!   "this sketch is up AND the screensaver is showing".
//! - The core [`settings::ScreensaverSettings`] resource (operator caption).
//! - The instruction-caption [`overlay`] (renders only when copy is set — D6).
//! - The [`fade::ScreensaverFade`] envelope the overlay (and future attract
//!   layers) cross-fade against.
//! - Per-tier **present-rate throttling** via `bevy::winit::WinitSettings`
//!   (`UpdateMode::Reactive { wait }`) — the thermal lever that actually lowers
//!   the unattended idle frame rate (Cool ≈ 24–30 fps, Warm lower, Hot ≈ 2–5
//!   fps). No new dependency; built into `bevy_winit`.
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
//! The framework owns the *lifecycle* (enter/exit, fade, caption, present rate,
//! tier signal). Each sketch owns its *content* — its attract choreography runs
//! as systems gated on `in_screensaver(AppState::That)` (Seam 3 for Line). A
//! sketch that registers no performer is **not** a black screen: because the
//! render pipeline stays resident through the screensaver (Idle/Screensaver are
//! sub-states of the sketch), its last frame keeps drawing under the throttled
//! present rate. Authoring richer attract visuals for the other sketches is
//! deferred (spec §6); the framework already supports them via [`in_screensaver`].

pub mod fade;
pub mod overlay;
pub mod run_condition;
pub mod settings;

use std::time::Duration;

use bevy::prelude::*;
use bevy::winit::{UpdateMode, WinitSettings};
use bevy_egui::EguiPrimaryContextPass;

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
        // Operator caption settings (persisted, User panel).
        app.register_sketch_settings::<settings::ScreensaverSettings>();

        // Fade envelope + caption overlay.
        app.init_resource::<fade::ScreensaverFade>();
        app.add_systems(Update, fade::drive_screensaver_fade);
        // egui caption overlay — inert in headless harnesses without EguiPlugin.
        app.add_systems(EguiPrimaryContextPass, overlay::draw_caption_overlay);

        // Enter/exit lifecycle.
        app.add_systems(OnEnter(SketchActivity::Screensaver), show);
        app.add_systems(OnExit(SketchActivity::Screensaver), hide);

        // Per-tier present-rate throttle + capture overrides (kept out of this
        // fn so `build` stays short and the present-rate wiring reads as a unit).
        register_present_rate_systems(app);
        register_capture_overrides(app);
    }
}

/// Register the per-tier present-rate throttle: apply while the screensaver is
/// showing, restore continuous updates on exit.
fn register_present_rate_systems(app: &mut App) {
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
    tracing::info!("screensaver: show");
    commands.insert_resource(ScreensaverActive);
}

/// `OnExit(Screensaver)` — remove the [`ScreensaverActive`] marker.
fn hide(mut commands: Commands<'_, '_>) {
    tracing::info!("screensaver: hide");
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

/// Target present interval (frame-to-frame wait) per tier while in the
/// screensaver. Larger wait = lower fps = less heat. These are the present-rate
/// half of the thermal ladder; the particle-count / dispatch half lives in each
/// sketch's attract driver (Seam 3 for Line).
///
/// - Cool ≈ 30 fps (33 ms): rich attract while there is headroom.
/// - Warm ≈ 15 fps (66 ms): noticeably calmer, still animated.
/// - Hot ≈ 3 fps (333 ms): "resting ember" present rate; combined with the
///   frozen compute dispatch this is genuine cooldown.
#[must_use]
fn tier_present_wait(tier: ThermalTier) -> Duration {
    match tier {
        ThermalTier::Cool => Duration::from_millis(33),
        ThermalTier::Warm => Duration::from_millis(66),
        ThermalTier::Hot => Duration::from_millis(333),
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

/// While the screensaver is showing, set `WinitSettings` to a reactive
/// update-mode whose `wait` matches the effective tier, throttling the present
/// rate. Interaction still wakes the loop instantly (`react_to_*` all true), so
/// a passer-by waving a hand resumes at full rate without perceptible lag.
///
/// Only writes `WinitSettings` when the desired mode changes (avoids churning a
/// resource every frame).
///
/// **Capture exception:** the visual-capture harness pins its own virtual clock
/// and drives frames deterministically via a screenshot schedule; a reactive
/// `wait` of hundreds of ms per frame (the Hot tier) would let the per-frame
/// wall-clock blow past the launcher's 90 s timeout. So when a `CaptureConfig`
/// is present this system is a no-op, leaving winit in `Continuous` — the
/// captured visual (particle state, dispatch freeze) is unaffected; only the
/// *present cadence* (which capture doesn't measure) is skipped. Debug-only
/// guard; compiled out of release.
fn apply_present_rate(
    thermal: Res<'_, ThermalState>,
    time: Res<'_, Time>,
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

    let duty_wake = duty.as_deref().map(|d| d.requested_wake(time.elapsed()));
    let wait = effective_wait(tier_present_wait(tier), duty_wake);
    let desired = UpdateMode::Reactive {
        wait,
        react_to_device_events: true,
        react_to_user_events: true,
        react_to_window_events: true,
    };
    if winit.focused_mode != desired {
        winit.focused_mode = desired;
        winit.unfocused_mode = desired;
    }
}

/// `OnExit(Screensaver)` — restore continuous updates so the live sketch runs at
/// full rate again the instant a visitor interacts.
fn restore_present_rate(mut winit: ResMut<'_, WinitSettings>) {
    if winit.focused_mode != UpdateMode::Continuous {
        winit.focused_mode = UpdateMode::Continuous;
        winit.unfocused_mode = UpdateMode::Continuous;
    }
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
/// it and flap show↔hide every frame (which un-freezes the Hot ember and can
/// stall the capture's frame-advance — observed as a 90 s timeout). Instead this
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
    fn present_wait_increases_with_heat() {
        assert!(tier_present_wait(ThermalTier::Cool) < tier_present_wait(ThermalTier::Warm));
        assert!(tier_present_wait(ThermalTier::Warm) < tier_present_wait(ThermalTier::Hot));
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
            disable_bloom: false,
            disable_bone_composite: false,
            disable_bone_camera: false,
            solid_particles: None,
            force_screensaver: false,
            force_tier: Some(ThermalTier::Hot),
        };
        assert_eq!(effective_tier(&thermal, Some(&forced)), ThermalTier::Hot);
        assert_eq!(effective_tier(&thermal, None), ThermalTier::Cool);
    }
}
