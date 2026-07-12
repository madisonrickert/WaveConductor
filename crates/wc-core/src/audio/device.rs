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
//! enumeration in particular). It is therefore only ever called from (a) the
//! one-shot startup path and event-driven rebuilds on the **main thread**, and
//! (b) the device-watcher OS thread added in Task 4 — never the audio callback
//! and never a per-frame render system. On WASAPI, cpal initialises COM
//! per-thread internally (`com::com_initialized()` runs at the top of every
//! device operation), so calling this from a freshly spawned watcher thread is
//! sound without any manual `CoInitializeEx`.

use bevy::prelude::Resource;

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
/// read is skipped. Returns an empty vec if enumeration itself errors, which the
/// resolver treats as "nothing available -> fall back to default". An empty list
/// is a real, expected state, not a bug: when the only endpoint is a sleeping
/// TV, the host enumerates nothing.
///
/// The result is **sorted** and *not* de-duplicated. Sorting makes the snapshot
/// canonical, so a host that re-orders its enumeration between polls does not
/// look like a topology change to the watcher, and the settings dropdown has a
/// stable order. De-duplicating would silently hide one of two identically-named
/// endpoints from that dropdown, so duplicates are kept as reported.
///
/// Allocates a `Vec<String>` (cpal returns owned names); this is forced by
/// cpal's API and is acceptable because it runs at most every ~2 s on a
/// background thread, never on the audio callback or a per-frame render system.
#[cfg(not(target_arch = "wasm32"))]
pub fn enumerate_output_names(host: &cpal::Host) -> Vec<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    match host.output_devices() {
        Ok(devices) => {
            let mut names: Vec<String> = devices.filter_map(|d| d.name().ok()).collect();
            names.sort_unstable();
            names
        }
        Err(err) => {
            tracing::warn!(?err, "cpal output_devices enumeration failed");
            Vec::new()
        }
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
}
