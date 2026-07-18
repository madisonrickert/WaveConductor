//! ONNX Runtime (`ort`) inference backend for the MediaPipe hand and BlazePose body pipelines.
//!
//! `OrtInference` is the sole concrete [`ModelInference`] implementation. It
//! registers a platform GPU execution provider so the conv-heavy palm and landmark
//! models run off the CPU: `CoreML` on macOS (GPU/Neural Engine; measured ~164 ms
//! CPU-only down to well under the 33 ms/frame budget at 30 Hz) and `DirectML` on
//! Windows (vendor-neutral DX12, covering AMD/Intel integrated GPUs). Other targets
//! (Linux) run on ONNX Runtime's CPU EP. ONNX Runtime partitions the graph and
//! places any op the EP cannot support back on the CPU — but that is *per-op
//! placement* fallback, not a safety net against an EP that fails at *commit*: a
//! GPU EP can register cleanly and then throw while fusing the graph (observed as
//! `DirectML` `DmlGraphFusionHelper` `0x80004005` on some AMD drivers), aborting
//! the whole load. `load` therefore retries on a fresh CPU-only session when the
//! accelerated commit fails, so a broken GPU EP degrades one model to CPU instead
//! of losing hand tracking entirely (see `load_with_ep_fallback`).
//!
//! (Plain code span, not an intra-doc link: `mod inference_ort;` in the parent
//! carries an outer doc comment, so rustdoc resolves this module's merged doc
//! fragments in the *parent's* scope, where a private item of this module is not
//! nameable. The links inside `load`'s own doc resolve normally.)
//!
//! `ort` ships the C++ ONNX Runtime as a prebuilt native binary downloaded at build
//! time (`download-binaries` feature). The binary is subject to the
//! CDLA-Permissive-2.0 license, already allowed in `deny.toml`.
//!
//! The same vendored `.onnx` models used throughout the pipeline work without
//! conversion; only the backend changes.

use std::fmt::Display;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use ort::ep::coreml::ComputeUnits;
#[cfg(target_os = "macos")]
use ort::ep::CoreML;
#[cfg(target_os = "windows")]
use ort::ep::DirectML;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use ort::ep::ExecutionProvider;
use ort::session::builder::{GraphOptimizationLevel, SessionBuilder};
use ort::session::Session;
use ort::value::TensorRef;

use super::{InferenceError, ModelInference, Tensor};
use crate::settings::HandTrackingBackend;

/// Backend label when the session runs on ONNX Runtime's CPU execution provider
/// (no GPU EP for this target, or the EP failed to register and fell back).
pub(crate) const BACKEND_CPU: &str = "ort/CPU";
/// Backend label when the macOS `CoreML` execution provider registered.
pub(crate) const BACKEND_COREML: &str = "ort/CoreML";
/// Backend label when the Windows `DirectML` execution provider registered.
pub(crate) const BACKEND_DIRECTML: &str = "ort/DirectML";

/// `ort`-backed inference for one ONNX model stage.
///
/// Output tensors are read back in the model's **declared output order** (not the
/// map iteration order), because the landmark stage's downstream selection is
/// index-based on that order: 0 image landmarks, 1 presence, 2 handedness,
/// 3 world landmarks.
pub struct OrtInference {
    session: Session,
    input_name: String,
    output_names: Vec<String>,
    backend: &'static str,
    /// Reused `i64` shape buffer for the input tensor view. The input shape is
    /// fixed per model, so refilling this in place each frame (rather than
    /// `collect`ing a fresh `Vec`) keeps `run` off the per-frame allocator.
    input_shape: Vec<i64>,
    /// This model's file name, for the demotion warning (`load` takes it as a
    /// borrow; a mid-session failure needs it long after `load` returned).
    model_name: String,
    /// The operator's EP preference, needed at *run* time because only
    /// [`HandTrackingBackend::Auto`] may demote — see [`should_demote_to_cpu`].
    backend_pref: HandTrackingBackend,
    /// The model bytes, retained **only** while a CPU demotion is still possible
    /// (`Auto` + currently accelerated), so [`Self::demote_to_cpu`] can re-commit
    /// the graph. ~4 MB per model; `None` — and the memory returned — for
    /// `ForceCpu`/`ForceGpu`, for a session already on the CPU EP, and immediately
    /// after a demotion is attempted, none of which can ever rebuild.
    model_bytes: Option<Vec<u8>>,
    /// Consecutive failed [`ModelInference::run`] calls. Reset to 0 by any success;
    /// the demotion trigger at
    /// [`INFERENCE_DEMOTE_AFTER_CONSECUTIVE_FAILURES`]. A `u32` increment on the
    /// failure path and a store on the success path — the healthy hot path stays
    /// allocation-free.
    consecutive_run_failures: u32,
    /// Whether the one-shot CPU demotion has been *used up* — set at load when
    /// there was never anywhere to fall back to, and set before the rebuild is
    /// attempted so that a rebuild which itself fails can never be retried.
    /// This is the anti-flapping latch: see [`Self::demote_to_cpu`].
    cpu_fallback_used: bool,
}

/// Consecutive failed inference runs before an `Auto` model demotes itself to the
/// CPU execution provider ([`should_demote_to_cpu`]).
///
/// **Why 10.** The trigger has to sit above every transient and below any outage a
/// human would notice, and inference runs at the worker's rate cap:
///
/// - *Above transients.* A single failed forward pass — a momentary GPU/ANE
///   resource hiccup, a frame that raced a display re-enumeration — must never tear
///   down and re-commit an ONNX graph. One is noise; ten in a row is not: a healthy
///   EP does not fail ten consecutive frames and then recover.
/// - *Below a human-visible outage.* At the 30 Hz inference cap, 10 consecutive
///   failures is ≈0.33 s of dead hand tracking before recovery starts. Even under
///   the 4 Hz idle throttle (`worker::IDLE_INFERENCE_HZ`, nobody watching) it is
///   2.5 s — and the alternative, today's behaviour, is *eight hours* of dead
///   tracking while the provider reports healthy.
///
/// The exact value is not delicate; anything in ~5–30 satisfies both. 10 is the
/// round number in the middle.
const INFERENCE_DEMOTE_AFTER_CONSECUTIVE_FAILURES: u32 = 10;

impl OrtInference {
    /// Load an ONNX model from its bytes, resolving its execution provider from
    /// `backend` (see `ep_plan`) and registering the platform GPU EP (see
    /// `register_accelerator`) when the plan calls for it.
    ///
    /// The EP is a *placement* preference, not a guarantee: ONNX Runtime moves
    /// individual unsupported ops to the CPU, but a GPU EP can still fail while
    /// fusing the graph at commit. On such a failure — unless the caller forced
    /// the GPU with [`HandTrackingBackend::ForceGpu`] — `load` rebuilds a fresh
    /// CPU-only session and returns `BACKEND_CPU`, so one broken EP never costs
    /// all hand tracking. `model_name` names the failing model in the warning.
    ///
    /// On macOS an accelerated commit failure is *first* treated as a poisoned
    /// on-disk `CoreML` artifact cache: the model's own cache directory is purged
    /// and the accelerated commit retried exactly once
    /// (`commit_accelerated_recovering_cache`) before any CPU degradation. The
    /// cache key is a pure function of the model bytes, so without that purge a
    /// corrupt entry fails identically on every launch — an unattended kiosk would
    /// run hand tracking on the CPU *forever* off one bad write. One purge turns
    /// that into one slow launch.
    ///
    /// [`HandTrackingBackend::ForceGpu`] is the strict counterpart, and it is
    /// strict about *both* ways an accelerator can go missing: a commit failure
    /// surfaces as an error, and so does a GPU EP that never registered at all
    /// (no DX12 device, a driver that refuses registration, or a target with no
    /// GPU EP compiled in). Otherwise the A/B control would quietly run the very
    /// CPU session it exists to rule out — see `accelerator_missing_under_force`.
    ///
    /// The session's CPU thread pool is capped to two intra-op threads with
    /// spin-waiting disabled (see `base_builder`).
    ///
    /// # Errors
    /// Returns [`InferenceError::Load`] if the session cannot be built or
    /// committed (and, under [`HandTrackingBackend::ForceGpu`], if the GPU EP
    /// fails to register or fails at commit), or if the model has no input.
    pub fn load(
        model_bytes: &[u8],
        backend: HandTrackingBackend,
        model_name: &str,
    ) -> Result<Self, InferenceError> {
        let plan = ep_plan(backend);
        let (session, backend_label) = if plan.try_accelerated {
            load_with_ep_fallback(
                model_name,
                platform_accelerator_label(),
                plan.allow_cpu_fallback,
                || {
                    commit_accelerated_recovering_cache(
                        model_name,
                        model_bytes,
                        plan.allow_cpu_fallback,
                    )
                },
                || commit_cpu(model_bytes),
            )?
        } else {
            commit_cpu(model_bytes)?
        };

        let (input_name, output_names) = session_io_names(&session)?;
        // Retain the model bytes only while a mid-session CPU demotion is still
        // reachable; otherwise drop them (and their ~4 MB) here.
        let demotable = cpu_demotion_possible(backend, backend_label);
        Ok(Self {
            session,
            input_name,
            output_names,
            backend: backend_label,
            input_shape: Vec::new(),
            model_name: model_name.to_owned(),
            backend_pref: backend,
            model_bytes: demotable.then(|| model_bytes.to_vec()),
            consecutive_run_failures: 0,
            cpu_fallback_used: !demotable,
        })
    }

