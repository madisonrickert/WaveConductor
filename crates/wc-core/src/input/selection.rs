//! Maps the user-facing "Tracking provider" setting onto a built
//! [`ProviderRegistry`] — the policy half of the DAW-style driver selector.
//!
//! The concrete provider constructors live in the `waveconductor` binary
//! (they are feature-gated and need `MediaPipeConfig` / `LeaprsProvider`
//! wiring), so this module receives them as injected installer closures
//! ([`ProviderInstallers`]) and owns only the choice → fallback policy.
//! That split keeps the policy unit-testable with mock providers, no
//! hardware or camera features required, and keeps `main.rs` thin.
//!
//! Data flow: at startup (and again on every live dropdown change) the
//! binary builds a fresh registry through [`build_registry`]; the returned
//! [`AutoMediaPipeWatch`] tells the binary's per-frame switch system whether
//! it must keep watching an optimistically-registered `MediaPipe` provider
//! for camera-open failure (see [`auto_mediapipe_camera_failed`]).

use super::provider::{ProviderId, ProviderRegistry};
use super::state::ServiceConnection;
use crate::settings::hand_tracking::HandProviderChoice;

/// Injected constructors for the concrete providers, supplied by the binary.
///
/// Each closure registers its provider into the given registry (via
/// [`ProviderRegistry::register`], which auto-starts) and reports how that
/// went:
///
/// - `leap` — returns whether the Leap provider started.
/// - `mediapipe` — returns `Some(started)` when the `MediaPipe` backend is
///   compiled in, `None` when the feature is absent. Note `started == true`
///   is *optimistic*: the webcam is opened asynchronously on the worker
///   thread, so a camera-open failure surfaces only later through
///   `status()` (see [`AutoMediaPipeWatch`]).
/// - `mock` — installs the silent mock simulator; cannot fail.
///
/// **Failure-visibility contract:** an installer must *leave* a provider
/// that failed to start registered — never remove it itself. The explicit
/// `Leap` / `MediaPipe` choices keep the dead provider visible (its
/// `Errored` status is the honest signal in the dev panel), and
/// [`build_registry`]'s `Auto` arm performs the cleanup *itself* before
/// falling through to the next candidate. That cleanup keys on
/// [`ProviderId::Leap`] / [`ProviderId::MediaPipe`], so installers must
/// register under those ids.
pub struct ProviderInstallers<'a> {
    /// Try to install + start the Leap provider; `true` = started. Must
    /// register under [`ProviderId::Leap`] and must leave a failed provider
    /// registered: the `Auto` arm of [`build_registry`] depends on doing
    /// the failure cleanup itself (see the struct docs), and the explicit
    /// `Leap` choice keeps the corpse visible.
    pub leap: &'a mut dyn FnMut(&mut ProviderRegistry) -> bool,
    /// Try to install + start the `MediaPipe` provider; `None` = feature
    /// not compiled in, `Some(started)` otherwise (optimistic — see above).
    /// Same leave-it-registered contract as `leap`, under
    /// [`ProviderId::MediaPipe`].
    pub mediapipe: &'a mut dyn FnMut(&mut ProviderRegistry) -> Option<bool>,
    /// Install the silent mock simulator (always succeeds).
    pub mock: &'a mut dyn FnMut(&mut ProviderRegistry),
}

/// Result of [`build_registry`]: the populated registry plus whether the
/// caller must watch an optimistically-registered `MediaPipe` provider for
/// camera-open failure.
pub struct BuiltRegistry {
    /// The freshly-built registry for the requested choice.
    pub registry: ProviderRegistry,
    /// [`AutoMediaPipeWatch::Pending`] when `Auto` registered `MediaPipe`
    /// and its camera verdict is still outstanding; otherwise `Idle`.
    pub watch: AutoMediaPipeWatch,
}

