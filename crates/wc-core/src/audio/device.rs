//! Output-device enumeration, name resolution, and topology diffing.
//!
//! ## Why the operator's device is remembered by *name*
//!
//! The kiosk's output endpoint is an HDMI TV. A TV that goes to sleep, or whose
//! input is switched away, simply stops being enumerated by the OS — and comes
//! back, under the same name, when it wakes. The persisted choice is therefore a
//! name, and a name that currently matches nothing is **not** an invalid choice:
//! it is a choice whose device is temporarily away. Resolution falls back to the
//! system default *for the session* and **keeps the saved name**, so the
//! supervisor can migrate back when it reappears. Both public functions here
//! take the saved name as `&str`, which makes "resolution never rewrites the
//! operator's choice" true at the type level rather than by convention — the
//! same discipline as `resolve_monitor_selection` in
//! `crate::settings::panel_user::display`.
//!
//! ## What runs where
//!
//! [`resolve_output_device`], [`saved_device_reappeared`],
//! [`bound_device_disappeared`], and [`default_device_switched`] are **pure** —
//! no host, no device, no thread — and carry the decisions this half turns on,
//! so they are unit-tested with literal name lists (CI has no audio device).
//! None of them allocates beyond the single owned name it returns, and none runs
//! per frame: the resolver runs on (re)build, the diffs run on the watcher's
//! ~2 s poll.
//!
//! [`enumerate_output_names`] calls into cpal and **can block** (WASAPI
//! enumeration in particular). It returns an `Option` precisely because "the
//! host has no output devices" and "we could not ask the host" are opposite
//! facts to the differ — see its docs. It is only ever called from (a) the
//! one-shot startup path and event-driven rebuilds on the **main thread**, and
//! (b) the device-watcher OS thread ([`spawn_device_watcher`]) — never the audio
//! callback and never a per-frame render system. On WASAPI, cpal initialises COM
//! per-thread internally (`com::com_initialized()` runs at the top of every
//! device operation), so calling this from a freshly spawned watcher thread is
//! sound without any manual `CoInitializeEx`.

use bevy::prelude::Resource;
#[cfg(not(target_arch = "wasm32"))]
use bevy::prelude::{Real, Res, ResMut, Time};

use crate::settings::RuntimeEnumOptionsSource;

/// The chosen output device after matching a saved name against the live list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceResolution {
    /// The saved name matched a live device; open it by name.
    Preferred(String),
    /// No usable saved name, or the saved name is not currently enumerated;
    /// open the host's default output device. `saved_unavailable` carries the
    /// name we are *keeping persisted* while falling back (e.g. a sleeping HDMI
    /// TV) so a later migrate-back can restore it, or `None` when the operator
    /// expressed no preference.
    Fallback {
        /// The saved-but-currently-absent device name, preserved for logging
        /// and for the migrate-back check. `None` when nothing was saved.
        saved_unavailable: Option<String>,
    },
}

/// Match a saved device name against the live output-device list.
///
/// An empty saved string is treated as "no preference" (the sentinel the
/// settings field uses for "system default"), never as a device literally named
/// `""`. A saved name that is not in `available` yields a
/// [`DeviceResolution::Fallback`] that **keeps the name** — resolution never
/// silently rewrites the operator's choice.
///
/// Matching is by exact name. If the host enumerates two devices under the same
/// name (two identical TVs, or one device reported twice), the first match wins,
/// which is the same device cpal's own by-name lookup would open — the pair is
/// indistinguishable to the operator either way.
#[must_use]
pub fn resolve_output_device(saved: Option<&str>, available: &[String]) -> DeviceResolution {
    match saved {
        Some(name) if !name.is_empty() && available.iter().any(|d| d == name) => {
            DeviceResolution::Preferred(name.to_owned())
        }
        Some(name) if !name.is_empty() => DeviceResolution::Fallback {
            saved_unavailable: Some(name.to_owned()),
        },
        _ => DeviceResolution::Fallback {
            saved_unavailable: None,
        },
    }
}

/// Whether a rebuild should be triggered to *migrate back* to the saved device.
///
/// True only on the rising edge: the saved endpoint is in `current` but was not
/// in `previous` (it just reappeared) **and** we are not already bound to it. A
/// missing or empty `saved`, or being already bound to it, yields `false`, so
/// steady-state polls never thrash the stream.
///
/// The diff is by **membership**, not by list equality: a host that re-orders
/// its device list without adding or removing anything (which some hosts do on
/// every enumeration) is not a topology change and must not provoke a rebuild
/// every poll for the rest of the session.
#[must_use]
pub fn saved_device_reappeared(
    saved: Option<&str>,
    previous: &[String],
    current: &[String],
    currently_bound: Option<&str>,
) -> bool {
    let Some(name) = saved.filter(|n| !n.is_empty()) else {
        return false;
    };
    if currently_bound == Some(name) {
        return false;
    }
    current.iter().any(|d| d == name) && !previous.iter().any(|d| d == name)
}

/// Whether the endpoint the live stream is bound to has **vanished** from a fresh
/// topology snapshot while the engine still believes it is running.
///
/// ## The second trigger for a stream death, and why one was not enough
///
/// The only other way a mid-run outage is noticed is cpal's error callback. But
/// cpal does not reliably raise a `StreamError` when an HDMI endpoint sleeps: on
/// several backends the stream becomes a *zombie* — it keeps getting its data
/// callback and renders into a void. When that happens:
///
/// - the status stays `Running` and an `AudioStream` is still present, so
///   `supervise_audio`'s `wants_reconnect` is false and no cycle ever starts; and
/// - [`saved_device_reappeared`] short-circuits on `currently_bound == saved`,
///   because the last successful build bound us to the very device that has since
///   gone away — so the TV coming back does not migrate us anywhere either.
///
/// Both safety nets are disarmed by the same stale binding, and the kiosk is
/// silent for the night. This is the check that arms them again: the watcher's
/// snapshot is a genuine second observation of the same fact, not a subordinate
/// one.
///
/// ## What it deliberately does *not* fire on
///
/// - **A failed enumeration.** `current` is only ever a snapshot the host actually
///   answered with; `topology_snapshot_to_publish` drops the "we could not ask"
///   case before it reaches a caller, so an enumeration error can never be read as
///   "every endpoint vanished".
/// - **A stream that is already down** (`running == false`): the error callback
///   got there first, the status is already `Reconnecting`, and the two triggers
///   must converge on one cycle rather than starting two.
/// - **A nameless binding** (`bound == None`): a device that cannot report its own
///   name is absent from every snapshot by construction (they are built from
///   `d.name().ok()`), so it would otherwise read as permanently missing. See
///   `engine::open_output_device`.
///
/// Pure, allocation-free, and only called when a snapshot actually arrived.
#[must_use]
pub fn bound_device_disappeared(bound: Option<&str>, current: &[String], running: bool) -> bool {
    let Some(name) = bound else {
        return false;
    };
    running && !current.iter().any(|d| d == name)
}

/// Whether a rebuild should be triggered because the host's **default** output
/// endpoint changed identity while the operator has no pinned device — i.e. the
/// stream is configured to *follow the system default* and the default moved out
/// from under it.
///
/// ## The third migrate trigger, and why the other two cannot cover it
///
/// "User plugs in the event PA and the OS promotes it to default" changes **no
/// list membership** when the old endpoint stays present, and it raises **no
/// cpal `StreamError`** on any backend (WASAPI errors the stream on *unplug*,
/// not on a default switch). So neither death trigger fires, and
/// [`saved_device_reappeared`] is inert with no saved name. Without this check a
/// kiosk following the system default keeps playing into the old endpoint all
/// night.
///
/// ## When it deliberately does *not* fire
///
/// - **A pinned device** (`saved` non-empty): the operator chose an endpoint
///   explicitly; the OS default is irrelevant to them, and yanking their stream
///   onto it would override that choice.
/// - **No live binding** (`bound == None`): the engine never came up or the
///   binding was cleared by a stream death — a reconnect cycle is already armed,
///   and its rebuild resolves to the *current* default anyway.
/// - **Already on the new default** (`bound == current`): nothing to do; this is
///   the steady state every successful fallback rebuild lands in.
/// - **Not a rising edge** (`previous == current`): snapshots are also published
///   for list-only changes, and re-firing on every one of those while
///   `bound != default` for any transient reason would turn each topology
///   publish into a stream rebuild.
///
/// Fires at most once per actual default switch (the edge), like
/// [`saved_device_reappeared`]. A pathological OS default that flaps A↔B
/// rebuilds once per flap — the same bounded exposure the reappearance edge
/// already accepts, and the supervisor's settle-window backoff still governs the
/// attempts if those rebuilds keep failing.
#[must_use]
pub fn default_device_switched(
    saved: Option<&str>,
    bound: Option<&str>,
    previous_default: Option<&str>,
    current_default: Option<&str>,
) -> bool {
    if saved.is_some_and(|name| !name.is_empty()) {
        return false;
    }
    let (Some(bound), Some(current)) = (bound, current_default) else {
        return false;
    };
    bound != current && previous_default != Some(current)
}

