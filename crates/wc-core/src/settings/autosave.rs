//! Debounced settings auto-save.
//!
//! When any registered settings resource changes, schedule a save after a
//! short idle window. This keeps disk writes bounded (one write per debounce
//! window, not one per keystroke) while ensuring no edit is lost on shutdown
//! beyond the debounce-window worth of buffered changes.

use std::collections::HashMap;

use bevy::prelude::*;
use smallvec::SmallVec;

use super::registry::SettingsRegistry;

/// Function pointer + key pair snapshot for change detection.
type ChangedSnapshot = SmallVec<[(fn(&World) -> bool, &'static str); 8]>;
/// Function pointer + key pair snapshot for save dispatch.
type SaveSnapshot = SmallVec<[(fn(&World), &'static str); 8]>;

/// Debounce window. Saves trigger this many seconds after the last change.
pub const DEBOUNCE_SECS: f32 = 0.5;

/// Per-storage-key debounce state. None = clean. Some(timer) = pending save.
#[derive(Resource, Default, Debug)]
pub struct AutosaveState {
    /// Map of `storage_key` → remaining seconds until save fires.
    pub pending: HashMap<&'static str, f32>,
}

/// Detect resource changes and (re)start the per-type debounce timer.
/// Runs in `Update` after `emit_restart_events` so we observe the same
/// change ticks.
pub fn detect_changes(world: &mut World) {
    let snapshot: ChangedSnapshot = world
        .get_resource::<SettingsRegistry>()
        .map(|r| {
            r.entries
                .iter()
                .map(|e| (e.is_changed_fn, e.storage_key))
                .collect()
        })
        .unwrap_or_default();
    let mut to_arm = SmallVec::<[&'static str; 8]>::new();
    for (changed_fn, key) in snapshot {
        if changed_fn(world) {
            to_arm.push(key);
        }
    }
    if to_arm.is_empty() {
        return;
    }
    let mut state = world.resource_mut::<AutosaveState>();
    for key in to_arm {
        state.pending.insert(key, DEBOUNCE_SECS);
    }
}

/// Advance debounce timers and fire `save_fn` when a timer reaches zero.
pub fn tick(world: &mut World) {
    let dt = world.resource::<Time>().delta_secs();
    let to_save: SmallVec<[&'static str; 8]> = {
        let mut state = world.resource_mut::<AutosaveState>();
        let mut fire = SmallVec::<[&'static str; 8]>::new();
        state.pending.retain(|key, timer| {
            *timer -= dt;
            if *timer <= 0.0 {
                fire.push(*key);
                false
            } else {
                true
            }
        });
        fire
    };
    if to_save.is_empty() {
        return;
    }
    let snapshot: SaveSnapshot = world
        .get_resource::<SettingsRegistry>()
        .map(|r| {
            r.entries
                .iter()
                .map(|e| (e.save_fn, e.storage_key))
                .collect()
        })
        .unwrap_or_default();
    for key in to_save {
        if let Some((save_fn, _)) = snapshot.iter().find(|(_, k)| *k == key) {
            save_fn(world);
            tracing::debug!(%key, "settings saved (debounce window elapsed)");
        }
    }
}

/// Drains any pending debounce timers on `AppExit` and writes every queued
/// settings type to disk. Without this, edits made in the <0.5 s window before
/// shutdown are lost because [`tick`] never fires their saves.
///
/// Reads `MessageReader<AppExit>` and runs in `Update` (not `Last`) because
/// Bevy's exit handling consumes `Update`'s schedule cycle.
pub fn flush_on_exit(world: &mut World) {
    let mut state = bevy::ecs::system::SystemState::<
        bevy::prelude::MessageReader<'_, '_, bevy::app::AppExit>,
    >::new(world);
    let mut reader = state.get_mut(world);
    let exiting = reader.read().next().is_some();
    state.apply(world);
    if !exiting {
        return;
    }
    let keys: smallvec::SmallVec<[&'static str; 8]> = {
        let mut s = world.resource_mut::<AutosaveState>();
        let collected = s.pending.keys().copied().collect();
        s.pending.clear();
        collected
    };
    if keys.is_empty() {
        return;
    }
    let snapshot: SaveSnapshot = world
        .get_resource::<SettingsRegistry>()
        .map(|r| {
            r.entries
                .iter()
                .map(|e| (e.save_fn, e.storage_key))
                .collect()
        })
        .unwrap_or_default();
    for key in keys {
        if let Some((save_fn, _)) = snapshot.iter().find(|(_, k)| *k == key) {
            save_fn(world);
            tracing::info!(%key, "settings saved (flush on AppExit)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::app::AppExit;

    #[test]
    fn flush_on_exit_drains_pending_when_exit_emitted() {
        let mut app = bevy::prelude::App::new();
        app.add_plugins(bevy::MinimalPlugins);
        app.init_resource::<AutosaveState>();
        app.init_resource::<crate::settings::registry::SettingsRegistry>();

        // Seed one pending key. We don't need a real save_fn here — with no
        // matching registry entry, flush_on_exit logs "no save fn" and moves
        // on. The key behavior under test is that `pending` is drained.
        app.world_mut()
            .resource_mut::<AutosaveState>()
            .pending
            .insert("synthetic-key", DEBOUNCE_SECS);

        // Emit AppExit and run one update.
        app.world_mut().write_message(AppExit::Success);
        app.add_systems(bevy::prelude::Update, flush_on_exit);
        app.update();

        let state = app.world().resource::<AutosaveState>();
        assert!(state.pending.is_empty(), "flush_on_exit must drain pending");
    }
}
