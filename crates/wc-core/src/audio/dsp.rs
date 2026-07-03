//! `DspHost` — the audio-thread DSP graph.
//!
//! Owns the master volume, mute state, and any active per-sketch voice
//! graphs; processes incoming [`AudioCommand`]s; renders samples into the
//! cpal output buffer. Fully unit-testable without any audio hardware:
//! construct, apply commands, render into a `Vec<f32>`, assert.
//!
//! ## Real-time invariants
//!
//! - `render` is allocation-free.
//! - `apply` is allocation-free **except** for `AddLineSynth`, `AddDotsSynth`,
//!   `AddCymaticsSynth`, and `AddFlameSynth`, each of which allocates a new
//!   voice graph or bundle exactly once per sketch activation. This is a
//!   one-shot cost at sketch boundaries, not a per-buffer allocation.
//! - `RemoveLineSynth` / `RemoveDotsSynth` / `RemoveCymaticsSynth` drop their
//!   graphs/bundles on the audio thread; deallocation is bounded (a handful of
//!   `Arc` and `Box` frees, no recursive structure).
//!
//! Plan 9 Phase A wires the Line synth voice graph; Phase B adds background
//! sample mixing via a named [`SampleBank`] and [`LoopVoice`]; Task C4 adds
//! the Cymatics voice bundle (`CymaticsVoices`) with its synth, looping blub,
//! and kick/risingbass one-shots.
//!
//! ## Background sample mixing
//!
//! The constructor accepts a [`SampleBank`] built from the decoded, resampled
//! assets. The looping background bed is addressed by name
//! ([`LINE_BACKGROUND_SAMPLE`]); its index is resolved once at construction.
//! Each render buffer the [`LoopVoice`] steps forward by one frame and wraps
//! automatically via `rem_euclid`. At `rate == 1.0` and an integer playhead
//! the fractional part is 0, so reads are bit-exact — identical to the old
//! `playhead: usize` path. The mix formula per frame is:
//!
//! ```text
//! out = gain · clamp(synth + line_background · background_volume, -1.0, 1.0)
//! ```
//!
//! The inner clamp prevents output overflow when both sources peak
//! simultaneously; the outer master `gain` is applied after so that muting
//! produces silence regardless of source levels.

use fundsp::shared::Shared;

use super::command::{AudioCommand, CymaticsSampleId};
use super::cymatics_synth::CymaticsSynth;
use super::dots_synth::DotsSynth;
use super::flame_synth::FlameSynth;
use super::line_synth::LineSynth;
use super::sample_bank::{LoopVoice, OneShotVoice, SampleBank};

/// Bank entry name for the looping background bed (the `line_background.ogg`
/// asset). Resolved to an index once at construction so the render loop never
/// does a string lookup per buffer.
pub const LINE_BACKGROUND_SAMPLE: &str = "line_background";

/// Default amplitude scalar applied to the background sample. Starts silent so
/// non-Line sketches cannot play the Line drone before Line has set its own
/// mixer parameter.
const DEFAULT_BACKGROUND_VOLUME: f32 = 0.0;

/// `SetLineParam` key that routes to the background-sample amplitude
/// scalar. Kept here rather than in `LineSynth::KNOWN_KEYS` because
/// background volume is a host-level mixer parameter, not a synth-graph
/// parameter.
const BACKGROUND_VOLUME_KEY: &str = "background_volume";

/// Bank entry name for the Cymatics percussive kick one-shot.
pub const CYMATICS_KICK: &str = "cymatics_kick";
/// Bank entry name for the Cymatics rising-bass one-shot.
pub const CYMATICS_RISINGBASS: &str = "cymatics_risingbass";
/// Bank entry name for the Cymatics looping blub voice.
pub const CYMATICS_BLUB: &str = "cymatics_blub";

