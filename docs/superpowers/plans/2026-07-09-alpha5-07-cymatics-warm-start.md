# Cymatics Warm Start Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every entry into Cymatics — not just the app's first boot — shows two distinct orange blobs immediately, instead of a blank field that reads as a blue screen of death.

**Architecture:** `OnEnter(AppState::Cymatics)` currently inserts `CymaticsState::default()`, an all-zero, overlapping-centres resting state, on *every* entry (`init_cymatics_state`, `crates/wc-sketches/src/cymatics/mod.rs:389`). This plan adds one pure function, `warm_start_state(&CymaticsSettings) -> CymaticsState`, and calls it from `init_cymatics_state` in place of `CymaticsState::default()`. It seeds exactly three fields — `center`/`center2` (reusing the already-pure, already-tested `screensaver::wander_centers` at `elapsed = 0.0`), `active_radius` (from the live `settings.attract_radius` Dev knob), and `ramp_time` (a new private constant, `WARM_START_RAMP_TIME`, chosen by a human looking at the rendered output) — each independently fixing one cause of the blank field. Nothing else changes: no new systems, no state-machine change, no shader edits.

**Tech Stack:** Rust, Bevy 0.19 (`OnEnter` schedule, `Res`/`Commands` system params), glam `Vec2`. No shader changes — `assets/shaders/cymatics/simulate.wgsl`'s alive-mask math already accepts whatever values land in `CymaticsSimParams`; this plan only changes which `CymaticsState` values `OnEnter` constructs before that math ever runs.

**Depends on:** Plan 02 — file overlap only. Plan 02 also edits `crates/wc-sketches/src/cymatics/mod.rs` for sim-grid re-init on resize. Land 02 first, or coordinate.

## Global Constraints

Copied verbatim from `AGENTS.md` and the alpha.5 program index's Part 1 (`docs/superpowers/plans/2026-07-09-alpha5-program-index.md`). Every task's requirements implicitly include this section.

- **CI gates**, all of which must pass before a task is complete:
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features --workspace -- -D warnings`
  - `cargo nextest run --workspace --all-features`
  - `cargo test --doc --workspace`
  - `cargo doc --no-deps --workspace --document-private-items` (CI runs with `RUSTDOCFLAGS="-D warnings"`; **no `--all-features`** when reproducing this gate — feature-gated modules surface unrelated errors)
  - `cargo deny check`
  - `cargo xtask check-secrets`
- **The per-task clippy gate must use `--all-targets`.** `cargo clippy -p <crate> --lib` skips the test target; CI runs `--all-targets` and will catch lints (`range_plus_one`, `used_underscore_binding`, `bool_assert_comparison`) that a `--lib`-only run misses.
- **Clippy is `-D warnings` over `pedantic`, including inside `#[cfg(test)]`.** `Cargo.toml:206-211` sets `pedantic = warn` plus `unwrap_used`, `expect_used`, `panic`, `as_conversions` at `warn`; CI's `-D warnings` escalates all of them, test code included. `.expect()`/`.unwrap()` in a `#[cfg(test)] mod tests` block is denied unless the block (or the specific test) already carries `#[allow(clippy::expect_used, reason = "...")]` — this file's `mod tests` already does, at the module level.
- **No `unwrap()` or `expect()` in non-test code** unless the panic is documented as an invariant violation. **No `as` casts** where `From`/`TryFrom`/`u32::try_from` would work.
- `///` rustdoc on every public item; module-level `//!` on every module root. **Never strip comments during refactors** — update stale comments rather than removing them.
- Public API at the top, private helpers at the bottom, tests in a `#[cfg(test)] mod tests` block at the file footer. One concept per file; ~300 lines is a guideline, not a hard cap.
- **The doc gate denies public→private intra-doc links** (`rustdoc::private_intra_doc_links`). A `pub` item's rustdoc linking to a `pub(crate)` or private item is denied; public trait-impl methods count as public. Demote to a plain code span when in doubt.
- **Sketches must run zero systems in `SketchActivity::Idle`.** Not mechanically at stake in this plan (it adds no new `Update` systems, only changes what one existing `OnEnter` system constructs), but stated here because it binds every task in this program.
- **Never allocate in a hot path** — per-frame Bevy systems, the audio callback, worker threads. Not mechanically at stake here either (`OnEnter` runs once per sketch entry, not per frame), but the same reasoning applies to why `warm_start_state` must stay a small, stack-only pure function rather than growing an allocation.
- **There are no GPU tests in CI.** Everything in `crates/wc-core/tests/ui_blur.rs` is `#[ignore]`d (winit needs the macOS main thread; `cargo nextest` skips ignored tests). `cargo xtask capture` returns all-black `[0,0,0]` frames when the app window is not foregrounded, so an agent cannot use it to verify rendering. **A human must run `cargo rund`.** This is why Task 1 below is a human-run spike, not an automated one. **Retracted — see the "Correction" subsection right after the Testing note below: this premise was tested and found false in the environment Task 1 actually ran in.**
- **Commit messages:** `-F <file>`, never `-m`. Backticks inside a `-m` string are command-substituted by zsh and silently eat words.
- **Never `git add -A`.** Stage named paths only, then `git show --stat HEAD` to confirm.
- **Branch:** all work lands on `windows-remediation`, branched from `v5-alpha`.
- **Do not** put `bevy/dynamic_linking` in any manifest `[features]` table. Use `cargo rund` for manual smoke tests, never the bare `target/` binary.

