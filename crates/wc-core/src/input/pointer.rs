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
pub fn pointer_merge_system(
    windows: Query<'_, '_, &Window>,
    touches: Res<'_, Touches>,
    hands: Res<'_, HandTrackingState>,
    mut pointer: ResMut<'_, PointerState>,
) {
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

    // Mouse (window cursor position).
    if let Some(window) = windows.iter().next() {
        if let Some(pos) = window.cursor_position() {
            *pointer = PointerState {
                primary: Some(pos),
                source: PointerSource::Mouse,
            };
            return;
        }
    }

    // No source.
    *pointer = PointerState::default();
}