    /// The inference backend this session registered: `"ort/CoreML"` (macOS) or
    /// `"ort/DirectML"` (Windows) when the platform GPU EP attached, or
    /// `BACKEND_CPU` (`"ort/CPU"`) when there is no GPU EP for the target or it
    /// fell back.
    ///
    /// This reflects registration success, not whole-graph placement: the EP may
    /// still partition unsupported ops back onto the CPU at commit time. To
    /// confirm what actually ran where on a given host, run with
    /// `ORT_LOG=verbose RUST_LOG=ort=trace` and read the node-placement dump.
    pub fn backend(&self) -> &'static str {
        self.backend
    }

    /// One forward pass, with no failure bookkeeping. [`ModelInference::run`] wraps
    /// this with the consecutive-failure counter and the CPU demotion.
    fn run_once(&mut self, input: &Tensor, out: &mut Vec<Tensor>) -> Result<(), InferenceError> {
        // Refill the reused i64 shape buffer in place (ort shapes are i64; our
        // usize dims convert infallibly for any realistic image/landmark tensor).
        self.input_shape.clear();
        for &d in &input.shape {
            self.input_shape.push(
                i64::try_from(d)
                    .map_err(|e| InferenceError::Run(format!("input dim overflow: {e}")))?,
            );
        }
        let in_tensor =
            TensorRef::from_array_view((self.input_shape.as_slice(), input.data.as_slice()))
                .map_err(run_err)?;

        let outputs = self
            .session
            .run(ort::inputs![self.input_name.as_str() => in_tensor])
            .map_err(run_err)?;

        // Reuse `out`: size it to the output count, then refill each tensor's
        // buffers in place, in declared order. `Shape` derefs to `[i64]`.
        out.truncate(self.output_names.len());
        while out.len() < self.output_names.len() {
            out.push(Tensor::default());
        }
        for (slot, name) in out.iter_mut().zip(&self.output_names) {
            let (shape, data) = outputs[name.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(run_err)?;
            slot.data.clear();
            slot.data.extend_from_slice(data);
            slot.shape.clear();
            for &d in shape.iter() {
                slot.shape.push(
                    usize::try_from(d)
                        .map_err(|e| InferenceError::Run(format!("bad output dim: {e}")))?,
                );
            }
        }
        Ok(())
    }

    /// Rebuild this model's session on the CPU execution provider, abandoning the
    /// accelerator for the rest of the process. Called **once**, on the failure
    /// edge, when [`should_demote_to_cpu`] says a persistent inference failure has
    /// outlived every transient explanation.
    ///
    /// Blocking and allocating (it re-commits an ONNX graph) and it runs on the
    /// inference worker thread, which `AGENTS.md` forbids allocating on in steady
    /// state. That is why this is a *one-shot edge*, not per-frame bookkeeping: the
    /// healthy path costs one `u32` store, the failing path one `u32` increment, and
    /// this function runs at most once in the life of the session.
    ///
    /// **No rebuild loop, by construction.** `cpu_fallback_used` is latched `true`
    /// *before* the rebuild is attempted and is never cleared, so:
    /// - a rebuild that **succeeds** leaves `backend == BACKEND_CPU`, and there is
    ///   nowhere further to fall — [`should_demote_to_cpu`] refuses forever;
    /// - a rebuild that **fails** leaves the broken accelerated session in place and
    ///   still refuses forever — the model degrades to per-frame errors exactly as
    ///   it did before this existed, rather than re-committing an ONNX graph on the
    ///   worker thread every ten frames for the rest of an 8-hour soak.
    ///
    /// This is the flap the audio supervisor hit, closed the same way: the recovery
    /// is one-way, so a source that fails, recovers, and fails again cannot pump it.
    fn demote_to_cpu(&mut self, err: &InferenceError) {
        // Latch FIRST — before anything that can fail — so no path re-enters.
        self.cpu_fallback_used = true;
        // `take`: a demotion consumes the retained bytes, returning their ~4 MB.
        let Some(bytes) = self.model_bytes.take() else {
            return;
        };
        tracing::warn!(
            model = %self.model_name,
            ep = self.backend,
            failures = self.consecutive_run_failures,
            %err,
            "inference has failed on the accelerated execution provider for \
             {INFERENCE_DEMOTE_AFTER_CONSECUTIVE_FAILURES} consecutive frames; rebuilding this \
             model on the CPU execution provider (a persistent EP failure at inference time is \
             otherwise unrecoverable — it would error every frame for the rest of the session \
             while the provider reported healthy)"
        );
        match commit_cpu(&bytes) {
            Ok((session, label)) => match session_io_names(&session) {
                Ok((input_name, output_names)) => {
                    self.session = session;
                    self.input_name = input_name;
                    self.output_names = output_names;
                    self.backend = label;
                    tracing::warn!(
                        model = %self.model_name,
                        backend = label,
                        "this model is now running on the CPU execution provider — hand tracking \
                         survives, but hotter and slower than a healthy GPU session; the settings \
                         panel's backend row shows the degraded mixed state"
                    );
                }
                Err(e) => tracing::error!(
                    model = %self.model_name,
                    "CPU rebuild committed but its input/output names could not be read ({e}); \
                     keeping the failing accelerated session — hand tracking is down for this model"
                ),
            },
            Err(e) => tracing::error!(
                model = %self.model_name,
                "CPU rebuild FAILED ({e}) after a persistent accelerated inference failure; \
                 there is no further fallback and none will be attempted — hand tracking is down \
                 for this model"
            ),
        }
    }
}

/// Whether this session could still demote itself to the CPU EP later: only an
/// [`HandTrackingBackend::Auto`] session that actually reached an accelerator has
/// anywhere to fall.
///
/// [`HandTrackingBackend::ForceGpu`] must **not** demote — it is the A/B control,
/// and it is *supposed* to fail loudly rather than quietly become the CPU run it
/// exists to rule out (the same reasoning as [`accelerator_missing_under_force`]).
/// [`HandTrackingBackend::ForceCpu`] and an `Auto` session that already landed on
/// [`BACKEND_CPU`] are on the CPU already.
fn cpu_demotion_possible(backend: HandTrackingBackend, label: &'static str) -> bool {
    ep_plan(backend).allow_cpu_fallback && label != BACKEND_CPU
}

/// Whether a persistently failing model should now be rebuilt on the CPU EP.
///
/// The whole inference-time demotion decision, as a pure function of the three
/// things it depends on — so it is unit-testable with no GPU EP present (there are
/// none in CI), exactly like [`ep_plan`]:
///
/// - `consecutive_failures`: only a *persistent* failure demotes
///   ([`INFERENCE_DEMOTE_AFTER_CONSECUTIVE_FAILURES`]); a one-off hiccup must never
///   tear down and re-commit an ONNX graph on the worker thread.
/// - `backend`: only [`HandTrackingBackend::Auto`] demotes (see
///   [`cpu_demotion_possible`]).
/// - `cpu_fallback_used`: the one-shot latch. Once the CPU rebuild has been
///   *attempted*, this is `true` forever, so no later failure can start another —
///   there is nowhere left to fall back to, and a rebuild loop on a continuously
///   failing model is the flapping hazard this guards.
fn should_demote_to_cpu(
    consecutive_failures: u32,
    backend: HandTrackingBackend,
    cpu_fallback_used: bool,
) -> bool {
    !cpu_fallback_used
        && ep_plan(backend).allow_cpu_fallback
        && consecutive_failures >= INFERENCE_DEMOTE_AFTER_CONSECUTIVE_FAILURES
}

/// The model's input name and its outputs in declared order.
///
/// Shared by [`OrtInference::load`] and the CPU rebuild in
/// [`OrtInference::demote_to_cpu`]: the rebuilt session is the same graph, but the
/// names are re-read from it rather than carried over, so the struct can never
/// describe a session it no longer holds.
fn session_io_names(session: &Session) -> Result<(String, Vec<String>), InferenceError> {
    let input_name = session
        .inputs()
        .first()
        .ok_or_else(|| InferenceError::Load("model has no inputs".into()))?
        .name()
        .to_owned();
    let output_names = session
        .outputs()
        .iter()
        .map(|o| o.name().to_owned())
        .collect();
    Ok((input_name, output_names))
}

