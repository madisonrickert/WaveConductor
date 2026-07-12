//! Core (not per-sketch) display settings: startup fullscreen, cursor
//! visibility, and monitor selection.
//!
//! ## Why this file lives under `settings/panel_user/`
//!
//! Every other file in this directory is shared panel-*rendering*
//! infrastructure (`dock`, `fields`, `widgets`, `provider_status`), not a
//! settings struct's home — those normally live directly under `settings/`
//! (`hand_tracking.rs`) or beside the domain they configure
//! (`lifecycle/screensaver/settings.rs`). This placement is a locked design
//! decision (`docs/superpowers/specs/2026-07-08-windows-remediation-design.md`,
//! Workstream 4), not this plan's choice, kept here as written rather than
//! relitigated. The *systems* that apply these settings — where the domain
//! logic actually lives — are in `crate::lifecycle::display`, matching the
//! rest of `lifecycle/`.
//!
//! ## `monitor: String`, not `Option<String>`
//!
//! The design doc and the alpha.5 program index describe `monitor:
//! Option<String>` with a fallback to `MonitorSelection::Current`. Plan
//! 03a's widget contract (`docs/superpowers/plans/2026-07-09-alpha5-program-index.md`,
//! Plan 03a entry) stores a plain `String`, matching every other `Text`-kind
//! setting (no persistence-format change). This plan reconciles the two:
//! `monitor: String`, where an **empty string** is the `None` case and always
//! resolves to [`MonitorSelection::Current`] — see [`resolve_monitor_selection`].
//! The persisted semantics (`Some(name)` / `None` / unresolvable-name
//! fallback) are unchanged; only the Rust type is.

use bevy::prelude::*;
use bevy::window::{MonitorSelection, WindowMode};
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

use crate::settings::RuntimeEnumOptionsSource;

/// Startup fullscreen, cursor visibility, and monitor selection.
///
/// Applied by `crate::lifecycle::display::apply_display_mode`, which runs
/// once at `Startup` and unconditionally every `Update` frame thereafter —
/// see that module's doc for why "every frame" is the mechanism that stands
/// in for a `MonitorAdded` / `MonitorRemoved` message Bevy 0.19 does not have.
///
/// This is the *persisted* configuration. The F11 keybind
/// (`lifecycle::nav::handle_navigation_actions`) does **not** write it — it
/// writes the session-only [`FullscreenOverride`] instead, so a stray keypress
/// at the installation cannot outlive a power cycle. The settings panel's
/// "Start fullscreen" checkbox does write this field, and does persist: that is
/// a deliberate operator choice, not a stray keypress.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "display")]
pub(crate) struct DisplaySettings {
    /// Whether the app claims the whole target monitor at startup.
    ///
    /// Default `true` in release (a kiosk build with no attached keyboard has
    /// no other way to reach fullscreen), `false` under `debug_assertions` so
    /// `cargo rund` does not swallow the dev window on every relaunch. See
    /// [`default_start_fullscreen_for`] for the testable branch logic. (The two
    /// profiles persist to separate files — see
    /// `crate::settings::persistence::SETTINGS_FILE_NAME` — so a debug run
    /// writing `false` here cannot poison the release kiosk's saved value.)
    ///
    /// The persisted intent only. What is actually on screen is
    /// [`FullscreenOverride::effective_fullscreen`], which lets F11 override this
    /// for the session without writing it to disk.
    #[setting(
        default = default_start_fullscreen(),
        ty = Boolean,
        section = "Display",
        category = User,
        label = "Start fullscreen"
    )]
    #[serde(default = "default_start_fullscreen")]
    pub start_fullscreen: bool,

    /// Whether to hide the OS mouse cursor over the window.
    ///
    /// Default `true` in release (kiosk-first: an unattended install has no
    /// reason to show a cursor), `false` under `debug_assertions` for the same
    /// reason `start_fullscreen` is gated — the "Hide cursor" toggle lives in a
    /// *mouse-driven* settings panel, so a dev build that hid the pointer by
    /// default would leave hand-editing the persisted config as the only way
    /// back. See [`default_hide_cursor_for`] for the testable branch logic.
    #[setting(
        default = default_hide_cursor(),
        ty = Boolean,
        section = "Display",
        category = User,
        label = "Hide cursor"
    )]
    #[serde(default = "default_hide_cursor")]
    pub hide_cursor: bool,

    /// Monitor to target when `start_fullscreen` is true, persisted by
    /// **name** (not by winit's per-boot enumeration index, which is not
    /// stable across reboots or monitor topology changes). Empty string means
    /// "no preference" and resolves to [`MonitorSelection::Current`] via
    /// [`resolve_monitor_selection`].
    ///
    /// Rendered by Plan 03a's runtime-enum widget as a dropdown populated from
    /// the [`AvailableMonitors`] source registered under `"monitors"`. A saved
    /// name that no longer resolves (an unplugged or sleeping monitor) is not
    /// dropped: 03a's widget shows it, marks it unavailable, and keeps it
    /// persisted, matching [`resolve_monitor_selection`]'s
    /// fall-back-without-rewrite behaviour. The stored value (a plain `String`)
    /// and its resolution semantics are unchanged from the `ty = Text` version;
    /// only the widget differs.
    #[setting(
        default = String::new(),
        ty = RuntimeEnum,
        options_key = "monitors",
        section = "Display",
        category = User,
        label = "Monitor"
    )]
    #[serde(default)]
    pub monitor: String,
}

