//! Commands (main → audio) and messages (audio → main).
//!
//! Both flow through `rtrb` ring buffers. The audio thread never allocates or
//! blocks **per buffer**, so message payloads are kept small and `Copy` where
//! possible (`AudioMessage::Errored` carries a `String` because errors are rare
//! and we accept the allocation when they occur).
//!
//! Note on `SetLineParam`/`SetDotsParam` `&'static str`: keeping the enum
//! `Copy` requires the parameter key to be `Copy`. A `&'static str` (string
//! literal) is `Copy` and zero-allocation; senders write keys like `"volume"`
//! or `"bandpass_freq"` directly. See [`super::line_synth::LineSynth`] and
//! [`super::dots_synth::DotsSynth`] for the legal key sets.

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
    /// Build and activate the Line sketch's synth voice graph. Idempotent: a
    /// second `AddLineSynth` while one is active is a no-op.
    ///
    /// The DSP graph is constructed on the audio thread the first time this
    /// command lands. Construction allocates (boxed graph nodes, parameter
    /// `Arc`s). This is a one-shot cost at sketch activation, not a per-buffer
    /// allocation, so it is acceptable on the audio thread.
    AddLineSynth,
    /// Stop the Line synth. Idempotent: a second `RemoveLineSynth` while no
    /// synth is active is a no-op. Drops the graph and its associated
    /// allocations.
    RemoveLineSynth,
    /// Set a named parameter on the Line synth. `key` is a `&'static str` to
    /// keep this variant `Copy` (the audio ring is lock-free and allocation-
    /// free; we cannot pass an owned `String`); see [`super::line_synth::LineSynth`]
    /// for the legal set. Unknown keys are logged via `tracing::warn!` and
    /// dropped silently — the DSP host must never panic on a stale key.
    SetLineParam {
        /// Parameter identifier. Must be a `'static` string literal.
        key: &'static str,
        /// New target value. Range and meaning depend on `key`.
        value: f32,
    },
    /// Build and activate the Dots sketch's synth voice graph. Idempotent: a
    /// second `AddDotsSynth` while one is active is a no-op.
    ///
    /// Construction allocates (boxed graph nodes, parameter `Arc`s) on the
    /// audio thread exactly once per sketch activation, not per buffer.
    AddDotsSynth,
    /// Stop the Dots synth. Idempotent: a second `RemoveDotsSynth` while no
    /// synth is active is a no-op. Drops the graph and its allocations.
    RemoveDotsSynth,
    /// Set a named parameter on the Dots synth. `key` is `&'static str` to
    /// keep this variant `Copy`; see [`super::dots_synth::DotsSynth`] for the
    /// legal set. Unknown keys are logged and dropped silently.
    SetDotsParam {
        /// Parameter identifier. Must be a `'static` string literal.
        key: &'static str,
        /// New target value. Range and meaning depend on `key`.
        value: f32,
    },
    /// Build and activate the Cymatics sketch's synth voice bundle. Idempotent:
    /// a second `AddCymaticsSynth` while voices are active is a no-op.
    ///
    /// Allocates (the `CymaticsSynth` graph plus `LoopVoice`/`OneShotVoice`
    /// structs) on the audio thread exactly once per sketch activation.
    AddCymaticsSynth,
    /// Stop the Cymatics voices. Idempotent: a second `RemoveCymaticsSynth`
    /// while no voices are active is a no-op. Drops the bundle and its
    /// associated allocations.
    RemoveCymaticsSynth,
    /// Set a named parameter on the Cymatics voice bundle. `key` is
    /// `&'static str` to keep this variant `Copy`; legal keys are
    /// `"osc_volume"`, `"osc_freq_scalar"` (→ synth), `"blub_volume"`,
    /// `"blub_rate"` (→ blub loop voice). Unknown keys are logged and dropped
    /// silently — the host never panics on a stale key.
    SetCymaticsParam {
        /// Parameter identifier. Must be a `'static` string literal.
        key: &'static str,
        /// New target value. Range and meaning depend on `key`.
        value: f32,
    },
    /// Trigger a one-shot Cymatics sample by ID. Re-triggering while a shot is
    /// still playing restarts it from the beginning. Fire-and-forget; no echo
    /// message is sent back.
    TriggerCymaticsSample(CymaticsSampleId),
    /// Build and activate the Flame sketch's synth voice graph. Idempotent: a
    /// second `AddFlameSynth` while one is active is a no-op.
    ///
    /// Construction allocates (the boxed `FlameSynth` graph plus its parameter
    /// `Shared` handles) on the audio thread exactly once per sketch activation,
    /// not per buffer.
    AddFlameSynth,
    /// Stop the Flame synth. Idempotent: a second `RemoveFlameSynth` while no
    /// synth is active is a no-op. Drops the graph and its allocations.
    RemoveFlameSynth,
    /// Set a named parameter on the Flame synth. `key` is `&'static str` to keep
    /// this variant `Copy`; see [`super::flame_synth::FlameSynth`] for the legal
    /// set. Unknown keys are logged and dropped silently — the host never panics
    /// on a stale key.
    SetFlameParam {
        /// Parameter identifier. Must be a `'static` string literal.
        key: &'static str,
        /// New target value. Range and meaning depend on `key`.
        value: f32,
    },
}

/// One-shot Cymatics sample identifiers (v4 `kick`/`risingbass`).
///
/// Used with [`AudioCommand::TriggerCymaticsSample`] to identify which
/// one-shot voice to fire. Both variants are `Copy` so the parent enum stays
/// `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CymaticsSampleId {
    /// Percussive kick on interaction onset.
    Kick,
    /// Rising bass swell on interaction onset.
    RisingBass,
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
    /// Sent after the audio thread applies an `AddLineSynth` command and
    /// successfully constructed the synth graph. Lets the main thread mirror
    /// activation state for UI.
    LineSynthActivated,
    /// Sent after the audio thread applies a `RemoveLineSynth` command.
    LineSynthDeactivated,
    /// Sent after the audio thread applies an `AddDotsSynth` command and
    /// successfully constructed the Dots synth graph.
    DotsSynthActivated,
    /// Sent after the audio thread applies a `RemoveDotsSynth` command.
    DotsSynthDeactivated,
    /// Sent after the audio thread applies an `AddCymaticsSynth` command and
    /// successfully constructed the Cymatics voice bundle.
    CymaticsSynthActivated,
    /// Sent after the audio thread applies a `RemoveCymaticsSynth` command.
    CymaticsSynthDeactivated,
    /// Sent after the audio thread applies an `AddFlameSynth` command and
    /// successfully constructed the Flame synth graph.
    FlameSynthActivated,
    /// Sent after the audio thread applies a `RemoveFlameSynth` command.
    FlameSynthDeactivated,
}
