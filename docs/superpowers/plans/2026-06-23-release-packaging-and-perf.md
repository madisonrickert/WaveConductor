# Release Packaging + Bone-Camera Gating Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make the release binary find its assets in every launch context, ship a macOS `.app` bundle, and stop the bone-wireframe render passes from running when no hand is tracked.

**Architecture:** One runtime `asset_root()` resolver in `wc-core` serves both Bevy's `AssetPlugin` and the MediaPipe model loader, covering dev-debug, dev-release, and a bundled `.app`. A `cargo xtask bundle-mac` subcommand assembles `WaveConductor.app`. The bone camera + composite gate on tracked-hand presence.

**Tech Stack:** Rust / Bevy 0.19, `clap`-based xtask, macOS `.app` bundle (Info.plist + `Contents/{MacOS,Resources}`).

## Global Constraints

- No new dependencies (xtask uses `std::fs` + the existing release build; no `cargo-bundle`).
- No hardcoded home-dir paths in source (the `cargo xtask check-secrets` gate). `env!("CARGO_MANIFEST_DIR")` is allowed only behind `#[cfg(debug_assertions)]` so it is never baked into the shipped release binary.
- No `unwrap()`/`expect()` in non-test code; `///` on public items; per-frame systems allocation-free.
- Behavior preservation: the asset-resolution change must keep `cargo rund` (debug) working exactly as before (it already finds `../../assets`).
- Gates before "done": `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features --workspace -- -D warnings`; `cargo nextest run --workspace --all-features`; `cargo build -p waveconductor` (and a release build for Task 3).

---

### Task 1: Unified runtime `asset_root()` resolver (makes release run)

**Files:**
- Create: `crates/wc-core/src/platform/assets.rs` (or nearest existing platform/util module — search first; do NOT make a `utils/` dump)
- Modify: `crates/waveconductor/src/main.rs` (`AssetPlugin.file_path`; optionally `LINE_BACKGROUND_PATH`)
- Modify: `crates/wc-core/src/input/providers/mediapipe/mod.rs` (the model-dir default near line 98 — currently defaults to a workspace/cwd-relative `assets/models/hand`)
- Test: unit tests for `asset_root()` in the new module

**Interfaces:**
- Produces: `pub fn asset_root() -> std::path::PathBuf` and a convenience `pub fn model_dir() -> PathBuf { asset_root().join("models/hand") }` (or have the caller join).

**Design** — resolve in this priority order, each guarded by an existence check so a missing candidate falls through:
```rust
pub fn asset_root() -> PathBuf {
    // 1. Explicit override (deployment / CI / `cargo run` from elsewhere).
    if let Some(p) = std::env::var_os("WAVECONDUCTOR_ASSET_ROOT") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // 2. macOS .app bundle: <App>.app/Contents/MacOS/<bin> -> ../Resources/assets
            if dir.ends_with("Contents/MacOS") {
                if let Some(contents) = dir.parent() {
                    let res = contents.join("Resources").join("assets");
                    if res.is_dir() { return res; }
                }
            }
            // 3. assets staged next to the binary.
            let next = dir.join("assets");
            if next.is_dir() { return next; }
        }
    }
    // 4. Dev tree only (NEVER baked into a release binary): workspace assets.
    #[cfg(debug_assertions)]
    {
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        if dev.is_dir() { return dev; }
    }
    // 5. cwd-relative, made absolute so Bevy's FileAssetReader (which resolves
    //    relative paths against current_exe in release) still finds it when the
    //    binary is launched from the workspace root (`cargo run --release`).
    std::env::current_dir()
        .map(|d| d.join("assets"))
        .unwrap_or_else(|_| PathBuf::from("assets"))
}
```
Wire-up:
- `main.rs`: replace the `#[cfg(debug_assertions)] file_path: "../../assets"` block with `file_path: wc_core::<module>::asset_root().to_string_lossy().into_owned()` for BOTH profiles. (Bevy accepts an absolute `file_path`.)
- `mediapipe/mod.rs`: the model-dir default becomes `asset_root().join("models/hand")` instead of the cwd-relative literal.
- This makes: dev-debug (`cargo rund`, cwd=repo) → step 4; dev-release (`cargo run -p waveconductor --release` from repo root) → step 5 (absolute cwd/assets); `.app` → step 2.

