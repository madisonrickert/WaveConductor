//! Sketch reload fade-overlay state machine.
//!
//! When a sketch needs to restart (e.g., `particle_density` or `dot_spacing`
//! changed via the settings panel, or a settled window resize), or when the
//! user navigates from one sketch to another (or to/from `Home`), this module
//! mediates the `sketch → Home → sketch` round-trip so the picker page is never
//! visible mid-transition and the swap itself is masked by a moment of black.
//! The [`ReloadReason`] passed to `begin_fade_out` selects the fade profile: a
//! settings restart blacks out over [`FADE_DURATION`] with a smooth audio
//! fade; a sketch-to-sketch switch blacks out over the longer
//! [`SKETCH_SWITCH_FADE_DURATION`], also with an audio fade; a window resize
//! or an audio-device reconnect is instant and silent (see the private
//! `fade_duration` / `fades_audio` mappings in this module). The sketch to
//! return to is carried in
//! [`SketchReloadState::return_state`], so any sketch (not just Line) can drive it.
//!
//! ## Phases
//!
//! ```text
//!  Idle ──► FadeOut (0, FADE_DURATION, or SKETCH_SWITCH_FADE_DURATION) ──► Switch (1 frame) ──► FadeIn (same duration as FadeOut) ──► Idle
//! ```
//!
//! - **Idle**: overlay alpha = 0; no effect on sketch or picker.
//! - **`FadeOut`**: visual alpha lerps 0 → 1 over the reason's fade duration;
//!   audio volume 1 → 0 each frame via `AudioCommand::SetMasterVolume` when the
//!   reason fades audio. Once the leg elapses the caller triggers
//!   `NextState::set(Home)` and advances to `Switch` — **unless the app is
//!   already at `Home`** (e.g. a picker click or a number-key select made
//!   while sitting on the picker page), in which case the `Home` hop is
//!   skipped: we are already where the hop would have taken us, and
//!   `NextState::set` on the *same* state Bevy is already in still re-fires
//!   `OnExit`/`OnEnter` (see the note on `Switch` below), so writing it
//!   unconditionally would burn a redundant `Home` teardown/rebuild for no
//!   visible effect.
//! - **Switch**: one frame in `Home` state (or zero additional frames, per the
//!   note above, when the reload started at `Home`). The picker is hidden
//!   (gated on `phase == Idle`). The driver triggers
//!   `NextState::set(return_state)` (the sketch, or `Home`, that the caller
//!   asked to land on) and advances to `FadeIn`, recording a fresh
//!   `started_at` — again **unless `return_state` is where the app already
//!   is** (the symmetric case: `NavigateHome` pressed from a sketch already
//!   delivered us to `Home` via the `FadeOut` hop above, so re-arming
//!   `NextState::set(Home)` here would double-fire `Home`'s `OnExit`/`OnEnter`).
//!   Bevy's `NextState::set` always uses the `Pending` variant, which sets
//!   `allow_same_state_transitions = true` — unlike `set_if_neq`, it does
//!   **not** skip `OnExit`/`OnEnter` on its own when entered == exited, which
//!   is exactly why the two guards above exist: this module has to do that
//!   bookkeeping itself, by comparing against `Res<State<AppState>>` at each
//!   phase-completion instant, rather than relying on Bevy to no-op a
//!   same-state `.set()`.
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
//! `nav::handle_navigation_actions` (keyboard select / Next / Prev / Home) and
//! `ui::picker::draw_sketch_picker` (mouse click on a picker tile) call the
//! same `begin_fade_out` with [`ReloadReason::SketchSwitch`] instead of
//! writing `NextState<AppState>` directly, so every sketch-to-sketch hop —
//! not just a settings restart — dips to black. `soak::system::drive_soak`'s
//! cycle timer does the same, so an unattended soak run exercises the
//! graceful path too.
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

/// Duration of each fade leg (`FadeOut` and `FadeIn`) for [`ReloadReason::SettingsRestart`].
pub const FADE_DURATION: Duration = Duration::from_millis(200);

/// Duration of each fade leg (`FadeOut` and `FadeIn`) for
/// [`ReloadReason::SketchSwitch`].
///
/// Deliberately longer than [`FADE_DURATION`]: a settings restart re-runs the
/// *same* sketch's spawn path (the visual continuity is high — same sketch,
/// tweaked knob), where a snappy 200 ms reads as a quick flash. A
/// sketch-to-sketch switch tears down and rebuilds an entirely different
/// render graph and synth voice, so a slightly more deliberate 400 ms dip
/// reads as an intentional scene change rather than a glitch, while staying
/// well under the ~1 s threshold at which a transition starts to feel
/// sluggish on a kiosk a visitor is actively driving.
pub const SKETCH_SWITCH_FADE_DURATION: Duration = Duration::from_millis(400);

