//! Hand-tracking provider installation and live switching.
//!
//! The user-facing "Tracking provider" dropdown
//! ([`wc_core::settings::HandProviderChoice`], persisted in
//! `HandTrackingSettings`) works like a DAW's audio-driver selector: pick
//! `Auto` / `Leap` / `MediaPipe` / `Off`, and the app rebuilds its
//! [`ProviderRegistry`] live — no restart. The choice → fallback *policy*
//! lives in [`wc_core::input::selection`] (unit-tested with mock providers);
//! this module owns the *mechanism*: constructing the concrete, feature-gated
//! providers, installing the registry at startup, and the per-frame
//! [`apply_provider_choice`] system that tears down and rebuilds it when the
//! setting changes.
//!
//! Signal flow:
//!
//! ```text
//! Startup: install_hand_tracking_providers
//!   env WAVECONDUCTOR_HAND_PROVIDER?  ──set──▶ launch provider (this run only)
//!   else HandTrackingSettings::provider ──▶ selection::build_registry
//!                                              │ (installer closures below)
//!                                              ▼
//!                              Res<ProviderRegistry> + Res<HandProviderControl>
//!
//! Update: apply_provider_choice
//!   - resolves Auto's optimistic Leap start (device-presence watcher: the
//!     Ultraleap daemon connects without a controller plugged in; no device
//!     within the grace period demotes to MediaPipe / mock)
//!   - resolves Auto's optimistic MediaPipe start (camera verdict watcher)
//!   - on dropdown change: stop()s every old provider synchronously
//!     (worker joined, camera/device released), then build_registry again
//! ```
//!
//! Env semantics (launch default, NOT a pin): `WAVECONDUCTOR_HAND_PROVIDER`
//! set to `auto` / `leap` / `mediapipe` / `off` / `mock` / `synthetic`
//! selects what is installed **at startup** — handy for launch scripts and
//! the capture harness (which sets `mock`/`synthetic` per scenario) — but the
//! "Tracking provider" dropdown stays fully live: the first change rebuilds
//! the registry from the setting, replacing the env-launched provider. The
//! session pin that briefly existed here is gone (operator decision
//! 2026-06-10: "it sets the launch mode but we can always change it in the
//! settings during runtime"). Note the dropdown displays the *persisted*
//! choice, which may differ from the env-launched provider until first
//! touched. `mock` / `synthetic` remain env-only test fixtures (not user
//! choices, so not in the enum). An unrecognized value warns and defers to
//! the setting.

#[cfg(feature = "hand-tracking-gestures")]
use bevy::prelude::*;
#[cfg(feature = "hand-tracking-gestures")]
use wc_core::input::provider::ProviderRegistry;
#[cfg(feature = "hand-tracking-gestures")]
use wc_core::input::selection::{
    auto_leap_device_verdict, auto_mediapipe_camera_failed, build_registry, demote_auto_leap,
    AutoLeapWatch, AutoMediaPipeWatch, BuiltRegistry, LeapWatchVerdict, ProviderInstallers,
    AUTO_LEAP_DEVICE_GRACE,
};
#[cfg(feature = "hand-tracking-gestures")]
use wc_core::settings::{HandProviderChoice, HandTrackingSettings};

/// Book-keeping for the live provider switch.
///
/// Inserted by [`install_hand_tracking_providers`] alongside the registry;
/// read/written by [`apply_provider_choice`] every frame.
#[cfg(feature = "hand-tracking-gestures")]
#[derive(Resource)]
pub struct HandProviderControl {
    /// The setting value the registry was last reconciled against; compared
    /// against the setting each frame to detect a dropdown change. At
    /// startup this is initialized to the *setting's* value even when the
    /// env var launched a different provider — so the env-launched provider
    /// survives until the operator actually moves the dropdown (no spurious
    /// frame-1 rebuild), and the first real change takes effect normally.
    last_applied: HandProviderChoice,
    /// Auto's optimistic-MediaPipe camera watcher (see
    /// [`wc_core::input::selection::AutoMediaPipeWatch`]).
    watch: AutoMediaPipeWatch,
    /// Auto's optimistic-Leap device watcher (see
    /// [`wc_core::input::selection::AutoLeapWatch`]): armed when Auto kept
    /// Leap on its service connection alone, demotes to `MediaPipe` / mock if
    /// no device attaches within the grace period. Resolved *before* `watch`
    /// each frame — a Leap demote may itself arm the `MediaPipe` watch.
    leap_watch: AutoLeapWatch,
}

/// What `WAVECONDUCTOR_HAND_PROVIDER` resolved to (a launch default — see
/// module docs; the dropdown stays live).
#[cfg(feature = "hand-tracking-gestures")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvOverride {
    /// One of the user-facing choices (`auto`/`leap`/`mediapipe`/`off`).
    Choice(HandProviderChoice),
    /// Silent scripted mock (env-only test fixture).
    Mock,
    /// Mock that emits a synthetic sweeping hand (env-only test fixture).
    Synthetic,
}

