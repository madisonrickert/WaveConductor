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
///
/// ## Why the sender is optional
///
/// It is `Option<NonSendMut<…>>` for two reasons, and both are failure modes this
/// system is on the critical path of:
///
/// 1. **A boot with no output device** (the kiosk powering on before its TV
///    wakes) installs no sender at all. This system is registered unconditionally
///    in `Update`, and a missing bare `NonSendMut` is a Bevy 0.19
///    `SystemParamValidationError` with `Severity::Panic` — it takes the whole
///    schedule down, so the app died on frame 1 of exactly the case the reconnect
///    machinery exists to recover from.
/// 2. **A dead stream mid-run.** `supervisor::supervise_audio` *removes* the
///    sender while the engine is `Reconnecting`, because a stream that is not
///    draining the command ring turns every push into a full-ring warning.
///
/// With no sender there is nothing to mute: the messages are still read (so the
/// cursor does not lag and replay a stale press at reconnect) and the press is
/// dropped. The kiosk is already silent.
pub fn handle_volume_toggle(
    mut actions: MessageReader<'_, '_, ActionInput>,
    state: Res<'_, AudioState>,
    sender: Option<NonSendMut<'_, AudioCommandSender>>,
) {
    // `emit_action_input` emits at most one matching `(action, phase)` per frame,
    // so `.any()` never leaves a relevant message unread. Read before the sender
    // check so an outage does not leave unread presses to replay on reconnect.
    let toggled = actions
        .read()
        .any(|a| a.action == WaveConductorAction::ToggleVolume && a.phase == ActionPhase::Pressed);
    let Some(mut sender) = sender else {
        if toggled {
            tracing::debug!("ToggleVolume pressed with no audio stream; nothing to mute");
        }
        return;
    };
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
