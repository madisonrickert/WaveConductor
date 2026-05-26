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

    /// The next sketch in the cycle; wraps around. Returns `Self::Line`
    /// when called on `Home`.
    ///
    /// The cycle is encoded as an exhaustive `match` so that any new
    /// `AppState` variant becomes a compile error here, instead of silently
    /// wrapping to the first sketch at runtime. The
    /// `next_prev_cycle_matches_sketch_order` test cross-checks the match
    /// arms against [`Self::SKETCH_ORDER`].
    #[must_use]
    pub fn next_sketch(self) -> Self {
        match self {
            Self::Home | Self::Waves => Self::Line,
            Self::Line => Self::Flame,
            Self::Flame => Self::Dots,
            Self::Dots => Self::Cymatics,
            Self::Cymatics => Self::Waves,
        }
    }

    /// The previous sketch in the cycle; wraps around. Returns the last
    /// sketch when called on `Home`.
    ///
    /// See [`Self::next_sketch`] for why this is an exhaustive `match`.
    #[must_use]
    pub fn prev_sketch(self) -> Self {
        match self {
            Self::Home | Self::Line => Self::Waves,
            Self::Flame => Self::Line,
            Self::Dots => Self::Flame,
            Self::Cymatics => Self::Dots,
            Self::Waves => Self::Cymatics,
        }
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

    /// Guard against drift between the [`AppState::SKETCH_ORDER`] iteration
    /// constant and the `next_sketch`/`prev_sketch` match arms. If a new
    /// sketch variant is added, both the const array and the match arms
    /// must be updated; this test verifies they agree.
    #[test]
    fn next_prev_cycle_matches_sketch_order() {
        let order = AppState::SKETCH_ORDER;
        for (i, &state) in order.iter().enumerate() {
            let expected_next = order[(i + 1) % order.len()];
            let expected_prev = order[(i + order.len() - 1) % order.len()];
            assert_eq!(
                state.next_sketch(),
                expected_next,
                "next_sketch arm for {state:?} drifted from SKETCH_ORDER"
            );
            assert_eq!(
                state.prev_sketch(),
                expected_prev,
                "prev_sketch arm for {state:?} drifted from SKETCH_ORDER"
            );
        }
    }
}