/// Build a registry for one [`HandProviderChoice`].
///
/// Policy per choice:
///
/// - **Off** — empty registry; mouse and touch input still work.
/// - **Leap** / **`MediaPipe`** — that backend only, no silent fallback,
///   and one shared failure philosophy: a provider that registered but
///   failed to start *stays registered* so the dev panel shows its
///   `Errored` status. The operator asked for this backend; surface its
///   failure, don't mask it behind a healthy-looking mock.
/// - **Auto** — probe order Leap → `MediaPipe` → silent mock. Unlike the
///   explicit choices, Auto *removes* each failed candidate before falling
///   through: a dead `Primary` corpse would shadow the mock in
///   [`ProviderRegistry::primary_status`], and "auto" means "give me
///   whatever works", not "show me what broke". `MediaPipe`'s start is
///   optimistic (camera opens async on the worker thread), so when Auto
///   lands on it the returned watch is [`AutoMediaPipeWatch::Pending`] and
///   the caller must poll [`auto_mediapipe_camera_failed`] to demote to
///   the mock if the webcam never opens.
pub fn build_registry(
    choice: HandProviderChoice,
    installers: &mut ProviderInstallers<'_>,
) -> BuiltRegistry {
    let mut registry = ProviderRegistry::default();
    let mut watch = AutoMediaPipeWatch::Idle;
    match choice {
        HandProviderChoice::Off => {
            tracing::info!("hand-tracking: provider set to Off; mouse and touch input still work");
        }
        HandProviderChoice::Leap => {
            if !(installers.leap)(&mut registry) {
                tracing::error!(
                    "hand-tracking: 'Leap' selected but the provider failed to start; \
                     its status stays visible in the dev panel, mouse and touch \
                     input still work"
                );
            }
        }
        HandProviderChoice::MediaPipe => match (installers.mediapipe)(&mut registry) {
            Some(true) => {
                tracing::info!("hand-tracking: MediaPipeProvider started (webcam)");
            }
            Some(false) => {
                tracing::warn!(
                    "hand-tracking: MediaPipeProvider failed to start; its status \
                     stays visible in the dev panel, mouse and touch input still work"
                );
            }
            None => {
                tracing::warn!(
                    "hand-tracking: 'MediaPipe' selected but the hand-tracking-mediapipe \
                     feature is not compiled in; no provider registered"
                );
            }
        },
        HandProviderChoice::Auto => {
            if !(installers.leap)(&mut registry) {
                // Failed-candidate cleanup is Auto's job (see the installer
                // contract on `ProviderInstallers`): the installer left the
                // dead Leap registered, and its Primary corpse must not
                // shadow the next candidate / mock in `primary_status()`.
                registry.remove(ProviderId::Leap);
                match (installers.mediapipe)(&mut registry) {
                    Some(true) => {
                        tracing::info!(
                            "hand-tracking: auto → MediaPipe registered (webcam verdict pending)"
                        );
                        watch = AutoMediaPipeWatch::Pending;
                    }
                    Some(false) => {
                        // Synchronous start failure (e.g. models missing):
                        // same Auto cleanup obligation as the Leap candidate
                        // above.
                        registry.remove(ProviderId::MediaPipe);
                        tracing::info!("hand-tracking: auto → falling back to MockProvider");
                        (installers.mock)(&mut registry);
                    }
                    None => {
                        tracing::info!("hand-tracking: auto → falling back to MockProvider");
                        (installers.mock)(&mut registry);
                    }
                }
            }
        }
    }
    BuiltRegistry { registry, watch }
}

/// Whether `Auto`'s optimistically-registered `MediaPipe` provider still owes
/// us a camera verdict.
///
/// `MediaPipe`'s `start()` returns optimistically (`Connecting`) because the
/// webcam is opened on the worker thread, ~tens of milliseconds later. The
/// worker's *first* status message after a successful open is `Connected`
/// (see `providers::mediapipe::worker`); on open failure it reports `Errored`
/// and exits, never to recover. So while a watch is `Pending`,
/// `Errored`-before-`Connected` unambiguously means "the camera never
/// opened" — that is the one case where `Auto` demotes to the silent mock.
/// Once `Connected` has been seen the watch goes `Idle` permanently:
/// *transient* mid-session errors (e.g. the camera unplugged) stay the
/// provider's business and keep its honest `Errored` LED.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoMediaPipeWatch {
    /// Nothing to watch (not in Auto, or the verdict already arrived).
    #[default]
    Idle,
    /// Auto registered `MediaPipe`; camera-open verdict outstanding.
    Pending,
}

