//! Audio-side handler for the `ToggleVolume` action.
//!
//! Reads `Res<ActionState<WaveConductorAction>>` and, when `ToggleVolume`
//! transitions to `just_pressed`, pushes a `SetMuted` command flipping the
//! current target mute state.

use bevy::ecs::system::NonSendMut;
use bevy::prelude::*;
use leafwing_input_manager::prelude::*;

use crate::lifecycle::actions::WaveConductorAction;

use super::command::AudioCommand;
use super::ring::AudioCommandSender;
use super::state::AudioState;

/// `Update` system that translates `ToggleVolume` action presses into
/// `SetMuted` commands.
///
/// Uses `NonSendMut<AudioCommandSender>` because `rtrb::Producer` is not
/// `Sync`; see `ring` module docs.
pub fn handle_volume_toggle(
    actions: Res<'_, ActionState<WaveConductorAction>>,
    state: Res<'_, AudioState>,
    mut sender: NonSendMut<'_, AudioCommandSender>,
) {
    if actions.just_pressed(&WaveConductorAction::ToggleVolume) {
        // Reads the current target state (Res<AudioState>::muted), inverts it, and
        // pushes the new value. AudioState::muted is mirrored from the audio thread's
        // echo, so a rapid double-press could push the same direction twice if the
        // echo hasn't landed yet (the race window is ~1 frame). The brief mismatch
        // is acceptable — the V key is user-driven and the worst case is one missed
        // toggle that the user re-presses.
        let new_muted = !state.muted;
        if let Err(_dropped) = sender.push(AudioCommand::SetMuted(new_muted)) {
            tracing::warn!("audio command ring full; dropping SetMuted command");
        } else {
            tracing::info!(new_muted, "toggle volume → SetMuted");
        }
    }
}
