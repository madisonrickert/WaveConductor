# Dots D7 — Parity closure — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** Close out the Dots ("Fabric") port: write the `dots/PARITY.md` record (with the full operator pre-tag checklist of deferred verifications), add the deterministic `cargo xtask capture` scenario(s) for Dots, and add the 8-hour `dots_soak` harness — so Dots has the same regression + soak coverage Line has.

**Architecture:** No new sketch behavior. Three artifacts: (1) `crates/wc-sketches/src/dots/PARITY.md` — the parity record mirroring `line/PARITY.md`, tracing the D1–D6b plan progression, the approved deviations from v4, and a consolidated operator checklist (the visual/hardware/audio/soak verifications deferred throughout the sprint); (2) `dots-synthetic` (+ `dots-screensaver`) scenarios in `tests/visual/scenarios.toml` — config only; **baselines are operator-seeded** on the deployment-class machine after visual confirmation (capture needs a real display + GPU-stable PNGs, per `tests/visual/CLAUDE.md`); (3) `crates/wc-sketches/tests/dots_soak.rs` — an `#[ignore]`d MinimalPlugins soak mirroring `line_soak.rs`.

**Tech Stack:** Markdown (PARITY record), TOML (capture scenario), Rust (`#[ignore]`d soak test on `sketches_test_app`).

## Global Constraints

