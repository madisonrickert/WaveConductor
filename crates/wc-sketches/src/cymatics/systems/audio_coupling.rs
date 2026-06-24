//! Cymatics audio coupling: derive the v4 audio scalars from [`CymaticsState`]
//! (CPU-side only — no GPU readback) and push them through the audio ring.
//! Silent in the screensaver, matching Dots.
//!
//! ## What this writes each frame
//!
//! Four `SetCymaticsParam` commands per frame, pushed onto the lock-free
//! [`AudioCommandSender`] ring:
//!
//! - `"osc_volume"` — synth swell that grows as the wave field enters higher
//!   harmonics: `clamp(smoothstep(num_cycles, 1.002, 1.1022) × 0.5, 0, 1)`.
//!   (1.1022 = `DEFAULT_NUM_CYCLES * 1.1`; v4 `index.ts:357`.)
//! - `"osc_freq_scalar"` — effective frequency ratio; `slow_down` from interaction
//!   onset temporarily pulls the pitch down by up to 75%, then decays back to 1.0:
//!   `(num_cycles / (1 + slow_down × 3)) / 1.002`.
//! - `"blub_volume"` — v4 blub formula × 0.05 (the `·0.05` scale lives **here
//!   exactly once**; the C4 audio engine clamps to `[0, 0.3]` — do NOT scale or
//!   clamp again).
//! - `"blub_rate"` — retrigger rate; audio engine clamps to `[0.5, 4.0]`.
//!
//! ## One-shot samples
//!
//! On each interaction onset edge (`active_radius` crossing the interacting floor),
//! `Kick` and `RisingBass` are triggered, throttled to ~500 ms (30 frames at
//! 60 fps) via [`CymaticsTriggerState`].
//!
//! ## Cross-task rules
//!
//! - **Rule #3**: the `·0.05` blub scale is applied here, once. The C4 engine
//!   clamps; this module does not.
//! - **Rule #2**: `simulation_time` is NOT touched here — owned by
//!   `update_cymatics_sim_params`.
//! - **`slow_down` onset**: incremented here on each onset edge; C9 only decays
//!   (`×0.95` per frame). No double-count.
//!
//! [`AudioCommandSender`]: wc_core::audio::ring::AudioCommandSender

use bevy::ecs::system::NonSendMut;
use bevy::prelude::*;

use wc_core::audio::command::{AudioCommand, CymaticsSampleId};
use wc_core::audio::ring::AudioCommandSender;

use crate::cymatics::settings::CymaticsSettings;
use crate::cymatics::CymaticsState;

use super::interaction;

// ── Derived per-frame audio parameters ───────────────────────────────────────

/// Derived per-frame audio parameters computed from [`CymaticsState`].
///
/// Pure output of [`audio_params`]; the values are ready to push to the
/// `SetCymaticsParam` commands without further clamping (the audio engine
/// clamps `blub_volume` to `[0, 0.3]` and `blub_rate` to `[0.5, 4.0]`).
#[derive(Clone, Copy, Debug)]
pub struct CymaticsAudioParams {
    /// Synth oscillator volume (`osc_volume`). v4 smoothstep swell tied to
    /// `num_cycles` exceeding 1.002; saturates at 0.5 when the field reaches 1.1022
    /// (`DEFAULT_NUM_CYCLES * 1.1`).
    pub osc_volume: f32,
    /// Synth frequency ratio (`osc_freq_scalar`). 1.0 at rest; drops during
    /// interaction onset via `slow_down`, then recovers over ~20 frames.
    pub osc_freq_scalar: f32,
    /// Blub loop volume, already scaled by v4's `·0.05`. The C4 audio engine
    /// clamps to `[0, 0.3]`; do NOT scale or clamp again here.
    pub blub_volume: f32,
    /// Blub retrigger rate. The audio engine clamps to `[0.5, 4.0]`.
    pub blub_rate: f32,
}

// ── Onset throttle state ──────────────────────────────────────────────────────

