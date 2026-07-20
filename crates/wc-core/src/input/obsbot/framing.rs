//! Live-apply of the operator's stored camera framing (gimbal pitch/yaw,
//! digital zoom, FOV preset) to the OBSBOT worker.
//!
//! The settings panel's framing sliders write straight into
//! [`ObsbotSettings`]; this module turns those values into
//! [`WorkerCommand`]s. Two rules shape everything here:
//!
//! 1. **Coalesce, latest wins.** A slider drag mutates the resource every
//!    frame (~60 Hz), but the worker executes each command as a blocking SDK
//!    round-trip, so flooding its (unbounded) channel would queue seconds of
//!    stale intermediate positions. [`plan_framing_send`] therefore diffs the
//!    *current* settings against the *last values actually sent* and emits at
//!    most one send per [`FRAMING_SEND_INTERVAL`]; anything that changes in
//!    between is never sent — the next allowed send transmits the latest
//!    value only.
//! 2. **Re-apply on (re)gaining control.** The take-control sequence
//!    recenters the gimbal and resets FOV/zoom to the wide defaults, so the
//!    moment the status transitions into [`ObsbotStatus::InControl`] —
//!    startup, hotplug re-acquisition, or the take-control toggle flipping
//!    back on — the stored framing is re-sent in full. An installation that
//!    restarts therefore comes back with the operator's framing, not the
//!    factory center. (The alternative — resetting the stored settings to the
//!    recentered defaults — would silently discard the operator's framing on
//!    every app restart, which is exactly when it matters most.)
//!
//! Outside `InControl` the system is a cheap no-op (one status match, no
//! commands): the worker would only warn-and-ignore manual commands anyway,
//! and the custom dock section (`section.rs`) tells the operator why the
//! sliders are inert.

use std::time::Duration;

use bevy::prelude::*;

use super::{FovPreset, ObsbotControl, ObsbotSettings, ObsbotStatus, WorkerCommand};

/// Minimum interval between framing sends while values keep changing (a
/// slider drag). 100 ms ≈ 10 sends/s: smooth enough to feel live on the
/// physical gimbal, sparse enough that the worker's blocking SDK round-trips
/// (tens of ms each) never build a backlog.
pub(super) const FRAMING_SEND_INTERVAL: Duration = Duration::from_millis(100);

/// The four framing values as one comparable unit — what was last *sent* is
/// held as one of these and diffed against the live settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct FramingValues {
    /// Gimbal pitch, degrees.
    pub pitch: f32,
    /// Gimbal yaw, degrees.
    pub yaw: f32,
    /// Absolute digital zoom, `1.0..=2.0`.
    pub zoom: f32,
    /// FOV preset.
    pub fov: FovPreset,
}

impl FramingValues {
    /// Snapshot the framing fields off the settings resource.
    pub(super) fn from_settings(s: &ObsbotSettings) -> Self {
        Self {
            pitch: s.gimbal_pitch,
            yaw: s.gimbal_yaw,
            zoom: s.zoom,
            fov: s.fov,
        }
    }
}

/// Which of the three command groups to send this frame. All-false means
/// "send nothing" — either nothing changed or the rate limit deferred it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct FramingPlan {
    /// Send [`WorkerCommand::SetGimbalAngle`] (pitch or yaw differ).
    pub gimbal: bool,
    /// Send [`WorkerCommand::SetZoom`].
    pub zoom: bool,
    /// Send [`WorkerCommand::SetFov`].
    pub fov: bool,
}

impl FramingPlan {
    /// A plan that sends every group (the re-apply-on-control case).
    const ALL: Self = Self {
        gimbal: true,
        zoom: true,
        fov: true,
    };

    /// Whether anything is to be sent.
    pub(super) fn any(self) -> bool {
        self.gimbal || self.zoom || self.fov
    }
}