## Testing note (deviation from the usual write-test-first shape)

Every other plan in this program writes a failing test before the implementation. This one cannot, for exactly one of its three seeded fields: **`ramp_time`**. The brief is explicit about why, and it is worth restating so nobody "fixes" this later by adding a paper-derived assertion:

> Do NOT derive `ramp_time`'s seed on paper and assert a magic number in a test. There are no GPU tests in CI, and `cargo xtask capture` returns all-black frames when the app window isn't foregrounded, so no agent can verify this. Madison must look at it. **(Retracted — see the Correction subsection immediately below.)**

So Task 1 below ships working code — including a *candidate* value for the one human-judged constant, `WARM_START_RAMP_TIME` — and asks a human to run `cargo rund`, look at the rendered field, and either accept the candidate or edit it in place. **Only Task 2**, after that visual review has landed, adds automated tests, and those tests deliberately do **not** re-derive or hardcode a second copy of the chosen number — they reference the same named constant the production code uses (so whatever value the human ultimately settles on, the test suite is automatically testing *that* value, not a stale guess) and assert the *structural* properties that are legitimately derivable without a screen: bounds, distinctness, purity, and idempotency across repeated `OnEnter` cycles.

The other two seeded fields (`center`/`center2`, `active_radius`) do not have this problem — they are seeded from an already-pure, already-unit-tested function (`screensaver::wander_centers`) and an existing live setting (`settings.attract_radius`), respectively, so their *correctness* tests are ordinary geometry/plumbing assertions Task 2 can write without needing a screen. They still get one look during the Task 1 visual review, because the three seeds interact in the rendered image and only a human can judge the composite result.

### Correction (recorded during Task 3): the "no agent can verify" premise above is false in this environment

Everything above this line in this section — and the matching Global Constraints bullet — asserted that `cargo xtask capture` returns all-black frames for a backgrounded window, and concluded from that a human at `cargo rund` was the *only* way to judge `WARM_START_RAMP_TIME` and the seeded centre placement. Task 1 tested that premise instead of taking it on faith: it ran `cargo xtask capture cymatics-synthetic` in this same agent-operated environment and got correctly rendered, non-black frames, which the operating agent reviewed directly — exactly what `AGENTS.md`'s Visual testing section says the capture harness is for. That review is what caught the second, more subtle defect (both wander centres pinned to the top of their Y range and clipped by the frame edge at `elapsed = 0.0`) that no amount of staring at the ramp-clamp arithmetic would have surfaced — see `WARM_START_WANDER_PHASE`'s doc comment and commit `84002e10` in `crates/wc-sketches/src/cymatics/mod.rs`.

Leave the retracted text above in place rather than deleting it — it accurately records what this plan assumed going in and why Task 1 was originally scoped as a human-run spike — but do not carry the "an agent cannot verify this" framing forward into later plans. The correct framing, matching `AGENTS.md`: the capture harness plus direct PNG review by the operating agent is the normal path for judging rendered output in this repo; a genuinely headless build session with no windowing surface at all (as distinct from an agent-operated session with a real, possibly-unfocused window) is the actual limiting case, and even that should be verified per-session rather than assumed.

## Design note: this does not add a second writer of `ramp_time`

The field tester's fix touches `CymaticsState::ramp_time`, and `crates/wc-sketches/src/cymatics/mod.rs` documents a single-owner invariant on that exact field: `update_cymatics_sim_params` is the only system that advances it, once per `Update`-schedule frame (read at `mod.rs:557` as `ramp_base`, written back at `mod.rs:577`; the same two lines also own `simulation_time`, its sibling clock). It is worth being precise about why seeding `ramp_time` in `init_cymatics_state` does not violate that invariant, because it is the subtlest part of this plan.

