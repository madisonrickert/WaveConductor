# Configurable Attract-Mode Timeout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let operators configure how long the app waits before entering attract mode (screensaver), instead of the hardcoded 60 s split across `InteractionTimer`'s two internal stages.

**Architecture:** Add a persisted `attract_mode_timeout_secs` field to the existing `ScreensaverSettings` resource (User settings panel), and a small Bevy system that splits it evenly into `InteractionTimer::idle_threshold` / `::screensaver_threshold` whenever the setting changes (including on initial load).

**Tech Stack:** Rust, Bevy ECS (resources, run conditions, system ordering), the project's `#[setting(...)]` / `SketchSettings` derive macro (`wc-core-macros`) for settings-panel + persistence wiring, `toml` (via `serde`) for persistence round-tripping.

## Global Constraints

- No `unwrap()`/`expect()` in non-test code (AGENTS.md).
- `///` rustdoc on every new public item; update the two module-level `//!` doc comments this work touches (AGENTS.md).
- No new dependencies — everything needed already exists in `wc-core`'s dependency graph (spec goals).
- Preserve today's default behavior exactly: default `attract_mode_timeout_secs = 60.0` must reproduce the current hardcoded 30 s / 30 s split (spec goals).
- Guard against a hand-edited TOML producing a degenerate value: `Duration::from_secs_f32` panics on negative, infinite, or NaN input, so the split function must clamp first — mirroring the existing `SCREENSAVER_FPS_MIN`/`SCREENSAVER_FPS_MAX` pattern already in `screensaver/mod.rs` (spec design, "Sync into InteractionTimer").
- `idle_threshold` / `screensaver_threshold` stay separate mutable `Duration` fields on `InteractionTimer` — do not collapse them or touch `rewind_past_screensaver` / `apply_force_screensaver` (spec non-goals).
- Run the CI-equivalent gates (`cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features --workspace -- -D warnings`, `cargo nextest run --workspace --all-features`, `cargo doc --no-deps --workspace --document-private-items`) before calling this done (AGENTS.md "Verifying changes").

**Spec:** `docs/superpowers/specs/2026-07-08-configurable-attract-mode-timeout-design.md`

---

### Task 1: Add the persisted `attract_mode_timeout_secs` setting

**Files:**
- Modify: `crates/wc-core/src/lifecycle/screensaver/settings.rs`
- Test: same file, `#[cfg(test)] mod tests` at the file footer

**Interfaces:**
- Produces: `ScreensaverSettings::attract_mode_timeout_secs: f32` (pub field, default 60.0, User-panel range 10.0–600.0). Consumed by Task 2.
- Produces: `fn default_attract_mode_timeout_secs() -> f32` (private serde fallback, same file).

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` block (after `legacy_toml_with_caption_keys_still_parses`):

```rust
    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "test-only: comparing an exact literal default"
    )]
    fn attract_mode_timeout_defaults_to_60_seconds() {
        assert_eq!(
            ScreensaverSettings::default().attract_mode_timeout_secs,
            60.0
        );
    }

    /// Forward-compat: TOML persisted before `attract_mode_timeout_secs`
    /// existed (only setting `screensaver_fps`) still parses, landing the
    /// new field on its documented default.
    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "test-only: comparing an exact literal default"
    )]
    fn legacy_toml_without_attract_timeout_key_still_parses() {
        let legacy = "screensaver_fps = 25.0";
        let parsed: ScreensaverSettings = toml::from_str(legacy).expect("legacy TOML must parse");
        assert_eq!(parsed.screensaver_fps, 25.0);
        assert_eq!(parsed.attract_mode_timeout_secs, 60.0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p wc-core screensaver`
Expected: FAIL — compile error, `no field 'attract_mode_timeout_secs' on type 'ScreensaverSettings'`.

- [ ] **Step 3: Add the field**

In the struct body, immediately after the `keep_display_awake` field (before the struct's closing `}`):

```rust
    /// Total time of inactivity (mouse, keyboard, touch, or hand tracking)
    /// before the screensaver's attract mode begins. Split evenly by
    /// [`crate::lifecycle::screensaver::sync_attract_timeout_from_settings`]
    /// into [`crate::lifecycle::idle::InteractionTimer`]'s two internal
    /// stages (`Active → Idle` throttles hand-tracking inference and freezes
    /// some sketch dispatches; `Idle → Screensaver` shows the attract
    /// visual) — that split is an implementation detail, not
    /// operator-facing. Default 60 (30 s + 30 s), matching the app's
    /// long-standing hardcoded behavior before this setting existed.
    #[setting(
        default = 60.0_f32,
        min = 10.0,
        max = 600.0,
        step = 5.0,
        section = "Attract Mode",
        category = User,
        label = "Idle timeout",
        unit = "s"
    )]
    #[serde(default = "default_attract_mode_timeout_secs")]
    pub attract_mode_timeout_secs: f32,
