# WaveConductor v5 — UI v4 Parity Sprint (Plan 11.5 + Backdrop Blur)

**Date:** 2026-05-26
**Workstream:** Line parity gate (Plan 11.5 in `docs/superpowers/roadmap.md`)
**Status:** Design — pending Madison review before plan-writing
**Scope window:** ~5–7 days (Plan 11.5's roadmap estimate of 3–5 days + the backdrop-blur render-graph node)

## Goal

Match v4's overlay UI style and behavior. The kiosk install presents the v4 visual language — translucent buttons, frosted-glass settings panels with real backdrop blur, sketch-picker grid, auto-fading chrome — built on top of v5's existing `bevy_egui` integration. The Plan 5 reflection-driven widget set stays unchanged; only the chrome and the surrounding overlay surface change.

The v4 reference lives at `.worktrees/v4/src/`. Authoritative style values are read from `.worktrees/v4/src/styles/overlayButton.scss`, `.worktrees/v4/src/styles/overlayPanel.scss`, and `.worktrees/v4/src/settings/DevSettingsPanel/advancedSettingsPanel.scss`.

## Scope

In scope:

- BackdropBlurPlugin: render-graph node + Kawase blur shader pair + paint callback + frame helper.
- OverlayStylePlugin: egui Style/Visuals tuned to v4, custom font loading (Inter + Fira Code).
- OverlayButtonsPlugin: Home, Settings cog, Volume — floating egui::Area buttons with v4 positioning.
- AutoFadePlugin: UiOpacity resource + 30s idle-fade + 0.6s ease driven from the existing InteractionTimer.
- SketchPickerPlugin: 3×2 tile grid rendered during AppState::Home, with sheen-on-hover.
- Restyle the existing `settings/panel_user.rs` (Settings cog-controlled) and `settings/panel_dev.rs` (Shift+D-controlled world inspector) to use the new `backdrop_blur_frame()` helper. Reflection-driven widgets stay unchanged; egui built-ins are restyled via Style/Visuals rather than rewritten as custom widgets.

Out of scope (deferred / documented):

- LeapStatusIndicator surface — owned by Plan 11.6.
- Responsive layout breakpoints — kiosk target is fixed 1920×1080.
- Light theme — kiosk is dark-only.
- Pixel-identical custom widgets (iOS toggle, color popover, image preview thumbnail) — egui's built-ins, restyled, are acceptable per Madison's call.
- Manual `PARITY.md` sign-off and `v5-line-parity` tag — owned by Plan 11.7 after this sprint and Plan 11.6 both land.

## Architecture

All UI chrome lives in a new `wc-core/src/ui/` module exposing `WaveConductorUiPlugin`. Settings panels stay in `wc-core/src/settings/` and consume primitives from `ui/`. No cycles.

```
crates/wc-core/src/ui/
├── mod.rs              # WaveConductorUiPlugin, wires sub-plugins
├── style.rs            # OverlayStyle resource + apply_overlay_style() at PostStartup
├── blur/
│   ├── mod.rs          # BackdropBlurPlugin, BackdropBlurTexture resource, run conditions
│   ├── node.rs         # BackdropBlurNode (Bevy render-graph node)
│   └── callback.rs     # BackdropBlurPaintCallback (egui paint callback)
├── buttons.rs          # HomeButton, SettingsButton, VolumeButton (egui::Area)
├── auto_fade.rs        # UiOpacity resource + drive_ui_opacity system
├── picker.rs           # Sketch picker page (AppState::Home rendering)
└── frame.rs            # backdrop_blur_frame() helper — wraps any panel

crates/wc-core/src/sketch/
├── mod.rs              # (existing) cross-sketch helpers
├── cleanup.rs          # (existing) despawn_with::<Root>
├── scheduling.rs       # (existing) sketch_active() run condition
└── manifest.rs         # NEW: SketchManifest registry — picker tile metadata

assets/shaders/backdrop_blur/
├── downsample.wgsl     # Kawase 5-tap downsample pass
└── upsample.wgsl       # Kawase 8-tap upsample + composite pass

assets/fonts/
├── Inter-Regular.ttf   # Chrome sans-serif
└── FiraCode-Regular.ttf # Numeric inputs
```

Plugin composition:

```rust
impl Plugin for WaveConductorUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            OverlayStylePlugin,
            BackdropBlurPlugin,
            AutoFadePlugin,
            OverlayButtonsPlugin,
            SketchPickerPlugin,
        ));
    }
}
```

## BackdropBlurPlugin

**Resources:**

- `BackdropBlurTexture { view: TextureView, sampler: Sampler, extent: UVec2 }` — half-res blurred output. Lives in `RenderApp`. Allocated lazily; resized on window-resize event.
- `BackdropBlurEnabled(bool)` — main-world toggle, default `true`. Extracted into `RenderApp` each frame to gate the node.
- `BackdropBlurPipeline { downsample: CachedRenderPipelineId, upsample: CachedRenderPipelineId, layout: BindGroupLayout }` — pipeline cache, created once at `RenderApp` startup.

**Render-graph node (`BackdropBlurNode`):**

Inserted between `Tonemapping` and `bevy_egui::EguiPass` in both `Core2d` and `Core3d` graphs. Edges:

```
Tonemapping → BackdropBlurNode → EguiPass
```

Algorithm — dual-Kawase blur (Bjørge, ARM 2015):

1. **Downsample pass 1:** sample `ViewTarget` post-tonemap LDR → half-res scratch texture A using `downsample.wgsl` (5-tap kernel: center×4 weight + 4 corners offset 1.0 texel).
2. **Downsample pass 2:** A → 1/4-res scratch B.
3. **Downsample pass 3:** B → 1/8-res scratch C.
4. **Upsample pass 1:** C → 1/4-res scratch B' using `upsample.wgsl` (8-tap: 4 diagonal + 4 cardinal offset 1.0 texel).
5. **Upsample pass 2:** B' → 1/2-res scratch A'.
6. **Final upsample:** A' → `BackdropBlurTexture` (still at half-res — the paint callback bilinear-samples on the way to display).

Total cost: 6 fragment-shader draws over small textures. ~0.2–0.4 ms on integrated GPUs at 1280×720, ~0.1 ms on the kiosk GPU at 1920×1080.

**Run conditions** (per AGENTS.md zero-systems-when-idle):

The node's `run()` returns `false` (skips all 6 passes) when **any** of:

- `BackdropBlurEnabled.0 == false`
- `UiOpacity.current < 0.01` (chrome fully faded — blur is invisible anyway)
- No camera with a render target exists

When skipped, the previous frame's blurred texture is left allocated but stale. The next paint callback after a skip samples a frame-old blur, which is invisible because the skip only happens when opacity is near zero.

**Paint callback (`BackdropBlurPaintCallback`):**

Constructed via `EguiBevyPaintCallback::new_paint_callback(rect, BackdropBlurPaintCallback { corner_radius })`. Implements `EguiBevyPaintCallbackImpl`:

- `update`: no-op (no per-callback state).
- `prepare_render`: no-op (texture is produced by the standalone node, not per-callback).
- `render`: binds a small fragment-shader pipeline (`backdrop_blur_composite.wgsl`) that samples `BackdropBlurTexture` with bilinear filtering, applies a corner-radius mask using SDF (`length(max(abs(p) - half_extent + r, 0)) - r`), and draws a textured quad covering `info.clip_rect`. The pipeline is cached in `BackdropBlurPipeline`; the bind group rebuilt per frame from the (possibly-resized) texture view.

**UV conversion:** `info.viewport_in_pixels()` gives the rect in physical pixels. UVs = `physical_xy / window_physical_size`. Since the blur texture covers the full viewport, the mapping is direct.

**Frame helper (`frame.rs`):**

```rust
pub fn backdrop_blur_frame(
    ui: &mut Ui,
    options: FrameOptions,   // corner_radius, padding, opacity_mul
    content: impl FnOnce(&mut Ui),
) -> Response;
```

Internal flow:

1. Allocate rect for content using `ui.allocate_rect`.
2. Push `Shape::Callback(BackdropBlurPaintCallback { rect, corner_radius })` to the painter.
3. Push `Shape::Rect` with `OverlayStyle::panel_fill` (alpha-multiplied by `opacity_mul`), `panel_stroke`, and the same corner radius.
4. Build a child `Ui` clipped to the padded inner rect, run the caller's closure.
5. Return the response for click-outside detection and hover handling.

Compositing order — blur → translucent tint → content — mirrors CSS `backdrop-filter: blur()`.

**Fallback when `BackdropBlurTexture` is unallocated** (first frame, post-resize): the helper detects an `extent == UVec2::ZERO` texture and skips the callback push, falling through to just the tint + content. Logs `tracing::debug!` once. Acceptable single-frame transition during window resize.

## OverlayStylePlugin (style.rs)

Runs `apply_overlay_style` at `PostStartup`. Produces:

- `OverlayStyle` resource — the source of truth for color/radius/shadow constants. Other plugins read it; the egui `Style` derives from it.
- `egui::Style` applied via `ctx.set_style(...)`.

Constants — extracted from v4 SCSS, each with the source-line citation in a doc comment:

| Key | Value | v4 source |
|---|---|---|
| `panel_fill` | `Color32::from_black_alpha(204)` | `overlayPanel.scss:5` (`rgba(0,0,0,0.8)`) |
| `panel_stroke` | `Color32::from_white_alpha(20)` | `overlayPanel.scss:13` |
| `panel_corner_radius` | `CornerRadius::same(10)` | `overlayPanel.scss:7` |
| `panel_shadow` | `Shadow { offset: (0,8), blur: 32, spread: 0, color: from_black_alpha(102) }` | `overlayPanel.scss:14` |
| `button_fill_inactive` | `Color32::from_black_alpha(102)` | `overlayButton.scss:9` |
| `button_fill_hovered` | `Color32::from_black_alpha(153)` | `overlayButton.scss:18` |
| `button_stroke` | `Color32::from_white_alpha(38)` | `overlayButton.scss:10` |
| `button_corner_radius` | `CornerRadius::same(6)` | `overlayButton.scss:11` |
| `button_size_fine` | `Vec2::splat(32.0)` | `overlayButton.scss:5–6` |
| `button_size_coarse` | `Vec2::splat(44.0)` | `overlayButton.scss:23–24` |
| `text_color_dim` | `Color32::from_gray(140)` | `overlayPanel.scss` ($gray3/$gray4) |
| `text_color_bright` | `Color32::WHITE` | various |
| `text_color_label_hover` | `Color32::WHITE` | `advancedSettingsPanel.scss:13` |
| `fade_duration_seconds` | `0.6` | `overlayButton.scss:14` |

Font loading via `FontDefinitions`:

- `FontData::from_static(include_bytes!("../../../../assets/fonts/Inter-Regular.ttf"))` registered as the primary proportional font.
- `FontData::from_static(include_bytes!("../../../../assets/fonts/FiraCode-Regular.ttf"))` registered as the primary monospace font.
- Fonts are scrubbed of identifying metadata before commit per AGENTS.md.

## OverlayButtonsPlugin (buttons.rs)

Three floating buttons rendered each frame via `egui::Area::new(id).fixed_pos(pos).order(Order::Foreground)`. Drawn in `EguiPrimaryContextPass`.

| Button | Position | Action | Keyboard | Visibility |
|---|---|---|---|---|
| Home | `(12, 12)` | `NextState<AppState>::set(Home)` | Escape (existing) | Hidden when `AppState == Home` |
| Settings cog | `(window_width - 44, 12)` | Toggle `SettingsPanelVisible` | none (cog click is the only entry — v4 also exposes only its dev panel via shortcut, not a user-settings shortcut) | Always |
| Volume | `(window_width - 88, 12)` | `AudioCommand::SetMuted(!muted)` | `v` (existing `WaveConductorAction::ToggleVolume`, `lifecycle/actions.rs:104`) | Always |

Each button is a `overlay_icon_button(ui, icon, opacity_mul) -> Response` widget — ~25 lines using `ui.allocate_response(button_size, Sense::click())` and `ui.painter()` calls for rect + icon.

Hover transition: `ctx.animate_value_with_time(button_id, target_alpha, 0.2)` lerps between `button_fill_inactive` and `button_fill_hovered` alpha. Matches v4's `transition: background 0.2s`.

Icons via `egui_phosphor` (light variant) — house, gear, speaker glyphs. The crate is small (~50KB) and the icon set covers everything we need. Added as a workspace dep.

Touch-coarse detection (`PointerCoarse(bool)` resource): set `true` after any `TouchInput::Started` event; auto-revert to `false` after 1s of no touch events. Buttons read this resource to switch between `button_size_fine` and `button_size_coarse`. Matches v4's `@media (pointer: coarse)` rule.

Volume button reads `AudioMuted` resource (added by `AudioPlugin` Plan 4 — confirm at implementation time, add if missing). Renders disabled (low-alpha icon, click no-op) if the resource doesn't exist.

## AutoFadePlugin (auto_fade.rs)

State:

```rust
#[derive(Resource)]
pub struct UiOpacity {
    pub current: f32,   // 0.0..=1.0
    pub target: f32,
}

impl Default for UiOpacity {
    fn default() -> Self { Self { current: 1.0, target: 1.0 } }
}
```

Two `Update` systems:

1. `update_opacity_target`: reads `InteractionTimer` (Plan 2 resource). If `seconds_since_interaction > OverlayUiSettings::idle_fade_threshold_seconds` (default 30.0), `target = 0.0`; else `target = 1.0`.
2. `lerp_opacity`: exponential approach — `current += (target - current) * (1.0 - (-dt / TAU).exp())` where `TAU = OverlayUiSettings::idle_fade_duration_seconds / 4.6` (so ≈99% of the gap is closed in `idle_fade_duration_seconds`; the 4.6 is `ln(100)` for the 99% threshold).

Consumption: every overlay button and panel reads `UiOpacity.current` and multiplies its fill/stroke/text alpha by it. The `backdrop_blur_frame()` helper takes `opacity_mul` as an explicit parameter so it stays pure (no implicit `Res<UiOpacity>` read).

Interaction-blocking during fade: buttons consult `UiOpacity.current < 0.5` and bail out before consuming the click, matching v4's `pointer-events: none`.

Pointer-event re-entry: any mouse/touch/hand event resets `InteractionTimer` (already handled by Plan 2 + Plan 3 systems). Opacity lerps back up automatically — no new wiring needed.

New `OverlayUiSettings` resource (added under `wc-core/src/ui/`):

```rust
#[derive(Resource, Reflect, Serialize, Deserialize)]
pub struct OverlayUiSettings {
    /// Seconds of pointer inactivity before chrome fades out. v4 uses 30.
    pub idle_fade_threshold_seconds: f32,
    /// Time constant for the opacity ease. v4 uses 0.6 (CSS `transition: opacity 0.6s ease`).
    pub idle_fade_duration_seconds: f32,
    /// Master toggle for the backdrop-blur pass. Dev escape hatch.
    pub backdrop_blur_enabled: bool,
}
```

Defaults: 30.0, 0.6, true. All three fields registered via the `#[setting(...)]` derive macro as `category = Dev` so they show up in the dev panel for runtime tuning.

## Sketch manifest registry (`wc-core/src/sketch/manifest.rs`)

The picker needs per-sketch display metadata — name, screenshot, availability. Hardcoding the full table in `picker.rs` would couple the picker to every sketch in `AppState`, defeating the isolation the rest of the codebase preserves (each sketch's plugin currently has zero references to any other sketch). The settings system already solves the same problem with a registry — `register_sketch_settings::<LineSettings>()`. The picker uses the same pattern.

**Types:**

```rust
/// Picker tile metadata for one sketch. Registered by each sketch's plugin
/// at startup so the picker can render tiles without naming any specific
/// sketch.
#[derive(Clone)]
pub struct SketchManifestEntry {
    pub state: AppState,                  // which AppState the tile launches
    pub display_name: &'static str,       // e.g. "Line", "Cymatics"
    pub screenshot: Handle<Image>,        // tile background
}

#[derive(Resource, Default)]
pub struct SketchManifest {
    entries: Vec<SketchManifestEntry>,
}

impl SketchManifest {
    pub fn get(&self, state: AppState) -> Option<&SketchManifestEntry>;
}

/// Extension trait — sketches call `app.register_sketch_manifest(...)` at
/// startup. Mirrors `register_sketch_settings` in `wc-core/src/settings/`.
pub trait RegisterSketchManifestExt {
    fn register_sketch_manifest(&mut self, entry: SketchManifestEntry) -> &mut Self;
}

impl RegisterSketchManifestExt for App { /* push to SketchManifest */ }
```

**Per-sketch wiring** — LinePlugin gains one line in its `build()`:

```rust
let screenshot = asset_server.load("sketches/line/screenshot.png");
app.register_sketch_manifest(SketchManifestEntry {
    state: AppState::Line,
    display_name: "Line",
    screenshot,
});
```

That's the entire coupling between Line and the picker. Future sketches (Flame, Dots, Cymatics, Waves in Plan 12+) each add their own one-liner when their plugin lands. Until then, those `AppState` variants stay unregistered and the picker renders them as placeholders automatically.

**Screenshot asset:** Line's screenshot lives at `assets/sketches/line/screenshot.png` — porting `screenshot.png` from the repo root as part of this sprint (the repo-root file gets removed once the asset is in place).

## SketchPickerPlugin (picker.rs)

Draws during `EguiPrimaryContextPass` gated on `AppState::Home`. Renders a `egui::CentralPanel` with `Color32::from_rgb(16, 22, 26)` background (v4's `#10161A`) and a 3×2 `egui::Grid` of sketch tiles.

**Tile generation** — pure iteration over `AppState::SKETCH_ORDER`, looking up entries in `SketchManifest`:

```rust
for (cell_idx, &state) in AppState::SKETCH_ORDER.iter().enumerate() {
    match manifest.get(state) {
        Some(entry) => render_active_tile(ui, entry),       // screenshot + sheen + clickable
        None        => render_placeholder_tile(ui, state),  // "Coming soon"
    }
    if cell_idx % 3 == 2 { ui.end_row(); }
}
// 6th cell stays empty (SKETCH_ORDER has 5 entries; grid has 6 cells).
```

`render_placeholder_tile` uses the variant's `Debug` name (`format!("{state:?}")` → `"Flame"` etc.) as the displayed sketch name. No string table in the picker, no per-sketch knowledge.

**Adding a new sketch in Plan 12+ requires zero changes to `picker.rs`** — registering the manifest entry in the new sketch's plugin is sufficient.

Both `render_active_tile` and `render_placeholder_tile` share a common widget signature:

```rust
fn render_active_tile(ui: &mut Ui, entry: &SketchManifestEntry) -> Response;
fn render_placeholder_tile(ui: &mut Ui, state: AppState) -> Response;
```

Active tile behavior:

- Allocates a rect at the cell's grid size (`window_size / Vec2::new(3.0, 2.0)`).
- Paints `entry.screenshot` via `ui.painter().image(texture_id, rect, uv, Color32::WHITE)` (`Handle<Image>` → `EguiUserTextures` → `TextureId`).
- Paints `entry.display_name` in Orbitron at the bottom-left in `RichText::new(...).size(40.0)`, with v4's gradient-fade overlay: a `Mesh` of two vertex-colored triangles forming a vertical gradient from `Color32::from_black_alpha(165)` at the bottom to `Color32::TRANSPARENT` at the top, drawn over the bottom 30% of the tile.
- **Sheen-on-hover:** a diagonal gradient sweep from off-screen-left to off-screen-right, 0.5s duration. Driven by `let t = ctx.animate_bool_with_time(tile_id, hovered, 0.5)` returning a progress `t ∈ [0,1]`; the sheen's center X interpolates from `-tile_width` to `2 × tile_width`. Painted as a 4-vertex `Mesh` with gradient stops `rgba(255,255,255,0.13)` → `rgba(255,255,255,0.5)` → `rgba(255,255,255,0)` from `homePage.scss:155–164`.
- Allocates with `Sense::click()`; on click sets `NextState<AppState>::set(entry.state)`.

Placeholder tile behavior:

- Paints `Color32::from_rgb(20, 26, 32)` solid fill.
- Paints the sketch name in Orbitron (derived from `format!("{state:?}")`) and a smaller subtitle "Coming soon" in `OverlayStyle::text_color_dim`.
- No sheen, no `Sense::click()` — the tile is inert.

Responsive layout: kiosk fixed at 1920×1080 — single layout, no breakpoints. Documented as out of scope.

Orbitron font: shipped to `assets/fonts/Orbitron-Bold.ttf`, registered with egui's `FontDefinitions` as a named family `"orbitron"`, applied via `RichText::new(...).family(FontFamily::Name("orbitron".into()))`.

The Home page renders *under* the overlay buttons. The Home button itself is hidden when `AppState == Home`. Settings cog stays visible.

## Restyled `settings/panel_user.rs` and `panel_dev.rs`

`panel_user.rs`:

1. Add `SettingsPanelVisible(bool)` resource, default `false`. Toggled by the Settings cog click (no keyboard shortcut).
2. Replace `egui::Window::new("Settings").show(...)` with `egui::Area::new("settings-panel").fixed_pos([window_width - 16.0 - 300.0, 60.0])` matching v4's `top: 60px; right: 16px`.
3. Wrap content in `frame::backdrop_blur_frame(ui, FrameOptions { corner_radius: 10.0, padding: Vec2::new(20.0, 16.0), opacity_mul }, |ui| { ...existing reflection loop... })`.
4. Title row uses `RichText::new("SETTINGS").color(OverlayStyle::text_color_dim).size(13.0)` with `letter-spacing` via a `text_style` override — applied per-glyph since egui lacks a built-in letter-spacing knob; if exact letter-spacing fidelity is too costly, accept default spacing and document as approved deviation.
5. **Click-outside-to-close:** a new `handle_panel_click_outside` system reads `EguiContexts::ctx().input(|i| i.pointer.any_pressed())` and checks if the last pointer position is outside `LastSettingsPanelRect`. Stored as `LastSettingsPanelRect(Rect)` resource updated each draw frame. The check skips one frame after `SettingsPanelVisible` flipped to `true` to avoid the cog click immediately closing the panel.

`panel_dev.rs` (the world inspector):

1. Visibility stays on existing `DevPanelVisible` (Shift+D shortcut).
2. Replace `egui::Window::new("Dev Inspector").show(...)` with `egui::Area::new("dev-panel").default_pos([16.0, 60.0])` (top-left, beneath where the Home button sits).
3. Wrap in `frame::backdrop_blur_frame(...)` with the same chrome.
4. The world inspector content is much taller than the user-settings panel — wrap in `egui::ScrollArea::vertical().max_height(window_height - 100.0)` before calling `bevy_inspector::ui_for_world(world, ui)`.
5. No click-outside dismiss (developer tool — Shift+D toggle suffices).

The reflection-driven widget loop in `panel_user.rs` is **unchanged**. Only the framing and positioning change.

## Data flow

```
InteractionTimer (Plan 2)
    │
    ▼
[update_opacity_target] ──► UiOpacity { target }
                                  │
                                  ▼
                          [lerp_opacity (Update)]
                                  │
                                  ▼
                            UiOpacity { current }
                                  │
              ┌───────────────────┼────────────────────┐
              ▼                   ▼                    ▼
     OverlayButtons        backdrop_blur_frame    BackdropBlurNode run condition
     alpha multiply        opacity_mul param        (skip when <0.01)


Camera ViewTarget (post-Tonemap LDR)
    │
    ▼ (RenderApp, Core2d/Core3d graph)
BackdropBlurNode ──► BackdropBlurTexture (half-res, persistent)
    │
    ▼
EguiPass ── each frame ──► BackdropBlurPaintCallback.render()
                                  │ (samples BackdropBlurTexture)
                                  ▼
                          ViewTarget composite (final framebuffer)


AppState::Home ──► SketchPickerPlugin iterates AppState::SKETCH_ORDER
                       ──► Looks up each state in SketchManifest resource
                              ──► Some(entry) ──► render_active_tile (clickable)
                              ──► None        ──► render_placeholder_tile (inert)
                       ──► Click on active tile ──► NextState<AppState>::set(entry.state)

Each sketch plugin at startup:
    app.register_sketch_manifest(SketchManifestEntry { state, display_name, screenshot })
        ──► SketchManifest.entries (consumed by picker; no cross-sketch coupling)

Settings cog click ──► toggle SettingsPanelVisible
                            ──► panel_user draws frame next pass

Shift+D ──► toggle DevPanelVisible (existing)
                ──► panel_dev draws frame next pass
```

## Testing

**Unit (colocated `#[cfg(test)] mod tests`):**

- `OverlayStyle` constants match v4 SCSS values byte-for-byte.
- `update_opacity_target` at `InteractionTimer` ∈ {0s, 29s, 31s, 60s} produces `target` ∈ {1.0, 1.0, 0.0, 0.0}.
- `lerp_opacity` reaches ≥99% of target after `idle_fade_duration_seconds` of simulated `dt`.
- `SettingsButton` click flips `SettingsPanelVisible`.
- Click-outside detection: pointer-down outside `LastSettingsPanelRect` → `SettingsPanelVisible = false`; inside → unchanged.
- Sketch picker Line-tile click transitions `AppState::Home → AppState::Line`.
- `SketchManifest::get(state)` returns `Some(entry)` for a registered sketch and `None` for an unregistered one. `register_sketch_manifest` appends to the entry list.
- Picker iteration over `AppState::SKETCH_ORDER` with a manifest containing only `AppState::Line` produces 1 active tile + 4 placeholder tiles, in `SKETCH_ORDER` order.
- `PointerCoarse(true)` after a `TouchInput::Started` event; back to `false` after 1s with no touch events.

All use `MinimalPlugins` test apps, no egui context required.

**RenderApp tests (`wc-core/tests/ui_blur.rs`):**

- `BackdropBlurNode.run()` returns `false` when `UiOpacity.current < 0.01`, when `BackdropBlurEnabled.0 == false`, and when no camera exists.
- After one `app.update()` with the plugin enabled and a primary camera, `BackdropBlurTexture` exists in `RenderApp` with non-zero `extent`.
- A window-resize event triggers texture reallocation; `extent` field reflects the new dimensions on the next frame.

If the harness becomes brittle, mark these `#[ignore]` and rely on the soak harness for coverage.

**Soak (extends Plan 10 harness):**

New variant of the 8-hour soak test with `WaveConductorUiPlugin` enabled. Toggle `UiOpacity` between 0 and 1 every 60s (simulating idle/active cycles). Pass criteria: flat VRAM and main-heap allocation within Plan 10 tolerance, no panic, no `tracing::error!`. Required per AGENTS.md before any release tag — runs as part of Plan 11.7's pre-tag checklist.

**Manual visual-parity gate (Plan 11.7):**

Side-by-side capture against v4 at 1280×720. Required captures: idle state, settings panel open, dev panel open, mid-fade (~15s into the 30s window), sketch picker page, sheen-on-hover mid-animation. Each frame visually compared; discrepancies fixed or recorded as approved deviations in `PARITY.md`. Likely approved deviations:

- Sheen-on-hover exact timing (CSS easing vs egui `animate_bool_with_time`).
- Backdrop-blur radius precise match (Kawase 6-pass approximation vs Safari's gaussian).
- Font rendering subpixel character (egui atlas vs CoreText hinting).
- Letter-spacing on uppercase title rows (if exact match is cost-prohibitive).

## Error handling matrix

| Failure mode | Behavior |
|---|---|
| `BackdropBlurTexture` unallocated (first frame, post-resize) | `backdrop_blur_frame()` skips callback, renders solid translucent fill. `tracing::debug!` once. |
| Egui context missing (`MinimalPlugins` tests) | All draw systems short-circuit (existing pattern: `panel_dev.rs:57`). |
| Inter / Fira Code / Orbitron font file missing | `include_bytes!` fails at compile time — caught by CI. |
| Tonemapping node absent on a camera | `BackdropBlurNode.run()` returns `false`; chrome falls back to opaque fill. |
| Multiple cameras with conflicting render targets | Node attaches to primary camera only; secondary cameras get unblurred overlay. Documented in plugin docstring. |
| `SettingsRegistry` empty | Existing `panel_user.rs` pattern: returns without drawing. |
| `SettingsPanelVisible == true` but no settings registered | Empty translucent frame with title bar. Acceptable degenerate case. |
| `AudioMuted` resource missing | `VolumeButton` renders disabled (low-alpha icon, click no-op). |
| `BackdropBlurEnabled == false` (user disabled in dev settings) | Node skipped; `backdrop_blur_frame()` renders solid translucent fill. Performance escape hatch. |
| Window minimized (zero-size viewport) | Node skipped (extent guard); resumes when window restores. |

## Open questions for plan-writing

- Exact font choice for Inter / Orbitron — confirmed open-license fonts; SIL-OFL fonts are the safe path. Plan-writing step verifies licensing and adds files to `assets/fonts/`.
- Whether `egui_phosphor` is the right icon crate vs. embedding a minimal SVG icon set. Default to `egui_phosphor` unless plan-writing finds a blocker.
- Whether `wc-core/src/audio/` exposes a `Res<AudioMuted>` or whether the volume button reads mute state through another resource. Plan-writing reviews `wc-core/src/audio/` to confirm.

## Acceptance

This sprint is done when:

1. All unit tests pass (`cargo test --workspace`).
2. RenderApp tests pass (or are documented as `#[ignore]` with soak coverage).
3. The 8-hour soak variant passes per the harness in Plan 10, with the new UI plugin enabled.
4. `cargo run -p waveconductor` shows: translucent overlay buttons in top-left/right with hover transitions; settings panel toggled by cog with backdrop blur; dev panel toggled by Shift+D with backdrop blur; sketch picker grid on Home with sheen-on-hover; chrome fades after 30s of inactivity, returns on pointer movement.
5. Side-by-side capture against v4 at 1280×720 passes Plan 11.7's perceptual-parity bar (or discrepancies recorded as approved deviations).

The `v5-line-parity` tag remains held until Plan 11.6 (Leap) and Plan 11.7 (capture) both land — this sprint produces no tag of its own.
