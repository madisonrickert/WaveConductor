//! Sketch reload fade-overlay state machine.
//!
//! When a sketch needs to restart (e.g., `particle_density` or `dot_spacing`
//! changed via the settings panel, or a settled window resize), this module
//! mediates the `sketch → Home → sketch` round-trip so the picker page is never
//! visible. The [`ReloadReason`] passed to `begin_fade_out` selects the fade
//! profile: a settings restart blacks out over [`FADE_DURATION`] with a smooth
//! audio fade; a window resize is instant and silent (see the private
//! `fade_duration` / `fades_audio` mappings in this module). The sketch to
//! return to is carried in
//! [`SketchReloadState::return_state`], so any sketch (not just Line) can drive it.
//!
//! ## Phases
//!
//! ```text
//!  Idle ──► FadeOut (0 or FADE_DURATION) ──► Switch (1 frame) ──► FadeIn (0 or FADE_DURATION) ──► Idle
//! ```
//!
//! - **Idle**: overlay alpha = 0; no effect on sketch or picker.
//! - **`FadeOut`**: visual alpha lerps 0 → 1 over the reason's fade duration;
//!   audio volume 1 → 0 each frame via `AudioCommand::SetMasterVolume` when the
//!   reason fades audio. Once the leg elapses the caller triggers
//!   `NextState::set(Home)` and advances to `Switch`.
//! - **Switch**: one frame in `Home` state. The picker is hidden (gated on
//!   `phase == Idle`). The driver triggers `NextState::set(return_state)` (the
//!   sketch that requested the restart) and advances to `FadeIn`, recording a
//!   fresh `started_at`.
//! - **`FadeIn`**: visual alpha lerps 1 → 0 over the reason's fade duration;
//!   audio volume 0 → 1 each frame when the reason fades audio. Once the leg
//!   elapses, phase returns to `Idle` and volume is restored to
//!   `pre_fade_volume` (only for reasons that fade audio).
//!
//! ## Data flow
//!
//! Each sketch's `restart_on_*_settings_change` listener (e.g.
//! `restart_on_settings_change` in `wc-sketches/src/line/mod.rs`,
//! `restart_on_dots_settings_change` in `wc-sketches/src/dots/mod.rs`):
//! - Calls `reload_state.begin_fade_out(time, pre_fade_volume, return_state, reason)`
//!   — stores the current master volume, the sketch to return to, and the
//!   [`ReloadReason`], and starts the `FadeOut` phase. The `drive_reload_state`
//!   system drives all subsequent phase transitions.
//!
//! `drive_reload_state` (registered by `ReloadOverlayPlugin`):
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

/// Why a reload was requested. Selects the fade profile.
///
/// A settings restart fades to black and dips audio over [`FADE_DURATION`]; a
/// window resize is silent and instant (one black repaint frame nobody sees, no
/// audio command), because the reload exists only to re-run the sketch's spawn
/// path at the new extent — there is nothing to fade, and a kiosk waking from
/// sleep must not have its sound cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReloadReason {
    /// A `requires_restart` settings change. The historical behaviour: 200 ms
    /// fade out, audio dip to silence, one `Home` frame, 200 ms fade in.
    #[default]
    SettingsRestart,
    /// A settled window resize (F11, monitor re-enumeration, startup scale
    /// settle). Instant and silent.
    WindowResize,
}

/// Fade-leg duration for a reload `reason`: [`FADE_DURATION`] for a settings
/// restart, [`Duration::ZERO`] for a window resize (which advances on the first
/// frame). Pure so the mapping is unit-testable without an app.
fn fade_duration(reason: ReloadReason) -> Duration {
    match reason {
        ReloadReason::SettingsRestart => FADE_DURATION,
        ReloadReason::WindowResize => Duration::ZERO,
    }
}

/// Whether a reload `reason` fades the master volume. A window resize must not
/// touch audio (a kiosk waking from sleep would otherwise cut its sound). Pure
/// so the mapping is unit-testable without an app.
fn fades_audio(reason: ReloadReason) -> bool {
    match reason {
        ReloadReason::SettingsRestart => true,
        ReloadReason::WindowResize => false,
    }
}

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
    /// Why this reload was requested; selects the fade profile (see
    /// [`ReloadReason`]). Defaults to [`ReloadReason::SettingsRestart`], the
    /// historical behaviour.
    pub reason: ReloadReason,
}