```

Then, after the existing `default_keep_display_awake` function:

```rust
/// Serde fallback so a config saved before `attract_mode_timeout_secs`
/// existed still loads at the documented default (today's hardcoded 60 s).
fn default_attract_mode_timeout_secs() -> f32 {
    60.0
}
```

Also update the struct's doc comment (the block starting `/// Operator-customizable attract-mode parameters.`) by appending a paragraph:

```rust
/// Operator-customizable attract-mode parameters.
///
/// Lives as a Bevy `Resource`; the overlay reads it with `Res<ScreensaverSettings>`.
/// Registered with the settings system via `register_sketch_settings` so it
/// appears in the User panel and round-trips through persistence.
///
/// `attract_mode_timeout_secs` is read by
/// [`crate::lifecycle::screensaver::sync_attract_timeout_from_settings`],
/// which splits it evenly into
/// [`crate::lifecycle::idle::InteractionTimer`]'s two thresholds; the other
/// two fields below are read directly by the framework's present-rate
/// throttle and OS display-sleep-inhibit systems.
```

And the file's top module doc comment line (currently `//! present-rate cap.`) becomes:

```rust
//! present-rate cap and the idle-to-attract-mode timeout.
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p wc-core screensaver`
Expected: PASS (all tests in `lifecycle::screensaver::settings::tests` and the pre-existing ones).

- [ ] **Step 5: Run the crate's lint/format gates**

Run: `cargo fmt --all -- --check && cargo clippy -p wc-core --all-targets --all-features -- -D warnings`
Expected: both clean (no diffs, no warnings).

- [ ] **Step 6: Commit**

```bash
git add crates/wc-core/src/lifecycle/screensaver/settings.rs
git commit -m "$(cat <<'EOF'
feat(screensaver): add operator-configurable attract-mode timeout setting

Adds attract_mode_timeout_secs to ScreensaverSettings (User panel, default
60s matching today's hardcoded behavior). Not yet wired to InteractionTimer —
that lands in the next commit.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Sync the setting into `InteractionTimer`

**Files:**
- Modify: `crates/wc-core/src/lifecycle/screensaver/mod.rs`
- Test: same file, `#[cfg(test)] mod tests` at the file footer

