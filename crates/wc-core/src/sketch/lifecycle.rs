//! Shared per-sketch lifecycle glue.
//!
//! Every sketch's `Plugin::build` wired several pieces of lifecycle plumbing by
//! hand, byte-for-byte identical across Line, Dots, and Cymatics. Before a
//! fourth sketch multiplied the copy-paste, that glue is collected here so a fix
//! lands once rather than three times.
//!
//! ## What this module owns
//!
//! - [`RESTART_DEBOUNCE`] — the single source of truth for the settings-change
//!   restart debounce window (previously re-declared in all three sketches).
//! - [`restart_on_settings_change`] — the generic restart listener that begins
//!   the reload fade when a `requires_restart` setting for the sketch changes.
//! - [`apply_render_profile`] / [`reset_render_profile`] — the camera
//!   tonemapping + bloom applier (per frame while the sketch is up) and its
//!   `OnExit` reset back to the SDR base.
//! - [`SketchLifecycle`] / [`RenderProfile`] — the trait + value type that let
//!   the generic systems above read a sketch's target [`AppState`] and its live
//!   render-profile knobs without knowing the concrete settings struct.
//!
//! ## Data flow
//!
//! A sketch's plugin implements [`SketchLifecycle`] for its settings struct
//! (associating the settings type with its [`AppState`] and exposing its
//! render-profile knobs), then registers the generic systems in `build`:
//!
//! ```ignore
//! app.add_systems(Update, restart_on_settings_change::<MySettings>);
//! app.add_systems(
//!     Update,
//!     apply_render_profile::<MySettings>.run_if(in_state(AppState::MySketch)),
//! );
//! app.add_systems(OnExit(AppState::MySketch), reset_render_profile);
//! ```
//!
//! The run condition stays at the call site so each sketch keeps its exact
//! gating (Line/Dots gate on `in_state`; Cymatics also runs through idle and the
//! screensaver). Material-level knobs that reference sketch-crate render types
//! (e.g. the particle-material master-brightness driver) stay in the sketch
//! crate, since `wc-core` cannot depend on `wc-sketches`.

use std::time::Duration;

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;

use crate::audio::state::AudioState;
use crate::lifecycle::reload::{ReloadReason, SketchReloadState};
use crate::lifecycle::state::AppState;
use crate::lifecycle::window_resize::WindowResizeSettled;
use crate::render::{BloomComposite, TonemapChoice};
use crate::settings::{SketchRestart, SketchSettings};

/// How long the user must stop adjusting a `requires_restart` setting before the
/// sketch restarts. 500 ms of quiescence prevents mid-drag sketch kills while
/// the user is still moving a slider.
///
/// This is the single definition consumed by every sketch via
/// [`restart_on_settings_change`]; sketches must not re-declare their own.
pub const RESTART_DEBOUNCE: Duration = Duration::from_millis(500);

/// The camera render-profile knobs a sketch exposes: the tonemapping operator
/// and the three bloom parameters written onto the main camera each frame.
///
/// A plain value type so [`apply_render_profile`] can read a sketch's live
/// profile through [`SketchLifecycle::render_profile`] without depending on the
/// concrete settings struct. Field-for-field the arguments of
/// [`crate::render::set_camera_render_profile`].
#[derive(Clone, Copy)]
pub struct RenderProfile {
    /// Camera tonemapping operator applied while the sketch is up.
    pub tonemapping: TonemapChoice,
    /// Bloom intensity for the main camera.
    pub bloom_intensity: f32,
    /// Bloom prefilter threshold.
    pub bloom_threshold: f32,
    /// Bloom composite mode (paired with the threshold; see [`BloomComposite`]).
    pub bloom_composite: BloomComposite,
}

/// Ties a sketch's settings struct to its shared lifecycle glue.
///
/// Implemented once per sketch (in the sketch crate, alongside the settings
/// struct) so the generic systems in this module can recover the sketch's
/// [`AppState`] and live render profile from the settings type parameter alone.
/// Extends [`SketchSettings`], so implementors already carry `STORAGE_KEY`,
/// `Resource`, `Default`, and the rest of the settings contract.
pub trait SketchLifecycle: SketchSettings {
    /// The [`AppState`] variant this sketch occupies. Used by
    /// [`restart_on_settings_change`] to gate the listener and to name the
    /// reload's return state.
    const STATE: AppState;

    /// The render profile derived from the current settings values. Read each
    /// frame by [`apply_render_profile`]; change-gating happens inside
    /// [`crate::render::set_camera_render_profile`], so returning a fresh value
    /// every frame is cheap.
    fn render_profile(&self) -> RenderProfile;
}

