//! Applies `DisplaySettings` to the primary window: startup fullscreen,
//! monitor selection, and cursor visibility.
//!
//! ## There is no `MonitorAdded` / `MonitorRemoved` message in Bevy 0.19
//!
//! The design doc and the alpha.5 program index both describe re-asserting
//! fullscreen "on `MonitorAdded` / `MonitorRemoved`". Verified against the
//! vendored `bevy_window-0.19.0` and `bevy_winit-0.19.0` source (2026-07-09):
//! **no such message type exists.** Monitors are plain [`Monitor`] ECS
//! components. `bevy_winit::system::create_monitors`
//! (`bevy_winit-0.19.0/src/system.rs:177-234`) `commands.spawn(Monitor {
//! .. })`s a new one, and `commands.entity(*entity).despawn()`s a
//! disappeared one, once per winit event-loop iteration, from
//! `WinitAppRunnerState::about_to_wait`
//! (`bevy_winit-0.19.0/src/state.rs:459-462`) — there is no dedicated
//! message, only component add/remove.
//!
//! Rather than reconstruct add/remove *events* from `Added<Monitor>` /
//! `RemovedComponents<Monitor>` on the mode-applying system (and risk a
//! frame where the reader is consumed by the wrong system, or a race between
//! `Startup` and the first `create_monitors` call), `apply_display_mode`
//! runs **unconditionally every `Update` frame** and idempotently re-derives
//! the window's target mode from `DisplaySettings` plus a fresh `Monitor`
//! query via `crate::settings::compute_display_mode`. This is cheap — a
//! handful of entity comparisons, no allocation, no-op when nothing changed
//! — and correct regardless of which frame a monitor actually (dis)appears
//! on: it re-asserts by construction, every frame, rather than by reacting to
//! an event that does not exist. It is also correct at boot even if
//! `Startup` runs before `create_monitors`'s first pass: the query is simply
//! empty that frame, `resolve_monitor_selection` falls back to
//! `MonitorSelection::Current`, and the very next `Update` frame converges on
//! the named monitor once it exists.
//!
//! (Intra-doc links to `apply_display_mode` / `sync_available_monitors` /
//! `DisplaySettings` / `AvailableMonitors` are deliberately written as plain
//! code spans throughout this module's *public* docs — the items are
//! `pub(crate)`, and a `pub` item linking to a private one is a hard error
//! under CI's `RUSTDOCFLAGS="-D warnings"`
//! (`rustdoc::private_intra_doc_links`). The private items' own docs below
//! may and do link normally.)
//!
//! ## F11 is a session override, not a settings write
//!
//! `apply_display_mode` derives fullscreen from the session-only
//! `FullscreenOverride` resource layered over the persisted
//! `DisplaySettings::start_fullscreen`, not from the setting alone. F11 writes
//! only the override, so a stray keypress at an unattended installation cannot
//! survive a power cycle; the settings-panel checkbox still writes (and persists)
//! the setting, and `clear_fullscreen_override_on_settings_edit` drops any live
//! override when it does, so the operator's explicit choice wins immediately.
//!
//! `sync_available_monitors` is the one system here that allocates
//! (`AvailableMonitors` owns a `Vec<String>`), so — unlike
//! `apply_display_mode` — it *is* gated on an actual add/remove signal,
//! read once and fully drained so the same event cannot re-trigger it on a
//! later frame.

use bevy::prelude::*;
use bevy::window::{CursorOptions, Monitor};

use crate::settings::{
    compute_display_mode, AvailableMonitors, DisplaySettings, FullscreenOverride,
};
use crate::settings::{RegisterRuntimeEnumOptionsExt, RegisterSketchSettingsExt};
use crate::ui::buttons::SettingsPanelVisible;

/// Plugin: registers `DisplaySettings`, initialises `AvailableMonitors` and
/// registers it as the `"monitors"` runtime-enum options source (which is what
/// makes the `monitor` setting render as a dropdown), initialises the
/// session-only `FullscreenOverride` (what F11 writes; never persisted), and
/// wires `apply_display_mode` / `sync_available_monitors` /
/// `clear_fullscreen_override_on_settings_edit`.
///
/// Registered by [`crate::lifecycle::LifecyclePlugin`].
pub struct DisplayPlugin;