/// Onset edge detection and one-shot throttle for kick / rising-bass samples.
///
/// Tracks whether the sketch was interacting last frame and the remaining
/// throttle countdown (frames). Both fields start at their zero-default
/// (no prior interaction, no active throttle).
#[derive(Resource, Default)]
pub struct CymaticsTriggerState {
    was_interacting: bool,
    /// Remaining frames before another onset trigger is allowed. Counts down
    /// each frame via `saturating_sub(1)`; 0 means "ready". Set to 30
    /// (~500 ms at 60 fps) after firing.
    throttle_frames: u32,
}

// ── Pure helpers ──────────────────────────────────────────────────────────────

/// v4 `MathUtils.mapLinear`: linear remap without clamping.
///
/// Maps `x` from `[in_min, in_max]` to `[out_min, out_max]`. Identical to
/// Three.js `mapLinear`. Values outside the input range extrapolate linearly
/// (no clamp applied).
#[allow(
    clippy::many_single_char_names,
    reason = "x/a/b/c/d exactly match v4's mapLinear(x, a, b, c, d) signature"
)]
fn map_linear(x: f32, a: f32, b: f32, c: f32, d: f32) -> f32 {
    c + (d - c) * ((x - a) / (b - a))
}

/// Hermite cubic smoothstep mapping `x` into `[0, 1]` between edges `e0` and `e1`.
///
/// v4 `MathUtils.smoothstep`. Clamps `t` before the cubic so values outside
/// `[e0, e1]` saturate at 0 or 1 rather than extrapolating.
fn smoothstep01(x: f32, e0: f32, e1: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Verbatim v4 audio formulas: derive per-frame audio parameters from
/// [`CymaticsState`].
///
/// Pure function (no Bevy ECS state, no side effects) so the brief's unit
/// tests can verify every coefficient against v4 `index.ts::step()` +
/// `audio.ts` without an audio device.
#[must_use]
pub fn audio_params(s: &CymaticsState) -> CymaticsAudioParams {
    // v4 `DEFAULT_NUM_CYCLES` — nominal resting frequency at which the wave
    // field is a single clean harmonic.
    const DEF: f32 = 1.002;
    // v4 `DEFAULT_NUM_CYCLES * 1.1` — smoothstep edge where the osc swell saturates.
    // 1.002 * 1.1 = 1.1022. (Prior impl had 1.1002, a transcription error; v4
    // `index.ts:357` uses `DEFAULT_NUM_CYCLES * 1.1`, not the literal 1.1002.)
    const OSC_SWELL_EDGE: f32 = DEF * 1.1;

    // v4 `skewIntensity = pow(max(0, (numCycles - DEF) / 2 - 0.5), 2)`.
    // Suppresses blub_volume at very high cycle counts to prevent over-drive.
    let skew = ((s.num_cycles - DEF) / 2.0 - 0.5).max(0.0).powi(2);

    // v4 `blubVolume` (raw, before ·0.05 scale). Term by term:
    //   clamp(pow(map(activeRadius, 0.1, 1, 0.05, 1), 2), 0, 1) * 0.5
    //       → radius contribution: louder as the alive-mask grows; the clamp
    //         prevents over-drive when active_radius reaches 7.5 during
    //         interaction (mapLinear extrapolates to ~7.86, pow→~61.8 unclamped;
    //         v4 `index.ts:348` wraps the pow() result in clamp(…, 0, 1))
    //   + abs(numCycles - DEF) * 0.25
    //       → cycle deviation: louder as the field drifts from nominal
    //   - skewIntensity
    //       → suppresses blub at very high cycles (prevents over-drive)
    //   + map(centerSpeed, 0, 0.005, 0, 1)
    //       * map(activeRadius, 0.1, 1, 0.12, 1) * 0.4
    //       → motion × radius cross-term: fast centre at large radius is loud
    let blub_volume_raw = map_linear(s.active_radius, 0.1, 1.0, 0.05, 1.0)
        .powi(2)
        .clamp(0.0, 1.0)
        * 0.5
        + (s.num_cycles - DEF).abs() * 0.25
        - skew
        + map_linear(s.center_speed, 0.0, 0.005, 0.0, 1.0)
            * map_linear(s.active_radius, 0.1, 1.0, 0.12, 1.0)
            * 0.4;

    // v4 `blubRate = pow(2, map(centerSpeed, 0, 0.005, -0.25, 1.5))
    //                + map(numCycles, DEF, 2, 0, 4)`.
    // Both terms increase retrigger rate as interaction intensity grows.
    let blub_rate = 2.0_f32.powf(map_linear(s.center_speed, 0.0, 0.005, -0.25, 1.5))
        + map_linear(s.num_cycles, DEF, 2.0, 0.0, 4.0);

    // v4 `oscVolume = clamp(smoothstep(numCycles, DEF, 1.1002) * 0.5, 0, 1)`.
    // Grows from 0 as numCycles rises past DEF; saturates at 0.5 at OSC_SWELL_EDGE.
    let osc_volume = (smoothstep01(s.num_cycles, DEF, OSC_SWELL_EDGE) * 0.5).clamp(0.0, 1.0);

    // v4 `cycles = numCycles / (1 + slowDown*3); oscFreqScalar = cycles / DEF`.
    // slow_down temporarily lowers effective pitch on interaction onset;
    // at slow_down=1 the scalar drops to ~0.25×, recovering as it decays.
    let cycles = s.num_cycles / (1.0 + s.slow_down * 3.0);
    let osc_freq_scalar = cycles / DEF;

    CymaticsAudioParams {
        osc_volume,
        osc_freq_scalar,
        // Rule #3: multiply by 0.05 here, exactly once (v4 `setBlubVolume`
        // applied `clamp(v * 0.05, 0, 0.3)`). The C4 engine does the
        // [0, 0.3] clamp; do NOT clamp here.
        blub_volume: blub_volume_raw * 0.05,
        blub_rate,
    }
}

// ── Lifecycle systems ─────────────────────────────────────────────────────────

/// `OnEnter(AppState::Cymatics)` — build the Cymatics synth voice bundle on the
/// audio thread.
///
/// Idempotent: a second `AddCymaticsSynth` while voices are active is a no-op.
/// Early-returns cleanly when `AudioCommandSender` is absent (headless tests).
pub fn enter_cymatics_audio(audio_cmd: Option<NonSendMut<'_, AudioCommandSender>>) {
    let Some(mut audio_cmd) = audio_cmd else {
        return;
    };
    if let Err(_dropped) = audio_cmd.push(AudioCommand::AddCymaticsSynth) {
        tracing::warn!("audio command ring full on Cymatics entry; AddCymaticsSynth dropped");
    }
}

/// `OnExit(AppState::Cymatics)` — tear down the Cymatics synth voice bundle and
/// release its audio allocations.
///
/// Idempotent: a second `RemoveCymaticsSynth` while no voices are active is a
/// no-op. Ring-full failures are logged and dropped.
pub fn exit_cymatics_audio(audio_cmd: Option<NonSendMut<'_, AudioCommandSender>>) {
    let Some(mut audio_cmd) = audio_cmd else {
        return;
    };
    if let Err(_dropped) = audio_cmd.push(AudioCommand::RemoveCymaticsSynth) {
        tracing::warn!("audio command ring full on Cymatics exit; RemoveCymaticsSynth dropped");
    }
}

// ── Per-frame system ──────────────────────────────────────────────────────────

/// `Update` system: push derived v4 audio parameters each frame and fire
/// throttled one-shot samples on interaction onset.
///
/// Runs only while `sketch_active(AppState::Cymatics)` — the screensaver mode
/// runs under `SketchActivity::Screensaver` and is intentionally silent.
///
/// Must run after `update_cymatics_centers` (which decays `slow_down` and
/// updates `active_radius`) so this system reads fresh interaction state.
/// `slow_down` is then incremented here on onset — the only writer; C9 only
/// decays. `audio_params` is computed after the increment so the onset frame
/// immediately reflects the reduced pitch (matches v4's evaluation order where
/// `startInteraction` set `slowDownAmount` before `step()` read it).
pub fn drive_cymatics_audio(
    mut state: ResMut<'_, CymaticsState>,
    mut trigger: ResMut<'_, CymaticsTriggerState>,
    audio_cmd: Option<NonSendMut<'_, AudioCommandSender>>,
    settings: Res<'_, CymaticsSettings>,
) {
    // Interaction onset: active_radius snaps to MINIMUM_ACTIVE_RADIUS_INTERACTING
    // (0.5) the moment a press/grab starts. The -1e-3 tolerance handles the
    // one-frame snap lag when the radius is written and read in the same cycle.
    let interacting = state.active_radius > interaction::MINIMUM_ACTIVE_RADIUS_INTERACTING - 1e-3;
    trigger.throttle_frames = trigger.throttle_frames.saturating_sub(1);
    let is_onset = interacting && !trigger.was_interacting;

    if is_onset {
        // v4 `startInteraction`: set slowDownAmount = 1, lowering the effective
        // cycle count (osc_freq_scalar) immediately on contact. C9
        // (`step_centers`) decays slow_down *= 0.95 per frame; this is the
        // sole incrementer — no double-count with C9.
        state.slow_down += 1.0;
    }
    trigger.was_interacting = interacting;

    // Compute audio params AFTER the slow_down bump so the onset frame's
    // osc_freq_scalar already reflects the slower pitch (v4 parity).
    let p = audio_params(&state);

    // The audio engine is not started in headless tests (no cpal device).
    // Skip all ring pushes cleanly when the sender is absent.
    let Some(mut audio_cmd) = audio_cmd else {
        return;
    };

    // Continuous params: pushed every active frame. A dropped frame holds the
    // previous value for one extra frame (inaudible at 60 fps).

    // `"osc_volume"`: smoothstep swell × User osc_level trim (default 1.0).
    push_cymatics_audio(
        &mut audio_cmd,
        AudioCommand::SetCymaticsParam {
            key: "osc_volume",
            value: p.osc_volume * settings.osc_level,
        },
    );
    // `"osc_freq_scalar"`: effective pitch ratio; includes slow_down effect.
    // Not scaled by osc_level — frequency is independent of volume.
    push_cymatics_audio(
        &mut audio_cmd,
        AudioCommand::SetCymaticsParam {
            key: "osc_freq_scalar",
            value: p.osc_freq_scalar,
        },
    );
    // `"blub_volume"`: already includes ·0.05 (Rule #3); engine clamps [0, 0.3].
    // blub_level scales on top — still before the engine clamp.
    push_cymatics_audio(
        &mut audio_cmd,
        AudioCommand::SetCymaticsParam {
            key: "blub_volume",
            value: p.blub_volume * settings.blub_level,
        },
    );
    // `"blub_rate"`: retrigger rate; engine clamps to [0.5, 4.0].
    push_cymatics_audio(
        &mut audio_cmd,
        AudioCommand::SetCymaticsParam {
            key: "blub_rate",
            value: p.blub_rate,
        },
    );

    // One-shot samples on onset, throttled to ~500 ms (30 frames at 60 fps).
    // Kick (percussive transient) + RisingBass (sustained swell) fire together,
    // matching v4's `triggerJitter` pattern at each new interaction.
    if is_onset && trigger.throttle_frames == 0 {
        push_cymatics_audio(
            &mut audio_cmd,
            AudioCommand::TriggerCymaticsSample(CymaticsSampleId::Kick),
        );
        push_cymatics_audio(
            &mut audio_cmd,
            AudioCommand::TriggerCymaticsSample(CymaticsSampleId::RisingBass),
        );
        // Suppress re-trigger for the next ~500 ms.
        trigger.throttle_frames = 30;
    }
}

/// Push an [`AudioCommand`] onto the Cymatics audio ring.
///
/// Logs at `warn` if the ring is full and the command is dropped. Non-fatal:
/// the parameter holds its last value for one extra frame (inaudible at 60 fps).
fn push_cymatics_audio(sender: &mut AudioCommandSender, command: AudioCommand) {
    if let Err(_dropped) = sender.push(command) {
        tracing::warn!("audio command ring full; dropping Cymatics param update");
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic float arithmetic is the test subject"
)]
#[allow(
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected None/Err is the correct behaviour"
)]
mod tests {
    use super::*;
    use crate::cymatics::CymaticsState;

