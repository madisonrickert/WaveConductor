//! Audio-side handler for the `ToggleVolume` action.
//!
//! Reads `MessageReader<ActionInput>` and, when a `ToggleVolume` + `Pressed`
//! message arrives, pushes a `SetMuted` command flipping the current target
//! mute state. The egui keyboard-capture gate lives in the `emit_action_input`
//! producer (`LifecyclePlugin`), so no `.run_if` is needed here.

use bevy::ecs::system::NonSendMut;
use bevy::prelude::*;

use crate::lifecycle::action_map::{ActionInput, ActionPhase};
use crate::lifecycle::actions::WaveConductorAction;

use super::command::AudioCommand;
use super::ring::AudioCommandSender;
use super::state::AudioState;

/// `Update` system that translates `ToggleVolume` presses into `SetMuted`.
///
/// Uses `NonSendMut<AudioCommandSender>` because `rtrb::Producer` is not
/// `Sync`; see `ring` module docs.
pub fn handle_volume_toggle(
    mut actions: MessageReader<'_, '_, ActionInput>,
    state: Res<'_, AudioState>,
    mut sender: NonSendMut<'_, AudioCommandSender>,
) {
    let toggled = actions
        .read()
        .any(|a| a.action == WaveConductorAction::ToggleVolume && a.phase == ActionPhase::Pressed);
    if toggled {
        // `state.muted` is mirrored from the audio thread's echo, so a rapid
        // double-press within the same echo-latency window (~1 frame) can push the
        // same direction twice. Acceptable for a user-driven V key (worst case: one
        // missed toggle the user re-presses).
        let new_muted = !state.muted;
        if let Err(_dropped) = sender.push(AudioCommand::SetMuted(new_muted)) {
            tracing::warn!("audio command ring full; dropping SetMuted command");
        } else {
            tracing::info!(new_muted, "toggle volume → SetMuted");
        }
    }
}
