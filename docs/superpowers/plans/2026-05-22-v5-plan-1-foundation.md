# v5 Plan 1: Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Take the WaveConductor repo from a v4 React/Three.js/Electron tree on `rewrite/bevy` to a clean Bevy workspace skeleton with all CI gates green and an empty Bevy window that opens via `cargo run`.

**Architecture:** Cargo workspace with three placeholder app crates (`waveconductor` binary, `wc-core` library, `wc-sketches` library) plus an `xtask` dispatcher. CI runs format, lint, secret scan, dependency policy, audit, doc build, and tests on every push. v4 source tree is deleted from `rewrite/bevy`; v4 lives only on `main` until v5.0 ships.

**Tech Stack:** Rust stable 1.85+, Bevy 0.18, Cargo workspaces, GitHub Actions, `cargo-deny`, `cargo-audit`, `cargo-nextest`, `cargo-llvm-cov`.

**Reference spec:** `docs/superpowers/specs/2026-05-22-bevy-rewrite-design.md`

**Branch:** All work commits to `rewrite/bevy`. Do not touch `main`.

---

## File map

**Created in this plan:**

- `rust-toolchain.toml` — pins Rust channel and components
- `rustfmt.toml` — workspace formatter config
- `clippy.toml` — workspace clippy config
- `deny.toml` — `cargo-deny` policy
- `.gitignore` — Rust + Bevy + OS noise
- `.cargo/config.toml` — `cargo xtask` alias
- `CLAUDE.md` — one-line pointer to AGENTS.md
- `AGENTS.md` — rewritten coding standards (five sections)
- `README.md` — replaced with v5 placeholder text
- `Cargo.toml` — workspace manifest
- `xtask/Cargo.toml`, `xtask/src/main.rs`, `xtask/src/check_secrets.rs`, `xtask/src/manifest.rs`, `xtask/tests/check_secrets.rs`
- `crates/waveconductor/Cargo.toml`, `crates/waveconductor/src/main.rs`
- `crates/wc-core/Cargo.toml`, `crates/wc-core/src/lib.rs`
- `crates/wc-sketches/Cargo.toml`, `crates/wc-sketches/src/lib.rs`
- `assets/.gitkeep`
- `docs/adr/README.md`
- `.github/workflows/ci.yml`

**Deleted in this plan (v4 files removed from `rewrite/bevy` only; preserved on `main`):**

- `src/` (all v4 TypeScript/React source)
- `electron/` (Electron main process)
- `scripts/` (leap-websocket.ts)
- `bin/` (Ultraleap-Tracking-WS binaries)
- `dist/`, `dist-electron/`, `release/`, `build/`
- `node_modules/` (if present)
- `package.json`, `package-lock.json`
- `vite.config.ts`, `vitest.config.ts`
- `tsconfig.json`, `tsconfig.test.json`
- `eslint.config.mjs`
- `index.html`
- `WaveConductor.code-workspace`

**Preserved:**

- `docs/` (specs and ADRs)
- `LICENSE`
- `icon.png` (will be reused for bundling)
- `screenshot.png` (placeholder; updated post-v5)
- `.git/` (obviously)

---

## Task 1: Verify branch and clean working tree

**Files:** none

- [ ] **Step 1: Confirm correct branch and clean state**

Run:
```bash
git rev-parse --abbrev-ref HEAD
git status --short
```

Expected:
```
rewrite/bevy
(empty - clean tree)
```

If the branch is not `rewrite/bevy`, run `git checkout rewrite/bevy`. If the tree has uncommitted changes, stop and resolve them before continuing.

