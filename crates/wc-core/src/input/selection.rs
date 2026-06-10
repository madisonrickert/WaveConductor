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
//! for camera-open failure (see [`auto_mediapipe_camera_failed`]), and the
//! [`BuiltRegistry::leap_verdict_outstanding`] flag tells it to arm an
//! [`AutoLeapWatch`] for the analogous Leap case: the service connection
//! opens synchronously, but device *presence* arrives asynchronously via
//! Leap device events (see [`auto_leap_device_verdict`]).

use std::time::Duration;

use super::provider::{ProviderId, ProviderRegistry};
use super::state::{DevicePresence, ServiceConnection};
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

/// Result of [`build_registry`]: the populated registry plus which async
/// verdicts the caller must still watch for.
pub struct BuiltRegistry {
    /// The freshly-built registry for the requested choice.
    pub registry: ProviderRegistry,
    /// [`AutoMediaPipeWatch::Pending`] when `Auto` registered `MediaPipe`
    /// and its camera verdict is still outstanding; otherwise `Idle`.
    pub watch: AutoMediaPipeWatch,
    /// `true` when `Auto` registered Leap on its *service* connection alone
    /// — device presence arrives asynchronously via Leap device events, so
    /// the binary must arm an [`AutoLeapWatch::Pending`] with
    /// `deadline = now + AUTO_LEAP_DEVICE_GRACE` (this module has no access
    /// to Bevy's `Time`, so it cannot compute the deadline itself). Always
    /// `false` for the explicit `Leap` choice: failure stays visible there,
    /// no fallback (the existing philosophy).
    pub leap_verdict_outstanding: bool,
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
///   the mock if the webcam never opens. Leap's start is optimistic too —
///   it only proves the *service* connection, not an attached device — so
///   when Auto lands on Leap the result reports
///   [`BuiltRegistry::leap_verdict_outstanding`] and the caller must arm an
///   [`AutoLeapWatch`] (see [`auto_leap_device_verdict`]).
pub fn build_registry(
    choice: HandProviderChoice,
    installers: &mut ProviderInstallers<'_>,
) -> BuiltRegistry {
    let mut registry = ProviderRegistry::default();
    let mut watch = AutoMediaPipeWatch::Idle;
    let mut leap_verdict_outstanding = false;
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
            if (installers.leap)(&mut registry) {
                // A `started` Leap only proves the *service* connection
                // opened — the Ultraleap daemon runs whether or not a device
                // is plugged in, and device presence arrives asynchronously
                // via Leap device events. The caller must arm an
                // `AutoLeapWatch` and demote to the next candidate if no
                // device shows up (see `auto_leap_device_verdict`).
                leap_verdict_outstanding = true;
            } else {
                watch = demote_auto_leap(&mut registry, installers);
            }
        }
    }
    BuiltRegistry {
        registry,
        watch,
        leap_verdict_outstanding,
    }
}