/// Live list of output-device names, refreshed by the device-watcher thread.
/// Read by the audio settings panel (via Plan 03a's runtime-enumerated dropdown,
/// see the [`RuntimeEnumOptionsSource`] impl below) and by the supervisor's
/// migrate-back check.
///
/// Main-thread-only resource; the watcher thread never touches it directly (it
/// sends snapshots over a channel that a main-thread system drains into here).
///
/// An empty list is a normal state, not an error: before the watcher's first
/// poll lands — and forever, in a headless harness — there is nothing to report.
/// 03a omits the key from its snapshot in that case and still renders the
/// persisted value.
#[derive(Resource, Default, Debug, Clone)]
pub struct AvailableAudioDevices(pub Vec<String>);

/// Feeds the live output-device list to Plan 03a's runtime-enum settings widget.
///
/// [`crate::audio::settings::AudioSettings::output_device`] declares
/// `options_key = "audio_output_devices"`, and 03a's panel resolves that key
/// against every registered [`RuntimeEnumOptionsSource`] at render time — so this
/// key and the field's must stay identical. Two tests pin the halves together:
/// `settings::output_device_options_key_matches_its_options_source` (the two
/// literals agree) and `crate::audio::tests::the_output_device_fields_options_key_resolves_against_a_registered_source`
/// (the source is actually registered with the `App`). A debug-build startup
/// check (`settings::runtime_enum::warn_on_unresolved_options_keys`) catches a
/// drift at runtime.
///
/// This is a cheap field read, as the trait requires: the blocking cpal
/// enumeration happens on the watcher thread, never here — the panel renders per
/// frame.
impl RuntimeEnumOptionsSource for AvailableAudioDevices {
    const OPTIONS_KEY: &'static str = "audio_output_devices";

    fn options(&self) -> &[String] {
        &self.0
    }
}

/// Name of the output device the live stream is currently bound to, or `None`
/// before the engine starts / when it failed to build.
///
/// Set on every successful (re)build (Task 5). The migrate-back check compares
/// against this so it does not rebuild a stream that is already on the saved
/// device.
#[derive(Resource, Default, Debug, Clone)]
pub struct BoundOutputDevice(pub Option<String>);

/// Name of the host's current **default** output endpoint, as last reported by
/// the device watcher; `None` before the first snapshot lands (and forever, in a
/// headless harness) or when the host has no default endpoint at all.
///
/// This is the "previous" half of [`default_device_switched`]'s rising-edge
/// check: [`drain_device_topology`] compares each incoming snapshot's default
/// against it, then overwrites it. Main-thread-only bookkeeping — the watcher
/// thread never touches it.
#[derive(Resource, Default, Debug, Clone)]
pub struct DefaultOutputDevice(pub Option<String>);

/// One watcher observation: the canonical output-name list plus the identity of
/// the host's current **default** output endpoint.
///
/// The default is carried alongside the list because it can change while the
/// membership does not (a default-device *switch* with the old endpoint still
/// present — see [`default_device_switched`]), and the list can change while the
/// default does not. Either difference is a publishable topology change.
///
/// `default_output: None` means the host reported no default endpoint (or the
/// endpoint could not report a name — cpal's API cannot distinguish the two);
/// it never means "the query failed", because a failed *list* enumeration skips
/// the whole tick upstream and this snapshot is never built.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct OutputTopology {
    /// Sorted, non-de-duplicated output names; see [`canonical_output_names`].
    pub outputs: Vec<String>,
    /// Name of the host's default output device, if it has one with a name.
    pub default_output: Option<String>,
}

/// Enumerate the host's output devices and collect their names, sorted.
///
/// **Can block** (WASAPI). Only called on the main thread (startup / rebuild)
/// or the watcher thread — see the module header. A device whose name cannot be
/// read is skipped.
///
/// ## `None` is not `Some(vec![])`
///
/// - `Some(names)` — we asked the host and this is the answer. `Some(vec![])`
///   means it genuinely enumerates **no** output device right now, which is a
///   real, expected state and not a bug: when the only endpoint is a sleeping
///   HDMI TV, the host reports nothing.
/// - `None` — we could not ask (`host.output_devices()` itself errored). This
///   carries **no information about the topology**.
///
/// The distinction matters to the *differ*, not the resolver.
/// [`saved_device_reappeared`] compares a previous snapshot against a current
/// one, so folding a failed enumeration into an empty list would read as "every
/// endpoint vanished" — and the next successful poll would then look like a
/// **rising edge**, provoking a spurious stream rebuild. The watcher therefore
/// treats `None` as "don't diff; keep the previous snapshot" and skips the tick —
/// see `topology_snapshot_to_publish`, where that rule lives and is tested.
/// [`resolve_output_device`], by contrast, is right to treat an
/// empty list as "nothing available -> fall back to the default (and keep the
/// saved name)": at build time we must pick *something*, and there is nothing to
/// pick.
///
/// The result is **sorted** and *not* de-duplicated (see
/// `canonical_output_names`, which is where that happens and where it is
/// tested). Sorting makes the snapshot canonical, so a host that re-orders its
/// enumeration between polls does not look like a topology change to the watcher,
/// and the settings dropdown has a stable order. De-duplicating would silently
/// hide one of two identically-named endpoints from that dropdown, so duplicates
/// are kept as reported.
///
/// Allocates a `Vec<String>` (cpal returns owned names); this is forced by
/// cpal's API and is acceptable because it runs at most every ~2 s on a
/// background thread, never on the audio callback or a per-frame render system.
///
/// ## This logs on every failure — do not call it on a schedule
///
/// The `warn!` below is written for the **one-shot** caller (the main-thread
/// startup/rebuild path), where a failure is worth a line. A caller that polls —
/// i.e. the device watcher, every ~2 s — would turn a persistently failing host
/// (a restarted Windows `audiosrv`, a wedged macOS `coreaudiod`) into ~1,800
/// formatted, allocating log lines an hour. Such callers use
/// `try_enumerate_output_names` and log the *edges* themselves; see
/// [`spawn_device_watcher`].
#[cfg(not(target_arch = "wasm32"))]
pub fn enumerate_output_names(host: &cpal::Host) -> Option<Vec<String>> {
    match try_enumerate_output_names(host) {
        Ok(names) => Some(names),
        Err(err) => {
            // Not "every device disappeared" — "we could not ask". The caller
            // must keep its previous snapshot rather than diff against this.
            tracing::warn!(?err, "cpal output_devices enumeration failed");
            None
        }
    }
}

/// [`enumerate_output_names`] without the logging: the host's answer, or the
/// host's error, handed back verbatim.
///
/// Exists for the one caller that runs on a schedule (the device-watcher thread),
/// which must decide for itself *when* a failure is worth a line — it logs the
/// transition into and out of failure, not every poll. Everything else should use
/// [`enumerate_output_names`], which logs the failure once and folds it to `None`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn try_enumerate_output_names(
    host: &cpal::Host,
) -> Result<Vec<String>, cpal::DevicesError> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let devices = host.output_devices()?;
    Ok(canonical_output_names(
        devices.filter_map(|d| d.name().ok()),
    ))
}