- **No new sketch behavior** — D7 is documentation + test infrastructure only.
- **Baselines are NOT seeded by a subagent.** Per `tests/visual/CLAUDE.md`: baselines are GPU/driver-sensitive and must be captured on the deployment-class machine (Madison's Mac) only after Reading the frames and visually confirming. A subagent has no display. So D7 adds the scenario CONFIG; the PARITY checklist records "operator: capture + `--update-baselines` after visual confirmation" as a pre-tag step.
- **The 8-hour soak is the operator's** (AGENTS.md: required before any release tag). D7 adds the `#[ignore]`d harness + the invocation command; the actual run is Madison's.
- **Mirror Line's artifacts:** `crates/wc-sketches/src/line/PARITY.md`, `tests/visual/scenarios.toml` (the `line-synthetic`/`line-screensaver` blocks), `crates/wc-sketches/tests/line_soak.rs` (the `#[ignore]` + `sketches_test_app` + `app.update()`-loop pattern).
- **`#[ignore]`** the soak so CI does not run it; it must still COMPILE under `--all-features` (the gate suite compiles `--all-targets`).
- **No home-dir paths / secrets** in the PARITY doc or scenarios (`cargo xtask check-secrets` gate). PARITY.md is internal docs — em dashes are fine there.
- **Verification gates:** fmt; clippy `--all-targets --all-features --workspace -D warnings`; nextest `--workspace --all-features` + `cargo test --doc`; `cargo doc`; `cargo xtask check-secrets`. The new soak test compiles but is `#[ignore]`d. Do NOT run `cargo rund` or `cargo xtask capture` (no display in a subagent).
- **Commit messages:** `git commit -F` (no backticks).

## Reference material (read these)

- `crates/wc-sketches/src/line/PARITY.md` (the record structure: parity target, reference media, plan progression, approved deviations, verdict/gaps).
- `tests/visual/scenarios.toml` + `tests/visual/CLAUDE.md` (the scenario schema; `sketch = "dots"` is already a supported value; `provider = "synthetic"` = stationary synthetic hand, `mock` = silent; `FORCE_SCREENSAVER` debug toggle for the attract scenario).
- `crates/wc-sketches/tests/line_soak.rs` (the `#[ignore]`d MinimalPlugins soak: `sketches_test_app`, the `app.update()` tick loop, the synthetic-cursor + press/release drive, the final state assertion) + `crates/wc-sketches/tests/common/` (the `sketches_test_app` helper + how to enter a sketch state in tests).
- The SDD ledger `.superpowers/sdd/progress.md` (the consolidated operator-deferred items + carry-forwards accumulated across D1–D6b — fold these into PARITY.md's checklist).

---

### Task 1: `dots/PARITY.md` + capture scenario config

**Files:**
- Create: `crates/wc-sketches/src/dots/PARITY.md`
- Modify: `tests/visual/scenarios.toml` (add `[scenarios.dots-synthetic]` + `[scenarios.dots-screensaver]`)

**Interfaces:** none (docs + config).

- [ ] **Step 1: Write `dots/PARITY.md`**

Mirror `line/PARITY.md`'s structure. Contents:
- **Parity target:** Perceptual, against v4 `src/sketches/dots/` (screenshots `dots1.png`/`dots3.png`, the explode post, `audio.ts`). Internal name `Dots`; display "Fabric".
- **Reference media:** the v4 worktree paths.
- **Plan progression (shipped):** D1 (shared `particles/` foundation + Line refactor + stationary-spring kernel term), D2 (scaffold + grid spawn + Dots sim + mouse/touch attractor), D3 (explode post-process + the Line-smear-whiteout + render-world-removal fixes), D4 (DotsSynth voice + envelope coupling, envelope-primary), D5 (Leap/MediaPipe hand attractors + hand audio), D6a (screensaver attract-mode), D6b (bone-wireframe hands + the Line bone-composite-leak fix).
- **Approved deviations from v4:** WGSL compute kernel replaces v4's CPU `particleSystem.ts`; envelope-primary audio (the `flatRatio`/variance field-shape stats are not GPU-readback'd — a documented perceptual gap, same trade Line made); the screensaver is a v5 kiosk addition (v4 Dots had none); the idle veto keeps Dots awake while the pointer is held (v5 UX, diverges from v4's sleep gate); known limit (#75) bones bypass the main bloom rolloff.
- **Operator pre-tag checklist (consolidate from the ledger's deferred items + carry-forwards):** the `cargo rund` visual checks (D2 grid pulled by pointer; D3 explode renders the chromatic "fabric" matching `dots1/dots3.png`, NOT white, no Line-smear leak, cursor-centered; Home unaffected by the Line-post gating change); D4 audio tuning by ear (the named envelope/cutoff constants) + the `flatRatio` gap; D5/D6b hardware hand-tracking (grab pulls the grid; proximity scales force; wireframe skeletons render; bone hue); D6a screensaver feel + the **soak-watch item** (does the idle grid hold its layout or drift under turbulence, given `stationary_constant=0.01` runs in attract); seed the capture baselines (Task 1 Step 2) on the deployment machine; run the 8-hour `dots_soak` (Task 2) before any tag; the carry-forwards (extract the duplicated leap-power curve / `palm_to_world` / hash / bone material+mesh+shader+composite + the wrong "premultiplied-alpha" doc wording + the unused bone-composite `#[allow]` into a shared `particles/` home, fixing Line+Dots together).

- [ ] **Step 2: Add the capture scenario config**

Append to `tests/visual/scenarios.toml`:
- `[scenarios.dots-synthetic]`: `sketch = "dots"`, `provider = "synthetic"`, `config = "clean"`, `frames = [30, 60, 120, 240]`, `dt = 0.016666667` (mirror `line-synthetic`).
- `[scenarios.dots-screensaver]` + `[scenarios.dots-screensaver.debug]` with `FORCE_SCREENSAVER = "1"`: `sketch = "dots"`, `provider = "mock"`, `config = "clean"`, a frame spread that samples the attract morph (mirror `line-screensaver`'s indices, or a sensible Dots spread — document the frame choices in a comment like Line does). Add a header comment explaining the scenario, and that **baselines are operator-seeded** (no baseline PNGs are committed by this task — they require the deployment machine + visual confirmation, per `tests/visual/CLAUDE.md`).

- [ ] **Step 3: Verify (config + docs only)**

```bash
cargo xtask capture --list            # confirm dots-synthetic + dots-screensaver now listed
cargo xtask check-secrets             # PARITY.md + scenarios have no home paths/secrets
cargo doc --no-deps -p wc-sketches --document-private-items   # PARITY.md is not rustdoc, but confirm no new warnings
```

Expected: `--list` shows the two new scenario names; check-secrets clean. (Do NOT run an actual capture — no display.)

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "docs(dots): PARITY record + capture scenario config (baselines operator-seeded)" "" "dots/PARITY.md traces the D1-D6b progression + approved v4 deviations + the operator pre-tag checklist (visual/hardware/audio/soak/baselines/carry-forwards). dots-synthetic + dots-screensaver scenarios added; baselines seeded on the deployment machine after visual confirmation." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

### Task 2: `dots_soak` harness

**Files:**
- Create: `crates/wc-sketches/tests/dots_soak.rs`

**Interfaces:** none (an `#[ignore]`d integration test).

- [ ] **Step 1: Write the soak harness**

Mirror `crates/wc-sketches/tests/line_soak.rs` (read it + `tests/common/`): build a `sketches_test_app`, enter `AppState::Dots`, and drive ~1.7M `app.update()` ticks (the same `SOAK_TICKS` Line uses) with a synthetic cursor sweep + a press/release cycle (mirror Line's drive), asserting the sketch stays in `AppState::Dots` (and does not panic). `#[ignore = "8-hour soak; run via cargo test --release -p wc-sketches --test dots_soak -- --ignored dots_soak_8h"]` on the test fn. Add the module `//!` doc explaining it runs under `MinimalPlugins` (no RenderApp/GPU), so it exercises the sim/lifecycle/idle path under multi-hour tick counts but not the renderer — and that a `DefaultPlugins` full-render soak on the deployment device is the separate, gating pre-tag artifact (cross-reference the roadmap's full-render soak item, mirroring Line's note).

- [ ] **Step 2: Confirm it compiles + is ignored**

```bash
cargo nextest run -p wc-sketches --test dots_soak   # the #[ignore]d test is SKIPPED (compiles, doesn't run the 8h loop)
cargo clippy -p wc-sketches --tests --all-features -- -D warnings
```

Expected: the soak compiles and is reported skipped/ignored (NOT run). clippy clean.

- [ ] **Step 3: Full gate suite**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo xtask check-secrets
```

Expected: all PASS (the soak compiles under `--all-targets`, runs only when `--ignored`).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "test(dots): 8-hour soak harness (ignored; MinimalPlugins tick loop)" "" "dots_soak_8h mirrors line_soak: ~1.7M app.update() ticks with synthetic cursor + press/release, asserts Dots stays active. Operator runs it before any release tag (AGENTS.md). A DefaultPlugins full-render soak on the device is the separate gating artifact." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

## Self-Review

**Spec coverage** (against §"Testing & parity" + D7 row):
- `dots/PARITY.md` with the plan progression + deviations + operator checklist → Task 1 Step 1. ✓
- `cargo xtask capture dots-*` scenario(s) → Task 1 Step 2 (config; baselines operator-seeded). ✓
- `dots_soak` harness (`#[ignore]`d, 8-hour) → Task 2. ✓
- The consolidated operator-deferred items + carry-forwards recorded for Madison → Task 1 Step 1 checklist. ✓

**Placeholder scan:** No TBD-as-deliverable. PARITY.md content is enumerated from the shipped plans + the ledger's deferred items; the scenarios mirror named Line blocks; the soak mirrors `line_soak.rs`. The deliberately-deferred items (baseline seeding, the 8h run, the visual/hardware checks) are explicitly the operator's, recorded in the checklist — that is the honest closing state, not a placeholder.

**Type consistency:** n/a (docs + config + one `#[ignore]`d test). The soak's `dots_soak_8h` name + the scenario names `dots-synthetic`/`dots-screensaver` are used consistently.

**Risks:** (1) The capture scenarios can't be baseline-verified by a subagent (no display) — by design, deferred to the operator with the rationale recorded. (2) The soak must COMPILE under `--all-features` even though it's `#[ignore]`d — pinned by Task 2 Step 2/3. (3) PARITY.md is the durable hand-off; its checklist must capture every deferred item — pulled from the ledger.