- **The invariant is about per-frame advancement, not initial value.** `update_cymatics_sim_params` runs in `Update`, every frame, and reads whatever is currently in the `CymaticsState` resource as its starting point (`ramp_base`) for that frame's advance. It has never cared, and does not need to care, what value was there before it first ran — today that value is `0.0` (from `CymaticsState::default()`); after this plan it is `WARM_START_RAMP_TIME`. Either way, `update_cymatics_sim_params` treats it as the frame's starting `ramp_base` and advances it exactly once. No behavior in that system changes.
- **`init_cymatics_state` runs in `OnEnter`, not `Update`.** It is not a competing per-frame advancer; it is the one-time resource *constructor* for a fresh sketch entry — the same role `CymaticsState::default()` already played. This plan changes which literal values that one-time constructor uses; it does not add a second system that writes `ramp_time` on an ongoing basis. A `crates/`-wide grep for `ramp_time` confirms the only writers are: the `Default` impl, this plan's new `warm_start_state`, `update_cymatics_sim_params`, and test code — no screensaver or interaction system touches it.
- **Every entry starts clean.** `OnExit(AppState::Cymatics)` runs `remove_cymatics_sim_params`, which drops the `CymaticsState` resource entirely (`mod.rs:478-481`). The next `OnEnter` re-inserts it from scratch via `init_cymatics_state`. There is no stale `ramp_time` left over from a previous entry for the new seed to race against or overwrite inconsistently — construction and advancement never overlap in the same frame.

Net: `update_cymatics_sim_params` remains the sole per-frame advancer of both `simulation_time` and `ramp_time`. `init_cymatics_state` remains the sole per-entry constructor of the resource, exactly as it was before this plan; only the constructed values change.

---

### Task 1: Human-run visual spike — seed `CymaticsState` on `OnEnter`

**Files:**
- Modify: `crates/wc-sketches/src/cymatics/mod.rs:6-8` (module doc)
- Modify: `crates/wc-sketches/src/cymatics/mod.rs:388-391` (replace `init_cymatics_state`, add `WARM_START_RAMP_TIME` and `warm_start_state`)

**Interfaces:**
- Consumes: `screensaver::wander_centers` (`crates/wc-sketches/src/cymatics/screensaver.rs:81`, already `pub fn` in an already-`pub mod screensaver` — **no visibility change needed**), `screensaver::LissajousSpeeds::from_settings` (`screensaver.rs:56`, already `pub`), `CymaticsSettings::attract_radius` (`crates/wc-sketches/src/cymatics/settings.rs`, already `pub` field), `CymaticsState::default()` (`mod.rs:124-139`, unchanged by this plan).
- Produces:
  - `const WARM_START_RAMP_TIME: f32` (private)
  - `fn warm_start_state(settings: &CymaticsSettings) -> CymaticsState` (private, pure)
  - `fn init_cymatics_state(mut commands: Commands<'_, '_>, settings: Res<'_, CymaticsSettings>)` (private; signature gains a `Res<CymaticsSettings>` parameter — confirmed safe: `register_sketch_settings::<CymaticsSettings>()` inserts the resource unconditionally at plugin-build time via `App::insert_resource`, so `Res<CymaticsSettings>` — not `Option<Res<_>>` — is already the established pattern for every other `OnEnter`-chained system in this file, e.g. `spawn_cymatics` at `mod.rs:409-416`)

**Why this is Task 1 and not preceded by a failing test:** see "Testing note" above. This step ships the real implementation, including a *candidate* value for the one field a screen must judge.

- [ ] **Step 1: Update the module doc**

At `crates/wc-sketches/src/cymatics/mod.rs:6-8`, replace:

```rust
//! 1. `OnEnter(AppState::Cymatics)` runs `init_cymatics_state` (insert the
//!    CPU-side [`CymaticsState`] defaults) then `spawn_cymatics` (read
//!    [`settings::CymaticsSettings`] → derive the sim resolution from the window
```

with:

```rust
//! 1. `OnEnter(AppState::Cymatics)` runs `init_cymatics_state` (insert a
//!    warm-started [`CymaticsState`] — see `warm_start_state`'s doc — so the
//!    field shows two distinct blobs immediately instead of a blank one) then
//!    `spawn_cymatics` (read
//!    [`settings::CymaticsSettings`] → derive the sim resolution from the window
```

The following line (`//!    aspect → allocate the two ping-pong textures`) is unchanged; only the three-line block above it is replaced by the four lines above.

- [ ] **Step 2: Replace `init_cymatics_state`, add the constant and the seed function**

At `crates/wc-sketches/src/cymatics/mod.rs:388-391`, replace the entire existing block:

```rust
/// `OnEnter(AppState::Cymatics)` — insert the resting [`CymaticsState`].
fn init_cymatics_state(mut commands: Commands<'_, '_>) {
    commands.insert_resource(CymaticsState::default());
}
```