/// Collect device names into the **canonical** (sorted, not de-duplicated) form
/// the topology differ compares by exact equality.
///
/// The sort is load-bearing, not cosmetic: `topology_snapshot_to_publish`
/// compares snapshots with `==`, so without it a host that re-orders its
/// enumeration between polls (some do, on every call) would read as a topology
/// change every 2 s forever — republishing, and with a saved name, rebuilding the
/// stream. Extracted from [`enumerate_output_names`] precisely so that property
/// can be pinned by a test on a machine with no audio host at all (CI has none):
/// see `canonicalisation_sorts_whatever_order_the_host_reports`.
///
/// Duplicates are **kept** as reported: two identically-named endpoints (two of
/// the same TV) are two endpoints, and hiding one from the settings dropdown
/// would be a lie.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
fn canonical_output_names(names: impl Iterator<Item = String>) -> Vec<String> {
    let mut names: Vec<String> = names.collect();
    names.sort_unstable();
    names
}

/// Apply an incoming topology snapshot to the live list and report whether the
/// saved endpoint just reappeared (so the caller should trigger a migrate-back).
///
/// Pure: `available` is the previous list on the way in, `incoming` is the fresh
/// snapshot. Compares them with [`saved_device_reappeared`] *before* overwriting,
/// then moves `incoming` into `available` (no clone). The list is replaced even
/// when no migrate is warranted — the settings dropdown reads it, so a device
/// merely *going away* still has to land.
///
/// Runs at the watcher's cadence (only when a snapshot actually arrived), never
/// per frame; see [`drain_device_topology`].
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub(crate) fn apply_topology(
    available: &mut Vec<String>,
    incoming: Vec<String>,
    saved: Option<&str>,
    bound: Option<&str>,
) -> bool {
    let migrate = saved_device_reappeared(saved, available, &incoming, bound);
    *available = incoming;
    migrate
}

/// One full watcher poll: the output-name list plus the default endpoint's name.
///
/// **Can block** (both halves are WASAPI COM calls); watcher thread only. The
/// default is queried only after the list enumeration succeeded — a host broken
/// enough to fail `output_devices()` gives no trustworthy default either, and
/// keeping the two in one all-or-nothing result preserves the differ's "a failed
/// poll teaches us nothing, skip the tick" rule for both.
///
/// The extra `String` this allocates per poll (the default's name) is in the
/// same cpal-forced class as the name list itself: ~every 2 s on the watcher
/// thread, never the audio callback and never a per-frame system. cpal's
/// `default_output_device()` folds "no default" and "query failed" into `None`;
/// both land here as `default_output: None`, which is the honest reading —
/// either way there is currently no default to follow.
#[cfg(not(target_arch = "wasm32"))]
fn poll_output_topology(host: &cpal::Host) -> Result<OutputTopology, cpal::DevicesError> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let outputs = try_enumerate_output_names(host)?;
    let default_output = host.default_output_device().and_then(|d| d.name().ok());
    Ok(OutputTopology {
        outputs,
        default_output,
    })
}

/// The ~2 s cadence at which the watcher re-enumerates output devices.
#[cfg(not(target_arch = "wasm32"))]
const WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Granularity at which the watcher wakes to check its stop flag, so app
/// shutdown joins the thread promptly instead of waiting out a full interval.
#[cfg(not(target_arch = "wasm32"))]
const WATCH_TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// Decide what one watcher poll should publish, given the previous snapshot and
/// the result of [`poll_output_topology`].
///
/// This is the whole decision the watcher thread makes, extracted so it is
/// testable without a device, a thread, or a two-second sleep. The thread around
/// it is a dumb loop.
///
/// - `polled == None` — the host enumeration **errored**; we learned nothing.
///   The tick is skipped: no diff, no publish, and the caller keeps its previous
///   snapshot. Folding this into "the list is now empty" would make the next
///   *successful* poll look like a rising edge and provoke a spurious rebuild.
/// - `polled == Some(topology)` equal to `last` — steady state (the
///   overwhelmingly common case). Nothing to publish, so the channel stays quiet.
/// - `polled == Some(topology)` differing from `last` in **either** field — the
///   name list *or* the default endpoint (including the very first poll, where
///   `last` is `None`) — a real topology change. Return it; the caller publishes
///   it and adopts it as the new `last`. A default-only difference is a real
///   change: it is the entire signal behind [`default_device_switched`].
///
/// An empty `outputs` is a legitimate answer, not an error: when the only
/// endpoint is a sleeping HDMI TV the host genuinely enumerates nothing.
/// Equality is exact, which is sound precisely because the list is sorted — a
/// host that re-orders its enumeration between polls is not a topology change.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub(crate) fn topology_snapshot_to_publish(
    last: Option<&OutputTopology>,
    polled: Option<OutputTopology>,
) -> Option<OutputTopology> {
    let current = polled?;
    if last == Some(&current) {
        return None;
    }
    Some(current)
}

/// Owns the device-watcher OS thread. Dropping it signals the thread to stop and
/// joins it, so the app exits cleanly and the thread can never outlive the
/// process. A Bevy `Resource`; Bevy drops it on app teardown.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Resource)]
pub struct DeviceWatcher {
    /// Set to `true` to ask the thread to exit at its next 100 ms tick.
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Join handle, taken on `Drop`. `None` when the thread could not be spawned
    /// at all (see [`spawn_device_watcher`]).
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for DeviceWatcher {
    /// Signal the thread to stop and join it.
    ///
    /// The stop flag is observed at each 100 ms sleep tick, so the join normally
    /// costs ≤ ~100 ms — **plus any enumeration already in flight**. The flag is
    /// *not* checked inside `host.output_devices()`, which is the one call
    /// documented as blocking (WASAPI, especially against an endpoint that is
    /// being torn down at the same moment), so a quit that lands mid-poll waits
    /// that call out. Nothing can be done about that from this side short of
    /// detaching the thread, which would trade a bounded shutdown stall for an
    /// unbounded one.
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            // A failed join means the watcher panicked. Log it and carry on:
            // this runs during teardown, and a watcher panic must degrade audio
            // recovery, never take the app down with it. The panic itself was
            // already logged at the time it happened (see `spawn_device_watcher`),
            // which is hours earlier than this line on a soak.
            if handle.join().is_err() {
                tracing::warn!("device-watcher thread panicked before join");
            }
        }
    }
}

/// Consumer end of the watcher → main-thread topology channel.
///
/// `mpsc::Receiver` is `Send` but not `Sync`, so — like the audio rings — it is
/// installed as a **non-send** resource and only ever read on the main thread.
#[cfg(not(target_arch = "wasm32"))]
pub struct DeviceTopologyReceiver {
    /// Receives a fresh topology snapshot only when it actually changed (name
    /// list or default endpoint).
    rx: std::sync::mpsc::Receiver<OutputTopology>,
}

#[cfg(not(target_arch = "wasm32"))]
impl DeviceTopologyReceiver {
    /// Collapse everything the watcher has queued since the last frame to the
    /// newest snapshot, or `None` if nothing arrived.
    ///
    /// Allocation-free in steady state: `try_recv` on an empty channel yields
    /// `Err` immediately and this returns `None` without touching the heap. A
    /// disconnected channel (the watcher exited, or panicked) is indistinguishable
    /// from an empty one here, and deliberately so — it is a normal end-of-life
    /// state, not something to log once per frame.
    fn latest(&self) -> Option<OutputTopology> {
        let mut newest = None;
        while let Ok(snapshot) = self.rx.try_recv() {
            newest = Some(snapshot);
        }
        newest
    }
}