impl Plugin for DisplayPlugin {
    fn build(&self, app: &mut App) {
        app.register_sketch_settings::<DisplaySettings>();
        app.init_resource::<AvailableMonitors>();
        // The F11 override. A plain resource, deliberately *not* a
        // SketchSettings: a stray F11 at the installation must not survive a
        // power cycle. See `crate::settings::FullscreenOverride`.
        app.init_resource::<FullscreenOverride>();
        // `apply_display_mode` reads SettingsPanelVisible (an open settings
        // panel forces the cursor visible). SettingsPlugin and
        // OverlayButtonsPlugin each init it too; `init_resource` is idempotent,
        // and doing it here keeps `DisplayPlugin` self-sufficient in the
        // MinimalPlugins test harnesses that load it alone.
        app.init_resource::<SettingsPanelVisible>();
        // Expose the live monitor list to Plan 03a's runtime-enum widget so
        // the `monitor` setting renders as a dropdown. `AvailableMonitors`
        // impls `RuntimeEnumOptionsSource` with `OPTIONS_KEY = "monitors"`,
        // matching the field's `options_key`.
        app.register_runtime_enum_options::<AvailableMonitors>();
        // Boot-time apply. May race bevy_winit's first `create_monitors`
        // pass (see the module doc); harmless, because the Update-scheduled
        // copy below converges on the next frame regardless.
        app.add_systems(Startup, apply_display_mode);
        app.add_systems(
            Update,
            (
                sync_available_monitors,
                // Ordered after the F11 handler so a toggle takes effect the
                // same frame it is pressed, not one frame later, and before
                // `apply_display_mode` so a panel edit made this frame is not
                // masked by a stale override for a frame.
                clear_fullscreen_override_on_settings_edit
                    .after(crate::lifecycle::nav::handle_navigation_actions),
                apply_display_mode
                    .after(crate::lifecycle::nav::handle_navigation_actions)
                    .after(clear_fullscreen_override_on_settings_edit),
            ),
        );
    }
}

/// Idempotently apply [`DisplaySettings`] to every window.
///
/// Runs at `Startup` and unconditionally every `Update` frame — see the
/// module doc for why "every frame" stands in for a monitor-topology message
/// Bevy 0.19 does not have. Writes `Window::mode` / `CursorOptions::visible`
/// only when the computed value actually differs from the current one, so an
/// unchanged frame does not spuriously mark either component "changed" for
/// downstream change-detection consumers (e.g. Plan 02's resize listeners).
///
/// Takes `Res<DisplaySettings>`, never `ResMut`: when the saved monitor name
/// matches no live monitor (an HDMI TV that is merely asleep, or a display
/// that has not re-enumerated yet after wake) the window falls back to the
/// current monitor but the *saved name is never rewritten*. A kiosk must not
/// lose its monitor binding because the TV slept overnight.
///
/// The fullscreen decision comes from
/// [`FullscreenOverride::effective_fullscreen`], not from
/// `settings.start_fullscreen` directly: F11 writes the session-only override so
/// a stray keypress at the installation cannot outlive a power cycle. Cursor
/// visibility additionally honours `SettingsPanelVisible` — an open settings
/// panel forces the pointer visible, because that panel is the only in-app way to
/// undo `hide_cursor` and it is mouse-driven.
///
/// Allocation-free: this runs every frame for the life of the session, so the
/// monitor list is passed as a borrowing iterator (never collected) and no
/// `String` is cloned.
pub(crate) fn apply_display_mode(
    settings: Res<'_, DisplaySettings>,
    fullscreen_override: Res<'_, FullscreenOverride>,
    panel_visible: Res<'_, SettingsPanelVisible>,
    monitors: Query<'_, '_, (Entity, &Monitor)>,
    mut windows: Query<'_, '_, (&mut Window, &mut CursorOptions)>,
) {
    let target = compute_display_mode(
        &settings,
        *fullscreen_override,
        panel_visible.0,
        monitors
            .iter()
            .map(|(entity, monitor)| (entity, monitor.name.as_deref())),
    );
    for (mut window, mut cursor) in &mut windows {
        if window.mode != target.mode {
            window.mode = target.mode;
        }
        if cursor.visible != target.cursor_visible {
            cursor.visible = target.cursor_visible;
        }
    }
}

/// Drop the session [`FullscreenOverride`] when the operator changes
/// "Start fullscreen" in the settings panel.
///
/// An explicit panel edit is the authoritative statement of intent and must take
/// effect on screen immediately; leaving a stale F11 override in place would make
/// the checkbox appear dead (it would save to disk but change nothing visible
/// until the next launch).
///
/// Gated on the *value* of `start_fullscreen` actually changing, tracked in a
/// `Local`, rather than on `Res::is_changed()`. The user panel writes its
/// reflected fields through a real `Mut<DisplaySettings>` borrow, so the resource
/// is marked changed on every frame the DISPLAY tab is merely *rendered*, edit or
/// no edit — arming off change detection would silently cancel an F11 override the
/// moment the operator opened the tab to look at it.
///
/// The `Local` starts `None`, so the first run only records the loaded value and
/// never clears anything: a boot is not an edit.
pub(crate) fn clear_fullscreen_override_on_settings_edit(
    settings: Res<'_, DisplaySettings>,
    mut fullscreen_override: ResMut<'_, FullscreenOverride>,
    mut previous_start_fullscreen: Local<'_, Option<bool>>,
) {
    let current = settings.start_fullscreen;
    let edited = previous_start_fullscreen.is_some_and(|previous| previous != current);
    *previous_start_fullscreen = Some(current);
    // Guarded so an unchanged frame never touches the resource (and so never
    // spuriously marks it changed for downstream change detection).
    if edited && fullscreen_override.0.is_some() {
        fullscreen_override.0 = None;
        tracing::info!(
            start_fullscreen = current,
            "settings-panel edit cleared the session fullscreen override"
        );
    }
}