/// Parse a raw `WAVECONDUCTOR_HAND_PROVIDER` value (case-insensitive).
/// `None` = unset or unrecognized (the caller logs the warning so this stays
/// pure and testable).
#[cfg(feature = "hand-tracking-gestures")]
fn parse_env_override(raw: &str) -> Option<EnvOverride> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(EnvOverride::Choice(HandProviderChoice::Auto)),
        "leap" => Some(EnvOverride::Choice(HandProviderChoice::Leap)),
        "mediapipe" => Some(EnvOverride::Choice(HandProviderChoice::MediaPipe)),
        "off" => Some(EnvOverride::Choice(HandProviderChoice::Off)),
        "mock" => Some(EnvOverride::Mock),
        "synthetic" => Some(EnvOverride::Synthetic),
        _ => None,
    }
}

/// Construct and insert the [`ProviderRegistry`] (plus the
/// [`HandProviderControl`] book-keeping) from `WAVECONDUCTOR_HAND_PROVIDER`
/// when set (launch default), else the persisted
/// `HandTrackingSettings::provider` choice (see module docs).
///
/// Runs as a `Startup` system so `Res<HandTrackingSettings>` is available
/// (settings persistence loads before any user-added system). The persisted
/// `leap_background` value is forwarded to `LeaprsProvider` on construction
/// so the first connection comes up with the correct policy state.
#[cfg(feature = "hand-tracking-gestures")]
pub fn install_hand_tracking_providers(
    mut commands: Commands<'_, '_>,
    settings: Res<'_, HandTrackingSettings>,
    time: Res<'_, Time>,
) {
    use wc_core::input::provider::{ProviderId, ProviderRole};
    use wc_core::input::providers::mock::MockProvider;

    let env_raw = std::env::var("WAVECONDUCTOR_HAND_PROVIDER").ok();
    let env = env_raw.as_deref().and_then(|raw| {
        let parsed = parse_env_override(raw);
        if parsed.is_none() {
            tracing::warn!(
                value = %raw,
                "hand-tracking: unknown WAVECONDUCTOR_HAND_PROVIDER value; \
                 using the Tracking provider setting"
            );
        }
        parsed
    });
    if env.is_some() {
        tracing::info!(
            "hand-tracking: WAVECONDUCTOR_HAND_PROVIDER selected the launch provider; \
             the Tracking provider dropdown stays live and replaces it on first change"
        );
    }

    let (registry, watch, leap_watch) = match env {
        Some(EnvOverride::Mock) => {
            let mut registry = ProviderRegistry::default();
            install_mock(&mut registry);
            (registry, AutoMediaPipeWatch::Idle, AutoLeapWatch::Idle)
        }
        Some(EnvOverride::Synthetic) => {
            // A sweeping synthetic open hand, for exercising hand-driven
            // visuals (bone mesh, attractor, gesture sketches) with no
            // hardware attached. Distinct from `mock` (which is silent).
            let mut registry = ProviderRegistry::default();
            registry.register(
                ProviderId::Mock,
                ProviderRole::Simulator,
                Box::new(MockProvider::synthetic_hand()),
            );
            tracing::info!("hand-tracking: synthetic MockProvider installed (open-hand fixture)");
            (registry, AutoMediaPipeWatch::Idle, AutoLeapWatch::Idle)
        }
        Some(EnvOverride::Choice(choice)) => {
            let built = build_for_choice(choice, &settings);
            let leap_watch = arm_leap_watch(&built, time.elapsed());
            (built.registry, built.watch, leap_watch)
        }
        None => {
            let built = build_for_choice(settings.provider, &settings);
            let leap_watch = arm_leap_watch(&built, time.elapsed());
            (built.registry, built.watch, leap_watch)
        }
    };

    commands.insert_resource(registry);
    commands.insert_resource(HandProviderControl {
        // Deliberately the SETTING's value even when the env var launched
        // something else: the env-launched provider then survives until the
        // dropdown actually moves (see the field docs).
        last_applied: settings.provider,
        watch,
        leap_watch,
    });
}

/// Convert [`BuiltRegistry::leap_verdict_outstanding`] into an armed
/// [`AutoLeapWatch`] with a concrete deadline.
///
/// Lives here rather than in `wc_core::input::selection` because the policy
/// module has no access to Bevy's `Time`; `now` is `Time::elapsed`, the same
/// clock [`apply_provider_choice`] passes to `auto_leap_device_verdict`.
#[cfg(feature = "hand-tracking-gestures")]
fn arm_leap_watch(built: &BuiltRegistry, now: std::time::Duration) -> AutoLeapWatch {
    if built.leap_verdict_outstanding {
        AutoLeapWatch::Pending {
            deadline: now + AUTO_LEAP_DEVICE_GRACE,
        }
    } else {
        AutoLeapWatch::Idle
    }
}

/// No-op stub when hand-tracking-gestures is compiled out.
///
/// Mouse and touch input still work without any hand-tracking provider.
#[cfg(not(feature = "hand-tracking-gestures"))]
pub fn install_hand_tracking_providers() {
    tracing::info!("hand-tracking: feature disabled at compile time; no providers");
}