/// Generic restart listener: begins the reload fade-overlay transition when a
/// `requires_restart` setting for sketch `S` changes, so the
/// `Sketch → Home → Sketch` cycle is blacked out rather than flashing the
/// picker page.
///
/// A [`RESTART_DEBOUNCE`] window (500 ms) prevents the restart from firing while
/// the user is still dragging a slider. The debounce timestamp is tracked in a
/// `Local<Option<Duration>>` updated on every matching message and checked each
/// frame against [`Time::elapsed`].
///
/// After the debounce window closes, calls
/// [`SketchReloadState::begin_fade_out`], which sets `phase = FadeOut`. The
/// `drive_reload_state` system (registered in `wc-core`'s `LifecyclePlugin`)
/// owns all subsequent phase transitions: `FadeOut` → Switch (sets
/// `NextState::Home`) → `FadeIn` (sets `NextState::S::STATE`).
///
/// Only arms while `AppState == S::STATE` (not during the `Home`/`FadeIn` return
/// leg) and when no reload is already in progress. This is the generic form of
/// what each sketch previously duplicated as `restart_on_*_settings_change`.
pub fn restart_on_settings_change<S: SketchLifecycle>(
    mut events: MessageReader<'_, '_, SketchRestart>,
    time: Res<'_, Time>,
    current: Res<'_, State<AppState>>,
    mut reload_state: ResMut<'_, SketchReloadState>,
    // Optional: not present in headless (MinimalPlugins) test harnesses.
    audio_state: Option<Res<'_, AudioState>>,
    // Tracks the `Time::elapsed` of the last received restart message.
    // `None` means no message has been received since the last restart.
    mut last_change_at: Local<'_, Option<Duration>>,
) {
    // Absorb any new restart messages for this sketch's key, resetting the
    // debounce timestamp. Only arm when in this sketch's state (not during the
    // Home/FadeIn return leg) and when no reload is already in progress.
    let got_message = events.read().any(|e| e.storage_key == S::STORAGE_KEY);
    if got_message && **current == S::STATE && reload_state.is_idle() {
        *last_change_at = Some(time.elapsed());
        tracing::debug!(
            "sketch settings ('{}') changed — debounce timer reset (500 ms)",
            S::STORAGE_KEY
        );
    }

    // Fire the FadeOut only after the debounce window of no further changes.
    if let Some(last) = *last_change_at {
        let elapsed_since = time.elapsed().saturating_sub(last);
        if elapsed_since >= RESTART_DEBOUNCE && **current == S::STATE && reload_state.is_idle() {
            // Fall back to full volume (1.0) when the audio engine hasn't
            // started — headless tests and early startup before the cpal
            // stream is active.
            let pre_fade_volume = audio_state.as_ref().map_or(1.0, |s| s.volume);
            reload_state.begin_fade_out(
                time.elapsed(),
                pre_fade_volume,
                S::STATE,
                ReloadReason::SettingsRestart,
            );
            *last_change_at = None;
            tracing::debug!(
                "sketch settings ('{}') debounce elapsed — beginning reload FadeOut",
                S::STORAGE_KEY
            );
        }
    }
}

