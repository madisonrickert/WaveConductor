# Configurable attract-mode timeout

Date: 2026-07-08

## Problem

The idle timer that drives `Active → Idle → Screensaver` (attract mode) is
hardcoded: `InteractionTimer::default()` in
`crates/wc-core/src/lifecycle/idle.rs` sets `idle_threshold` and
`screensaver_threshold` to 30 s each (60 s total to attract mode), and neither
field is exposed through the settings system. Operators running an unattended
install have no way to change how long the app waits before entering attract
mode without editing source and rebuilding.

`ScreensaverSettings` (`crates/wc-core/src/lifecycle/screensaver/settings.rs`)
already exposes two related, persisted, User-panel settings for attract mode
(`screensaver_fps`, `keep_display_awake`) — the natural place to add a third.

## Goals

- Operator-configurable "time until attract mode" in the User settings panel,
  persisted like every other sketch/core setting.
- Preserve today's default behavior exactly (60 s total) so existing installs
  see no behavior change until an operator adjusts it.
- No change to the internal two-stage model (`Idle` throttles hand-tracking
  inference and freezes some sketch dispatches; `Screensaver` shows the
  attract visual) — that distinction stays internal, not operator-facing.

## Non-goals

- Exposing `idle_threshold` and `screensaver_threshold` as independent
  settings. A single combined "time until attract mode" number is the
  operator's actual mental model; the two-stage split is an implementation
  detail.
- Any change to the `Shift+S` skip or the `WC_DEBUG_FORCE_SCREENSAVER` debug
  toggle, both of which continue to mutate `InteractionTimer`'s fields
  directly and take precedence when active.

## Design

### New setting

Add to `ScreensaverSettings`:

```rust
#[setting(
    default = 60.0_f32,
    min = 10.0_f32,
    max = 600.0_f32,
    step = 5.0_f32,
    section = "Attract Mode",
    category = User,
    label = "Idle timeout",
    unit = "s"
)]
#[serde(default = "default_attract_mode_timeout_secs")]
pub attract_mode_timeout_secs: f32,
```

with `fn default_attract_mode_timeout_secs() -> f32 { 60.0 }`, following the
file's existing `#[serde(default = "...")]` forward-compat convention (a TOML
file persisted before this field existed still deserializes cleanly).

### Sync into `InteractionTimer`

`InteractionTimer::idle_threshold` and `::screensaver_threshold` remain
separate mutable `Duration` fields — they're written directly by
`InteractionTimer::rewind_past_screensaver` (`Shift+S` skip) and by the
`#[cfg(debug_assertions)]` `apply_force_screensaver` toggle, and unit tests
set them directly. Collapsing them into a single field would touch that
existing machinery for no benefit.

Instead, add a system in `crates/wc-core/src/lifecycle/screensaver/mod.rs`
(which already depends on `lifecycle::idle`, not the reverse) that splits the
setting evenly across both stages:

```rust
fn sync_attract_timeout_from_settings(
    settings: Res<'_, ScreensaverSettings>,
    mut timer: ResMut<'_, crate::lifecycle::idle::InteractionTimer>,
) {
    let half = Duration::from_secs_f32(settings.attract_mode_timeout_secs / 2.0);
    timer.idle_threshold = half;
    timer.screensaver_threshold = half;
}
```

Registered in `ScreensaverPlugin::build`:

```rust
app.add_systems(
    Update,
    sync_attract_timeout_from_settings
        .run_if(resource_changed::<ScreensaverSettings>)
        .before(crate::lifecycle::idle::advance_activity),
);
```

`resource_changed::<ScreensaverSettings>` is true on initial insertion as well
as on every subsequent edit, so a persisted non-default value takes effect
from the first frame, not just after the operator re-touches the setting in
the panel. At the 60 s default this reproduces today's 30 s / 30 s split
exactly.

Ordering relative to the debug-only `apply_force_screensaver` system doesn't
need to be pinned: the two only matter on the same frame if an operator edits
`ScreensaverSettings` in the exact frame the `WC_DEBUG_FORCE_SCREENSAVER`
toggle is active, which isn't a real operating scenario.

### Docs

- Update the `ScreensaverPlugin` module doc-comment bullet list
  (`screensaver/mod.rs`) to mention the new setting.
- Update the `ScreensaverSettings` struct doc comment to describe
  `attract_mode_timeout_secs` alongside the existing two fields.

## Testing

- `ScreensaverSettings`: default value test and a legacy-TOML round-trip test
  (mirrors the existing `legacy_toml_with_caption_keys_still_parses` test),
  confirming a config saved before this field existed still loads at the
  documented 60 s default.
- `sync_attract_timeout_from_settings`: a Bevy `App`-based test confirming a
  given `attract_mode_timeout_secs` splits evenly into
  `InteractionTimer::idle_threshold` / `::screensaver_threshold`, and that the
  system only writes when `ScreensaverSettings` actually changes (matching the
  existing `advance_activity` / `apply_force_screensaver` test style already
  in `idle.rs` and `screensaver/mod.rs`).

## Out of scope / follow-ups

None identified — this is a self-contained addition to an existing settings
struct plus one small sync system.