/// Live provider switch (`Update`): applies dropdown changes to the running
/// registry, and resolves Auto's outstanding verdicts — Leap's async device
/// presence first, then `MediaPipe`'s camera open.
///
/// The watch resolution runs only on frames where the dropdown is *unchanged*.
/// On a change frame we skip straight to the rebuild, which tears down and
/// re-arms both watches anyway: resolving a Leap demote on the same frame the
/// operator moves the dropdown would build — and immediately discard — a real
/// `MediaPipe` provider (opening the camera) for nothing.
///
/// Steady-state cost is three enum compares and no allocation; a watcher arm
/// reads one provider status only while its verdict is pending (the first
/// ~3 s of Auto-with-Leap, or a few frames of Auto-with-MediaPipe), and a
/// switch arm runs only on an actual dropdown change.
///
/// Teardown on switch is explicit and synchronous:
/// [`ProviderRegistry::shutdown_all`] `stop()`s each provider (`MediaPipe`
/// joins its worker thread, releasing the camera; Leap drops its service
/// connection) *before* the replacement registry is built, so a successor
/// provider can immediately re-acquire the hardware.
#[cfg(feature = "hand-tracking-gestures")]
pub fn apply_provider_choice(
    settings: Res<'_, HandTrackingSettings>,
    time: Res<'_, Time>,
    mut control: ResMut<'_, HandProviderControl>,
    mut registry: ResMut<'_, ProviderRegistry>,
) {
    use wc_core::input::provider::ProviderId;
    use wc_core::input::state::{DevicePresence, ServiceConnection};

    // Resolve the watches only when the dropdown is unchanged this frame. Watch
    // resolution is choice-independent book-keeping for a registry that is
    // already installed — an env-launched `WAVECONDUCTOR_HAND_PROVIDER=auto`
    // session needs its verdicts (and fallbacks) exactly as much as a
    // dropdown-driven one — but on a *change* frame the rebuild below re-arms
    // both watches anyway, so resolving a Leap demote here would build (and
    // immediately discard) a real MediaPipe provider on the exact frame a grace
    // deadline and a dropdown move coincide.
    let choice = settings.provider;
    if choice == control.last_applied {
        // The Leap watch resolves before the MediaPipe watch: a Leap demote may
        // itself register MediaPipe optimistically and arm the camera watch,
        // which the block below then polls (same frame: harmless — a freshly
        // started MediaPipe reports `Connecting`, which keeps waiting).
        if matches!(control.leap_watch, AutoLeapWatch::Pending { .. }) {
            match registry.provider(ProviderId::Leap) {
                Some(slot) => {
                    let status = slot.inner.status();
                    match auto_leap_device_verdict(
                        &mut control.leap_watch,
                        status.device,
                        status.service,
                        time.elapsed(),
                    ) {
                        LeapWatchVerdict::Demote => {
                            // Name the actual cause: a hard failure (rule 1)
                            // demotes well before the grace deadline, so a
                            // blanket "no device after Ns" line would misreport
                            // it.
                            let reason = if matches!(status.service, ServiceConnection::Errored) {
                                "Leap service errored"
                            } else if matches!(
                                status.device,
                                DevicePresence::Lost | DevicePresence::Failed
                            ) {
                                "Leap device dropped or failed"
                            } else {
                                "Leap service is running but no device attached \
                                 within the grace period"
                            };
                            tracing::info!(
                                device = ?status.device,
                                service = ?status.service,
                                grace_s = AUTO_LEAP_DEVICE_GRACE.as_secs(),
                                "hand-tracking: auto → {reason}; trying MediaPipe"
                            );
                            // The demote may register MediaPipe optimistically;
                            // adopt its camera watch.
                            control.watch = demote_leap_to_next_candidate(&mut registry, &settings);
                        }
                        LeapWatchVerdict::Keep | LeapWatchVerdict::KeepWaiting => {}
                    }
                }
                // Provider vanished (e.g. a test replaced the registry): nothing
                // left to watch.
                None => control.leap_watch = AutoLeapWatch::Idle,
            }
        }

        if control.watch == AutoMediaPipeWatch::Pending {
            match registry.provider(ProviderId::MediaPipe) {
                Some(slot) => {
                    if auto_mediapipe_camera_failed(&mut control.watch, slot.inner.status().service)
                    {
                        tracing::info!(
                            "hand-tracking: auto → MediaPipe camera failed to open; \
                             falling back to MockProvider"
                        );
                        registry.remove(ProviderId::MediaPipe);
                        install_mock(&mut registry);
                    }
                }
                // Provider vanished (e.g. a test replaced the registry): nothing
                // left to watch.
                None => control.watch = AutoMediaPipeWatch::Idle,
            }
        }

        return;
    }

    tracing::info!(
        from = ?control.last_applied,
        to = ?choice,
        "hand-tracking: switching provider"
    );
    registry.shutdown_all();
    let built = build_for_choice(choice, &settings);
    // Re-arm both watches from the fresh build (fresh grace deadline) —
    // re-picking Auto in the dropdown is the deliberate way to re-probe a
    // Leap that was demoted earlier (a later plug-in never switches back
    // automatically; see `AutoLeapWatch`).
    control.watch = built.watch;
    control.leap_watch = arm_leap_watch(&built, time.elapsed());
    *registry = built.registry;
    control.last_applied = choice;
}