/// Decide what to send, as a pure function so the coalescing policy is
/// unit-testable without a worker or a clock.
///
/// - `entered_control`: the status just transitioned into `InControl`; send
///   everything, bypassing the rate limit (take-control just recentered the
///   camera, so the stored framing must be re-asserted now).
/// - `last_sent`: the values most recently sent, `None` if nothing has been
///   sent yet this control session (treated like `entered_control` — there is
///   no baseline to diff against, so assert the full framing).
/// - `now` / `next_allowed`: the rate limit. Diffs found before
///   `next_allowed` are *not* sent and *not* recorded, so the next allowed
///   send naturally transmits the latest value only (latest-wins coalescing).
#[allow(
    clippy::float_cmp,
    reason = "deliberate exact comparison: these are slider-written values copied \
              verbatim, not arithmetic results — any bit change is an operator \
              change, and an epsilon would only delay small tweaks"
)]
pub(super) fn plan_framing_send(
    last_sent: Option<FramingValues>,
    current: FramingValues,
    entered_control: bool,
    now: Duration,
    next_allowed: Duration,
) -> FramingPlan {
    if entered_control {
        return FramingPlan::ALL;
    }
    let Some(last) = last_sent else {
        // No baseline: only reachable if control was entered before this
        // system ever ran (e.g. system-order churn); assert everything.
        return FramingPlan::ALL;
    };
    let plan = FramingPlan {
        // Exact comparison is right here: these are slider-written values,
        // not arithmetic results, so any bit change is an operator change.
        gimbal: last.pitch != current.pitch || last.yaw != current.yaw,
        zoom: last.zoom != current.zoom,
        fov: last.fov != current.fov,
    };
    if !plan.any() || now < next_allowed {
        return FramingPlan::default();
    }
    plan
}

/// Per-system-instance coalescer state (a Bevy [`Local`]).
#[derive(Default)]
pub(super) struct FramingApplyState {
    /// Values most recently sent to the worker; `None` before the first send.
    last_sent: Option<FramingValues>,
    /// `Time::elapsed` before which no further send may happen.
    next_allowed: Duration,
    /// Whether the previous frame observed `InControl` (edge detection for
    /// the re-apply-on-control rule).
    was_in_control: bool,
}

