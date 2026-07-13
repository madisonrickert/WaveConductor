# Upstream-parity fix wave — BlazePose landmark/segmentation stage

Five upstream-MediaPipe stability adoptions in the Rust BlazePose port, matrix
items #16, #19, #20, #21, #25. Detection stage untouched (its audit found no
divergences). Branch `radiance`.

## Upstream sources fetched (raw.githubusercontent.com, master)

- `modules/pose_landmark/pose_landmark_filtering.pbtxt` — the aux / visibility /
  world filter banks.
- `modules/pose_landmark/pose_segmentation_filtering.pbtxt` — segmentation
  combine ratio.
- `calculators/image/segmentation_smoothing_calculator.cc` — uncertainty
  polynomial (CPU `blending_fn`).
- `calculators/util/world_landmark_projection_calculator.cc` — world x/y
  rotation transform.

### Exact constants confirmed

| Item | Upstream constants (verbatim) |
|------|-------------------------------|
| #16 aux filter | `one_euro_filter { min_cutoff: 0.01 beta: 10.0 derivate_cutoff: 1.0 }`; aux bank connects `OBJECT_SCALE_ROI:roi` with **no** `disable_value_scaling` ⇒ aux DOES use object-scale value scaling (scale = the aux alignment box side). |
| #19 visibility | `VisibilitySmoothingCalculator { low_pass_filter { alpha: 0.1 } }`, on the normalized, world, and aux banks. |
| #20 world filter | `one_euro_filter { min_cutoff: 0.1 beta: 40.0 derivate_cutoff: 1.0 disable_value_scaling: true }`. |
| #21 world de-rotation | `out.x = cos·in.x − sin·in.y; out.y = sin·in.x + cos·in.y` (z copied), angle = rect.rotation. |
| #25 mask blend | `combine_with_previous_ratio: 0.7`; `x = (new−0.5)²`, `uncertainty = 1 − min(1, x·(c1 + x·(c2 + x·(c3 + x·(c4 + x·c5)))))`, `out = new + (prev−new)·(uncertainty·ratio)`, with c1=5.68842, c2=−0.748699, c3=−57.8051, c4=291.309, c5=−624.717. |

## What changed, per finding

### #16 Aux-landmark temporal filter (worker-side, `pipeline.rs`)
- New `AuxRoiFilter` (two points × x/y = four `OneEuroFilter` channels) with the
  upstream aux params `AUX_MIN_CUTOFF=0.01`, `AUX_BETA=10.0`. Reuses the existing
  `OneEuroFilter` from `smoothing.rs` (widened to `pub(super)`) — no duplicated
  One-Euro math.
- Object-scale value scaling matched: `value_scale = 1 / (2·|scale − centre|)`,
  computed from the **raw** aux points each frame (mirrors upstream's
  `OBJECT_SCALE_ROI` built from unfiltered aux landmarks).
- Applied in `landmark_stage` on rows 33/34 **before** `roi_from_alignment_points`
  derives the next-frame tracking ROI (was `roi_from_body_landmarks` on raw rows).
- Reset on every fresh track: `landmark_stage` takes `fresh_track` (true whenever
  `self.tracked` was `None` at frame start), which resets the filter. Any track
  interruption (detector re-run, low-confidence drop, ROI untrackable, idle probe,
  invalid frame) forces the next successful frame to be a cold start, so a new
  person never inherits stale filter state.
- `process()` gained a `now: Duration` param (threaded from the worker's
  `loop_start.duration_since(start)`) to drive the filter timestep, mirroring
  `BodySmoother::smooth`. No per-frame allocation (fixed arrays on the struct).
- Aux **visibility** smoothing (upstream also low-passes aux visibility) was
  intentionally not added: aux visibility feeds nothing downstream
  (`roi_from_alignment_points` ignores it), so it is a genuine no-op here.

### #19 Visibility smoothing (main-side, `smoothing.rs`)
- `BodySmoother` gained a `[f32; 33]` per-landmark low-pass (`VISIBILITY_LOW_PASS_ALPHA=0.1`)
  + `has_vis` flag. First frame passes through; subsequent frames
  `alpha·new + (1−alpha)·prev`. Reset by the smoother's existing `clear()`.
  Published visibility is now low-passed rather than raw sigmoid.

### #20 World-landmark filter params (main-side, `smoothing.rs`)
- New `WORLD_MIN_CUTOFF=0.1`, `WORLD_BETA=40.0` (pub, documented). The world
  filter bank is constructed with these instead of the screen params (0.05/80);
  `value_scale` stays 1.0 (= `disable_value_scaling: true`).