/// Cymatics audio voices: synth, looping blub, and kick/risingbass one-shots.
///
/// Built on [`AudioCommand::AddCymaticsSynth`] and dropped on
/// [`AudioCommand::RemoveCymaticsSynth`]. All sample indices are resolved from
/// the bank at construction so the render loop never does a string lookup per
/// buffer.
struct CymaticsVoices {
    /// Six-oscillator + noise synth graph. Controlled via
    /// `SetCymaticsParam { key: "osc_volume" | "osc_freq_scalar" }`.
    synth: CymaticsSynth,
    /// Looping blub voice. Volume and rate are set via `SetCymaticsParam`.
    blub: LoopVoice,
    /// Resolved bank index for the blub sample, or `None` if absent.
    blub_idx: Option<usize>,
    /// Blub amplitude scalar. Clamped to `[0.0, 0.3]` on write (v4 range).
    blub_volume: f32,
    /// Blub playback rate. Clamped to `[0.5, 4.0]` on write (v4 range).
    blub_rate: f64,
    /// One-shot kick voice. Triggered via `TriggerCymaticsSample(Kick)`.
    kick: OneShotVoice,
    /// One-shot rising-bass voice. Triggered via `TriggerCymaticsSample(RisingBass)`.
    risingbass: OneShotVoice,
    /// Resolved bank index for the kick sample, or `None` if absent.
    kick_idx: Option<usize>,
    /// Resolved bank index for the risingbass sample, or `None` if absent.
    risingbass_idx: Option<usize>,
}

/// DSP host owned by the cpal audio thread.
///
/// All hot-path state is either plain `Copy`/`f32`, a `SampleBank` (immutable
/// after construction), or an `Option<Box<…>>` whose presence is checked once
/// per buffer.
///
/// `Debug` is hand-rolled because `fundsp::Shared` does not implement it;
/// the formatter prints the loaded value via `Shared::value()`.
pub struct DspHost {
    /// Output sample rate in Hz. Forwarded to `LineSynth::new` when a synth
    /// is added so its internal `set_sample_rate` is correct.
    sample_rate: u32,
    /// Output channel count. Used by `render` to splat the mono synth
    /// output across the interleaved buffer.
    channels: u16,
    volume: f32,
    muted: bool,
    /// Active Line voice graph, if any. `None` means the sketch is not
    /// loaded and the synth contributes nothing to the output mix.
    line_synth: Option<LineSynth>,
    /// Active Dots voice graph, if any. Independent of `line_synth`; in
    /// practice only one is active at a time, but both slots are summed
    /// when both happen to be present.
    dots_synth: Option<DotsSynth>,
    /// All decoded samples, immutable after construction.
    bank: SampleBank,
    /// Index of the looping background bed in `bank`, or `None` if absent.
    background_idx: Option<usize>,
    /// Looping background voice (plays `background_idx` at rate 1.0).
    background: LoopVoice,
    /// Background amplitude (the `background_volume` `SetLineParam` key).
    background_volume: Shared,
    /// Active Cymatics voice bundle, if any. `None` means the sketch is not
    /// loaded and the voices contribute nothing to the output mix.
    cymatics: Option<CymaticsVoices>,
    /// Active Flame voice graph, if any. `None` means the sketch is not loaded
    /// and the synth contributes nothing to the output mix.
    flame_synth: Option<FlameSynth>,
    /// Count of `Set*Param` commands received while the target voice was
    /// inactive (stale-param drops). Incremented on the audio thread via
    /// `saturating_add` instead of logging: `tracing::warn!` would take the
    /// `LogBuffer` mutex and allocate, both forbidden on the audio callback.
    /// Exposed via [`Self::stale_param_drops`] for tests and diagnostics.
    stale_param_drops: u64,
}

impl DspHost {
    /// Construct a default-silent host for the given output format.
    ///
    /// `bank` is the pre-decoded, pre-resampled named sample bank. The looping
    /// background bed is the entry named [`LINE_BACKGROUND_SAMPLE`]; if absent
    /// the background voice remains silent. Pass [`SampleBank::default`] to
    /// disable all sample playback.
    #[must_use]
    pub fn new(sample_rate: u32, channels: u16, bank: SampleBank) -> Self {
        let background_idx = bank.index_of(LINE_BACKGROUND_SAMPLE);
        let mut background = LoopVoice::silent();
        background.set_sample(background_idx);
        Self {
            sample_rate,
            channels,
            volume: 1.0,
            muted: false,
            line_synth: None,
            dots_synth: None,
            bank,
            background_idx,
            background,
            background_volume: Shared::new(DEFAULT_BACKGROUND_VOLUME),
            cymatics: None,
            flame_synth: None,
            stale_param_drops: 0,
        }
    }

