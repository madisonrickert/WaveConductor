# Plan 10: Line Polish + PARITY.md Sign-off Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the Line parity loop. Heatmap-image spawn template ships, the remaining carry-forwards drain, an 8-hour soak harness lands (required per AGENTS.md before any release tag), and `PARITY.md` gets its signed verdict.

**Architecture:** Three small additions on top of Plans 7–9. The heatmap-image sampler ports v4's `sampleParticlesFromHeatmap` directly: read a user-supplied PNG, build a CDF over luminance × alpha, weighted-sample particle positions. New `spawn_template: Option<PathBuf>` setting (Text category with a path picker; if no path, fall back to horizontal-line spawn). The soak harness is a `#[ignore]`-d integration test (`cargo test --release --ignored line_soak_8h`) that runs the production sketch loop for 8 hours, asserting frame-time stability and no allocator growth via a `wc_perf::tracker` resource.

**Tech Stack:** `image` crate (already a transitive dep via Bevy) for PNG decode. No new workspace deps required.

**Reference v4 source:** `src/sketches/line/heatmapSampler.ts` — the CDF sampler logic.

**Branch:** `rewrite/bevy`. Pre-flight: HEAD at or after `v5-line-audio`.

---

## Scope check

Plan 10 closes Line. Subsequent sketches (Flame, Dots, Cymatics, Waves) are Plans 11+ per the roadmap.

Four phases, four commits. Phase D pushes and tags `v5-line-parity`.

## File map

**Modified:**

