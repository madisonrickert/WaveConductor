# Agent Instructions

These coding standards apply to all source contributions to WaveConductor v5. CI enforces them where it can; human and AI reviewers enforce the rest.

## Local development

- **Run the app:** `cargo rund` — the dev fast-iteration alias (`.cargo/config.toml`). It links Bevy as a shared library (`bevy/dynamic_linking`) so every rebuild after the first is a small incremental link instead of a full Bevy relink. Debug build; this is the default for "does the change work / sound right" smoke tests, and the command to prompt Madison with for manual testing.
- The `cargo rund` binary is **not self-contained** (libstd + `libbevy_dylib` are dynamically linked) — launch it via `cargo rund`, never the bare `target/` binary. `cargo run -p waveconductor` is the plain, statically-linked fallback.
- Dynamic linking is **dev-only**: never put `bevy/dynamic_linking` in a manifest `[features]` table — CI's `--all-features` would leak it into CI/release/WASM (it is incompatible with the release profile's fat-LTO + strip). Reserve `cargo build -p waveconductor --release` (~5–8 min) for explicit release-binary requests or pre-tag verification.

## Verifying changes

Run the gates CI enforces (`.github/workflows/ci.yml`) before claiming work done:

- `cargo fmt --all -- --check` — formatting. The `rustfmt.toml` "unstable features … only available in nightly" warnings are expected on stable and harmless.
- `cargo clippy --all-targets --all-features --workspace -- -D warnings` — lints are hard errors.
- `cargo nextest run --workspace --all-features` — tests (CI's runner). nextest does **not** run doctests; cover those with `cargo test --doc --workspace`. If nextest is absent, `cargo test --workspace --all-features` is a superset fallback.
- `cargo doc --no-deps --workspace --document-private-items` — rustdoc build; CI's `doc` job runs it with `RUSTDOCFLAGS="-D warnings"`, so broken intra-doc links are hard errors and the doc build is clean.
- `cargo deny check` — advisories, licenses, bans, sources. The single advisory gate: it reads the RustSec DB and honors `deny.toml` ignores (the redundant standalone `cargo audit` CI job was retired 2026-07-02 — it didn't honor those ignores; see `docs/runbooks/ci-cost-review.md`).
- `cargo xtask check-secrets` — blocks developer home-directory paths (`/Users/...`, `/home/...`, `C:\Users\...`), email addresses, and secret prefixes (AWS `AKIA...` keys, GitHub `ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_` tokens, `sk-` API keys, bearer tokens). It scans the whole tree except `vendor/`, `target/`, `.git/`, and the `docs/superpowers/` dated planning archive — living `docs/` (`docs/adr`, `docs/runbooks`, README) and `tests/` are scanned like any other tree.
- Rendered-sketch output: `cargo xtask capture <scenario>` — see the **Visual testing** section below and `tests/visual/CLAUDE.md`.

`--all-features` is deliberate — it exercises `hand-tracking-gestures` (leaprs) and, on macOS, `thermal-sensor-macos` (macmon). It does **not** enable `bevy/dynamic_linking`, which is alias-only (see above).

## In-code documentation

- `///` rustdoc on every public item (struct, enum, trait, fn, module).
- Module-level `//!` on every `mod.rs` or module root describing role and data flow.
- Document signal and data flow at plugin entry points (the `build()` method of each `Plugin`), not at every system call site.
- Inline `//` for math, DSP, and shader uniform contracts. Explain what each term in a formula represents.
- Never strip comments during refactors. Update stale comments rather than removing them.

## Code readability

- One concept per file, split when a file carries two unrelated responsibilities. ~300 lines is a guideline, not a hard cap — some UI/panel files legitimately run longer where the cohesion (one panel, one concern) outweighs the line count; `panel_user.rs` is being split as part of this same work as an example of when a large file has actually crossed into "two responsibilities" territory.
- Public API at the top, private helpers at the bottom, tests in a `#[cfg(test)] mod tests` block at the file footer.
- Prefer named structs over tuple structs once a type has more than one semantically meaningful field.
- No `unwrap()` or `expect()` in non-test code unless the panic is documented as an invariant violation.
- No `as` casts on numeric types where `From` / `TryFrom` / `u32::try_from` would work.
- Function bodies fit on one screen; if not, extract.

## File organization

- One sketch per directory; entry is `mod.rs`, never an inline single file.
- Shaders live in `assets/shaders/<sketch>/<name>.wgsl`. Never inline WGSL strings in Rust.
- Platform-specific code gets its own `platform/` submodule (e.g. `platform/native.rs`, `platform/macos.rs`, `platform/windows.rs`) when the platform surface is large enough to warrant one — see `lifecycle/thermal/platform/`. For a handful of lines, an inline `#[cfg(...)]` in the portable module is fine (e.g. `frame_limiter/`, `settings/persistence.rs`, `input/providers/mediapipe/inference_ort.rs`); don't force a submodule split for a one-line `cfg`.
- Test files colocated with source as `#[cfg(test)] mod tests`.
- No `src/utils/` or `src/helpers/` dumping grounds. Helpers live with the module that uses them; truly shared helpers go in a named module under `wc-core/`.

## Application performance

- Default target is multi-hour unattended thermal stability, not peak FPS.
- Sketches must run zero systems when in `SketchActivity::Idle`. This is enforced by convention and review, not a CI check — the sanctioned always-on exception is the three `restart_on_*_settings_change` systems (settings-reload listeners), which are expected to keep running in `Idle`.
- **Never allocate in a hot path.** A hot path is *any* code that runs repeatedly for the life of a session, not just the render frame: per-frame Bevy systems, **egui paint-callback `update`/`render` hooks**, the audio callback, **and continuously-running worker/background threads** (e.g. the input/inference worker loop in `wc-core/src/input/providers/mediapipe/`). The multi-hour soak target makes per-iteration allocation a thermal/jitter regression even off the render thread. Pre-allocate at init and reuse: own scratch buffers on the struct (or `bevy::ecs::system::Local`), refill with `vec.clear()` (keeps capacity) instead of reallocating, and take a reused buffer out via `std::mem::take` when it must be borrowed alongside `&mut self`. Allocating convenience wrappers are fine in tests/benchmarks but must not sit on the steady-state path. Where a dependency's API forces a residual copy (e.g. an inference backend that owns its input tensor), document the exact cost inline and flag it as a profiling-gated follow-up rather than leaving it silent.
- Audio thread is real-time-friendly: lock-free ring buffers only, no `Mutex`, no allocations after init.
- GPU resources are released by three distinct mechanisms, and it matters which one applies:
  1. **Entity-owned** resources (meshes, materials, storage buffers held via `Handle`s on a sketch's root entity) are released when `OnExit` despawns that entity.
  2. **Render-world `Resource`s** are *not* touched by an entity despawn. `ExtractResourcePlugin` does not propagate removals, so each one needs an explicit removal system (see `remove_particle_sim_params_if_absent` in `line/particles/compute.rs` and its siblings).
  3. **Render-world `Local` caches** (bind groups keyed by `TextureViewId`, per-widget GPU slots) are owned by no entity and survive every state transition. Each must be **bounded by construction** and must revalidate against the id of the resource it actually holds — never against a proxy such as the window size. Bevy reallocates a `ViewTarget` on any change to `(camera.target, texture_usage, main_texture_format, Msaa)`, so a size-keyed cache is silently wrong the first time anything toggles HDR or MSAA. The reference shape is upstream's `bevy_core_pipeline::fullscreen_material::FullscreenMaterialBindGroup`: two slots, one per ping-pong view, each compared against the `TextureViewId` it binds. `line/post_process.rs` and `dots/post_process.rs` follow it; `hand_mesh/bone_composite.rs` uses an older id-keyed clear.
  A resource that fits none of these three leaks. `Box::leak` on a wgpu handle always leaks, because the handle owns `Arc` references to everything it binds; `clippy.toml` bans it.
- Compute shader dispatch sizes scale with settings; do not dispatch unused workgroups.
- An 8-hour soak test is required before any release tag. Today this is a manual procedure: run the app under representative load (hand tracking + audio active, sketch cycling) on the target deployment hardware for ~8 hours and watch RSS, GPU memory, and FPS for drift or a thermal-induced stall. An agent-operable `cargo xtask soak-test` command that automates this is planned but not yet implemented — don't cite it as if it exists.

## Visual testing

- Rendered sketches have a deterministic capture + regression harness: `cargo xtask capture <scenario>`. Run `cargo xtask manifest` to see all xtask subcommands, `cargo xtask capture --list` for scenarios, and read `tests/visual/CLAUDE.md` for the full surface (flags, `--json` shape, the `WC_DEBUG_*` render-stage isolation toggles, and how to add a scenario or update baselines).
- Prefer it over ad-hoc screenshot scripts when diagnosing or regression-testing a sketch's rendered output. It pins a fixed sim timestep (so captures are reproducible) and writes a self-describing `run.json`; the operating agent reviews the captured PNGs itself (no LLM API spend).
- The capture system and `WC_DEBUG_*` toggles are `#[cfg(debug_assertions)]`-gated and absent from release builds; never enable `debug-assertions` on a release/soak profile (see the guard on `[profile.release]` in `Cargo.toml`).

## Security and privacy

- No private personal information in the repo. No real email addresses (use `noreply.github.com` or placeholder), no phone numbers, no API keys, no tokens, no session IDs, no analytics IDs tied to a real account. Secrets go in environment variables loaded at runtime, never committed.
- No hardcoded local paths. No developer-machine-specific home directories (`/Users/<name>/...`, `C:\Users\<name>\...`, `/home/<name>/...`) in source, configs, scripts, CI, or comments. Paths come from workspace-relative literals (`assets/shaders/...`), runtime resolution (`dirs::config_dir()`, `std::env::current_exe()`), or environment variables.
- Pre-commit lint check: `cargo xtask check-secrets` blocks merges that introduce home-directory path patterns, email patterns, or the AWS/GitHub/`sk-`/bearer-token secret prefixes listed above; it scans the whole tree except `vendor/`, `target/`, and `.git/`.
- `.env` is `.gitignore`d. (Nothing at runtime currently needs a secret env var, so there's no `.env.example` to check in.)
- Screenshots in `README.md` or `docs/` are scrubbed of system chrome that exposes usernames or local paths.