/// Publish the coarse [`HandTrackingActivation`](wc_core::input::activation::HandTrackingActivation) cue the settings panel reads.
///
/// Composes the watch bookkeeping this module owns with the registry-derived
/// state from [`wc_core::input::activation::activation_from_registry`]: while a
/// watch is still pending (Auto probing Leap's device grace, or `MediaPipe`
/// starting), tracking is absent-but-expected, so report `Settling` regardless
/// of what the half-installed registry says. Otherwise defer to the registry —
/// which catches the silent mock fallback (`FellBackToMock`) and a failed
/// provider (`Failed`) that the raw service axis reads as healthy.
///
/// Runs chained after [`apply_provider_choice`] so it observes the post-rebuild
/// state. Writes only on change, so the resource's change-detection stays quiet
/// in the steady state (the panel reads it every frame it is open).
#[cfg(feature = "hand-tracking-gestures")]
pub fn publish_hand_activation(
    control: Res<'_, HandProviderControl>,
    registry: Res<'_, ProviderRegistry>,
    mut activation: ResMut<'_, wc_core::input::activation::HandTrackingActivation>,
) {
    use wc_core::input::activation::{activation_from_registry, HandTrackingActivation};

    let next = if matches!(control.leap_watch, AutoLeapWatch::Pending { .. })
        || control.watch == AutoMediaPipeWatch::Pending
    {
        HandTrackingActivation::Settling
    } else {
        activation_from_registry(&registry)
    };
    if *activation != next {
        *activation = next;
    }
}

/// Auto's per-frame Leap demote: fall through to `MediaPipe` (else the mock)
/// with the real installer closures, mirroring [`build_for_choice`].
///
/// Thin on purpose: the demote *policy* (remove Leap, try `MediaPipe`,
/// else mock, and which watch results) lives in
/// [`wc_core::input::selection::demote_auto_leap`], unit-tested there with
/// scripted installers; this binds it to the concrete constructors exactly
/// like [`build_for_choice`] binds [`build_registry`].
#[cfg(feature = "hand-tracking-gestures")]
fn demote_leap_to_next_candidate(
    registry: &mut ProviderRegistry,
    settings: &HandTrackingSettings,
) -> AutoMediaPipeWatch {
    let mut leap = |registry: &mut ProviderRegistry| try_leap(registry, settings.leap_background);
    let mut mediapipe = |registry: &mut ProviderRegistry| register_mediapipe(registry, settings);
    let mut mock = |registry: &mut ProviderRegistry| install_mock(registry);
    demote_auto_leap(
        registry,
        &mut ProviderInstallers {
            leap: &mut leap,
            mediapipe: &mut mediapipe,
            mock: &mut mock,
        },
    )
}

/// Bind the concrete, feature-gated provider constructors to the shared
/// selection policy (see [`wc_core::input::selection::build_registry`]).
#[cfg(feature = "hand-tracking-gestures")]
fn build_for_choice(choice: HandProviderChoice, settings: &HandTrackingSettings) -> BuiltRegistry {
    let mut leap = |registry: &mut ProviderRegistry| try_leap(registry, settings.leap_background);
    let mut mediapipe = |registry: &mut ProviderRegistry| register_mediapipe(registry, settings);
    let mut mock = |registry: &mut ProviderRegistry| install_mock(registry);
    build_registry(
        choice,
        &mut ProviderInstallers {
            leap: &mut leap,
            mediapipe: &mut mediapipe,
            mock: &mut mock,
        },
    )
}

/// Register + start the Leap provider; `true` = service connection opened.
///
/// Note: [`ProviderRegistry::register`] auto-starts the provider. We check
/// `status().service` after registration to detect startup failure
/// (`LeaprsProvider` reports `Errored` on create/open failure).
///
/// Per the installer contract on
/// [`wc_core::input::selection::ProviderInstallers`], a failed provider is
/// left registered: the explicit `Leap` choice keeps the corpse visible in
/// the dev panel, and the selection policy's `Auto` arm does the
/// failed-candidate cleanup itself before falling through.
#[cfg(feature = "hand-tracking-gestures")]
fn try_leap(registry: &mut ProviderRegistry, leap_background: bool) -> bool {
    use wc_core::input::provider::{ProviderId, ProviderRole};
    use wc_core::input::providers::leap_native::LeaprsProvider;
    use wc_core::input::state::ServiceConnection;

    let mut leap = LeaprsProvider::default();
    leap.request_background = leap_background;
    registry.register(ProviderId::Leap, ProviderRole::Primary, Box::new(leap));
    let started = registry.provider(ProviderId::Leap).is_some_and(|r| {
        !matches!(
            r.inner.status().service,
            ServiceConnection::Errored | ServiceConnection::NotStarted
        )
    });
    if started {
        tracing::info!("hand-tracking: LeaprsProvider started");
    } else {
        tracing::warn!("hand-tracking: LeaprsProvider failed to start");
    }
    started
}