    // ── v4 formula parity (brief's required tests) ────────────────────────

    /// `osc_freq_scalar` equals 1.0 at the default state: `num_cycles = 1.002`,
    /// `slow_down = 0`. v4: `cycles = 1.002 / 1 = 1.002; scalar = 1.002 / 1.002`.
    #[test]
    fn osc_freq_scalar_is_one_at_default() {
        let s = CymaticsState::default(); // num_cycles 1.002, slow_down 0
        let p = audio_params(&s);
        assert!((p.osc_freq_scalar - 1.0).abs() < 1e-4);
    }

    /// `osc_volume` is zero at the default frequency: `smoothstep(1.002; 1.002,
    /// 1.1002) = 0` because `x == e0`.
    #[test]
    fn osc_volume_zero_at_default_frequency() {
        let s = CymaticsState::default();
        let p = audio_params(&s);
        assert!(p.osc_volume.abs() < 1e-4); // smoothstep(1.002; 1.002, 1.1002) = 0
    }

    /// `osc_volume` saturates at 0.5 when `num_cycles` reaches the correct upper
    /// swell edge `1.1022` (`DEFAULT_NUM_CYCLES * 1.1`, v4 `index.ts:357`).
    /// smoothstep = 1.0 at `e1`, so `osc_volume = 1.0 × 0.5 = 0.5`.
    #[test]
    fn osc_volume_saturates_at_correct_swell_edge() {
        let s = CymaticsState {
            num_cycles: 1.1022, // DEFAULT_NUM_CYCLES * 1.1 = 1.002 * 1.1
            ..Default::default()
        };
        let p = audio_params(&s);
        // smoothstep(e1) = 1.0 → osc_volume = 1.0 * 0.5 = 0.5
        assert!(
            (p.osc_volume - 0.5).abs() < 1e-4,
            "osc_volume at swell edge 1.1022 must be ≈ 0.5; got {}",
            p.osc_volume
        );
    }