- `set_params` (the Dev-knob retune) now touches only the screen bank — the world
  bank keeps its fixed upstream tuning (world coords are a metric unit that must
  not track the screen knob).

### #21 World-landmark ROI de-rotation (worker-side, `pipeline.rs`)
- `decode_world_landmarks(raw, roi)` rotates x/y by the ROI rotation
  (`cos·x − sin·y`, `sin·x + cos·y`; z unchanged) before publishing, per
  `WorldLandmarkProjectionCalculator`. Applied at decode with the current frame's
  ROI. (The `hot_person` fixture has rotation 0, so existing world assertions
  still hold.)

### #25 Uncertainty-weighted mask blend (`mask.rs`)
- Replaced the global EMA (`ema_blend`, `acc += (new−acc)·α`) with
  `uncertainty_blend`, implementing upstream's per-pixel uncertainty polynomial
  verbatim (c1..c5 cited inline). Boundary pixels (new≈0.5) blend up to `ratio`
  toward previous; confident pixels (new≈0/1) track the new frame almost exactly.
  Order kept (blend after warp — audit called it benign); first-frame copy and
  absent-frame `ema_decay` behavior kept.
- **`mask_ema` field name is PINNED and unchanged**; its meaning is redefined as
  the combine-with-previous ratio. Default constant `DEFAULT_MASK_EMA_ALPHA`
  0.35 → **0.7**. Updated coherently everywhere: the field/knob doc comments
  (`mod.rs`, `pipeline.rs` `PoseConfig`/`BodyLiveTuning`), the Radiance setting
  (`settings.rs` `default = 0.7`, `default_mask_ema()` → 0.7, doc), and stale
  "EMA-smoothed mask" comments in `mask.rs`/`mod.rs`/`edges.rs`/`systems.rs`.

## Tests (TDD, colocated)

New/changed:
- `mask.rs`: `uncertainty_blend_boundary_pixel_pulls_toward_previous` (new=0.5,
  prev=1.0, ratio=0.7 → 0.85, hand-computed), `..._confident_pixel_tracks_the_new_frame`
  (new=0/1 → uncertainty≈0 → tracks new), `..._ratio_zero_is_passthrough`. Old
  `ema_blend_*` tests removed; `ema_decay_fades_toward_zero` kept.
- `smoothing.rs`: `visibility_low_passes_toward_a_stepped_target` (0.9→0.0 step,
  first low-pass = 0.81 hand-computed, converges), `clear_resets_visibility_history`,
  `world_filter_keeps_its_own_params_when_screen_is_retuned` (freeze screen filter,
  world still eases because its bank is untouched by set_params).
- `pipeline.rs`: `aux_filter_reduces_roi_centre_jitter` (jittered synthetic aux
  centre → filtered variance < 0.25× raw variance), `aux_filter_reset_cold_starts`
  (post-reset sample passes through), `decode_world_landmarks_derotates_by_roi_rotation`
  (90° ROI → x/y swap+negate; 0° = identity). All existing `process()` call sites
  updated for the new `now` arg.

## Gate outputs

- `cargo fmt --all -- --check` — exit 0.
- `CARGO_INCREMENTAL=0 cargo clippy --all-targets --all-features --workspace -- -D warnings` — exit 0.
- `CARGO_INCREMENTAL=0 cargo nextest run --workspace --all-features` — **1283 passed, 12 skipped, 0 failed** (incl. the 4 `model_tests` needing assets/models/pose, and all 74 `input::body` tests).
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --document-private-items` — exit 0 (default features). Extra self-check `cargo doc -p wc-core --all-features --document-private-items` (documents the gated body module + its new intra-doc links) — exit 0.

Doctests: no runnable ` ```rust ` doc examples were added (new fenced blocks are
` ```text `), so `cargo test --doc` was not separately required by this change.

## Notes / concerns

- The absent-frame mask **decay** (`fade_mask_into` → `ema_decay`, our EXTRA #26)
  is driven by the same `mask_ema` knob, whose default moved 0.35 → 0.7, so the
  person-left silhouette now fades roughly twice as fast per frame. Kept per the
  brief ("keep existing absent-frame decay behavior"); flagged for ear/eye-tuning
  at the hardware checkpoint if the fade reads too quick.
- Disk: the data volume was full at start; `target/` was removed to free ~36 GB
  before building (per the low-disk memory note).