/// Install the silent mock simulator (cannot fail).
#[cfg(feature = "hand-tracking-gestures")]
fn install_mock(registry: &mut ProviderRegistry) {
    use wc_core::input::provider::{ProviderId, ProviderRole};
    use wc_core::input::providers::mock::MockProvider;

    registry.register(
        ProviderId::Mock,
        ProviderRole::Simulator,
        Box::new(MockProvider::default()),
    );
    tracing::info!("hand-tracking: MockProvider installed");
}

/// Register the `MediaPipe` webcam provider as the primary source.
///
/// Returns `Some(started)` when the `hand-tracking-mediapipe` feature is
/// compiled in, or `None` when it is absent. `started == true` is
/// *optimistic*: the camera opens asynchronously on the worker thread, so
/// only a synchronous failure (e.g. missing models) reports `false` here —
/// the camera verdict arrives later via `status()` (see
/// [`wc_core::input::selection::AutoMediaPipeWatch`]).
#[cfg(all(
    feature = "hand-tracking-gestures",
    feature = "hand-tracking-mediapipe"
))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "Option is the shared signature; the None case is the feature-absent variant below"
)]
fn register_mediapipe(
    registry: &mut ProviderRegistry,
    settings: &HandTrackingSettings,
) -> Option<bool> {
    use wc_core::input::provider::{ProviderId, ProviderRole};
    use wc_core::input::providers::mediapipe::{MediaPipeConfig, MediaPipeProvider};
    use wc_core::input::state::ServiceConnection;

    // `WAVECONDUCTOR_HAND_SMOOTHING=off|0|false|no` (case-insensitive) exposes
    // the raw inference poses for A/B tuning; smoothing is on by default.
    let smoothing = std::env::var("WAVECONDUCTOR_HAND_SMOOTHING")
        .ok()
        .is_none_or(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "off" | "0" | "false" | "no"
            )
        });

    // The numeric feel tunables (grab deadzone, depth calibration k, smoothing
    // min-cutoff/beta) are owned by HandTrackingSettings — the Dev panel edits
    // them live and they persist. Seed the provider from the (possibly
    // persisted) settings so a saved value applies at startup;
    // `apply_mediapipe_tuning_settings` keeps it in sync on every change.
    let config = MediaPipeConfig {
        smoothing,
        grab_rest_deadzone: settings.grab_rest_deadzone,
        depth_calibration_k: settings.depth_calibration_k,
        smoothing_min_cutoff: settings.smoothing_min_cutoff,
        smoothing_beta: settings.smoothing_beta,
        backend: settings.backend,
        ..MediaPipeConfig::default()
    };

    registry.register(
        ProviderId::MediaPipe,
        ProviderRole::Primary,
        Box::new(MediaPipeProvider::new(config)),
    );
    Some(registry.provider(ProviderId::MediaPipe).is_some_and(|r| {
        !matches!(
            r.inner.status().service,
            ServiceConnection::Errored | ServiceConnection::NotStarted
        )
    }))
}

/// Feature-absent variant: signals the selection policy that `MediaPipe`
/// is not compiled in.
#[cfg(all(
    feature = "hand-tracking-gestures",
    not(feature = "hand-tracking-mediapipe")
))]
fn register_mediapipe(
    _registry: &mut ProviderRegistry,
    _settings: &HandTrackingSettings,
) -> Option<bool> {
    None
}

#[cfg(all(test, feature = "hand-tracking-gestures"))]
mod tests {
    use super::*;

    #[test]
    fn env_override_parses_all_documented_values_case_insensitively() {
        assert_eq!(
            parse_env_override("Auto"),
            Some(EnvOverride::Choice(HandProviderChoice::Auto))
        );
        assert_eq!(
            parse_env_override("LEAP"),
            Some(EnvOverride::Choice(HandProviderChoice::Leap))
        );
        assert_eq!(
            parse_env_override("mediapipe"),
            Some(EnvOverride::Choice(HandProviderChoice::MediaPipe))
        );
        assert_eq!(
            parse_env_override(" off "),
            Some(EnvOverride::Choice(HandProviderChoice::Off))
        );
        assert_eq!(parse_env_override("mock"), Some(EnvOverride::Mock));
        assert_eq!(
            parse_env_override("synthetic"),
            Some(EnvOverride::Synthetic)
        );
    }

    #[test]
    fn env_override_rejects_unknown_values() {
        assert_eq!(parse_env_override("webcam"), None);
        assert_eq!(parse_env_override(""), None);
    }