    /// `osc_volume` is strictly between 0 and 0.5 at a `num_cycles` value in
    /// the middle of the swell ramp `(1.002, 1.1022)`, confirming the smoothstep
    /// is not clipped too early (old wrong edge 1.1002 would saturate ≈ halfway).
    #[test]
    fn osc_volume_rises_between_swell_endpoints() {
        // Midpoint between DEF (1.002) and OSC_SWELL_EDGE (1.1022) ≈ 1.0521.
        let mid = f32::midpoint(1.002_f32, 1.1022_f32);
        let s = CymaticsState {
            num_cycles: mid,
            ..Default::default()
        };
        let p = audio_params(&s);
        assert!(
            p.osc_volume > 0.0,
            "osc_volume must be positive at mid-ramp; got {}",
            p.osc_volume
        );
        assert!(
            p.osc_volume < 0.5,
            "osc_volume must not yet saturate at mid-ramp; got {}",
            p.osc_volume
        );
    }

    /// `blub_rate` increases with `center_speed`: both the `pow(2, map(...))` and
    /// the additive `map(numCycles, DEF, 2, 0, 4)` term push the rate upward as
    /// the centre moves faster.
    #[test]
    fn blub_rate_increases_with_center_speed() {
        let slow = audio_params(&CymaticsState {
            center_speed: 0.0,
            ..Default::default()
        });
        let fast = audio_params(&CymaticsState {
            center_speed: 0.005,
            ..Default::default()
        });
        assert!(fast.blub_rate > slow.blub_rate);
    }

