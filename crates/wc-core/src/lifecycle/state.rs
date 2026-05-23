//! Top-level app navigation [`AppState`] and the sketch-active [`SketchActivity`]
//! sub-state. The state machine sits at the heart of the lifecycle plugin and
//! gates every sketch's update systems.

use bevy::prelude::*;

/// Which top-level scene is active.
///
/// The home screen is the default; selecting a sketch transitions out of `Home`
/// and into the matching variant. Pressing Escape returns to `Home`.
#[derive(States, Default, Clone, Copy, Eq, PartialEq, Hash, Debug)]
#[allow(missing_docs, reason = "variant names are self-documenting")]
pub enum AppState {
    #[default]
    Home,
    Line,
    Flame,
    Dots,
    Cymatics,
    Waves,
}

impl AppState {
    /// Stable ordering of the sketch variants, used by Next/Previous navigation.
    ///
    /// `Home` is not part of the cycle; it is the entry/exit point only.
    pub const SKETCH_ORDER: [Self; 5] = [
        Self::Line,
        Self::Flame,
        Self::Dots,
        Self::Cymatics,
        Self::Waves,
    ];

    /// Whether this state represents an active sketch (i.e. not `Home`).
    #[must_use]
    pub fn is_sketch(self) -> bool {
        !matches!(self, Self::Home)
    }

    /// The next sketch in [`Self::SKETCH_ORDER`]; wraps around. Returns `Self::Line`
    /// when called on `Home`.
    #[must_use]
    pub fn next_sketch(self) -> Self {
        if self == Self::Home {
            return Self::SKETCH_ORDER[0];
        }
        let idx = Self::SKETCH_ORDER
            .iter()
            .position(|s| *s == self)
            .unwrap_or_else(|| {
                debug_assert!(
                    false,
                    "AppState variant {self:?} is missing from SKETCH_ORDER"
                );
                0
            });
        Self::SKETCH_ORDER[(idx + 1) % Self::SKETCH_ORDER.len()]
    }

    /// The previous sketch in [`Self::SKETCH_ORDER`]; wraps around. Returns the last
    /// sketch when called on `Home`.
    #[must_use]
    pub fn prev_sketch(self) -> Self {
        if self == Self::Home {
            return Self::SKETCH_ORDER[Self::SKETCH_ORDER.len() - 1];
        }
        let idx = Self::SKETCH_ORDER
            .iter()
            .position(|s| *s == self)
            .unwrap_or_else(|| {
                debug_assert!(
                    false,
                    "AppState variant {self:?} is missing from SKETCH_ORDER"
                );
                0
            });
        let len = Self::SKETCH_ORDER.len();
        Self::SKETCH_ORDER[(idx + len - 1) % len]
    }
}

/// Whether the currently-active sketch is simulating, idle, or showing the
/// screensaver overlay. Only meaningful when [`AppState`] is a sketch (not
/// `Home`); the sub-state is gated to the sketch variants by Bevy.
#[derive(SubStates, Default, Clone, Eq, PartialEq, Hash, Debug)]
#[source(AppState = AppState::Line | AppState::Flame | AppState::Dots
                  | AppState::Cymatics | AppState::Waves)]
#[allow(missing_docs, reason = "variant names are self-documenting")]
pub enum SketchActivity {
    #[default]
    Active,
    Idle,
    Screensaver,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_sketch_wraps() {
        assert_eq!(AppState::Line.next_sketch(), AppState::Flame);
        assert_eq!(AppState::Waves.next_sketch(), AppState::Line);
    }

    #[test]
    fn prev_sketch_wraps() {
        assert_eq!(AppState::Flame.prev_sketch(), AppState::Line);
        assert_eq!(AppState::Line.prev_sketch(), AppState::Waves);
    }

    #[test]
    fn home_navigation_returns_to_endpoints() {
        assert_eq!(AppState::Home.next_sketch(), AppState::Line);
        assert_eq!(AppState::Home.prev_sketch(), AppState::Waves);
    }

    #[test]
    fn is_sketch_excludes_home() {
        assert!(!AppState::Home.is_sketch());
        for s in AppState::SKETCH_ORDER {
            assert!(s.is_sketch(), "{s:?} should be a sketch");
        }
    }
}