/// How a [`HandTrackingBackend`] preference resolves into the two independent
/// load-time decisions: whether to attempt the platform GPU EP at all, and
/// whether a commit failure on that EP may rebuild on the CPU.
///
/// Split out as a plain data value so the mapping is unit-testable with no GPU EP
/// present (there are none in CI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EpPlan {
    /// Attempt the platform GPU EP (`false` only for
    /// [`HandTrackingBackend::ForceCpu`], which builds CPU-only from the start).
    try_accelerated: bool,
    /// On an accelerated commit failure, rebuild on the CPU EP (`false` for
    /// [`HandTrackingBackend::ForceGpu`], which surfaces the error instead).
    allow_cpu_fallback: bool,
}

/// Resolve a backend preference into an [`EpPlan`].
fn ep_plan(backend: HandTrackingBackend) -> EpPlan {
    match backend {
        HandTrackingBackend::Auto => EpPlan {
            try_accelerated: true,
            allow_cpu_fallback: true,
        },
        HandTrackingBackend::ForceGpu => EpPlan {
            try_accelerated: true,
            allow_cpu_fallback: false,
        },
        HandTrackingBackend::ForceCpu => EpPlan {
            try_accelerated: false,
            allow_cpu_fallback: false,
        },
    }
}

/// Try an accelerated session build+commit, and on error optionally rebuild on
/// the CPU EP, returning the committed session and the backend label that
/// actually took (the accelerated label on success, whatever `build_cpu` returns
/// on the fallback path).
///
/// `ep` is the accelerator that was attempted ([`platform_accelerator_label`]):
/// logging it makes the warning self-describing (`ep = "ort/DirectML"`) instead
/// of leaving the reader to infer the platform from the surrounding lines.
///
/// Generic over the session type `S` and error `E: Display` so the retry decision
/// is unit-testable without any GPU EP: a test passes closures returning `Ok`/`Err`
/// to drive every branch. `E: Display` is used only to render the failing EP's
/// error — which carries the exact failing node — into the warning, so the field
/// tester's "upload the log" workflow captures the diagnostic.
fn load_with_ep_fallback<S, E: Display>(
    model_name: &str,
    ep: &'static str,
    allow_cpu_fallback: bool,
    try_accelerated: impl FnOnce() -> Result<(S, &'static str), E>,
    build_cpu: impl FnOnce() -> Result<(S, &'static str), E>,
) -> Result<(S, &'static str), E> {
    match try_accelerated() {
        Ok(loaded) => Ok(loaded),
        Err(err) if allow_cpu_fallback => {
            tracing::warn!(
                model = model_name,
                ep,
                %err,
                "accelerated execution provider failed to commit the graph; \
                 rebuilding this model on the CPU execution provider"
            );
            build_cpu()
        }
        Err(err) => Err(err),
    }
}

/// The accelerator this target *would* register: `CoreML` on macOS, `DirectML`
/// on Windows, and [`BACKEND_CPU`] elsewhere (no GPU EP is compiled in, so the
/// "accelerated" path is a plain CPU session).
///
/// Known before registration is attempted, which is exactly when the warning in
/// [`load_with_ep_fallback`] needs it — [`register_accelerator`] only returns a
/// label on the paths that got far enough to have one.
const fn platform_accelerator_label() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        BACKEND_COREML
    }
    #[cfg(target_os = "windows")]
    {
        BACKEND_DIRECTML
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        BACKEND_CPU
    }
}

/// Whether a GPU EP the operator *forced* is in fact absent: the plan forbids a
/// CPU fallback, yet registration produced the CPU label.
///
/// [`register_accelerator`] fails soft — it warns and returns [`BACKEND_CPU`]
/// when the EP refuses to attach (no DX12 device, a driver that rejects
/// registration, or a target with no GPU EP at all). Under
/// [`HandTrackingBackend::Auto`] that is the intended safety net. Under
/// [`HandTrackingBackend::ForceGpu`] it is a lie by omission: the operator's
/// deliberate "no CPU" A/B control would commit and happily run an unaccelerated
/// session, reporting success. Forcing the GPU therefore fails on a *registration*
/// failure exactly as it does on a commit failure.
fn accelerator_missing_under_force(label: &str, allow_cpu_fallback: bool) -> bool {
    !allow_cpu_fallback && label == BACKEND_CPU
}

/// Build a `SessionBuilder` with the CPU-thread-pool options shared by every
/// execution provider.
///
/// Two sessions (palm + landmark) each own a pool; capping intra-op threads and
/// disabling spin-waiting stops idle inference from burning whole cores between
/// frames at our `<= 30 Hz` cadence. This is independent of EP/model format, so
/// both the accelerated and CPU-only builders start from it.
fn base_builder() -> Result<SessionBuilder, InferenceError> {
    Session::builder()
        .map_err(load_err)?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(load_err)?
        .with_intra_threads(2)
        .map_err(load_err)?
        .with_intra_op_spinning(false)
        .map_err(load_err)
}

/// Build the platform-accelerated `SessionBuilder` (EP registered, session
/// options applied), returning it with the backend label that registered.
///
/// On Windows this also applies the `DirectML` session options
/// (`configure_accelerator_session`); on Linux [`register_accelerator`] is a
/// no-op and the label is [`BACKEND_CPU`].
///
/// `allow_cpu_fallback` is the plan's fallback flag, and it also decides what an
/// EP that *fails to register* means: with a fallback allowed (`Auto`) a soft
/// landing on [`BACKEND_CPU`] is the safety net, so the session commits
/// unaccelerated; with it forbidden ([`HandTrackingBackend::ForceGpu`]) the
/// missing accelerator is an error rather than a silently unaccelerated session
/// reported as a success (see [`accelerator_missing_under_force`]).
///
/// Split from the commit so that **every** commit attempt starts from a *fresh*
/// builder: the first one, the post-cache-purge retry in
/// [`commit_accelerated_recovering_cache`], and the CPU rebuild in
/// [`OrtInference::load`]. That is not cosmetic — the purge deletes the very
/// directory this builder's `CoreML` EP was registered against, so the retry must
/// re-register the EP against the freshly re-created cache dir rather than reuse a
/// builder holding the stale one (and `ort` guarantees nothing about a builder
/// whose commit has already failed).
fn accelerated_builder(
    model_bytes: &[u8],
    allow_cpu_fallback: bool,
) -> Result<(SessionBuilder, &'static str), InferenceError> {
    let mut builder = base_builder()?;
    #[cfg(target_os = "windows")]
    {
        builder = configure_accelerator_session(builder)?;
    }
    // `Ok` registration means the EP attached to the session options, NOT that
    // every node runs on it — the graph is partitioned at commit and any
    // unsupported op still falls to the CPU. The label reflects registration, not
    // whole-graph placement (see [`OrtInference::backend`]).
    let label = register_accelerator(&mut builder, model_bytes);
    if accelerator_missing_under_force(label, allow_cpu_fallback) {
        return Err(InferenceError::Load(format!(
            "no GPU execution provider registered on this host (target accelerator: {}), \
             and the inference backend is pinned to ForceGpu — refusing to run the \
             unaccelerated session the operator ruled out; select Auto or ForceCpu",
            platform_accelerator_label()
        )));
    }
    Ok((builder, label))
}

/// Build the platform-accelerated session and commit it, returning the committed
/// session and the registered backend label.
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only reachable via the macOS-only retry_after_cache_purge cache-recovery path; \
                  no caller on non-macOS targets"
    )
)]
fn commit_accelerated(
    model_bytes: &[u8],
    allow_cpu_fallback: bool,
) -> Result<(Session, &'static str), InferenceError> {
    let (mut builder, label) = accelerated_builder(model_bytes, allow_cpu_fallback)?;
    let session = builder.commit_from_memory(model_bytes).map_err(load_err)?;
    Ok((session, label))
}

