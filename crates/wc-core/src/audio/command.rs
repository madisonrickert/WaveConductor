//! Commands (main → audio) and messages (audio → main).
//!
//! Both flow through `rtrb` ring buffers. The audio thread never allocates or
//! blocks, so message payloads are kept small and `Copy` where possible
//! (`AudioMessage::Errored` carries a `String` because errors are rare and
//! we accept the allocation when they occur).

/// Commands the main thread sends to the audio thread.
///
/// Every command is processed once per cpal callback; sustained command floods
/// may be coalesced (latest write wins) once that becomes a real concern.
#[derive(Debug, Clone, Copy)]
pub enum AudioCommand {
    /// Set the master volume. Clamped to `[0.0, 1.0]` by the audio thread.
    SetMasterVolume(f32),
    /// Set the muted flag. `true` overrides volume with zero output.
    SetMuted(bool),
}

/// Messages the audio thread sends back to the main thread.
///
/// The audio thread uses `try_push` for these; if the message ring is full
/// (the main thread is severely backlogged), messages are dropped. Status
/// messages (`StreamStarted`, `Errored`) are infrequent and effectively
/// guaranteed; per-buffer messages (`VolumeApplied`, `MutedApplied`) may drop.
#[derive(Debug, Clone)]
pub enum AudioMessage {
    /// Sent once when the cpal stream begins rendering.
    StreamStarted {
        /// Stream sample rate in Hz.
        sample_rate: u32,
        /// Stream channel count.
        channels: u16,
    },
    /// Sent if the cpal error callback fires. Carries the formatted error.
    Errored(String),
    /// Sent after the audio thread applies a `SetMasterVolume` command.
    /// Allows the main thread to update its mirror of the volume value.
    VolumeApplied(f32),
    /// Sent after the audio thread applies a `SetMuted` command.
    MutedApplied(bool),
}
