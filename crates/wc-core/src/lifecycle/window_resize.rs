//! Debounced window-resize settling signal.
//!
//! ## Why this exists
//!
//! Nothing in Line, Dots, or Flame reacts to a window resize: each derives its
//! particle count — and Cymatics its sim-grid resolution — from the window size
//! exactly once, at spawn (`OnEnter`). Pressing F11 fullscreens the window, but
//! the sketch keeps drawing its field into the old extent until the operator
//! navigates away and back, which respawns it. See
//! `docs/superpowers/specs/2026-07-08-windows-remediation-design.md` §2.3.
//!
//! ## What this module does
//!
//! [`debounce_window_resize`] watches both [`bevy::window::WindowResized`] and
//! [`bevy::window::WindowScaleFactorChanged`] and, once [`RESIZE_DEBOUNCE`]
//! (250 ms) has passed with no further event, emits a single
//! [`WindowResizeSettled`] message. Debouncing prevents respawn thrash while a
//! window edge is dragged; in kiosk use a resize only happens at F11, at a
//! monitor re-enumeration, and at the startup scale-factor settle, so the signal
//! fires rarely.
//!
//! Each sketch listens for [`WindowResizeSettled`] and re-runs its spawn path
//! via the shared reload overlay (see `crate::sketch::reload_on_resize_settled`).
//!
//! ## Why the timing is a free function
//!
//! `debounce_step` is a pure function of `(last_event_at, got_event, now)`, so
//! the settle timing is unit-tested in a tight loop without a window, an egui
//! context, or a GPU — none of which CI has (there are no GPU tests in CI, and
//! the capture harness returns black frames for a backgrounded window). The
//! Bevy system is a thin shell that drains the two message readers and calls it.

use std::time::Duration;

use bevy::prelude::*;
use bevy::window::{WindowResized, WindowScaleFactorChanged};

/// Quiet window that must elapse after the last resize / scale-factor event
/// before [`WindowResizeSettled`] fires.
///
/// 250 ms is short enough that an F11 fullscreen feels immediate, long enough
/// that dragging a window edge (a stream of `WindowResized` events) collapses to
/// a single respawn at the end of the drag rather than one per frame.
pub const RESIZE_DEBOUNCE: Duration = Duration::from_millis(250);

/// Emitted once the window has stopped resizing for [`RESIZE_DEBOUNCE`].
///
/// Consumed by each sketch's `reload_on_resize_settled` listener, which
/// re-runs the sketch's spawn path so its window-size-derived resources
/// (particle counts, the Cymatics sim grid) are rebuilt at the new extent.
#[derive(Message, Debug, Clone)]
pub struct WindowResizeSettled;

/// Debounce [`WindowResized`] and [`WindowScaleFactorChanged`] into a single
/// [`WindowResizeSettled`] message emitted [`RESIZE_DEBOUNCE`] after the last
/// event.
///
/// Registered unconditionally in [`crate::lifecycle::LifecyclePlugin`] `Update`.
/// Like `drive_reload_state` and the `restart_on_settings_change` listeners,
/// this is a sanctioned always-on message listener: it must observe resize
/// events in every state (including `Home`), and it no-ops in one cheap branch
/// on any frame with no event, so it does not violate "zero systems when idle"
/// (see AGENTS.md, which names this exception class).
///
/// The per-system [`Local`] holds the timestamp of the last observed event
/// (`None` once a settle has been emitted). All timing decisions are delegated
/// to the pure `debounce_step`.
pub fn debounce_window_resize(
    mut resized: MessageReader<'_, '_, WindowResized>,
    mut scale_changed: MessageReader<'_, '_, WindowScaleFactorChanged>,
    time: Res<'_, Time>,
    mut writer: MessageWriter<'_, WindowResizeSettled>,
    mut last_event_at: Local<'_, Option<Duration>>,
) {
    // Drain BOTH readers every frame. `||` would short-circuit and leave the
    // second reader's messages unread — they would persist and re-trigger next
    // frame — so read each into a bool first, then combine.
    let got_resize = resized.read().count() > 0;
    let got_scale = scale_changed.read().count() > 0;

    let outcome = debounce_step(*last_event_at, got_resize || got_scale, time.elapsed());
    *last_event_at = outcome.next_last_event_at;
    if outcome.emit {
        writer.write(WindowResizeSettled);
        tracing::debug!("window resize settled (debounced); emitting WindowResizeSettled");
    }
}

