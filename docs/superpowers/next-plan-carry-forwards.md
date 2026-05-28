# Next-plan carry-forwards

A running list of small, well-scoped items that surfaced after Plan 6 landed and should fold into Plan 7's Phase 0 housekeeping pass. Items are added as they're discovered (in code review, manual testing, or production use) and removed when the next plan absorbs them.

## From Plan 6 final code review (2026-05-24)

1. **Save-on-exit flush.** `autosave::tick` only fires saves when its 500 ms timer elapses. If the user adjusts a slider and closes the window in <500 ms, the edit is lost. Bevy's `AppExit` reader can drain `AutosaveState.pending` and call every queued `save_fn` before shutdown. ~15 LOC + 1 test.

2. **Reflection panel type coverage.** `render_number` currently dispatches on `u32`, `f32`, `f64`. Missing: `i32`, `i64`, `Vec2`, `Vec3`, `bevy::color::Color`. Next sketch with one of these will silently render `(unsupported number type for X)` instead of a widget. Extend `render_number` and add a `render_color`-style branch for `bevy::color::Color`. ~30 LOC.

3. **Auto-reenter on `requires_restart`.** `restart_on_settings_change` currently punts to `AppState::Home` and forces the user to re-click the sketch tile. Replace with a same-frame `OnExit → OnEnter` cycle (e.g., via a deferred `NextState` set, or a one-frame `Local` counter). Visual continuity matters for the kiosk install.

4. **Render-graph node `tracing::trace!` on early returns.** `LineComputeNode::run` silently returns `Ok(())` when the bind group is missing or the pipeline is still compiling. A `trace!` on each branch makes "why aren't particles dispatching?" easier to diagnose. One line per branch.

5. **`min_binding_size: NonZeroU64`** on the compute bind group entries. Catches struct-size drift earlier (pipeline validation time) than runtime binding mismatch.

6. **Drop `test_settings.rs` from the production binary.** The module is currently `pub mod test_settings;` (not `#[cfg(test)]`-gated) so it ships in the release binary even though production no longer registers it. Either gate behind `#[cfg(test)]` (and update the integration tests that import it directly) or move under a test-only `tests/common/` module.

7. **`Single<&Window>` in `update_sim_params`.** Currently uses `Query<&Window>::iter().next()`. Multi-window isn't a goal, so `Single<&Window>` is both more idiomatic and would fail loudly if that assumption ever broke.

## From manual testing on 2026-05-24

8. **Asset path config for release bundles.** `main.rs` currently sets `AssetPlugin.file_path = "../../assets"` so `cargo run -p waveconductor` finds the workspace-root `assets/` tree. macOS DMG / Windows portable exe / AppImage all bundle `assets/` next to the binary, so the release build needs the default `"assets"`. Use `cfg(debug_assertions)` (or a more sophisticated env-based switch) to pick the right path. Don't ship the dev-time relative path in a notarized release.

9. **Gravity formula tuning + remove 1 Hz diagnostic log.** `simulate.wgsl` currently uses inverse-linear gravity (`G·radius/dist`) so particles are visible at default settings. This isn't tuned to v4 perceptual parity — the trail character, momentum, and equilibrium speed need a side-by-side review. Once the formula feels right, remove the 1 Hz `tracing::info!` in `update_sim_params::diag_timer`. PARITY.md verdict re-checked at that point.

## From Plan 7 Phase 0 review (2026-05-25)

10. **`LineRestartPending` cleanup is unsolved.** The trampoline marker can linger if a non-trampoline state change races the two-frame `Line→Home→Line` cycle (e.g. Escape pressed between trampoline phases). The naive cleanup spot — `OnExit(AppState::Line)` — breaks the trampoline itself because that exit *is* what the trampoline drives. Options: timestamp the marker and reap on `Last` after N frames, replace the global resource with a `Local` on the handler system so it auto-clears, or convert to a one-shot message. Land in Plan 8 alongside the renderer touch-ups; the current leak window is narrow and harmless (a stale resource that `set(Line)` no-ops against).

11. **`NonZeroU64::new(...).expect(...)` in `compute.rs` should be a `const`.** Replacing with `const SIM_PARAMS_SIZE: NonZeroU64 = match NonZeroU64::new(...) { Some(n) => n, None => panic!("...") };` pushes the assertion to compile time and removes the runtime `#[allow(clippy::expect_used)]`. Pure improvement.