with:

```rust
/// Alive-bloom ramp-clock seed for `warm_start_state`'s `ramp_time` field
/// (see `RAMP_TIME_CAP` and the shader's `(iter.time - 500.0) / 500.0` ramp,
/// `assets/shaders/cymatics/simulate.wgsl:114`). Below `500.0` that ramp term
/// is negative and the alive mask clamps to `0.0` everywhere, regardless of
/// `active_radius` or how far apart the two centres are — the third,
/// independent cause of the blank field.
///
/// Chosen by a human running `cargo rund` and looking at the rendered field
/// (Task 1 of
/// `docs/superpowers/plans/2026-07-09-alpha5-07-cymatics-warm-start.md`), not
/// derived from the shader formula on paper: there are no GPU tests in CI and
/// `cargo xtask capture` returns black frames for a backgrounded window, so
/// only a human looking at the screen can judge whether the seeded field
/// reads as "already blooming" versus an ugly instantaneous snap. `900.0` is
/// the shipped candidate: `min(0.8, (900.0 - 500.0) / 500.0) == 0.8`, i.e. the
/// ramp term is already fully saturated, matching how the field looks after
/// about 15 seconds of normal play at the default cadence (900 phase units).
/// If Task 1's visual review changes this value, update it here and nowhere
/// else — Task 2's tests read this constant rather than duplicating it.
const WARM_START_RAMP_TIME: f32 = 900.0;

/// Compute the [`CymaticsState`] a fresh `OnEnter(AppState::Cymatics)` should
/// seed, in place of [`CymaticsState::default()`]'s all-zero, overlapping
/// resting state.
///
/// Fixes three independent causes of the field's blank "blue screen of death"
/// look, which the field tester reported happening every time he cycled
/// through the picker into Cymatics — not just once at app boot:
///
/// 1. **`center` == `center2`**: both default to `(0.5, 0.5)`, so a bloomed
///    mask shows one blob, not the two the tester asked for. Seeded from
///    `screensaver::wander_centers` at `elapsed = 0.0`, which is already pure
///    and unit-tested and returns two separated points ((0.5, 0.8) and
///    approximately (0.80, 0.75)).
/// 2. **`active_radius` at its resting floor**: `MINIMUM_ACTIVE_RADIUS` =
///    `0.1` is, per `CymaticsSettings::attract_radius`'s own doc, "a nearly
///    invisible mask." Seeded to `settings.attract_radius` — the same live
///    Dev knob the screensaver's attract driver already treats as its calm-
///    pond target — so this warm start needs no tunable constant of its own
///    for this field. (If an operator has dragged `attract_radius` all the
///    way down to its own `0.1` floor, the seeded radius equals
///    `MINIMUM_ACTIVE_RADIUS` exactly rather than exceeding it; that operator
///    has already opted into a near-invisible mask everywhere else the
///    setting applies, so a matching warm start is consistent, not a
///    regression.)
/// 3. **`ramp_time` below the shader's bloom-ramp foot**: see
///    `WARM_START_RAMP_TIME`.
///
/// Pure: identical `settings` in always produces an identical `CymaticsState`
/// out — no `Time`, no RNG, no mutable global state is read — which is what
/// makes repeated `OnEnter(AppState::Cymatics)` cycles (the field tester's
/// exact repro: cycling through the picker four times in under a minute)
/// seed identically every time rather than drifting.
///
/// `num_cycles`, `slow_down`, `simulation_time`, and `center_speed` are left
/// at their [`CymaticsState::default()`] resting values: none of the three
/// causes above implicates them, and `simulation_time` in particular must
/// stay owned solely by `update_cymatics_sim_params` (see the module's design
/// note on the single-owner invariant). This function only chooses the
/// *starting* value that system reads on its first frame — exactly the
/// relationship `CymaticsState::default()` already had to it.
fn warm_start_state(settings: &CymaticsSettings) -> CymaticsState {
    let speeds = screensaver::LissajousSpeeds::from_settings(settings);
    let (center, center2) = screensaver::wander_centers(0.0, &speeds);
    CymaticsState {
        center,
        center2,
        active_radius: settings.attract_radius,
        ramp_time: WARM_START_RAMP_TIME,
        ..CymaticsState::default()
    }
}

/// `OnEnter(AppState::Cymatics)` — insert a warm-started [`CymaticsState`]
/// (see `warm_start_state`) instead of [`CymaticsState::default()`]'s
/// all-zero, overlapping-centres resting state.
///
/// Every entry into Cymatics — not just the app's first boot — allocates a
/// fresh ping-pong texture pair and re-inserts this resource (`spawn_cymatics`
/// runs immediately after, chained in the same `OnEnter`), so the old
/// `CymaticsState::default()` seed reproduced the field tester's "blue screen
/// of death" on every navigation into the sketch, not once at boot.
fn init_cymatics_state(mut commands: Commands<'_, '_>, settings: Res<'_, CymaticsSettings>) {
    commands.insert_resource(warm_start_state(&settings));
}
```