/// Outcome of one [`debounce_step`]: the timer state to carry to the next frame,
/// and whether a settle should be emitted this frame.
struct DebounceOutcome {
    /// New value for the caller's `last_event_at` timer. `None` means disarmed
    /// (either never armed, or just emitted).
    next_last_event_at: Option<Duration>,
    /// Whether [`WindowResizeSettled`] should be written this frame.
    emit: bool,
}

/// Pure debounce decision.
///
/// Given the previously stored event timestamp, whether an event arrived this
/// frame, and the current elapsed time, returns the next timer state and whether
/// to emit. An event (re)arms the timer to `now`; a settle fires the first frame
/// on which `now` is at least [`RESIZE_DEBOUNCE`] past the armed timestamp, and
/// disarms so it fires exactly once per quiet period.
fn debounce_step(
    last_event_at: Option<Duration>,
    got_event: bool,
    now: Duration,
) -> DebounceOutcome {
    // An event this frame rearms the timer to `now`, pushing the deadline out.
    let armed = if got_event { Some(now) } else { last_event_at };
    match armed {
        Some(t) if now.saturating_sub(t) >= RESIZE_DEBOUNCE => DebounceOutcome {
            next_last_event_at: None,
            emit: true,
        },
        other => DebounceOutcome {
            next_last_event_at: other,
            emit: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed base instant so the tests read as wall-clock arithmetic.
    const T0: Duration = Duration::from_secs(1);

    #[test]
    fn idle_with_no_pending_timer_emits_nothing() {
        let out = debounce_step(None, false, T0);
        assert!(out.next_last_event_at.is_none());
        assert!(!out.emit);
    }

    #[test]
    fn an_event_arms_the_timer_without_emitting() {
        let out = debounce_step(None, true, T0);
        assert_eq!(
            out.next_last_event_at,
            Some(T0),
            "the arming frame records `now`"
        );
        assert!(
            !out.emit,
            "the arming frame must not emit; the debounce waits for quiet"
        );
    }

    #[test]
    fn a_second_event_before_the_window_pushes_the_deadline_out() {
        // A fresh event 100 ms after the first (< RESIZE_DEBOUNCE) rearms the
        // timer to the new `now` rather than emitting.
        let later = T0 + Duration::from_millis(100);
        let out = debounce_step(Some(T0), true, later);
        assert_eq!(
            out.next_last_event_at,
            Some(later),
            "a fresh event rearms to `now`"
        );
        assert!(!out.emit);
    }

    #[test]
    fn no_emit_one_millisecond_before_the_window_closes() {
        let now = T0 + RESIZE_DEBOUNCE.saturating_sub(Duration::from_millis(1));
        let out = debounce_step(Some(T0), false, now);
        assert!(
            !out.emit,
            "must not fire before the full quiet window elapses"
        );
        assert_eq!(out.next_last_event_at, Some(T0), "timer stays armed");
    }

    #[test]
    fn emits_and_disarms_exactly_at_the_window() {
        let now = T0 + RESIZE_DEBOUNCE;
        let out = debounce_step(Some(T0), false, now);
        assert!(out.emit, "settle fires once the debounce window elapses");
        assert!(
            out.next_last_event_at.is_none(),
            "and disarms so it fires exactly once per quiet period"
        );
    }

    #[test]
    fn does_not_re_emit_after_disarming() {
        // After a settle `last_event_at` is None; further quiet frames stay silent.
        let out = debounce_step(None, false, T0 + Duration::from_secs(10));
        assert!(!out.emit);
        assert!(out.next_last_event_at.is_none());
    }
}