/// `PreUpdate` (chained after the status drain): forward framing-settings
/// changes to the worker while — and only while — the app holds control of
/// the camera. See the module docs for the coalescing and re-apply rules.
/// Runs in every activity state like its siblings: it is a cheap
/// value-compare no-op when idle, and framing must keep applying during the
/// screensaver (the operator tunes it from the settings panel whenever).
pub(super) fn apply_framing_settings(
    time: Res<'_, Time>,
    settings: Res<'_, ObsbotSettings>,
    ctl: Res<'_, ObsbotControl>,
    mut state: Local<'_, FramingApplyState>,
) {
    let in_control = matches!(ctl.status, ObsbotStatus::InControl { .. });
    if !in_control {
        // Dropping out of control invalidates the baseline: the next
        // InControl re-applies everything (take-control recentered the
        // device, so `last_sent` no longer describes its physical state).
        state.was_in_control = false;
        state.last_sent = None;
        return;
    }
    let entered_control = !state.was_in_control;
    state.was_in_control = true;

    let current = FramingValues::from_settings(&settings);
    let now = time.elapsed();
    let plan = plan_framing_send(
        state.last_sent,
        current,
        entered_control,
        now,
        state.next_allowed,
    );
    if !plan.any() {
        return;
    }
    if plan.gimbal {
        ctl.send_command(WorkerCommand::SetGimbalAngle {
            pitch: current.pitch,
            yaw: current.yaw,
        });
    }
    if plan.zoom {
        ctl.send_command(WorkerCommand::SetZoom(current.zoom));
    }
    if plan.fov {
        ctl.send_command(WorkerCommand::SetFov(current.fov));
    }
    state.last_sent = Some(current);
    state.next_allowed = now + FRAMING_SEND_INTERVAL;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn framing(pitch: f32, yaw: f32, zoom: f32, fov: FovPreset) -> FramingValues {
        FramingValues {
            pitch,
            yaw,
            zoom,
            fov,
        }
    }

    fn neutral() -> FramingValues {
        framing(0.0, 0.0, 1.0, FovPreset::Wide86)
    }

    /// Entering control re-asserts everything, even values equal to the
    /// recentered defaults, and bypasses the rate limit — the device was just
    /// recentered, so the stored framing is the truth to restore.
    #[test]
    fn entering_control_sends_everything_immediately() {
        let plan = plan_framing_send(
            Some(neutral()),
            neutral(),
            true,
            Duration::ZERO,
            Duration::from_secs(999), // a pending rate limit must not defer it
        );
        assert_eq!(plan, FramingPlan::ALL);
    }

    /// No baseline (nothing sent yet this control session) also asserts the
    /// full framing rather than silently diffing against nothing.
    #[test]
    fn missing_baseline_sends_everything() {
        let plan = plan_framing_send(None, neutral(), false, Duration::ZERO, Duration::ZERO);
        assert_eq!(plan, FramingPlan::ALL);
    }

    /// Unchanged values send nothing, however much time has passed.
    #[test]
    fn unchanged_values_send_nothing() {
        let plan = plan_framing_send(
            Some(neutral()),
            neutral(),
            false,
            Duration::from_secs(10),
            Duration::ZERO,
        );
        assert_eq!(plan, FramingPlan::default());
        assert!(!plan.any());
    }

    /// Only the changed group is sent: a zoom tweak must not also move the
    /// gimbal or re-set the FOV.
    #[test]
    fn only_changed_groups_are_planned() {
        let last = neutral();
        let zoomed = framing(0.0, 0.0, 1.4, FovPreset::Wide86);
        let plan = plan_framing_send(
            Some(last),
            zoomed,
            false,
            Duration::from_secs(1),
            Duration::ZERO,
        );
        assert_eq!(
            plan,
            FramingPlan {
                gimbal: false,
                zoom: true,
                fov: false
            }
        );

        // Either gimbal axis changing plans the (single) gimbal command.
        let pitched = framing(10.0, 0.0, 1.0, FovPreset::Wide86);
        let plan = plan_framing_send(
            Some(last),
            pitched,
            false,
            Duration::from_secs(1),
            Duration::ZERO,
        );
        assert!(plan.gimbal && !plan.zoom && !plan.fov);

        let fov = framing(0.0, 0.0, 1.0, FovPreset::Narrow65);
        let plan = plan_framing_send(
            Some(last),
            fov,
            false,
            Duration::from_secs(1),
            Duration::ZERO,
        );
        assert!(plan.fov && !plan.gimbal && !plan.zoom);
    }

    /// The rate limit defers a changed value (send nothing now); once the
    /// window opens the *latest* value goes out — the drag's intermediate
    /// positions were never queued anywhere (latest-wins coalescing).
    #[test]
    fn rate_limit_defers_then_sends_latest() {
        let last = neutral();
        let mid_drag = framing(5.0, 0.0, 1.0, FovPreset::Wide86);
        let now = Duration::from_millis(50);
        let next_allowed = Duration::from_millis(100);
        // Inside the window: deferred, nothing sent, baseline untouched.
        let plan = plan_framing_send(Some(last), mid_drag, false, now, next_allowed);
        assert_eq!(plan, FramingPlan::default());

        // Window open, drag has moved on: the latest value is what's planned.
        let drag_end = framing(20.0, -8.0, 1.0, FovPreset::Wide86);
        let plan = plan_framing_send(
            Some(last),
            drag_end,
            false,
            Duration::from_millis(120),
            next_allowed,
        );
        assert!(plan.gimbal && !plan.zoom && !plan.fov);
    }

    /// The settings→values snapshot maps every field to its command lane.
    #[test]
    #[allow(clippy::float_cmp, reason = "exact copies, no arithmetic")]
    fn framing_values_snapshot_maps_settings_fields() {
        let settings = ObsbotSettings {
            take_control: true,
            gimbal_pitch: -30.0,
            gimbal_yaw: 45.0,
            zoom: 1.8,
            fov: FovPreset::Medium78,
        };
        let v = FramingValues::from_settings(&settings);
        assert_eq!(v.pitch, -30.0);
        assert_eq!(v.yaw, 45.0);
        assert_eq!(v.zoom, 1.8);
        assert_eq!(v.fov, FovPreset::Medium78);
    }
}
