//! Sketch reload fade-overlay state machine.
//!
//! When a sketch needs to restart (e.g., `particle_density` or `dot_spacing`
//! changed via the settings panel), this module mediates the `sketch → Home →
//! sketch` round-trip so the picker page is never visible and the transition is
//! blacked-out with a smooth audio fade. The sketch to return to is carried in
//! [`SketchReloadState::return_state`], so any sketch (not just Line) can drive it.
//!
//! ## Phases
//!
//! ```text
//!  Idle ──► FadeOut (200 ms) ──► Switch (1 frame) ──► FadeIn (200 ms) ──► Idle
//! ```
//!
//! - **Idle**: overlay alpha = 0; no effect on sketch or picker.
//! - **`FadeOut`**: visual alpha lerps 0 → 1 over 200 ms; audio volume 1 → 0
//!   each frame via `AudioCommand::SetMasterVolume`. After 200 ms the caller
//!   triggers `NextState::set(Home)` and advances to `Switch`.
//! - **Switch**: one frame in `Home` state. The picker is hidden (gated on
//!   `phase == Idle`). The driver triggers `NextState::set(return_state)` (the
//!   sketch that requested the restart) and advances to `FadeIn`, recording a
//!   fresh `started_at`.
//! - **`FadeIn`**: visual alpha lerps 1 → 0 over 200 ms; audio volume 0 → 1
//!   each frame. After 200 ms phase returns to `Idle` and volume is restored
//!   to `pre_fade_volume`.
//!
//! ## Data flow
//!
//! Each sketch's `restart_on_*_settings_change` listener (e.g.
//! `restart_on_settings_change` in `wc-sketches/src/line/mod.rs`,
//! `restart_on_dots_settings_change` in `wc-sketches/src/dots/mod.rs`):
//! - Calls `reload_state.begin_fade_out(time, pre_fade_volume, return_state)` —
//!   stores the current master volume and the sketch to return to, and starts
//!   the `FadeOut` phase. The `drive_reload_state` system drives all subsequent
//!   phase transitions.
//!
//! `drive_reload_state` (registered by [`ReloadOverlayPlugin`]):
//! - Runs each `Update` frame.
//! - Advances phase transitions, sets `NextState<AppState>` as needed, and
//!   pushes `AudioCommand::SetMasterVolume` continuously during fade phases.
//!
//! `draw_reload_overlay` (in `wc-core/src/ui/reload_overlay.rs`):
//! - Runs in `EguiPrimaryContextPass`; paints a full-screen opaque black `Area`
//!   scaled by the current alpha. Alpha = 0 when Idle, so no paint occurs.

use std::time::Duration;

use bevy::prelude::*;

use super::state::AppState;

/// Duration of each fade leg (`FadeOut` and `FadeIn`).
pub const FADE_DURATION: Duration = Duration::from_millis(200);

/// Phase of the sketch reload transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReloadPhase {
    /// No reload in progress; overlay is transparent.
    #[default]
    Idle,
    /// Fading to black + silencing audio. Duration: [`FADE_DURATION`].
    FadeOut,
    /// One-frame pause in `Home` state while `NextState(return_state)` is armed.
    Switch,
    /// Fading back in from black + restoring audio. Duration: [`FADE_DURATION`].
    FadeIn,
}

/// Resource tracking the current sketch-reload overlay state.
///
/// Updated by each sketch's `restart_on_*_settings_change` system (in
/// `wc-sketches`) and driven frame-by-frame by [`drive_reload_state`].
#[derive(Resource, Debug, Default)]
pub struct SketchReloadState {
    /// Current phase of the reload transition.
    pub phase: ReloadPhase,
    /// `Time::elapsed()` at the start of the current phase.
    pub started_at: Duration,
    /// Master volume to restore after the reload completes.
    pub pre_fade_volume: f32,
    /// The [`AppState`] to navigate back to after the `Switch` phase completes.
    ///
    /// Set by [`SketchReloadState::begin_fade_out`] before `FadeOut` starts;
    /// consumed by [`drive_reload_state`] in the `Switch` phase. Defaults to
    /// [`AppState::Home`] but is always overwritten before `Switch` fires.
    pub return_state: AppState,
}

impl SketchReloadState {
    /// Returns `true` when no reload is in progress (picker + normal UI should
    /// run).
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.phase == ReloadPhase::Idle
    }

    /// Start the `FadeOut` phase. Stores the current master volume and the
    /// sketch state to return to after the cycle completes.
    ///
    /// Called by each sketch's `restart_on_*_settings_change` system instead
    /// of the old `NextState::Home` + `*RestartPending` approach.
    ///
    /// `return_state` must be the currently active sketch state (e.g.
    /// `AppState::Line`, `AppState::Dots`) so that [`drive_reload_state`]
    /// navigates back to the correct sketch in its `Switch` phase.
    pub fn begin_fade_out(&mut self, now: Duration, pre_fade_volume: f32, return_state: AppState) {
        self.phase = ReloadPhase::FadeOut;
        self.started_at = now;
        self.pre_fade_volume = pre_fade_volume;
        self.return_state = return_state;
    }

    /// Compute the current overlay alpha (0.0 = transparent, 1.0 = opaque).
    ///
    /// Returns 0.0 during `Idle`; linearly ramps 0→1 during `FadeOut`; holds
    /// at 1.0 during `Switch`; linearly ramps 1→0 during `FadeIn`.
    #[must_use]
    pub fn overlay_alpha(&self, now: Duration) -> f32 {
        let fade_secs = FADE_DURATION.as_secs_f32();
        match self.phase {
            ReloadPhase::Idle => 0.0,
            ReloadPhase::FadeOut => {
                let t = now.saturating_sub(self.started_at).as_secs_f32() / fade_secs;
                t.clamp(0.0, 1.0)
            }
            ReloadPhase::Switch => 1.0,
            ReloadPhase::FadeIn => {
                let t = now.saturating_sub(self.started_at).as_secs_f32() / fade_secs;
                (1.0 - t).clamp(0.0, 1.0)
            }
        }
    }
}