- [ ] **Step 2: Confirm v4 files are present (we're about to delete them)**

Run:
```bash
ls package.json src electron 2>/dev/null | head
```

Expected:
```
electron
src
package.json
```

This is the starting state. The next task removes these.

---

## Task 2: Remove v4 source tree

**Files:** delete the v4 tree as listed in the File Map.

- [ ] **Step 1: Delete v4 build outputs and dependencies first**

Run:
```bash
rm -rf node_modules dist dist-electron release build
```

Expected: silent success.

- [ ] **Step 2: Delete v4 source directories**

Run:
```bash
rm -rf src electron scripts bin
```

Expected: silent success.

- [ ] **Step 3: Delete v4 root-level config files**

Run:
```bash
rm -f package.json package-lock.json \
      vite.config.ts vitest.config.ts \
      tsconfig.json tsconfig.test.json \
      eslint.config.mjs \
      index.html \
      WaveConductor.code-workspace
```

Expected: silent success.

- [ ] **Step 4: Verify preserved files survived**

Run:
```bash
ls LICENSE icon.png screenshot.png docs README.md AGENTS.md
```

Expected: all six listed without error.

- [ ] **Step 5: Verify git status reflects deletions**

Run:
```bash
git status --short | head -20
```

Expected output begins with lines like:
```
 D AGENTS.md
 D eslint.config.mjs
 D index.html
 D package-lock.json
 D package.json
 D src/app.test.tsx
...
```

(Many `D` entries. `AGENTS.md` is listed because we will rewrite it; the deletion will be staged together with the new content in Task 5.)

- [ ] **Step 6: Stage and commit the deletion**

Note: do not commit `AGENTS.md` deletion yet — its rewrite happens in Task 5 and goes in the same commit. Restore it for now:

```bash
git restore AGENTS.md
git add -A
git status --short | head -5
```

Expected output now shows deletions only, no `AGENTS.md`.

Run:
```bash
git commit -m "$(cat <<'EOF'
Remove v4 source tree from rewrite/bevy

v5 will live in this branch; v4 continues on main and tagged releases.
The perf-audit harness (Plan 6) reaches v4 via a sibling checkout.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 3: Pin Rust toolchain

**Files:** Create `rust-toolchain.toml`

- [ ] **Step 1: Create rust-toolchain.toml**

Create `rust-toolchain.toml` with this exact content:

```toml
[toolchain]
channel = "1.85.0"
components = ["rustfmt", "clippy", "llvm-tools-preview"]
profile = "minimal"
```

- [ ] **Step 2: Verify toolchain installs**

Run:
```bash
rustc --version
```

Expected: `rustc 1.85.0` (rustup will auto-install the channel if needed).

If rustup is not installed, install it first via `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`.

---

## Task 4: Author CLAUDE.md and AGENTS.md

**Files:** Create `CLAUDE.md`; rewrite `AGENTS.md`.

- [ ] **Step 1: Create CLAUDE.md**

Create `CLAUDE.md` with this exact content:

```
@AGENTS.md
```

(One line, no trailing newline matters.)

- [ ] **Step 2: Rewrite AGENTS.md**

Replace the entire contents of `AGENTS.md` with the content from §6.2 of the design spec. Use this exact content:

````markdown
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

## Security and privacy

- No private personal information in the repo. No real email addresses (use `noreply.github.com` or placeholder), no phone numbers, no API keys, no tokens, no session IDs, no analytics IDs tied to a real account. Secrets go in environment variables loaded at runtime, never committed.
- No hardcoded local paths. No developer-machine-specific home directories (`/Users/<name>/...`, `C:\Users\<name>\...`, `/home/<name>/...`) in source, configs, scripts, CI, or comments. Paths come from workspace-relative literals (`assets/shaders/...`), runtime resolution (`dirs::config_dir()`, `std::env::current_exe()`), or environment variables.
- Pre-commit lint check: `cargo xtask check-secrets` blocks merges that introduce home-directory path patterns, email patterns, or common secret prefixes.
- `.env.example` checked in; `.env` is `.gitignore`d.
- Screenshots in `README.md` or `docs/` are scrubbed of system chrome that exposes usernames or local paths.
````

- [ ] **Step 3: Verify both files**

Run:
```bash
cat CLAUDE.md
head -3 AGENTS.md
```

Expected:
```
@AGENTS.md
# Agent Instructions

These coding standards apply to all source contributions to WaveConductor v5. CI enforces them where it can; human and AI reviewers enforce the rest.
```

---

## Task 5: Replace README.md with v5 placeholder

**Files:** Rewrite `README.md`.

- [ ] **Step 1: Replace README.md**

Replace the entire contents of `README.md` with:

````markdown
# WaveConductor

[![License](https://img.shields.io/github/license/madisonrickert/WaveConductor)](LICENSE)

Interactive art gallery. Five generative-art sketches with hand-tracking and audio reactivity.

> **v5 is under construction on the `rewrite/bevy` branch.** The current shipping release is v4 — see [Releases](../../releases) for binaries.
>
> v5 is a from-scratch rewrite in Rust on the Bevy engine, designed for multi-hour unattended thermal stability. See `docs/superpowers/specs/2026-05-22-bevy-rewrite-design.md` for the design.

## Development (v5)

```sh
cargo run -p waveconductor
```

Requires Rust 1.85+. Pinned via `rust-toolchain.toml`.

## Documentation

- `AGENTS.md` — coding standards
- `docs/superpowers/specs/` — design specs
- `docs/superpowers/plans/` — implementation plans
- `docs/adr/` — architecture decision records

## License

See [LICENSE](LICENSE).
````

- [ ] **Step 2: Verify README**

Run:
```bash
head -5 README.md
```

Expected:
```
# WaveConductor

[![License](https://img.shields.io/github/license/madisonrickert/WaveConductor)](LICENSE)

Interactive art gallery. Five generative-art sketches with hand-tracking and audio reactivity.
```

---

## Task 6: Author .gitignore

**Files:** Replace `.gitignore`.

- [ ] **Step 1: Check current .gitignore**

Run:
```bash
cat .gitignore 2>/dev/null || echo "no .gitignore"
```

Whatever was there is v4-shaped. Replace fully.

- [ ] **Step 2: Replace .gitignore**

Replace the entire contents of `.gitignore` with:

```gitignore
# Rust / Cargo
/target/
**/*.rs.bk
Cargo.lock.bak

# IDE
.vscode/
.idea/
*.swp
*.swo
*~

# OS
.DS_Store
Thumbs.db

# Build artifacts
/dist/
/release/
/release-artifacts/

# Local env
.env
.env.local

# Coverage
/coverage/
/lcov.info

# Web build outputs (Plan 5)
/web/dist/
/web/.trunk/

# Perf audit outputs (Plan 6) — keep reports, ignore intermediate raw data
/perf-harness/raw/

# Misc
*.log
```

- [ ] **Step 3: Verify**

Run:
```bash
grep -c "/target/" .gitignore
```

Expected: `1` (or higher).

---

## Task 7: Author workspace Cargo.toml

**Files:** Create root `Cargo.toml`.

- [ ] **Step 1: Create workspace Cargo.toml**

Create `Cargo.toml` at repo root with this exact content:

```toml
[workspace]
resolver = "2"
members = [
    "crates/waveconductor",
    "crates/wc-core",
    "crates/wc-sketches",
    "xtask",
]

[workspace.package]
version = "5.0.0-dev"
edition = "2021"
license = "MIT"
authors = ["Madison Rickert <3495636+madisonrickert@users.noreply.github.com>"]
repository = "https://github.com/madisonrickert/WaveConductor"
rust-version = "1.85"

[workspace.dependencies]
# Core engine. Note: bevy_audio is deliberately omitted (spec §5.4 says we use
# cpal+fundsp+rustfft directly, not bevy_audio). DefaultPlugins gracefully
# skips AudioPlugin when the feature is absent.
bevy = { version = "0.18", default-features = false, features = [
    "bevy_winit",
    "bevy_render",
    "bevy_pbr",
    "bevy_core_pipeline",
    "bevy_asset",
    "default_font",
    "x11",
    "wayland",
    "webgl2",
    "tonemapping_luts",
] }
# UI
bevy_egui = "0.30"
bevy-inspector-egui = "0.27"
# Input action mapping (added in Plan 2; declared here so workspace pins the version)
leafwing-input-manager = "0.16"
# Diagnostics & debug
bevy_mod_debugdump = "0.13"
# Errors & logging
thiserror = "1"
tracing = "0.1"
# Serde stack (used by settings persistence in Plan 2)
serde = { version = "1", features = ["derive"] }
toml = "0.8"
# Native config dirs
dirs = "5"
# xtask
clap = { version = "4", features = ["derive"] }
walkdir = "2"
regex = "1"
# Workspace-internal crates
wc-core = { path = "crates/wc-core" }
wc-sketches = { path = "crates/wc-sketches" }

[workspace.lints.rust]
unsafe_code = "deny"
missing_docs = "warn"
rust_2018_idioms = "warn"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
unwrap_used = "warn"
expect_used = "warn"
panic = "warn"
as_conversions = "warn"
# Allowed pedantic lints (too noisy for game/sketch code):
module_name_repetitions = "allow"
missing_errors_doc = "allow"
must_use_candidate = "allow"

[profile.dev]
opt-level = 1
debug = true

[profile.dev.package."*"]
opt-level = 3

[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"
```

- [ ] **Step 2: Verify the manifest parses**

Run:
```bash
cargo metadata --format-version 1 --no-deps > /dev/null && echo OK
```

Expected: `OK`.

If parse fails, fix the TOML syntax.

---

## Task 8: Configure rustfmt and clippy

**Files:** Create `rustfmt.toml`, `clippy.toml`.

- [ ] **Step 1: Create rustfmt.toml**

Create `rustfmt.toml` with this exact content:

```toml
edition = "2021"
imports_granularity = "Crate"
group_imports = "StdExternalCrate"
newline_style = "Unix"
use_field_init_shorthand = true
use_try_shorthand = true
```

- [ ] **Step 2: Create clippy.toml**

Create `clippy.toml` with this exact content:

```toml
# MSRV used by clippy for lint behavior; matches rust-version in workspace Cargo.toml
msrv = "1.85"
# Cap the cognitive complexity of a single function before clippy::cognitive_complexity warns
cognitive-complexity-threshold = 25
# Cap arguments per function
too-many-arguments-threshold = 8
# Allow longer enum variant names (HandTrackingProvider etc.)
enum-variant-name-threshold = 5
```

- [ ] **Step 3: Verify rustfmt is happy with itself**

Run:
```bash
cargo fmt --all -- --check
```

Expected: silent success (exit 0). There's no source to format yet, but the config must parse.

---

## Task 9: Configure cargo-deny

**Files:** Create `deny.toml`.

- [ ] **Step 1: Create deny.toml**

Create `deny.toml` with this exact content:

```toml
[graph]
all-features = true

[advisories]
db-urls = ["https://github.com/RustSec/advisory-db"]
ignore = []

[licenses]
allow = [
    "MIT",
    "MIT-0",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Unicode-DFS-2016",
    "Unicode-3.0",
    "Zlib",
    "CC0-1.0",
    "BSL-1.0",
    "MPL-2.0",
]
confidence-threshold = 0.93

[bans]
multiple-versions = "warn"
wildcards = "deny"
deny = []

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
allow-git = []
```

- [ ] **Step 2: Verify deny.toml parses (no installation required to test)**

Run:
```bash
test -f deny.toml && echo "exists"
```

Expected: `exists`.

Actual `cargo deny check` will run in CI; locally it requires `cargo install cargo-deny`. Not required for the plan.

---

## Task 10: Create the .cargo/config.toml alias for xtask

**Files:** Create `.cargo/config.toml`.

- [ ] **Step 1: Create .cargo directory and config**

Run:
```bash
mkdir -p .cargo
```

Create `.cargo/config.toml` with this exact content:

```toml
[alias]
xtask = "run --quiet --package xtask --"

[build]
# Enable parallel frontend; speeds up incremental builds on multi-core machines.
# Stable since 1.74. Comment out if it causes flakiness on a specific machine.
# rustflags = ["-Z", "threads=8"]
```

- [ ] **Step 2: Verify alias is recognized**

Run:
```bash
cargo xtask --help 2>&1 | head -3
```

Expected: error or help text mentioning that the `xtask` package does not yet exist. The alias is recognized; xtask itself is built in Task 11.

---

## Task 11: Scaffold the xtask crate

**Files:** Create `xtask/Cargo.toml`, `xtask/src/main.rs`.

- [ ] **Step 1: Create xtask/Cargo.toml**

Create `xtask/Cargo.toml` with this exact content:

```toml
[package]
name = "xtask"
version.workspace = true
edition.workspace = true
license.workspace = true
publish = false

[dependencies]
clap = { workspace = true }
walkdir = { workspace = true }
regex = { workspace = true }
thiserror = { workspace = true }
```

- [ ] **Step 2: Create xtask/src/main.rs**

Create `xtask/src/main.rs` with this exact content:

```rust
//! `cargo xtask` dispatcher.
//!
//! Single binary providing the agent-friendly subcommands documented in the
//! workspace design spec (§5.10). Every subcommand accepts `--json` for
//! machine-readable output. New subcommands are added as modules under
//! `xtask/src/` and registered in [`Cli`].

#![allow(clippy::print_stdout, reason = "xtask is a CLI; printing is its job")]

mod check_secrets;
mod manifest;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", version, about = "WaveConductor workspace dispatcher")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List all xtask subcommands with descriptions.
    Manifest(manifest::Args),
    /// Regex-scan the working tree for forbidden secrets and local paths.
    CheckSecrets(check_secrets::Args),
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Manifest(args) => manifest::run(args),
        Command::CheckSecrets(args) => check_secrets::run(args),
    }
}
```

- [ ] **Step 3: Verify xtask compiles even though subcommand modules are not yet present**

Skip the build for now — modules are added in Task 12 and 13. Verify only that the file is syntactically present.

Run:
```bash
test -f xtask/src/main.rs && wc -l xtask/src/main.rs
```

Expected: a positive line count (~30).

---

## Task 12: Implement xtask `manifest` subcommand

**Files:** Create `xtask/src/manifest.rs`.

- [ ] **Step 1: Write failing test (integration test stub)**

Create `xtask/tests/manifest.rs` with this exact content:

```rust
//! Integration test for `cargo xtask manifest`.

use std::process::Command;

#[test]
fn manifest_lists_subcommands() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("manifest")
        .output()
        .expect("failed to spawn xtask");
    assert!(
        output.status.success(),
        "manifest subcommand failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("manifest output not utf8");
    assert!(stdout.contains("manifest"), "manifest output should list itself: {stdout}");
    assert!(stdout.contains("check-secrets"), "manifest output should list check-secrets: {stdout}");
}
```

- [ ] **Step 2: Run test, verify it fails**

Run:
```bash
cargo test --package xtask --test manifest 2>&1 | tail -10
```

Expected: build failure or test failure (manifest module does not yet compile).

- [ ] **Step 3: Implement manifest module**

Create `xtask/src/manifest.rs` with this exact content:

```rust
//! `cargo xtask manifest` — list all xtask subcommands with one-line descriptions.
//!
//! Used by tooling (and agentic harnesses) to discover what xtask can do without
//! parsing `--help` output. The entries are hand-maintained here so the listing
//! always matches the registered subcommands in `main.rs`.

use clap::Parser;

#[derive(Parser)]
pub struct Args {
    /// Emit JSON instead of the human-readable table.
    #[arg(long)]
    pub json: bool,
}

/// One subcommand entry in the manifest table.
struct Entry {
    name: &'static str,
    description: &'static str,
}

const ENTRIES: &[Entry] = &[
    Entry {
        name: "manifest",
        description: "List all xtask subcommands with descriptions.",
    },
    Entry {
        name: "check-secrets",
        description: "Regex-scan the working tree for forbidden secrets and local paths.",
    },
];

/// Print the manifest in either human or JSON form.
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    if args.json {
        print_json();
    } else {
        print_human();
    }
    Ok(())
}

fn print_human() {
    println!("xtask subcommands:");
    for entry in ENTRIES {
        println!("  {:16}  {}", entry.name, entry.description);
    }
}

fn print_json() {
    print!("[");
    let mut first = true;
    for entry in ENTRIES {
        if !first {
            print!(",");
        }
        first = false;
        print!(
            r#"{{"name":"{}","description":"{}"}}"#,
            entry.name, entry.description
        );
    }
    println!("]");
}
```

- [ ] **Step 4: Run test, verify it passes**

Run:
```bash
cargo test --package xtask --test manifest 2>&1 | tail -5
```

Expected: `test result: ok. 1 passed`.

If still failing because `check_secrets.rs` doesn't compile, proceed to Task 13 first and re-run.

---

## Task 13: Implement xtask `check-secrets` subcommand

**Files:** Create `xtask/src/check_secrets.rs`, `xtask/tests/check_secrets.rs`.

- [ ] **Step 1: Write failing integration test**

Create `xtask/tests/check_secrets.rs` with this exact content:

```rust
//! Integration test for `cargo xtask check-secrets`.

use std::fs;
use std::process::Command;

/// Helper: run check-secrets against a temp directory containing the supplied file.
fn run_against(file_contents: &str) -> (bool, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("planted.rs");
    fs::write(&path, file_contents).expect("write planted file");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("check-secrets")
        .arg("--root")
        .arg(tmp.path())
        .output()
        .expect("spawn xtask");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    (output.status.success(), format!("{stdout}{stderr}"))
}

#[test]
fn clean_tree_passes() {
    let (ok, out) = run_against("fn main() { println!(\"hi\"); }\n");
    assert!(ok, "clean tree should pass: {out}");
}

#[test]
fn home_dir_path_is_flagged() {
    let (ok, out) = run_against("// path: /Users/alice/Developer/foo\n");
    assert!(!ok, "home-dir path should be flagged");
    assert!(out.contains("/Users/"), "report should mention the offending pattern: {out}");
}

#[test]
fn windows_home_dir_path_is_flagged() {
    let (ok, out) = run_against("// path: C:\\\\Users\\\\bob\\\\code\n");
    assert!(!ok, "Windows home-dir path should be flagged");
}

#[test]
fn linux_home_dir_path_is_flagged() {
    let (ok, out) = run_against("// path: /home/alice/code\n");
    assert!(!ok, "Linux home-dir path should be flagged");
}

#[test]
fn email_pattern_is_flagged() {
    let (ok, _out) = run_against("// contact: alice@example.com\n");
    assert!(!ok, "real email pattern should be flagged");
}

#[test]
fn noreply_email_is_allowed() {
    let (ok, out) = run_against("// 12345+madisonrickert@users.noreply.github.com\n");
    assert!(ok, "noreply.github.com emails should pass: {out}");
}
```

Update `xtask/Cargo.toml` to add the `tempfile` dev-dependency:

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Run tests, verify they fail**

Run:
```bash
cargo test --package xtask --test check_secrets 2>&1 | tail -10
```

Expected: build failure (check_secrets module does not exist yet).

- [ ] **Step 3: Implement check_secrets module**

Create `xtask/src/check_secrets.rs` with this exact content:

```rust
//! `cargo xtask check-secrets` — regex scan for forbidden patterns.
//!
//! Implements the security gate from spec §6.2. Walks the working tree (or a
//! provided root) and flags any file containing developer-machine-specific home
//! directory paths, real email addresses, or common secret prefixes. Exits non-zero
//! if any are found; intended to gate merges.
//!
//! Allowed exceptions:
//!   * `*.noreply.github.com` style emails
//!   * paths inside `target/`, `.git/`, or `node_modules/` (skipped entirely)

use std::path::{Path, PathBuf};

use clap::Parser;
use regex::Regex;
use walkdir::WalkDir;

#[derive(Parser)]
pub struct Args {
    /// Root directory to scan. Defaults to the current working directory.
    #[arg(long, default_value = ".")]
    pub root: PathBuf,

    /// Emit machine-readable JSON results instead of a human report.
    #[arg(long)]
    pub json: bool,
}

/// One offending match found during the scan.
struct Finding {
    file: PathBuf,
    line_number: usize,
    line: String,
    pattern: &'static str,
}

/// Run the secret scan and exit non-zero if any findings.
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let patterns = build_patterns();
    let findings = scan(&args.root, &patterns)?;
    report(&findings, args.json);
    if findings.is_empty() {
        Ok(())
    } else {
        Err(format!("check-secrets failed: {} finding(s)", findings.len()).into())
    }
}

/// Compiled regexes paired with human-readable labels for reporting.
struct Pattern {
    name: &'static str,
    regex: Regex,
}

fn build_patterns() -> Vec<Pattern> {
    vec![
        Pattern {
            name: "macOS home directory",
            regex: Regex::new(r"/Users/[A-Za-z0-9_.-]+/").expect("valid regex"),
        },
        Pattern {
            name: "Windows home directory",
            regex: Regex::new(r"C:\\\\Users\\\\[A-Za-z0-9_.-]+").expect("valid regex"),
        },
        Pattern {
            name: "Linux home directory",
            regex: Regex::new(r"/home/[A-Za-z0-9_.-]+/").expect("valid regex"),
        },
        Pattern {
            // Real email (not *.noreply.github.com) — naive but adequate for source review.
            name: "real email address",
            regex: Regex::new(
                r"(?P<email>[A-Za-z0-9._%+\-]+@(?!users\.noreply\.github\.com)[A-Za-z0-9.\-]+\.[A-Za-z]{2,})",
            )
            .expect("valid regex"),
        },
    ]
}

fn scan(root: &Path, patterns: &[Pattern]) -> Result<Vec<Finding>, std::io::Error> {
    let mut findings = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_excluded(e.path()))
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        // Skip binary-looking files cheaply
        if !is_text_path(path) {
            continue;
        }
        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue, // non-utf8 or unreadable: skip
        };
        for (idx, line) in contents.lines().enumerate() {
            for pattern in patterns {
                if pattern.regex.is_match(line) {
                    findings.push(Finding {
                        file: path.to_path_buf(),
                        line_number: idx + 1,
                        line: line.to_string(),
                        pattern: pattern.name,
                    });
                }
            }
        }
    }
    Ok(findings)
}

fn is_excluded(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/target/")
        || s.contains("/.git/")
        || s.contains("/node_modules/")
        || s.contains("/dist/")
        || s.contains("/release/")
}

fn is_text_path(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        // No extension: check by name. README/LICENSE etc. are text.
        return matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some("README" | "LICENSE" | "CLAUDE.md" | "AGENTS.md")
        );
    };
    matches!(
        ext,
        "rs" | "toml"
            | "md"
            | "yml"
            | "yaml"
            | "json"
            | "wgsl"
            | "glsl"
            | "sh"
            | "txt"
            | "lock"
            | "html"
            | "css"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "scss"
    )
}

fn report(findings: &[Finding], json: bool) {
    if json {
        report_json(findings);
    } else {
        report_human(findings);
    }
}

fn report_human(findings: &[Finding]) {
    if findings.is_empty() {
        println!("check-secrets: 0 findings (PASS)");
        return;
    }
    println!("check-secrets: {} finding(s) (FAIL)", findings.len());
    for f in findings {
        println!(
            "  {}:{}  [{}]  {}",
            f.file.display(),
            f.line_number,
            f.pattern,
            f.line.trim()
        );
    }
}

fn report_json(findings: &[Finding]) {
    print!("[");
    let mut first = true;
    for f in findings {
        if !first {
            print!(",");
        }
        first = false;
        print!(
            r#"{{"file":"{}","line":{},"pattern":"{}","content":{}}}"#,
            f.file.display(),
            f.line_number,
            f.pattern,
            escape_json_string(&f.line),
        );
    }
    println!("]");
}

fn escape_json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
```

- [ ] **Step 4: Run tests, verify they pass**

Run:
```bash
cargo test --package xtask 2>&1 | tail -10
```

Expected: all 7 tests pass (1 from manifest.rs, 6 from check_secrets.rs).

If a test fails, read the failure and adjust the regex or test until it passes. Do not loosen test assertions to make them pass.

- [ ] **Step 5: Verify xtask subcommands work end-to-end**

Run:
```bash
cargo xtask manifest
```

Expected output:
```
xtask subcommands:
  manifest          List all xtask subcommands with descriptions.
  check-secrets     Regex-scan the working tree for forbidden secrets and local paths.
```

Run:
```bash
cargo xtask check-secrets
```

Expected: `check-secrets: 0 findings (PASS)`. (If a file legitimately contains a flagged pattern, fix the file or extend the exclusion list — do not loosen the check.)

- [ ] **Step 6: Commit toolchain, configs, AGENTS.md, README.md, .gitignore, workspace manifest, and xtask**

Run:
```bash
git add rust-toolchain.toml rustfmt.toml clippy.toml deny.toml .gitignore \
        .cargo/config.toml \
        CLAUDE.md AGENTS.md README.md \
        Cargo.toml \
        xtask/
git commit -m "$(cat <<'EOF'
Add workspace foundation: toolchain, lints, xtask dispatcher

Pins Rust 1.85, configures rustfmt/clippy/deny, adds CLAUDE.md +
expanded AGENTS.md (five-section coding standards from spec §6.2).
xtask scaffolds the cargo dispatcher with `manifest` and
`check-secrets` subcommands plus integration tests.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 14: Scaffold `wc-core` crate

**Files:** Create `crates/wc-core/Cargo.toml`, `crates/wc-core/src/lib.rs`.

- [ ] **Step 1: Create wc-core/Cargo.toml**

Run:
```bash
mkdir -p crates/wc-core/src
```

Create `crates/wc-core/Cargo.toml` with this exact content:

```toml
[package]
name = "wc-core"
version.workspace = true
edition.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
rust-version.workspace = true
description = "WaveConductor shared infrastructure: lifecycle, audio, input, settings, math."

[lints]
workspace = true

[dependencies]
bevy = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
```

- [ ] **Step 2: Create wc-core/src/lib.rs**

Create `crates/wc-core/src/lib.rs` with this exact content:

```rust
//! # wc-core
//!
//! Shared infrastructure for WaveConductor v5: lifecycle, audio, input, settings,
//! and math helpers. Sketches consume this crate via [`CorePlugin`]; the binary
//! crate registers `CorePlugin` once at app startup.
//!
//! In Plan 1, `CorePlugin` is an empty placeholder. Subsystems are filled in by
//! Plan 2 (Core Scaffolding) and beyond.

#![warn(missing_docs)]

use bevy::prelude::*;

/// Single plugin that bundles every wc-core subsystem.
///
/// Registered once by the binary crate. As subsystems land in Plan 2 and later,
/// they are added as sub-plugins inside this `build()` method (audio, input,
/// lifecycle, settings, ui).
pub struct CorePlugin;

impl Plugin for CorePlugin {
    fn build(&self, _app: &mut App) {
        // Plan 2 fills this in. Intentionally empty in Plan 1 so the crate
        // compiles and the binary can wire it up end-to-end.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_plugin_builds_without_panicking() {
        let mut app = App::new();
        app.add_plugins(CorePlugin);
        // No assertion beyond "did not panic during plugin construction".
    }
}
```

- [ ] **Step 3: Verify it compiles and tests pass**

Run:
```bash
cargo test --package wc-core 2>&1 | tail -10
```

Expected: `test result: ok. 1 passed`.

---

## Task 15: Scaffold `wc-sketches` crate

**Files:** Create `crates/wc-sketches/Cargo.toml`, `crates/wc-sketches/src/lib.rs`, `crates/wc-sketches/tests/.gitkeep`.

- [ ] **Step 1: Create wc-sketches directory tree**

Run:
```bash
mkdir -p crates/wc-sketches/src crates/wc-sketches/tests
touch crates/wc-sketches/tests/.gitkeep
```

- [ ] **Step 2: Create wc-sketches/Cargo.toml**

Create `crates/wc-sketches/Cargo.toml` with this exact content:

```toml
[package]
name = "wc-sketches"
version.workspace = true
edition.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
rust-version.workspace = true
description = "WaveConductor sketch plugins: line, flame, dots, cymatics, waves."

[lints]
workspace = true

[dependencies]
bevy = { workspace = true }
wc-core = { workspace = true }
```

- [ ] **Step 3: Create wc-sketches/src/lib.rs**

Create `crates/wc-sketches/src/lib.rs` with this exact content:

```rust
//! # wc-sketches
//!
//! Bundle plugin that registers every sketch in WaveConductor v5. Sketches
//! themselves arrive in Plan 3 (Line) and Plan 4 (Flame, Dots, Cymatics, Waves).
//! In Plan 1 this is an empty placeholder so the workspace builds end-to-end.

#![warn(missing_docs)]

use bevy::prelude::*;

/// Single plugin that bundles every sketch.
///
/// Registered once by the binary crate. Each sketch is a sub-plugin added inside
/// `build()` as it lands.
pub struct SketchesPlugin;

impl Plugin for SketchesPlugin {
    fn build(&self, _app: &mut App) {
        // Plan 3 onward will add per-sketch sub-plugins here.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sketches_plugin_builds_without_panicking() {
        let mut app = App::new();
        app.add_plugins(SketchesPlugin);
    }
}
```

- [ ] **Step 4: Verify**

Run:
```bash
cargo test --package wc-sketches 2>&1 | tail -10
```

Expected: `test result: ok. 1 passed`.

---

## Task 16: Scaffold `waveconductor` binary crate

**Files:** Create `crates/waveconductor/Cargo.toml`, `crates/waveconductor/src/main.rs`.

- [ ] **Step 1: Create waveconductor directory tree**

Run:
```bash
mkdir -p crates/waveconductor/src
```

- [ ] **Step 2: Create waveconductor/Cargo.toml**

Create `crates/waveconductor/Cargo.toml` with this exact content:

```toml
[package]
name = "waveconductor"
version.workspace = true
edition.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
rust-version.workspace = true
description = "WaveConductor v5 — Rust + Bevy interactive art gallery."

[[bin]]
name = "waveconductor"
path = "src/main.rs"

[lints]
workspace = true

[dependencies]
bevy = { workspace = true }
wc-core = { workspace = true }
wc-sketches = { workspace = true }
tracing = { workspace = true }
```

- [ ] **Step 3: Create waveconductor/src/main.rs**

Create `crates/waveconductor/src/main.rs` with this exact content:

```rust
//! WaveConductor v5 binary entry point.
//!
//! Constructs the Bevy [`App`], registers core plugins, and runs the event loop.
//! In Plan 1 this opens an empty Bevy window to prove the workspace links and
//! runs end-to-end. Subsystem registration (audio, input, settings) lands in
//! Plan 2; sketch plugins land in Plans 3 and 4.

use bevy::prelude::*;
use wc_core::CorePlugin;
use wc_sketches::SketchesPlugin;

fn main() {
    App::new()
        .add_plugins((
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: "WaveConductor".into(),
                    resolution: (1280.0, 720.0).into(),
                    ..default()
                }),
                ..default()
            }),
            CorePlugin,
            SketchesPlugin,
        ))
        .add_systems(Startup, log_startup)
        .run();
}

/// One-shot logger that confirms the app booted. Removed once Plan 2 wires in
/// proper logging configuration.
fn log_startup() {
    tracing::info!("WaveConductor v5 starting (Plan 1 scaffold)");
}
```

- [ ] **Step 4: Verify the workspace builds**

Run:
```bash
cargo build --workspace 2>&1 | tail -15
```

Expected: a successful build of all four crates, possibly with deprecation warnings on Bevy features (acceptable).

If the build fails because Bevy features mismatch, adjust the `bevy` features in the workspace `Cargo.toml` to match the installed Bevy version's feature flags. Refer to https://docs.rs/bevy/0.18 for the feature list.

- [ ] **Step 5: Smoke test the binary**

Run:
```bash
timeout 5 cargo run -p waveconductor 2>&1 | tail -10 || true
```

Expected: a Bevy window opens, the log "WaveConductor v5 starting (Plan 1 scaffold)" is printed, and the process is killed by `timeout` after 5 seconds. The window closing on `timeout` is the success signal — there's no crash before that.

If the binary fails to run because no display is available (e.g., headless CI), this manual smoke step is skipped locally and validated only when run on a workstation.

- [ ] **Step 6: Verify rustfmt and clippy are clean**

Run:
```bash
cargo fmt --all -- --check
cargo clippy --all-targets --workspace -- -D warnings
```

Expected: both succeed silently. Fix any warnings before continuing.

- [ ] **Step 7: Verify tests across the workspace**

Run:
```bash
cargo test --workspace 2>&1 | tail -15
```

Expected: 9 tests pass (1 each from wc-core, wc-sketches; 7 from xtask).

- [ ] **Step 8: Commit the crate scaffolds**

Run:
```bash
git add crates/ assets/
git commit -m "$(cat <<'EOF'
Scaffold workspace crates: waveconductor, wc-core, wc-sketches

Three empty placeholder crates wired through the workspace. Binary
opens a Bevy window; CorePlugin and SketchesPlugin are empty plugins
that subsequent plans will fill in. Establishes the lib/bin split for
the rest of v5 development.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 17: Add assets directory

**Files:** Create `assets/.gitkeep`, `assets/shaders/.gitkeep`.

- [ ] **Step 1: Create the assets tree**

Run:
```bash
mkdir -p assets/shaders assets/fonts assets/icons
touch assets/.gitkeep assets/shaders/.gitkeep assets/fonts/.gitkeep assets/icons/.gitkeep
```

- [ ] **Step 2: Verify**

Run:
```bash
find assets -type f
```

Expected:
```
assets/.gitkeep
assets/shaders/.gitkeep
assets/fonts/.gitkeep
assets/icons/.gitkeep
```

(`.gitkeep` files keep the directories under version control.)

---

## Task 18: Create docs/adr scaffolding

**Files:** Create `docs/adr/README.md`.

- [ ] **Step 1: Create the ADR directory and README**

Run:
```bash
mkdir -p docs/adr
```

Create `docs/adr/README.md` with this exact content:

```markdown
# Architecture Decision Records

Short records of architectural decisions made during WaveConductor v5 development.

## Format

Each ADR is a numbered Markdown file: `NNNN-short-title.md`, where `NNNN` is a zero-padded sequence starting at `0001`.

Each ADR contains:

- **Status:** Proposed, Accepted, Superseded by ADR-NNNN, or Deprecated.
- **Context:** What problem are we solving? What constraints exist?
- **Decision:** What we chose.
- **Consequences:** What this means going forward, including trade-offs accepted.

ADRs are append-only. To change a decision, write a new ADR that supersedes the old one and update the old one's Status.

## Index

- (No ADRs yet — first one will land in Plan 2 when subsystem boundaries lock in.)
```

- [ ] **Step 2: Commit assets and ADR scaffolds**

Run:
```bash
git add assets/ docs/adr/
git commit -m "$(cat <<'EOF'
Add assets and docs/adr scaffolding

Empty directories versioned via .gitkeep; first ADR lands in Plan 2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 19: Author CI workflow

**Files:** Create `.github/workflows/ci.yml`.

- [ ] **Step 1: Create the workflows directory**

Run:
```bash
mkdir -p .github/workflows
```

- [ ] **Step 2: Create ci.yml**

Create `.github/workflows/ci.yml` with this exact content:

```yaml
name: CI

on:
  push:
    branches: [main, rewrite/bevy]
  pull_request:
    branches: [main, rewrite/bevy]

env:
  CARGO_TERM_COLOR: always
  RUSTFLAGS: "-D warnings"

jobs:
  fmt:
    name: rustfmt
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: 1.85.0
          components: rustfmt
      - run: cargo fmt --all -- --check

  clippy:
    name: clippy
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: 1.85.0
          components: clippy
      - uses: Swatinem/rust-cache@v2
      - name: Install Linux build deps for Bevy
        run: |
          sudo apt-get update
          sudo apt-get install -y \
            libasound2-dev libudev-dev \
            libwayland-dev libxkbcommon-dev libx11-dev libxcursor-dev libxi-dev libxrandr-dev
      - run: cargo clippy --all-targets --workspace -- -D warnings

  check-secrets:
    name: check-secrets
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: 1.85.0
      - uses: Swatinem/rust-cache@v2
      - run: cargo xtask check-secrets

  deny:
    name: cargo-deny
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: EmbarkStudios/cargo-deny-action@v2
        with:
          arguments: --workspace --all-features

  audit:
    name: cargo-audit
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: rustsec/audit-check@v2
        with:
          token: ${{ secrets.GITHUB_TOKEN }}

  test:
    name: test (${{ matrix.os }})
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: 1.85.0
      - uses: Swatinem/rust-cache@v2
      - name: Install Linux build deps for Bevy
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y \
            libasound2-dev libudev-dev \
            libwayland-dev libxkbcommon-dev libx11-dev libxcursor-dev libxi-dev libxrandr-dev
      - name: Install cargo-nextest
        uses: taiki-e/install-action@nextest
      - run: cargo nextest run --workspace --all-features

  doc:
    name: doc
    runs-on: ubuntu-latest
    env:
      RUSTDOCFLAGS: "-D warnings"
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: 1.85.0
      - uses: Swatinem/rust-cache@v2
      - name: Install Linux build deps for Bevy
        run: |
          sudo apt-get update
          sudo apt-get install -y \
            libasound2-dev libudev-dev \
            libwayland-dev libxkbcommon-dev libx11-dev libxcursor-dev libxi-dev libxrandr-dev
      - run: cargo doc --no-deps --workspace --document-private-items
```

- [ ] **Step 3: Commit the CI workflow**

Run:
```bash
git add .github/workflows/ci.yml
git commit -m "$(cat <<'EOF'
Add CI workflow: fmt, clippy, check-secrets, deny, audit, test, doc

All gates from spec §5.10 except coverage (added in Plan 3 once there's
meaningful sketch code to measure) and the release/web workflows (Plans
5 and 7). Runs on push and PR for main and rewrite/bevy.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 20: Push and verify CI green

**Files:** none.

- [ ] **Step 1: Verify local clean state**

Run:
```bash
git status
git log --oneline -10
```

Expected: clean tree; recent commits show the foundation work.

- [ ] **Step 2: Run all local gates one more time**

Run:
```bash
cargo fmt --all -- --check
cargo clippy --all-targets --workspace -- -D warnings
cargo test --workspace
cargo xtask check-secrets
cargo doc --no-deps --workspace --document-private-items
```

Expected: all five commands succeed with no warnings.

If any fails, fix the underlying issue (do not loosen the check) and amend or add a follow-up commit.

- [ ] **Step 3: Push the branch**

Run:
```bash
git push -u origin rewrite/bevy
```

Expected: branch published.

- [ ] **Step 4: Verify CI passes**

Run:
```bash
gh run watch --exit-status
```

Or open the GitHub Actions page in a browser and wait for all jobs on the latest `rewrite/bevy` push to turn green.

Expected: all CI jobs succeed (`fmt`, `clippy`, `check-secrets`, `deny`, `audit`, `test` on all three OSes, `doc`).

If a job fails:
1. Read the failing job's log.
2. Reproduce the failure locally if possible.
3. Fix the underlying issue.
4. Commit and push again.
5. Re-run.

Do not merge to `main` from `rewrite/bevy` — v4 stays on `main` until v5.0 ships.

- [ ] **Step 5: Tag the foundation milestone (optional)**

Once CI is green, tag the foundation milestone for future reference:

```bash
git tag v5-foundation
git push origin v5-foundation
```

Expected: tag pushed.

---

## Plan complete

At the end of Plan 1, `rewrite/bevy` contains:

- Cargo workspace with four crates (`waveconductor`, `wc-core`, `wc-sketches`, `xtask`)
- Pinned Rust 1.85, rustfmt + clippy configured per spec
- `cargo-deny` policy, `cargo-audit` ready
- `cargo xtask manifest` and `cargo xtask check-secrets` working with integration tests
- `CLAUDE.md` + expanded `AGENTS.md` (five-section coding standards)
- `cargo run -p waveconductor` opens an empty Bevy window
- CI green on all jobs (fmt, clippy, secret-scan, deny, audit, test on macOS + Linux + Windows, doc)
- v4 source tree removed from `rewrite/bevy`; preserved on `main`

**Next plan:** Plan 2 (Core Scaffolding) fills in `CorePlugin` with lifecycle, audio, input, settings, and the leafwing action layer. After Plan 2, the binary will navigate between empty sketch states, have working audio output, accept hand-tracking input via the mock provider, and render an egui settings panel.