    // ── apply_provider_choice (headless Bevy app) ───────────────────────

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use wc_core::input::provider::{HandTrackingProvider, ProviderId, ProviderRole};
    use wc_core::input::state::{
        DevicePresence, HandTrackingError, HandTrackingFrame, ProviderDiagnostics, ProviderStatus,
        ServiceConnection,
    };

    /// Test provider with scripted service/device state and a `stop()` counter.
    struct ServiceStub {
        service: ServiceConnection,
        device: DevicePresence,
        stops: Arc<AtomicUsize>,
    }

    impl HandTrackingProvider for ServiceStub {
        fn start(&mut self) -> Result<(), HandTrackingError> {
            Ok(())
        }

        fn stop(&mut self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }

        fn poll(&mut self, _now: Duration, _out: &mut Messages<HandTrackingFrame>) {}

        fn status(&self) -> ProviderStatus {
            ProviderStatus {
                service: self.service,
                device: self.device,
                ..ProviderStatus::default()
            }
        }

        fn diagnostics(&self) -> ProviderDiagnostics {
            ProviderDiagnostics::default()
        }
    }

    /// Minimal headless app running only [`apply_provider_choice`], with a
    /// scripted provider under `id` and the given control state.
    ///
    /// `Time` is the default resource (elapsed = 0, never ticked), so an
    /// `AutoLeapWatch::Pending { deadline: Duration::ZERO }` is already past
    /// its deadline and any nonzero deadline is comfortably in the future.
    fn test_app(
        id: ProviderId,
        service: ServiceConnection,
        device: DevicePresence,
        control: HandProviderControl,
    ) -> (App, Arc<AtomicUsize>) {
        let stops = Arc::new(AtomicUsize::new(0));
        let mut registry = ProviderRegistry::default();
        registry.register(
            id,
            ProviderRole::Primary,
            Box::new(ServiceStub {
                service,
                device,
                stops: Arc::clone(&stops),
            }),
        );
        let mut app = App::new();
        app.init_resource::<Time>();
        app.insert_resource(HandTrackingSettings::default());
        app.insert_resource(registry);
        app.insert_resource(control);
        app.add_systems(Update, apply_provider_choice);
        (app, stops)
    }

