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
    Radiance,
    Waves,
}

impl AppState {
    /// Stable ordering of the sketch variants, used by Next/Previous navigation.
    ///
    /// `Home` is not part of the cycle; it is the entry/exit point only.
    ///
    /// `Waves` is deliberately absent (`AUDIT.md` T5): the `AppState` variant
    /// and its [`SketchActivity`] `#[source]` wiring stay in place as a seam
    /// for a future sketch, but it has no implemented plugin or registered
    /// [`crate::sketch::SketchManifest`] entry yet, so cycling into it would
    /// land on a black screen. `Flame` re-entered the cycle in the
    /// 2026-07-02 flame port; `Radiance` entered it in the 2026-07-12
    /// Radiance plan. Once a real plugin exists for `Waves`, add it
    /// back here (the `sketch_order_entries_are_all_known_implemented_sketches`
    /// test in `tests/ui_picker.rs` guards against re-adding a placeholder by
    /// mistake).
    pub const SKETCH_ORDER: [Self; 5] = [
        Self::Line,
        Self::Flame,
        Self::Dots,
        Self::Cymatics,
        Self::Radiance,
    ];

    /// Whether this state represents an active sketch (i.e. not `Home`).
    #[must_use]
    pub fn is_sketch(self) -> bool {
        !matches!(self, Self::Home)
    }

    /// Parse a sketch name (case-insensitive, surrounding whitespace ignored)
    /// into its [`AppState`]. Returns `None` for unknown names, for `home`
    /// (Home is the implicit default, not a navigable sketch target here),
    /// and for `waves` — that `AppState` variant is a reserved seam
    /// (`AUDIT.md` T5) with no implemented sketch behind it yet, so parsing
    /// it intentionally fails closed rather than accepting a name that
    /// would boot straight into a black screen.
    ///
    /// Backs the `WAVECONDUCTOR_START_SKETCH` startup override (see the binary
    /// crate, which logs a warning and falls back to `Home` on `None`) and is
    /// a convenient name→state mapping for any future CLI/config plumbing.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "line" => Some(Self::Line),
            "flame" => Some(Self::Flame),
            "dots" => Some(Self::Dots),
            "cymatics" => Some(Self::Cymatics),
            "radiance" => Some(Self::Radiance),
            _ => None,
        }
    }

    /// The next sketch in the cycle; wraps around. Returns `Self::Line`
    /// when called on `Home`.
    ///
    /// The cycle is encoded as an exhaustive `match` so that any new
    /// `AppState` variant becomes a compile error here, instead of silently
    /// wrapping to the first sketch at runtime. The
    /// `next_prev_cycle_matches_sketch_order` test cross-checks the match
    /// arms against [`Self::SKETCH_ORDER`].
    ///
    /// `Waves` is unreachable from live input (not in [`Self::SKETCH_ORDER`],
    /// no picker tile, no key binding — `AUDIT.md` T5), so its arm below is
    /// dead in practice; it exists only so this remains a total function
    /// over every `AppState` variant. It is grouped into the live-cycle arm
    /// it would defensively fall through to (rather than looping to itself)
    /// in case a future dev-only entry point ever lands on it directly —
    /// alongside the wrap-around back to `Line`. Grouping it with `|`
    /// (instead of a separate identical-body arm) also keeps
    /// `clippy::match_same_arms` quiet.
    #[must_use]
    pub fn next_sketch(self) -> Self {
        match self {
            Self::Home | Self::Radiance | Self::Waves => Self::Line,
            Self::Line => Self::Flame,
            Self::Flame => Self::Dots,
            Self::Dots => Self::Cymatics,
            Self::Cymatics => Self::Radiance,
        }
    }

    /// The previous sketch in the cycle; wraps around. Returns the last
    /// sketch when called on `Home`.
    ///
    /// See [`Self::next_sketch`] for why this is an exhaustive `match` and
    /// why the `Waves` arm is unreachable-but-present and grouped with `|`
    /// into a live arm.
    #[must_use]
    pub fn prev_sketch(self) -> Self {
        match self {
            Self::Home | Self::Line | Self::Waves => Self::Radiance,
            Self::Flame => Self::Line,
            Self::Dots => Self::Flame,
            Self::Cymatics => Self::Dots,
            Self::Radiance => Self::Cymatics,
        }
    }
}

/// Whether the currently-active sketch is simulating, idle, or showing the
/// screensaver overlay. Only meaningful when [`AppState`] is a sketch (not
/// `Home`); the sub-state is gated to the sketch variants by Bevy.
#[derive(SubStates, Default, Clone, Eq, PartialEq, Hash, Debug)]
#[source(AppState = AppState::Line | AppState::Flame | AppState::Dots
                  | AppState::Cymatics | AppState::Radiance | AppState::Waves)]
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
        assert_eq!(AppState::Cymatics.next_sketch(), AppState::Radiance);
        assert_eq!(AppState::Radiance.next_sketch(), AppState::Line);
    }

    #[test]
    fn prev_sketch_wraps() {
        assert_eq!(AppState::Flame.prev_sketch(), AppState::Line);
        assert_eq!(AppState::Line.prev_sketch(), AppState::Radiance);
        assert_eq!(AppState::Radiance.prev_sketch(), AppState::Cymatics);
    }

    #[test]
    fn home_navigation_returns_to_endpoints() {
        assert_eq!(AppState::Home.next_sketch(), AppState::Line);
        assert_eq!(AppState::Home.prev_sketch(), AppState::Radiance);
    }

    /// Waves stays a de-routed seam (2026-07 audit T5); Radiance entered the
    /// cycle in the 2026-07-12 Radiance plan.
    #[test]
    fn waves_arms_are_present_but_unreachable_from_the_cycle() {
        assert!(AppState::SKETCH_ORDER.contains(&AppState::Radiance));
        assert!(!AppState::SKETCH_ORDER.contains(&AppState::Waves));
        assert_eq!(AppState::Waves.next_sketch(), AppState::Line);
        assert_eq!(AppState::Waves.prev_sketch(), AppState::Radiance);
    }

    #[test]
    fn is_sketch_excludes_home() {
        assert!(!AppState::Home.is_sketch());
        for s in AppState::SKETCH_ORDER {
            assert!(s.is_sketch(), "{s:?} should be a sketch");
        }
    }

    #[test]
    fn from_name_parses_every_sketch_case_insensitively() {
        assert_eq!(AppState::from_name("line"), Some(AppState::Line));
        assert_eq!(AppState::from_name("  DOTS  "), Some(AppState::Dots));
        assert_eq!(AppState::from_name("Cymatics"), Some(AppState::Cymatics));
        assert_eq!(AppState::from_name("Flame"), Some(AppState::Flame));
        assert_eq!(AppState::from_name("Radiance"), Some(AppState::Radiance));
        // Home, unknown names, and the reserved-but-unimplemented Waves seam
        // (AUDIT.md T5) all yield None — the caller (the binary's
        // WAVECONDUCTOR_START_SKETCH handling) warns and falls back to Home.
        assert_eq!(AppState::from_name("home"), None);
        assert_eq!(AppState::from_name("nope"), None);
        assert_eq!(AppState::from_name("waves"), None);
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