/// Session-only fullscreen override: what F11 writes instead of
/// [`DisplaySettings::start_fullscreen`].
///
/// `None` (the default) means "no override — use the persisted
/// `start_fullscreen`". `Some(v)` forces the window mode to `v` for the rest of
/// this process's life.
///
/// **Never persisted.** It is a plain `Resource`, not a `SketchSettings`: it has
/// no `STORAGE_KEY`, is not registered with the settings registry, is not
/// `Reflect`, and is `None` at every boot by construction. That is the whole
/// point. This is a kiosk installation running unattended on a TV: if a
/// passer-by presses F11, the app drops to a small window on a black screen with
/// the cursor hidden, and if that flag were persisted the kiosk would still be
/// broken after a power cycle — recovery would mean SSH-ing in to hand-edit the
/// config. With the override session-only, unplugging the TV fixes it.
///
/// Read by `crate::lifecycle::display::apply_display_mode` (via
/// [`compute_display_mode`]) and cleared by
/// `crate::lifecycle::display::clear_fullscreen_override_on_settings_edit` when
/// the operator changes "Start fullscreen" in the settings panel — an explicit
/// panel edit is the authoritative choice and must take effect immediately, not
/// sit behind a stale F11 press.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FullscreenOverride(pub(crate) Option<bool>);

impl FullscreenOverride {
    /// The fullscreen state actually in effect: the session override if one is
    /// set, else the persisted [`DisplaySettings::start_fullscreen`].
    ///
    /// The single definition of "what is on screen right now", shared by
    /// [`compute_display_mode`] and the F11 handler — so F11 toggles relative to
    /// what the operator can actually see, never relative to a persisted value
    /// that an earlier override has already superseded.
    pub(crate) fn effective_fullscreen(self, settings: &DisplaySettings) -> bool {
        self.0.unwrap_or(settings.start_fullscreen)
    }
}

/// Monitor names currently reported by the OS.
///
/// Refreshed by `crate::lifecycle::display::sync_available_monitors`
/// whenever a `Monitor` ECS component is added or removed (not every frame —
/// see that system's doc for why). Registered as Plan 03a's options source for
/// the `"monitors"` key (see the [`RuntimeEnumOptionsSource`] impl below and
/// `crate::lifecycle::display::DisplayPlugin::build`), which is what populates
/// the `monitor` field's dropdown.
///
/// An empty list is a normal state, not an error: `bevy_winit` spawns its
/// `Monitor` entities on the first event-loop iteration, which can land after
/// `Startup`. 03a omits the key from its snapshot in that case and still
/// renders the persisted value.
#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub(crate) struct AvailableMonitors(pub(crate) Vec<String>);

/// Feeds the live monitor-name list to Plan 03a's runtime-enum settings
/// widget. The `monitor` field of [`DisplaySettings`] declares
/// `options_key = "monitors"` (above), and 03a's panel resolves that key
/// against every registered [`RuntimeEnumOptionsSource`] at render time — so
/// this key and the field's must stay identical. The
/// `monitor_field_options_key_matches_its_options_source` test at the footer of
/// this file pins them together; a debug-build startup check
/// (`settings::runtime_enum::warn_on_unresolved_options_keys`) catches a drift
/// at runtime.
impl RuntimeEnumOptionsSource for AvailableMonitors {
    const OPTIONS_KEY: &'static str = "monitors";