/// Window-resize listener: re-runs sketch `S`'s spawn path when the window has
/// settled at a new size, by driving the reload overlay with
/// [`ReloadReason::WindowResize`] (instant and silent — no fade, no audio dip).
///
/// Reuse rationale: a sketch derives its window-size-dependent resources
/// (particle counts; the Cymatics sim grid) in its `OnEnter` spawn systems, so
/// the cleanest "respawn at the new size" is to re-run `OnEnter`. The reload
/// overlay already performs exactly that `Sketch -> Home -> Sketch` round-trip
/// (see [`crate::lifecycle::reload`]); the `WindowResize` reason makes it cost a
/// single black repaint frame with no audio dropout. Rebuilding rather than
/// rescaling in place is required because a sketch's element *count* changes
/// with size (Dots' grid, Line's particle count), so the GPU buffers must be
/// reallocated.
///
/// Registered **always-on** (no `run_if`), mirroring `restart_on_settings_change`
/// and gating internally on being the running sketch with no reload in flight.
/// It deliberately does **not** gate on `sketch_active`: a resize during `Idle`
/// or the attract screensaver (e.g. a TV re-enumerating after sleep) must still
/// respawn the sketch at the new size. The `Home` hop resets the sketch's
/// `SketchActivity` sub-state to `Active`, after which the idle timer re-engages
/// normally — acceptable for an unattended kiosk. It is a sanctioned always-on
/// listener (see AGENTS.md); it no-ops in one cheap branch when no settle
/// arrived.
///
/// `resize_reload_should_fire` (private; see below) encodes the gate.
pub fn reload_on_resize_settled<S: SketchLifecycle>(
    mut settled: MessageReader<'_, '_, WindowResizeSettled>,
    time: Res<'_, Time>,
    current: Res<'_, State<AppState>>,
    mut reload_state: ResMut<'_, SketchReloadState>,
    // Absent in headless (MinimalPlugins) test harnesses and before the cpal
    // stream is up; fall back to full volume then. (A resize never dips audio,
    // so this value is only carried for symmetry with the settings-restart path.)
    audio_state: Option<Res<'_, AudioState>>,
) {
    // Drain the reader every frame regardless of the decision.
    let got_settle = settled.read().count() > 0;
    if !resize_reload_should_fire(got_settle, **current == S::STATE, reload_state.is_idle()) {
        return;
    }
    let pre_fade_volume = audio_state.as_ref().map_or(1.0, |s| s.volume);
    reload_state.begin_fade_out(
        time.elapsed(),
        pre_fade_volume,
        S::STATE,
        ReloadReason::WindowResize,
    );
    tracing::debug!(
        "window resize settled while in '{}' — beginning a silent, instant reload at the new size",
        S::STORAGE_KEY
    );
}

/// Whether a settled resize should begin a reload this frame.
///
/// Pure so the gate is unit-testable without an app: fire only when a settle
/// arrived AND this is the running sketch AND no reload is already in progress
/// (a reload drives its own `Sketch -> Home -> Sketch` transition, which must
/// not re-trigger one).
fn resize_reload_should_fire(got_settle: bool, in_state: bool, reload_idle: bool) -> bool {
    got_settle && in_state && reload_idle
}

/// Write sketch `S`'s tonemapping + bloom profile onto the main camera each
/// frame (live dev-panel tuning). Change-gated inside
/// [`crate::render::set_camera_render_profile`], so an unchanged profile is a
/// no-op.
///
/// The run condition is supplied at the call site so each sketch keeps its exact
/// gating (Line/Dots gate on `in_state`; Cymatics also runs through idle and the
/// screensaver). This is the generic form of the per-sketch
/// `apply_*_render_profile` systems.
pub fn apply_render_profile<S: SketchLifecycle>(
    settings: Res<'_, S>,
    mut camera: Query<'_, '_, (&mut Tonemapping, &mut Bloom), With<Camera2d>>,
) {
    let profile = settings.render_profile();
    for (mut tonemapping, mut bloom) in &mut camera {
        crate::render::set_camera_render_profile(
            &mut tonemapping,
            &mut bloom,
            profile.tonemapping,
            profile.bloom_intensity,
            profile.bloom_threshold,
            profile.bloom_composite,
        );
    }
}

/// Restore the SDR camera base so Home/picker renders un-tonemapped.
///
/// Wire into a sketch's `OnExit(AppState::X)` schedule. Sketch-agnostic (it
/// touches only the shared `Camera2d`), so the single function is registered on
/// every sketch's `OnExit` — distinct schedules, one implementation. This is the
/// generic form of the per-sketch `reset_*_render_profile` systems.
pub fn reset_render_profile(
    mut camera: Query<'_, '_, (&mut Tonemapping, &mut Bloom), With<Camera2d>>,
) {
    for (mut tonemapping, mut bloom) in &mut camera {
        crate::render::reset_camera_render_profile(&mut tonemapping, &mut bloom);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reload only begins on a real settle while this is the running sketch
    /// and no reload is already in flight. The negatives are the ones that
    /// matter: no stray fire, no firing for another sketch, and no re-trigger
    /// mid-reload (the reload drives its own `Sketch → Home → Sketch` hop, which
    /// would otherwise loop).
    #[test]
    fn fires_only_on_a_settle_in_this_sketch_while_reload_is_idle() {
        assert!(
            resize_reload_should_fire(true, true, true),
            "settle, our sketch, idle → fire"
        );
        assert!(
            !resize_reload_should_fire(false, true, true),
            "no settle → nothing"
        );
        assert!(
            !resize_reload_should_fire(true, false, true),
            "settle but not our sketch → nothing"
        );
        assert!(
            !resize_reload_should_fire(true, true, false),
            "settle mid-reload must not re-trigger (its own Home hop would loop)"
        );
    }
}