/// Why a reload was requested. Selects the fade profile.
///
/// A settings restart fades to black and dips audio over [`FADE_DURATION`]; a
/// window resize is instant (one black repaint frame nobody sees) and pushes **no
/// master-volume command**, because the reload exists only to re-run the sketch's
/// spawn path at the new extent, and a kiosk waking from sleep must not have its
/// output fade to silence.
///
/// **This does not mean a resize is inaudible.** The reload still hops through
/// `Home`, so the sketch's own `OnExit`/`OnEnter` audio hooks run: Line and Dots
/// tear down and rebuild their synth voice graph and momentarily drop their
/// background bed to zero. On the `SettingsRestart` path the 200 ms fade is what
/// masks that churn. `WindowResize` skips the fade, so the churn happens at full
/// master volume. Whether it is audible depends on whether the DSP host ramps
/// parameter changes or applies them as hard steps; that has not been measured.
/// If it clicks, the fix is either a short (~30-50 ms) master fade for this
/// reason, or skipping the per-sketch audio hooks when the reload is a resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReloadReason {
    /// A `requires_restart` settings change. The historical behaviour: 200 ms
    /// fade out, audio dip to silence, one `Home` frame, 200 ms fade in.
    #[default]
    SettingsRestart,
    /// A user-initiated sketch-to-sketch switch: a picker tile click, a
    /// number-key select, Next/Prev, or `NavigateHome` — anything that used
    /// to write `NextState<AppState>` directly and cut instantly to the new
    /// sketch. 400 ms fade out, audio dip to silence, one `Home` frame (or
    /// zero, when the switch already started at or ends at `Home` — see the
    /// module doc's note on the `Switch` phase), 400 ms fade in.
    ///
    /// ## Why this needs its own reason, not `SettingsRestart`
    ///
    /// `SettingsRestart` masks a same-sketch respawn — 200 ms reads as a
    /// quick flash appropriate to "you tweaked a knob". A sketch switch
    /// swaps the entire scene and synth voice; see [`SKETCH_SWITCH_FADE_DURATION`]
    /// for why that gets a longer, more deliberate fade.
    SketchSwitch,
    /// A settled window resize (F11, monitor re-enumeration, startup scale
    /// settle). Instant and silent.
    WindowResize,
    /// The audio output stream was rebuilt after a device reconnect (a sleeping
    /// HDMI TV woke, a USB interface came back). Instant and silent, exactly like
    /// [`Self::WindowResize`].
    ///
    /// ## Why a reload at all
    ///
    /// A rebuild constructs a **fresh `DspHost`**, which has no synth voice. The
    /// only producers of `AddLineSynth` / `AddDotsSynth` / `AddCymaticsSynth` /
    /// `AddFlameSynth` are the four sketches' `OnEnter(AppState::…)` systems, and
    /// a stream rebuild does not re-run `OnEnter`; the per-frame `Set*Param`
    /// commands do not repair it either, because `DspHost::apply` routes them to
    /// a voice that does not exist. So a mid-sketch reconnect used to report
    /// `Running` with a playing transport and **no sound** until a visitor
    /// happened to navigate away and back. On an unattended kiosk that is
    /// indistinguishable from the outage it was recovering from. The reload's
    /// `sketch → Home → sketch` hop re-runs `OnEnter`, which re-adds the synth
    /// graph and re-seeds its parameters through the sketch's own normal path.
    ///
    /// ## Why instant and silent
    ///
    /// The audio has *just* come back. A 200 ms black fade would be a visible
    /// flash for a visitor who saw nothing wrong, and dipping the master volume
    /// would duck the output we are in the middle of restoring. Same profile as a
    /// resize: [`Duration::ZERO`], no master-volume command. (The same "not
    /// inaudible" caveat as `WindowResize` applies — the `Home` hop still churns
    /// the sketch's own audio hooks — but here the alternative is *permanent*
    /// silence.)
    AudioDeviceReconnect,
}

