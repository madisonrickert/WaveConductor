//! Audio coupling: two per-frame scalars drive the [`FlameSynth`] voice.
//!
//! ## Approach (ENVELOPE-PRIMARY)
//!
//! v4's `VelocityTrackerVisitor` walked the live IFS tree each frame and
//! measured how far points had moved since the last frame — a per-particle
//! CPU stat unavailable in v5 without a GPU readback (a hard constraint the
//! plan rules out). Instead this module derives a single scalar,
//! "morph-energy", from two CPU-only proxies for how much the fractal is
//! changing shape right now:
//!
//! - [`flame_cx_rate`] — the *analytic* time-derivative of the attractor's
//!   `cX` oscillation (`|d(cX)/dt|`). The fractal keeps morphing on its own as
//!   `cX` sweeps, even with no pointer input, so this alone gives the "breathes
//!   on its own" quality v4 had.
//! - `warp_speed` — how fast the pointer/hand warp offset is moving
//!   (`|Δwarp_input| / dt`), the CPU-visible stand-in for user-driven motion.
//!
//! Both are scaled into v4's `velocityFactor` range (clamped at 0.06 inside
//! [`FlameSynth::set_param`]'s `"morph_energy"` arm) via [`CX_ENERGY_WEIGHT`]
//! and [`WARP_ENERGY_WEIGHT`], then summed and run through [`step_flame_energy`]
//! — an attack/release follow filter (the `step_dots_envelope` shape) so the
//! synth sees a smoothed, click-free scalar rather than a raw per-frame spike.
//!
//! ## What this writes each frame
//!
//! [`drive_flame_audio`] pushes three [`AudioCommand::SetFlameParam`] commands
//! onto the lock-free [`AudioCommandSender`] ring every frame:
//!
//! - `"morph_energy"` = the smoothed [`FlameMorphEnergy`] envelope.
//! - `"camera_distance"` = [`FlameCamera::distance`] (the orbit radius —
//!   zooming in raises `camera_gain` inside the synth).
//! - `"volume_scale"` = `settings.synth_volume_scale × (1 - fade.alpha())` —
//!   the [`ScreensaverFade`] envelope IS the screensaver audio ramp: it fades
//!   the synth out during the fade-in to attract mode and back in during the
//!   wake fade-out, with no hard mute.
//!
//! The rest of the per-frame surface (`filter_freq`, `filter_q`, `noise_scale`,
//! `has_noise`, `is_major`, `chord_degree`, `density`, `chord_energy`) is
//! name-derived, not per-frame: [`push_flame_config`] pushes it once on entry
//! ([`enter_flame_audio`]) and once per rebuild (`watch_flame_name`, preceded
//! there by an instant `"duck_pulse"` — v4's anti-click mute before the swap,
//! which the synth's `follow(0.016)` smoother turns into a fast dip rather
//! than an audible pop).
//!
//! ## Ring-full handling
//!
//! If the audio ring is full the dropped command logs at `warn` once per
//! occurrence and that frame's push is skipped — the parameter holds its last
//! value for one extra frame, mirroring the `drive_dots_audio` idiom.
//!
//! [`FlameSynth`]: wc_core::audio::flame_synth::FlameSynth
//! [`FlameSynth::set_param`]: wc_core::audio::flame_synth::FlameSynth::set_param

use bevy::ecs::system::NonSendMut;
use bevy::prelude::*;

use wc_core::audio::command::AudioCommand;
use wc_core::audio::ring::AudioCommandSender;
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;

use super::branches::NameAudioConfig;
use super::settings::FlameSettings;
use super::systems::camera::FlameCamera;
use super::systems::sim_params::FlameState;

// ── v4 velocity-range weights ───────────────────────────────────────────────