/// `Auto`'s fall-through past a failed/dead Leap candidate: remove Leap from
/// `registry`, try `MediaPipe`, else install the silent mock.
///
/// Shared by [`build_registry`]'s `Auto` arm (Leap failed to start
/// synchronously) and the binary's per-frame system (the [`AutoLeapWatch`]
/// demoted a service-only Leap after the device grace period). Removing the
/// candidate first is Auto's cleanup obligation (see the installer contract
/// on [`ProviderInstallers`]): a dead `Primary` corpse must not shadow the
/// next candidate / mock in `ProviderRegistry::primary_status`.
///
/// Returns the `MediaPipe` camera watch the caller must adopt:
/// [`AutoMediaPipeWatch::Pending`] when `MediaPipe` was registered
/// optimistically, [`AutoMediaPipeWatch::Idle`] when the mock was installed.
pub fn demote_auto_leap(
    registry: &mut ProviderRegistry,
    installers: &mut ProviderInstallers<'_>,
) -> AutoMediaPipeWatch {
    registry.remove(ProviderId::Leap);
    match (installers.mediapipe)(registry) {
        Some(true) => {
            tracing::info!("hand-tracking: auto → MediaPipe registered (webcam verdict pending)");
            AutoMediaPipeWatch::Pending
        }
        Some(false) => {
            // Synchronous start failure (e.g. models missing): same Auto
            // cleanup obligation as the Leap candidate above.
            registry.remove(ProviderId::MediaPipe);
            tracing::info!("hand-tracking: auto → falling back to MockProvider");
            (installers.mock)(registry);
            AutoMediaPipeWatch::Idle
        }
        None => {
            tracing::info!("hand-tracking: auto → falling back to MockProvider");
            (installers.mock)(registry);
            AutoMediaPipeWatch::Idle
        }
    }
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

/// How long `Auto` waits for the Leap *device* after the service connection
/// opened, before demoting to the next candidate.
///
/// The Ultraleap daemon accepts connections with no controller plugged in,
/// and when one *is* plugged in its `Device` event lands well under a second
/// after connect. 3 s absorbs slow daemon device enumeration (cold service
/// start, USB re-handshake) without leaving the operator staring at a dead
/// `Auto` for long when no controller exists.
pub const AUTO_LEAP_DEVICE_GRACE: Duration = Duration::from_secs(3);

/// Whether `Auto`'s service-connected Leap provider still owes us a device
/// verdict.
///
/// `LeaprsProvider` connects to the Ultraleap *service* synchronously, so the
/// installer reports `started == true` even on a machine where the daemon
/// runs with no controller attached. Device presence
/// ([`DevicePresence::Attached`]) arrives asynchronously through the
/// provider's poll path, so while a watch is `Pending` the binary polls
/// [`auto_leap_device_verdict`] each frame until a device attaches, a failure
/// surfaces, or the deadline passes — the latter two demote to the next
/// `Auto` candidate (`MediaPipe`, else the mock).
///
/// Once a device has been seen the watch goes `Idle` permanently: a later
/// unplug is the provider's business (its honest `Lost` LED), exactly like
/// [`AutoMediaPipeWatch`] after its first `Connected`. Note the converse,
/// too: plugging a Leap in *after* a demote does **not** switch back
/// automatically — re-picking `Auto` in the dropdown re-probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoLeapWatch {
    /// Nothing to watch (not in Auto-with-Leap, or the verdict arrived).
    #[default]
    Idle,
    /// Auto registered Leap on its service connection; device verdict
    /// outstanding until `deadline` (same monotonic clock as the `now`
    /// passed to [`auto_leap_device_verdict`] — the binary uses Bevy
    /// `Time::elapsed`).
    Pending {
        /// Instant after which a still-absent device demotes Leap.
        deadline: Duration,
    },
}

/// Outcome of one [`auto_leap_device_verdict`] poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeapWatchVerdict {
    /// Keep the Leap provider; no watch outstanding (verdict arrived, or the
    /// watch was never armed).
    Keep,
    /// Verdict still outstanding; poll again next frame.
    KeepWaiting,
    /// No device materialized (or the service/device failed): the caller
    /// must replace Leap with the next `Auto` candidate via
    /// [`demote_auto_leap`].
    Demote,
}