/// Fade-leg duration for a reload `reason`: [`FADE_DURATION`] for a settings
/// restart, [`Duration::ZERO`] for a window resize or an audio-device reconnect
/// (both of which advance on the first frame). Pure so the mapping is
/// unit-testable without an app.
///
/// A zero duration is a supported value, not a degenerate one:
/// [`SketchReloadState::overlay_alpha`] short-circuits to the terminal alpha
/// rather than dividing by it (which would be `0.0 / 0.0` = NaN, and
/// `NaN.clamp` is NaN).
fn fade_duration(reason: ReloadReason) -> Duration {
    match reason {
        ReloadReason::SettingsRestart => FADE_DURATION,
        ReloadReason::SketchSwitch => SKETCH_SWITCH_FADE_DURATION,
        ReloadReason::WindowResize | ReloadReason::AudioDeviceReconnect => Duration::ZERO,
    }
}

/// Whether a reload `reason` fades the master volume. Neither a window resize (a
/// kiosk waking from sleep would otherwise cut its sound) nor an audio-device
/// reconnect (which would duck the output it is in the middle of restoring) may
/// touch audio. Pure so the mapping is unit-testable without an app.
fn fades_audio(reason: ReloadReason) -> bool {
    match reason {
        ReloadReason::SettingsRestart | ReloadReason::SketchSwitch => true,
        ReloadReason::WindowResize | ReloadReason::AudioDeviceReconnect => false,
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
    current: Res<'_, State<super::state::AppState>>,
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
                // FadeOut complete — switch to Home so the sketch exits
                // cleanly. Skipped when we are already at Home (a reload that
                // began at Home, e.g. a picker click or number-key select
                // made from the picker page): `NextState::set` always uses
                // the `Pending` variant, which Bevy runs with
                // `allow_same_state_transitions = true` — unlike
                // `set_if_neq`, a same-state `.set()` does NOT no-op on its
                // own, so writing it here unconditionally would re-fire
                // `OnExit(Home)`/`OnEnter(Home)` for a hop we never actually
                // needed. See the module doc's `Switch` phase note.
                if *current.get() != super::state::AppState::Home {
                    next_app.set(super::state::AppState::Home);
                }
                state.phase = ReloadPhase::Switch;
                tracing::debug!("reload overlay: FadeOut complete → Switch (Home)");
            }
        }
        ReloadPhase::Switch => {
            // One frame in Home (or zero additional frames, per the FadeOut
            // guard above); arm the re-entry into the sketch — or Home
            // itself — that the caller asked to land on (set by
            // `begin_fade_out`). Symmetric guard to the one in `FadeOut`:
            // when `return_state` is Home and the FadeOut leg's hop already
            // delivered us there (or we started there and skipped the hop),
            // `*current.get()` already equals `return_to`, so writing
            // `NextState::set(return_to)` again would double-fire
            // `Home`'s `OnExit`/`OnEnter` for the same reason described above.
            let return_to = state.return_state;
            if return_to != *current.get() {
                next_app.set(return_to);
            }
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

    /// The reconnect reload's policy is *identical* to a resize's: a visitor who
    /// never noticed the outage must not see a 200 ms black flash, and the master
    /// volume must not be ducked on the output we have just restored. Both halves
    /// are pinned, because either one drifting would be silently wrong (the fade
    /// would look like a glitch; the audio dip would be inaudible-by-accident only
    /// when the graph happens to be empty anyway).
    #[test]
    fn an_audio_device_reconnect_is_instant_and_silent_exactly_like_a_resize() {
        assert_eq!(
            fade_duration(ReloadReason::AudioDeviceReconnect),
            fade_duration(ReloadReason::WindowResize),
        );
        assert_eq!(
            fade_duration(ReloadReason::AudioDeviceReconnect),
            Duration::ZERO,
        );
        assert!(!fades_audio(ReloadReason::AudioDeviceReconnect));
    }

    /// The zero-duration fade path has a NaN hazard (`elapsed / 0.0`), guarded in
    /// `overlay_alpha`. `WindowResize` is covered below; this pins that the new
    /// reason genuinely takes the same guarded path rather than a fresh one.
    #[test]
    fn a_reconnect_reload_takes_the_guarded_zero_fade_path_without_nan() {
        let fade_out = SketchReloadState {
            phase: ReloadPhase::FadeOut,
            reason: ReloadReason::AudioDeviceReconnect,
            ..Default::default()
        };
        let a = fade_out.overlay_alpha(Duration::ZERO);
        assert!(a.is_finite(), "alpha must not be NaN");
        assert!(a > 0.99, "zero-length FadeOut is fully opaque, got {a}");

        let fade_in = SketchReloadState {
            phase: ReloadPhase::FadeIn,
            reason: ReloadReason::AudioDeviceReconnect,
            started_at: Duration::ZERO,
            ..Default::default()
        };
        let b = fade_in.overlay_alpha(Duration::ZERO);
        assert!(b.is_finite(), "alpha must not be NaN");
        assert!(b < 0.01, "zero-length FadeIn is fully transparent, got {b}");
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

    /// Guards the whole reload mechanism's premise: `drive_reload_state` must
    /// actually walk `FadeOut -> Switch -> FadeIn`, hopping through
    /// `AppState::Home` for at least one frame and re-firing the sketch's
    /// `OnEnter` exactly once.
    ///
    /// Why this matters: the reload works by bouncing `sketch -> Home ->
    /// sketch`, because Bevy does not re-fire `OnEnter` for a state it is
    /// already in. The `Home` hop in the `Switch` phase is the *only* thing
    /// that re-runs a sketch's spawn systems, which is how a sketch rebuilds
    /// its window-size-dependent resources (particle counts, the Cymatics sim
    /// grid) after a resize. If the zero-duration `WindowResize` path were
    /// ever "optimised" by folding the `Switch` arm into `FadeOut` — writing
    /// both `NextState(Home)` and `NextState(return_state)` in the same
    /// frame — Bevy would apply only the last `NextState` write; `OnExit`
    /// and `OnEnter` would never fire, and every sketch would silently stop
    /// respawning on resize while every *other* existing test kept passing
    /// (none of them drive `drive_reload_state` past `FadeOut`).
    ///
    /// Headless and GPU-free: `MinimalPlugins` + `StatesPlugin`, no window, no
    /// egui, no audio. Uses `ReloadReason::WindowResize` (a zero-length fade,
    /// see `window_resize_is_instant_and_silent` above) so every phase
    /// transition is unconditionally due on the very next frame regardless of
    /// elapsed wall-clock time — no `TimeUpdateStrategy` needed for the
    /// `>= fade` comparisons to hold, though we still install a small
    /// `ManualDuration` step for deterministic `Time::elapsed()` values in the
    /// assertions.
    #[test]
    fn window_resize_reload_hops_through_home_and_refires_on_enter() {
        use bevy::state::app::StatesPlugin;
        use bevy::time::TimeUpdateStrategy;

        /// Counts how many times `OnEnter(AppState::Line)` has run.
        #[derive(Resource, Default)]
        struct EnterCount(u32);

        fn count_enter(mut count: ResMut<'_, EnterCount>) {
            count.0 += 1;
        }

        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(StatesPlugin);
        app.init_state::<AppState>();
        app.init_resource::<SketchReloadState>();
        app.init_resource::<EnterCount>();
        app.add_systems(OnEnter(AppState::Line), count_enter);
        app.add_systems(Update, drive_reload_state);

        // Enter Line directly (not via the reload overlay) and settle.
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Line);
        app.update();
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Line,
            "precondition: must be in Line before the reload starts"
        );
        assert_eq!(
            app.world().resource::<EnterCount>().0,
            1,
            "precondition: OnEnter(Line) fired exactly once for the initial entry"
        );

        // Deterministic small per-frame step from here on.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            10,
        )));

        // Begin a silent, instant WindowResize reload while sitting in Line.
        {
            let now = app.world().resource::<Time>().elapsed();
            let mut reload_state = app.world_mut().resource_mut::<SketchReloadState>();
            reload_state.begin_fade_out(now, 1.0, AppState::Line, ReloadReason::WindowResize);
        }
        assert_eq!(
            app.world().resource::<SketchReloadState>().phase,
            ReloadPhase::FadeOut
        );

        // Frame 1: FadeOut's fade duration is ZERO for WindowResize, so
        // `drive_reload_state` completes the leg immediately this Update,
        // calls `NextState::set(Home)`, and advances phase -> Switch.
        // `NextState` writes made during `Update` are applied by the
        // `StateTransition` schedule at the START of the *next* `app.update()`
        // — the same one-tick lag every other test in this codebase accounts
        // for (see e.g. `select_line_transitions_into_line_state` in
        // `tests/lifecycle.rs`: "Pending transitions resolve on the next
        // update tick") — so `State<AppState>` still reads `Line` here.
        app.update();
        assert_eq!(
            app.world().resource::<SketchReloadState>().phase,
            ReloadPhase::Switch,
            "FadeOut's zero-length fade must complete on the very first frame"
        );

        // Frame 2: StateTransition (start of this frame) applies the pending
        // `NextState(Home)` write from Frame 1 — THIS is where the Home hop
        // becomes observable. Then Update runs `drive_reload_state` again:
        // Switch sets `NextState(return_state = Line)` and advances phase ->
        // FadeIn.
        app.update();
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Home,
            "the reload MUST pass through Home for at least one frame — this \
             is the only thing that re-fires the sketch's OnEnter, since Bevy \
             does not re-run OnEnter for a state it is already in"
        );
        assert_eq!(
            app.world().resource::<EnterCount>().0,
            1,
            "OnEnter(Line) must not have re-fired yet — the Home hop only \
             just became active this frame"
        );
        assert_eq!(
            app.world().resource::<SketchReloadState>().phase,
            ReloadPhase::FadeIn
        );

        // Frame 3: StateTransition (start of this frame) applies the pending
        // `NextState(Line)` write from Frame 2, re-entering Line and
        // re-running `OnEnter(Line)`. Then Update runs `drive_reload_state`:
        // FadeIn's zero-length fade completes immediately, returning phase to
        // Idle.
        app.update();
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Line
        );
        assert_eq!(
            app.world().resource::<EnterCount>().0,
            2,
            "OnEnter(Line) must fire exactly once more, driven solely by the \
             reload's Home hop"
        );
        assert_eq!(
            app.world().resource::<SketchReloadState>().phase,
            ReloadPhase::Idle
        );
    }

    #[test]
    fn sketch_switch_fades_over_the_full_duration_and_dips_audio() {
        assert_eq!(
            fade_duration(ReloadReason::SketchSwitch),
            SKETCH_SWITCH_FADE_DURATION
        );
        assert!(fades_audio(ReloadReason::SketchSwitch));
        // Pins the deliberate-vs-quick relationship documented on
        // `SKETCH_SWITCH_FADE_DURATION`: a full scene swap gets a longer fade
        // than a same-sketch settings restart.
        assert!(
            SKETCH_SWITCH_FADE_DURATION > FADE_DURATION,
            "a sketch-to-sketch switch must fade more deliberately than a \
             same-sketch settings restart"
        );
    }

    /// Regression guard against `SketchSwitch` silently falling back to
    /// [`FADE_DURATION`] (e.g. a copy-pasted match arm): at the settings-restart
    /// duration the sketch-switch fade must still be mid-ramp, not already
    /// opaque, and it must only reach full opacity at its own, longer duration.
    #[test]
    fn sketch_switch_fade_out_ramps_over_its_own_longer_duration() {
        let mut s = SketchReloadState::default();
        s.begin_fade_out(
            Duration::ZERO,
            1.0,
            AppState::Flame,
            ReloadReason::SketchSwitch,
        );
        assert!(s.overlay_alpha(Duration::ZERO) < 0.01);
        assert!(
            s.overlay_alpha(FADE_DURATION) < 0.99,
            "at the settings-restart duration (200 ms) a sketch-switch fade \
             (400 ms) must still be ramping, not already opaque"
        );
        assert!(s.overlay_alpha(SKETCH_SWITCH_FADE_DURATION) >= 1.0);
    }

    /// A graceful sketch-to-sketch switch (e.g. Next/Prev, a picker click, a
    /// number-key select) between two *different* sketches must walk the same
    /// `FadeOut -> Switch -> FadeIn` phases as `SettingsRestart`/`WindowResize`,
    /// hopping through exactly one `Home` frame so both sketches' `OnExit`/
    /// `OnEnter` fire exactly once each, in the right order.
    ///
    /// Uses a 500 ms manual time step (comfortably past
    /// `SKETCH_SWITCH_FADE_DURATION`'s 400 ms) so each fade leg resolves in a
    /// single `app.update()`, mirroring the zero-duration `WindowResize` test
    /// above but for a reason with a *real* fade duration.
    #[test]
    fn sketch_switch_reload_hops_through_home_between_two_different_sketches() {
        use bevy::state::app::StatesPlugin;
        use bevy::time::{TimeUpdateStrategy, Virtual};

        /// Records `OnEnter`/`OnExit` firings in order, across every tracked
        /// state, so the whole walk's transition sequence can be asserted in
        /// one place.
        #[derive(Resource, Default)]
        struct TransitionLog(Vec<&'static str>);

        fn log_enter_home(mut log: ResMut<'_, TransitionLog>) {
            log.0.push("enter:Home");
        }
        fn log_exit_home(mut log: ResMut<'_, TransitionLog>) {
            log.0.push("exit:Home");
        }
        fn log_enter_line(mut log: ResMut<'_, TransitionLog>) {
            log.0.push("enter:Line");
        }
        fn log_exit_line(mut log: ResMut<'_, TransitionLog>) {
            log.0.push("exit:Line");
        }
        fn log_enter_flame(mut log: ResMut<'_, TransitionLog>) {
            log.0.push("enter:Flame");
        }
        fn log_exit_flame(mut log: ResMut<'_, TransitionLog>) {
            log.0.push("exit:Flame");
        }

        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(StatesPlugin);
        app.init_state::<AppState>();
        app.init_resource::<SketchReloadState>();
        app.init_resource::<TransitionLog>();
        app.add_systems(OnEnter(AppState::Home), log_enter_home);
        app.add_systems(OnExit(AppState::Home), log_exit_home);
        app.add_systems(OnEnter(AppState::Line), log_enter_line);
        app.add_systems(OnExit(AppState::Line), log_exit_line);
        app.add_systems(OnEnter(AppState::Flame), log_enter_flame);
        app.add_systems(OnExit(AppState::Flame), log_exit_flame);
        app.add_systems(Update, drive_reload_state);

        // Settle into Line first, outside the reload machine, then clear the
        // log so only events from the SketchSwitch reload itself are asserted.
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Line);
        app.update();
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Line,
            "precondition: must be in Line before the reload starts"
        );
        app.world_mut().resource_mut::<TransitionLog>().0.clear();

        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            500,
        )));
        // `Time<Virtual>`'s default `max_delta` (250 ms) would otherwise
        // silently clamp the 500 ms manual step below
        // `SKETCH_SWITCH_FADE_DURATION`'s 400 ms, stalling the fade forever.
        app.world_mut()
            .resource_mut::<Time<Virtual>>()
            .set_max_delta(Duration::from_secs(1));

        // Begin a graceful SketchSwitch reload from Line to Flame.
        {
            let now = app.world().resource::<Time>().elapsed();
            let mut reload_state = app.world_mut().resource_mut::<SketchReloadState>();
            reload_state.begin_fade_out(now, 1.0, AppState::Flame, ReloadReason::SketchSwitch);
        }

        // Frame 1: FadeOut's 400 ms leg is comfortably covered by the 500 ms
        // step, so it completes this Update; current is not Home (Line), so
        // `NextState(Home)` is queued and phase advances to Switch.
        app.update();
        assert_eq!(
            app.world().resource::<SketchReloadState>().phase,
            ReloadPhase::Switch
        );

        // Frame 2: StateTransition applies Line -> Home (exit:Line, enter:Home).
        // Switch phase then runs: return_to (Flame) != current (Home), so
        // NextState(Flame) is queued and phase advances to FadeIn.
        app.update();
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Home
        );
        assert_eq!(
            app.world().resource::<SketchReloadState>().phase,
            ReloadPhase::FadeIn
        );

        // Frame 3: StateTransition applies Home -> Flame (exit:Home, enter:Flame).
        // FadeIn's 400 ms leg is again covered by the 500 ms step, so it
        // completes this Update, returning phase to Idle.
        app.update();
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Flame
        );
        assert_eq!(
            app.world().resource::<SketchReloadState>().phase,
            ReloadPhase::Idle
        );

        assert_eq!(
            app.world().resource::<TransitionLog>().0,
            vec!["exit:Line", "enter:Home", "exit:Home", "enter:Flame"],
            "a sketch-to-sketch switch must hop through exactly one Home \
             frame, firing each OnExit/OnEnter exactly once, in order"
        );
    }

    /// The "clean Home hop" guarantee described in the module doc's `Switch`
    /// phase note: a `SketchSwitch` reload whose destination IS `Home`
    /// (`NavigateHome` pressed from a sketch) must fire `Home`'s
    /// `OnEnter`/`OnExit` exactly once — via the `FadeOut` leg's hop — and must
    /// NOT re-fire it a second time when the `Switch` phase's
    /// `NextState::set(return_state)` targets the same `Home` we already
    /// reached. Without the `current`-aware guard in `drive_reload_state`, this
    /// would double-fire because Bevy's `NextState::set` uses
    /// `allow_same_state_transitions = true` and does not no-op a same-state
    /// transition on its own.
    #[test]
    fn navigating_to_home_does_not_double_fire_homes_transition() {
        use bevy::state::app::StatesPlugin;
        use bevy::time::{TimeUpdateStrategy, Virtual};

        #[derive(Resource, Default)]
        struct TransitionLog(Vec<&'static str>);

        fn log_enter_home(mut log: ResMut<'_, TransitionLog>) {
            log.0.push("enter:Home");
        }
        fn log_exit_home(mut log: ResMut<'_, TransitionLog>) {
            log.0.push("exit:Home");
        }
        fn log_enter_line(mut log: ResMut<'_, TransitionLog>) {
            log.0.push("enter:Line");
        }
        fn log_exit_line(mut log: ResMut<'_, TransitionLog>) {
            log.0.push("exit:Line");
        }

        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(StatesPlugin);
        app.init_state::<AppState>();
        app.init_resource::<SketchReloadState>();
        app.init_resource::<TransitionLog>();
        app.add_systems(OnEnter(AppState::Home), log_enter_home);
        app.add_systems(OnExit(AppState::Home), log_exit_home);
        app.add_systems(OnEnter(AppState::Line), log_enter_line);
        app.add_systems(OnExit(AppState::Line), log_exit_line);
        app.add_systems(Update, drive_reload_state);

        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Line);
        app.update();
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Line
        );
        app.world_mut().resource_mut::<TransitionLog>().0.clear();

        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            500,
        )));
        // See the max_delta comment in the phase-walk test above.
        app.world_mut()
            .resource_mut::<Time<Virtual>>()
            .set_max_delta(Duration::from_secs(1));

        // Begin a graceful SketchSwitch reload from Line to Home.
        {
            let now = app.world().resource::<Time>().elapsed();
            let mut reload_state = app.world_mut().resource_mut::<SketchReloadState>();
            reload_state.begin_fade_out(now, 1.0, AppState::Home, ReloadReason::SketchSwitch);
        }

        app.update(); // FadeOut completes: current (Line) != Home -> NextState(Home) queued -> Switch
        app.update(); // StateTransition: Line -> Home (exit:Line, enter:Home). Switch: return_to == current (Home) -> skipped -> FadeIn
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Home
        );
        app.update(); // No pending transition (Switch skipped it); FadeIn completes -> Idle
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Home
        );
        assert_eq!(
            app.world().resource::<SketchReloadState>().phase,
            ReloadPhase::Idle
        );

        assert_eq!(
            app.world().resource::<TransitionLog>().0,
            vec!["exit:Line", "enter:Home"],
            "Home's OnEnter must fire exactly once, not twice, when a \
             SketchSwitch reload's destination is Home"
        );
    }

    /// Symmetric to [`navigating_to_home_does_not_double_fire_homes_transition`]:
    /// a `SketchSwitch` reload that *begins* at `Home` (a picker click or a
    /// number-key select made from the picker page) must not re-fire `Home`'s
    /// `OnExit`/`OnEnter` via the `FadeOut` leg's hop, because we are already
    /// there — the guard in `drive_reload_state`'s `FadeOut` arm skips
    /// `NextState::set(Home)` when `current == Home` already.
    #[test]
    fn navigating_from_home_does_not_double_fire_homes_transition() {
        use bevy::state::app::StatesPlugin;
        use bevy::time::{TimeUpdateStrategy, Virtual};

        #[derive(Resource, Default)]
        struct TransitionLog(Vec<&'static str>);

        fn log_enter_home(mut log: ResMut<'_, TransitionLog>) {
            log.0.push("enter:Home");
        }
        fn log_exit_home(mut log: ResMut<'_, TransitionLog>) {
            log.0.push("exit:Home");
        }
        fn log_enter_line(mut log: ResMut<'_, TransitionLog>) {
            log.0.push("enter:Line");
        }
        fn log_exit_line(mut log: ResMut<'_, TransitionLog>) {
            log.0.push("exit:Line");
        }

        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(StatesPlugin);
        app.init_state::<AppState>();
        app.init_resource::<SketchReloadState>();
        app.init_resource::<TransitionLog>();
        app.add_systems(OnEnter(AppState::Home), log_enter_home);
        app.add_systems(OnExit(AppState::Home), log_exit_home);
        app.add_systems(OnEnter(AppState::Line), log_enter_line);
        app.add_systems(OnExit(AppState::Line), log_exit_line);
        app.add_systems(Update, drive_reload_state);

        // Settle at the default Home state, then clear whatever the initial
        // state-machine bring-up logged.
        app.update();
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Home
        );
        app.world_mut().resource_mut::<TransitionLog>().0.clear();

        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            500,
        )));
        // See the max_delta comment in the phase-walk test above.
        app.world_mut()
            .resource_mut::<Time<Virtual>>()
            .set_max_delta(Duration::from_secs(1));

        // Begin a graceful SketchSwitch reload from Home to Line (a picker
        // click or number-key select made while sitting on the picker page).
        {
            let now = app.world().resource::<Time>().elapsed();
            let mut reload_state = app.world_mut().resource_mut::<SketchReloadState>();
            reload_state.begin_fade_out(now, 1.0, AppState::Line, ReloadReason::SketchSwitch);
        }

        app.update(); // FadeOut completes: current is already Home -> hop skipped -> Switch
        assert_eq!(
            app.world().resource::<SketchReloadState>().phase,
            ReloadPhase::Switch
        );
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Home,
            "we never left Home, so no transition should have run yet"
        );
        app.update(); // No pending transition from the skipped hop; Switch: return_to (Line) != current (Home) -> NextState(Line) queued -> FadeIn
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Home,
            "the Line transition is queued this frame but not applied until the next"
        );
        app.update(); // StateTransition: Home -> Line (exit:Home, enter:Line); FadeIn completes -> Idle
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Line
        );
        assert_eq!(
            app.world().resource::<SketchReloadState>().phase,
            ReloadPhase::Idle
        );

        assert_eq!(
            app.world().resource::<TransitionLog>().0,
            vec!["exit:Home", "enter:Line"],
            "Home's OnExit must fire exactly once, not twice, when a \
             SketchSwitch reload begins at Home"
        );
    }

    /// End-to-end audio-dip-and-restore assertion for [`ReloadReason::SketchSwitch`]:
    /// the `FadeOut` leg must push at least one `SetMasterVolume` command well
    /// below the starting volume, and the very last command pushed by the whole
    /// walk must restore `pre_fade_volume` exactly (the `FadeIn` completion's
    /// explicit restore, which runs after that leg's final ramp-driven push —
    /// see `drive_reload_state`'s `FadeIn` arm).
    #[test]
    #[allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test code with locally-constructed values; a panic here is the test failing, \
                  and the panic/expect messages carry the diagnostic detail"
    )]
    fn sketch_switch_reload_dips_and_restores_master_volume() {
        use bevy::state::app::StatesPlugin;
        use bevy::time::{TimeUpdateStrategy, Virtual};

        let (producer, mut consumer) = rtrb::RingBuffer::<crate::audio::command::AudioCommand>::new(
            crate::audio::ring::RING_CAPACITY,
        );

        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(StatesPlugin);
        app.init_state::<AppState>();
        app.init_resource::<SketchReloadState>();
        app.insert_non_send(crate::audio::ring::AudioCommandSender::new(producer));
        app.add_systems(Update, drive_reload_state);

        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Line);
        app.update();
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Line
        );

        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            500,
        )));
        // See the max_delta comment in the phase-walk test above.
        app.world_mut()
            .resource_mut::<Time<Virtual>>()
            .set_max_delta(Duration::from_secs(1));

        let pre_fade_volume = 0.8_f32;
        {
            let now = app.world().resource::<Time>().elapsed();
            let mut reload_state = app.world_mut().resource_mut::<SketchReloadState>();
            reload_state.begin_fade_out(
                now,
                pre_fade_volume,
                AppState::Flame,
                ReloadReason::SketchSwitch,
            );
        }

        // Walk the full FadeOut -> Switch -> FadeIn -> Idle cycle.
        app.update();
        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<SketchReloadState>().phase,
            ReloadPhase::Idle
        );

        let volumes: Vec<f32> = std::iter::from_fn(|| consumer.pop().ok())
            .map(|c| match c {
                crate::audio::command::AudioCommand::SetMasterVolume(v) => v,
                other => panic!("unexpected audio command during reload: {other:?}"),
            })
            .collect();

        assert!(
            !volumes.is_empty(),
            "a SketchSwitch reload must push at least one SetMasterVolume command"
        );
        assert!(
            volumes.iter().any(|v| *v < 0.5),
            "the FadeOut leg must dip the master volume well below its \
             starting point: {volumes:?}"
        );
        let restored = *volumes.last().expect("at least one volume command");
        assert!(
            (restored - pre_fade_volume).abs() < 1e-3,
            "the final SetMasterVolume command must restore pre_fade_volume \
             ({pre_fade_volume}), got {restored} (all: {volumes:?})"
        );
    }
}