/// Weight on [`flame_cx_rate`] before it enters the synth's v4 velocity
/// curves (which clamp `morph_energy × noise_gain_scale` at 0.06 — see
/// `FlameSynth::set_param`'s `"morph_energy"` arm). Primary ear-tune surface,
/// alongside [`WARP_ENERGY_WEIGHT`] and [`FlameSettings::morph_energy_scale`].
const CX_ENERGY_WEIGHT: f32 = 0.03;

/// Weight on the pointer/hand `warp_speed` term. See [`CX_ENERGY_WEIGHT`].
const WARP_ENERGY_WEIGHT: f32 = 0.01;

// ── Analytic morph-rate ─────────────────────────────────────────────────────

/// `|d(cX)/dt|` in closed form, replacing v4's `VelocityTrackerVisitor` as the
/// time-driven morph source.
///
/// [`flame_cx`] is `cX = 2·σ(u) - 1` with `u = 6·sin(t/3)`. By the chain rule:
///
/// ```text
/// du/dt   = 6·cos(t/3)/3
/// dσ/du   = σ(u)·(1 - σ(u))            (logistic derivative)
/// d(cX)/dt = 2·σ'(u)·du/dt = 2·σ'(u)·6·cos(t/3)/3
/// ```
///
/// Always `>= 0` after the `abs()`: the synth only cares about *how fast*
/// the attractor is morphing, not the sign of the sweep.
#[must_use]
pub fn flame_cx_rate(elapsed_secs: f64) -> f32 {
    let t_third = elapsed_secs / 3.0;
    let u = 6.0 * t_third.sin();
    // Logistic sigmoid and its derivative, matching flame_cx's inner term.
    let sigmoid = 1.0 / (1.0 + (-u).exp());
    let sigmoid_prime = sigmoid * (1.0 - sigmoid);
    let rate = (2.0 * sigmoid_prime * 6.0 * t_third.cos() / 3.0).abs();
    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "f64 -> f32 on a bounded, already-abs'd rate; matches flame_cx's own f64->f32 \
                  narrowing at the same call boundary"
    )]
    {
        rate as f32
    }
}

// ── Morph-energy envelope ───────────────────────────────────────────────────

/// Smoothed morph-energy scalar pushed to the synth as `"morph_energy"`.
///
/// A non-negative envelope in `[0, 1]` that follows the raw
/// `cX_rate + warp_speed` proxy with asymmetric attack/release rates
/// ([`FlameSettings::synth_attack_ms`] / [`FlameSettings::synth_release_ms`]),
/// avoiding an audible per-frame stairstep. Advanced every frame by
/// [`drive_flame_audio`]; persists across `OnEnter`/`OnExit` cycles (the synth
/// itself is rebuilt on entry, so a residual non-zero value on re-entry just
/// means the envelope resumes from its last state — the same tradeoff
/// `DotsAudioEnvelope` makes).
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct FlameMorphEnergy(pub f32);

/// Advance [`FlameMorphEnergy`] by one frame: an exponential follow toward
/// `raw`, using `attack_rate` while rising and `release_rate` while falling,
/// clamped to `[0, 1]` (the `step_dots_envelope` shape, generalized from a
/// binary target to a continuous one).
pub(crate) fn step_flame_energy(
    env: f32,
    raw: f32,
    dt: f32,
    attack_rate: f32,
    release_rate: f32,
) -> f32 {
    let rate = if raw > env { attack_rate } else { release_rate };
    // `(rate * dt).min(1.0)` prevents overshoot on a large frame delta (a
    // hitch); the outer clamp guards against floating-point noise escaping
    // [0, 1] even though `raw` and `env` should already be non-negative.
    (env + (raw - env) * (rate * dt).min(1.0)).clamp(0.0, 1.0)
}

// ── Per-frame system ─────────────────────────────────────────────────────────