12. **`extern crate self as wc_core`** in `crates/wc-core/src/lib.rs` is now justified only by future macro consumers. Either drop it now and reinstate when Plan 8's in-crate sketch lands, or tighten the `reason` to name a concrete blocker.

13. **Restart-cycle `info!` logs in `line/mod.rs` should drop to `debug!`** once the trampoline is proven stable. They fire on every settings restart and are noise in release.

14. **`LineComputeNode` trace messages** could become structured tags (`tracing::trace!(node = "LineComputeNode", "no pipeline yet")`) for cleaner log queries. Style-only.

15. **Verify `groupedUpness` spelling in `PARITY.md`.** Currently used as a domain term; confirm it's not a typo for "groupedness" before Plan 9 picks it up as a Rust identifier.

## From Plan 7 Phase A review (2026-05-25)

16. **`InteractionTimer::clone()` in `advance_activity` deserves an inline comment.** `crates/wc-core/src/lifecycle/idle.rs:144` — the clone is required to release the immutable resource borrow before `any_veto_active` reads other world state, but the source reads as gratuitous without a one-line explanation.

17. **Drop the unused `expect_used` allow** at the top of `crates/wc-core/tests/lifecycle_idle_veto.rs` (currently no `.expect()` in the file body). Re-add narrowly when actually needed.

18. **`advance_activity` early-return on `Home`** runs the timer/veto compute before checking whether `SketchActivity` exists. Negligible cost, but inverting the check would skip the `InteractionTimer.clone()` in `Home` state.

19. **`test_app()` / `build_app()` duplication** between `crates/wc-core/tests/lifecycle.rs` and `crates/wc-core/tests/lifecycle_idle_veto.rs`. When the third test file lands, hoist a shared `common::lifecycle_app()` helper into `tests/common/mod.rs`.

20. **Two stray "vetos" spellings in doc-comment prose** in `crates/wc-core/src/lifecycle/idle.rs` (lines 58 and 80). Field is now `vetoes`; prose should match for consistency.

## From Plan 7 Phase B review (2026-05-25) — rolling into Phase C

21. **Hoist v4 drag constants to named consts.** `crates/wc-sketches/src/line/systems.rs:238,244` embed `0.93075095702_f32` / `0.53913643334_f32` as inline literals. Replace with `const V4_PULLING_DRAG_CONSTANT: f32 = 0.93075095702;` / `const V4_INERTIAL_DRAG_CONSTANT: f32 = 0.53913643334;` / `const V4_FIXED_DT: f32 = 0.032;` at module scope. Eliminates duplicate `#[allow(...)]` blocks and surfaces v4-parity constants alongside `MOUSE_POWER_DECAY/FLOOR/PRESS`. Tests can assert by name.

22. **Press-while-held re-asserts power every frame, masking decay.** `crates/wc-sketches/src/line/systems.rs:173-177` re-asserts `power = MOUSE_POWER_PRESS` every frame the button is held. v4 only re-asserts on mousemove events, so a stationary held mouse decays freely (asymptotes to floor=2). This port holds power near 9.2 while held. **Fix:** drop the held-branch re-assertion (`just_pressed` only); let decay alone govern held behavior. Update the comment to honestly describe the chosen semantics ("matches v4: only just-pressed sets power=10; held with stationary mouse decays to floor"). Reconcile the parity claim with the code.

23. **`MouseAttractorState::Default` is hand-rolled.** `systems.rs:52-59` writes `power: 0.0, position: [0.0, 0.0]` manually. `#[derive(Default)]` would produce identical output and matches `Attractor`'s derive style. Field-doc comment for power can move from the Default impl to the struct field.

24. **`1e-2` epsilon in `decay_mouse_attractor` is magic.** `systems.rs:192` uses `MOUSE_POWER_FLOOR + 1e-2` as the zero cutoff. Promote to `const MOUSE_POWER_DECAY_EPSILON: f32 = 1e-2;` or add an inline comment explaining the tolerance.

25. **`Touches::iter().next().is_some()` needs a 1-line comment.** `systems.rs:162` — non-consuming iteration is correct but easy to second-guess. Add `// Any active touch counts as "held"; iter() is non-consuming.`