- `crates/wc-sketches/src/line/settings.rs` — add `spawn_template: String` (Text setting; empty = no template).
- `crates/wc-sketches/src/line/heatmap.rs` — *new* — CDF sampler port.
- `crates/wc-sketches/src/line/systems/spawn.rs` — branch on `settings.spawn_template`: if non-empty, sample particle positions from the image; else use the existing horizontal-line layout.
- `crates/wc-sketches/src/line/PARITY.md` — final verdict.
- `crates/wc-sketches/tests/line_soak.rs` — *new* — `#[ignore]` 8-hour test.
- `crates/wc-core/src/lib.rs` — Optional: thread the `extern crate self as wc_core` allow into a more meaningful comment now that Plan 8 absorbed it (carry-forward #12).
- Remaining carry-forwards 23–48 absorbed where they don't conflict.

---

# Phase 0 — Remaining carry-forwards

Drain the smaller items from `docs/superpowers/next-plan-carry-forwards.md` that didn't fit earlier phases. The full list has 55 items; Plan 10 closes the ones whose value-to-cost ratio is right *now*.

### Task 1: Triage the carry-forwards list

Read `docs/superpowers/next-plan-carry-forwards.md` and categorize each remaining open item as:

- **Absorb now** — small, related to Plan 10's work, or test-quality polish
- **Carry to Plan 11+** — sketch-specific (Flame/Dots/Cymatics/Waves)
- **Long-term** — pre-release distribution items

Items expected to fit Plan 10:
- #11 (`SIM_PARAMS_SIZE` cast doc) — usually already done in Plan 9 Phase 0; verify
- #13 (`info!` → `debug!` log levels on restart trampoline)
- #20 (stray "vetos" prose spellings)
- #28 (`MAX_ATTRACTORS > 16` TODO note)
- #34 (`LineSettings` serde forward-compat note)
- #35 (lifecycle test `>= 0.1` floor)
- #42 (`MouseAttractorState.power` cross-reference)
- #43 (test prerequisite comment ordering)
- #44 (`line_idle_veto` visibility flag — leave; not currently needed)
- #46 (`move_pointer` rustdoc) — should be done in Plan 8 Phase 0; verify
- #47 (`seed_pointer` hoist — moot once #45 closed)
- #49 (`enter_line()` update count)
- #50 (`MOUSE_POWER_PRESS` const) — Plan 9 Phase 0 absorbed; verify
- #55 (visual verification of gravity-smear)

### Task 2: Apply the picks

Per-item, surgically apply the listed change. Tracker doc inline at the top of each commit lists which items landed.

### Task 3: Commit Phase 0

```bash
git commit -m "$(cat <<'EOF'
Plan 10 Phase 0: drain remaining carry-forwards

Absorbs the small/related items from next-plan-carry-forwards.md that
didn't fit Plans 7–9: log-level adjustments, prose spelling, doc
cross-references, test floor assertions, TODO notes near Plan 11
concerns. Leaves sketch-specific items (Flame/Dots/Cymatics/Waves)
and pre-release distribution work in the doc for their respective
plans.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase A — Heatmap-image spawn template

Direct port of v4's `sampleParticlesFromHeatmap`.

### Task 4: Add the `spawn_template` setting

**File:** `crates/wc-sketches/src/line/settings.rs`

Append a third field:

```rust
/// Path to a PNG file whose luminance × alpha drives particle spawn density.
/// Empty string = use the default horizontal-line layout. Relative paths
/// resolve against the asset directory; absolute paths are honored as-is.
/// v4 default = "" (no template).
#[setting(default = String::from(""), ty = Text, category = User, requires_restart)]
pub spawn_template: String,
```

### Task 5: Create the sampler

**File:** `crates/wc-sketches/src/line/heatmap.rs` (new)

Port `sampleParticlesFromHeatmap` directly. The CDF approach:

```rust
//! Heatmap-image particle spawn sampler.
//!
//! Loads a PNG, computes per-pixel luminance × alpha, builds a CDF over
//! the resulting weights, and returns `particle_count` sampled (x, y)
//! positions in window-space.
//!
//! Direct port of v4's `src/sketches/line/heatmapSampler.ts`.

use std::path::Path;

use bevy::math::Vec2;
use image::GenericImageView;

/// Sample particles from a brightness heatmap image.
///
/// `path` — image file (PNG / JPG / WebP — anything `image` crate decodes).
/// `canvas_w` / `canvas_h` — pixel dimensions of the target rendering canvas.
/// `count` — number of particles to sample.
///
/// Returns `Vec<Vec2>` with `count` (x, y) positions in window space
/// (top-left origin, +y down). On error (file missing, all-black image,
/// etc.) returns the fallback (a horizontal mid-line + sawtooth) so a
/// broken template doesn't break the sketch.
pub fn sample_from_heatmap(
    path: &Path,
    canvas_w: f32,
    canvas_h: f32,
    count: usize,
) -> Vec<Vec2> {
    match try_sample_from_heatmap(path, canvas_w, canvas_h, count) {
        Ok(positions) => positions,
        Err(err) => {
            tracing::warn!(
                ?err,
                ?path,
                "heatmap sample failed; falling back to horizontal line"
            );
            fallback_line(canvas_w, canvas_h, count)
        }
    }
}

fn try_sample_from_heatmap(
    path: &Path,
    canvas_w: f32,
    canvas_h: f32,
    count: usize,
) -> Result<Vec<Vec2>, image::ImageError> {
    let img = image::open(path)?;
    let sample_w = canvas_w.min(256.0) as u32;
    let sample_h = canvas_h.min(256.0) as u32;
    let img = img.resize_exact(sample_w, sample_h, image::imageops::FilterType::Triangle);
    let rgba = img.to_rgba8();

    // Build CDF.
    let total_pixels = (sample_w * sample_h) as usize;
    let mut cdf: Vec<f64> = Vec::with_capacity(total_pixels);
    let mut cumulative = 0.0_f64;
    for px in rgba.pixels() {
        let r = px[0] as f64;
        let g = px[1] as f64;
        let b = px[2] as f64;
        let a = px[3] as f64 / 255.0;
        let luminance = 0.299 * r + 0.587 * g + 0.114 * b;
        cumulative += luminance * a;
        cdf.push(cumulative);
    }

    if cumulative == 0.0 {
        return Ok(fallback_line(canvas_w, canvas_h, count));
    }

    // Sample `count` particles via binary search on the CDF.
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let scale_x = canvas_w / sample_w as f32;
    let scale_y = canvas_h / sample_h as f32;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let target = rng.gen::<f64>() * cumulative;
        let idx = cdf.partition_point(|&c| c < target);
        let idx = idx.min(total_pixels - 1);
        let px = (idx as u32) % sample_w;
        let py = (idx as u32) / sample_w;
        let x = (px as f32 + rng.gen::<f32>()) * scale_x;
        let y = (py as f32 + rng.gen::<f32>()) * scale_y;
        out.push(Vec2::new(x, y));
    }
    Ok(out)
}

fn fallback_line(canvas_w: f32, canvas_h: f32, count: usize) -> Vec<Vec2> {
    let half_w = canvas_w * 0.5;
    let mid_y = canvas_h * 0.5;
    (0..count)
        .map(|i| {
            let x = (i as f32 / count.max(1) as f32) * canvas_w - half_w;
            let jitter_strand = (i % 5) as f32 - 2.0;
            let y = mid_y + jitter_strand * 2.0;
            Vec2::new(x + half_w, y) // window-space
        })
        .collect()
}
```

> **`rand` dep:** add `rand = "0.8"` to `[workspace.dependencies]` if not already present, and depend on it in `crates/wc-sketches/Cargo.toml`. The crate is permissively-licensed and commonly used.

### Task 6: Branch `spawn_line` on `spawn_template`

**File:** `crates/wc-sketches/src/line/systems/spawn.rs`

After computing `count`, check `settings.spawn_template`:

```rust
let initial_positions: Vec<Vec2> = if settings.spawn_template.is_empty() {
    // Existing horizontal-line + sawtooth path.
    (0..count).map(...).collect()
} else {
    let path = Path::new(&settings.spawn_template);
    super::heatmap::sample_from_heatmap(path, w, h, count as usize)
};
```

Then build the `Particle` array from these positions, keeping `velocity: [0; 2], original_xy: pos.to_array(), alpha: 0.0`.

### Task 7: Commit Phase A

---

# Phase B — 8-hour soak test

Required by AGENTS.md before any release tag. Marked `#[ignore]` so it doesn't run in normal CI; CI runs only on manual trigger or release branches.