/// Advance an [`AutoLeapWatch`] with the Leap provider's current device and
/// service state. Cheap (enum compares); intended to run every frame from
/// the binary's switch system while the watch is `Pending`.
///
/// Rule precedence while `Pending` (first match wins):
///
/// 1. Service `Errored`, or device `Lost` / `Failed` → `Demote` (the
///    provider is dead or the device failed; waiting out the grace period
///    would only delay the fallback). Checked *before* `Attached` so a
///    simultaneously-errored service wins — `Auto` means "give me whatever
///    works".
/// 2. Device `Attached` → `Keep`, watch `Idle` permanently (even when `now`
///    already passed the deadline: the event arrived, use it).
/// 3. `now >= deadline` with still no device → `Demote` (service runs, no
///    controller plugged in — the user-reported Auto-stuck-on-Leap case).
/// 4. Otherwise `KeepWaiting`. This includes `ServiceMissing` /
///    `Disconnected` / `Connecting` blips: they are not conclusive by
///    themselves, and the deadline backstops them.
pub fn auto_leap_device_verdict(
    watch: &mut AutoLeapWatch,
    device: DevicePresence,
    service: ServiceConnection,
    now: Duration,
) -> LeapWatchVerdict {
    let AutoLeapWatch::Pending { deadline } = *watch else {
        // Idle never fires: the verdict already arrived (or was never owed).
        return LeapWatchVerdict::Keep;
    };
    // Rule 1 — hard failures demote immediately, ahead of the deadline.
    if matches!(service, ServiceConnection::Errored)
        || matches!(device, DevicePresence::Lost | DevicePresence::Failed)
    {
        *watch = AutoLeapWatch::Idle;
        return LeapWatchVerdict::Demote;
    }
    // Rule 2 — a device showed up: Leap is the right Auto pick, for good.
    if matches!(device, DevicePresence::Attached) {
        *watch = AutoLeapWatch::Idle;
        return LeapWatchVerdict::Keep;
    }
    // Rule 3 — grace period exhausted with no device.
    if now >= deadline {
        *watch = AutoLeapWatch::Idle;
        return LeapWatchVerdict::Demote;
    }
    // Rule 4 — still inside the grace period.
    LeapWatchVerdict::KeepWaiting
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
        // Leap's start only proved the service connection; the device verdict
        // is async, so the caller must arm an AutoLeapWatch.
        assert!(
            built.leap_verdict_outstanding,
            "Auto-with-started-Leap must report the device verdict outstanding"
        );
    }

    #[test]
    fn explicit_leap_choice_owes_no_device_verdict() {
        // Explicit Leap gets NO watch: failure stays visible, no fallback.
        let mut script = Script::new(true, Some(true));
        let built = script.run(HandProviderChoice::Leap);
        assert!(!built.leap_verdict_outstanding);
    }

    #[test]
    fn auto_with_failed_leap_owes_no_device_verdict() {
        // The sync-failure path already fell through; nothing left to watch.
        let mut script = Script::new(false, Some(true));
        let built = script.run(HandProviderChoice::Auto);
        assert!(!built.leap_verdict_outstanding);
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

    // ── AutoLeapWatch / auto_leap_device_verdict ────────────────────────

    /// A watch pending until the full grace period from t=0.
    fn pending_grace() -> AutoLeapWatch {
        AutoLeapWatch::Pending {
            deadline: AUTO_LEAP_DEVICE_GRACE,
        }
    }

    #[test]
    fn leap_watch_attached_keeps_permanently() {
        let mut watch = pending_grace();
        assert_eq!(
            auto_leap_device_verdict(
                &mut watch,
                DevicePresence::Attached,
                ServiceConnection::Connected,
                Duration::from_millis(200),
            ),
            LeapWatchVerdict::Keep
        );
        assert_eq!(watch, AutoLeapWatch::Idle);
        // Permanent: a later unplug (Lost, past the deadline) is the
        // provider's business, never a demote.
        assert_eq!(
            auto_leap_device_verdict(
                &mut watch,
                DevicePresence::Lost,
                ServiceConnection::Connected,
                Duration::from_mins(1),
            ),
            LeapWatchVerdict::Keep
        );
    }

    #[test]
    fn leap_watch_attached_wins_even_at_the_deadline() {
        // The event arrived as the deadline expired: use it (rule 2 outranks
        // rule 3).
        let mut watch = pending_grace();
        assert_eq!(
            auto_leap_device_verdict(
                &mut watch,
                DevicePresence::Attached,
                ServiceConnection::Connected,
                AUTO_LEAP_DEVICE_GRACE,
            ),
            LeapWatchVerdict::Keep
        );
    }

    #[test]
    fn leap_watch_deadline_expiry_demotes() {
        let mut watch = pending_grace();
        assert_eq!(
            auto_leap_device_verdict(
                &mut watch,
                DevicePresence::NoDevice,
                ServiceConnection::Connected,
                AUTO_LEAP_DEVICE_GRACE,
            ),
            LeapWatchVerdict::Demote
        );
        assert_eq!(watch, AutoLeapWatch::Idle, "one-shot: never fires twice");
    }

    #[test]
    fn leap_watch_service_error_demotes_early() {
        let mut watch = pending_grace();
        assert_eq!(
            auto_leap_device_verdict(
                &mut watch,
                DevicePresence::NoDevice,
                ServiceConnection::Errored,
                Duration::from_millis(100), // well before the deadline
            ),
            LeapWatchVerdict::Demote
        );
        assert_eq!(watch, AutoLeapWatch::Idle);
    }

    #[test]
    fn leap_watch_device_lost_or_failed_demotes_early() {
        for device in [DevicePresence::Lost, DevicePresence::Failed] {
            let mut watch = pending_grace();
            assert_eq!(
                auto_leap_device_verdict(
                    &mut watch,
                    device,
                    ServiceConnection::Connected,
                    Duration::from_millis(100),
                ),
                LeapWatchVerdict::Demote,
                "{device:?}"
            );
        }
    }

    #[test]
    fn leap_watch_keeps_waiting_before_deadline() {
        let mut watch = pending_grace();
        assert_eq!(
            auto_leap_device_verdict(
                &mut watch,
                DevicePresence::NoDevice,
                ServiceConnection::Connected,
                Duration::from_millis(2999), // 1 ms shy of the 3 s grace deadline
            ),
            LeapWatchVerdict::KeepWaiting
        );
        assert_eq!(watch, pending_grace(), "deadline must not drift");
    }

    #[test]
    fn leap_watch_idle_never_demotes() {
        let mut watch = AutoLeapWatch::Idle;
        assert_eq!(
            auto_leap_device_verdict(
                &mut watch,
                DevicePresence::NoDevice,
                ServiceConnection::Errored,
                Duration::from_mins(1),
            ),
            LeapWatchVerdict::Keep
        );
        assert_eq!(watch, AutoLeapWatch::Idle);
    }

    // ── demote_auto_leap composition ────────────────────────────────────

    /// Run `demote_auto_leap` on a registry holding a Leap candidate, with
    /// scripted installer outcomes.
    fn run_demote(mediapipe: Option<bool>) -> (ProviderRegistry, AutoMediaPipeWatch) {
        let mut registry = ProviderRegistry::default();
        install_as(&mut registry, ProviderId::Leap, ProviderRole::Primary);
        let mut leap = |_: &mut ProviderRegistry| -> bool {
            unreachable!("demote never re-tries the Leap installer")
        };
        let mut mediapipe_fn = |registry: &mut ProviderRegistry| {
            if mediapipe.is_some() {
                install_as(registry, ProviderId::MediaPipe, ProviderRole::Primary);
            }
            mediapipe
        };
        let mut mock = |registry: &mut ProviderRegistry| {
            install_as(registry, ProviderId::Mock, ProviderRole::Simulator);
        };
        let watch = demote_auto_leap(
            &mut registry,
            &mut ProviderInstallers {
                leap: &mut leap,
                mediapipe: &mut mediapipe_fn,
                mock: &mut mock,
            },
        );
        (registry, watch)
    }

    #[test]
    fn demote_removes_leap_installs_mediapipe_and_watches() {
        let (registry, watch) = run_demote(Some(true));
        assert_eq!(ids(&registry), [ProviderId::MediaPipe]);
        assert_eq!(
            watch,
            AutoMediaPipeWatch::Pending,
            "optimistic MediaPipe start must be watched for camera failure"
        );
    }

    #[test]
    fn demote_falls_back_to_mock_on_mediapipe_sync_failure() {
        let (registry, watch) = run_demote(Some(false));
        assert_eq!(ids(&registry), [ProviderId::Mock]);
        assert_eq!(watch, AutoMediaPipeWatch::Idle);
    }

    #[test]
    fn demote_falls_back_to_mock_when_mediapipe_feature_absent() {
        let (registry, watch) = run_demote(None);
        assert_eq!(ids(&registry), [ProviderId::Mock]);
        assert_eq!(watch, AutoMediaPipeWatch::Idle);
    }
}
