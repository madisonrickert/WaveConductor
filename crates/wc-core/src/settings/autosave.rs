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