    fn options(&self) -> &[String] {
        &self.0
    }
}

/// The window mode and cursor visibility implied by a [`DisplaySettings`]
/// value, computed against a snapshot of live monitors.
///
/// A named struct rather than a `(WindowMode, bool)` tuple, per AGENTS.md's
/// preference for named fields once a type carries more than one
/// semantically meaningful value — `target.cursor_visible` reads at the call
/// site; `target.1` does not.
pub(crate) struct DisplayModeTarget {
    /// The window mode to apply (`Windowed` or `BorderlessFullscreen(_)`).
    pub(crate) mode: WindowMode,
    /// Whether the OS cursor should be visible: `!settings.hide_cursor`, but
    /// forced `true` while the settings panel is open (see
    /// [`compute_display_mode`]).
    pub(crate) cursor_visible: bool,
}

/// Pure computation of the window mode and cursor visibility implied by
/// `settings`, the session [`FullscreenOverride`], and whether the settings
/// panel is open, given the monitors currently known to the ECS.
///
/// Shared by `apply_display_mode`'s `Startup` run (booting the kiosk) and its
/// `Update` run (every frame thereafter), so a settings-panel edit, an F11
/// toggle, and a monitor add/remove all re-derive the same target instead of
/// three code paths that can disagree about what "fullscreen" currently
/// means.
///
/// `settings_panel_open` **forces the cursor visible**, regardless of
/// `hide_cursor`. A release/kiosk build defaults `hide_cursor = true`, and the
/// only way to turn it back off is the *mouse-driven* settings panel — so with
/// the pointer hidden the operator would be blind-clicking their way through
/// the one UI that can undo it. The panel being open is exactly the moment a
/// pointer is wanted; the cursor disappears again the frame it closes. Passed
/// in (rather than read from `SettingsPanelVisible` inside the system) so this
/// stays a pure, unit-testable function.
///
/// Takes an iterator rather than a slice so a live `Query` never has to
/// collect into a `Vec` first (AGENTS.md's "never allocate in a hot path" —
/// `resolve_monitor_selection`'s `find` short-circuits on the first match, so
/// nothing downstream needs the full list materialised).
pub(crate) fn compute_display_mode<'a>(
    settings: &DisplaySettings,
    fullscreen_override: FullscreenOverride,
    settings_panel_open: bool,
    live_monitors: impl IntoIterator<Item = (Entity, Option<&'a str>)>,
) -> DisplayModeTarget {
    let mode = if fullscreen_override.effective_fullscreen(settings) {
        WindowMode::BorderlessFullscreen(resolve_monitor_selection(
            &settings.monitor,
            live_monitors,
        ))
    } else {
        WindowMode::Windowed
    };
    DisplayModeTarget {
        mode,
        cursor_visible: settings_panel_open || !settings.hide_cursor,
    }
}

/// Resolve a persisted monitor name to a [`MonitorSelection`] against the
/// monitors currently known to the ECS.
///
/// - An empty `saved_name` (the field's default) means "no preference":
///   always [`MonitorSelection::Current`].
/// - A non-empty name matching a live monitor's `Some(name)` resolves to
///   that monitor's `Entity`.
/// - A non-empty name with no live match — the monitor is asleep, unplugged,
///   or winit has not enumerated any monitors yet (this can run at
///   `Startup`, which may race `bevy_winit`'s monitor sync) — falls back to
///   [`MonitorSelection::Current`]. The caller never rewrites `saved_name` in
///   this case; the `&str` parameter makes that structurally true. An HDMI TV
///   that is merely asleep must not lose its saved binding.
fn resolve_monitor_selection<'a>(
    saved_name: &str,
    live_monitors: impl IntoIterator<Item = (Entity, Option<&'a str>)>,
) -> MonitorSelection {
    if saved_name.is_empty() {
        return MonitorSelection::Current;
    }
    live_monitors
        .into_iter()
        .find(|(_, name)| *name == Some(saved_name))
        .map_or(MonitorSelection::Current, |(entity, _)| {
            MonitorSelection::Entity(entity)
        })
}

/// Serde fallback and `#[setting(default = ...)]` value for `start_fullscreen`.
///
/// Delegates to the pure, fully unit-tested [`default_start_fullscreen_for`]
/// so both branches of the `cfg!` are exercised by tests without needing to
/// recompile under a different profile — `cargo test` / `cargo nextest`
/// always run under `debug_assertions`, so a direct `cfg!(debug_assertions)`
/// call in a test would only ever cover one branch.
fn default_start_fullscreen() -> bool {
    default_start_fullscreen_for(cfg!(debug_assertions))
}