/// Refresh [`AvailableMonitors`] from the live `Monitor` set, but only on a
/// frame where a monitor was actually added or removed.
///
/// `removed.read().count()` fully drains the `RemovedComponents` reader, so
/// a removal is consumed exactly once and cannot re-trigger this system on a
/// later frame it never happened on (unlike calling `.is_empty()`, which
/// would peek without advancing the cursor). `Added<Monitor>` needs no
/// equivalent draining — Bevy scopes it to "added since this query's last
/// run" automatically.
pub(crate) fn sync_available_monitors(
    mut available: ResMut<'_, AvailableMonitors>,
    monitors: Query<'_, '_, &Monitor>,
    added: Query<'_, '_, (), Added<Monitor>>,
    mut removed: RemovedComponents<'_, '_, Monitor>,
) {
    let any_removed = removed.read().count() > 0;
    if added.is_empty() && !any_removed {
        return;
    }
    available.0.clear();
    available
        .0
        .extend(monitors.iter().filter_map(|m| m.name.clone()));
}

#[cfg(test)]
mod tests {
    use bevy::prelude::*;

    use super::*;

    #[test]
    fn display_plugin_registers_its_resources_without_panicking() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(DisplayPlugin);
        assert!(app
            .world()
            .contains_resource::<crate::settings::DisplaySettings>());
        assert!(app
            .world()
            .contains_resource::<crate::settings::AvailableMonitors>());
        assert!(app.world().contains_resource::<FullscreenOverride>());
        assert!(app.world().contains_resource::<SettingsPanelVisible>());
    }

    /// A settings-panel run of the systems with no edit must leave a live F11
    /// override alone. The user panel marks `DisplaySettings` changed on every
    /// frame the DISPLAY tab is merely rendered (it writes through a real
    /// `Mut<DisplaySettings>` reflected borrow), so a change-detection-based
    /// clear would cancel the operator's F11 the instant they opened the tab to
    /// look at it. The clear arms on the *value* of `start_fullscreen` changing.
    #[test]
    fn a_changed_but_unedited_display_settings_does_not_clear_the_override() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(DisplayPlugin);
        app.update();

        app.world_mut().resource_mut::<FullscreenOverride>().0 = Some(true);
        // Touch the resource the way the panel does: a mutable borrow with no
        // actual value change, which still marks it `is_changed()`.
        app.world_mut()
            .resource_mut::<crate::settings::DisplaySettings>()
            .set_changed();
        app.update();

        assert_eq!(
            *app.world().resource::<FullscreenOverride>(),
            FullscreenOverride(Some(true)),
            "merely rendering the DISPLAY tab must not cancel an F11 override"
        );
    }

    /// The other direction: an actual edit to "Start fullscreen" in the panel is
    /// the operator's explicit, persisted choice, and must take effect on screen
    /// immediately rather than sitting behind a stale F11 override (which would
    /// make the checkbox look dead).
    #[test]
    fn editing_start_fullscreen_in_the_panel_clears_the_override() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(DisplayPlugin);
        app.update();

        let initial = app
            .world()
            .resource::<crate::settings::DisplaySettings>()
            .start_fullscreen;
        app.world_mut().resource_mut::<FullscreenOverride>().0 = Some(!initial);
        app.world_mut()
            .resource_mut::<crate::settings::DisplaySettings>()
            .start_fullscreen = !initial;
        app.update();

        assert_eq!(
            *app.world().resource::<FullscreenOverride>(),
            FullscreenOverride(None),
            "a panel edit to start_fullscreen must drop the session override"
        );
    }

    /// The half that `settings::panel_user::display`'s
    /// `monitor_field_options_key_matches_its_options_source` cannot see: that
    /// the options source is actually *registered* with the `App`, so the
    /// `monitor` field's declared `options_key` resolves to a real entry in
    /// 03a's snapshot. This is the same condition
    /// `settings::runtime_enum::warn_on_unresolved_options_keys` warns about at
    /// startup in debug builds; asserting it here means CI fails on a broken
    /// wiring instead of a human having to spot a `warn!` line.
    ///
    /// Note it checks the snapshot *entry*, not `options_for`'s slice: with no
    /// `Monitor` entities in a headless `App` the option list is legitimately
    /// empty, which is exactly what an unresolved key also looks like. Only the
    /// key's presence distinguishes the two.
    #[test]
    fn the_monitor_fields_options_key_resolves_against_a_registered_source() {
        use crate::settings::runtime_enum::snapshot;
        use crate::settings::{SettingKind, SketchSettings};

        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(DisplayPlugin);

        let Some(def) = crate::settings::DisplaySettings::settings_def()
            .into_iter()
            .find(|d| d.field_name == "monitor")
        else {
            unreachable!("the derive macro always emits a def for `monitor`");
        };
        let SettingKind::RuntimeEnum { options_key } = def.kind else {
            unreachable!("`monitor` is declared `ty = RuntimeEnum`");
        };

        let snap = snapshot(app.world());
        assert!(
            snap.iter().any(|entry| entry.options_key == options_key),
            "no registered RuntimeEnumOptionsSource reports `{options_key}`; \
             the monitor dropdown would render empty"
        );
    }
}