/// System: drives phase transitions and audio fades for the reload overlay.
///
/// Runs each `Update` frame regardless of `AppState`. No-ops instantly when
/// `phase == Idle` so there is zero overhead during normal operation.
pub fn drive_reload_state(
    mut state: ResMut<'_, SketchReloadState>,
    time: Res<'_, Time>,
    mut next_app: ResMut<'_, NextState<super::state::AppState>>,
    mut audio_cmd: Option<
        bevy::ecs::system::NonSendMut<'_, crate::audio::ring::AudioCommandSender>,
    >,
) {
    let now = time.elapsed();
    let alpha = state.overlay_alpha(now);

    // Push smooth audio fade every frame during active fade phases.
    // Volume is the inverse of the overlay alpha: full when transparent, silent
    // when fully opaque.
    if matches!(state.phase, ReloadPhase::FadeOut | ReloadPhase::FadeIn) {
        if let Some(ref mut sender) = audio_cmd {
            let _ = sender.push(crate::audio::command::AudioCommand::SetMasterVolume(
                1.0 - alpha,
            ));
        }
    }

    match state.phase {
        ReloadPhase::Idle => {
            // Nothing to drive.
        }
        ReloadPhase::FadeOut => {
            if now.saturating_sub(state.started_at) >= FADE_DURATION {
                // FadeOut complete — switch to Home so the sketch exits cleanly.
                next_app.set(super::state::AppState::Home);
                state.phase = ReloadPhase::Switch;
                tracing::debug!("reload overlay: FadeOut complete → Switch (Home)");
            }
        }
        ReloadPhase::Switch => {
            // One frame in Home; arm the re-entry into the sketch that
            // triggered the restart (set by `begin_fade_out`).
            let return_to = state.return_state;
            next_app.set(return_to);
            state.phase = ReloadPhase::FadeIn;
            state.started_at = now;
            tracing::debug!("reload overlay: Switch → FadeIn ({:?})", return_to);
        }
        ReloadPhase::FadeIn => {
            if now.saturating_sub(state.started_at) >= FADE_DURATION {
                // FadeIn complete — restore volume and return to Idle.
                let restore_vol = state.pre_fade_volume;
                state.phase = ReloadPhase::Idle;
                // Ensure volume is fully restored even if the last per-frame push
                // was slightly below 1.0 due to floating-point timing.
                if let Some(ref mut sender) = audio_cmd {
                    let _ = sender.push(crate::audio::command::AudioCommand::SetMasterVolume(
                        restore_vol,
                    ));
                }
                tracing::debug!("reload overlay: FadeIn complete → Idle");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_alpha_is_zero() {
        let s = SketchReloadState::default();
        // Idle phase produces the constant 0.0 — strict equality is the
        // right shape, matching the `switch_alpha_is_one` test below.
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(s.overlay_alpha(Duration::from_secs(0)), 0.0);
        }
    }

    #[test]
    fn fade_out_ramps_zero_to_one() {
        let mut s = SketchReloadState::default();
        s.begin_fade_out(Duration::ZERO, 1.0, AppState::Line);
        // At start of FadeOut: alpha should be 0.
        assert!(s.overlay_alpha(Duration::ZERO) < 0.01);
        // At end of FadeOut: alpha should be ≥ 1.
        assert!(s.overlay_alpha(FADE_DURATION) >= 1.0);
    }

    #[test]
    fn switch_alpha_is_one() {
        let s = SketchReloadState {
            phase: ReloadPhase::Switch,
            ..Default::default()
        };
        // During Switch, the overlay sits at full opacity regardless of
        // elapsed time — `overlay_alpha` returns the constant 1.0, so a
        // strict equality check is the right shape here.
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(s.overlay_alpha(Duration::from_secs(5)), 1.0);
        }
    }

    #[test]
    fn fade_in_ramps_one_to_zero() {
        let s = SketchReloadState {
            phase: ReloadPhase::FadeIn,
            started_at: Duration::ZERO,
            ..Default::default()
        };
        // At start of FadeIn: alpha should be ~1.
        assert!(s.overlay_alpha(Duration::ZERO) > 0.99);
        // At end of FadeIn: alpha should be ≤ 0.
        assert!(s.overlay_alpha(FADE_DURATION) <= 0.0);
    }
}