/// [`commit_accelerated`], with one macOS-only recovery step in front of the CPU
/// degradation: treat a commit failure as a **poisoned `CoreML` artifact cache**,
/// purge that one model's cache directory, and retry the accelerated commit
/// exactly once.
///
/// Why this exists: `coreml_cache_dir` keys the compiled-artifact cache by a
/// hash of the model bytes, so a corrupt entry produces the *same* commit failure
/// on every launch, forever (empirically: a garbled `.mlmodelc` fails session
/// creation with `Failed to create MLModel … Unable to load model`). Under
/// [`HandTrackingBackend::Auto`] the EP fallback would then dutifully degrade that
/// model to the CPU on every launch, and an unattended kiosk would run hand
/// tracking unaccelerated for the rest of its life with one startup `warn!` as the
/// only evidence. A poisoned cache is not hypothetical here — a *stale* one was the
/// root cause of the historical `output_features has no value` crash (see
/// `docs/runbooks/onnx-coreml-model-surgery.md`).
///
/// **Exactly one retry.** A retry *loop* against a genuinely broken EP would delete
/// and recompile the `CoreML` artifact on every launch forever; one retry
/// distinguishes "cache was poisoned, we recovered" from "the GPU EP is broken on
/// this box" and then lets the caller degrade.
///
/// The build-time failure that [`accelerated_builder`] raises (`ForceGpu` with no
/// accelerator registered) is deliberately **not** cache-recovered: no commit ran,
/// so no cache is implicated and there is nothing to purge.
///
/// Non-macOS targets have no EP artifact cache in this code, so this is a
/// transparent pass-through to [`commit_accelerated`].
fn commit_accelerated_recovering_cache(
    model_name: &str,
    model_bytes: &[u8],
    allow_cpu_fallback: bool,
) -> Result<(Session, &'static str), InferenceError> {
    let (mut builder, label) = accelerated_builder(model_bytes, allow_cpu_fallback)?;
    match builder.commit_from_memory(model_bytes).map_err(load_err) {
        Ok(session) => Ok((session, label)),
        Err(first_err) => {
            retry_after_cache_purge(model_name, model_bytes, allow_cpu_fallback, first_err)
        }
    }
}

/// Purge this model's `CoreML` artifact cache and retry the accelerated commit
/// once; on a second failure (or when there was no purgeable cache to blame)
/// return the error so [`load_with_ep_fallback`] can degrade to the CPU EP.
///
/// The three `warn!` lines here are the operator's whole diagnosis: they name the
/// model and the exact cache directory, say that it was purged, and say whether
/// the retry then *recovered the accelerator* or *failed again* — which is the
/// difference between "one bad cache write, self-healed" and "the GPU EP is
/// genuinely broken on this host, investigate the driver/OS".
#[cfg(target_os = "macos")]
fn retry_after_cache_purge(
    model_name: &str,
    model_bytes: &[u8],
    allow_cpu_fallback: bool,
    first_err: InferenceError,
) -> Result<(Session, &'static str), InferenceError> {
    let Some(dir) = purge_model_cache(model_bytes) else {
        // Nothing was purged — no cache directory (caching unavailable), or the
        // computed path failed the guard. Either way a poisoned cache cannot be
        // the explanation, so do not retry: degrade (or, under ForceGpu, fail).
        return Err(first_err);
    };
    tracing::warn!(
        model = model_name,
        cache_dir = %dir.display(),
        err = %first_err,
        "CoreML accelerated commit failed; purged this model's compiled-artifact cache \
         directory and retrying the accelerated commit once (a corrupt cache entry would \
         otherwise fail identically on every launch, degrading this model to the CPU forever)"
    );
    match commit_accelerated(model_bytes, allow_cpu_fallback) {
        Ok(loaded) => {
            tracing::warn!(
                model = model_name,
                cache_dir = %dir.display(),
                "CoreML cache was poisoned; the purge + recompile RECOVERED the accelerated \
                 session — this launch is slower (the artifact recompiled), later launches are not"
            );
            Ok(loaded)
        }
        Err(retry_err) => {
            tracing::warn!(
                model = model_name,
                cache_dir = %dir.display(),
                err = %retry_err,
                "CoreML accelerated commit failed AGAIN on a freshly purged cache — the GPU \
                 execution provider is broken on this host, not merely cache-poisoned; this \
                 model degrades to the CPU execution provider (unless pinned to ForceGpu, \
                 which fails loudly instead)"
            );
            Err(retry_err)
        }
    }
}

/// No on-disk EP artifact cache on this target: a commit failure is the final
/// word, and the caller degrades to the CPU EP exactly as before.
#[cfg(not(target_os = "macos"))]
fn retry_after_cache_purge(
    _model_name: &str,
    _model_bytes: &[u8],
    _allow_cpu_fallback: bool,
    first_err: InferenceError,
) -> Result<(Session, &'static str), InferenceError> {
    Err(first_err)
}

/// Build a CPU-only session (no GPU EP registered) and commit it.
///
/// Used both for [`HandTrackingBackend::ForceCpu`] and as the fallback when an
/// accelerated commit fails. Starts from a fresh [`base_builder`]: the failed
/// accelerated builder still has the GPU EP registered on it, so rebuilding — not
/// reusing — is what makes this session genuinely CPU-only.
fn commit_cpu(model_bytes: &[u8]) -> Result<(Session, &'static str), InferenceError> {
    let session = base_builder()?
        .commit_from_memory(model_bytes)
        .map_err(load_err)?;
    Ok((session, BACKEND_CPU))
}

/// Map an `ort` error to a model-load failure. Generic over the recovery
/// context `R` because rc.12's `SessionBuilder` error-recovery API parameterizes
/// `ort::Error<R>` by the value `.recover()` would hand back (`SessionBuilder`,
/// `Session`, or `()`), so a single non-generic closure can't span the call
/// sites here.
fn load_err<R>(e: ort::Error<R>) -> InferenceError {
    InferenceError::Load(e.to_string())
}

/// Map an `ort` error to an inference-run failure. Generic over the recovery
/// context for the same reason as [`load_err`].
fn run_err<R>(e: ort::Error<R>) -> InferenceError {
    InferenceError::Run(e.to_string())
}

/// Apply session options required by the platform accelerator before registering
/// the execution provider.
///
/// Windows `DirectML` rejects memory-pattern optimization and parallel graph
/// execution, so both are disabled explicitly here. Parallel execution is
/// already ONNX Runtime's default-off state, but spelling it out keeps the
/// `DirectML` contract close to the registration site and catches future builder
/// default changes. `CoreML` and CPU targets need no special session options.
#[cfg(target_os = "windows")]
fn configure_accelerator_session(
    builder: SessionBuilder,
) -> Result<SessionBuilder, InferenceError> {
    builder
        .with_parallel_execution(false)
        .map_err(load_err)?
        .with_memory_pattern(false)
        .map_err(load_err)
}

/// Register the macOS `CoreML` execution provider on `builder`, returning the
/// backend label (`CoreML` on success, CPU on a graceful fallback).
///
/// `CoreML` runs in its default `NeuralNetwork` model format. The newer
/// `MLProgram` format covers a few more ops in principle, but its stricter parser
/// rejects these vendored `MediaPipe` graphs at compile time: the build fails on a
/// fused `Conv` op with `Required param 'pad' is missing`. Even patched it only
/// reaches 27 `CoreML` partitions — worse than `NeuralNetwork`'s 6 once the palm
/// model's `PReLU` slopes are reshaped to the `[C, 1, 1]` shape the EP accepts — so
/// we stay on `NeuralNetwork`. `Core ML` places each segment on ANE/GPU/CPU itself,
/// and the compiled artifact is cached on disk per model ([`coreml_cache_dir`]) to
/// skip recompiling every launch.
#[cfg(target_os = "macos")]
fn register_accelerator(builder: &mut SessionBuilder, model_bytes: &[u8]) -> &'static str {
    let mut coreml = CoreML::default()
        // ALL lets Core ML place each segment on ANE/GPU/CPU as it sees fit (the
        // default, set explicitly). NeuralNetwork model format is kept deliberately
        // — see the doc comment: MLProgram fails to compile these vendored models.
        .with_compute_units(ComputeUnits::All);
    // Core ML compiles each model to a native artifact on first load; caching it on
    // disk avoids paying that compile every launch. A missing cache dir is
    // non-fatal — we just recompile each run.
    if let Some(cache) = coreml_cache_dir(model_bytes) {
        coreml = coreml.with_model_cache_dir(cache.display());
    }
    match coreml.register(builder) {
        Ok(()) => BACKEND_COREML,
        Err(e) => {
            tracing::warn!("CoreML EP registration failed; running on CPU: {e}");
            BACKEND_CPU
        }
    }
}

/// Register the Windows `DirectML` execution provider on `builder`, returning the
/// backend label (`DirectML` on success, CPU on a graceful fallback).
///
/// `DirectML` is the vendor-neutral DX12 EP, so one path accelerates AMD and Intel
/// integrated GPUs alike. Registration fails gracefully to the CPU EP on a host
/// with no DX12 device (e.g. a GPU-less CI runner).
#[cfg(target_os = "windows")]
fn register_accelerator(builder: &mut SessionBuilder, _model_bytes: &[u8]) -> &'static str {
    match DirectML::default().register(builder) {
        Ok(()) => BACKEND_DIRECTML,
        Err(e) => {
            tracing::warn!("DirectML EP registration failed; running on CPU: {e}");
            BACKEND_CPU
        }
    }
}

