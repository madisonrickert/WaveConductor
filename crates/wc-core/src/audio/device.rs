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
//! [`resolve_output_device`] and [`saved_device_reappeared`] are **pure** — no
//! host, no device, no thread — and carry the two decisions this half turns on,
//! so they are unit-tested with literal name lists (CI has no audio device).
//! Neither allocates beyond the single owned name it returns, and neither runs
//! per frame: the resolver runs on (re)build, the diff runs on the watcher's
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

/// Live list of output-device names, refreshed by the device-watcher thread
/// (Task 4). Read by the audio settings panel (Task 7, via Plan 03a's
/// runtime-enumerated dropdown) and by the supervisor's migrate-back check.
///
/// Main-thread-only resource; the watcher thread never touches it directly (it
/// sends snapshots over a channel that a main-thread system drains into here).
#[derive(Resource, Default, Debug, Clone)]
pub struct AvailableAudioDevices(pub Vec<String>);

/// Name of the output device the live stream is currently bound to, or `None`
/// before the engine starts / when it failed to build.
///
/// Set on every successful (re)build (Task 5). The migrate-back check compares
/// against this so it does not rebuild a stream that is already on the saved
/// device.
#[derive(Resource, Default, Debug, Clone)]
pub struct BoundOutputDevice(pub Option<String>);

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