### Task 8: `tests/line_soak.rs`

**File:** `crates/wc-sketches/tests/line_soak.rs` (new)

Builds a real sketch app with `DefaultPlugins` (not MinimalPlugins — soak needs the full render path), enters Line, simulates 8 hours of continuous interaction by writing synthetic mouse events on a loop, and asserts at the end:

- Frame-time mean within 2× of the first 10s baseline (no thermal throttling)
- Allocator high-watermark within 1.5× of the first 10s baseline (no slow leak)
- No panics (caught via `std::panic::catch_unwind` around `app.update()`)

Use `wc_perf` (a new tiny crate) if frame-tracking instrumentation isn't already in the project. Alternatively, use Bevy's `FrameTimeDiagnosticsPlugin` and read from `DiagnosticsStore`.

```rust
#![cfg(not(target_arch = "wasm32"))]

mod common;
use common::input::{press_left, release_left, move_pointer};
use common::sketches_test_app_with_default_plugins;

use std::time::Duration;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::input::keyboard::KeyCode;

#[test]
#[ignore = "8-hour soak; run via cargo test --release --ignored line_soak_8h"]
fn line_soak_8h() {
    let mut app = sketches_test_app_with_default_plugins();
    app.add_plugins(FrameTimeDiagnosticsPlugin::default());
    app.update();
    // Enter line via Digit1 keyboard nav...
    // Loop for 8 hours with synthetic mouse motion + occasional press/release...
    // At end, read diagnostics, assert.
}
```

> **`sketches_test_app_with_default_plugins`** is a new variant of the existing test app builder that uses `DefaultPlugins` instead of `MinimalPlugins`. Add it to `tests/common/mod.rs`.

### Task 9: Document the soak workflow

Add a short section to `crates/wc-sketches/src/line/PARITY.md` explaining when to run the soak (before release tag) and what its acceptance criteria are.

### Task 10: Commit Phase B

---

# Phase C — Final PARITY sign-off

### Task 11: Side-by-side capture

Open v4 (`npm run dev` on `main` branch in a separate clone or via the v4 binary) at 1280×720; open v5 (`cargo run -p waveconductor`) at the same resolution. Drive the same interactions in both. Capture screenshots of:

1. Idle (no input)
2. Mid-press at center
3. Mid-decay (5 seconds after release)

### Task 12: Update PARITY.md

Append a "Verdict" section noting:
- Side-by-side capture commit hashes (v4 = `main` commit, v5 = `v5-line-audio` tag)
- Sign-off: PASS / NEEDS_TUNING / FAIL with reasoning
- If PASS, the tag will be `v5-line-parity`

If anything looks off, file a carry-forward and fix before Phase D's tag.

### Task 13: Commit Phase C

---

# Phase D — Push, watch CI, tag `v5-line-parity`

### Task 14: Final gates

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git push origin rewrite/bevy
# wait for CI
git tag v5-line-parity
git push origin v5-line-parity
```

### Task 15: Update roadmap

Plan 10 → ✅ shipped; tag `v5-line-parity`. Plan 11 (next sketch) → ⏳ next.

Append a celebratory note: **Line is done.** v5 has its first fully-parity-validated sketch.

### Task 16: Commit + push roadmap

---

## Self-review checklist

- [ ] Heatmap-image spawn template loads test images correctly
- [ ] `line_soak_8h` is `#[ignore]` and Madison can run it manually before release
- [ ] PARITY.md has a signed verdict
- [ ] Tag `v5-line-parity` pushed
- [ ] Roadmap updated, Plan 11 ⏳ next

## Carry-forwards for Plan 11+

After Plan 10 lands, the remaining items in `next-plan-carry-forwards.md` are scoped for the next sketch (Flame / Dots / Cymatics / Waves) or for pre-release distribution work. Both get their own plans.

## Execution handoff

Two execution options: subagent-driven (recommended) or inline. Subagent-driven for the 8-hour soak phase is particularly valuable since the test can run unattended while the controller orchestrates other work.