- [ ] **Step 3: Compile-check**

Run: `cargo check -p wc-sketches 2>&1 | tail -30`

Expected: PASS, no errors. (There is no automated test yet — see the Testing note above — so this step exists only to confirm the plumbing type-checks before the human spends time on the visual review.)

- [ ] **Step 4: Human visual review — the actual spike**

This step must be run by Madison, on the deployment-representative dev machine, not by an agent.

Run: `cargo rund`

Then:

1. From the Home picker, navigate into Cymatics.
2. Confirm: two distinct orange blobs are visible **immediately** — within the first rendered frame or two, not after a fade-in. No blank, near-black, or single-blob frame.
3. Navigate back to Home (Esc or the picker), then back into Cymatics. Repeat this navigate-away-and-back cycle 4–5 times in under a minute — this is the field tester's exact reported repro. Confirm every entry looks the same as the first: two blobs, immediately, every time.
4. If it looks right: leave `WARM_START_RAMP_TIME` at `900.0`, check off this step, and move to Task 2.
5. If it does not look right (e.g. the field looks like a hard, ugly pop rather than an already-settled bloom, or the two blobs still read as one), edit `WARM_START_RAMP_TIME` in `crates/wc-sketches/src/cymatics/mod.rs` directly — a higher value (up to `RAMP_TIME_CAP = 1000.0`) saturates the bloom ramp further; recall the ramp term is `min(0.8, (ramp_time - 500.0) / 500.0)`, so anything `>= 900.0` is already at the `+0.8` ceiling and further increases only matter below that. `cargo rund` picks up the change on the next incremental rebuild — no need to stop and restart the whole toolchain. Repeat from step 1.
6. **Record the final chosen value.** Whatever `WARM_START_RAMP_TIME` ends up as when you check off this step *is* the recorded value — it lives in the source file (per Step 2's doc comment, "update it here and nowhere else"). If you changed it from `900.0`, note the new value and a one-line reason in this checkbox's line when you check it off (e.g. `- [x] Step 4: ... changed WARM_START_RAMP_TIME to 750.0 — 900 looked slightly too abrupt`), so the commit message in Step 5 can cite it.

- [ ] **Step 5: Run the scoped gate and commit**

```bash
cargo fmt --all
cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings
cargo check -p wc-sketches
git add crates/wc-sketches/src/cymatics/mod.rs
```

Write the commit message to a file (backticks in `-m` get shell-substituted):

```bash
cat > /tmp/cymatics-warm-start-commit-1.txt <<'EOF'
fix(cymatics): seed a warm-started CymaticsState on every OnEnter

init_cymatics_state inserted CymaticsState::default() -- an all-zero,
overlapping-centres resting state -- on every OnEnter(AppState::Cymatics),
not just app boot. A field tester cycling through the picker into
Cymatics saw a blank "blue screen of death" field every time, confirmed
by four navigate-into-Cymatics events in under a minute in his log.

warm_start_state seeds three independently-blank-causing fields:
center/center2 (were both (0.5, 0.5); now screensaver::wander_centers at
t=0, already pure and tested), active_radius (was the MINIMUM_ACTIVE_RADIUS
resting floor, documented elsewhere as "a nearly invisible mask"; now the
live attract_radius Dev knob), and ramp_time (was 0.0, below the shader's
(time-500)/500 bloom-ramp foot; now WARM_START_RAMP_TIME, a constant
chosen by visual review under cargo rund -- there are no GPU tests in CI
and cargo xtask capture returns black frames for a backgrounded window,
so this constant could not be derived on paper).

This is a warm start in Active, not "enter attract mode" -- that
mechanism was considered and rejected (see the Plan 07 doc): it doesn't
fix the reported bug (which is about re-entry, not boot), can't work
without a cooldown the navigation input itself would immediately break,
and the cooldown effects (20fps throttle, closed settings panel, silenced
audio) are worse than the blank screen they'd replace.
EOF
git commit -F /tmp/cymatics-warm-start-commit-1.txt
```

---

### Task 2: Regression tests for `warm_start_state`

**Files:**
- Modify: `crates/wc-sketches/src/cymatics/mod.rs` — append to the existing `#[cfg(test)] mod tests` block at the file footer (after `register_cymatics_manifest_appends_entry`'s closing brace, before the module's final closing `}`)

**Interfaces:**
- Consumes: `warm_start_state`, `WARM_START_RAMP_TIME`, `MINIMUM_ACTIVE_RADIUS`, `RAMP_TIME_CAP`, `init_cymatics_state`, `CymaticsState::default()`, `CymaticsSettings::default()` — all landed in Task 1.
- Produces: nothing consumed by later tasks.

**Why no failing-test step:** see the Testing note above the tasks. Task 1's implementation is already correct by construction (the three seed rules are simple field assignments), so there is no bug for a first "red" run to catch. These tests exist to make the seeding behavior a permanent regression guard, not to discover a new one. The one place this could plausibly regress silently — someone "simplifying" `init_cymatics_state` back to `CymaticsState::default()` in a future refactor — is exactly what `warm_start_active_radius_clears_the_resting_floor` and `warm_start_ramp_time_clears_the_bloom_ramp_foot_and_stays_bounded` catch.

- [ ] **Step 1: Write the tests**

Append to `crates/wc-sketches/src/cymatics/mod.rs`, inside the existing `mod tests` block (it already has `use super::*;`, `use bevy::ecs::system::RunSystemOnce;`, and the module-level `#[allow(clippy::expect_used, ...)]` that covers `.expect()` calls below):

```rust
    /// `warm_start_state`: `center` and `center2` land at distinct points
    /// inside `[0,1]^2`, rather than the overlapping `(0.5, 0.5)` pair
    /// `CymaticsState::default()` produces (which makes a bloomed mask show
    /// only one blob, not two).
    #[test]
    fn warm_start_centers_are_distinct_and_in_unit_square() {
        let settings = CymaticsSettings::default();
        let state = warm_start_state(&settings);

        assert!(
            (0.0..=1.0).contains(&state.center.x) && (0.0..=1.0).contains(&state.center.y),
            "center must stay inside the sim UV field, got {:?}",
            state.center
        );
        assert!(
            (0.0..=1.0).contains(&state.center2.x) && (0.0..=1.0).contains(&state.center2.y),
            "center2 must stay inside the sim UV field, got {:?}",
            state.center2
        );
        assert!(
            state.center.distance(state.center2) > 0.1,
            "center and center2 must be visibly separated, not overlapping at (0.5, 0.5) each \
             like CymaticsState::default() (distance was {})",
            state.center.distance(state.center2)
        );
    }

    /// `warm_start_state`: the seeded `active_radius` clears the resting
    /// floor (`MINIMUM_ACTIVE_RADIUS` = 0.1) at default settings, and is
    /// sourced from the live `attract_radius` Dev knob rather than a new
    /// invented constant.
    #[test]
    fn warm_start_active_radius_clears_the_resting_floor() {
        let settings = CymaticsSettings::default();
        let state = warm_start_state(&settings);

        assert!(
            state.active_radius > MINIMUM_ACTIVE_RADIUS,
            "seeded active_radius ({}) must exceed the resting floor ({MINIMUM_ACTIVE_RADIUS}) \
             at default settings",
            state.active_radius
        );
        assert!(
            (state.active_radius - settings.attract_radius).abs() < f32::EPSILON,
            "active_radius must be seeded from the live attract_radius Dev knob, not a \
             hardcoded value"
        );
    }

    /// `warm_start_state`: `ramp_time` is seeded to `WARM_START_RAMP_TIME`
    /// (the human-reviewed constant from Task 1), which is above
    /// `CymaticsState::default()`'s resting `0.0` and within the bounds
    /// `update_cymatics_sim_params` maintains for the rest of the sketch's
    /// life (`RAMP_TIME_CAP`). This test intentionally reads
    /// `WARM_START_RAMP_TIME` rather than hardcoding a second copy of the
    /// number, so it stays correct no matter what Task 1's visual review
    /// landed on.
    #[test]
    fn warm_start_ramp_time_clears_default_and_stays_within_the_clock_cap() {
        let settings = CymaticsSettings::default();
        let state = warm_start_state(&settings);

        assert!(
            state.ramp_time > CymaticsState::default().ramp_time,
            "warm-started ramp_time must exceed the CymaticsState::default() resting value (0.0)"
        );
        assert!(
            state.ramp_time <= RAMP_TIME_CAP,
            "warm-started ramp_time ({}) must not exceed RAMP_TIME_CAP ({RAMP_TIME_CAP})",
            state.ramp_time
        );
        assert!(
            (state.ramp_time - WARM_START_RAMP_TIME).abs() < f32::EPSILON,
            "warm_start_state must seed exactly WARM_START_RAMP_TIME, the constant Task 1's \
             visual review landed on"
        );
    }

    /// `warm_start_state` is pure: identical settings in always produce a
    /// bit-identical `CymaticsState` out. No `Time`, no RNG, no mutable
    /// global state is read.
    #[test]
    fn warm_start_state_is_pure() {
        let settings = CymaticsSettings::default();
        let a = warm_start_state(&settings);
        let b = warm_start_state(&settings);

        assert!(a.center.distance(b.center) < f32::EPSILON);
        assert!(a.center2.distance(b.center2) < f32::EPSILON);
        assert!((a.active_radius - b.active_radius).abs() < f32::EPSILON);
        assert!((a.ramp_time - b.ramp_time).abs() < f32::EPSILON);
    }

    /// Repeated `OnEnter(AppState::Cymatics)` cycles — the field tester's
    /// exact reproduction ("cycling thru" the picker four times in under a
    /// minute) — seed an identical `CymaticsState` every time, not a
    /// drifting or progressively-blanker one. Runs the real
    /// `init_cymatics_state` system (not just `warm_start_state` directly)
    /// through the same remove-then-reinsert cycle `OnExit`/`OnEnter`
    /// perform in the real app.
    #[test]
    fn repeated_on_enter_seeds_identical_state_each_time() {
        let mut world = World::new();
        world.insert_resource(CymaticsSettings::default());

        world
            .run_system_once(init_cymatics_state)
            .expect("init_cymatics_state run (first entry)");
        let first = world.resource::<CymaticsState>().clone();

        // Mirrors OnExit's remove_cymatics_sim_params dropping CymaticsState,
        // then a second OnEnter re-inserting it.
        let _ = world.remove_resource::<CymaticsState>();
        world
            .run_system_once(init_cymatics_state)
            .expect("init_cymatics_state run (second entry)");
        let second = world.resource::<CymaticsState>().clone();

        assert!(first.center.distance(second.center) < f32::EPSILON);
        assert!(first.center2.distance(second.center2) < f32::EPSILON);
        assert!((first.active_radius - second.active_radius).abs() < f32::EPSILON);
        assert!((first.ramp_time - second.ramp_time).abs() < f32::EPSILON);
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p wc-sketches --lib cymatics::tests 2>&1 | tail -40`

Expected: PASS — all five new tests plus the file's existing `cymatics::tests` suite green. (No red step; see the Testing note above.)

- [ ] **Step 3: Run the scoped gate and commit**

```bash
cargo fmt --all
cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings
cargo test -p wc-sketches --lib cymatics::tests
git add crates/wc-sketches/src/cymatics/mod.rs
```

```bash
cat > /tmp/cymatics-warm-start-commit-2.txt <<'EOF'
test(cymatics): lock in warm_start_state's seeding behaviour

Five regression tests over the pure warm_start_state function and the
init_cymatics_state system Task 1 landed: center/center2 are distinct
and stay in [0,1]^2, active_radius clears the resting floor and tracks
the live attract_radius setting, ramp_time clears the default and stays
within RAMP_TIME_CAP, the seed function is pure, and repeated OnEnter
cycles (the field tester's exact repro) seed identically every time.

The ramp_time bound and equality tests read WARM_START_RAMP_TIME rather
than a hardcoded literal, so they stay correct regardless of the exact
value Task 1's human visual review settled on -- there are no GPU tests
in CI, so that value could not be derived or asserted on paper.
EOF
git commit -F /tmp/cymatics-warm-start-commit-2.txt
```

---

### Task 3: Record the warm start in `PARITY.md`, then run the full workspace gate

**Files:**
- Modify: `crates/wc-sketches/src/cymatics/PARITY.md:240-242` (operator pre-tag checklist, "Visual" section)

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing consumed by other tasks — documentation only.

`PARITY.md` is the operator-facing pre-tag checklist for this sketch (see its own header: "Status: PENDING — operator sign-off required before tagging"). Its existing "Non-black field" bullet only covers the first-boot case; without an update it would not tell a future operator to check the case this plan actually fixes (repeated entry).

- [ ] **Step 1: Update the checklist**

At `crates/wc-sketches/src/cymatics/PARITY.md:240-242`, after the existing "Non-black field" bullet:

```markdown
- [ ] **Non-black field**: confirm the wave field is visible (dark-blue ground, concentric
  ripple from the centre). A black screen indicates `rgba32float` write-only storage is unsupported
  or the compute pipeline failed to compile — check the log for a `PipelineCache` error.
```

insert a new bullet immediately after it:

```markdown
- [ ] **Warm start on every entry, not just first boot** (Plan 07,
  `docs/superpowers/plans/2026-07-09-alpha5-07-cymatics-warm-start.md`): from Home, navigate into
  Cymatics, back to Home, then into Cymatics again, several times in a row (the field tester's
  exact repro was four navigations in under a minute). Confirm two distinct orange blobs are
  visible immediately on every entry — no blank or near-black frame while the field "warms up."
  A blank frame on the *second or later* entry (but not the first) means `warm_start_state`
  regressed to `CymaticsState::default()`'s all-zero seed.
```

- [ ] **Step 2: Run the full workspace gate**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
```

Expected: all pass. This is the first workspace-wide (not `-p wc-sketches`-scoped) run in this plan; it exists to catch anything the scoped gates in Tasks 1–2 could not see, including the doc gate (which Tasks 1–2 did not run at all) and any interaction with the rest of the workspace.

- [ ] **Step 3: Commit**

```bash
git add crates/wc-sketches/src/cymatics/PARITY.md
```

```bash
cat > /tmp/cymatics-warm-start-commit-3.txt <<'EOF'
docs(cymatics): add a repeated-entry check to the warm start's PARITY.md

The existing "Non-black field" checklist item only covers first boot.
Add a bullet for the case this plan actually fixes: navigating into
Cymatics repeatedly must show two blobs immediately every time, not
just the first.
EOF
git commit -F /tmp/cymatics-warm-start-commit-3.txt
```

---

## Self-Review

**Spike-first structure.** Task 1 ships working code, including a reasoned *candidate* value (`900.0`, chosen because it already saturates the shader's `min(0.8, (ramp_time-500)/500)` ramp term) for the one field — `ramp_time` — that genuinely cannot be judged without a screen. No test anywhere in this plan asserts that `900.0` is correct; Task 2's tests read `WARM_START_RAMP_TIME` by name and check bounds/deltas against it, so they remain valid whatever value Task 1's human review lands on, including a value the human changes after this plan is written.

**All three seed parameters covered.** `center`/`center2` (Task 1 Step 2's `warm_start_state`, cause 1; tested by `warm_start_centers_are_distinct_and_in_unit_square`), `active_radius` (cause 2; tested by `warm_start_active_radius_clears_the_resting_floor`), `ramp_time` (cause 3; tested by `warm_start_ramp_time_clears_default_and_stays_within_the_clock_cap`).

**Single-owner invariant.** Addressed in its own design-note section above the tasks: `update_cymatics_sim_params` remains the sole per-frame advancer of `simulation_time` and `ramp_time` (verified by `rg -n "ramp_time" crates/wc-sketches/src/cymatics/` during research — the only non-test writers are the `Default` impl, `warm_start_state`, and `update_cymatics_sim_params` itself); `init_cymatics_state` is a per-entry constructor, the same role it already played, and construction and per-frame advancement never overlap in the same frame because `OnExit` fully drops the resource before the next `OnEnter` reconstructs it.

**`wander_centers` reused, not reimplemented.** `warm_start_state` calls `screensaver::wander_centers(0.0, &speeds)` directly. Visibility check: `wander_centers` is already `pub fn` inside an already-`pub mod screensaver` (`mod.rs:44`), so **no visibility change is needed** — this is called out explicitly in Task 1's Interfaces section rather than left implicit.

**Placeholder scan.** No "TBD", no "similar to Task N", no elided code — every code block in Tasks 1–3 is complete and directly pasteable. The one deliberately-open value (`WARM_START_RAMP_TIME`'s literal) is not a placeholder; it is a real, working default the human review may keep or change in place, per the Testing note.

**Type consistency.** `warm_start_state(settings: &CymaticsSettings) -> CymaticsState` — matches `CymaticsSettings` (already imported in `mod.rs` via `use settings::CymaticsSettings;`) and `CymaticsState` (defined in the same file). `init_cymatics_state`'s new `Res<'_, CymaticsSettings>` parameter matches the pattern already used by `spawn_cymatics`, `update_cymatics_sim_params`, and `update_cymatics_material` in the same file. `screensaver::wander_centers(elapsed: f32, speeds: &LissajousSpeeds) -> (Vec2, Vec2)` and `screensaver::LissajousSpeeds::from_settings(s: &CymaticsSettings) -> LissajousSpeeds` are used with exactly their declared signatures.

**Clippy hygiene of the example code.** No `.expect()`/`.unwrap()` outside the existing `mod tests` block (which already carries the module-level `#[allow(clippy::expect_used, ...)]`). No `assert_eq!(x.is_some(), true)`-style comparisons. No `0..(N+1)` ranges. No bare `Default::default()` (all uses are the explicit `CymaticsState::default()`, which is the pattern clippy's `default_trait_access` prefers, not the one it warns on). No new `as` casts. All float comparisons use `.abs() < f32::EPSILON` or `.distance() < f32::EPSILON`, matching this file's established idiom throughout its existing test suite.
