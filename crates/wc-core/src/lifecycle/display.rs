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
//! `sync_available_monitors` is the one system here that allocates
//! (`AvailableMonitors` owns a `Vec<String>`), so — unlike
//! `apply_display_mode` — it *is* gated on an actual add/remove signal,
//! read once and fully drained so the same event cannot re-trigger it on a
//! later frame.

use bevy::prelude::*;
use bevy::window::{CursorOptions, Monitor};

use crate::settings::{compute_display_mode, AvailableMonitors, DisplaySettings};
use crate::settings::{RegisterRuntimeEnumOptionsExt, RegisterSketchSettingsExt};

/// Plugin: registers `DisplaySettings`, initialises `AvailableMonitors` and
/// registers it as the `"monitors"` runtime-enum options source (which is what
/// makes the `monitor` setting render as a dropdown), and wires
/// `apply_display_mode` / `sync_available_monitors`.
///
/// Registered by [`crate::lifecycle::LifecyclePlugin`].
pub struct DisplayPlugin;

impl Plugin for DisplayPlugin {
    fn build(&self, app: &mut App) {
        app.register_sketch_settings::<DisplaySettings>();
        app.init_resource::<AvailableMonitors>();
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
                // same frame it is pressed, not one frame later.
                apply_display_mode.after(crate::lifecycle::nav::handle_navigation_actions),
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
pub(crate) fn apply_display_mode(
    settings: Res<'_, DisplaySettings>,
    monitors: Query<'_, '_, (Entity, &Monitor)>,
    mut windows: Query<'_, '_, (&mut Window, &mut CursorOptions)>,
) {
    let target = compute_display_mode(
        &settings,
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
