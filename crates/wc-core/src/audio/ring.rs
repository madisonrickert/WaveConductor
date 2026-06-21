//! Typed wrappers around `rtrb` ring buffers for audio command/message exchange.
//!
//! `rtrb` is single-producer / single-consumer and lock-free. The producer
//! and consumer ends are separated at construction so the audio callback can
//! own one end exclusively without any synchronization at runtime.
//!
//! ## Thread model
//!
//! `rtrb::Producer` and `rtrb::Consumer` are `Send` but not `Sync` — they use
//! `Cell<usize>` for their head/tail indices. Bevy's `Resource` trait requires
//! `Send + Sync`, so these wrappers are installed as **non-send resources**
//! (via `World::insert_non_send`) instead. Systems access them with
//! `bevy::ecs::system::NonSendMut` rather than `ResMut`; this is correct
//! because they should only ever be accessed from the main thread.

use super::command::{AudioCommand, AudioMessage};

/// Capacity for both rings. Audio callbacks run far faster than the main
/// thread; the main thread drains messages each tick and writes commands only
/// in response to discrete events (key presses, sketch transitions). 64 is
/// well over the steady-state need.
pub const RING_CAPACITY: usize = 64;

/// Producer end of the main → audio command ring.
///
/// Installed as a non-send resource (see module-level docs). Access via
/// `NonSendMut<AudioCommandSender>` in Bevy systems.
pub struct AudioCommandSender {
    producer: rtrb::Producer<AudioCommand>,
}

impl AudioCommandSender {
    /// Construct a sender from the producer half of an `rtrb` ring buffer.
    ///
    /// Typically called by the audio engine startup system; also available
    /// to test code that constructs rings manually without a real cpal stream.
    pub fn new(producer: rtrb::Producer<AudioCommand>) -> Self {
        Self { producer }
    }

    /// Push a command. Returns `Err` if the ring is full (the audio thread
    /// is severely backlogged); callers may choose to drop the command in
    /// that case.
    pub fn push(&mut self, command: AudioCommand) -> Result<(), AudioCommand> {
        self.producer
            .push(command)
            .map_err(|rtrb::PushError::Full(c)| c)
    }
}

/// Consumer end of the audio → main message ring.
///
/// Installed as a non-send resource (see module-level docs). The
/// [`super::state::pump_audio_messages`] system drains it each `PreUpdate`
/// via `NonSendMut<AudioMessageReceiver>`. Other systems should read
/// [`super::state::AudioState`] instead unless they need raw message access.
pub struct AudioMessageReceiver {
    consumer: rtrb::Consumer<AudioMessage>,
}

impl AudioMessageReceiver {
    /// Construct a receiver from the consumer half of an `rtrb` ring buffer.
    ///
    /// Typically called by the audio engine startup system; also available
    /// to test code that constructs rings manually without a real cpal stream.
    pub fn new(consumer: rtrb::Consumer<AudioMessage>) -> Self {
        Self { consumer }
    }

    /// Drain every message currently available, in FIFO order.
    pub fn drain(&mut self) -> impl Iterator<Item = AudioMessage> + '_ {
        std::iter::from_fn(|| self.consumer.pop().ok())
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::match_wildcard_for_single_variants,
    reason = "expect, panic, and wildcard match are appropriate in test code"
)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_command_via_ring() {
        let (producer, consumer) = rtrb::RingBuffer::<AudioCommand>::new(RING_CAPACITY);
        let mut sender = AudioCommandSender::new(producer);
        sender
            .push(AudioCommand::SetMasterVolume(0.5))
            .expect("push should succeed");

        // Audio thread (simulated) pops the command.
        let mut consumer = consumer;
        let cmd = consumer.pop().expect("command should be available");
        match cmd {
            AudioCommand::SetMasterVolume(v) => assert!((v - 0.5).abs() < f32::EPSILON),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn round_trip_message_via_drain() {
        let (mut producer, consumer) = rtrb::RingBuffer::<AudioMessage>::new(RING_CAPACITY);
        producer
            .push(AudioMessage::StreamStarted {
                sample_rate: 48_000,
                channels: 2,
            })
            .expect("push");
        producer
            .push(AudioMessage::MutedApplied(true))
            .expect("push");

        let mut receiver = AudioMessageReceiver::new(consumer);
        let messages: Vec<_> = receiver.drain().collect();
        assert_eq!(messages.len(), 2);

        match &messages[0] {
            AudioMessage::StreamStarted {
                sample_rate,
                channels,
            } => {
                assert_eq!(*sample_rate, 48_000);
                assert_eq!(*channels, 2);
            }
            other => panic!("unexpected message: {other:?}"),
        }
        match &messages[1] {
            AudioMessage::MutedApplied(m) => assert!(*m),
            other => panic!("unexpected message: {other:?}"),
        }
    }
}
