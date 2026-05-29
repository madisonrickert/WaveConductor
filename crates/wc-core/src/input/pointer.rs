//! Optional unified pointer.
//!
//! For sketches that want a single "wherever the user is pointing" stream
//! across mouse, touch, and hand-tracking, this module merges those sources
//! into a single [`PointerState`] resource. Sketches that only care about
//! mouse can ignore this and read Bevy's `window.cursor_position()` directly.

use bevy::prelude::*;
use bevy::reflect::Reflect;

use super::projection::palm_to_world;
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
    ///
    /// Unified across all sources with priority hand > touch > mouse — for
    /// sketches that want a single "wherever the user is pointing" stream.
    pub primary: Option<Vec2>,
    /// Which source produced [`Self::primary`].
    pub source: PointerSource,
    /// Mouse/touch pointer position **excluding hand-tracking**, in window
    /// logical coordinates, or `None`.
    ///
    /// This is the cursor a user drives with a mouse or finger, independent of
    /// any tracked hand. Sketches that run a hand attractor *and* a separate
    /// pointer attractor (e.g. Line) read this so the two stay independent —
    /// otherwise a tracked hand would hijack [`Self::primary`] and drag the
    /// mouse attractor onto the hand.
    pub cursor: Option<Vec2>,
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
    let window = windows.iter().next();

    // --- Mouse/touch cursor (hand-independent) -------------------------------
    // Touch beats mouse. Mouse uses the latest `CursorMoved` this tick (so
    // synthetic-event tests work end-to-end) and falls back to
    // `Window::cursor_position()` (winit's persistent state) in production.
    let touch_position = touches.iter().next().map(bevy::input::touch::Touch::position);
    let mouse_position =
        cursor_msg_position.or_else(|| window.and_then(Window::cursor_position));
    let cursor = touch_position.or(mouse_position);

    // --- Unified primary: hand > touch > mouse -------------------------------
    let (primary, source) = if let (Some(hand), Some(window)) =
        (hands.right().or_else(|| hands.left()), window)
    {
        // The index-fingertip landmark is in Leap device millimetres (same
        // convention as `palm_to_world` and the bone-mesh projection) — NOT
        // NDC. Project it through `palm_to_world` (mm → centered world, +y up),
        // then convert to window-logical coords (top-left origin, +y down).
        // (The earlier code treated the mm landmark as NDC and produced wildly
        // out-of-window positions — e.g. a fingertip at -56 mm mapped to
        // x ≈ -35200 px — which corrupted any consumer of the pointer.)
        let landmark = hand.landmark(super::hand::LandmarkIndex::IndexTip);
        let size = Vec2::new(window.width(), window.height());
        let world = palm_to_world(landmark, size);
        let x = world.x + size.x * 0.5;
        let y = size.y * 0.5 - world.y;
        (Some(Vec2::new(x, y)), PointerSource::Hand)
    } else if let Some(t) = touch_position {
        (Some(t), PointerSource::Touch)
    } else if let Some(m) = mouse_position {
        (Some(m), PointerSource::Mouse)
    } else {
        (None, PointerSource::None)
    };

    *pointer = PointerState {
        primary,
        source,
        cursor,
    };
}