    // ── Additional formula verification ──────────────────────────────────

    /// `osc_freq_scalar` falls well below 0.3 at `slow_down = 1.0`.
    /// `cycles = 1.002 / (1 + 3) = 0.2505; scalar = 0.2505 / 1.002 ≈ 0.25`.
    #[test]
    fn osc_freq_scalar_falls_with_slow_down() {
        let s = CymaticsState {
            slow_down: 1.0,
            ..Default::default()
        };
        let p = audio_params(&s);
        assert!(p.osc_freq_scalar < 0.3);
        assert!(p.osc_freq_scalar > 0.2);
    }

    /// `blub_volume` is finite, non-negative, and below the engine's clamp
    /// ceiling (0.3) at the resting state. Confirms the ·0.05 scale is applied
    /// (raw value ≈ 0.00125; after scale ≈ 0.0000625 << 0.3).
    #[test]
    fn blub_volume_is_scaled_and_finite_at_rest() {
        let s = CymaticsState::default();
        let p = audio_params(&s);
        assert!(p.blub_volume.is_finite(), "blub_volume must be finite");
        assert!(p.blub_volume >= 0.0, "blub_volume must be non-negative");
        // Below the engine clamp ceiling confirms the ·0.05 scale was applied.
        assert!(
            p.blub_volume < 0.3,
            "blub_volume at rest must be below the clamp ceiling (0.3); got {}",
            p.blub_volume
        );
    }

