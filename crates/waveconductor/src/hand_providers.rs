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
    auto_mediapipe_camera_failed, build_registry, AutoMediaPipeWatch, BuiltRegistry,
    ProviderInstallers,
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

    let (registry, watch) = match env {
        Some(EnvOverride::Mock) => {
            let mut registry = ProviderRegistry::default();
            install_mock(&mut registry);
            (registry, AutoMediaPipeWatch::Idle)
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
            (registry, AutoMediaPipeWatch::Idle)
        }
        Some(EnvOverride::Choice(choice)) => {
            let built = build_for_choice(choice, &settings);
            (built.registry, built.watch)
        }
        None => {
            let built = build_for_choice(settings.provider, &settings);
            (built.registry, built.watch)
        }
    };

    commands.insert_resource(registry);
    commands.insert_resource(HandProviderControl {
        // Deliberately the SETTING's value even when the env var launched
        // something else: the env-launched provider then survives until the
        // dropdown actually moves (see the field docs).
        last_applied: settings.provider,
        watch,
    });
}

/// No-op stub when hand-tracking-gestures is compiled out.
///
/// Mouse and touch input still work without any hand-tracking provider.
#[cfg(not(feature = "hand-tracking-gestures"))]
pub fn install_hand_tracking_providers() {
    tracing::info!("hand-tracking: feature disabled at compile time; no providers");
}

/// Live provider switch (`Update`): applies dropdown changes to the running
/// registry, and resolves Auto's outstanding `MediaPipe` camera verdict.
///
/// Steady-state cost is two enum compares and no allocation; the watcher arm
/// reads one provider status only while a verdict is pending (a few frames
/// after entering Auto-with-MediaPipe), and a switch arm runs only on an
/// actual dropdown change.
///
/// Teardown on switch is explicit and synchronous:
/// [`ProviderRegistry::shutdown_all`] `stop()`s each provider (`MediaPipe`
/// joins its worker thread, releasing the camera; Leap drops its service
/// connection) *before* the replacement registry is built, so a successor
/// provider can immediately re-acquire the hardware.
#[cfg(feature = "hand-tracking-gestures")]
pub fn apply_provider_choice(
    settings: Res<'_, HandTrackingSettings>,
    mut control: ResMut<'_, HandProviderControl>,
    mut registry: ResMut<'_, ProviderRegistry>,
) {
    use wc_core::input::provider::ProviderId;

    // Resolve Auto's optimistic MediaPipe start FIRST — before the
    // change-check early-out. Watch resolution is choice-independent
    // book-keeping for a registry that is already installed: an env-launched
    // `WAVECONDUCTOR_HAND_PROVIDER=auto` session needs its camera verdict
    // (and mock fallback) exactly as much as a dropdown-driven one.
    if control.watch == AutoMediaPipeWatch::Pending {
        match registry.provider(ProviderId::MediaPipe) {
            Some(slot) => {
                if auto_mediapipe_camera_failed(&mut control.watch, slot.inner.status().service) {
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

    let choice = settings.provider;
    if choice == control.last_applied {
        return;
    }

    tracing::info!(
        from = ?control.last_applied,
        to = ?choice,
        "hand-tracking: switching provider"
    );
    registry.shutdown_all();
    let built = build_for_choice(choice, &settings);
    *registry = built.registry;
    control.watch = built.watch;
    control.last_applied = choice;
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
        HandTrackingError, HandTrackingFrame, ProviderDiagnostics, ProviderStatus,
        ServiceConnection,
    };

    /// Test provider with a scripted service state and a `stop()` counter.
    struct ServiceStub {
        service: ServiceConnection,
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
                ..ProviderStatus::default()
            }
        }

        fn diagnostics(&self) -> ProviderDiagnostics {
            ProviderDiagnostics::default()
        }
    }

    /// Minimal headless app running only [`apply_provider_choice`], with a
    /// scripted provider under `id` and the given control state.
    fn test_app(
        id: ProviderId,
        service: ServiceConnection,
        control: HandProviderControl,
    ) -> (App, Arc<AtomicUsize>) {
        let stops = Arc::new(AtomicUsize::new(0));
        let mut registry = ProviderRegistry::default();
        registry.register(
            id,
            ProviderRole::Primary,
            Box::new(ServiceStub {
                service,
                stops: Arc::clone(&stops),
            }),
        );
        let mut app = App::new();
        app.insert_resource(HandTrackingSettings::default());
        app.insert_resource(registry);
        app.insert_resource(control);
        app.add_systems(Update, apply_provider_choice);
        (app, stops)
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
            HandProviderControl {
                last_applied: HandProviderChoice::Auto,
                watch: AutoMediaPipeWatch::Pending,
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
            HandProviderControl {
                last_applied: HandProviderChoice::Auto,
                watch: AutoMediaPipeWatch::Idle,
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
}