/// `Update` system (registered under BOTH `sketch_active(Flame)` and
/// `in_screensaver(Flame)`, `.after(update_flame_camera)` in each gate):
/// advances [`FlameMorphEnergy`] and pushes the per-frame audio param surface.
///
/// The envelope is advanced **before** the `audio_cmd` early-return so
/// headless tests without an [`AudioCommandSender`] can still observe
/// [`FlameMorphEnergy`] — the `drive_dots_audio` idiom.
pub fn drive_flame_audio(
    time: Res<'_, Time>,
    state: Res<'_, FlameState>,
    camera: Res<'_, FlameCamera>,
    settings: Res<'_, FlameSettings>,
    fade: Res<'_, ScreensaverFade>,
    mut energy: ResMut<'_, FlameMorphEnergy>,
    mut last_warp: Local<'_, Vec2>,
    audio_cmd: Option<NonSendMut<'_, AudioCommandSender>>,
) {
    let dt = time.delta_secs();

    // Analytic morph source: the attractor keeps sweeping even with no
    // pointer input, so the fractal "breathes" on its own.
    let cx_rate = flame_cx_rate(time.elapsed_secs_f64());

    // User-driven morph source: how fast the warp offset moved this frame.
    let warp_speed = if dt > 0.0 {
        (state.warp_input - *last_warp).length() / dt
    } else {
        0.0
    };
    *last_warp = state.warp_input;

    let raw = (cx_rate * CX_ENERGY_WEIGHT + warp_speed * WARP_ENERGY_WEIGHT)
        * settings.morph_energy_scale;

    // ms -> s^-1, matching the `drive_dots_audio` rate derivation.
    let attack_rate = 1000.0 / settings.synth_attack_ms;
    let release_rate = 1000.0 / settings.synth_release_ms;
    energy.0 = step_flame_energy(energy.0, raw, dt, attack_rate, release_rate);

    // The audio engine is not started in headless integration tests (no cpal
    // device). Skip ring pushes cleanly when the sender is absent.
    let Some(mut audio_cmd) = audio_cmd else {
        return;
    };

    push_flame_param(&mut audio_cmd, "morph_energy", energy.0);
    push_flame_param(&mut audio_cmd, "camera_distance", camera.distance);
    // The ScreensaverFade multiplier IS the smooth screensaver audio ramp:
    // out during fade-in to attract mode, back in during the wake fade-out.
    push_flame_param(
        &mut audio_cmd,
        "volume_scale",
        settings.synth_volume_scale * (1.0 - fade.alpha()),
    );
}

// ── Enter / exit lifecycle ───────────────────────────────────────────────────

/// `OnEnter(AppState::Flame)`: push `AddFlameSynth` to build the synth voice
/// graph, then the full name-derived config so the very first bloom sounds
/// correct rather than waiting on the first rebuild.
///
/// Ordered after [`super::systems::spawn::spawn_flame`] (which inserts
/// [`FlameState`]) in [`super::FlamePlugin::build`]'s `OnEnter` chain.
/// Early-returns cleanly when [`AudioCommandSender`] is absent (headless
/// tests: no cpal device). Mirrors `crate::dots::enter_dots_audio`.
pub fn enter_flame_audio(
    state: Res<'_, FlameState>,
    settings: Res<'_, FlameSettings>,
    audio_cmd: Option<NonSendMut<'_, AudioCommandSender>>,
) {
    let Some(mut audio_cmd) = audio_cmd else {
        return;
    };
    if let Err(_dropped) = audio_cmd.push(AudioCommand::AddFlameSynth) {
        tracing::warn!("audio command ring full on Flame entry; AddFlameSynth dropped");
    }
    push_flame_config(
        &mut audio_cmd,
        &state.spec.audio,
        settings.chord_energy_scale,
    );
}

/// `OnExit(AppState::Flame)`: push `RemoveFlameSynth` to tear down the synth
/// voice graph and release its audio-thread allocations.
///
/// Idempotent (a second `RemoveFlameSynth` while none is active is a no-op,
/// handled by the audio engine). Early-returns cleanly when
/// [`AudioCommandSender`] is absent. Mirrors `crate::dots::exit_dots_audio`.
pub fn exit_flame_audio(audio_cmd: Option<NonSendMut<'_, AudioCommandSender>>) {
    let Some(mut audio_cmd) = audio_cmd else {
        return;
    };
    if let Err(_dropped) = audio_cmd.push(AudioCommand::RemoveFlameSynth) {
        tracing::warn!("audio command ring full on Flame exit; RemoveFlameSynth dropped");
    }
}

