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

37. **Visual verification of horizontal-line spawn is deferred.** Implementer confirmed binary boots and lifecycle test passes but couldn't click into `AppState::Line` from the harness. Madison or a manual session needs to verify five horizontal strands at mid-Y are visible and respond to click-drag before the v5-line-sim tag.

38. **`mid_y = 0.0_f32` in `spawn.rs:57` could become a setting** if Plan 11+ moves the Line camera off-center. Note for that point.

## From Plan 7 Phase E review (2026-05-25)

39. **Duplicated `arm_idle_timeline` pattern** between `crates/wc-sketches/tests/line_lifecycle.rs:183-193,324-334` and `crates/wc-core/tests/lifecycle_idle_veto.rs:44-60`. Hoist a shared helper into `tests/common/` once Plan 7.5 lands the test harness.

40. **`idle_veto_keeps_line_active_during_attractor_decay` is not adjacent to** `update_sim_params_does_not_run_when_idle` in `line_lifecycle.rs`. Group the two veto-aware tests together for readability.

41. **`use wc_core::lifecycle::RegisterIdleVetoExt;` is buried inside `LinePlugin::build`** (`crates/wc-sketches/src/line/mod.rs:42`). Hoist to the file's top `use` block for consistency with other crate-level imports.

42. **`MouseAttractorState.power` field doc** (`crates/wc-sketches/src/line/systems/mouse.rs:20`) doesn't note that `line_idle_veto` consumes it. A one-line cross-reference would shorten future rename-discovery.

43. **Test prerequisite comment ordering** in `update_sim_params_does_not_run_when_idle` (`line_lifecycle.rs:202-206`): the comment sits between the prereq assert and the `dt_before` capture; reads ambiguously. Move comment immediately before `let dt_before`.

44. **`line_idle_veto` is private** (`crates/wc-sketches/src/line/mod.rs:135`). If a future unit test wants to assert against the function directly, it'll need `pub(crate)`. Flag for if and when that arises.

## From Plan 7.5 Phase A review (2026-05-25)

45. **Test-fidelity gap: `move_pointer` → `PointerState.primary` edge is bypassed via `seed_pointer`.** `crates/wc-core/src/input/pointer.rs::pointer_merge_system` reads `window.cursor_position()`, which is written by winit (not by any system that consumes `CursorMoved` messages). So synthesized `CursorMoved` events do not flow to `PointerState`. Two follow-up options: (a) refactor `pointer_merge_system` so a mouse-only branch consumes `CursorMoved` directly (production change), or (b) extend `move_pointer` to additionally call `window.set_physical_cursor_position(...)` and register `pointer_merge_system` (or a mouse-only variant) in `sketches_test_app`. Either fix lets `line_input.rs` drop the `seed_pointer` shortcut. Tag as **test-fidelity**, not blocker. Land alongside Plan 8 if convenient.

46. **`move_pointer` rustdoc claims `PointerState` consumes via the merge system** — true in production, currently false in tests. Adjust the doc to note "consuming code must either register `pointer_merge_system` and update the Window, or seed PointerState directly (see `seed_pointer` in `line_input.rs`)." Resolves once #45 lands.

47. **Hoist `seed_pointer` to `tests/common/`** if/when a second sketch needs it. Currently lives in `line_input.rs` only.

48. **`#[path]` fragility** for the wc-sketches → wc-core `tests/common/input.rs` import is acknowledged in the module doc. No action; reminder for any future file move.

49. **`enter_line()` runs 4 `app.update()` calls after `tap_key`** — 3 would likely suffice (1 fold + 1 leafwing tick + 1 nav handler + 1 OnEnter). Tune if test-time perf ever matters.