/// Pure branch behind [`default_start_fullscreen`]. `cargo rund` (dev) must
/// not default to fullscreen, or every dev relaunch would swallow the window;
/// a release/kiosk build must, or a field tester with no keyboard has no way
/// to find F11.
fn default_start_fullscreen_for(is_debug_build: bool) -> bool {
    !is_debug_build
}

/// Serde fallback and `#[setting(default = ...)]` value for `hide_cursor`.
///
/// Delegates to the pure, fully unit-tested [`default_hide_cursor_for`], for
/// the same reason [`default_start_fullscreen`] does: tests always compile
/// under `debug_assertions`, so a direct `cfg!` here could only ever cover one
/// branch.
fn default_hide_cursor() -> bool {
    default_hide_cursor_for(cfg!(debug_assertions))
}

/// Pure branch behind [`default_hide_cursor`]. A release/kiosk build hides the
/// cursor (nobody is holding a mouse in front of the TV); a dev build must not,
/// because the only in-app way to turn it back on is the mouse-driven settings
/// panel — hiding the pointer by default would be the same chicken-and-egg trap
/// [`default_start_fullscreen_for`] exists to avoid.
fn default_hide_cursor_for(is_debug_build: bool) -> bool {
    !is_debug_build
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;
    use crate::settings::{SettingKind, SketchSettings};

    fn entity(raw: u32) -> Entity {
        Entity::from_raw_u32(raw).expect("small literal is a valid non-max raw id")
    }

    // --- default_start_fullscreen_for ---

    #[test]
    fn start_fullscreen_defaults_to_false_under_debug_assertions_so_cargo_rund_stays_sane() {
        assert!(!default_start_fullscreen_for(true));
    }

    #[test]
    fn start_fullscreen_defaults_to_true_in_release_so_a_kiosk_boots_fullscreen() {
        assert!(default_start_fullscreen_for(false));
    }

    // --- default_hide_cursor_for ---

    #[test]
    fn hide_cursor_defaults_to_false_under_debug_assertions_so_the_settings_panel_stays_clickable()
    {
        assert!(!default_hide_cursor_for(true));
    }

    #[test]
    fn hide_cursor_defaults_to_true_in_release_so_a_kiosk_shows_no_pointer() {
        assert!(default_hide_cursor_for(false));
    }

    // --- resolve_monitor_selection ---

    #[test]
    fn empty_saved_name_resolves_to_current_regardless_of_live_monitors() {
        let live = [(entity(1), Some("DELL U2720Q"))];
        assert_eq!(
            resolve_monitor_selection("", live),
            MonitorSelection::Current
        );
    }

    #[test]
    fn a_saved_name_matching_a_live_monitor_resolves_to_its_entity() {
        let target = entity(2);
        let live = [
            (entity(1), Some("Built-in Display")),
            (target, Some("LG TV")),
        ];
        assert_eq!(
            resolve_monitor_selection("LG TV", live),
            MonitorSelection::Entity(target)
        );
    }

    #[test]
    fn a_saved_name_with_no_live_match_falls_back_to_current() {
        // The caller (compute_display_mode / apply_display_mode) never mutates
        // `saved_name` in this case — the type signature makes that
        // structurally true: `&str` in, no mutation possible. An HDMI TV
        // that is merely asleep must not lose its saved binding.
        let live = [(entity(1), Some("Built-in Display"))];
        assert_eq!(
            resolve_monitor_selection("LG TV (asleep)", live),
            MonitorSelection::Current
        );
    }

    #[test]
    fn a_saved_name_resolves_against_an_empty_monitor_list_to_current() {
        // Covers the Startup-vs-create_monitors race: at boot the ECS may not
        // have any Monitor entities yet. Falling back here is what makes that
        // race harmless — see the module doc on compute_display_mode.
        let live: [(Entity, Option<&str>); 0] = [];
        assert_eq!(
            resolve_monitor_selection("LG TV", live),
            MonitorSelection::Current
        );
    }

    #[test]
    fn an_unnamed_live_monitor_never_matches_a_non_empty_saved_name() {
        let live = [(entity(1), None)];
        assert_eq!(
            resolve_monitor_selection("LG TV", live),
            MonitorSelection::Current
        );
    }

    // --- compute_display_mode ---

    /// No session override and no settings panel: the plain configured case
    /// every pre-existing test below assumed before `FullscreenOverride` and
    /// the panel-open cursor rule existed.
    const NO_OVERRIDE: FullscreenOverride = FullscreenOverride(None);
    const PANEL_CLOSED: bool = false;
    const PANEL_OPEN: bool = true;

    #[test]
    fn windowed_when_start_fullscreen_is_false_regardless_of_monitor() {
        let settings = DisplaySettings {
            start_fullscreen: false,
            hide_cursor: false,
            monitor: "LG TV".to_string(),
        };
        let live = [(entity(1), Some("LG TV"))];
        let target = compute_display_mode(&settings, NO_OVERRIDE, PANEL_CLOSED, live);
        assert_eq!(target.mode, WindowMode::Windowed);
    }

    #[test]
    fn fullscreen_on_current_when_monitor_is_unset() {
        let settings = DisplaySettings {
            start_fullscreen: true,
            hide_cursor: false,
            monitor: String::new(),
        };
        let target = compute_display_mode(&settings, NO_OVERRIDE, PANEL_CLOSED, []);
        assert_eq!(
            target.mode,
            WindowMode::BorderlessFullscreen(MonitorSelection::Current)
        );
    }

    #[test]
    fn fullscreen_on_the_named_monitor_when_it_resolves() {
        let target_entity = entity(3);
        let settings = DisplaySettings {
            start_fullscreen: true,
            hide_cursor: false,
            monitor: "LG TV".to_string(),
        };
        let live = [
            (entity(1), Some("Built-in Display")),
            (target_entity, Some("LG TV")),
        ];
        let target = compute_display_mode(&settings, NO_OVERRIDE, PANEL_CLOSED, live);
        assert_eq!(
            target.mode,
            WindowMode::BorderlessFullscreen(MonitorSelection::Entity(target_entity))
        );
    }

    #[test]
    fn cursor_visible_is_the_negation_of_hide_cursor() {
        let hidden = DisplaySettings {
            start_fullscreen: false,
            hide_cursor: true,
            monitor: String::new(),
        };
        let shown = DisplaySettings {
            start_fullscreen: false,
            hide_cursor: false,
            monitor: String::new(),
        };
        assert!(!compute_display_mode(&hidden, NO_OVERRIDE, PANEL_CLOSED, []).cursor_visible);
        assert!(compute_display_mode(&shown, NO_OVERRIDE, PANEL_CLOSED, []).cursor_visible);
    }

    // --- FullscreenOverride ---

    #[test]
    fn no_override_defers_to_the_persisted_start_fullscreen() {
        let windowed = DisplaySettings {
            start_fullscreen: false,
            hide_cursor: false,
            monitor: String::new(),
        };
        let fullscreen = DisplaySettings {
            start_fullscreen: true,
            ..windowed.clone()
        };
        assert!(!FullscreenOverride::default().effective_fullscreen(&windowed));
        assert!(FullscreenOverride::default().effective_fullscreen(&fullscreen));
    }

    #[test]
    fn a_session_override_wins_over_the_persisted_start_fullscreen_in_both_directions() {
        let kiosk = DisplaySettings {
            start_fullscreen: true,
            hide_cursor: true,
            monitor: String::new(),
        };
        let dev = DisplaySettings {
            start_fullscreen: false,
            ..kiosk.clone()
        };
        assert!(!FullscreenOverride(Some(false)).effective_fullscreen(&kiosk));
        assert!(FullscreenOverride(Some(true)).effective_fullscreen(&dev));
    }

    #[test]
    fn an_override_of_false_windows_a_kiosk_configured_for_fullscreen() {
        // The F11-at-the-party case, in reverse: the persisted config says
        // fullscreen, the session override says no. The *override* decides what
        // is on screen — and, being session-only, is gone after a power cycle.
        let kiosk = DisplaySettings {
            start_fullscreen: true,
            hide_cursor: false,
            monitor: "LG TV".to_string(),
        };
        let live = [(entity(1), Some("LG TV"))];
        let target =
            compute_display_mode(&kiosk, FullscreenOverride(Some(false)), PANEL_CLOSED, live);
        assert_eq!(target.mode, WindowMode::Windowed);
    }

    #[test]
    fn an_override_of_true_fullscreens_on_the_saved_monitor() {
        let target_entity = entity(2);
        let settings = DisplaySettings {
            start_fullscreen: false,
            hide_cursor: false,
            monitor: "LG TV".to_string(),
        };
        let live = [(target_entity, Some("LG TV"))];
        let target = compute_display_mode(
            &settings,
            FullscreenOverride(Some(true)),
            PANEL_CLOSED,
            live,
        );
        assert_eq!(
            target.mode,
            WindowMode::BorderlessFullscreen(MonitorSelection::Entity(target_entity)),
            "an override must still honour the saved monitor, not fall back to Current"
        );
    }

    #[test]
    fn fullscreen_override_defaults_to_none_so_every_boot_starts_from_the_persisted_value() {
        assert_eq!(FullscreenOverride::default(), FullscreenOverride(None));
    }

    // --- settings-panel cursor rule ---

    #[test]
    fn an_open_settings_panel_forces_the_cursor_visible_even_when_hide_cursor_is_set() {
        // The release/kiosk default is hide_cursor = true, and the only in-app
        // way to turn it off is this very panel — which is mouse-driven. Hiding
        // the pointer while it is open would mean blind-clicking.
        let kiosk = DisplaySettings {
            start_fullscreen: true,
            hide_cursor: true,
            monitor: String::new(),
        };
        assert!(compute_display_mode(&kiosk, NO_OVERRIDE, PANEL_OPEN, []).cursor_visible);
        assert!(
            !compute_display_mode(&kiosk, NO_OVERRIDE, PANEL_CLOSED, []).cursor_visible,
            "closing the panel must hide the cursor again — the rule is scoped to \
             the panel, it does not permanently defeat hide_cursor"
        );
    }

    #[test]
    fn an_open_settings_panel_does_not_change_the_window_mode() {
        let kiosk = DisplaySettings {
            start_fullscreen: true,
            hide_cursor: true,
            monitor: String::new(),
        };
        assert_eq!(
            compute_display_mode(&kiosk, NO_OVERRIDE, PANEL_OPEN, []).mode,
            WindowMode::BorderlessFullscreen(MonitorSelection::Current),
            "the panel-open rule is about the cursor only"
        );
    }

    // --- struct plumbing ---

    #[test]
    fn display_settings_default_matches_the_debug_build_default() {
        // `cargo test` always compiles under debug_assertions, so this
        // exercises the same branch as `cargo rund`; the release branches are
        // covered directly by `start_fullscreen_defaults_to_true_in_release...`
        // and `hide_cursor_defaults_to_true_in_release...` above via the
        // parameterised helpers.
        let settings = DisplaySettings::default();
        assert!(!settings.start_fullscreen);
        assert!(!settings.hide_cursor);
        assert_eq!(settings.monitor, String::new());
    }

    #[test]
    fn available_monitors_defaults_empty() {
        assert!(AvailableMonitors::default().0.is_empty());
    }

    /// The derive macro's `options_key` (on the `monitor` field) and the
    /// options source's `OPTIONS_KEY` (on [`AvailableMonitors`]) are written a
    /// few dozen lines apart and tied together by nothing but a string literal;
    /// 03a resolves an unknown key to an empty option list, so a drift between
    /// them degrades the dropdown silently rather than failing the build. Pin
    /// them together. (`crate::lifecycle::display`'s
    /// `the_monitor_fields_options_key_resolves_against_a_registered_source`
    /// covers the other half: that the source is actually *registered*.)
    ///
    /// `unreachable!` (not `panic!`) on the two structural invariants: the
    /// derive macro always emits a def for every field, and `monitor` is
    /// declared `ty = RuntimeEnum`, so neither `else` arm is reachable unless
    /// the struct definition above changed out from under this test. Bare
    /// `panic!` is denied workspace-wide; `clippy::unreachable` is a
    /// `restriction` lint and is not enabled.
    #[test]
    fn monitor_field_options_key_matches_its_options_source() {
        let Some(def) = DisplaySettings::settings_def()
            .into_iter()
            .find(|d| d.field_name == "monitor")
        else {
            unreachable!("the derive macro always emits a def for `monitor`");
        };
        let SettingKind::RuntimeEnum { options_key } = def.kind else {
            unreachable!("`monitor` is declared `ty = RuntimeEnum`");
        };
        assert_eq!(options_key, AvailableMonitors::OPTIONS_KEY);
    }
}