/// The ~2 s cadence at which the watcher re-enumerates output devices.
#[cfg(not(target_arch = "wasm32"))]
const WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Granularity at which the watcher wakes to check its stop flag, so app
/// shutdown joins the thread promptly instead of waiting out a full interval.
#[cfg(not(target_arch = "wasm32"))]
const WATCH_TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// Decide what one watcher poll should publish, given the previous snapshot and
/// the result of [`enumerate_output_names`].
///
/// This is the whole decision the watcher thread makes, extracted so it is
/// testable without a device, a thread, or a two-second sleep. The thread around
/// it is a dumb loop.
///
/// - `polled == None` — the host enumeration **errored**; we learned nothing.
///   The tick is skipped: no diff, no publish, and the caller keeps its previous
///   snapshot. Folding this into "the list is now empty" would make the next
///   *successful* poll look like a rising edge and provoke a spurious rebuild.
/// - `polled == Some(list)` equal to `last` — steady state (the overwhelmingly
///   common case). Nothing to publish, so the channel stays quiet.
/// - `polled == Some(list)` differing from `last` (including the very first poll,
///   where `last` is `None`) — a real topology change. Return it; the caller
///   publishes it and adopts it as the new `last`.
///
/// `Some(vec![])` is a legitimate answer, not an error: when the only endpoint is
/// a sleeping HDMI TV the host genuinely enumerates nothing. Equality is exact,
/// which is sound precisely because [`enumerate_output_names`] sorts — a host that
/// re-orders its list between polls is not a topology change.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub(crate) fn topology_snapshot_to_publish(
    last: Option<&[String]>,
    polled: Option<Vec<String>>,
) -> Option<Vec<String>> {
    let current = polled?;
    if last == Some(current.as_slice()) {
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
    /// Receives a fresh name snapshot only when the list actually changed.
    rx: std::sync::mpsc::Receiver<Vec<String>>,
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
    fn latest(&self) -> Option<Vec<String>> {
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
    let (tx, rx) = std::sync::mpsc::channel::<Vec<String>>();

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
fn watch_devices(stop: &std::sync::atomic::AtomicBool, tx: &std::sync::mpsc::Sender<Vec<String>>) {
    use std::sync::atomic::Ordering;

    let host = cpal::default_host();
    // Reused across the session; the first poll (`last == None`) always
    // publishes, so the dropdown and the resolver see a list without
    // waiting out an interval.
    let mut last: Option<Vec<String>> = None;
    // Whether the *previous* poll's enumeration failed. The one piece of state
    // that turns a per-poll warning into a per-outage one.
    let mut failing = false;

    while !stop.load(Ordering::Relaxed) {
        let polled = match try_enumerate_output_names(&host) {
            Ok(names) => {
                if failing {
                    tracing::info!("cpal output-device enumeration recovered");
                    failing = false;
                }
                Some(names)
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

        if let Some(current) = topology_snapshot_to_publish(last.as_deref(), polled) {
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
/// update [`AvailableAudioDevices`], and ask the supervisor for an immediate
/// rebuild when the saved endpoint has just reappeared.
///
/// Runs on the **main thread** ([`DeviceTopologyReceiver`] is non-send) and every
/// frame — so it is a hot path. It never enumerates (the blocking enumeration
/// already happened on the watcher thread) and it never allocates in steady state:
/// the common case is an empty channel, `DeviceTopologyReceiver::latest` returns
/// `None`, and the system returns. When a snapshot *has* arrived it only moves an
/// already-built `Vec<String>` into the resource.
///
/// `saved` is the operator's persisted
/// [`crate::audio::settings::AudioSettings::output_device`], read (never written)
/// as a `&str`: the migrate-back edge is "the name the operator chose has just
/// reappeared". `Option<Res<…>>` degrades cleanly if the settings resource is
/// somehow absent (a harness that loads `AudioPlugin`'s systems without its
/// settings registration); in the app it is inserted at plugin build.
#[cfg(not(target_arch = "wasm32"))]
pub fn drain_device_topology(
    receiver: Option<bevy::ecs::system::NonSend<'_, DeviceTopologyReceiver>>,
    mut available: ResMut<'_, AvailableAudioDevices>,
    bound: Res<'_, BoundOutputDevice>,
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
    // Borrowed, not cloned: this system runs every frame, and the empty string is
    // the "no preference" sentinel `resolve_output_device`/`saved_device_reappeared`
    // already understand.
    let saved: Option<&str> = settings
        .as_ref()
        .map(|s| s.output_device.as_str())
        .filter(|name| !name.is_empty());
    if apply_topology(&mut available.0, incoming, saved, bound.0.as_deref()) {
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

    /// The watcher, its channel, and the drain system are native-only (cpal
    /// enumeration is), so the pure cores they are built out of are too.
    #[cfg(not(target_arch = "wasm32"))]
    mod topology {
        use super::*;

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
            let first = names(&["Built-in"]);
            assert_eq!(
                topology_snapshot_to_publish(None, Some(first.clone())),
                Some(first),
            );
            // Even when the host genuinely has nothing: `Some(vec![])` is an answer.
            assert_eq!(
                topology_snapshot_to_publish(None, Some(Vec::new())),
                Some(Vec::new()),
            );
        }

        /// Steady state: the same list, poll after poll, for hours. Nothing is
        /// published, so the channel stays silent and the main thread does no work.
        #[test]
        fn an_unchanged_list_publishes_nothing() {
            let last = names(&["Built-in", "LG TV (HDMI)"]);
            assert_eq!(
                topology_snapshot_to_publish(Some(&last), Some(last.clone())),
                None,
            );
        }

        /// A real change — the TV woke up — is published.
        #[test]
        fn a_changed_list_is_published() {
            let last = names(&["Built-in"]);
            let with = names(&["Built-in", "LG TV (HDMI)"]);
            assert_eq!(
                topology_snapshot_to_publish(Some(&last), Some(with.clone())),
                Some(with),
            );
            // And so is the reverse: the TV sleeping empties the list, which the
            // dropdown must reflect.
            let gone: Vec<String> = Vec::new();
            assert_eq!(
                topology_snapshot_to_publish(Some(&last), Some(gone.clone())),
                Some(gone),
            );
        }

        /// The defect this function exists to prevent. A *failed* enumeration
        /// (`None`) is not "every endpoint vanished". If it were folded into an empty
        /// list, the next successful poll would read as a rising edge and rebuild the
        /// stream for no reason — every time the host hiccups, forever.
        #[test]
        fn a_failed_enumeration_skips_the_tick_and_keeps_the_previous_snapshot() {
            let last = names(&["Built-in", "LG TV (HDMI)"]);
            assert_eq!(topology_snapshot_to_publish(Some(&last), None), None);
            // The caller therefore still holds `last`, so the next successful poll of
            // the same list is (correctly) not a change either.
            assert_eq!(
                topology_snapshot_to_publish(Some(&last), Some(last.clone())),
                None,
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
            let one = canonical_output_names(
                names(&["LG TV (HDMI)", "Built-in", "Headphones"]).into_iter(),
            );
            let other = canonical_output_names(
                names(&["Headphones", "LG TV (HDMI)", "Built-in"]).into_iter(),
            );
            assert_eq!(topology_snapshot_to_publish(Some(&one), Some(other)), None);
        }
    }
}
