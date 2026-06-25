# Design: consistent tonemapping + per-sketch glow tuning

Date: 2026-06-25
Status: Approved pending spec review

## Background

The backdrop-blur render node called `ViewTarget::post_process_write()` only to
read the frame, but that call flips the ping-pong buffers as a side effect.
Because the node never wrote the destination, the stray flip left the
**pre-tonemap** buffer as the displayed frame whenever the node ran — so the
camera's AgX tonemap was bypassed. The node is gated on UI opacity, so the bypass
toggled with the chrome auto-fade (30 s idle threshold, 0.6 s fade): vivid
pre-tonemap while the chrome showed, then a sudden mute to the real tonemapped
image when it faded. That mute read as a "gray pop" ~0.5 s after the screensaver
engaged.

Fixed in `crates/wc-core/src/ui/blur/node.rs` by reading the frame with
`main_texture_view()` (a plain read, no ping-pong side effect), so the tonemap
always lands.

With tonemapping now consistent, AgX's highlight desaturation became visible
everywhere (not just the screensaver). The operator prefers a chroma-preserving
"neon glow." The screensaver-only brightness/saturation compensation was a
workaround for the muted look and is now obsolete.

## Goals

- Consistent tonemapping in active play and screensaver (no pop) — already done
  by the node fix; this spec makes the *look* intentional.
- A "neon glow": bright blooming highlights that keep their color.
- Per-sketch tuning knobs with informed defaults the operator fine-tunes from.
- Home/picker screen stays SDR (its art is already SDR; no tonemap).
- Remove the obsolete screensaver color compensation.

## Non-goals

- Changing source emissive values inside sketch shaders (per-sketch art, separate).
- Bloom knobs beyond intensity + threshold (no scatter/scale knobs yet).
- Removing Line/Dots `attract_brightness` (deferred; revisit if the attract field
  reads too dim under Reinhard).

## Design

### Tonemapping — per state

- The main `Camera2d` spawns with `Tonemapping::None` (SDR baseline). Home/picker
  therefore stays SDR — the picker tiles are SDR PNGs and must not be pushed
  through a tonemap curve.
- Each sketch gets a `tonemapping` enum setting (default `ReinhardLuminance`).
- A shared `TonemapChoice` Reflect enum lives in `wc-core` with unit variants
  `{ ReinhardLuminance, TonyMcMapface, AgX, AcesFitted, None }` and a
  `fn to_bevy(self) -> bevy::core_pipeline::tonemapping::Tonemapping`. Centralizing
  it keeps sketch crates from depending on the Bevy tonemapping module directly
  and guarantees the settings dropdown variants match the mapping.
- On `OnEnter(AppState::<sketch>)`, a per-sketch system sets the camera's
  `Tonemapping` from that sketch's setting. On `OnExit` (return to Home), reset to
  `None`. Mirrors the save/restore pattern the screensaver present-rate throttle
  already uses (`crates/wc-core/src/lifecycle/screensaver/mod.rs`).
- The screensaver is a sub-state of the sketch, so it inherits the sketch's
  tonemapping — active and attract render identically.
- Category: Dev (a set-once aesthetic lever; the value persists in TOML even
  though the ADVANCED toggle's visibility resets each launch).

### Bloom — per sketch

- Per-sketch `bloom_intensity` (default **0.35**, up from the global 0.15) and
  `bloom_threshold` (default **0.7**, up from 0.0).
- Applied to the camera's `Bloom` component on `OnEnter(<sketch>)`; restored to
  the spawn default (0.15 / 0.0) on exit/home, same mechanism as the tonemapping
  apply.
- Rationale: threshold > 0 means only HDR cores bloom, giving crisp midtones with
  glowing bright spots instead of a full-frame haze — the primary "beautiful glow"
  lever. A sketch can set threshold back to 0 for the dreamy look.
- Category: Dev.

### Brightness / gamma — per sketch

- Cymatics already has `master_brightness` + `gamma`. Add `master_brightness` to
  Line and Dots (both already have `gamma`). Defaults 1.0 / 1.0. Category: User.
- These place each sketch's content on the Reinhard curve. Reinhard is lower
  contrast than AgX, so the operator may nudge gamma to ~1.1–1.2 per sketch for
  pop; left at 1.0 so that stays a deliberate choice.
- Plumbing: cymatics already routes both through its material `skew` lane; Line
  and Dots route `gamma` through the particle material and gain a
  `master_brightness` lane the same way.

### Removals

- Cymatics screensaver color compensation:
  - Remove the `attract_saturation` setting (+ its serde default + tests).
  - Remove the `skew.w` saturation lane in `assets/shaders/cymatics/render.wgsl`
    and the saturation packing in `update_cymatics_material`.
  - Remove the cymatics `attract_brightness` fade lift; the material's brightness
    becomes plain `master_brightness` (no `ScreensaverFade` term). Drop the
    cymatics `attract_brightness` setting.
  - The shared `ScreensaverFade` resource stays (Line/Dots still use it); cymatics
    simply stops consuming it for color.
- Remove the `WC_DEBUG_REF_SWATCHES` reference-swatch scaffolding from
  `crates/waveconductor/src/main.rs`. Keep `WC_DEBUG_TONEMAP` (still useful for
  auditioning operators against the per-sketch setting).

## Defaults summary

| Knob | Default | Category | Scope |
|---|---|---|---|
| tonemapping | `ReinhardLuminance` | Dev | per sketch |
| home/base tonemapping | `None` (SDR) | fixed | camera spawn |
| bloom_intensity | 0.35 | Dev | per sketch |
| bloom_threshold | 0.7 | Dev | per sketch |
| master_brightness | 1.0 | User | per sketch |
| gamma | 1.0 | User | per sketch |

## Testing

- Unit: `TonemapChoice::to_bevy` maps every variant; per-sketch settings defaults
  match their serde defaults (existing `default_values_match_serde_defaults`
  pattern); missing-field deserialize still falls back (existing pattern).
- Visual: `cargo xtask capture` per sketch + home before/after — confirm no
  screensaver pop, picker tiles render SDR-correct, glows bloom with retained
  chroma.
- Gates: fmt, clippy `-D warnings`, nextest, `cargo test --doc`, `cargo doc`.

## Follow-ups (out of scope here)

- Per-sketch tonemapping picker is in; revisit whether Home should also be a
  configurable choice rather than fixed `None`.
- Evaluate removing Line/Dots `attract_brightness` once the Reinhard attract look
  is judged.
- Tuning each sketch's source emissive + bloom recipe against Reinhard.