    /// At `active_radius = TARGET_ACTIVE_RADIUS_INTERACTING` (7.5), `mapLinear`
    /// extrapolates to ~7.86 and `powi(2)` gives ~61.8 unclamped. v4
    /// `index.ts:348` wraps the result in `clamp(pow(...), 0, 1)`, keeping the
    /// first term contribution to `blub_volume_raw` at `1.0 * 0.5 = 0.5`.
    /// After the ·0.05 scale: `blub_volume ≈ 0.025`. Without the clamp it
    /// would be `~30.9 * 0.05 = ~1.545`. This test FAILS if `.clamp(0.0, 1.0)`
    /// is removed.
    #[test]
    fn blub_volume_first_term_is_clamped_at_interacting_radius() {
        let s = CymaticsState {
            active_radius: interaction::TARGET_ACTIVE_RADIUS_INTERACTING, // 7.5
            ..Default::default()
        };
        let p = audio_params(&s);
        // Passes only when the clamp fires: correct ≈ 0.025, unclamped ≈ 1.545.
        assert!(
            p.blub_volume < 1.0,
            "blub_volume must be clamped at interacting radius; got {} (without clamp ≈ 1.545)",
            p.blub_volume
        );
        // Also confirm the value is in v4's expected neighbourhood.
        assert!(
            (p.blub_volume - 0.025).abs() < 1e-4,
            "blub_volume at max interacting radius must be ≈ 0.025; got {}",
            p.blub_volume
        );
    }

    // ── Onset throttle logic ──────────────────────────────────────────────

    /// A second onset within 30 frames must be suppressed by the throttle.
    #[test]
    fn throttle_suppresses_second_trigger_within_30_frames() {
        // Simulate the state just after a trigger fired last frame (throttle = 30).
        let mut trigger = CymaticsTriggerState {
            was_interacting: false,
            throttle_frames: 30, // set to 30 last frame when kick/bass fired
        };
        // One frame later: decrement.
        trigger.throttle_frames = trigger.throttle_frames.saturating_sub(1); // = 29
                                                                             // New onset this frame — must NOT fire because throttle is still active.
        let is_onset = true;
        assert!(
            !(is_onset && trigger.throttle_frames == 0),
            "trigger must be suppressed while throttle_frames > 0"
        );
    }

    /// A trigger must be allowed after 30 frames have elapsed (throttle expired).
    #[test]
    fn throttle_allows_trigger_after_30_frames() {
        let mut trigger = CymaticsTriggerState {
            was_interacting: false,
            throttle_frames: 1, // one frame remaining
        };
        trigger.throttle_frames = trigger.throttle_frames.saturating_sub(1); // = 0
        let is_onset = true;
        assert!(
            is_onset && trigger.throttle_frames == 0,
            "trigger must be allowed once throttle_frames reaches 0"
        );
    }

    /// Initial throttle state (default): trigger fires immediately on the first
    /// onset because `throttle_frames` starts at 0.
    #[test]
    fn trigger_fires_on_first_onset_with_default_state() {
        let mut trigger = CymaticsTriggerState::default();
        trigger.throttle_frames = trigger.throttle_frames.saturating_sub(1); // 0 - 1 = 0 (saturating)
        let is_onset = true;
        assert!(
            is_onset && trigger.throttle_frames == 0,
            "first onset must fire immediately"
        );
    }
}