26. **`update_sim_params` lacks unit tests for new fields.** A targeted test inserting `MouseAttractorState { power: 10.0, position: [5, 5] }` then asserting `sim.params.attractor_count == 1` and `sim.params.attractors[0].power == 10.0 * gravity_constant` would catch unintended drift. Add in Phase C alongside the CPU mirror tests.

27. **Pre-split `systems.rs` into focused submodules** before Phase C extends it. Current file is 269 lines; Phase C adds Particle field initialization (~15 lines) and may add CPU mirror wiring. Recommended structure: `systems/spawn.rs`, `systems/mouse.rs`, `systems/sim_params.rs`, with `systems.rs` becoming a thin module root that re-exports.

28. **`MAX_ATTRACTORS` GPU cost note.** `particle.rs:42` — when `MAX_ATTRACTORS` grows past ~16 (Plan 11+ Leap hands), the uniform buffer will get large. Add a `// TODO(plan-11): consider dynamic-sized storage buffer if MAX_ATTRACTORS > ~16`.

29. **Update Plan 7 doc with the `_pad` arithmetic correction** for Tasks 15 and 16. The plan claims "eight scalars above total 36 bytes" but actual is 40, needing `[f32; 2]` pad (Rust) and `vec2<f32>` (WGSL), not the `[f32; 3]` / `vec3<f32>` shown. Implementer applied the correction; the plan doc should reflect reality so a future re-execution doesn't trip.

## From Plan 7 Phase C review (2026-05-25)

30. **`_held` is dead code** in `crates/wc-sketches/src/line/systems/mouse.rs:53-54`. Computed every frame, immediately discarded. Either delete the computation and replace with a comment ("touches.iter() and mouse_buttons.pressed() are intentionally not read"), or keep with `#[allow(unused_variables, reason = "Plan 11 hand-tracking will read this")]`.

31. **Weak directional assertion in `one_attractor_pulls_particle`** (`crates/wc-sketches/src/line/sim_cpu.rs:127-145`). Only checks `velocity[0] > 0.0`. A numeric assertion comparing to `power * size_scale * dt` (for an x-axis-aligned attractor) would catch a force-formula regression directly.

32. **Brittleness of `update_sim_params_writes_mouse_attractor_with_gravity_scaling`** (`crates/wc-sketches/tests/line_lifecycle.rs:230-256`). Hard-codes the post-decay power value (9.2) without naming it. Promote to `const EXPECTED_POST_DECAY_POWER: f32 = MOUSE_POWER_FLOOR + (MOUSE_POWER_PRESS - MOUSE_POWER_FLOOR) * MOUSE_POWER_DECAY;` so the dependency on system order is explicit.

33. **`step_one` rustdoc** should note its hot-path role (`crates/wc-sketches/src/line/sim_cpu.rs:39`): "Pure function, allocation-free; called once per particle per frame from `step_cpu_mirror`."

## From Plan 7 Phase D review (2026-05-25)

34. **`LineSettings` module doc should mention serde forward-compat.** `crates/wc-sketches/src/line/settings.rs:1-14` explains *why* `drag`/`attractor_radius` were dropped but doesn't tell a maintainer that existing user TOML with those keys still deserializes cleanly. A future engineer might add `#[serde(deny_unknown_fields)]` and break upgrades from v5-line.

35. **`line_settings_resource_inserted` test assertion is weaker than before.** `crates/wc-sketches/tests/line_lifecycle.rs:90` checks `particle_density > 0.0`. Tighten to `>= 0.1` (the documented min) so a future typo dropping the default below the floor is caught.

36. **Commit message `ba515e8` has a stale "drag moves to Dev" claim.** Plan doc Task 26 Step 2 said that, but the implementation removed `drag` entirely. The in-tree settings doc is correct; only the commit message lies. Patch the plan doc for any future re-execution (commits are immutable).

38. **`mid_y = 0.0_f32` in `spawn.rs:57` could become a setting** if Plan 11+ moves the Line camera off-center. Note for that point.

## From Plan 7 Phase E review (2026-05-25)

39. **Duplicated `arm_idle_timeline` pattern** between `crates/wc-sketches/tests/line_lifecycle.rs:183-193,324-334` and `crates/wc-core/tests/lifecycle_idle_veto.rs:44-60`. Hoist a shared helper into `tests/common/` once Plan 7.5 lands the test harness.