**Interfaces:**
- Consumes: `settings::ScreensaverSettings::attract_mode_timeout_secs: f32` (Task 1). `crate::lifecycle::idle::InteractionTimer::{idle_threshold, screensaver_threshold}: Duration` (pre-existing, both `pub`).
- Produces: `fn split_attract_timeout(total_secs: f32) -> (Duration, Duration)` (private, pure). `fn sync_attract_timeout_from_settings(...)` (private Bevy system), registered in `ScreensaverPlugin::build`.

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` block in `crates/wc-core/src/lifecycle/screensaver/mod.rs`:

```rust
    #[test]
    fn split_attract_timeout_splits_evenly() {
        // The 60 s default reproduces today's hardcoded 30 s / 30 s split.
        assert_eq!(
            split_attract_timeout(60.0),
            (Duration::from_secs(30), Duration::from_secs(30))
        );
        assert_eq!(
            split_attract_timeout(10.0),
            (Duration::from_secs(5), Duration::from_secs(5))
        );
    }

    #[test]
    fn split_attract_timeout_clamps_degenerate_values() {
        // A hand-edited TOML with a zero/negative/absurd/NaN total must not
        // panic Duration::from_secs_f32 (which panics on all four).
        let floor = Duration::from_secs_f32(ATTRACT_TIMEOUT_MIN_SECS / 2.0);
        assert_eq!(split_attract_timeout(0.0), (floor, floor));
        assert_eq!(split_attract_timeout(-10.0), (floor, floor));

        let ceiling = Duration::from_secs_f32(ATTRACT_TIMEOUT_MAX_SECS / 2.0);
        assert_eq!(split_attract_timeout(1_000_000.0), (ceiling, ceiling));

        let (idle, screensaver) = split_attract_timeout(f32::NAN);
        assert_eq!(idle, floor);
        assert_eq!(screensaver, floor);
    }

    #[test]
    fn sync_attract_timeout_only_writes_when_settings_change() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<crate::lifecycle::idle::InteractionTimer>();
        app.insert_resource(settings::ScreensaverSettings {
            attract_mode_timeout_secs: 20.0,
            ..settings::ScreensaverSettings::default()
        });
        app.add_systems(
            Update,
            sync_attract_timeout_from_settings
                .run_if(resource_changed::<settings::ScreensaverSettings>),
        );

        app.update(); // initial insertion counts as a change
        {
            let timer = app
                .world()
                .resource::<crate::lifecycle::idle::InteractionTimer>();
            assert_eq!(timer.idle_threshold, Duration::from_secs(10));
            assert_eq!(timer.screensaver_threshold, Duration::from_secs(10));
        }

        // Hand-edit the timer to a sentinel value with the setting untouched:
        // the next update must NOT overwrite it, since ScreensaverSettings
        // hasn't changed.
        app.world_mut()
            .resource_mut::<crate::lifecycle::idle::InteractionTimer>()
            .idle_threshold = Duration::from_secs(999);
        app.update();
        assert_eq!(
            app.world()
                .resource::<crate::lifecycle::idle::InteractionTimer>()
                .idle_threshold,
            Duration::from_secs(999),
            "system must not run when ScreensaverSettings is unchanged"
        );

        // Now actually change the setting: the next update picks it up.
        app.world_mut()
            .resource_mut::<settings::ScreensaverSettings>()
            .attract_mode_timeout_secs = 100.0;
        app.update();
        let timer = app
            .world()
            .resource::<crate::lifecycle::idle::InteractionTimer>();
        assert_eq!(timer.idle_threshold, Duration::from_secs(50));
        assert_eq!(timer.screensaver_threshold, Duration::from_secs(50));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p wc-core screensaver`
Expected: FAIL — compile error, `cannot find function 'split_attract_timeout'` (and `sync_attract_timeout_from_settings`, `ATTRACT_TIMEOUT_MIN_SECS`, `ATTRACT_TIMEOUT_MAX_SECS`) in this scope.

- [ ] **Step 3: Implement the split function and the sync system**

Add near the existing `SCREENSAVER_FPS_MIN`/`SCREENSAVER_FPS_MAX` constants (same file):

```rust
/// Guard rails on the persisted `attract_mode_timeout_secs` setting,
/// mirroring [`SCREENSAVER_FPS_MIN`]/[`SCREENSAVER_FPS_MAX`]: a hand-edited
/// TOML outside the slider's 10–600 s range is clamped here rather than
/// feeding a degenerate (zero/negative/NaN/infinite) value into
/// `Duration::from_secs_f32`, which panics on all four.
const ATTRACT_TIMEOUT_MIN_SECS: f32 = 1.0;
const ATTRACT_TIMEOUT_MAX_SECS: f32 = 3600.0;

/// Split the operator-facing "time until attract mode" into the two
/// [`crate::lifecycle::idle::InteractionTimer`] thresholds it drives,
/// evenly: half throttles hand-tracking inference and freezes some sketch
/// dispatches (`Active → Idle`), half shows the attract visual
/// (`Idle → Screensaver`). Extracted as a pure function so the split (and
/// its degenerate-value guard) is unit-tested directly, matching this
/// module's `tier_present_wait` / `screensaver_update_mode` pattern.
#[must_use]
#[allow(
    clippy::manual_clamp,
    reason = "max().min() is deliberate: clamp() passes NaN through, and a NaN duration panics \
              in Duration::from_secs_f32 — max/min sanitize a degenerate persisted TOML to the rail"
)]
fn split_attract_timeout(total_secs: f32) -> (Duration, Duration) {
    let clamped = total_secs
        .max(ATTRACT_TIMEOUT_MIN_SECS)
        .min(ATTRACT_TIMEOUT_MAX_SECS);
    let half = Duration::from_secs_f32(clamped / 2.0);
    (half, half)
}

/// Copy the operator's `attract_mode_timeout_secs` setting into
/// [`crate::lifecycle::idle::InteractionTimer`] whenever it changes —
/// including on initial load, so a persisted non-default value takes effect
/// from the first frame rather than only after the next edit. Registered to
/// run before [`crate::lifecycle::idle::advance_activity`] so a same-frame
/// change is visible to it immediately.
fn sync_attract_timeout_from_settings(
    settings: Res<'_, settings::ScreensaverSettings>,
    mut timer: ResMut<'_, crate::lifecycle::idle::InteractionTimer>,
) {
    let (idle, screensaver) = split_attract_timeout(settings.attract_mode_timeout_secs);
    timer.idle_threshold = idle;
    timer.screensaver_threshold = screensaver;
}
```

In `ScreensaverPlugin::build`, immediately after
`app.register_sketch_settings::<settings::ScreensaverSettings>();`, add:

```rust
        // Sync the operator's "time until attract mode" setting into the
        // idle timer, split evenly across its two internal stages.
        // `resource_changed` fires on initial insertion too, so a persisted
        // non-default value takes effect from the first frame.
        app.add_systems(
            Update,
            sync_attract_timeout_from_settings
                .run_if(resource_changed::<settings::ScreensaverSettings>)
                .before(crate::lifecycle::idle::advance_activity),
        );
