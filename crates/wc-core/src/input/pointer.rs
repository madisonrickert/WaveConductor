//! Optional unified pointer.
//!
//! For sketches that want a single "wherever the user is pointing" stream
//! across mouse, touch, and hand-tracking, this module merges those sources
//! into a single [`PointerState`] resource. Sketches that only care about
//! mouse can ignore this and read Bevy's `window.cursor_position()` directly.

use bevy::prelude::*;
use bevy::reflect::Reflect;

use super::state::HandTrackingState;

/// Where the user is pointing, normalized to window logical coordinates
/// (top-left origin, +x right, +y down — matching `window.cursor_position()`).
///
/// `None` when no input source is providing a pointer (no mouse motion, no
/// active touches, no tracked hand). Sketches that need a specific source
/// (e.g., "only mouse") should read Bevy's native resources directly.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Reflect)]
pub struct PointerState {
    /// Primary pointer position in window logical coordinates, or `None`.
    pub primary: Option<Vec2>,
    /// Which source produced [`Self::primary`].
    pub source: PointerSource,
}

/// Which input source produced the [`PointerState::primary`] value this frame.
#[derive(Debug, Clone, Copy, PartialEq, Default, Reflect)]
pub enum PointerSource {
    /// No source currently producing a pointer.
    #[default]
    None,
    /// Mouse cursor.
    Mouse,
    /// First active touch.
    Touch,
    /// Projected hand-tracking landmark (typically index fingertip).
    Hand,
}

/// Reads mouse, touch, and hand-tracking sources and writes the unified
/// [`PointerState`].
///
/// Source priority when multiple are active: hand-tracking > touch > mouse.
/// Rationale: in `WaveConductor`, hand-tracking is the "premium" input — if a
/// user has both their hand in the tracking volume and a mouse on the table,
/// they almost certainly mean to use the hand.
///
/// The mouse source resolves in two stages: the latest `CursorMoved` message
/// this tick wins (so synthetic-event integration tests are end-to-end),
/// falling back to `Window::cursor_position()` (winit's persistent cursor
/// state) when no message arrived this tick. In production both update in
/// lockstep — winit writes the window cursor position *and* fires a
/// `CursorMoved` message each frame the cursor moves — so "latest known"
/// remains correct regardless of which path supplies it.
pub fn pointer_merge_system(
    windows: Query<'_, '_, &Window>,
    touches: Res<'_, Touches>,
    hands: Res<'_, HandTrackingState>,
    mut cursor_moved_reader: MessageReader<'_, '_, bevy::window::CursorMoved>,
    mut pointer: ResMut<'_, PointerState>,
) {
    // Drain pending CursorMoved messages once, up front — even if a hand
    // source ends up winning the merge below. Leaving them in the channel
    // would accumulate stale positions for the next tick.
    // Newest-wins: `last()` discards intermediate positions intentionally — we
    // want the pointer's *current* location, not its motion path. Sketches that
    // need motion deltas should read `CursorMoved` events directly instead of
    // going through PointerState.
    let cursor_msg_position: Option<Vec2> = cursor_moved_reader.read().last().map(|c| c.position);

    // Hand wins if any hand is present. Use the right-hand index-finger tip
    // if available, otherwise the left.
    if let Some(hand) = hands.right().or_else(|| hands.left()) {
        // Project landmark NDC (x in [-1, 1], y in [-1, 1] with +y up) onto
        // the primary window's logical size, with +y down.
        if let Some(window) = windows.iter().next() {
            let landmark = hand.landmark(super::hand::LandmarkIndex::IndexTip);
            let w = window.width();
            let h = window.height();
            let x = (landmark.x * 0.5 + 0.5) * w;
            let y = (1.0 - (landmark.y * 0.5 + 0.5)) * h;
            *pointer = PointerState {
                primary: Some(Vec2::new(x, y)),
                source: PointerSource::Hand,
            };
            return;
        }
    }

    // Touch (any active touch).
    if let Some(touch) = touches.iter().next() {
        *pointer = PointerState {
            primary: Some(touch.position()),
            source: PointerSource::Touch,
        };
        return;
    }

    // Mouse — latest `CursorMoved` this tick beats `Window::cursor_position()`
    // (which only updates in production via winit, not in tests). Either path
    // produces the same value during normal operation; the message path is
    // what makes synthetic-event tests work end-to-end.
    let mouse_position =
        cursor_msg_position.or_else(|| windows.iter().next().and_then(Window::cursor_position));
    if let Some(pos) = mouse_position {
        *pointer = PointerState {
            primary: Some(pos),
            source: PointerSource::Mouse,
        };
        return;
    }

    // No source.
    *pointer = PointerState::default();
}