40. **`idle_veto_keeps_line_active_during_attractor_decay` is not adjacent to** `update_sim_params_does_not_run_when_idle` in `line_lifecycle.rs`. Group the two veto-aware tests together for readability.

41. **`use wc_core::lifecycle::RegisterIdleVetoExt;` is buried inside `LinePlugin::build`** (`crates/wc-sketches/src/line/mod.rs:42`). Hoist to the file's top `use` block for consistency with other crate-level imports.

42. **`MouseAttractorState.power` field doc** (`crates/wc-sketches/src/line/systems/mouse.rs:20`) doesn't note that `line_idle_veto` consumes it. A one-line cross-reference would shorten future rename-discovery.

43. **Test prerequisite comment ordering** in `update_sim_params_does_not_run_when_idle` (`line_lifecycle.rs:202-206`): the comment sits between the prereq assert and the `dt_before` capture; reads ambiguously. Move comment immediately before `let dt_before`.

44. **`line_idle_veto` is private** (`crates/wc-sketches/src/line/mod.rs:135`). If a future unit test wants to assert against the function directly, it'll need `pub(crate)`. Flag for if and when that arises.

## From Plan 7.5 Phase A review (2026-05-25)

45. RESOLVED 2026-05-25 (Plan 11 Phase B audit): Plan 8 Phase 0 already wired
    `pointer_merge_system` into `sketches_test_app`. `seed_pointer` is gone;
    synthetic CursorMoved events flow end-to-end.

46. **`move_pointer` rustdoc claims `PointerState` consumes via the merge system** — true in production, currently false in tests. Adjust the doc to note "consuming code must either register `pointer_merge_system` and update the Window, or seed PointerState directly (see `seed_pointer` in `line_input.rs`)." Resolves once #45 lands.

47. **Hoist `seed_pointer` to `tests/common/`** if/when a second sketch needs it. Currently lives in `line_input.rs` only.

48. **`#[path]` fragility** for the wc-sketches → wc-core `tests/common/input.rs` import is acknowledged in the module doc. No action; reminder for any future file move.

49. **`enter_line()` runs 4 `app.update()` calls after `tap_key`** — 3 would likely suffice (1 fold + 1 leafwing tick + 1 nav handler + 1 OnEnter). Tune if test-time perf ever matters.

## From Plan 8 Phase 0 review (2026-05-25)

50. **`EXPECTED_POST_DECAY_POWER` uses local `SEEDED_MOUSE_POWER`** instead of production `MOUSE_POWER_PRESS`. Both happen to be `10.0` today; if tuning changes one, the test won't track. Replace with the production const for full coupling.

51. **`SIM_PARAMS_SIZE`'s `as u64` cast** is an unavoidable const-context wart (`u64::try_from(usize)` isn't const-stable). Add `#[allow(clippy::cast_possible_truncation, reason = "size_of fits in u64 on all supported targets")]` or a one-line comment explaining the constraint.

52. **`cursor_moved_reader.read().last()` discards intermediate positions silently.** Intentional (we want "newest wins" for pointer position, not motion path) but the comment in `pointer_merge_system` could explicitly note this design choice.

## From Plan 8 Phase C (2026-05-25)

53. **Post-process runs in every state, not just `AppState::Line`.** Plan 9's audio-driven `g_constant` modulation will produce a degenerate result when not in Line (no particles, no rings → smear of background), but a tighter gate would be either: (a) `add_render_graph_edges` conditional on `AppState`, or (b) zero `g_constant` outside Line. Option (b) is one-liner in `update_sim_params`. Land in Plan 9 or Plan 10.

54. **Per-frame uniform-buffer allocation in `LinePostProcessNode::run`.** Currently `create_buffer_with_data` allocates 32 bytes each frame. Compute pipeline solved this with a persistent buffer + `queue.write_buffer`; mirror that pattern. Out of hot-path principle per AGENTS.md. Plan 9 follow-up.

55. **Visual verification of gravity-smear pending.** Implementer confirmed boot-without-panic and clippy/test green, but couldn't click into Line and drag from inside the agent session. Madison should manually verify chromatic-smear is visible on press+drag.

## From Plan 10 manual testing (2026-05-25)

