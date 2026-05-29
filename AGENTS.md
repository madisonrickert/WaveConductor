# Agent Instructions

These coding standards apply to all source contributions to WaveConductor v5. CI enforces them where it can; human and AI reviewers enforce the rest.

## In-code documentation

- `///` rustdoc on every public item (struct, enum, trait, fn, module).
- Module-level `//!` on every `mod.rs` or module root describing role and data flow.
- Document signal and data flow at plugin entry points (the `build()` method of each `Plugin`), not at every system call site.
- Inline `//` for math, DSP, and shader uniform contracts. Explain what each term in a formula represents.
- Never strip comments during refactors. Update stale comments rather than removing them.

## Code readability

- One concept per file. Files over ~300 lines or carrying two unrelated responsibilities are split.
- Public API at the top, private helpers at the bottom, tests in a `#[cfg(test)] mod tests` block at the file footer.
- Prefer named structs over tuple structs once a type has more than one semantically meaningful field.
- No `unwrap()` or `expect()` in non-test code unless the panic is documented as an invariant violation.
- No `as` casts on numeric types where `From` / `TryFrom` / `u32::try_from` would work.
- Function bodies fit on one screen; if not, extract.

## File organization

- One sketch per directory; entry is `mod.rs`, never an inline single file.
- Shaders live in `assets/shaders/<sketch>/<name>.wgsl`. Never inline WGSL strings in Rust.
- Platform-specific code lives in `platform/native.rs` and `platform/web.rs`; portable modules do not contain `cfg` blocks.
- Test files colocated with source as `#[cfg(test)] mod tests`.
- No `src/utils/` or `src/helpers/` dumping grounds. Helpers live with the module that uses them; truly shared helpers go in a named module under `wc-core/`.

## Application performance

- Default target is multi-hour unattended thermal stability, not peak FPS.
- Sketches must run zero systems when in `SketchActivity::Idle`. Verified by inspecting the schedule with `bevy_mod_debugdump`.
- No allocations in hot paths (per-frame systems, audio callbacks). Pre-allocate buffers, reuse `Vec`s, use `bevy::ecs::system::Local` for scratch state.
- Audio thread is real-time-friendly: lock-free ring buffers only, no `Mutex`, no allocations after init.
- GPU resources: every per-sketch resource is owned by an entity tagged with the sketch's marker component, despawned on `OnExit` to release VRAM.
- Compute shader dispatch sizes scale with settings; do not dispatch unused workgroups.
- An 8-hour soak test is required before any release tag.

## Visual testing

- Rendered sketches have a deterministic capture + regression harness: `cargo xtask capture <scenario>`. Run `cargo xtask manifest` to see all xtask subcommands, `cargo xtask capture --list` for scenarios, and read `tests/visual/CLAUDE.md` for the full surface (flags, `--json` shape, the `WC_DEBUG_*` render-stage isolation toggles, and how to add a scenario or update baselines).
- Prefer it over ad-hoc screenshot scripts when diagnosing or regression-testing a sketch's rendered output. It pins a fixed sim timestep (so captures are reproducible) and writes a self-describing `run.json`; the operating agent reviews the captured PNGs itself (no LLM API spend).
- The capture system and `WC_DEBUG_*` toggles are `#[cfg(debug_assertions)]`-gated and absent from release builds; never enable `debug-assertions` on a release/soak profile (see the guard on `[profile.release]` in `Cargo.toml`).

## Security and privacy

- No private personal information in the repo. No real email addresses (use `noreply.github.com` or placeholder), no phone numbers, no API keys, no tokens, no session IDs, no analytics IDs tied to a real account. Secrets go in environment variables loaded at runtime, never committed.
- No hardcoded local paths. No developer-machine-specific home directories (`/Users/<name>/...`, `C:\Users\<name>\...`, `/home/<name>/...`) in source, configs, scripts, CI, or comments. Paths come from workspace-relative literals (`assets/shaders/...`), runtime resolution (`dirs::config_dir()`, `std::env::current_exe()`), or environment variables.
- Pre-commit lint check: `cargo xtask check-secrets` blocks merges that introduce home-directory path patterns, email patterns, or common secret prefixes.
- `.env.example` checked in; `.env` is `.gitignore`d.
- Screenshots in `README.md` or `docs/` are scrubbed of system chrome that exposes usernames or local paths.