- [ ] **Step 1: Write failing tests** for `asset_root()`: (a) `WAVECONDUCTOR_ASSET_ROOT` override wins (set+unset around the assertion); (b) a synthetic `<tmp>/Foo.app/Contents/MacOS/` exe path with a `Contents/Resources/assets` dir resolves to that Resources path (construct the dirs in a tempdir, but DON'T rely on `current_exe()` — refactor the bundle/next-to-exe logic into a pure helper `fn resolve_from_exe_dir(dir: &Path) -> Option<PathBuf>` that the test can call directly with a fabricated dir). The env-var and exe-dir branches must both be covered by a pure, `current_exe`-independent helper.
- [ ] **Step 2: Run** `cargo test -p wc-core --lib` (the new module) → FAIL.
- [ ] **Step 3: Implement** the resolver + the pure `resolve_from_exe_dir` helper + wire `main.rs` and `mediapipe/mod.rs`.
- [ ] **Step 4: Run** `cargo test -p wc-core --lib`; `cargo clippy -p wc-core -p waveconductor --all-targets --all-features -- -D warnings`; `cargo build -p waveconductor`. All pass.
- [ ] **Step 5: Headless launch check (the "make release run" verification).** `cargo build -p waveconductor --release`, then run the release binary FROM THE REPO ROOT in the background for ~8 s capturing logs, then kill it; grep the log for `Path not found` / `not found`. EXPECT: no asset/shader/model "Path not found" lines (the window opening on screen is expected; this confirms asset resolution, not rendering). Report the captured log tail.
- [ ] **Step 6: Commit** — `fix(assets): unified runtime asset_root() so release + bundle find assets`.

---

### Task 2: Gate the bone camera + composite on tracked-hand presence

**Files:**
- Modify: `crates/wc-sketches/src/dots/hand_mesh.rs` (bone `Camera3d` — toggle `is_active` on hand presence) and `crates/wc-sketches/src/dots/bone_composite.rs` (skip the composite pass when no hands)
- Modify: `crates/wc-sketches/src/dots/mod.rs` (register the presence system)
- Test: a unit/system test that the camera goes inactive with zero `TrackedHand` and active with ≥1

**Design:**
- Add an `Update` system gated `sketch_active(AppState::Dots)`: query `Query<(), With<TrackedHand>>`; set the bone `Camera3d`'s `is_active = !hands.is_empty()`. An inactive camera is skipped wholesale by Bevy's render graph (no extract, no pass) — this removes the MSAA×4 full-res 3D pass when no hand is present, at ~zero runtime cost (a bool write + an O(≤2) presence check).
- The composite must ALSO skip when no hands, or it composites a stale (last-rendered) bone texture as ghost bones. Mirror the existing "no target → early-return BEFORE `post_process_write`" pattern in `bone_composite.rs` (the same pattern `WC_DEBUG_DISABLE_BONE_COMPOSITE` uses): extract a `bool` hands-present flag to the render world (a tiny `ExtractResource`, or reuse the camera's active state) and early-return the composite node when false, BEFORE flipping the ping-pong (so the chain stays correct).
- Keep the `WC_DEBUG_DISABLE_BONE_CAMERA` toggle working. Preserve the hands-present path exactly: when ≥1 hand, both run as today.
- **CAUTION (verification boundary):** the no-hands path is verifiable headlessly / by the operator on a desktop; the hands-PRESENT path (bones still render correctly when a hand appears, no ghosting when it leaves) needs Leap/MediaPipe hardware and is operator-deferred. Do NOT regress the hands-present rendering — when in doubt, keep it identical to today.

- [ ] **Step 1: Write failing test** — spawn a Dots world with the bone camera; with zero `TrackedHand`, after running the presence system the camera `is_active == false`; spawn a `TrackedHand`, run again, `is_active == true`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** the presence system + the composite early-return-when-no-hands (extracting the flag), preserving the ping-pong contract and the hands-present path.
- [ ] **Step 4: Run** `cargo test -p wc-sketches --lib dots`; clippy; `cargo build -p waveconductor` (naga validates any shader/graph change).
- [ ] **Step 5: Commit** — `perf(dots): gate the bone camera + composite on tracked-hand presence`.

---

### Task 3: `cargo xtask bundle-mac` → `WaveConductor.app`

**Files:**
- Create: `xtask/src/bundle_mac.rs`
- Modify: `xtask/src/main.rs` (add `BundleMac(bundle_mac::Args)` to the `Command` enum + dispatch arm + `mod bundle_mac;`)
- Modify: the harness CLAUDE.md / xtask manifest if subcommands are documented there (search; keep `manifest` output in sync)

**Design** — a subcommand that produces `target/WaveConductor.app`:
1. Run `cargo build -p waveconductor --release` (shell out via `std::process::Command`; surface a clear error if it fails). Accept a `--skip-build` flag to bundle an already-built binary.
2. Assemble the bundle skeleton under `target/WaveConductor.app/Contents/`:
   - `MacOS/waveconductor` ← copy `target/release/waveconductor`, preserve the executable bit.
   - `Resources/assets/` ← copy the entire workspace `assets/` tree (shaders, sketches, textures, AND `models/hand/*.onnx`). This is what `asset_root()` step 2 resolves to.
   - `Info.plist` ← generate (see below).
   - `PkgInfo` ← `APPL????` (optional but conventional).
3. `Info.plist` (XML) keys — these are REQUIRED for a working kiosk app:
   - `CFBundleName` = `WaveConductor`; `CFBundleDisplayName` = `WaveConductor`.
   - `CFBundleIdentifier` = `com.madisonrickert.waveconductor`.
   - `CFBundleExecutable` = `waveconductor`; `CFBundlePackageType` = `APPL`.
   - `CFBundleVersion` / `CFBundleShortVersionString` = the crate version (read `env!("CARGO_PKG_VERSION")` of waveconductor, or pass via arg).
   - `NSHighResolutionCapable` = `true` (Retina; without it macOS renders the window upscaled/blurry at 1×).
   - **`NSCameraUsageDescription`** = a human string e.g. `"WaveConductor uses the camera for hand-gesture tracking."` — MANDATORY: without it macOS denies camera access and the MediaPipe provider fails (no hand tracking).
   - `LSMinimumSystemVersion` = a sane floor (e.g. `"12.0"`).
   - `LSApplicationCategoryType` = `public.app-category.entertainment` (optional).
4. `--json` output mode (every xtask subcommand supports it): emit the `.app` path, the binary version, byte size, and asset file count.
5. Print, in human mode, a one-line "open with: `open target/WaveConductor.app`" hint and a note that an unsigned app needs a right-click → Open (or `xattr -dr com.apple.quarantine target/WaveConductor.app`) the first time.
6. **Out of scope (note in output, do not implement):** code-signing / notarization (needed only for distribution off this machine; local kiosk runs unsigned), and a custom `.icns` icon (ships with the default until an icon asset exists).

- [ ] **Step 1: Write a failing test** for the pure Info.plist generator: `fn info_plist(name, ident, exe, version, camera_usage) -> String` must contain `<key>NSCameraUsageDescription</key>`, `<key>NSHighResolutionCapable</key><true/>`, the identifier, and be well-formed (starts with the plist DOCTYPE). Keep the filesystem assembly (copying) out of the unit test; test only the pure plist string + any pure path-derivation helpers.
- [ ] **Step 2: Run** `cargo test -p xtask` → FAIL.
- [ ] **Step 3: Implement** `bundle_mac.rs` (Args with `--json`/`--skip-build`, the build shell-out, the copy/assemble, the plist generator) + wire into `main.rs`.
- [ ] **Step 4: Run** `cargo test -p xtask`; `cargo clippy -p xtask --all-targets -- -D warnings`; then actually run `cargo xtask bundle-mac` and confirm `target/WaveConductor.app` exists with `Contents/MacOS/waveconductor`, `Contents/Resources/assets/shaders/dots/explode.wgsl`, `Contents/Resources/assets/models/hand/palm_detection.onnx`, and a valid `Info.plist` (`plutil -lint target/WaveConductor.app/Contents/Info.plist` if available). Report the `--json` output.
- [ ] **Step 5: Commit** — `feat(xtask): bundle-mac subcommand builds WaveConductor.app`.

---

## Final verification

- Full gate suite (fmt / clippy --all-features --workspace -D / nextest --workspace --all-features / doctests / check-secrets / build).
- `cargo xtask manifest` lists `bundle-mac`.
- Operator-deferred: launch `WaveConductor.app` from Finder (camera permission prompt appears; hand tracking works); verify the bone-gating hands-present path on Leap/MediaPipe hardware; compare release FPS vs debug now that release runs.
