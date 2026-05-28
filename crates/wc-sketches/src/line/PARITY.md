# Line — Parity Record

**Parity target:** Perceptual

**Reference media:** v4 main branch, `src/sketches/line/screenshots/gravity4_cropped.png` and the festival-loop recording from `scenarios/festival-loop.toml` at Pi Party 2026-03 timestamp.

**Plan progression toward parity:**

- **Plan 6 (shipped, tag `v5-line`)** — sketch scaffolding, single-attractor inverse-linear gravity, flat-color quads.
- **Plan 7 (shipped, tag `v5-line-sim`)** — multi-attractor physics with dual drag, size-scaled gravity, mouse-power decay, `original_xy` + constrain-to-box, fade-in α, horizontal-line spawn with sawtooth jitter, `particle_density` setting.
- **Plan 8 (shipped, tag `v5-line-render`)** — gravity-smear post-process shader, star sprite, attractor ring meshes, `gamma` setting.
- **Plan 9 (shipped, tag `v5-line-audio`)** — fundsp-based synthesis (bandpass cascade + LFO + noise + master gain mixed with looped `line_background.ogg`), `ParticleStats` CPU reduction over `LineCpuMirror`, coupling system writing per-frame `SetLineParam` (lfo_freq/bandpass_freq/noise_freq/volume) and overriding `LinePostParams.g_constant` + `i_mouse_factor` from groupedUpness × triangle-wave.
- **Plan 10 (shipped, parity gaps deferred to Plan 11)** — heatmap-image spawn, soak harness, manual-testing fixes (PNG feature, OGG path via `CARGO_MANIFEST_DIR`, black `ClearColor`, dep trim of unused Bevy features). Originally scoped to also sign the verdict, but the first hands-on run on 2026-05-25 surfaced four parity gaps that don't fit "polish" (see Verdict section below). Untagged at this state; the `v5-line-parity` tag is reserved for the Plan 11 closure.
  - **Phase A** — heatmap-image spawn template ported from v4's
    `src/sketches/line/heatmapSampler.ts`. New `LineSettings.spawn_template:
    String` field (Text setting, `requires_restart`); empty = horizontal-line
    layout (v5-line-sim baseline), non-empty = PNG path passed to
    `heatmap::sample_from_heatmap` which builds a CDF over luminance × alpha
    and inverse-CDF-samples `count` window-space positions. Errors (missing
    file, undecodable, all-zero weight) fall back to the horizontal layout.
  - **Phase B** — 8-hour soak harness at
    `crates/wc-sketches/tests/line_soak.rs`. `#[ignore]`-d so normal CI does
    not run it; Madison invokes it before tagging a release via
    `cargo test --release -p wc-sketches --test line_soak -- --ignored line_soak_8h`.
    Drives ~1.7M `app.update()` ticks with synthetic cursor motion and a
    press/release cycle, asserts the sketch stays in `AppState::Line`.
    Runs under `MinimalPlugins`, so the renderer is not exercised; a
    `DefaultPlugins` variant is reserved for Plan 11+.

**Approved deviations from v4** (carried forward):

- WGSL compute kernel replaces CPU-side `particleSystem.ts` for rendering; a parallel Rust CPU mirror runs the same math on the host (introduced in Plan 7) to feed `ParticleStats` in Plan 9 without a GPU readback stall. The two integrators may drift by ≤1% over long timescales due to floating-point order-of-operations differences; acceptable for groupedUpness and friends.

## Phase F approximation note (audio coupling)

Plan 11 Phase F replaced the per-frame `ParticleStats` reduction over
`LineCpuMirror` with smoothed CPU envelopes driven by attractor state. The
audio coupling no longer reads per-particle state; it reads attack/release
envelopes keyed on `MouseAttractorState.power` events.

**Approved deviation**: audio output is no longer mathematically equivalent
to v4's `computeStats`-driven reactivity. It IS perceptually equivalent:
the same musical shape (rising on press, sustained during hold, decaying
after release) at near-zero CPU cost. Tuning constants in
`crates/wc-sketches/src/line/particle_stats.rs` shape the response; adjust
during sign-off if specific synth voices sound off.

This brings Line in line with Cymatics (v4)'s architectural pattern: audio
derives from CPU-side simulation inputs, never from GPU-side simulation
outputs. Future GPU-compute sketches should follow the same pattern.

## Verdict

**Status:** PENDING — verdict deferred to Plan 11.

Plan 10 landed the bulk of the parity work: multi-attractor physics, the gravity-smear post-process, the fundsp synthesis graph plus `line_background.ogg` loop, the audio↔visual reactivity coupling, and the heatmap-image spawn template — covered by automated integration tests (`crates/wc-sketches/tests/line_input.rs`, `crates/wc-sketches/tests/line_lifecycle.rs`) and a `#[ignore]`-d 8-hour soak (`crates/wc-sketches/tests/line_soak.rs`).

The first hands-on run on 2026-05-25 surfaced four parity gaps that Plan 11 will close before signing:

1. **Attractor ring rotation invisible.** `bevy::math::primitives::Annulus` is rotationally symmetric, so the per-frame `(10 - idx) / 20 * power` rotation has no visible effect. v4's rings appear visibly spinning. Plan 11 swaps to a low-segment polygon (`RegularPolygon` with 6–8 sides) or a stroked custom mesh whose corners make rotation legible.
2. **Touch and hand-tracking cannot activate the attractor.** `update_mouse_attractor` reads `Res<ButtonInput<MouseButton>>::just_pressed(Left)` only. Pointer position routes correctly from touch/hand into `PointerState`, but only the mouse triggers press/release. v4 used pointer events that fired for both. Plan 11 adds `Res<Touches>` for `TouchPhase::Started`/`Ended` and a hand-tracking gesture for synthetic press.
3. **`spawn_template` lacks a file picker.** Currently a free-text input — typing absolute paths is a poor kiosk UX. Plan 11 adds an `rfd`-backed Browse… button next to the field.
4. **Manual side-by-side capture not yet performed.** The human-in-the-loop step below is what flips this verdict from PENDING to PASS.

**Tag points for the side-by-side capture (when Plan 11 performs it):**

- **v5** — `v5-line-parity` tag on this repository (`rewrite/bevy` branch HEAD at the time Plan 11 tags). Plan 10's commits are reachable via plain branch history; no Plan 10 tag was created since parity was incomplete.
- **v4** — `main` branch on this same repository at commit `3b85676` ("Bump version to 4.2.0"). The v4 codebase lives at the repository root prior to the `rewrite/bevy` branch — `npm run dev` on `main` boots it.

**Human-in-the-loop step (Plan 11):** Madison runs both apps at 1280×720 and confirms perceptual parity in three states:

1. **Idle** — no input. Particles settle to the horizontal-line spawn (or to the heatmap if `spawn_template` is set).
2. **Mid-press at center** — left button held at canvas center. Gravity smear concentric rings, chromatic shift, attractor ring rotation visible.
3. **Mid-decay (~5s after release)** — power decays geometrically, idle veto holds the sketch Active, audio reactivity tracks `groupedUpness` back down through silence.

**Known remaining parity deviations after Plan 11 closes:** none expected. The CPU mirror integration drift listed under "Approved deviations" is the only documented divergence and is bounded at ≤1% over long timescales — imperceptible at the festival-loop horizon.

## Leap-path hands-on verification (Plan 11.6)

Plan 11.6 lands the real `LeaprsProvider` + per-hand `LineHandAttractor` + HandMesh visualization. The nine scenarios below cover the Leap input surface end-to-end; Madison runs them with `cargo run -p waveconductor` and an Ultraleap-connected Mac. Each verdict starts PENDING and flips to PASS / FAIL / NEEDS_FIX with notes during the hands-on pass.

| # | Scenario | v4 behaviour | v5 verdict | Notes |
|---|---|---|---|---|
| 1 | **Service detection.** Stop the Ultraleap service via Activity Monitor; status LED should turn red ("Ultraleap service not running"). Restart the service; LED should transition through `ServiceOnly` (orange) → `DeviceAttached` (blue) → `Streaming` (green) within a few seconds. | ✓ | PENDING | |
| 2 | **Background-frames policy.** Toggle the `leap_background` setting in the user panel. Focus another window; with the setting OFF, the dev panel's `last_frame_ago` should freeze. With the setting ON, frames should keep arriving even when the WaveConductor window is not focused. | ✓ | PENDING | |
| 3 | **Grab above threshold spawns attractor.** Close fist over the Leap; particles should converge to the projected hand position. Open the hand; particles should disperse as `LineHandAttractor.power` geometrically decays. | ✓ | PENDING | |
| 4 | **Hold-with-motion.** Sustain a closed-fist grab while moving the hand laterally across the Leap's view; the attractor should follow the palm without visible lag. | ✓ | PENDING | |
| 5 | **Two hands → two attractors.** Both fists closed simultaneously; two converging particle clusters should appear at the two projected palm positions, each driven by its own `LineHandAttractor`. | ✓ | PENDING | |
| 6 | **Focal point follows first hand.** Hand A enters the tracking volume and grabs; gravity-smear focal point locks to A. Hand B enters later and grabs harder; focal point should stay on A (the lowest-index Entity). Drop A out of the volume; focal point should transfer to B on the next frame. | ✓ | PENDING | |
| 7 | **HandMesh visual.** Confirm ~20 small green (#add6b6) wireframe spheres per hand, tracking bone positions. Scope note: bones render on top of the Camera2d output but NOT through the HDR/bloom/AgX pipeline (Phase 13 punt — see carry-forward below). | ✓ | PENDING | |
| 8 | **Smudged sensor.** Smear the Leap sensor lens with a fingerprint; within a few seconds the status LED should turn yellow ("Tracking degraded") and the Shift+D dev panel's "Health:" row should show `SMUDGED`. | ✓ | PENDING | |
| 9 | **USB unplug mid-session.** Pull the Leap's USB cable mid-session; status LED should red, all `TrackedHand` entities should despawn (including their HandMesh children — verify via dev panel entity count), no crash. Reconnect; tracking should recover within a few seconds. | ✓ | PENDING | |

**FAIL or NEEDS_FIX entries:** any verdict that lands in those states gets a follow-up `fix:` commit. Once all nine flip to PASS, Plan 11.7 can perform the side-by-side capture and flip the top-level Verdict from PENDING to PASS.