/// No GPU execution provider for this target (e.g. Linux): the session runs on
/// ONNX Runtime's CPU EP. `builder` is left untouched.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn register_accelerator(_builder: &mut SessionBuilder, _model_bytes: &[u8]) -> &'static str {
    BACKEND_CPU
}

/// Compute a stable per-model cache key from the model bytes.
///
/// ONNX Runtime's `CoreML` EP names its compiled-artifact subdirectory by a
/// model hash that does **not** change when only our model's initializers change:
/// the palm model's `PReLU` slope reshape (`[1,C,1,1]` → `[C,1,1]`, which moves
/// `PReLU` onto `CoreML` and collapses the graph from 30 partitions to 6) leaves
/// that EP-side key identical to the pre-reshape model's. Without our own
/// namespacing, a model update therefore loads the *previous* model's stale
/// compiled partition and fails at inference with `output_features has no value`.
/// Hashing the model bytes here lands every distinct model in its own directory,
/// so a changed model can never collide with a prior compile.
///
/// The hash only needs to be stable within a single binary (the same build that
/// wrote the cache reads it back), so a `std` hasher suffices and adds no
/// dependency. A toolchain change that alters the hash merely forces a one-time
/// recompile, which is harmless.
///
/// macOS-only: the on-disk EP artifact cache is a `CoreML` concern.
#[cfg(target_os = "macos")]
fn model_cache_key(model_bytes: &[u8]) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    model_bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(all(target_os = "macos", test))]
thread_local! {
    /// Test-only override of the `CoreML` cache root ([`coreml_cache_root`]).
    ///
    /// The on-disk cache is off by default under `cfg(test)` (see
    /// [`coreml_cache_root`]); the poisoned-cache recovery regression needs it on,
    /// so it points this at its own temp directory — never the real user cache.
    ///
    /// Deliberately **thread-local**, not a process global: under a plain
    /// `cargo test` (one process, tests on threads) a global would switch the
    /// cache on for every *other* concurrently-running test too, resurrecting
    /// exactly the parallel cache-population race [`coreml_cache_root`] documents.
    /// A thread-local is scoped to the one test that opted in, under either runner.
    static CACHE_ROOT_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Root directory holding every model's `CoreML` artifact cache:
/// `<user cache>/waveconductor/coreml-cache`. Each model gets exactly one
/// directory *directly* beneath it, named by [`model_cache_key`].
///
/// This is the single source of truth for that root. [`coreml_cache_dir`] (which
/// creates a model's directory) and [`purge_model_cache`] (which deletes it) both
/// derive from here, so the delete can never be aimed at a path the create never
/// produced — see [`is_purgeable_model_cache_dir`].
///
/// `None` under `cfg(test)` unless a test overrides it: the unit tests load the
/// same model from many parallel `nextest` processes, and ONNX Runtime's `CoreML`
/// EP is not safe against two of them populating the shared cache directory at
/// once (the loser of the move-into-place race fails with "an item with the same
/// name already exists"). A test loads each model once, so the cache buys nothing,
/// and skipping it also keeps tests out of the real user cache dir. Production
/// keeps the cache for fast startup, where each model is loaded exactly once.
#[cfg(target_os = "macos")]
fn coreml_cache_root() -> Option<PathBuf> {
    #[cfg(test)]
    let root = CACHE_ROOT_OVERRIDE.with(|root| root.borrow().clone());
    #[cfg(not(test))]
    let root = Some(
        dirs::cache_dir()?
            .join("waveconductor")
            .join("coreml-cache"),
    );
    root
}

/// Resolve the on-disk `CoreML` model-cache directory for a specific model
/// (`<coreml-cache-root>/<model-key>`), creating it if absent.
///
/// The per-model `<model-key>` ([`model_cache_key`]) is what makes reusing the
/// cache across model revisions safe — see that function for why a directory
/// shared between models corrupts after a model change.
///
/// Returns `None` when caching is disabled ([`coreml_cache_root`]), no cache dir
/// is available, or it cannot be created; the caller then loads without a cache
/// (recompiling the Core ML artifact each run) rather than failing.
#[cfg(target_os = "macos")]
fn coreml_cache_dir(model_bytes: &[u8]) -> Option<PathBuf> {
    let dir = coreml_cache_root()?.join(model_cache_key(model_bytes));
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(e) => {
            tracing::warn!("CoreML cache dir {} unavailable: {e}", dir.display());
            None
        }
    }
}

/// Whether `dir` is safe to hand to `remove_dir_all` as one model's `CoreML`
/// artifact cache. **The guard on a recursive delete under the user's cache
/// directory** — a bug here removes something it shouldn't, so it is a separate,
/// directly-tested predicate rather than an inline condition.
///
/// All three conditions must hold:
///
/// 1. `dir`'s parent is *exactly* `root` — one component below the cache root, so
///    neither the root itself nor anything deeper or outside it can be passed.
/// 2. `dir` has a real final component. `Path::file_name` is `None` for a path
///    ending in `..` or `/`, which is what stops a lexical `root/..` (whose
///    `parent()` *is* `root`) from qualifying.
/// 3. `dir` is a real directory *and not a symlink*: `symlink_metadata` does not
///    follow links, so a symlink planted in the cache root can never redirect the
///    delete at its target.
///
/// A path failing any of these is not deleted; the caller logs and degrades.
#[cfg(target_os = "macos")]
fn is_purgeable_model_cache_dir(dir: &Path, root: &Path) -> bool {
    dir.parent() == Some(root)
        && dir.file_name().is_some()
        && std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_dir())
}

/// Delete one model's `CoreML` artifact cache directory, returning the purged
/// path (`None` if nothing was purged).
///
/// The path is derived from [`coreml_cache_dir`] — *the same function that
/// created it* — never re-assembled by hand, and is then re-checked against
/// [`is_purgeable_model_cache_dir`] before the recursive delete. A path that fails
/// the guard is logged and left alone.
#[cfg(target_os = "macos")]
fn purge_model_cache(model_bytes: &[u8]) -> Option<PathBuf> {
    let root = coreml_cache_root()?;
    let dir = coreml_cache_dir(model_bytes)?;
    if !is_purgeable_model_cache_dir(&dir, &root) {
        tracing::warn!(
            "refusing to purge CoreML cache path {} — it is not a directory sitting directly \
             under the cache root {}; leaving it alone and degrading this model instead",
            dir.display(),
            root.display()
        );
        return None;
    }
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(e) => {
            tracing::warn!(
                "could not purge CoreML cache dir {}: {e}; degrading this model instead",
                dir.display()
            );
            None
        }
    }
}

impl ModelInference for OrtInference {
    /// Run one stage, and recover from a *persistently* failing execution provider
    /// by demoting this model to the CPU EP.
    ///
    /// Allocation-free on the steady-state hot path. The input is bound as a
    /// borrowed [`TensorRef`] view over the pipeline's reused per-frame input
    /// buffer (no input copy; each frame previously cloned ≈0.4 MB palm / ≈0.6 MB
    /// landmark f32). Each output is written into `out`, a buffer the caller owns
    /// and reuses across frames: `out` is grown/truncated to the model's output
    /// count once, then each tensor's `data`/`shape` is refilled in place
    /// (`clear` keeps capacity). After the first call neither the input shape, the
    /// output container, nor the output data allocates.
    ///
    /// Outputs are written in the model's **declared output order** (see the
    /// struct doc), which the landmark stage selects by index.
    ///
    /// **The recovery.** A GPU EP that fails at *inference* (rather than at commit,
    /// which [`OrtInference::load`] already handles) has until now been terminal:
    /// the worker counts the error, pushes it, and runs the identical failing
    /// forward pass on the next frame — every frame, for the whole session — while
    /// the provider still reports `Streaming`. That is the historical `CoreML`
    /// failure mode (`output_features has no value`; see
    /// `docs/runbooks/onnx-coreml-model-surgery.md`). So: a success resets the
    /// consecutive-failure counter; `INFERENCE_DEMOTE_AFTER_CONSECUTIVE_FAILURES`
    /// failures in a row on an [`HandTrackingBackend::Auto`] session
    /// (`should_demote_to_cpu`) rebuild the model on the CPU EP once
    /// (`OrtInference::demote_to_cpu`) and retry the failing frame on it. The
    /// demotion is one-way and one-shot, so there is no rebuild loop.
    ///
    /// The demotion is *not* laundered into looking healthy: [`Self::backend_label`]
    /// starts reporting `BACKEND_CPU`, which the provider folds into the mixed
    /// `"ort/CoreML+CPU"` label and the settings panel renders amber.
    fn run(&mut self, input: &Tensor, out: &mut Vec<Tensor>) -> Result<(), InferenceError> {
        match self.run_once(input, out) {
            Ok(()) => {
                // The whole cost of the healthy path: one store, no allocation.
                self.consecutive_run_failures = 0;
                Ok(())
            }
            Err(err) => {
                self.consecutive_run_failures = self.consecutive_run_failures.saturating_add(1);
                if !should_demote_to_cpu(
                    self.consecutive_run_failures,
                    self.backend_pref,
                    self.cpu_fallback_used,
                ) {
                    return Err(err);
                }
                self.demote_to_cpu(&err);
                // Retry this frame on the rebuilt session. If the rebuild failed,
                // `run_once` simply fails again on the old session — and the latch
                // in `demote_to_cpu` guarantees we never try to rebuild a second
                // time, however many frames keep failing.
                let retried = self.run_once(input, out);
                if retried.is_ok() {
                    self.consecutive_run_failures = 0;
                }
                retried
            }
        }
    }