56. **Attractor rings use perfectly-circular `Annulus`, so per-frame rotation is invisible.** `(10 - idx) / 20 * power` rotation is applied to the entity Transform every frame, but `Annulus` is rotationally symmetric and the visual is identical at any angle. v4's rings appear visibly spinning. Likely fix: switch to a low-segment-count polygon (Bevy `RegularPolygon` with 6–8 sides, or a custom mesh) so corners make rotation legible. Alternative: replace meshes with stroked SVG-style paths if v4's actually uses Three.js `RingGeometry(inner, outer, thetaSegments=6)`. Source-of-truth check needed against v4's `src/sketches/line/index.ts` attractor visual code.

57. **LineSettings TOML migration: missing-field warnings on legacy persisted state.** When `gamma` (added Plan 8) is absent in a previously-saved `wc-settings.toml`, the whole `[line]` section fails to deserialize and falls back to defaults — silently discarding `particle_density` and `gravity_constant` too. Add `#[serde(default)]` per field so partial deserialization preserves what's there. Repeat for any future field additions.

58. RESOLVED 2026-05-25 (Plan 11 Phase 0): Bevy 0.18 shutdown noise; upstream issue; not actionable. Re-evaluate at Bevy 0.19+ point bump.

59. RESOLVED 2026-05-25 (Plan 11 Phase 0): bevy_pbr Metal info warning (bevy issue #18149); not actionable from our code.

60. **Touch & hand-tracking can move the pointer but can't activate the Line attractor.** `update_mouse_attractor` in `crates/wc-sketches/src/line/systems/mouse.rs` reads `Res<ButtonInput<MouseButton>>::just_pressed(Left)` exclusively. The pointer-merge layer (`crates/wc-core/src/input/pointer.rs`) routes touch and hand positions into `PointerState` correctly, but only the mouse can trigger attractor press/release. v4 used `pointerdown`/`pointerup` which fire for both mouse and touch. Fix: read `Res<Touches>` for `TouchPhase::Started`/`Ended` events alongside mouse, and decide on a hand-tracking gesture (pinch? fist?) for synthetic press. Gallery target is a touchscreen kiosk, so this is important before public install. Plan 11+.

61. **No hand-tracking provider implementation.** `HandTrackingState` resource exists and `pointer_merge_system` reads from it correctly, but nothing currently writes to it — Plan 3 stubbed the shape; the actual Leap Motion / Mediapipe provider that fills it in is Plan 11+ work. Until then, hand source priority in `pointer_merge_system` is theoretical.

62. **`spawn_template` heatmap-image setting needs a file picker.** Currently rendered as a free-text input (`SettingCategory::Text` → egui `text_edit_singleline`). Adding a "Browse…" button next to the field that opens an `rfd::FileDialog` (cross-platform native picker — macOS NSOpenPanel, Windows IFileDialog, GTK on Linux) is ~30 LOC: new `SettingCategory::FilePath { extensions: &[&str] }` variant, renderer adds the browse button, attribute on the field becomes `#[setting(category = file_path, extensions = ["png", "jpg"])]`. Dependency add: `rfd = "0.15"`. Important because typing an absolute path manually is a poor kiosk UX and Madison wants to iterate on heatmap source images visually.

63. **Heatmap-image spawn untested end-to-end.** Plan 10 Phase A ports the v4 inverse-CDF sampler with unit tests for the math, but no agent driver has set `spawn_template` to a real PNG and observed the resulting particle distribution. Madison should verify with one of: `assets/sketches/line/star.png` (trivial — should cluster near the center where the sprite is bright), a black-with-white-circle test image, or her actual chosen photograph. Confirm the fallback path (typo'd / missing path → horizontal-line layout, no panic) by entering an obviously-wrong path. Fold into the eventual Plan 11 file-picker work or independently.

## From Plan 11.5 manual verification (2026-05-27)

64. **Fullscreen toggle overlay button never shipped.** The original Plan 11.5 spec called for sketch-picker / fullscreen-toggle / settings-open / info-about as the four nav buttons; only Home, Settings cog, and Volume landed. `WaveConductorAction::ToggleFullscreen` already exists in `crates/wc-core/src/lifecycle/actions.rs:106` (keybinding wired); a small `draw_fullscreen_button` in `crates/wc-core/src/ui/buttons.rs` that pushes a `WindowMode::Fullscreen ↔ Windowed` transition and matches the existing button chrome would close this gap. ~40 LOC including the Phosphor icon constant lookup (likely `CORNERS_OUT` or `ARROWS_OUT`).

65. **Info/About overlay button never shipped.** Also in the original 11.5 spec. v4 doesn't actually have one either — this was scoped optimistically. If the credits cell on the home page is the "info/about" surface, this item can be marked done. Madison decides: ship a separate cog-adjacent ⓘ button that opens an "About" panel with the credits content, or accept the home-page credits tile as sufficient and drop this item.

66. **Dev panel doesn't section-group its fields.** The user panel renders fields under `## Particles`, `## Visual`, `## Audio`, `## Spawn` headers via the `#[setting(section = "...")]` metadata. The dev panel (`panel_dev.rs`) still uses `bevy_inspector_egui::ui_for_world` which has its own flat-tree layout. To get section grouping for Dev-category fields, either (a) bypass `ui_for_world` and walk the settings registry the same way the user panel does, OR (b) keep `ui_for_world` for the world inspector and add a separate "Dev Settings" section above it that renders Dev-category fields via the same Grid-by-section pattern.

67. **`SketchManifest` has no `iter()` method.** The picker iterates `AppState::SKETCH_ORDER` and looks up each variant via `manifest.get(state)`. This works but inverts the natural "iterate registered sketches" flow. Add `pub fn iter(&self) -> impl Iterator<Item = &SketchManifestEntry>` for consumers that want to walk the manifest directly. Cosmetic; picker doesn't need it.

68. **`buttons.rs` and `blur/node.rs` exceed the ~300-line soft limit.** `buttons.rs` is ~580 lines (PointerCoarse + overlay_icon_button + Home/Settings/Volume + draw + sync + LastSettingsPanelRect). `blur/node.rs` is ~560 lines (BackdropBlurPipeline + BackdropBlurNode + scratch resource + run-count proxy + extract). Split candidates: `buttons/` submodule with `widget.rs` (overlay_icon_button + helpers), `draw.rs` (the three draw_* systems + resources), and `mod.rs` (plugin + PointerCoarse). For blur: split `pipeline.rs` (BackdropBlurPipeline + FromWorld) from `node.rs` (BackdropBlurNode + scratch + run logic).

69. **`text_color_dim` palette field is caller-only, not applied to global egui Style.** `OverlayStyle` exposes `text_color_dim` (≈ v4 `$gray3`) for explicit `RichText::new(...).color(style.text_color_dim)` calls. It is not wired into `Visuals::widgets.noninteractive.fg_stroke` or similar, so any egui-default text falls back to `text_color_bright`. Currently fine because all dim-coloured text is explicit; if a future widget needs dim text by default, wire the palette into Visuals.

70. RESOLVED 2026-05-27: Tracked on the roadmap's Pre-release tier as the "Licenses surface" port item — a real v4 feature port, not a carry-forward polish.

71. **`line_soak_with_overlay_ui` integration test is `#[ignore]`'d.** Required per AGENTS.md before any release tag. Run it once before Plan 11.7's final capture: `cargo test -p wc-sketches --test line_soak line_soak_with_overlay_ui -- --ignored --nocapture` (8 hours). Fold into the Plan 11.7 pre-tag checklist.

72. **`BackdropBlurNode::run` calls `ViewTarget::post_process_write()` and drops the returned token without writing back.** Documented inline; safe today because no downstream node in the graph also uses `post_process_write()` between this node and the surface composite. If a future node inserts itself in that range and also uses `post_process_write`, the ViewTarget's swap counter will be desynchronized. Worth a one-line CAUTION comment near the call site so future graph editors notice.

73. **`buttons.rs` documents `sync_volume_muted` as ordered after `pump_audio_messages` AND the optimistic-flip on button click can race with the sync system, causing a 1-2 frame icon flicker.** Documented during Task 15 review. Fix options: (a) gate `sync_volume_muted` writes on `VolumeMuted == AudioState.muted` mismatch only when no command is in flight (needs a pending-flag); (b) remove the optimistic flip and let the audio echo drive the icon. Acceptable as-is at 60 FPS (16ms artifact).
