//! Sketch reload fade-overlay state machine.
//!
//! When a sketch needs to restart (e.g., `particle_density` changed via the
//! settings panel), this module mediates the `Line → Home → Line` round-trip so
//! the picker page is never visible and the transition is blacked-out with a
//! smooth audio fade.
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
//!   `phase == Idle`). The caller triggers `NextState::set(Line)` and advances
//!   to `FadeIn`, recording a fresh `started_at`.
//! - **`FadeIn`**: visual alpha lerps 1 → 0 over 200 ms; audio volume 0 → 1
//!   each frame. After 200 ms phase returns to `Idle` and volume is restored
//!   to `pre_fade_volume`.
//!
//! ## Data flow
//!
//! `restart_on_settings_change` (in `wc-sketches/src/line/mod.rs`):
//! - Previously: `next.set(Home)` + `insert_resource(LineRestartPending)`.
//! - Now: `reload_state.begin_fade_out(time, pre_fade_volume)` — stores
//!   current master volume and starts the `FadeOut` phase. The `drive_reload_state`
//!   system drives all subsequent phase transitions.
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
    /// One-frame pause in `Home` state while `NextState::Line` is armed.
    Switch,
    /// Fading back in from black + restoring audio. Duration: [`FADE_DURATION`].
    FadeIn,
}

/// Resource tracking the current sketch-reload overlay state.
///
/// Updated by `restart_on_settings_change` (in `wc-sketches`) and driven
/// frame-by-frame by [`drive_reload_state`].
#[derive(Resource, Debug, Default)]
pub struct SketchReloadState {
    /// Current phase of the reload transition.
    pub phase: ReloadPhase,
    /// `Time::elapsed()` at the start of the current phase.
    pub started_at: Duration,
    /// Master volume to restore after the reload completes.
    pub pre_fade_volume: f32,
}

impl SketchReloadState {
    /// Returns `true` when no reload is in progress (picker + normal UI should
    /// run).
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.phase == ReloadPhase::Idle
    }

    /// Start the `FadeOut` phase. Stores the current master volume so it can be
    /// restored at the end of `FadeIn`.
    ///
    /// Called by `restart_on_settings_change` instead of the old
    /// `NextState::Home` + `LineRestartPending` approach.
    pub fn begin_fade_out(&mut self, now: Duration, pre_fade_volume: f32) {
        self.phase = ReloadPhase::FadeOut;
        self.started_at = now;
        self.pre_fade_volume = pre_fade_volume;
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
    mut audio_cmd: Option<bevy::ecs::system::NonSendMut<'_, crate::audio::ring::AudioCommandSender>>,
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
            // One frame in Home; arm the re-entry into Line.
            next_app.set(super::state::AppState::Line);
            state.phase = ReloadPhase::FadeIn;
            state.started_at = now;
            tracing::debug!("reload overlay: Switch → FadeIn (Line)");
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
        assert_eq!(s.overlay_alpha(Duration::from_secs(0)), 0.0);
    }

    #[test]
    fn fade_out_ramps_zero_to_one() {
        let mut s = SketchReloadState::default();
        s.begin_fade_out(Duration::ZERO, 1.0);
        // At start of FadeOut: alpha should be 0.
        assert!(s.overlay_alpha(Duration::ZERO) < 0.01);
        // At end of FadeOut: alpha should be ≥ 1.
        assert!(s.overlay_alpha(FADE_DURATION) >= 1.0);
    }

    #[test]
    fn switch_alpha_is_one() {
        let mut s = SketchReloadState::default();
        s.phase = ReloadPhase::Switch;
        assert_eq!(s.overlay_alpha(Duration::from_secs(5)), 1.0);
    }

    #[test]
    fn fade_in_ramps_one_to_zero() {
        let mut s = SketchReloadState::default();
        s.phase = ReloadPhase::FadeIn;
        s.started_at = Duration::ZERO;
        // At start of FadeIn: alpha should be ~1.
        assert!(s.overlay_alpha(Duration::ZERO) > 0.99);
        // At end of FadeIn: alpha should be ≤ 0.
        assert!(s.overlay_alpha(FADE_DURATION) <= 0.0);
    }
}