    /// The EP this model is running on right now — `BACKEND_CPU` after a
    /// mid-session demotion, whatever [`OrtInference::load`] registered before one.
    fn backend_label(&self) -> Option<&'static str> {
        Some(self.backend)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg(target_os = "macos")]
    #[test]
    fn coreml_cache_key_is_per_model_and_deterministic() {
        // Regression: ONNX Runtime's CoreML EP reuses one on-disk cache key
        // across our model revisions, so after a model change it would serve the
        // previous model's stale compiled partition and fail at inference with
        // "output_features has no value" (observed when the PReLU slope reshape
        // collapsed the palm graph 30 -> 6 partitions against a 30-partition
        // cache). The cache directory must be namespaced by model content:
        // distinct bytes -> distinct key, identical bytes -> identical key.
        let v1 = model_cache_key(b"palm-model-rev-1");
        let v2 = model_cache_key(b"palm-model-rev-2");
        assert_ne!(
            v1, v2,
            "different model bytes must namespace to different cache keys"
        );
        assert_eq!(
            v1,
            model_cache_key(b"palm-model-rev-1"),
            "the same model bytes must map to the same cache key"
        );
    }

    /// Point this thread's `CoreML` artifact cache at `root` for the rest of the
    /// test (thread-local — see [`CACHE_ROOT_OVERRIDE`]).
    #[cfg(target_os = "macos")]
    fn use_cache_root(root: &Path) {
        CACHE_ROOT_OVERRIDE.with(|slot| *slot.borrow_mut() = Some(root.to_path_buf()));
    }

    /// Overwrite every regular file under `dir` with garbage, keeping the tree
    /// shape — the shape a half-written / truncated cache entry leaves behind.
    /// Returns how many files were poisoned.
    #[cfg(target_os = "macos")]
    fn poison_every_file(dir: &Path) -> usize {
        let mut poisoned = 0;
        let mut stack = vec![dir.to_path_buf()];
        while let Some(next) = stack.pop() {
            for entry in std::fs::read_dir(&next).expect("read cache dir") {
                let path = entry.expect("cache dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    std::fs::write(&path, b"POISONED").expect("poison cache file");
                    poisoned += 1;
                }
            }
        }
        poisoned
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_purge_guard_only_accepts_a_model_dir_directly_under_the_cache_root() {
        // This predicate gates a `remove_dir_all` under the user's cache
        // directory, so every way of aiming it somewhere else must be refused.
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = tmp.path().join("coreml-cache");
        let model = root.join("0123456789abcdef");
        let victim = tmp.path().join("precious"); // OUTSIDE the cache root
        std::fs::create_dir_all(model.join("0_dynamic_nn")).expect("model cache tree");
        std::fs::create_dir_all(&victim).expect("victim dir");
        std::fs::write(root.join("stray_file"), b"x").expect("stray file");
        std::os::unix::fs::symlink(&victim, root.join("evil_link")).expect("symlink");

        assert!(
            is_purgeable_model_cache_dir(&model, &root),
            "the real per-model cache dir is exactly what may be purged"
        );
        for (path, why) in [
            (root.clone(), "the cache root itself must never be deleted"),
            (
                model.join("0_dynamic_nn"),
                "a path deeper than one component below the root",
            ),
            (
                root.join(".."),
                "a lexical `..` escape — its parent() IS the root, so only the \
                 file_name check refuses it",
            ),
            (victim.clone(), "a path outside the cache root entirely"),
            (root.join("stray_file"), "a file, not a directory"),
            (
                root.join("evil_link"),
                "a symlink — the delete must never follow it to its target",
            ),
            (root.join("never_created"), "a path that does not exist"),
        ] {
            assert!(
                !is_purgeable_model_cache_dir(&path, &root),
                "{}: {why}",
                path.display()
            );
        }
        // The symlink's target survived the guard being asked about it.
        assert!(victim.is_dir(), "the guard must not touch anything");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_poisoned_coreml_cache_is_purged_and_the_accelerated_commit_recovers() {
        // The hole this closes: the CoreML cache key is a pure function of the
        // model bytes, so a corrupt entry fails the accelerated commit
        // IDENTICALLY on every launch (empirically: "Failed to create MLModel …
        // Unable to load model"). Under Auto the EP fallback would then degrade
        // this model to the CPU on every launch, forever — an unattended kiosk
        // running hand tracking unaccelerated for its whole life off one bad
        // write. The purge + single retry must recover the accelerator instead.
        //
        // Uses a temp cache root (thread-local override), never the user's.
        let tmp = tempfile::tempdir().expect("temp cache root");
        use_cache_root(tmp.path());
        let bytes = model_bytes("palm_detection.onnx");

        // Cold load: compiles the CoreML artifact and populates the cache.
        let cold = OrtInference::load(&bytes, HandTrackingBackend::Auto, "palm_detection.onnx")
            .expect("cold load");
        assert_eq!(
            cold.backend(),
            BACKEND_COREML,
            "premise: the cold load must reach CoreML, or this test proves nothing"
        );
        drop(cold);

        let dir = coreml_cache_dir(&bytes).expect("the cache dir the cold load wrote");
        let poisoned = poison_every_file(&dir);
        assert!(
            poisoned > 0,
            "the CoreML EP wrote no cache artifacts to {} — nothing to poison, so this \
             test would pass vacuously",
            dir.display()
        );

        // Warm load over the poisoned cache: the accelerated commit fails, the
        // cache dir is purged, and the retry recompiles and lands on CoreML.
        // Without the purge this would return BACKEND_CPU — every launch.
        let recovered =
            OrtInference::load(&bytes, HandTrackingBackend::Auto, "palm_detection.onnx")
                .expect("load over a poisoned cache");
        assert_eq!(
            recovered.backend(),
            BACKEND_COREML,
            "a poisoned CoreML cache must be purged and the accelerated commit retried — \
             degrading to the CPU here is the permanent-silent-CPU kiosk failure"
        );
    }

    /// The settings panel's degraded-backend verdict
    /// (`settings::panel_user::provider_status::backend_degradation`) is compiled
    /// on every target, including builds without this feature, so it cannot name
    /// these constants: it matches the CPU label as a literal and asks
    /// `input::provider::platform_has_gpu_execution_provider()` whether this host
    /// ever had an accelerator to lose. Both of those are restatements of things
    /// defined here, so pin them — if they drift, a kiosk with *both* models on the
    /// CPU stops showing the amber row and looks perfectly healthy again.
    #[test]
    fn the_labels_the_settings_panel_matches_on_still_hold() {
        assert_eq!(
            BACKEND_CPU, "ort/CPU",
            "the panel matches this label as a literal"
        );
        assert_eq!(
            platform_accelerator_label() != BACKEND_CPU,
            crate::input::provider::platform_has_gpu_execution_provider(),
            "the panel's platform-accelerator predicate must agree with this \
             module's per-target EP choice"
        );
    }

    #[test]
    fn ep_plan_maps_each_backend_preference() {
        // Auto: try the accelerator, allow a CPU rebuild if commit fails.
        assert_eq!(
            ep_plan(HandTrackingBackend::Auto),
            EpPlan {
                try_accelerated: true,
                allow_cpu_fallback: true
            }
        );
        // ForceGpu: try the accelerator, but never fall back (loud failure).
        assert_eq!(
            ep_plan(HandTrackingBackend::ForceGpu),
            EpPlan {
                try_accelerated: true,
                allow_cpu_fallback: false
            }
        );
        // ForceCpu: never register a GPU EP at all.
        assert_eq!(
            ep_plan(HandTrackingBackend::ForceCpu),
            EpPlan {
                try_accelerated: false,
                allow_cpu_fallback: false
            }
        );
    }

    #[test]
    fn ep_fallback_keeps_the_accelerated_result_on_success() {
        // On a successful accelerated commit the (session, label) pair is returned
        // unchanged and the CPU builder is never invoked. The GPU EP path is
        // unreachable in CI (no GPU tests), so the decision logic is exercised
        // with plain stand-in values instead of a real Session.
        let mut cpu_built = false;
        let (session, label) = load_with_ep_fallback::<u32, String>(
            "palm_detection.onnx",
            BACKEND_DIRECTML,
            true,
            || Ok((42, BACKEND_DIRECTML)),
            || {
                cpu_built = true;
                Ok((0, BACKEND_CPU))
            },
        )
        .expect("accelerated commit succeeds");
        assert_eq!(session, 42);
        assert_eq!(label, BACKEND_DIRECTML);
        assert!(
            !cpu_built,
            "CPU builder must not run when the accelerated path commits"
        );
    }

    #[test]
    fn ep_fallback_rebuilds_on_cpu_when_the_accelerated_commit_fails() {
        // This is the regression for the shipped bug: a DirectML fusion crash at
        // commit must degrade to the CPU EP, not abort the whole load.
        let (session, label) = load_with_ep_fallback::<u32, String>(
            "palm_detection.onnx",
            BACKEND_DIRECTML,
            true,
            || Err("80004005: DmlGraphFusionHelper".to_owned()),
            || Ok((7, BACKEND_CPU)),
        )
        .expect("cpu rebuild succeeds");
        assert_eq!(session, 7);
        assert_eq!(label, BACKEND_CPU);
    }

    #[test]
    fn ep_fallback_propagates_the_error_when_cpu_fallback_is_disallowed() {
        // ForceGpu semantics: a commit failure must surface as an error, and the
        // CPU builder must not run.
        let mut cpu_built = false;
        let result = load_with_ep_fallback::<u32, String>(
            "palm_detection.onnx",
            BACKEND_DIRECTML,
            false,
            || Err("commit failed".to_owned()),
            || {
                cpu_built = true;
                Ok((0, BACKEND_CPU))
            },
        );
        assert!(result.is_err());
        assert!(
            !cpu_built,
            "no CPU rebuild is attempted when fallback is disallowed"
        );
    }

    #[test]
    fn force_gpu_treats_a_failed_ep_registration_as_a_failure() {
        // `register_accelerator` fails soft: it warns and hands back the CPU
        // label when the EP will not attach (no DX12 device, a driver that
        // refuses registration). Under Auto that is the safety net; under
        // ForceGpu — the A/B *control* — committing that unaccelerated session
        // and reporting success would quietly run the very CPU path the operator
        // pinned the setting to rule out.
        assert!(
            accelerator_missing_under_force(BACKEND_CPU, false),
            "ForceGpu + no registered accelerator must fail, not run on the CPU"
        );
        assert!(
            !accelerator_missing_under_force(BACKEND_CPU, true),
            "Auto's soft landing on the CPU EP is the intended safety net"
        );
        for label in [BACKEND_COREML, BACKEND_DIRECTML] {
            assert!(
                !accelerator_missing_under_force(label, false),
                "{label} registered — ForceGpu is satisfied"
            );
        }
    }

    fn model_bytes(name: &str) -> Vec<u8> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/models/hand")
            .join(name);
        std::fs::read(path).expect("read vendored model")
    }

    #[test]
    fn backend_label_is_one_of_the_known_values() {
        // The backend label must be observable and one of the two known states,
        // so a silent CPU fallback (the 240% CPU symptom) is never hidden behind
        // an empty or bogus string in diagnostics.
        let model = OrtInference::load(
            &model_bytes("palm_detection.onnx"),
            HandTrackingBackend::Auto,
            "palm_detection.onnx",
        )
        .expect("load via ort");
        let backend = model.backend();
        // The label must be observable and one of the known states, so a silent
        // CPU fallback (the 240% CPU symptom) is never hidden behind a bogus string
        // in diagnostics. Which GPU EP is expected depends on the target: CoreML on
        // macOS (compiled in via the `coreml` ort feature; load succeeded above, so
        // anything else is a real registration regression), DirectML-or-CPU on
        // Windows (CPU when the runner has no DX12 device), CPU elsewhere.
        #[cfg(target_os = "macos")]
        assert_eq!(backend, BACKEND_COREML, "CoreML must register on macOS");
        #[cfg(target_os = "windows")]
        assert!(
            backend == BACKEND_DIRECTML || backend == BACKEND_CPU,
            "Windows backend must be DirectML or CPU fallback, got {backend:?}"
        );
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(
            backend, BACKEND_CPU,
            "non-accelerated targets use the CPU EP"
        );
    }

    #[test]
    fn force_cpu_loads_a_cpu_only_session() {
        // The seam `backend_label_is_one_of_the_known_values` does not cover:
        // that test pins Auto -> the platform accelerator, this one pins
        // ForceCpu -> the CPU EP, and together they pin `load`'s routing in both
        // directions. Without it, `load` could drop its `plan.try_accelerated`
        // dispatch entirely — always taking the fallback path — and every other
        // test would still pass while ForceCpu silently registered CoreML,
        // defeating the operator's only lever against a flaky GPU EP.
        //
        // Needs no GPU EP of its own (it asserts the *absence* of one), so it
        // runs anywhere the vendored model does.
        let model = OrtInference::load(
            &model_bytes("palm_detection.onnx"),
            HandTrackingBackend::ForceCpu,
            "palm_detection.onnx",
        )
        .expect("load via ort");
        assert_eq!(
            model.backend(),
            BACKEND_CPU,
            "ForceCpu must build a CPU-only session — no GPU EP may be registered"
        );
    }

    #[test]
    fn ort_palm_model_runs_and_emits_raw_box_and_score_tensors() {
        // The graph-surgeried palm detector: input [1,192,192,3] → raw
        // [1,2016,18] boxes + [1,2016,1] scores (anchor decode + NMS are done
        // in Rust, not in the graph). Proves ort loads and runs it in-crate.
        let mut model = OrtInference::load(
            &model_bytes("palm_detection.onnx"),
            HandTrackingBackend::Auto,
            "palm_detection.onnx",
        )
        .expect("load via ort");
        let mut out = Vec::new();
        model
            .run(&Tensor::zeros(vec![1, 192, 192, 3]), &mut out)
            .expect("ort palm forward pass");
        let shapes: Vec<&[usize]> = out.iter().map(|t| t.shape.as_slice()).collect();
        assert!(
            shapes.contains(&[1, 2016, 18].as_slice()),
            "shapes={shapes:?}"
        );
        assert!(
            shapes.contains(&[1, 2016, 1].as_slice()),
            "shapes={shapes:?}"
        );
    }

    #[test]
    fn ort_landmark_model_runs_and_emits_expected_shapes() {
        // The ort backend must yield the output set the pipeline selects by
        // declared index order: two [1,63] landmark tensors and two [1,1]
        // scalars. On a host without CoreML, ort falls back to CPU — still
        // exercising load + run + the declared-order shape extraction.
        let mut model = OrtInference::load(
            &model_bytes("hand_landmark.onnx"),
            HandTrackingBackend::Auto,
            "hand_landmark.onnx",
        )
        .expect("load via ort");
        let mut out = Vec::new();
        model
            .run(&Tensor::zeros(vec![1, 224, 224, 3]), &mut out)
            .expect("ort landmark forward pass");
        let shapes: Vec<&[usize]> = out.iter().map(|t| t.shape.as_slice()).collect();
        assert_eq!(out.len(), 4, "shapes={shapes:?}");
        // Positional: the pipeline selects by declared index order, so each
        // index must carry its declared shape — not merely the right multiset.
        assert_eq!(out[0].shape, vec![1, 63], "output 0: image landmarks");
        assert_eq!(out[1].shape, vec![1, 1], "output 1: presence");
        assert_eq!(out[2].shape, vec![1, 1], "output 2: handedness");
        assert_eq!(out[3].shape, vec![1, 63], "output 3: world landmarks");
    }

    #[test]
    fn ort_landmark_presence_is_a_probability_from_the_graph() {
        // Premise lock: the vendored hand_landmark.onnx applies a Sigmoid op to
        // the presence head INSIDE the graph, so declared output 1 is already a
        // probability and the pipeline must NOT sigmoid it again. An all-zeros
        // input contains no hand, so presence must read low. If a future model
        // swap ships raw logits instead (no baked-in activation), an empty
        // input's logit would be strongly negative — outside what this asserts
        // only by luck — while a logit-positive model or a non-[0,1] head fails
        // here loudly before the pipeline silently misreads it.
        //
        // The handedness head's baked-in sigmoid (declared output 2) is NOT
        // separately pinned here: proving it needs a hand-shaped input (an
        // empty frame says nothing about handedness either way). It is covered
        // at the mock level by the pipeline test
        // `handedness_probability_below_half_reads_left`.
        let mut model = OrtInference::load(
            &model_bytes("hand_landmark.onnx"),
            HandTrackingBackend::Auto,
            "hand_landmark.onnx",
        )
        .expect("load via ort");
        let mut out = Vec::new();
        model
            .run(&Tensor::zeros(vec![1, 224, 224, 3]), &mut out)
            .expect("ort landmark forward pass");
        assert_eq!(
            out[1].shape,
            vec![1, 1],
            "declared output 1 must be the presence scalar"
        );
        let presence = *out[1].data.first().expect("presence scalar");
        assert!(
            (0.0..=1.0).contains(&presence),
            "presence {presence} outside [0, 1] — model head is not pre-activated"
        );
        assert!(
            presence < 0.5,
            "presence {presence} on an empty (all-zeros) input should be < 0.5"
        );
    }

    #[test]
    fn demotion_needs_a_persistent_failure_on_an_auto_session_that_has_not_demoted() {
        use HandTrackingBackend::{Auto, ForceCpu, ForceGpu};
        const N: u32 = INFERENCE_DEMOTE_AFTER_CONSECUTIVE_FAILURES;

        // A transient must never tear down and re-commit an ONNX graph on the
        // worker thread: below the threshold, nothing happens.
        for failures in [0, 1, N - 1] {
            assert!(
                !should_demote_to_cpu(failures, Auto, false),
                "{failures} consecutive failures is a transient, not a broken EP"
            );
        }
        // A persistent failure on an Auto session that still has the CPU in
        // reserve: demote.
        for failures in [N, N + 1, u32::MAX] {
            assert!(
                should_demote_to_cpu(failures, Auto, false),
                "{failures} consecutive failures is a dead EP — rebuild on the CPU"
            );
        }
        // ForceGpu is the A/B *control*: it is supposed to fail loudly, not
        // quietly become the CPU session the operator pinned the setting to rule
        // out. ForceCpu is already on the CPU and can never reach this path.
        for backend in [ForceGpu, ForceCpu] {
            assert!(
                !should_demote_to_cpu(u32::MAX, backend, false),
                "{backend:?} must never demote — only Auto does"
            );
        }
        // The anti-flapping latch: once the one CPU rebuild has been ATTEMPTED
        // (whether it succeeded or failed), no later failure may start another.
        // Without this, a model that keeps failing would re-commit an ONNX graph
        // on the worker thread every N frames for the rest of an 8-hour soak.
        assert!(
            !should_demote_to_cpu(u32::MAX, Auto, true),
            "the CPU fallback is one-shot — a used latch must never rebuild again"
        );
    }

    #[test]
    fn only_an_accelerated_auto_session_retains_the_bytes_needed_to_demote() {
        // The retained model bytes (~4 MB) are the price of being able to rebuild
        // on the CPU; a session that can never demote must not pay it, and must
        // start with the latch already spent.
        assert!(cpu_demotion_possible(
            HandTrackingBackend::Auto,
            BACKEND_COREML
        ));
        assert!(cpu_demotion_possible(
            HandTrackingBackend::Auto,
            BACKEND_DIRECTML
        ));
        assert!(
            !cpu_demotion_possible(HandTrackingBackend::Auto, BACKEND_CPU),
            "already on the CPU EP — there is nowhere to fall back to"
        );
        assert!(
            !cpu_demotion_possible(HandTrackingBackend::ForceGpu, BACKEND_COREML),
            "ForceGpu must fail loudly, never silently degrade"
        );
        assert!(!cpu_demotion_possible(
            HandTrackingBackend::ForceCpu,
            BACKEND_CPU
        ));
    }

    #[test]
    fn a_persistently_failing_ep_demotes_the_model_to_the_cpu_and_keeps_tracking_alive() {
        // The hole this closes: an EP failure at INFERENCE time (the historical
        // CoreML `output_features has no value`) had zero coverage — the worker
        // would count the error and run the identical failing forward pass every
        // frame, forever, while the provider reported healthy. A persistent
        // failure must instead rebuild this model on the CPU EP, once.
        //
        // The persistent failure is induced with a shape ONNX Runtime rejects on
        // every call (a real `InferenceError::Run` out of `session.run`, not a
        // mock): the demotion path cannot tell one un-runnable session from
        // another, which is the point — it reacts to persistence, not to a
        // specific error string.
        let mut model = OrtInference::load(
            &model_bytes("hand_landmark.onnx"),
            HandTrackingBackend::Auto,
            "hand_landmark.onnx",
        )
        .expect("load via ort");
        let started_accelerated = model.backend() != BACKEND_CPU;

        let bad_input = Tensor::zeros(vec![1, 192, 192, 3]); // landmark wants 224²
        let mut out = Vec::new();
        for i in 0..INFERENCE_DEMOTE_AFTER_CONSECUTIVE_FAILURES {
            assert!(
                model.run(&bad_input, &mut out).is_err(),
                "the wrong-shape input must fail on every call (failure {i})"
            );
        }

        assert_eq!(
            model.backend(),
            BACKEND_CPU,
            "after {INFERENCE_DEMOTE_AFTER_CONSECUTIVE_FAILURES} consecutive inference \
             failures the model must have been rebuilt on the CPU EP"
        );
        assert_eq!(
            ModelInference::backend_label(&model),
            Some(BACKEND_CPU),
            "the demotion must be OBSERVABLE — this is what lights the settings \
             panel's amber degraded-backend row"
        );
        // Hand tracking survives the demotion: the rebuilt CPU session runs the
        // real input shape and yields the declared output set.
        model
            .run(&Tensor::zeros(vec![1, 224, 224, 3]), &mut out)
            .expect("the demoted CPU session must still run the model");
        assert_eq!(out.len(), 4, "the rebuilt session is the same graph");

        // No rebuild loop: the latch is spent, so a further persistent failure
        // cannot trigger a second re-commit (there is nowhere left to fall).
        assert!(
            model.cpu_fallback_used,
            "the one-shot CPU fallback must be latched after use"
        );
        assert!(
            model.model_bytes.is_none(),
            "the retained model bytes are released once the demotion is spent"
        );
        for _ in 0..(INFERENCE_DEMOTE_AFTER_CONSECUTIVE_FAILURES * 3) {
            assert!(model.run(&bad_input, &mut out).is_err());
        }
        assert!(
            !should_demote_to_cpu(
                model.consecutive_run_failures,
                model.backend_pref,
                model.cpu_fallback_used
            ),
            "a model that keeps failing after demotion must never rebuild again"
        );
        assert_eq!(model.backend(), BACKEND_CPU);

        // Sanity on hosts with a GPU EP (macOS here): the session really did start
        // accelerated, so the assertions above are about a genuine demotion rather
        // than a session that was on the CPU all along.
        #[cfg(target_os = "macos")]
        assert!(
            started_accelerated,
            "premise: macOS must load this model on CoreML, or this test proves nothing"
        );
        let _ = started_accelerated;
    }

    #[test]
    fn a_single_transient_failure_does_not_demote_or_stick() {
        // One bad frame must not tear down the session, and must not leave a
        // latent failure count that makes the next 9 bad frames demote a healthy
        // model: any success resets the counter.
        let mut model = OrtInference::load(
            &model_bytes("hand_landmark.onnx"),
            HandTrackingBackend::Auto,
            "hand_landmark.onnx",
        )
        .expect("load via ort");
        let loaded_on = model.backend();
        let mut out = Vec::new();

        assert!(model
            .run(&Tensor::zeros(vec![1, 192, 192, 3]), &mut out)
            .is_err());
        assert_eq!(model.consecutive_run_failures, 1);
        model
            .run(&Tensor::zeros(vec![1, 224, 224, 3]), &mut out)
            .expect("a good frame after a bad one");
        assert_eq!(
            model.consecutive_run_failures, 0,
            "a success must reset the consecutive-failure count"
        );
        assert_eq!(
            model.backend(),
            loaded_on,
            "a single transient failure must not rebuild the session"
        );
    }

    #[test]
    fn ort_run_rejects_wrong_input_shape() {
        // ONNX Runtime should return an error (not panic) when the input tensor
        // has a shape that disagrees with the model's declared input.
        let mut model = OrtInference::load(
            &model_bytes("hand_landmark.onnx"),
            HandTrackingBackend::Auto,
            "hand_landmark.onnx",
        )
        .expect("load via ort");
        // Landmark model expects [1,224,224,3]; supply a palm-sized input instead.
        let mut out = Vec::new();
        let err = model
            .run(&Tensor::zeros(vec![1, 192, 192, 3]), &mut out)
            .expect_err("shape mismatch should return an error");
        assert!(matches!(err, InferenceError::Run(_)));
    }
}