/// Advance an [`AutoMediaPipeWatch`] with the `MediaPipe` provider's current
/// service state. Returns `true` exactly when the camera failed to open and
/// the caller should replace the provider with the silent mock.
///
/// Cheap (two enum compares); intended to run every frame from the binary's
/// switch system while the watch is `Pending` — which resolves within a few
/// frames of startup.
pub fn auto_mediapipe_camera_failed(
    watch: &mut AutoMediaPipeWatch,
    service: ServiceConnection,
) -> bool {
    if *watch != AutoMediaPipeWatch::Pending {
        return false;
    }
    // Exhaustive on purpose: a new `ServiceConnection` variant must make a
    // deliberate decision here rather than silently falling into a
    // keep-waiting catch-all.
    match service {
        // First Connected = camera opened; the watch is done for good.
        ServiceConnection::Connected => {
            *watch = AutoMediaPipeWatch::Idle;
            false
        }
        // Errored before ever Connected = camera never opened (the worker
        // has already exited). Demote to mock.
        ServiceConnection::Errored => {
            *watch = AutoMediaPipeWatch::Idle;
            true
        }
        // Keep watching. NotStarted / Connecting = still handshaking.
        // ServiceMissing / Disconnected are never emitted by the MediaPipe
        // worker before its verdict (its first message is Connected or
        // Errored; ServiceMissing is Leap-specific, Disconnected implies a
        // prior Connected) — if one ever shows up mid-watch, keep watching:
        // the verdict contract is strictly Connected-or-Errored.
        ServiceConnection::NotStarted
        | ServiceConnection::Connecting
        | ServiceConnection::ServiceMissing
        | ServiceConnection::Disconnected => false,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;
    use crate::input::provider::ProviderRole;
    use crate::input::providers::mock::MockProvider;

    /// Install a `MockProvider` under an arbitrary id/role so the policy
    /// tests can observe which installers ran and what ended up registered.
    fn install_as(registry: &mut ProviderRegistry, id: ProviderId, role: ProviderRole) {
        registry.register(id, role, Box::new(MockProvider::default()));
    }

    /// Run `build_registry` against scripted installer outcomes, recording
    /// invocation counts.
    struct Script {
        leap_started: bool,
        mediapipe: Option<bool>,
        leap_calls: usize,
        mediapipe_calls: usize,
        mock_calls: usize,
    }

    impl Script {
        fn new(leap_started: bool, mediapipe: Option<bool>) -> Self {
            Self {
                leap_started,
                mediapipe,
                leap_calls: 0,
                mediapipe_calls: 0,
                mock_calls: 0,
            }
        }

        fn run(&mut self, choice: HandProviderChoice) -> BuiltRegistry {
            let leap_started = self.leap_started;
            let mediapipe = self.mediapipe;
            let (mut leap_calls, mut mediapipe_calls, mut mock_calls) = (0, 0, 0);
            let built = {
                let mut leap = |registry: &mut ProviderRegistry| {
                    leap_calls += 1;
                    // Installer contract: register and leave the provider in
                    // place even on failure (mirrors the binary, where
                    // `register` always inserts). Failure cleanup is
                    // `build_registry`'s job on the Auto path.
                    install_as(registry, ProviderId::Leap, ProviderRole::Primary);
                    leap_started
                };
                let mut mediapipe_fn = |registry: &mut ProviderRegistry| {
                    mediapipe_calls += 1;
                    if mediapipe.is_some() {
                        // Mirrors the binary: `register` always inserts, even
                        // when the provider fails to start.
                        install_as(registry, ProviderId::MediaPipe, ProviderRole::Primary);
                    }
                    mediapipe
                };
                let mut mock = |registry: &mut ProviderRegistry| {
                    mock_calls += 1;
                    install_as(registry, ProviderId::Mock, ProviderRole::Simulator);
                };
                build_registry(
                    choice,
                    &mut ProviderInstallers {
                        leap: &mut leap,
                        mediapipe: &mut mediapipe_fn,
                        mock: &mut mock,
                    },
                )
            };
            self.leap_calls = leap_calls;
            self.mediapipe_calls = mediapipe_calls;
            self.mock_calls = mock_calls;
            built
        }
    }

    fn ids(registry: &ProviderRegistry) -> Vec<ProviderId> {
        registry.iter().map(|p| p.id).collect()
    }

    #[test]
    fn off_builds_an_empty_registry_and_tries_nothing() {
        let mut script = Script::new(true, Some(true));
        let built = script.run(HandProviderChoice::Off);
        assert!(ids(&built.registry).is_empty());
        assert_eq!(
            (script.leap_calls, script.mediapipe_calls, script.mock_calls),
            (0, 0, 0)
        );
        assert_eq!(built.watch, AutoMediaPipeWatch::Idle);
    }

    #[test]
    fn leap_choice_keeps_failed_provider_visible_no_fallback() {
        let mut script = Script::new(false, Some(true));
        let built = script.run(HandProviderChoice::Leap);
        // Same philosophy as explicit MediaPipe: the dead provider stays
        // registered so the dev panel shows its Errored status, and no
        // silent mock masks the failure.
        assert_eq!(ids(&built.registry), [ProviderId::Leap]);
        assert_eq!(
            (script.leap_calls, script.mediapipe_calls, script.mock_calls),
            (1, 0, 0)
        );
    }

    #[test]
    fn mediapipe_choice_tries_only_mediapipe_keeps_failed_provider_visible() {
        let mut script = Script::new(true, Some(false));
        let built = script.run(HandProviderChoice::MediaPipe);
        // The dead provider stays registered: the dev panel must show its
        // Errored status when the operator explicitly chose this backend.
        assert_eq!(ids(&built.registry), [ProviderId::MediaPipe]);
        assert_eq!(
            (script.leap_calls, script.mediapipe_calls, script.mock_calls),
            (0, 1, 0)
        );
        assert_eq!(built.watch, AutoMediaPipeWatch::Idle);
    }

    #[test]
    fn mediapipe_choice_with_feature_absent_registers_nothing() {
        let mut script = Script::new(true, None);
        let built = script.run(HandProviderChoice::MediaPipe);
        assert!(ids(&built.registry).is_empty());
        assert_eq!(
            script.mock_calls, 0,
            "no silent fallback on explicit choice"
        );
    }

    #[test]
    fn auto_prefers_leap_and_skips_the_rest() {
        let mut script = Script::new(true, Some(true));
        let built = script.run(HandProviderChoice::Auto);
        assert_eq!(ids(&built.registry), [ProviderId::Leap]);
        assert_eq!((script.mediapipe_calls, script.mock_calls), (0, 0));
        assert_eq!(built.watch, AutoMediaPipeWatch::Idle);
    }

    #[test]
    fn auto_removes_failed_leap_candidate_then_mediapipe_and_watches() {
        let mut script = Script::new(false, Some(true));
        let built = script.run(HandProviderChoice::Auto);
        // Unlike the explicit Leap choice, Auto removes the failed Leap
        // candidate before falling through — no Primary corpse shadowing
        // the next candidate in primary_status().
        assert_eq!(ids(&built.registry), [ProviderId::MediaPipe]);
        assert_eq!(script.mock_calls, 0);
        assert_eq!(
            built.watch,
            AutoMediaPipeWatch::Pending,
            "optimistic MediaPipe start must be watched for camera failure"
        );
    }

    #[test]
    fn auto_removes_sync_failed_mediapipe_and_falls_back_to_mock() {
        let mut script = Script::new(false, Some(false));
        let built = script.run(HandProviderChoice::Auto);
        // Both failed candidates (Leap, then MediaPipe) are removed; the
        // mock must be the sole survivor so primary_status() reads it.
        assert_eq!(ids(&built.registry), [ProviderId::Mock]);
        assert_eq!(script.mock_calls, 1);
        assert_eq!(built.watch, AutoMediaPipeWatch::Idle);
    }

    #[test]
    fn auto_falls_back_to_mock_when_mediapipe_feature_absent() {
        let mut script = Script::new(false, None);
        let built = script.run(HandProviderChoice::Auto);
        // The failed Leap candidate is removed here too.
        assert_eq!(ids(&built.registry), [ProviderId::Mock]);
        assert_eq!(built.watch, AutoMediaPipeWatch::Idle);
    }

    // ── AutoMediaPipeWatch ──────────────────────────────────────────────

    #[test]
    fn watch_resolves_to_keep_on_first_connected() {
        let mut watch = AutoMediaPipeWatch::Pending;
        assert!(!auto_mediapipe_camera_failed(
            &mut watch,
            ServiceConnection::Connected
        ));
        assert_eq!(watch, AutoMediaPipeWatch::Idle);
        // A later transient error does NOT demote: the watch is done.
        assert!(!auto_mediapipe_camera_failed(
            &mut watch,
            ServiceConnection::Errored
        ));
    }

    #[test]
    fn watch_demotes_on_errored_before_connected() {
        let mut watch = AutoMediaPipeWatch::Pending;
        assert!(auto_mediapipe_camera_failed(
            &mut watch,
            ServiceConnection::Errored
        ));
        assert_eq!(
            watch,
            AutoMediaPipeWatch::Idle,
            "one-shot: never fires twice"
        );
    }

    #[test]
    fn watch_keeps_waiting_through_connecting() {
        let mut watch = AutoMediaPipeWatch::Pending;
        assert!(!auto_mediapipe_camera_failed(
            &mut watch,
            ServiceConnection::Connecting
        ));
        assert_eq!(watch, AutoMediaPipeWatch::Pending);
    }

    #[test]
    fn idle_watch_never_fires() {
        let mut watch = AutoMediaPipeWatch::Idle;
        assert!(!auto_mediapipe_camera_failed(
            &mut watch,
            ServiceConnection::Errored
        ));
    }
}