/// Spawn the device-watcher thread. Returns the owning [`DeviceWatcher`] resource
/// and the [`DeviceTopologyReceiver`] the main thread drains.
///
/// ## What runs on the thread
///
/// The thread builds its **own** `cpal::Host` in-thread (hosts are not moved
/// across threads; on WASAPI cpal initialises COM per-thread internally), then
/// loops: enumerate, publish *only if the list changed*, and sleep
/// `WATCH_INTERVAL` in `WATCH_TICK` increments, checking the stop flag at each
/// increment. Enumeration can block — which is precisely why it is here and not
/// on the audio callback or the render thread.
///
/// ## Steady-state cost
///
/// It re-uses one `last` buffer across the whole session. The only allocations per
/// poll are the ones cpal's API forces (the `Vec<String>` of names it hands back),
/// and the only *extra* one — the clone handed to the channel — happens on a real
/// topology change, not on a poll. In steady state the channel is silent and the
/// main thread's drain does nothing.
///
/// ## A failing host is logged on the edges, not on every poll
///
/// A host whose enumeration errors *persistently* (a restarted Windows
/// `audiosrv`, a wedged macOS `coreaudiod`) fails every poll for as long as it is
/// broken. Logging each one would emit ~1,800 formatted, allocating lines an hour
/// — ~14,000 over an 8-hour soak — from the one thread whose entire premise is
/// being cheap in steady state. So the loop calls `try_enumerate_output_names`
/// (which does not log) and keeps a single `failing: bool`: it warns **once** when
/// enumeration starts failing and logs **once** when it recovers. The failure stays
/// discoverable; it just stops being a flood.
///
/// ## Lifetime
///
/// The thread cannot outlive the app: `DeviceWatcher`'s `Drop` sets the stop flag
/// and joins (see its docs for the shutdown-latency caveat). It also exits on its
/// own if the receiver is dropped — `send` failing means the main thread is gone,
/// so it returns rather than spinning. If it panics, the app is unaffected: the
/// channel simply disconnects, the drain sees nothing, and audio degrades to "no
/// early migrate-back" while the supervisor's backoff still recovers the stream.
/// The panic is caught and logged **at the moment it happens** rather than only
/// surfacing as a failed join at app exit: on a soak, a watcher that dies in hour 1
/// is otherwise silently gone for the remaining seven.
///
/// On spawn failure it returns a shell with no thread. The app still runs; it just
/// cannot see topology changes, and recovery falls back to the supervisor's timed
/// retries.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn spawn_device_watcher() -> (DeviceWatcher, DeviceTopologyReceiver) {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let (tx, rx) = std::sync::mpsc::channel::<OutputTopology>();

    let spawned = std::thread::Builder::new()
        .name("wc-audio-device-watcher".to_owned())
        .spawn(move || {
            // Catch a panic here so it is reported *when it happens*. Without
            // this, the only trace of a watcher that died at hour 1 of a soak is
            // the failed join in `DeviceWatcher::drop`, hours later at app exit.
            // `AssertUnwindSafe` is sound because nothing observable is shared:
            // the loop owns its `host` and `last`, and the channel's other end
            // only ever sees whole snapshots that were already sent.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                watch_devices(&stop_thread, &tx);
            }));
            if result.is_err() {
                // Degraded, not fatal: audio still works and the supervisor's
                // backoff still recovers the stream — only the early
                // migrate-back is gone. Say so, rather than leaving the operator
                // to infer it from silence.
                tracing::error!(
                    "device-watcher thread panicked; topology changes will go unnoticed for the \
                     rest of this session (audio recovery falls back to the supervisor's backoff)"
                );
            }
        });

    match spawned {
        Ok(handle) => (
            DeviceWatcher {
                stop,
                handle: Some(handle),
            },
            DeviceTopologyReceiver { rx },
        ),
        Err(err) => {
            tracing::warn!(
                ?err,
                "failed to spawn device-watcher thread; topology changes will go unnoticed"
            );
            (
                DeviceWatcher { stop, handle: None },
                DeviceTopologyReceiver { rx },
            )
        }
    }
}

/// The watcher thread's body: build a host, then poll → publish-if-changed →
/// sleep until asked to stop (or until the main thread drops the receiver).
///
/// Split out of [`spawn_device_watcher`] so the `catch_unwind` there wraps one
/// named thing rather than a closure the size of the whole loop.
///
/// Enumeration failures are logged on the **edges** only (see the `failing` flag
/// below and the function's caller docs): a persistently broken host must not turn
/// a 2 s poll into a log flood for the length of a soak.
#[cfg(not(target_arch = "wasm32"))]
fn watch_devices(
    stop: &std::sync::atomic::AtomicBool,
    tx: &std::sync::mpsc::Sender<OutputTopology>,
) {
    use std::sync::atomic::Ordering;

    let host = cpal::default_host();
    // Reused across the session; the first poll (`last == None`) always
    // publishes, so the dropdown and the resolver see a list without
    // waiting out an interval.
    let mut last: Option<OutputTopology> = None;
    // Whether the *previous* poll's enumeration failed. The one piece of state
    // that turns a per-poll warning into a per-outage one.
    let mut failing = false;

    while !stop.load(Ordering::Relaxed) {
        let polled = match poll_output_topology(&host) {
            Ok(topology) => {
                if failing {
                    tracing::info!("cpal output-device enumeration recovered");
                    failing = false;
                }
                Some(topology)
            }
            Err(err) => {
                if !failing {
                    tracing::warn!(
                        ?err,
                        "cpal output-device enumeration is failing; the device watcher will keep \
                         polling every 2 s and log once when it recovers"
                    );
                    failing = true;
                }
                // "We could not ask" — not "every device disappeared". The
                // differ must keep the previous snapshot; see
                // `topology_snapshot_to_publish`.
                None
            }
        };

        if let Some(current) = topology_snapshot_to_publish(last.as_ref(), polled) {
            // The one clone: the channel takes ownership, and we keep a copy to
            // diff the next poll against. Only on a real change.
            if tx.send(current.clone()).is_err() {
                return; // main side dropped the receiver — we are done
            }
            last = Some(current);
        }

        // Sleep the interval in short increments so a stop request is honoured
        // within ~one tick rather than ~one interval.
        let mut waited = std::time::Duration::ZERO;
        while waited < WATCH_INTERVAL {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(WATCH_TICK);
            waited += WATCH_TICK;
        }
    }
}