// ── Name-derived config push ─────────────────────────────────────────────────

/// Push the whole name-derived (non-per-frame) audio param surface: filter
/// character, noise/chord flavor, and register.
///
/// `chord_energy` is [`FlameSettings::chord_energy_scale`] — the operator's
/// stand-in for v4's `count^2 / 8` box-count factor (no v5 source for
/// `count`). Called from [`enter_flame_audio`] and from `watch_flame_name`
/// after a rebuild (there, preceded by an instant `"duck_pulse"` push — see
/// the module docs).
pub(crate) fn push_flame_config(
    sender: &mut AudioCommandSender,
    audio: &NameAudioConfig,
    chord_energy: f32,
) {
    push_flame_param(sender, "filter_freq", audio.filter_freq);
    push_flame_param(sender, "filter_q", audio.filter_q);
    push_flame_param(sender, "noise_scale", audio.noise_gain_scale);
    push_flame_param(sender, "has_noise", f32::from(audio.has_noise));
    push_flame_param(sender, "is_major", f32::from(audio.is_major));
    push_flame_param(sender, "chord_degree", audio.chord_degree);
    push_flame_param(sender, "density", audio.pseudo_density);
    push_flame_param(sender, "chord_energy", chord_energy);
}

/// Push a single `SetFlameParam` command, logging (and dropping) on a full
/// ring. Non-fatal: the parameter holds its last value for one extra frame.
fn push_flame_param(sender: &mut AudioCommandSender, key: &'static str, value: f32) {
    if let Err(_dropped) = sender.push(AudioCommand::SetFlameParam { key, value }) {
        tracing::warn!("audio command ring full; dropping Flame param update ({key})");
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    use crate::flame::branches::build_flame_spec;
    use crate::flame::levels::LevelLayout;
    use crate::flame::systems::sim_params::flame_cx;

    // Rates matching FlameSettings defaults: attack = 1000/120 s^-1, release = 1000/600 s^-1.
    const TEST_ATTACK_RATE: f32 = 1000.0 / 120.0;
    const TEST_RELEASE_RATE: f32 = 1000.0 / 600.0;

    // ── flame_cx_rate: analytic vs. numeric, and turning points ───────────

    /// `flame_cx_rate` agrees with a central finite difference of `flame_cx`
    /// at 20 sample points.
    #[test]
    fn flame_cx_rate_matches_finite_difference() {
        let h = 1e-4_f64;
        for i in 0..20_u32 {
            let t = f64::from(i) * 0.31 + 0.02;
            let plus = f64::from(flame_cx(t + h));
            let minus = f64::from(flame_cx(t - h));
            let numeric = ((plus - minus) / (2.0 * h)).abs();
            let analytic = f64::from(flame_cx_rate(t));
            assert!(
                (analytic - numeric).abs() < 1e-3,
                "t={t}: analytic={analytic}, numeric={numeric}"
            );
        }
    }

    /// The rate vanishes at the oscillation's turning points, where
    /// `t/3 = pi/2` makes `cos(t/3) = 0`.
    #[test]
    fn flame_cx_rate_zero_at_turning_point() {
        let t = 3.0 * std::f64::consts::FRAC_PI_2;
        let rate = flame_cx_rate(t);
        assert!(
            rate.abs() < 1e-3,
            "rate should vanish at turning point; got {rate}"
        );
    }

    // ── step_flame_energy: rise/decay/clamp shapes ─────────────────────────

    /// Envelope rises toward a positive `raw` target.
    #[test]
    fn step_flame_energy_rises_toward_raw() {
        let after = step_flame_energy(0.0, 0.5, 1.0 / 60.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
        assert!(
            after > 0.0,
            "envelope must rise toward raw > env; got {after}"
        );
        assert!(after <= 0.5, "envelope must not overshoot raw; got {after}");
    }

    /// Envelope decays toward a lower `raw` target.
    #[test]
    fn step_flame_energy_decays_toward_raw() {
        let after = step_flame_energy(0.5, 0.0, 1.0 / 60.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
        assert!(
            after < 0.5,
            "envelope must decay toward raw < env; got {after}"
        );
        assert!(after >= 0.0, "envelope must stay >= 0.0; got {after}");
    }

    /// A huge `dt` (frame hitch) must not push the envelope outside `[0, 1]`.
    #[test]
    fn step_flame_energy_stays_in_unit_interval_with_extreme_dt() {
        let at_peak = step_flame_energy(0.5, 1.0, 100.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
        assert!(
            (0.0..=1.0).contains(&at_peak),
            "envelope out of [0,1] on attack with dt=100: {at_peak}"
        );
        let at_floor = step_flame_energy(0.5, 0.0, 100.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
        assert!(
            (0.0..=1.0).contains(&at_floor),
            "envelope out of [0,1] on release with dt=100: {at_floor}"
        );
    }

    /// Envelope rises monotonically across frames while `raw` stays above it.
    #[test]
    fn step_flame_energy_rises_monotonically_across_frames() {
        let mut env = 0.0_f32;
        for frame in 0..20 {
            let next = step_flame_energy(env, 1.0, 1.0 / 60.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
            assert!(
                next >= env,
                "envelope decreased on active frame {frame}: {env} -> {next}"
            );
            env = next;
        }
        assert!(
            env > 0.0,
            "envelope must have risen above 0 after 20 active frames"
        );
    }

    /// Envelope decays monotonically across frames while `raw` stays below it.
    #[test]
    fn step_flame_energy_decays_monotonically_across_frames() {
        let mut env = 1.0_f32;
        for frame in 0..20 {
            let next = step_flame_energy(env, 0.0, 1.0 / 60.0, TEST_ATTACK_RATE, TEST_RELEASE_RATE);
            assert!(
                next <= env,
                "envelope increased on idle frame {frame}: {env} -> {next}"
            );
            env = next;
        }
        assert!(
            env < 1.0,
            "envelope must have decayed below 1 after 20 idle frames"
        );
    }

    // ── drive_flame_audio: world-level, audio-absent behavior ─────────────

    /// `drive_flame_audio` advances `FlameMorphEnergy` when run without an
    /// `AudioCommandSender` (headless mode) — the Dots headless pattern.
    #[test]
    fn drive_flame_audio_advances_energy_without_audio_sender() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = World::new();

        let mut time = Time::<()>::default();
        time.advance_by(std::time::Duration::from_millis(16));
        world.insert_resource(time);

        let spec = build_flame_spec("madison");
        let layout = LevelLayout::build(4, 100_000.0);
        world.insert_resource(FlameState {
            spec,
            layout,
            last_name: "madison".into(),
            last_target_points: 100_000.0,
            c_x: 0.0,
            warp_input: Vec2::new(0.5, 0.2),
            complexity: 1.0,
        });
        world.insert_resource(FlameCamera::default());
        world.insert_resource(FlameSettings::default());
        world.insert_resource(ScreensaverFade::default());
        world.insert_resource(FlameMorphEnergy::default());
        // No AudioCommandSender inserted — system must skip ring pushes cleanly.

        world
            .run_system_once(drive_flame_audio)
            .expect("drive_flame_audio must run without error");

        let energy = world.resource::<FlameMorphEnergy>().0;
        assert!(
            energy > 0.0,
            "drive_flame_audio must raise the envelope from motion + oscillation; got {energy}"
        );
        assert!(energy <= 1.0, "envelope must stay <= 1.0; got {energy}");
    }
}