    /// Apply a command. Clamps and validates values that the type system
    /// can't constrain (e.g., `SetMasterVolume` outside `[0, 1]`).
    ///
    /// `AddLineSynth` / `AddDotsSynth` / `AddCymaticsSynth` are idempotent
    /// (a second add while active is a no-op). Their `Remove*` counterparts
    /// are likewise idempotent. `Set*Param` commands received while the
    /// corresponding voices are inactive are dropped and tallied in
    /// [`Self::stale_param_drops`] (no logging on the audio thread — that
    /// would take a mutex and allocate); the host never panics on stale params.
    pub fn apply(&mut self, command: AudioCommand) {
        match command {
            AudioCommand::SetMasterVolume(v) => {
                self.volume = v.clamp(0.0, 1.0);
            }
            AudioCommand::SetMuted(m) => {
                self.muted = m;
            }
            AudioCommand::AddLineSynth => {
                if self.line_synth.is_none() {
                    self.line_synth = Some(LineSynth::new(f64::from(self.sample_rate)));
                }
            }
            AudioCommand::RemoveLineSynth => {
                self.line_synth = None;
            }
            AudioCommand::SetLineParam { key, value } => {
                // `background_volume` is a host-level mixer parameter
                // (it scales the LoopVoice this struct owns) and therefore
                // stays valid regardless of whether the synth is active.
                // Handle it here before delegating the rest to the synth's
                // per-graph parameter table.
                if key == BACKGROUND_VOLUME_KEY {
                    self.background_volume.set(value.max(0.0));
                } else if let Some(synth) = &self.line_synth {
                    synth.set_param(key, value);
                } else {
                    // No active LineSynth: drop and count. Logging here would
                    // take the tracing LogBuffer mutex and allocate on the
                    // audio thread (see `stale_param_drops`).
                    self.stale_param_drops = self.stale_param_drops.saturating_add(1);
                }
            }
            AudioCommand::AddDotsSynth => {
                if self.dots_synth.is_none() {
                    self.dots_synth = Some(DotsSynth::new(f64::from(self.sample_rate)));
                }
            }
            AudioCommand::RemoveDotsSynth => {
                self.dots_synth = None;
            }
            AudioCommand::SetDotsParam { key, value } => {
                if let Some(synth) = &self.dots_synth {
                    synth.set_param(key, value);
                } else {
                    // No active DotsSynth: drop and count (no audio-thread log).
                    self.stale_param_drops = self.stale_param_drops.saturating_add(1);
                }
            }
            AudioCommand::AddCymaticsSynth => self.activate_cymatics(),
            AudioCommand::RemoveCymaticsSynth => {
                self.cymatics = None;
            }
            AudioCommand::SetCymaticsParam { key, value } => {
                if let Some(c) = &mut self.cymatics {
                    match key {
                        // Host-level loop-voice params; clamped to v4 ranges.
                        "blub_volume" => c.blub_volume = value.clamp(0.0, 0.3),
                        "blub_rate" => c.blub_rate = f64::from(value.clamp(0.5, 4.0)),
                        // Synth-graph params forwarded to CymaticsSynth.
                        _ => c.synth.set_param(key, value),
                    }
                } else {
                    // No active Cymatics voices: drop and count (no audio-thread log).
                    self.stale_param_drops = self.stale_param_drops.saturating_add(1);
                }
            }
            AudioCommand::AddFlameSynth => {
                if self.flame_synth.is_none() {
                    self.flame_synth = Some(FlameSynth::new(f64::from(self.sample_rate)));
                }
            }
            AudioCommand::RemoveFlameSynth => {
                self.flame_synth = None;
            }
            AudioCommand::SetFlameParam { key, value } => {
                // `FlameSynth::set_param` is `&mut` (it carries the v4 one-pole
                // mapping accumulators), so match with `&mut self.flame_synth`.
                if let Some(synth) = &mut self.flame_synth {
                    synth.set_param(key, value);
                } else {
                    // No active FlameSynth: drop and count (no audio-thread log).
                    self.stale_param_drops = self.stale_param_drops.saturating_add(1);
                }
            }
            AudioCommand::TriggerCymaticsSample(id) => {
                if let Some(c) = &mut self.cymatics {
                    match id {
                        CymaticsSampleId::Kick => {
                            if let Some(i) = c.kick_idx {
                                c.kick.trigger(i);
                            }
                        }
                        CymaticsSampleId::RisingBass => {
                            if let Some(i) = c.risingbass_idx {
                                c.risingbass.trigger(i);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Activate the Cymatics voice bundle if not already active. Idempotent.
    ///
    /// Resolves the three sample indices from the bank once (string lookup),
    /// then allocates the synth graph and voice structs. Called only from the
    /// `AddCymaticsSynth` match arm; extracted so `apply` stays under the
    /// function-length lint threshold.
    fn activate_cymatics(&mut self) {
        if self.cymatics.is_some() {
            return;
        }
        // Resolve indices once at activation; the render loop fetches bank refs
        // per buffer using these Copy indices (no per-frame string lookup).
        let blub_idx = self.bank.index_of(CYMATICS_BLUB);
        let kick_idx = self.bank.index_of(CYMATICS_KICK);
        let risingbass_idx = self.bank.index_of(CYMATICS_RISINGBASS);
        let mut blub = LoopVoice::silent();
        blub.set_sample(blub_idx);
        self.cymatics = Some(CymaticsVoices {
            synth: CymaticsSynth::new(f64::from(self.sample_rate)),
            blub,
            blub_idx,
            blub_volume: 0.0,
            blub_rate: 1.0,
            kick: OneShotVoice::silent(),
            risingbass: OneShotVoice::silent(),
            kick_idx,
            risingbass_idx,
        });
    }

    /// Current master volume in `[0.0, 1.0]`. Cached for status reporting.
    #[must_use]
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Current mute state.
    #[must_use]
    pub fn muted(&self) -> bool {
        self.muted
    }

    /// True if a Line voice graph is currently active.
    #[must_use]
    pub fn line_synth_active(&self) -> bool {
        self.line_synth.is_some()
    }

    /// True if a Dots voice graph is currently active.
    #[must_use]
    pub fn dots_synth_active(&self) -> bool {
        self.dots_synth.is_some()
    }

    /// True if a Flame voice graph is currently active.
    #[must_use]
    pub fn flame_synth_active(&self) -> bool {
        self.flame_synth.is_some()
    }

    /// Current Line background-sample amplitude scalar. Reflects the most
    /// recent `SetLineParam { key: "background_volume" }` write the audio
    /// thread has observed.
    #[must_use]
    pub fn background_volume(&self) -> f32 {
        self.background_volume.value()
    }

    /// True if a background sample entry is present in the bank. When false,
    /// the background voice contributes silence regardless of volume.
    #[must_use]
    pub fn has_background(&self) -> bool {
        self.background_idx.is_some()
    }

    /// Number of `Set*Param` commands dropped because their target voice was
    /// inactive, accumulated on the audio thread. Saturating: it never wraps
    /// or panics. Non-zero means the main thread sent params for a synth that
    /// was not (yet) active — usually a benign ordering race at sketch
    /// activation, not a defect. Read directly in tests; a future diagnostics
    /// path can surface it over the message ring.
    #[must_use]
    pub fn stale_param_drops(&self) -> u64 {
        self.stale_param_drops
    }

    /// Render samples into `output`.
    ///
    /// `output` is a flat slice in cpal's interleaved layout: `[L, R, L, R, …]`
    /// for stereo. The buffer length is divisible by `channels`.
    ///
    /// Mixing per frame:
    /// 1. Tick the Line synth for one mono sample (zero if not active).
    /// 2. Tick the Dots synth for one mono sample (zero if not active).
    /// 3. Zero the frame, then add the Line background bed via
    ///    [`LoopVoice::mix_frame`] (scaled by `background_volume`). Zeroing
    ///    first means cpal's potentially uninitialized output buffer never
    ///    leaks into the mix, and tests can pass arbitrary initial values and
    ///    still get deterministic results.
    /// 4. If Cymatics is active, tick its synth, add the blub loop, and add any
    ///    in-flight kick/risingbass one-shots — all accumulating into the frame.
    /// 5. Broadcast the summed Line+Dots+Flame synth sample across channels, add
    ///    to the accumulated frame, **clamp once** to `[-1.0, 1.0]`.
    /// 6. Multiply by master `gain` (`muted ? 0 : volume`).
    ///
    /// The single clamp in step 5 covers all sources (background, Cymatics
    /// voices, Line, Dots). There is no second clamp. The background
    /// [`LoopVoice`] advances its playhead by one frame per output frame,
    /// wrapping at the sample boundary via `rem_euclid`. At `rate == 1.0` the
    /// fractional part is 0 for integer playheads, giving bit-exact reads
    /// identical to the old `playhead: usize` path.
    pub fn render(&mut self, output: &mut [f32]) {
        let gain = if self.muted { 0.0 } else { self.volume };
        // `u16 → usize` is infallible on every target we support.
        let channels = usize::from(self.channels.max(1));
        // Atomic load once per buffer; smoothing deferred to the
        // reactivity-coupling layer (fires at most once per visual frame).
        let bg_volume = self.background_volume.value();
        // Disjoint-field borrow: resolve immutable bank references before the
        // loop that mutably borrows voice fields. `self.bank` and voice fields
        // (`self.background`, `self.cymatics`) are distinct struct fields, so
        // the borrow checker accepts coexisting borrows on them.
        let bg_sample = self.background_idx.and_then(|i| self.bank.sample(i));
        // Pre-store Cymatics sample indices (Copy usize) so the render loop can
        // fetch bank refs inside the loop using only `self.bank` (immutable) and
        // `self.cymatics` (mutably borrowed as the voice owner) — disjoint fields.
        let cym_blub_idx = self.cymatics.as_ref().and_then(|c| c.blub_idx);
        let cym_kick_idx = self.cymatics.as_ref().and_then(|c| c.kick_idx);
        let cym_rb_idx = self.cymatics.as_ref().and_then(|c| c.risingbass_idx);

        for frame in output.chunks_mut(channels) {
            // Tick synths before touching the frame (no interdependence).
            let line_sample = self.line_synth.as_mut().map_or(0.0, LineSynth::tick_mono);
            let dots_sample = self.dots_synth.as_mut().map_or(0.0, DotsSynth::tick_mono);
            let flame_sample = self.flame_synth.as_mut().map_or(0.0, FlameSynth::tick_mono);
            let synth_sample = line_sample + dots_sample + flame_sample;
            // Zero the frame before all additive sources accumulate into it.
            for slot in frame.iter_mut() {
                *slot = 0.0;
            }
            // Background bed: one interpolated frame at rate 1.0.
            self.background.mix_frame(bg_sample, frame, bg_volume, 1.0);
            // Cymatics voices: synth (mono broadcast) + blub loop + one-shots.
            // `self.bank` and `self.cymatics` are disjoint fields; the borrow
            // checker accepts the immutable bank ref alongside &mut self.cymatics.
            if let Some(c) = &mut self.cymatics {
                let cym = c.synth.tick_mono();
                for slot in frame.iter_mut() {
                    *slot += cym;
                }
                // Extract Copy fields before mix_frame calls to avoid borrowing
                // `c` both for the field values and the mutable voice methods.
                let blub_vol = c.blub_volume;
                let blub_rate = c.blub_rate;
                c.blub.mix_frame(
                    cym_blub_idx.and_then(|i| self.bank.sample(i)),
                    frame,
                    blub_vol,
                    blub_rate,
                );
                c.kick
                    .mix_frame(cym_kick_idx.and_then(|i| self.bank.sample(i)), frame, 1.0);
                c.risingbass
                    .mix_frame(cym_rb_idx.and_then(|i| self.bank.sample(i)), frame, 1.0);
            }
            // Single clamp across ALL sources (background + cymatics + Line/Dots),
            // then master gain. No second clamp anywhere in the pipeline.
            for slot in frame.iter_mut() {
                *slot = (synth_sample + *slot).clamp(-1.0, 1.0) * gain;
            }
        }
    }
}

impl core::fmt::Debug for DspHost {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DspHost")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("volume", &self.volume)
            .field("muted", &self.muted)
            .field("line_synth_active", &self.line_synth.is_some())
            .field("dots_synth_active", &self.dots_synth.is_some())
            .field("cymatics_active", &self.cymatics.is_some())
            .field("flame_synth_active", &self.flame_synth.is_some())
            .field("background_idx", &self.background_idx)
            .field("background_playhead", &self.background.playhead)
            .field("background_volume", &self.background_volume.value())
            .field("stale_param_drops", &self.stale_param_drops)
            // `bank` is intentionally omitted: printing all decoded PCM data
            // would make debug output extremely large. Use `background_idx` to
            // identify which bank entry is active.
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "dsp_tests.rs"]
mod tests;