/// `PreUpdate` system: pull the newest topology snapshot off the watcher channel,
/// update [`AvailableAudioDevices`] / [`DefaultOutputDevice`], and act on the
/// three edges it can carry.
///
/// ## The three edges
///
/// 1. **Our endpoint vanished** ([`bound_device_disappeared`]) — the stream is a
///    zombie: cpal never raised a `StreamError`, but the device it renders into is
///    gone. Clear the **binding** and drive
///    [`crate::audio::state::AudioStatus::Reconnecting`]; the supervisor takes it
///    from there on its normal 1 s-first backoff. This is a full second trigger
///    for a stream death, not a subordinate one — see `bound_device_disappeared`
///    for why one was not enough.
/// 2. **The operator's saved endpoint reappeared** ([`saved_device_reappeared`]) —
///    ask the supervisor to bring its next attempt forward rather than waiting out
///    a backoff that may be as long as 30 s, so the stream migrates back promptly.
/// 3. **The system default switched while we follow it**
///    ([`default_device_switched`]) — no saved preference, and the host promoted a
///    different endpoint to default (the event PA was just plugged in) without
///    erroring the live stream or removing its device. Ask the supervisor for an
///    immediate rebuild; `rebuild_engine`'s fallback path opens the *current*
///    default, which is precisely the migration wanted.
///
/// They compose: a TV that sleeps and later wakes fires (1) then (2), and if a
/// snapshot somehow carries several they still converge on **one** reconnect cycle
/// — (1) only moves a status that is still `Running`, (3) short-circuits on a
/// cleared binding, and the supervisor's `begin` is gated on no cycle already
/// being armed.
///
/// The saved **setting** is never touched by any edge. Only
/// [`BoundOutputDevice`] — what we are currently *bound to* — is cleared, and only
/// by (1). The operator's choice survives its device being away; that is the whole
/// premise of remembering it by name.
///
/// ## Cost
///
/// Runs on the **main thread** ([`DeviceTopologyReceiver`] is non-send) and every
/// frame — so it is a hot path. It never enumerates (the blocking enumeration
/// already happened on the watcher thread) and it never allocates in steady state:
/// the common case is an empty channel, `DeviceTopologyReceiver::latest` returns
/// `None`, and the system returns before touching anything else. When a snapshot
/// *has* arrived it only moves an already-built `Vec<String>` into the resource.
///
/// `saved` is the operator's persisted
/// [`crate::audio::settings::AudioSettings::output_device`], read (never written)
/// as a `&str`. `Option<Res<…>>` degrades cleanly if the settings resource is
/// somehow absent (a harness that loads `AudioPlugin`'s systems without its
/// settings registration); in the app it is inserted at plugin build.
#[cfg(not(target_arch = "wasm32"))]
pub fn drain_device_topology(
    receiver: Option<bevy::ecs::system::NonSend<'_, DeviceTopologyReceiver>>,
    mut available: ResMut<'_, AvailableAudioDevices>,
    mut bound: ResMut<'_, BoundOutputDevice>,
    mut default_device: ResMut<'_, DefaultOutputDevice>,
    mut state: ResMut<'_, crate::audio::state::AudioState>,
    settings: Option<Res<'_, crate::audio::settings::AudioSettings>>,
    mut supervisor: ResMut<'_, crate::audio::supervisor::AudioSupervisor>,
    time: Res<'_, Time<Real>>,
) {
    // Absent until `start_audio_engine` has spawned the watcher — and, in the
    // headless test harnesses, forever. `Option` (not a bare `NonSend`) is what
    // makes this always-on system safe to register unconditionally.
    let Some(receiver) = receiver else {
        return;
    };
    let Some(incoming) = receiver.latest() else {
        return;
    };
    let OutputTopology {
        outputs: incoming_outputs,
        default_output: incoming_default,
    } = incoming;

    // Edge 1: the endpoint under the live stream is gone. Checked against the
    // *incoming* snapshot, before `apply_topology` consumes it.
    let running = state.status == crate::audio::state::AudioStatus::Running;
    if bound_device_disappeared(bound.0.as_deref(), &incoming_outputs, running) {
        tracing::warn!(
            device = bound.0.as_deref().unwrap_or_default(),
            "the bound output device is no longer enumerated; treating the stream as dead. \
             Entering Reconnecting — the supervisor will rebuild it (the saved device setting is \
             untouched)."
        );
        // The *binding*, not the setting: we are no longer on that endpoint, and
        // leaving the stale name here is what disarmed the migrate-back too.
        bound.0 = None;
        crate::audio::state::mark_reconnecting_from_device_loss(&mut state);
    }

    // Borrowed, not cloned: this system runs every frame, and the empty string is
    // the "no preference" sentinel `resolve_output_device`/`saved_device_reappeared`
    // already understand.
    let saved: Option<&str> = settings
        .as_ref()
        .map(|s| s.output_device.as_str())
        .filter(|name| !name.is_empty());

    // Edge 3: the system default switched while nothing is pinned. Read after
    // edge 1 so a binding it just cleared short-circuits this (a reconnect cycle
    // is starting anyway, and its rebuild resolves to the current default).
    if default_device_switched(
        saved,
        bound.0.as_deref(),
        default_device.0.as_deref(),
        incoming_default.as_deref(),
    ) {
        tracing::info!(
            from = bound.0.as_deref().unwrap_or_default(),
            to = incoming_default.as_deref().unwrap_or_default(),
            "system default output device changed and no device is pinned in settings; \
             requesting an immediate stream rebuild to follow it"
        );
        supervisor.request_now(time.elapsed_secs_f64());
    }
    // Adopt the new default *after* the edge check — it is the check's
    // "previous" half.
    default_device.0 = incoming_default;

    // Edge 2: the saved endpoint came back. `bound` is read *after* edge 1 may have
    // cleared it, so a device that vanished and returned in the same snapshot gap
    // is still seen as something to migrate to.
    if apply_topology(
        &mut available.0,
        incoming_outputs,
        saved,
        bound.0.as_deref(),
    ) {
        // Bring the next reconnect attempt forward instead of waiting out a
        // backoff that may be as long as 30 s. `Time<Real>` is the monotonic
        // clock the supervisor's contract requires.
        supervisor.request_now(time.elapsed_secs_f64());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn saved_name_present_resolves_to_preferred() {
        let available = names(&["Built-in", "LG TV (HDMI)"]);
        assert_eq!(
            resolve_output_device(Some("LG TV (HDMI)"), &available),
            DeviceResolution::Preferred("LG TV (HDMI)".to_owned()),
        );
    }

    #[test]
    fn saved_name_absent_falls_back_but_keeps_the_name() {
        // The HDMI TV is merely asleep and not enumerated right now. We fall
        // back to the default so there is *some* sound, but we must remember
        // the operator's choice so a later migrate-back can restore it.
        let available = names(&["Built-in"]);
        assert_eq!(
            resolve_output_device(Some("LG TV (HDMI)"), &available),
            DeviceResolution::Fallback {
                saved_unavailable: Some("LG TV (HDMI)".to_owned()),
            },
        );
    }

    #[test]
    fn no_saved_name_or_empty_falls_back_with_no_regret() {
        let available = names(&["Built-in"]);
        assert_eq!(
            resolve_output_device(None, &available),
            DeviceResolution::Fallback {
                saved_unavailable: None
            },
        );
        // An empty stored string is "no choice", not a device literally named "".
        assert_eq!(
            resolve_output_device(Some(""), &available),
            DeviceResolution::Fallback {
                saved_unavailable: None
            },
        );
    }

    /// Every endpoint is gone — the state the kiosk is actually in while its
    /// only display sleeps. A saved name still survives the round trip; the
    /// operator's choice is not collateral damage of an empty enumeration.
    #[test]
    fn an_empty_device_list_still_preserves_the_saved_name() {
        let none: Vec<String> = Vec::new();
        assert_eq!(
            resolve_output_device(Some("LG TV (HDMI)"), &none),
            DeviceResolution::Fallback {
                saved_unavailable: Some("LG TV (HDMI)".to_owned()),
            },
        );
        assert_eq!(
            resolve_output_device(None, &none),
            DeviceResolution::Fallback {
                saved_unavailable: None
            },
        );
    }

    /// Two endpoints reported under the same name (identical TVs, or one device
    /// enumerated twice) still resolve — ambiguously, but to *a* real device,
    /// which is what cpal's own by-name lookup does.
    #[test]
    fn a_duplicate_name_still_resolves_to_preferred() {
        let available = names(&["LG TV (HDMI)", "LG TV (HDMI)"]);
        assert_eq!(
            resolve_output_device(Some("LG TV (HDMI)"), &available),
            DeviceResolution::Preferred("LG TV (HDMI)".to_owned()),
        );
    }

    #[test]
    fn reappearance_is_true_only_on_the_rising_edge_when_not_already_bound() {
        let saved = Some("LG TV (HDMI)");
        let without = names(&["Built-in"]);
        let with = names(&["Built-in", "LG TV (HDMI)"]);

        // Rising edge: absent last poll, present now, and we are on the fallback.
        assert!(saved_device_reappeared(
            saved,
            &without,
            &with,
            Some("Built-in")
        ));
        // Steady presence (was already there) is not an edge.
        assert!(!saved_device_reappeared(
            saved,
            &with,
            &with,
            Some("Built-in")
        ));
        // Already bound to the saved device: nothing to migrate.
        assert!(!saved_device_reappeared(
            saved,
            &without,
            &with,
            Some("LG TV (HDMI)")
        ));
        // No saved preference: never migrate.
        assert!(!saved_device_reappeared(
            None,
            &without,
            &with,
            Some("Built-in")
        ));
        // Still absent: not an edge.
        assert!(!saved_device_reappeared(
            saved,
            &without,
            &without,
            Some("Built-in")
        ));
    }

    /// An empty saved name is the "system default" sentinel, not a device — it
    /// can never reappear, so it can never trigger a migrate-back.
    #[test]
    fn an_empty_saved_name_never_migrates() {
        let without = names(&["Built-in"]);
        let with = names(&["Built-in", ""]);
        assert!(!saved_device_reappeared(
            Some(""),
            &without,
            &with,
            Some("Built-in")
        ));
    }

    /// The TV sleeps, wakes, sleeps, wakes. Each *wake* is an edge; the polls in
    /// between are not. This is the multi-hour soak's actual shape.
    #[test]
    fn a_device_cycling_away_and_back_fires_once_per_return() {
        let saved = Some("LG TV (HDMI)");
        let bound = Some("Built-in");
        let without = names(&["Built-in"]);
        let with = names(&["Built-in", "LG TV (HDMI)"]);

        // Wake #1.
        assert!(saved_device_reappeared(saved, &without, &with, bound));
        // Still awake, next poll: no edge.
        assert!(!saved_device_reappeared(saved, &with, &with, bound));
        // Sleeps again: a *disappearance* is never a migrate-back trigger (the
        // stream-death path handles that).
        assert!(!saved_device_reappeared(saved, &with, &without, bound));
        // Wake #2: an edge again.
        assert!(saved_device_reappeared(saved, &without, &with, bound));
    }

    /// A host that re-orders its enumeration without adding or removing anything
    /// has not changed the topology. If this read as an edge, the kiosk would
    /// rebuild its stream every poll — every 2 s — forever.
    #[test]
    fn reordering_the_same_membership_is_not_a_topology_change() {
        let saved = Some("LG TV (HDMI)");
        let one_order = names(&["Built-in", "LG TV (HDMI)", "Headphones"]);
        let other_order = names(&["LG TV (HDMI)", "Headphones", "Built-in"]);
        assert!(!saved_device_reappeared(
            saved,
            &one_order,
            &other_order,
            Some("Built-in")
        ));
        assert!(!saved_device_reappeared(
            saved,
            &other_order,
            &one_order,
            Some("Built-in")
        ));
    }

    /// Nothing is bound yet (the engine failed its first build, or has not built
    /// at all). A reappearance is still an edge worth acting on — otherwise a
    /// kiosk booted while its TV slept would never pick the TV up.
    #[test]
    fn an_unbound_stream_still_sees_the_reappearance() {
        let saved = Some("LG TV (HDMI)");
        let without = names(&["Built-in"]);
        let with = names(&["Built-in", "LG TV (HDMI)"]);
        assert!(saved_device_reappeared(saved, &without, &with, None));
    }

    /// Boot with no endpoints at all, then the TV finishes waking: previous is
    /// empty, current has the device. That is the first edge of the session.
    #[test]
    fn appearing_out_of_an_empty_list_is_an_edge() {
        let saved = Some("LG TV (HDMI)");
        let empty: Vec<String> = Vec::new();
        let with = names(&["LG TV (HDMI)"]);
        assert!(saved_device_reappeared(saved, &empty, &with, None));
        // And the reverse (everything vanished) is not.
        assert!(!saved_device_reappeared(saved, &with, &empty, None));
    }

    /// The zombie-stream case, which is the *only* thing standing between a
    /// sleeping TV and a silent night when cpal declines to raise a
    /// `StreamError`: our endpoint is simply not in the list any more.
    #[test]
    fn a_bound_device_missing_from_a_running_streams_snapshot_is_a_death() {
        let with = names(&["Built-in", "LG TV (HDMI)"]);
        let without = names(&["Built-in"]);
        let empty: Vec<String> = Vec::new();

        assert!(bound_device_disappeared(
            Some("LG TV (HDMI)"),
            &without,
            true
        ));
        // The TV was the kiosk's only endpoint, so its sleeping empties the list.
        assert!(bound_device_disappeared(Some("LG TV (HDMI)"), &empty, true));
        // Still there: nothing happened.
        assert!(!bound_device_disappeared(Some("LG TV (HDMI)"), &with, true));
    }

    /// The three cases it must **not** fire on, each of which would cost a
    /// spurious cpal teardown-and-reopen (or a reconnect loop).
    #[test]
    fn a_vanished_device_does_not_fire_when_the_stream_is_already_down_or_nameless() {
        let without = names(&["Built-in"]);

        // Already reconnecting: the error callback got there first. Two triggers,
        // one cycle.
        assert!(!bound_device_disappeared(
            Some("LG TV (HDMI)"),
            &without,
            false
        ));
        // Nothing is bound (no stream ever came up, or the binding was already
        // cleared by a previous poll): there is nothing to lose.
        assert!(!bound_device_disappeared(None, &without, true));
        // A device that cannot report its own name binds as `None` precisely so it
        // does not read as permanently missing from snapshots it can never be in.
        assert!(!bound_device_disappeared(None, &Vec::new(), true));
    }

    /// The third migrate trigger: the host's default endpoint changed identity
    /// while the operator follows the system default. The case none of the other
    /// checks can see — no list change, no stream error.
    mod default_switch {
        use super::*;

        /// The headline case: no pinned device, the event PA is plugged in and
        /// the OS promotes it. The old endpoint is still present and the stream
        /// is still "healthy" — only this check notices.
        #[test]
        fn a_default_switch_with_no_pinned_device_fires() {
            assert!(default_device_switched(
                None,
                Some("Built-in"),
                Some("Built-in"),
                Some("Event PA"),
            ));
        }

        /// A pinned device makes the OS default irrelevant: the operator chose an
        /// endpoint, and a default switch must never yank the stream off it.
        #[test]
        fn a_pinned_device_ignores_default_switches() {
            assert!(!default_device_switched(
                Some("LG TV (HDMI)"),
                Some("LG TV (HDMI)"),
                Some("Built-in"),
                Some("Event PA"),
            ));
            // Even when the stream is on a fallback because the pinned device is
            // away: the operator's choice is the TV, not "whatever is default".
            assert!(!default_device_switched(
                Some("LG TV (HDMI)"),
                Some("Built-in"),
                Some("Built-in"),
                Some("Event PA"),
            ));
        }

        /// The empty saved string is the "follow the system default" sentinel,
        /// exactly as in the resolver — it must behave like no preference.
        #[test]
        fn an_empty_saved_name_follows_the_default() {
            assert!(default_device_switched(
                Some(""),
                Some("Built-in"),
                Some("Built-in"),
                Some("Event PA"),
            ));
        }

        /// Already bound to the new default (the state every fallback rebuild
        /// lands in): nothing to migrate. This is the steady state after the
        /// migration succeeds, so it must not re-fire.
        #[test]
        fn already_on_the_new_default_does_not_fire() {
            assert!(!default_device_switched(
                None,
                Some("Event PA"),
                Some("Built-in"),
                Some("Event PA"),
            ));
        }

        /// No live binding: either the engine never came up or a death edge just
        /// cleared it — a reconnect cycle is armed either way, and its rebuild
        /// resolves to the current default without help from this trigger.
        #[test]
        fn no_binding_means_nothing_to_migrate_from() {
            assert!(!default_device_switched(
                None,
                None,
                Some("Built-in"),
                Some("Event PA"),
            ));
        }

        /// No current default (deviceless host, or a nameless default endpoint):
        /// there is nothing to follow.
        #[test]
        fn no_current_default_never_fires() {
            assert!(!default_device_switched(
                None,
                Some("Built-in"),
                Some("Built-in"),
                None,
            ));
        }

        /// Snapshots are also published for list-only changes. A stable default
        /// must not re-fire on those — only the rising edge of the default's
        /// *identity* counts, or every headphone unplug would rebuild the stream.
        #[test]
        fn an_unchanged_default_is_not_an_edge() {
            assert!(!default_device_switched(
                None,
                Some("Built-in"),
                Some("Event PA"),
                Some("Event PA"),
            ));
        }

        /// The first snapshot of the session (`previous == None`) is an edge when
        /// the binding disagrees with the default — the startup build raced a
        /// default change, and the stream should follow.
        #[test]
        fn a_first_snapshot_disagreeing_with_the_binding_fires() {
            assert!(default_device_switched(
                None,
                Some("Built-in"),
                None,
                Some("Event PA"),
            ));
        }
    }

    /// The watcher, its channel, and the drain system are native-only (cpal
    /// enumeration is), so the pure cores they are built out of are too.
    #[cfg(not(target_arch = "wasm32"))]
    mod topology {
        use super::*;

        /// An [`OutputTopology`] from literals, so each test reads as data.
        fn topo(list: &[&str], default: Option<&str>) -> OutputTopology {
            OutputTopology {
                outputs: names(list),
                default_output: default.map(str::to_owned),
            }
        }

        #[test]
        fn apply_topology_updates_the_list_and_flags_reappearance() {
            let mut available = names(&["Built-in"]);
            // The saved HDMI TV reappears while we are on the fallback.
            let migrate = apply_topology(
                &mut available,
                names(&["Built-in", "LG TV (HDMI)"]),
                Some("LG TV (HDMI)"),
                Some("Built-in"),
            );
            assert!(migrate, "saved endpoint reappeared -> migrate back");
            assert_eq!(available, names(&["Built-in", "LG TV (HDMI)"]));
        }

        #[test]
        fn apply_topology_no_migrate_when_nothing_relevant_changed() {
            let mut available = names(&["Built-in", "LG TV (HDMI)"]);
            // Same list, already bound to the saved device: no migrate.
            let migrate = apply_topology(
                &mut available,
                names(&["Built-in", "LG TV (HDMI)"]),
                Some("LG TV (HDMI)"),
                Some("LG TV (HDMI)"),
            );
            assert!(!migrate);
        }

        /// The list still has to be replaced when there is nothing to migrate to —
        /// the settings dropdown reads `AvailableAudioDevices`, so a snapshot whose
        /// only change is "the headphones went away" must still land, even though it
        /// is not a migrate-back edge.
        #[test]
        fn apply_topology_swaps_the_list_even_when_no_migrate_is_warranted() {
            let mut available = names(&["Built-in", "Headphones"]);
            let migrate =
                apply_topology(&mut available, names(&["Built-in"]), None, Some("Built-in"));
            assert!(!migrate);
            assert_eq!(available, names(&["Built-in"]));
        }

        /// The recovery-only stage (Tasks 4–5) passes `saved: None` because the
        /// persisted setting is not wired in until Task 6. Migrate-back is therefore
        /// inert, but the drain must still be harmless: no edge, no thrash, list
        /// updated.
        #[test]
        fn apply_topology_with_no_saved_name_never_migrates() {
            let mut available = names(&["Built-in"]);
            let migrate = apply_topology(
                &mut available,
                names(&["Built-in", "LG TV (HDMI)"]),
                None,
                Some("Built-in"),
            );
            assert!(!migrate);
            assert_eq!(available, names(&["Built-in", "LG TV (HDMI)"]));
        }

        /// The watcher only sends on a real change, but a duplicate snapshot must be
        /// idempotent anyway (two drains of the same list, or a re-send after a
        /// reconnect): the second application sees the device already in `previous`,
        /// so it is not a rising edge and the stream is not rebuilt twice.
        #[test]
        fn applying_the_same_snapshot_twice_fires_at_most_one_migrate() {
            let mut available = names(&["Built-in"]);
            let saved = Some("LG TV (HDMI)");
            let bound = Some("Built-in");
            let with = names(&["Built-in", "LG TV (HDMI)"]);

            assert!(apply_topology(&mut available, with.clone(), saved, bound));
            assert!(!apply_topology(&mut available, with, saved, bound));
        }

        /// The first poll of the session has no previous snapshot, so it always
        /// publishes — otherwise the dropdown would be empty until something changed.
        #[test]
        fn the_first_poll_always_publishes() {
            let first = topo(&["Built-in"], Some("Built-in"));
            assert_eq!(
                topology_snapshot_to_publish(None, Some(first.clone())),
                Some(first),
            );
            // Even when the host genuinely has nothing: an empty answer is an answer.
            let empty = topo(&[], None);
            assert_eq!(
                topology_snapshot_to_publish(None, Some(empty.clone())),
                Some(empty),
            );
        }

        /// Steady state: the same list and default, poll after poll, for hours.
        /// Nothing is published, so the channel stays silent and the main thread
        /// does no work.
        #[test]
        fn an_unchanged_topology_publishes_nothing() {
            let last = topo(&["Built-in", "LG TV (HDMI)"], Some("LG TV (HDMI)"));
            assert_eq!(
                topology_snapshot_to_publish(Some(&last), Some(last.clone())),
                None,
            );
        }

        /// A real change — the TV woke up — is published.
        #[test]
        fn a_changed_list_is_published() {
            let last = topo(&["Built-in"], Some("Built-in"));
            let with = topo(&["Built-in", "LG TV (HDMI)"], Some("Built-in"));
            assert_eq!(
                topology_snapshot_to_publish(Some(&last), Some(with.clone())),
                Some(with),
            );
            // And so is the reverse: the TV sleeping empties the list, which the
            // dropdown must reflect.
            let gone = topo(&[], None);
            assert_eq!(
                topology_snapshot_to_publish(Some(&last), Some(gone.clone())),
                Some(gone),
            );
        }

        /// A default-only change — same membership, the OS promoted a different
        /// endpoint — is a real topology change too. It is the entire signal
        /// behind `default_device_switched`; swallowing it here would blind the
        /// follow-the-default migration.
        #[test]
        fn a_default_switch_alone_is_published() {
            let last = topo(&["Built-in", "Event PA"], Some("Built-in"));
            let switched = topo(&["Built-in", "Event PA"], Some("Event PA"));
            assert_eq!(
                topology_snapshot_to_publish(Some(&last), Some(switched.clone())),
                Some(switched),
            );
        }

        /// The defect this function exists to prevent. A *failed* enumeration
        /// (`None`) is not "every endpoint vanished". If it were folded into an empty
        /// list, the next successful poll would read as a rising edge and rebuild the
        /// stream for no reason — every time the host hiccups, forever.
        #[test]
        fn a_failed_enumeration_skips_the_tick_and_keeps_the_previous_snapshot() {
            let last = topo(&["Built-in", "LG TV (HDMI)"], Some("LG TV (HDMI)"));
            assert_eq!(topology_snapshot_to_publish(Some(&last), None), None);
            // The caller therefore still holds `last`, so the next successful poll of
            // the same topology is (correctly) not a change either.
            assert_eq!(
                topology_snapshot_to_publish(Some(&last), Some(last.clone())),
                None,
            );
        }

        /// The whole of Fix 2, end to end through the real system: the TV that the
        /// live stream is bound to stops being enumerated, and cpal says **nothing**
        /// (no error flag is set anywhere in this test — there is no stream at all).
        ///
        /// Before this, `AudioState` sat at `Running` with `bound = "LG TV (HDMI)"`
        /// forever: `supervise_audio` saw a `Running` status and did nothing, and
        /// `saved_device_reappeared` short-circuited on `currently_bound == saved`
        /// when the TV came back, so neither safety net ever armed. Silent for the
        /// night.
        /// A headless app carrying exactly the resources `drain_device_topology`
        /// takes, plus a channel standing in for the watcher thread.
        fn drain_test_app() -> (bevy::prelude::App, std::sync::mpsc::Sender<OutputTopology>) {
            use bevy::prelude::*;

            let mut app = App::new();
            app.add_plugins(MinimalPlugins);
            app.init_resource::<AvailableAudioDevices>();
            app.init_resource::<BoundOutputDevice>();
            app.init_resource::<DefaultOutputDevice>();
            app.init_resource::<crate::audio::state::AudioState>();
            app.init_resource::<crate::audio::supervisor::AudioSupervisor>();
            app.add_systems(PreUpdate, drain_device_topology);

            // Stand in for the watcher thread: the same channel it would send on.
            let (tx, rx) = std::sync::mpsc::channel::<OutputTopology>();
            app.insert_non_send(DeviceTopologyReceiver { rx });
            (app, tx)
        }

        #[test]
        fn a_vanished_bound_device_drives_reconnecting_and_clears_the_binding() {
            use crate::audio::state::{AudioState, AudioStatus};
            use bevy::prelude::*;

            let (mut app, tx) = drain_test_app();

            // A healthy stream, bound to the TV, which is in the current list.
            app.world_mut().resource_mut::<AudioState>().status = AudioStatus::Running;
            app.world_mut().resource_mut::<BoundOutputDevice>().0 = Some("LG TV (HDMI)".to_owned());
            app.world_mut().resource_mut::<AvailableAudioDevices>().0 =
                names(&["Built-in", "LG TV (HDMI)"]);

            // 2 a.m.: the TV sleeps. cpal raises nothing.
            assert!(tx.send(topo(&["Built-in"], Some("Built-in"))).is_ok());
            app.update();

            assert_eq!(
                app.world().resource::<AudioState>().status,
                AudioStatus::Reconnecting,
                "the watcher is the only witness to this outage; it must act on it",
            );
            assert!(
                app.world().resource::<BoundOutputDevice>().0.is_none(),
                "the stale binding is what disarmed the migrate-back; clear it",
            );
            assert_eq!(
                app.world().resource::<AvailableAudioDevices>().0,
                names(&["Built-in"]),
                "and the list still lands, for the settings dropdown",
            );
            assert_eq!(
                app.world().resource::<DefaultOutputDevice>().0.as_deref(),
                Some("Built-in"),
                "the default bookkeeping lands too",
            );

            // A second snapshot with the device still gone changes nothing: the
            // status is no longer `Running`, so the trigger does not re-fire.
            assert!(tx
                .send(topo(&["Built-in", "Headphones"], Some("Built-in")))
                .is_ok());
            app.update();
            assert_eq!(
                app.world().resource::<AudioState>().status,
                AudioStatus::Reconnecting,
            );
        }

        /// Edge 3 end to end through the real system: the operator has no pinned
        /// device (there is no `AudioSettings` resource at all here, the strongest
        /// form of "no preference"), the event PA is plugged in, and the OS
        /// promotes it. No membership the stream cares about changed, no error
        /// fired — only the default's identity moved. The supervisor must be asked
        /// for an immediate rebuild, and the stream itself is left alone (the
        /// rebuild is the supervisor's job, on the main thread, next `Update`).
        #[test]
        fn a_default_switch_with_no_pinned_device_arms_an_immediate_rebuild() {
            use crate::audio::state::{AudioState, AudioStatus};
            use crate::audio::supervisor::AudioSupervisor;

            let (mut app, tx) = drain_test_app();
            app.world_mut().resource_mut::<AudioState>().status = AudioStatus::Running;
            app.world_mut().resource_mut::<BoundOutputDevice>().0 = Some("Built-in".to_owned());

            // Baseline snapshot: bound to the default, nothing to do.
            assert!(tx.send(topo(&["Built-in"], Some("Built-in"))).is_ok());
            app.update();
            assert!(
                !app.world().resource::<AudioSupervisor>().is_reconnecting(),
                "bound == default: no migration armed",
            );

            // The event PA arrives and becomes the default. The old endpoint stays.
            assert!(tx
                .send(topo(&["Built-in", "Event PA"], Some("Event PA")))
                .is_ok());
            app.update();

            assert!(
                app.world().resource::<AudioSupervisor>().is_reconnecting(),
                "a default switch while following the default must arm a rebuild",
            );
            assert_eq!(
                app.world().resource::<AudioState>().status,
                AudioStatus::Running,
                "this is a migration, not a death: the stream is healthy until \
                 the supervisor swaps it",
            );
            assert_eq!(
                app.world().resource::<BoundOutputDevice>().0.as_deref(),
                Some("Built-in"),
                "the binding is rewritten by the rebuild, never by the drain",
            );
            assert_eq!(
                app.world().resource::<DefaultOutputDevice>().0.as_deref(),
                Some("Event PA"),
            );

            // The same snapshot content again (e.g. a later list-only change with
            // the default stable) is not an edge: no re-arm after the cycle would
            // have been spent. Clear the armed cycle to observe that directly.
            app.world_mut()
                .resource_mut::<AudioSupervisor>()
                .record_success(0.0);
            assert!(tx
                .send(topo(
                    &["Built-in", "Event PA", "Headphones"],
                    Some("Event PA"),
                ))
                .is_ok());
            app.update();
            assert!(
                !app.world().resource::<AudioSupervisor>().is_reconnecting(),
                "a stable default is not an edge, whatever else the list does",
            );
        }

        /// The negative: an operator who pinned a device keeps it across any
        /// number of default switches. Their explicit choice outranks the OS.
        #[test]
        fn a_default_switch_with_a_pinned_device_is_ignored() {
            use crate::audio::settings::AudioSettings;
            use crate::audio::state::{AudioState, AudioStatus};
            use crate::audio::supervisor::AudioSupervisor;

            let (mut app, tx) = drain_test_app();
            app.insert_resource(AudioSettings {
                output_device: "Built-in".to_owned(),
            });
            app.world_mut().resource_mut::<AudioState>().status = AudioStatus::Running;
            app.world_mut().resource_mut::<BoundOutputDevice>().0 = Some("Built-in".to_owned());
            app.world_mut().resource_mut::<AvailableAudioDevices>().0 = names(&["Built-in"]);
            app.world_mut().resource_mut::<DefaultOutputDevice>().0 = Some("Built-in".to_owned());

            assert!(tx
                .send(topo(&["Built-in", "Event PA"], Some("Event PA")))
                .is_ok());
            app.update();

            assert!(
                !app.world().resource::<AudioSupervisor>().is_reconnecting(),
                "a pinned device must not follow the OS default anywhere",
            );
            assert_eq!(
                app.world().resource::<AudioState>().status,
                AudioStatus::Running,
            );
        }

        /// The steady state this must not disturb: the bound device is present in
        /// every snapshot, so a topology change that only adds or removes *other*
        /// endpoints leaves a healthy stream completely alone.
        #[test]
        fn a_topology_change_that_keeps_the_bound_device_leaves_a_running_stream_alone() {
            use crate::audio::state::{AudioState, AudioStatus};
            use crate::audio::supervisor::AudioSupervisor;

            let (mut app, tx) = drain_test_app();

            app.world_mut().resource_mut::<AudioState>().status = AudioStatus::Running;
            app.world_mut().resource_mut::<BoundOutputDevice>().0 = Some("LG TV (HDMI)".to_owned());
            app.world_mut().resource_mut::<DefaultOutputDevice>().0 =
                Some("LG TV (HDMI)".to_owned());

            // Someone plugs in headphones. Our endpoint — also the default — is
            // untouched.
            assert!(tx
                .send(topo(
                    &["Built-in", "Headphones", "LG TV (HDMI)"],
                    Some("LG TV (HDMI)"),
                ))
                .is_ok());
            app.update();

            assert_eq!(
                app.world().resource::<AudioState>().status,
                AudioStatus::Running,
            );
            assert_eq!(
                app.world().resource::<BoundOutputDevice>().0.as_deref(),
                Some("LG TV (HDMI)"),
            );
            assert!(
                !app.world().resource::<AudioSupervisor>().is_reconnecting(),
                "no cycle armed on a healthy stream",
            );
        }

        /// The sort itself, pinned where it actually lives.
        ///
        /// CI has no cpal host, so [`enumerate_output_names`] cannot be called
        /// here — which is exactly why the sort was lifted out of it into
        /// [`canonical_output_names`], whose input is a plain iterator of names.
        /// Delete the `sort_unstable` there and this test fails, which is what the
        /// old version of this test only *claimed* to do: it sorted its own inputs
        /// before handing them to the differ, so it passed with the production sort
        /// removed.
        #[test]
        fn canonicalisation_sorts_whatever_order_the_host_reports() {
            let reported = names(&["LG TV (HDMI)", "Built-in", "Headphones"]);
            assert_eq!(
                canonical_output_names(reported.into_iter()),
                names(&["Built-in", "Headphones", "LG TV (HDMI)"]),
            );
            // Duplicates survive: two identical TVs are two endpoints, and the
            // settings dropdown must not hide one of them.
            let dupes = names(&["LG TV (HDMI)", "Built-in", "LG TV (HDMI)"]);
            assert_eq!(
                canonical_output_names(dupes.into_iter()),
                names(&["Built-in", "LG TV (HDMI)", "LG TV (HDMI)"]),
            );
        }

        /// …and the reason the sort matters: the watcher's diff is exact equality,
        /// so two orderings of the same membership must be *canonicalised into the
        /// same snapshot* before they reach the differ. If they were not, the kiosk
        /// would republish (and, with a saved name, rebuild) every 2 s forever.
        ///
        /// The canonicalisation here is the production one — the same call
        /// `try_enumerate_output_names` makes — not a hand-sort in the test.
        #[test]
        fn two_host_orderings_canonicalise_to_the_same_snapshot_and_do_not_reach_the_differ() {
            let one = OutputTopology {
                outputs: canonical_output_names(
                    names(&["LG TV (HDMI)", "Built-in", "Headphones"]).into_iter(),
                ),
                default_output: Some("Built-in".to_owned()),
            };
            let other = OutputTopology {
                outputs: canonical_output_names(
                    names(&["Headphones", "LG TV (HDMI)", "Built-in"]).into_iter(),
                ),
                default_output: Some("Built-in".to_owned()),
            };
            assert_eq!(topology_snapshot_to_publish(Some(&one), Some(other)), None);
        }
    }
}
