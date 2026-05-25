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