    /// A scripted provider with the given service/device and a throwaway stop
    /// counter, boxed for registration.
    fn stub(service: ServiceConnection, device: DevicePresence) -> Box<dyn HandTrackingProvider> {
        Box::new(ServiceStub {
            service,
            device,
            stops: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// `publish_hand_activation` composes the watch state with the registry: a
    /// pending watch settles regardless of registry contents, a mock primary
    /// reads as fell-back, a connected non-mock is active, and an empty registry
    /// is inactive.
    #[test]
    fn publish_activation_reflects_watch_and_registry() {
        use wc_core::input::activation::HandTrackingActivation;

        fn run(registry: ProviderRegistry, control: HandProviderControl) -> HandTrackingActivation {
            let mut app = App::new();
            app.insert_resource(registry);
            app.insert_resource(control);
            app.init_resource::<HandTrackingActivation>();
            app.add_systems(Update, publish_hand_activation);
            app.update();
            *app.world().resource::<HandTrackingActivation>()
        }

        let idle = || HandProviderControl {
            last_applied: HandProviderChoice::Auto,
            watch: AutoMediaPipeWatch::Idle,
            leap_watch: AutoLeapWatch::Idle,
        };

        // Pending Leap watch → Settling, even though the Leap stub reports a
        // healthy service (the whole point: a watch outranks the registry).
        let mut reg = ProviderRegistry::default();
        reg.register(
            ProviderId::Leap,
            ProviderRole::Primary,
            stub(ServiceConnection::Connected, DevicePresence::NoDevice),
        );
        let pending = HandProviderControl {
            last_applied: HandProviderChoice::Auto,
            watch: AutoMediaPipeWatch::Idle,
            leap_watch: AutoLeapWatch::Pending {
                deadline: Duration::from_secs(3),
            },
        };
        assert_eq!(run(reg, pending), HandTrackingActivation::Settling);

        // Mock primary, watches idle → FellBackToMock (mock reports Connected,
        // so identity, not service, must catch it).
        let mut reg = ProviderRegistry::default();
        reg.register(
            ProviderId::Mock,
            ProviderRole::Primary,
            stub(ServiceConnection::Connected, DevicePresence::Attached),
        );
        assert_eq!(run(reg, idle()), HandTrackingActivation::FellBackToMock);

        // Connected non-mock provider, watches idle → Active.
        let mut reg = ProviderRegistry::default();
        reg.register(
            ProviderId::Leap,
            ProviderRole::Primary,
            stub(ServiceConnection::Connected, DevicePresence::Attached),
        );
        assert_eq!(run(reg, idle()), HandTrackingActivation::Active);

        // Empty registry (Off), watches idle → Inactive.
        assert_eq!(
            run(ProviderRegistry::default(), idle()),
            HandTrackingActivation::Inactive
        );
    }

    /// Watch resolution runs before the change-check early-out: an
    /// env-launched `auto` session (camera open failed) must resolve its
    /// `MediaPipe` watch and fall back to the mock even though the setting
    /// hasn't changed — and a later dropdown change must still rebuild
    /// (the env launch is a default, not a pin).
    #[test]
    fn watch_resolves_and_falls_back_then_dropdown_still_works() {
        let (mut app, stops) = test_app(
            ProviderId::MediaPipe,
            ServiceConnection::Errored,
            DevicePresence::NoDevice,
            HandProviderControl {
                last_applied: HandProviderChoice::Auto,
                watch: AutoMediaPipeWatch::Pending,
                leap_watch: AutoLeapWatch::Idle,
            },
        );
        app.update();

        let registry = app.world().resource::<ProviderRegistry>();
        assert!(
            registry.provider(ProviderId::MediaPipe).is_none(),
            "camera-failed MediaPipe must be demoted"
        );
        assert!(
            registry.provider(ProviderId::Mock).is_some(),
            "mock fallback must be installed"
        );
        assert_eq!(
            stops.load(Ordering::SeqCst),
            1,
            "demotion stops the provider"
        );
        assert_eq!(
            app.world().resource::<HandProviderControl>().watch,
            AutoMediaPipeWatch::Idle
        );

        // No pin: flipping the dropdown rebuilds the registry (Off empties it).
        app.world_mut()
            .resource_mut::<HandTrackingSettings>()
            .provider = HandProviderChoice::Off;
        app.update();
        let registry = app.world().resource::<ProviderRegistry>();
        assert_eq!(
            registry.iter().count(),
            0,
            "dropdown change must rebuild even after an env launch"
        );
    }

    /// An unrelated settings-field change (e.g. a tuning slider) must NOT
    /// rebuild the registry — the switch keys on the `provider` enum compare,
    /// not on settings change detection.
    #[test]
    fn unrelated_settings_change_does_not_rebuild_registry() {
        let (mut app, stops) = test_app(
            ProviderId::Leap,
            ServiceConnection::Connected,
            DevicePresence::Attached,
            HandProviderControl {
                last_applied: HandProviderChoice::Auto,
                watch: AutoMediaPipeWatch::Idle,
                leap_watch: AutoLeapWatch::Idle,
            },
        );
        // `provider` already matches last_applied (both Auto).
        app.update();
        app.world_mut()
            .resource_mut::<HandTrackingSettings>()
            .grab_rest_deadzone = 0.3;
        app.update();
        app.update();

        assert_eq!(
            stops.load(Ordering::SeqCst),
            0,
            "tuning-only change must not tear providers down"
        );
        assert!(app
            .world()
            .resource::<ProviderRegistry>()
            .provider(ProviderId::Leap)
            .is_some());

        // Sanity check the inverse: an actual provider change does rebuild.
        // Off is used because it constructs no real hardware providers.
        app.world_mut()
            .resource_mut::<HandTrackingSettings>()
            .provider = HandProviderChoice::Off;
        app.update();
        assert_eq!(
            stops.load(Ordering::SeqCst),
            1,
            "switch stops the old provider"
        );
        assert_eq!(app.world().resource::<ProviderRegistry>().iter().count(), 0);
    }

    // ── Auto's Leap device watch (bookkeeping; no real providers) ───────
    //
    // These cover only the verdicts that do NOT demote: a demote calls the
    // real installer closures (`register_mediapipe` constructs a real
    // `MediaPipeProvider`, which loads models and opens the webcam), which a
    // default-run test must never do. The demote *composition* is fully
    // unit-tested in `wc_core::input::selection` with scripted installers;
    // the real-installer path is covered by the `#[ignore]`d test below.

    /// Device already attached: the watch resolves to Keep — Leap stays, the
    /// watch idles, nothing is stopped. Deadline 0 (already expired against
    /// the unticked test clock) also pins rule precedence at the binary
    /// level: Attached outranks deadline expiry.
    #[test]
    fn leap_watch_keeps_attached_device_and_idles() {
        let (mut app, stops) = test_app(
            ProviderId::Leap,
            ServiceConnection::Connected,
            DevicePresence::Attached,
            HandProviderControl {
                last_applied: HandProviderChoice::Auto,
                watch: AutoMediaPipeWatch::Idle,
                leap_watch: AutoLeapWatch::Pending {
                    deadline: Duration::ZERO,
                },
            },
        );
        app.update();

        assert!(app
            .world()
            .resource::<ProviderRegistry>()
            .provider(ProviderId::Leap)
            .is_some());
        assert_eq!(stops.load(Ordering::SeqCst), 0);
        assert_eq!(
            app.world().resource::<HandProviderControl>().leap_watch,
            AutoLeapWatch::Idle,
            "verdict arrived; watch must idle permanently"
        );
    }

    /// No device yet but the deadline is still ahead: keep waiting, keep Leap.
    #[test]
    fn leap_watch_waits_out_the_grace_period() {
        let deadline = Duration::from_hours(1); // far future vs. unticked Time
        let (mut app, stops) = test_app(
            ProviderId::Leap,
            ServiceConnection::Connected,
            DevicePresence::NoDevice,
            HandProviderControl {
                last_applied: HandProviderChoice::Auto,
                watch: AutoMediaPipeWatch::Idle,
                leap_watch: AutoLeapWatch::Pending { deadline },
            },
        );
        app.update();
        app.update();

        assert!(app
            .world()
            .resource::<ProviderRegistry>()
            .provider(ProviderId::Leap)
            .is_some());
        assert_eq!(stops.load(Ordering::SeqCst), 0);
        assert_eq!(
            app.world().resource::<HandProviderControl>().leap_watch,
            AutoLeapWatch::Pending { deadline },
            "verdict outstanding; watch must stay pending"
        );
    }

    /// L2: when the Leap device-grace deadline expires on the exact frame the
    /// operator moves the dropdown, the demote must be skipped — it would build
    /// (and immediately discard) a real `MediaPipe` provider, opening the
    /// camera. The rebuild handles the switch instead. Choosing `Off` makes the
    /// rebuild path observable (empty registry) and constructs no hardware, so
    /// the test stays camera-free even though the deadline has expired.
    #[test]
    fn dropdown_change_skips_expired_leap_demote() {
        let (mut app, stops) = test_app(
            ProviderId::Leap,
            ServiceConnection::Connected,
            DevicePresence::NoDevice,
            HandProviderControl {
                last_applied: HandProviderChoice::Auto,
                watch: AutoMediaPipeWatch::Idle,
                leap_watch: AutoLeapWatch::Pending {
                    deadline: Duration::ZERO, // already expired vs. unticked Time
                },
            },
        );
        // Operator flips the dropdown to Off on the same frame the deadline
        // lapses.
        app.world_mut()
            .resource_mut::<HandTrackingSettings>()
            .provider = HandProviderChoice::Off;
        app.update();

        // Took the rebuild path (Off → empty registry), NOT the demote path
        // (which would have left a real MediaPipe provider or a mock behind).
        assert_eq!(
            app.world().resource::<ProviderRegistry>().iter().count(),
            0,
            "dropdown change rebuilds to Off; the leap demote must not run"
        );
        assert_eq!(
            stops.load(Ordering::SeqCst),
            1,
            "the switch stops the old Leap exactly once"
        );
        assert_eq!(
            app.world().resource::<HandProviderControl>().leap_watch,
            AutoLeapWatch::Idle,
            "rebuild to Off arms no leap watch"
        );
    }

    /// A pending watch whose Leap provider vanished (e.g. a test replaced
    /// the registry) idles instead of dangling forever.
    #[test]
    fn leap_watch_idles_when_provider_vanished() {
        let (mut app, _stops) = test_app(
            ProviderId::Mock, // registry holds no Leap
            ServiceConnection::Connected,
            DevicePresence::NoDevice,
            HandProviderControl {
                last_applied: HandProviderChoice::Auto,
                watch: AutoMediaPipeWatch::Idle,
                leap_watch: AutoLeapWatch::Pending {
                    deadline: Duration::from_hours(1),
                },
            },
        );
        app.update();
        assert_eq!(
            app.world().resource::<HandProviderControl>().leap_watch,
            AutoLeapWatch::Idle
        );
    }

    /// Full real-installer demote path: service-only Leap past its deadline
    /// is removed and replaced by the next Auto candidate.
    ///
    /// `#[ignore]`d because the demote invokes the REAL
    /// `register_mediapipe`, which constructs a `MediaPipeProvider` — on a
    /// machine with the models present that spawns a worker and opens the
    /// webcam, which default-run tests must never do. Run manually with
    /// `cargo nextest run -p waveconductor --all-features --run-ignored all
    /// leap_demote` when validating the wiring end to end.
    #[test]
    #[ignore = "constructs a real MediaPipeProvider (may open the webcam); run manually"]
    fn leap_demote_replaces_service_only_leap_with_next_candidate() {
        let (mut app, stops) = test_app(
            ProviderId::Leap,
            ServiceConnection::Connected,
            DevicePresence::NoDevice,
            HandProviderControl {
                last_applied: HandProviderChoice::Auto,
                watch: AutoMediaPipeWatch::Idle,
                leap_watch: AutoLeapWatch::Pending {
                    deadline: Duration::ZERO, // already expired
                },
            },
        );
        app.update();

        let registry = app.world().resource::<ProviderRegistry>();
        assert!(
            registry.provider(ProviderId::Leap).is_none(),
            "service-only Leap must be demoted after the grace period"
        );
        assert!(
            registry.provider(ProviderId::MediaPipe).is_some()
                || registry.provider(ProviderId::Mock).is_some(),
            "a successor candidate must be installed"
        );
        assert_eq!(stops.load(Ordering::SeqCst), 1, "demotion stops Leap");
        assert_eq!(
            app.world().resource::<HandProviderControl>().leap_watch,
            AutoLeapWatch::Idle
        );
    }
}