impl SketchReloadState {
    /// Returns `true` when no reload is in progress (picker + normal UI should
    /// run).
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.phase == ReloadPhase::Idle
    }

    /// Start the `FadeOut` phase. Stores the current master volume, the
    /// sketch state to return to, and the [`ReloadReason`] that selects the
    /// fade profile (a settings restart fades + dips audio; a window resize
    /// is instant and silent).
    ///
    /// Called by each sketch's `restart_on_*_settings_change` system instead
    /// of the old `NextState::Home` + `*RestartPending` approach.
    ///
    /// `return_state` must be the currently active sketch state (e.g.
    /// `AppState::Line`, `AppState::Dots`) so that [`drive_reload_state`]
    /// navigates back to the correct sketch in its `Switch` phase.
    pub fn begin_fade_out(
        &mut self,
        now: Duration,
        pre_fade_volume: f32,
        return_state: AppState,
        reason: ReloadReason,
    ) {
        self.phase = ReloadPhase::FadeOut;
        self.started_at = now;
        self.pre_fade_volume = pre_fade_volume;
        self.return_state = return_state;
        self.reason = reason;
    }

    /// Compute the current overlay alpha (0.0 = transparent, 1.0 = opaque).
    ///
    /// Returns 0.0 during `Idle`; ramps 0→1 during `FadeOut`; holds at 1.0
    /// during `Switch`; ramps 1→0 during `FadeIn`. The ramp duration is the
    /// private `fade_duration` of [`Self::reason`]; a zero-length fade (a
    /// window resize) returns the terminal alpha directly, avoiding a
    /// `0.0 / 0.0` NaN.
    #[must_use]
    pub fn overlay_alpha(&self, now: Duration) -> f32 {
        let fade_secs = fade_duration(self.reason).as_secs_f32();
        match self.phase {
            ReloadPhase::Idle => 0.0,
            ReloadPhase::FadeOut => {
                // Zero-duration (WindowResize): fully opaque at once. Guard the
                // divide — `elapsed / 0.0` is NaN at elapsed 0 and NaN.clamp is
                // NaN. `drive_reload_state` advances the phase on the first
                // frame, so this is a single black repaint frame nobody sees.
                if fade_secs <= 0.0 {
                    return 1.0;
                }
                let t = now.saturating_sub(self.started_at).as_secs_f32() / fade_secs;
                t.clamp(0.0, 1.0)
            }
            ReloadPhase::Switch => 1.0,
            ReloadPhase::FadeIn => {
                // Zero-duration: already fully transparent (same NaN guard).
                if fade_secs <= 0.0 {
                    return 0.0;
                }
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
    let reason = state.reason;
    // Per-leg duration for this reason: FADE_DURATION for a settings restart,
    // ZERO for a window resize (which then advances on the first frame below).
    let fade = fade_duration(reason);
    let alpha = state.overlay_alpha(now);

    // Push the audio fade only for reasons that fade audio (a window resize does
    // not touch the master volume). Volume is the inverse of the overlay alpha:
    // full when transparent, silent when fully opaque.
    if fades_audio(reason) && matches!(state.phase, ReloadPhase::FadeOut | ReloadPhase::FadeIn) {
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
            if now.saturating_sub(state.started_at) >= fade {
                // FadeOut complete — switch to Home so the sketch exits cleanly.
                next_app.set(super::state::AppState::Home);
                state.phase = ReloadPhase::Switch;
                tracing::debug!("reload overlay: FadeOut complete → Switch (Home)");
            }
        }
        ReloadPhase::Switch => {
            // One frame in Home; arm the re-entry into the sketch that
            // triggered the reload (set by `begin_fade_out`).
            let return_to = state.return_state;
            next_app.set(return_to);
            state.phase = ReloadPhase::FadeIn;
            state.started_at = now;
            tracing::debug!("reload overlay: Switch → FadeIn ({:?})", return_to);
        }
        ReloadPhase::FadeIn => {
            if now.saturating_sub(state.started_at) >= fade {
                // FadeIn complete — restore volume and return to Idle.
                let restore_vol = state.pre_fade_volume;
                state.phase = ReloadPhase::Idle;
                // Restore volume only for reasons that dipped it; a window
                // resize never issued a volume command, so it issues none here.
                if fades_audio(reason) {
                    if let Some(ref mut sender) = audio_cmd {
                        let _ = sender.push(crate::audio::command::AudioCommand::SetMasterVolume(
                            restore_vol,
                        ));
                    }
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
        s.begin_fade_out(
            Duration::ZERO,
            1.0,
            AppState::Line,
            ReloadReason::SettingsRestart,
        );
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

    #[test]
    fn settings_restart_fades_over_the_full_duration_and_dips_audio() {
        assert_eq!(fade_duration(ReloadReason::SettingsRestart), FADE_DURATION);
        assert!(fades_audio(ReloadReason::SettingsRestart));
    }

    #[test]
    fn window_resize_is_instant_and_silent() {
        assert_eq!(fade_duration(ReloadReason::WindowResize), Duration::ZERO);
        assert!(!fades_audio(ReloadReason::WindowResize));
    }

    #[test]
    fn zero_duration_reload_gives_terminal_alpha_without_nan() {
        // A WindowResize reason has a zero-length fade. `overlay_alpha` must
        // return the terminal opacity (opaque in FadeOut, transparent in
        // FadeIn) rather than NaN from a divide-by-zero.
        let fade_out = SketchReloadState {
            phase: ReloadPhase::FadeOut,
            reason: ReloadReason::WindowResize,
            ..Default::default()
        };
        let a = fade_out.overlay_alpha(Duration::ZERO);
        assert!(a.is_finite(), "alpha must not be NaN");
        assert!(
            a > 0.99,
            "FadeOut with zero duration is fully opaque, got {a}"
        );

        let fade_in = SketchReloadState {
            phase: ReloadPhase::FadeIn,
            reason: ReloadReason::WindowResize,
            started_at: Duration::ZERO,
            ..Default::default()
        };
        let b = fade_in.overlay_alpha(Duration::ZERO);
        assert!(b.is_finite(), "alpha must not be NaN");
        assert!(
            b < 0.01,
            "FadeIn with zero duration is fully transparent, got {b}"
        );
    }
}