```

Finally, update the module's top doc-comment bullet (currently):

```rust
//! - The core [`settings::ScreensaverSettings`] resource (the FPS cap; the
//!   former operator caption was cut 2026-06-10 — see the settings module).
```

to:

```rust
//! - The core [`settings::ScreensaverSettings`] resource (the FPS cap; the
//!   former operator caption was cut 2026-06-10 — see the settings module)
//!   plus the operator's idle-to-attract-mode timeout, synced into
//!   [`crate::lifecycle::idle::InteractionTimer`] by
//!   [`sync_attract_timeout_from_settings`].
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p wc-core screensaver`
Expected: PASS (all new tests plus every pre-existing test in this file and `settings.rs`).

- [ ] **Step 5: Run the full test file's neighbor integration suite**

Run: `cargo nextest run -p wc-core --test screensaver`
Expected: PASS — confirms the new `Update`-schedule system registration doesn't disturb the existing end-to-end lifecycle/fade/present-rate integration tests in `crates/wc-core/tests/screensaver.rs`.

- [ ] **Step 6: Run the crate's lint/format gates**

Run: `cargo fmt --all -- --check && cargo clippy -p wc-core --all-targets --all-features -- -D warnings`
Expected: both clean.

- [ ] **Step 7: Commit**

```bash
git add crates/wc-core/src/lifecycle/screensaver/mod.rs
git commit -m "$(cat <<'EOF'
feat(screensaver): wire attract-mode timeout setting into InteractionTimer

sync_attract_timeout_from_settings splits attract_mode_timeout_secs evenly
into InteractionTimer's idle/screensaver thresholds on load and on every
settings change, so the operator-facing setting added in the prior commit
now actually controls when attract mode begins.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Full verification pass

**Files:** none (verification only — no code changes expected; if a gate below surfaces something, fix it in place before committing).

**Interfaces:** none — this task consumes the finished feature from Tasks 1–2 and confirms it against the project's full CI-equivalent gate list (AGENTS.md "Verifying changes").

- [ ] **Step 1: Formatting**

Run: `cargo fmt --all -- --check`
Expected: no output (clean).

- [ ] **Step 2: Lints (hard errors)**

Run: `cargo clippy --all-targets --all-features --workspace -- -D warnings`
Expected: `Finished` with zero warnings.

- [ ] **Step 3: Full workspace test suite**

Run: `cargo nextest run --workspace --all-features`
Expected: all tests pass, including the new ones in `wc-core`'s `lifecycle::screensaver::settings::tests` and `lifecycle::screensaver::tests`, plus the pre-existing `crates/wc-core/tests/screensaver.rs` integration suite.

- [ ] **Step 4: Doctests**

Run: `cargo test --doc --workspace`
Expected: all pass (nextest doesn't run doctests, per AGENTS.md).

- [ ] **Step 5: Docs build**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --document-private-items`
Expected: clean build, no broken intra-doc links (the new `[\`...\`]` links added in Tasks 1–2 resolve: `crate::lifecycle::screensaver::sync_attract_timeout_from_settings`, `crate::lifecycle::idle::InteractionTimer`).

- [ ] **Step 6: Secrets/path lint**

Run: `cargo xtask check-secrets`
Expected: clean (no new paths/emails/secrets were introduced).

- [ ] **Step 7: Manual smoke test**

Run: `cargo rund`, open the User settings panel, navigate to the "Attract Mode" section, confirm "Idle timeout" appears with the 60 s default and a 10–600 s slider range, lower it to e.g. 15 s, and confirm attract mode now engages after ~15 s of no input instead of ~60 s.

- [ ] **Step 8: Fix-forward commit (only if any gate above required a change)**

If every gate in Steps 1–6 was already clean, skip this step — Tasks 1 and 2 already committed the complete feature. Otherwise:

```bash
git add -A
git commit -m "$(cat <<'EOF'
fix(screensaver): address CI gate findings on attract-mode timeout feature

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```
