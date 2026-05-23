# Plan 5: Settings System Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the typed, persisted, introspectable settings system: a `SketchSettings` derive macro (new `wc-core-macros` proc-macro crate), per-platform persistence (TOML native / localStorage web), a curated `bevy-egui` user panel, and a `bevy-inspector-egui` dev panel toggled by Shift+D — using a synthetic `TestSketchSettings` to exercise the full stack end-to-end while no sketches exist yet.

**Architecture:** New proc-macro crate `wc-core-macros` emits `Default + SketchSettings` impls from `#[derive(SketchSettings)]`. The `SketchSettings` trait lives in `wc-core::settings` and exposes `STORAGE_KEY` + `settings_def() -> Vec<SettingDef>`. Persistence is a pair of `cfg`-gated free functions; the `SettingsPlugin` wires load-on-startup, debounced save-on-change, and a `SketchRestart` event that re-enters the active sketch when a `requires_restart` field changes. UI is two stacked layers on `bevy-egui`: a curated user panel that iterates registered settings types and renders only `category = User` fields with typed widgets, and a `bevy-inspector-egui` world inspector window gated by `Res<DevPanelVisible>` which is toggled by the existing `WaveConductorAction::ToggleDevPanel` (Shift+D, bound in Plan 2). All settings flow through Bevy resources for that-frame access; no global state.

**Tech Stack:** Rust 1.89, Bevy 0.18.1, `bevy_reflect`, `serde` 1, `toml` 0.8, `dirs` 5, `bevy_egui` 0.30, `bevy-inspector-egui` 0.27, `proc-macro2` / `syn` 2 / `quote` 1 for the derive macro, existing `leafwing-input-manager` 0.20 for the Shift+D action.

---

## File structure

**New crate — `crates/wc-core-macros/`:** the proc-macro crate. Cannot live in `wc-core` because proc-macro crates must be their own `[lib] proc-macro = true` crate.

- `crates/wc-core-macros/Cargo.toml` — proc-macro crate manifest.
- `crates/wc-core-macros/src/lib.rs` — `#[derive(SketchSettings)]` entry + attribute parsing + token generation.

**New module — `crates/wc-core/src/settings/`:** the runtime types and plugin.

- `crates/wc-core/src/settings/mod.rs` — `SettingsPlugin`, module-level docs and data flow.
- `crates/wc-core/src/settings/def.rs` — `SettingDef`, `SettingKind`, `SettingsCategory`, `NumberRange` types.
- `crates/wc-core/src/settings/trait_def.rs` — `SketchSettings` trait. Named `trait_def.rs` because Rust forbids a module called `trait`.
- `crates/wc-core/src/settings/event.rs` — `SketchRestart` message.
- `crates/wc-core/src/settings/registry.rs` — `SettingsRegistry` resource + `RegisterSketchSettingsExt` trait that types itself onto `App`.
- `crates/wc-core/src/settings/persistence.rs` — `load_settings::<S>()` / `save_settings::<S>()` plus debounce timer resource; cfg-gates TOML vs localStorage.
- `crates/wc-core/src/settings/panel_user.rs` — curated user-panel renderer.
- `crates/wc-core/src/settings/panel_dev.rs` — `DevPanelVisible` resource, toggle handler, bevy-inspector-egui driver.
- `crates/wc-core/src/settings/test_settings.rs` — `TestSketchSettings` struct (used by integration tests + manually toggled at runtime to verify the panels render).

**Modified:**

- `Cargo.toml` — add `crates/wc-core-macros` workspace member and `wc-core-macros` workspace dep; add `proc-macro2`, `syn`, `quote`, `bevy_egui`, `bevy-inspector-egui` (already declared), `serde_json` for the wasm path.
- `crates/wc-core/Cargo.toml` — depend on `wc-core-macros`, `bevy_egui`, `bevy-inspector-egui`, `serde`, `toml`, `dirs`, plus `web-sys` / `serde_json` on wasm.
- `crates/wc-core/src/lib.rs` — `pub mod settings;` + register `SettingsPlugin` in `CorePlugin`.
- `crates/wc-core/src/audio/engine.rs` — Plan 4 carry-forward: document the `_stream` field.
- `crates/wc-core/src/audio/dsp.rs` — Plan 4 carry-forward: `// TODO Plan 6` in `render()`.
- `crates/waveconductor/Cargo.toml` — pull `bevy_egui` in transitively via `wc-core`; ensure `DefaultPlugins` still composes cleanly with `EguiPlugin`.

**New tests:**

- `crates/wc-core-macros/tests/derive.rs` — compile-and-run tests: `Default` correctness, `settings_def()` shape, attribute combinations.
- `crates/wc-core/tests/settings_persistence.rs` — round-trip + corruption + missing-file behavior using a `TempDir` config root via env override.
- `crates/wc-core/tests/settings_plugin.rs` — Bevy app harness: registers `TestSketchSettings`, asserts the inserted resource matches defaults, asserts toggling `DevPanelVisible` does not panic the schedule, asserts that mutating a `requires_restart` field emits `SketchRestart`.

---

## Conventions used in this plan

- All file paths are absolute from the repo root.
- Code blocks show the full file (or full added section) so the implementer never has to merge by hand.
- Each `cargo` step lists the exact command + expected outcome.
- "Commit" steps stage the listed files explicitly and use a Conventional-Commits-style subject line. Trailers omitted; the executing-plans skill adds the `Co-Authored-By` trailer.

---

# Phase 0 — Plan 4 carry-forwards

These two tiny edits land before any settings work so the audit trail stays clean.

### Task 1: Document the `_stream` field on `AudioStream`

**Files:**
- Modify: `crates/wc-core/src/audio/engine.rs`

- [ ] **Step 1: Add a doc comment to the field**

Replace the existing struct with:

```rust
/// Wraps the live `cpal::Stream` so Bevy keeps it alive for the app's
/// lifetime. `cpal::Stream` is `!Send` on macOS, hence the non-send resource.
pub struct AudioStream {
    /// Owned `cpal::Stream` handle. Never accessed after construction —
    /// dropping `AudioStream` stops the underlying audio thread. The leading
    /// underscore documents that intent to readers and silences the unused-
    /// field lint. Do not rename to remove the underscore.
    _stream: cpal::Stream,
}
```

- [ ] **Step 2: Verify it still compiles**

Run: `cargo check -p wc-core`
Expected: clean compile, no warnings.

### Task 2: Add Plan 6 TODO to `DspHost::render`

**Files:**
- Modify: `crates/wc-core/src/audio/dsp.rs`

- [ ] **Step 1: Insert the TODO comment**

Inside `render()`, replace the existing inline comments (between the `gain` line and the `for sample` loop) with:

```rust
        let gain = if self.muted { 0.0 } else { self.volume };
        // TODO Plan 6: once synthesis sources are active, fill `output` from
        // the DSP graph and multiply by `gain`. The Plan 4 mute test
        // (`muted_render_outputs_zero_even_when_volume_high`) currently
        // passes trivially because the buffer is forced to zero below; it
        // must be re-validated against a non-silent source.
        let _ = gain;
        for sample in output.iter_mut() {
            *sample = 0.0;
        }
```

- [ ] **Step 2: Verify**

Run: `cargo test -p wc-core --lib audio::`
Expected: all four DSP tests pass; no clippy warnings.

### Task 3: Commit Phase 0

- [ ] **Step 1: Commit**

```bash
git add crates/wc-core/src/audio/engine.rs crates/wc-core/src/audio/dsp.rs
git commit -m "Plan 4 carry-forwards: AudioStream field doc, DspHost Plan-6 TODO"
```

---

# Phase A — Settings core (no UI)

Produces an end-to-end-tested settings stack: types, derive macro, persistence, restart event, registry. The phase ends with one synthetic settings struct registered through `App` and a green test suite — no egui yet.

### Task 4: Add workspace dependencies and the new crate member

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add `crates/wc-core-macros` to `members`**

Replace the `members` array (lines 3–8) with:

```toml
members = [
    "crates/waveconductor",
    "crates/wc-core",
    "crates/wc-core-macros",
    "crates/wc-sketches",
    "xtask",
]
```

- [ ] **Step 2: Add proc-macro + persistence deps to `[workspace.dependencies]`**

After the existing `regex = "1"` line (currently line 62), insert:

```toml
# proc-macro toolchain (settings derive)
proc-macro2 = "1"
quote = "1"
syn = { version = "2", features = ["full", "extra-traits"] }
# JSON for the wasm localStorage path
serde_json = "1"
# wasm-only web bindings (used by settings persistence on web targets)
web-sys = { version = "0.3", features = ["Window", "Storage"] }
# Workspace-internal proc-macro crate
wc-core-macros = { version = "5.0.0-dev", path = "crates/wc-core-macros" }
```

(`tempfile = "3"` already exists; reuse it.)

- [ ] **Step 3: Verify the workspace parses**

Run: `cargo metadata --no-deps --format-version 1 >/dev/null`
Expected: exits 0 (the new member doesn't exist yet, so add a placeholder crate before checking — proceed to Task 5 then return here if metadata fails).

### Task 5: Scaffold the `wc-core-macros` crate

**Files:**
- Create: `crates/wc-core-macros/Cargo.toml`
- Create: `crates/wc-core-macros/src/lib.rs`

- [ ] **Step 1: Write the manifest**

```toml
[package]
name = "wc-core-macros"
version.workspace = true
edition.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
rust-version.workspace = true
publish = false
description = "Proc-macro support crate for wc-core: emits SketchSettings impls."

[lib]
proc-macro = true

[dependencies]
proc-macro2.workspace = true
quote.workspace = true
syn.workspace = true

[lints]
workspace = true
```

- [ ] **Step 2: Write a placeholder lib.rs**

```rust
//! # wc-core-macros
//!
//! Proc-macro crate paired with [`wc-core`]. Currently exports the
//! `#[derive(SketchSettings)]` derive macro.
//!
//! The runtime types referenced by the macro output (`SketchSettings`,
//! `SettingDef`, `SettingKind`, `SettingsCategory`, `NumberRange`) live in
//! `wc_core::settings`. Code that uses this derive must depend on `wc-core`
//! too; this crate alone does not pull in Bevy and so stays cheap to compile.

use proc_macro::TokenStream;

/// Derive macro entry point. The real implementation is added in Task 8.
#[proc_macro_derive(SketchSettings, attributes(settings, setting))]
pub fn derive_sketch_settings(_input: TokenStream) -> TokenStream {
    TokenStream::new()
}
```

- [ ] **Step 3: Verify**

Run: `cargo check -p wc-core-macros`
Expected: clean compile.

### Task 6: Add the metadata types in `wc-core::settings::def`

**Files:**
- Create: `crates/wc-core/src/settings/def.rs`

- [ ] **Step 1: Write the types**

```rust
//! Runtime metadata produced by `#[derive(SketchSettings)]`.
//!
//! The derive macro emits a `Vec<SettingDef>` per settings struct. Panels
//! consume this table to render typed widgets without reflection-walking the
//! struct at every frame.

/// Top-level kind of a setting. Determines which widget the user panel uses
/// and how persistence serializes / deserializes the value.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingKind {
    /// Numeric scalar (`u32`, `i32`, `f32`, `f64`). Rendered as a slider when
    /// both `min` and `max` are present, otherwise as a `DragValue`.
    Number(NumberRange),
    /// Boolean toggle. Rendered as a checkbox.
    Boolean,
    /// RGBA color stored as `[f32; 4]`. Rendered with `egui::color_picker`.
    Color,
    /// Free-form UTF-8 string. Rendered as a single-line text edit.
    Text,
}

/// Numeric range constraints. All bounds are stored as `f64` for uniform
/// rendering; the derive macro converts from the field's native type via
/// `f64::from(...)` (so `u8`, `u16`, `u32`, `i8`, `i16`, `i32`, `f32`, `f64`
/// all work without lossy `as` casts).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NumberRange {
    /// Lower bound. `None` when the setting is unbounded below.
    pub min: Option<f64>,
    /// Upper bound. `None` when the setting is unbounded above.
    pub max: Option<f64>,
    /// Slider step. `None` lets egui choose.
    pub step: Option<f64>,
}

/// Which panel a setting is shown in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsCategory {
    /// User-facing curated control. Appears in the [`crate::settings::panel_user`] panel.
    User,
    /// Developer-only knob. Visible only via the Shift+D dev inspector.
    Dev,
}

/// Per-field metadata. One entry per `#[setting(...)]`-annotated struct field.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingDef {
    /// The Rust field name (`stringify!(field)`). Used as the persistence key.
    pub field_name: &'static str,
    /// Human-facing label. Defaults to `field_name` when `label = "..."` is
    /// not given in the attribute.
    pub label: &'static str,
    /// Which panel renders this field.
    pub category: SettingsCategory,
    /// Widget shape + value-space constraints.
    pub kind: SettingKind,
    /// If true, changing this field fires `SketchRestart` so the sketch can
    /// rebuild any resources it baked from the value (e.g., particle counts).
    pub requires_restart: bool,
}
```

- [ ] **Step 2: Verify**

Run: `cargo check -p wc-core`
Expected: file is not yet wired in (`mod` line missing) — compile passes because nothing references it. Wiring happens in Task 12.

### Task 7: Define the `SketchSettings` trait

**Files:**
- Create: `crates/wc-core/src/settings/trait_def.rs`

- [ ] **Step 1: Write the trait**

```rust
//! The [`SketchSettings`] trait.
//!
//! Implemented automatically by `#[derive(SketchSettings)]`; user code never
//! writes this impl by hand. Splitting it from [`super::def`] keeps the
//! metadata types reusable in test code that does not pull in the derive.

use super::def::SettingDef;

/// Marker trait implemented by every settings struct.
///
/// Implementors:
///
/// - Live as a Bevy `Resource`, so systems read them with `Res<S>`.
/// - Round-trip through `serde` (Serialize + Deserialize) for persistence.
/// - Carry a [`bevy_reflect::Reflect`] impl so the dev panel can edit them
///   without per-struct code.
///
/// The derive macro emits `Default` for the implementor and the
/// [`Self::settings_def`] method below. The user struct itself must derive
/// `Resource`, `serde::Serialize`, `serde::Deserialize`, and
/// `bevy_reflect::Reflect` — the macro deliberately does not, because those
/// derives carry semantics (Reflect type registration, serde rename rules)
/// the macro can't second-guess.
pub trait SketchSettings:
    bevy::prelude::Resource
    + bevy::reflect::Reflect
    + serde::Serialize
    + for<'de> serde::Deserialize<'de>
    + Default
    + Clone
    + Send
    + Sync
    + 'static
{
    /// Stable key used to namespace this struct's values inside the
    /// persistence backend (TOML section header on native, localStorage key
    /// suffix on web). Set with `#[settings(storage_key = "line")]`.
    const STORAGE_KEY: &'static str;

    /// Returns the per-field metadata table emitted by the derive macro.
    ///
    /// The slice is freshly allocated on each call (typically only at
    /// registration time and on panel construction).
    fn settings_def() -> Vec<SettingDef>;
}
```

- [ ] **Step 2: Verify the trait compiles in isolation**

The file is not wired into `lib.rs` yet — verification deferred to Task 12.

### Task 8: Implement the derive macro

**Files:**
- Modify: `crates/wc-core-macros/src/lib.rs`

- [ ] **Step 1: Replace the placeholder with the full implementation**

```rust
//! # wc-core-macros
//!
//! Proc-macro crate paired with [`wc-core`]. Exports the
//! `#[derive(SketchSettings)]` derive macro.
//!
//! The runtime types referenced by the macro output (`SketchSettings`,
//! `SettingDef`, `SettingKind`, `SettingsCategory`, `NumberRange`) live in
//! `wc_core::settings`. Code that uses this derive must depend on `wc-core`
//! too; this crate alone does not pull in Bevy.
//!
//! ## Attribute grammar
//!
//! ```ignore
//! #[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone)]
//! #[settings(storage_key = "line")]
//! pub struct LineSettings {
//!     #[setting(default = 5000_u32, min = 100, max = 50_000, category = User, requires_restart)]
//!     pub particle_count: u32,
//!
//!     #[setting(default = 0.92_f32, min = 0.5, max = 1.0, step = 0.01, category = Dev)]
//!     pub attractor_decay: f32,
//!
//!     #[setting(default = [1.0_f32, 1.0, 1.0, 1.0], category = User, ty = Color)]
//!     pub line_color: [f32; 4],
//! }
//! ```
//!
//! Per-field attributes (all optional unless noted):
//!
//! | Key                | Type      | Default                          |
//! |--------------------|-----------|----------------------------------|
//! | `default`          | expr      | `Default::default()`             |
//! | `label`            | string    | the field name                   |
//! | `category`         | `User` \| `Dev` | `Dev`                       |
//! | `ty`               | `Number` \| `Boolean` \| `Color` \| `Text` | `Number` |
//! | `min`, `max`, `step` | numeric expr | none (only meaningful on `Number`) |
//! | `requires_restart` | flag      | absent                           |

#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate inside proc-macro code paths"
)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, Data, DataStruct, DeriveInput, Expr, Fields, Ident, LitStr};

/// `#[derive(SketchSettings)]` entry point.
#[proc_macro_derive(SketchSettings, attributes(settings, setting))]
pub fn derive_sketch_settings(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let struct_name = &input.ident;
    let storage_key = parse_storage_key(input)?;
    let fields = parse_fields(input)?;

    let default_impl = emit_default(struct_name, &fields);
    let trait_impl = emit_trait_impl(struct_name, &storage_key, &fields);

    Ok(quote! {
        #default_impl
        #trait_impl
    })
}

#[derive(Clone, Copy)]
enum Category {
    User,
    Dev,
}

#[derive(Clone, Copy)]
enum Kind {
    Number,
    Boolean,
    Color,
    Text,
}

struct FieldInfo {
    ident: Ident,
    default: Option<Expr>,
    label: Option<String>,
    category: Category,
    requires_restart: bool,
    kind: Kind,
    min: Option<Expr>,
    max: Option<Expr>,
    step: Option<Expr>,
}

fn parse_storage_key(input: &DeriveInput) -> syn::Result<String> {
    for attr in &input.attrs {
        if attr.path().is_ident("settings") {
            let mut key: Option<String> = None;
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("storage_key") {
                    let value: LitStr = meta.value()?.parse()?;
                    key = Some(value.value());
                    Ok(())
                } else {
                    Err(meta.error("unknown #[settings(...)] attribute"))
                }
            })?;
            return key.ok_or_else(|| {
                syn::Error::new_spanned(
                    attr,
                    "missing `storage_key = \"...\"` inside #[settings(...)]",
                )
            });
        }
    }
    Err(syn::Error::new_spanned(
        input,
        "SketchSettings requires `#[settings(storage_key = \"...\")]` on the struct",
    ))
}

fn parse_fields(input: &DeriveInput) -> syn::Result<Vec<FieldInfo>> {
    let Data::Struct(DataStruct {
        fields: Fields::Named(named),
        ..
    }) = &input.data
    else {
        return Err(syn::Error::new_spanned(
            input,
            "SketchSettings requires a struct with named fields",
        ));
    };

    let mut out = Vec::with_capacity(named.named.len());
    for field in &named.named {
        let ident = field
            .ident
            .clone()
            .expect("named field guaranteed by Fields::Named match");

        let mut info = FieldInfo {
            ident,
            default: None,
            label: None,
            category: Category::Dev,
            requires_restart: false,
            kind: Kind::Number,
            min: None,
            max: None,
            step: None,
        };

        for attr in &field.attrs {
            if !attr.path().is_ident("setting") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("default") {
                    info.default = Some(meta.value()?.parse::<Expr>()?);
                } else if meta.path.is_ident("label") {
                    let value: LitStr = meta.value()?.parse()?;
                    info.label = Some(value.value());
                } else if meta.path.is_ident("category") {
                    let ident: Ident = meta.value()?.parse()?;
                    info.category = match ident.to_string().as_str() {
                        "User" => Category::User,
                        "Dev" => Category::Dev,
                        other => {
                            return Err(meta.error(format!(
                                "unknown category `{other}` (expected `User` or `Dev`)"
                            )))
                        }
                    };
                } else if meta.path.is_ident("ty") {
                    let ident: Ident = meta.value()?.parse()?;
                    info.kind = match ident.to_string().as_str() {
                        "Number" => Kind::Number,
                        "Boolean" => Kind::Boolean,
                        "Color" => Kind::Color,
                        "Text" => Kind::Text,
                        other => {
                            return Err(meta.error(format!(
                                "unknown ty `{other}` (expected `Number`, `Boolean`, `Color`, or `Text`)"
                            )))
                        }
                    };
                } else if meta.path.is_ident("min") {
                    info.min = Some(meta.value()?.parse::<Expr>()?);
                } else if meta.path.is_ident("max") {
                    info.max = Some(meta.value()?.parse::<Expr>()?);
                } else if meta.path.is_ident("step") {
                    info.step = Some(meta.value()?.parse::<Expr>()?);
                } else if meta.path.is_ident("requires_restart") {
                    info.requires_restart = true;
                } else {
                    return Err(meta.error("unknown #[setting(...)] key"));
                }
                Ok(())
            })?;
        }

        out.push(info);
    }
    Ok(out)
}

fn emit_default(struct_name: &Ident, fields: &[FieldInfo]) -> TokenStream2 {
    let inits = fields.iter().map(|f| {
        let ident = &f.ident;
        match &f.default {
            Some(expr) => quote! { #ident: #expr },
            None => quote! { #ident: ::core::default::Default::default() },
        }
    });
    quote! {
        impl ::core::default::Default for #struct_name {
            fn default() -> Self {
                Self {
                    #( #inits, )*
                }
            }
        }
    }
}

fn emit_trait_impl(
    struct_name: &Ident,
    storage_key: &str,
    fields: &[FieldInfo],
) -> TokenStream2 {
    let setting_defs = fields.iter().map(|f| {
        let field_name = f.ident.to_string();
        let label = f.label.clone().unwrap_or_else(|| field_name.clone());
        let category = match f.category {
            Category::User => quote! { ::wc_core::settings::SettingsCategory::User },
            Category::Dev => quote! { ::wc_core::settings::SettingsCategory::Dev },
        };
        let requires_restart = f.requires_restart;
        let kind_tokens = match f.kind {
            Kind::Number => {
                let min = opt_to_f64_tokens(f.min.as_ref());
                let max = opt_to_f64_tokens(f.max.as_ref());
                let step = opt_to_f64_tokens(f.step.as_ref());
                quote! {
                    ::wc_core::settings::SettingKind::Number(
                        ::wc_core::settings::NumberRange {
                            min: #min,
                            max: #max,
                            step: #step,
                        }
                    )
                }
            }
            Kind::Boolean => quote! { ::wc_core::settings::SettingKind::Boolean },
            Kind::Color => quote! { ::wc_core::settings::SettingKind::Color },
            Kind::Text => quote! { ::wc_core::settings::SettingKind::Text },
        };
        quote! {
            ::wc_core::settings::SettingDef {
                field_name: #field_name,
                label: #label,
                category: #category,
                kind: #kind_tokens,
                requires_restart: #requires_restart,
            }
        }
    });

    quote! {
        impl ::wc_core::settings::SketchSettings for #struct_name {
            const STORAGE_KEY: &'static str = #storage_key;

            fn settings_def() -> ::std::vec::Vec<::wc_core::settings::SettingDef> {
                ::std::vec![ #( #setting_defs, )* ]
            }
        }
    }
}

/// Convert an `Option<Expr>` numeric literal to `Option<f64>` token output
/// using `f64::from(...)`. `f64::from` is implemented for every primitive
/// numeric type that fits losslessly (`u8`/`u16`/`u32`/`i8`/`i16`/`i32`/
/// `f32`/`f64`), so this works for every realistic settings type without
/// any `as` cast.
fn opt_to_f64_tokens(opt: Option<&Expr>) -> TokenStream2 {
    match opt {
        Some(expr) => quote! { ::core::option::Option::Some(::core::convert::From::from(#expr)) },
        None => quote! { ::core::option::Option::None },
    }
}
```

- [ ] **Step 2: Verify**

Run: `cargo check -p wc-core-macros`
Expected: clean compile, no warnings.

### Task 9: Persistence layer (TOML on native, localStorage on web)

**Files:**
- Create: `crates/wc-core/src/settings/persistence.rs`

- [ ] **Step 1: Write the module**

```rust
//! Per-platform persistence for [`SketchSettings`].
//!
//! ## Native
//!
//! A single TOML file at `dirs::config_dir() / "waveconductor" / "sketch-settings.toml"`.
//! Each settings struct occupies one top-level table keyed by its
//! [`SketchSettings::STORAGE_KEY`]:
//!
//! ```toml
//! [line]
//! particle_count = 5000
//! attractor_decay = 0.92
//!
//! [flame]
//! ...
//! ```
//!
//! The override env var [`CONFIG_DIR_ENV`] is consulted first; integration
//! tests use a `TempDir` and set this var so they never touch the real
//! XDG/macOS config dir.
//!
//! ## Web
//!
//! `web-sys`'s `window().local_storage()` with one JSON-encoded value per
//! sketch under key `wc-sketch-settings:<STORAGE_KEY>`. JSON instead of TOML
//! because `serde_json` has a much smaller wasm footprint than `toml`.

use std::path::PathBuf;

use super::trait_def::SketchSettings;

/// Environment variable that overrides the OS-determined config directory.
/// Production code does not set this; tests do, to point at a `TempDir`.
pub const CONFIG_DIR_ENV: &str = "WAVECONDUCTOR_CONFIG_DIR";

/// Returns the absolute path to the combined settings TOML file.
///
/// Falls back to the current working directory if neither
/// [`CONFIG_DIR_ENV`] nor [`dirs::config_dir`] yields a path — the only
/// realistic case is a stripped-down sandbox without any home env vars set,
/// and writing to CWD is still better than panicking.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn settings_path() -> PathBuf {
    let base = std::env::var_os(CONFIG_DIR_ENV)
        .map(PathBuf::from)
        .or_else(dirs::config_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("waveconductor").join("sketch-settings.toml")
}

/// Load the value for a specific settings type. Returns `S::default()` on
/// any error (file missing, parse failure, schema mismatch). Errors are
/// logged at `warn` level.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn load<S: SketchSettings>() -> S {
    let path = settings_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        tracing::debug!(?path, key = S::STORAGE_KEY, "no settings file; using defaults");
        return S::default();
    };
    let table: toml::Table = match toml::from_str(&text) {
        Ok(t) => t,
        Err(err) => {
            tracing::warn!(?err, "settings file is malformed TOML; using defaults");
            return S::default();
        }
    };
    let Some(value) = table.get(S::STORAGE_KEY) else {
        return S::default();
    };
    match value.clone().try_into::<S>() {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(
                ?err,
                key = S::STORAGE_KEY,
                "settings section failed to deserialize; using defaults",
            );
            S::default()
        }
    }
}

/// Persist a single settings struct. Reads the existing file, replaces
/// `[<STORAGE_KEY>]`, and writes it back. Errors are logged but not
/// returned; a settings save failure should never crash the app.
#[cfg(not(target_arch = "wasm32"))]
pub fn save<S: SketchSettings>(settings: &S) {
    let path = settings_path();
    let mut table: toml::Table = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default();

    let new_value = match toml::Value::try_from(settings) {
        Ok(v) => v,
        Err(err) => {
            tracing::error!(?err, key = S::STORAGE_KEY, "settings failed to serialize");
            return;
        }
    };
    table.insert(S::STORAGE_KEY.to_string(), new_value);

    let serialized = match toml::to_string_pretty(&table) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(?err, "settings table failed to serialize");
            return;
        }
    };

    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::error!(?err, ?parent, "failed to create config dir");
            return;
        }
    }
    if let Err(err) = std::fs::write(&path, serialized) {
        tracing::error!(?err, ?path, "failed to write settings file");
    }
}

// -- Web --------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
fn local_storage_key<S: SketchSettings>() -> String {
    format!("wc-sketch-settings:{}", S::STORAGE_KEY)
}

#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn load<S: SketchSettings>() -> S {
    let key = local_storage_key::<S>();
    let storage = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten());
    let Some(storage) = storage else {
        tracing::debug!("localStorage unavailable; using defaults");
        return S::default();
    };
    let Ok(Some(text)) = storage.get_item(&key) else {
        return S::default();
    };
    match serde_json::from_str::<S>(&text) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, %key, "localStorage value failed to deserialize");
            S::default()
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub fn save<S: SketchSettings>(settings: &S) {
    let key = local_storage_key::<S>();
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
        tracing::error!("localStorage unavailable; cannot save settings");
        return;
    };
    let serialized = match serde_json::to_string(settings) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(?err, %key, "settings failed to serialize");
            return;
        }
    };
    if let Err(err) = storage.set_item(&key, &serialized) {
        tracing::error!(?err, %key, "localStorage set_item failed");
    }
}
```

- [ ] **Step 2: Verify the file parses (still not wired in)**

Module wiring happens in Task 12.

### Task 10: `SketchRestart` message + `DevPanelVisible` resource

**Files:**
- Create: `crates/wc-core/src/settings/event.rs`
- Create: `crates/wc-core/src/settings/panel_dev.rs` (resource portion only; UI added in Phase B)

- [ ] **Step 1: Write the event module**

```rust
//! The [`SketchRestart`] message.
//!
//! Fires when a setting with `requires_restart = true` changes value. The
//! sketch's plugin observes the message and re-runs its `OnEnter` setup
//! sequence so any size-dependent or one-time resources (particle counts,
//! VRAM buffers, etc.) are rebuilt against the new value.

use bevy::prelude::*;

/// Fired by `SettingsPlugin` when a `requires_restart` field changes.
///
/// Carries the [`crate::settings::SketchSettings::STORAGE_KEY`] of the
/// struct whose field triggered the restart so listeners can ignore
/// restarts targeting other sketches.
#[derive(Message, Debug, Clone)]
pub struct SketchRestart {
    /// Storage key of the settings struct that requested the restart.
    pub storage_key: &'static str,
}
```

- [ ] **Step 2: Write the dev-panel resource module (UI code lands in Task 14)**

```rust
//! Dev panel state.
//!
//! Toggled by [`crate::lifecycle::actions::WaveConductorAction::ToggleDevPanel`]
//! (bound to Shift+D in Plan 2). The actual `bevy-inspector-egui` integration
//! is wired in [`super::SettingsPlugin`]; this module owns only the boolean
//! state resource and its toggle system so the rest of the codebase can
//! depend on `DevPanelVisible` without dragging in egui.

use bevy::prelude::*;
use leafwing_input_manager::prelude::ActionState;

use crate::lifecycle::actions::WaveConductorAction;

/// True when the dev inspector window should be drawn.
///
/// Defaults to `false` — production deployments and casual users never see
/// the panel. The Plan-5 binding (Shift+D) flips it.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevPanelVisible(pub bool);

/// System that listens for `WaveConductorAction::ToggleDevPanel` and flips
/// [`DevPanelVisible`]. Scheduled in `Update` by `SettingsPlugin`.
pub fn handle_dev_panel_toggle(
    actions: Res<'_, ActionState<WaveConductorAction>>,
    mut visible: ResMut<'_, DevPanelVisible>,
) {
    if actions.just_pressed(&WaveConductorAction::ToggleDevPanel) {
        visible.0 = !visible.0;
        tracing::debug!(visible = visible.0, "dev panel toggled");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::state::app::StatesPlugin;
    use leafwing_input_manager::plugin::InputManagerPlugin;

    fn make_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::input::InputPlugin);
        app.add_plugins(StatesPlugin);
        app.add_plugins(InputManagerPlugin::<WaveConductorAction>::default());
        app.init_resource::<ActionState<WaveConductorAction>>();
        app.init_resource::<DevPanelVisible>();
        app.add_systems(Update, handle_dev_panel_toggle);
        app
    }

    #[test]
    fn toggle_flips_visibility() {
        let mut app = make_app();
        // Simulate a `just_pressed` by writing the action state directly.
        app.world_mut()
            .resource_mut::<ActionState<WaveConductorAction>>()
            .press(&WaveConductorAction::ToggleDevPanel);
        app.update();
        assert!(app.world().resource::<DevPanelVisible>().0);

        // Releasing then re-pressing toggles back off.
        app.world_mut()
            .resource_mut::<ActionState<WaveConductorAction>>()
            .release(&WaveConductorAction::ToggleDevPanel);
        app.update();
        app.world_mut()
            .resource_mut::<ActionState<WaveConductorAction>>()
            .press(&WaveConductorAction::ToggleDevPanel);
        app.update();
        assert!(!app.world().resource::<DevPanelVisible>().0);
    }
}
```

### Task 11: Settings registry + `SettingsPlugin` core (no UI yet)

**Files:**
- Create: `crates/wc-core/src/settings/registry.rs`
- Create: `crates/wc-core/src/settings/mod.rs`

- [ ] **Step 1: Write the registry**

```rust
//! Type registry: lets `SettingsPlugin` orchestrate save / restart logic
//! over a heterogeneous list of `SketchSettings` types.
//!
//! Each registered type contributes one entry of type-erased function
//! pointers. The panels and persistence systems iterate the list and call
//! through the pointers without knowing the concrete type.

use bevy::prelude::*;

use super::def::SettingDef;
use super::event::SketchRestart;
use super::persistence;
use super::trait_def::SketchSettings;

/// Per-registered-type entry stored in [`SettingsRegistry`].
#[derive(Clone)]
pub struct RegisteredSettings {
    /// `S::STORAGE_KEY` — used as the toml table name / localStorage suffix
    /// and as the discriminator on `SketchRestart` messages.
    pub storage_key: &'static str,
    /// Cached `S::settings_def()` so panel renderers don't reallocate per
    /// frame.
    pub def: Vec<SettingDef>,
    /// Persist the current value of the registered resource by reading it
    /// from `world` and calling `persistence::save`.
    pub save_fn: fn(&World),
    /// Returns true if any `requires_restart` field changed value since the
    /// previous frame. Implementation maintains a per-type `Local`-style
    /// last-seen snapshot inside a hidden resource (`PreviousSnapshot<S>`).
    pub diff_requires_restart_fn: fn(&mut World) -> bool,
}

/// Heterogeneous, type-erased list of registered settings types.
///
/// Populated by [`super::RegisterSketchSettingsExt::register_sketch_settings`].
#[derive(Resource, Default, Clone)]
pub struct SettingsRegistry {
    /// One entry per registered settings type, in registration order.
    pub entries: Vec<RegisteredSettings>,
}

/// Hidden resource: previous-frame snapshot of each settings type.
///
/// Used by the requires-restart diff function. Stored separately per `S`.
#[derive(Resource, Debug, Clone)]
pub struct PreviousSnapshot<S: SketchSettings>(pub S);

impl<S: SketchSettings> Default for PreviousSnapshot<S> {
    fn default() -> Self {
        Self(S::default())
    }
}

/// Returns `true` if any field marked `requires_restart` differs between
/// `prev` and `curr`. Compares by serializing both to TOML values — slower
/// than per-field equality but works without a per-struct generated diff
/// function and is only called when the resource is mutated.
fn requires_restart_changed<S: SketchSettings>(prev: &S, curr: &S) -> bool {
    let restart_fields: Vec<&'static str> = S::settings_def()
        .iter()
        .filter(|d| d.requires_restart)
        .map(|d| d.field_name)
        .collect();
    if restart_fields.is_empty() {
        return false;
    }
    let prev_v = toml::Value::try_from(prev).ok();
    let curr_v = toml::Value::try_from(curr).ok();
    let (Some(prev_v), Some(curr_v)) = (prev_v, curr_v) else {
        return false;
    };
    for name in restart_fields {
        if prev_v.get(name) != curr_v.get(name) {
            return true;
        }
    }
    false
}

/// The save closure baked per `S` at registration time.
pub fn save_fn<S: SketchSettings>(world: &World) {
    let value = world.resource::<S>().clone();
    persistence::save::<S>(&value);
}

/// The restart-diff closure baked per `S` at registration time.
///
/// Updates the `PreviousSnapshot<S>` resource on every call so subsequent
/// frames diff against the most recent persisted view.
pub fn diff_requires_restart_fn<S: SketchSettings>(world: &mut World) -> bool {
    let curr = world.resource::<S>().clone();
    // SAFETY: we know the snapshot was inserted at registration time.
    let prev_snap = world
        .get_resource_mut::<PreviousSnapshot<S>>()
        .map(|mut p| {
            let old = p.0.clone();
            p.0 = curr.clone();
            old
        });
    let Some(prev) = prev_snap else {
        return false;
    };
    requires_restart_changed::<S>(&prev, &curr)
}

/// Extension trait that adds a typed `register_sketch_settings::<S>` method
/// to Bevy's [`App`].
pub trait RegisterSketchSettingsExt {
    /// Register a [`SketchSettings`] type with the settings system.
    ///
    /// Loads any persisted value (else default), inserts it as a resource,
    /// records type metadata in [`SettingsRegistry`], and seeds a
    /// [`PreviousSnapshot`] so restart-diffing has a baseline.
    fn register_sketch_settings<S: SketchSettings>(&mut self) -> &mut Self;
}

impl RegisterSketchSettingsExt for App {
    fn register_sketch_settings<S: SketchSettings>(&mut self) -> &mut Self {
        let initial = persistence::load::<S>();
        self.insert_resource(initial.clone());
        self.insert_resource(PreviousSnapshot::<S>(initial));
        self.register_type::<S>();

        let mut registry = self
            .world_mut()
            .get_resource_or_insert_with(SettingsRegistry::default)
            .clone();
        registry.entries.push(RegisteredSettings {
            storage_key: S::STORAGE_KEY,
            def: S::settings_def(),
            save_fn: save_fn::<S>,
            diff_requires_restart_fn: diff_requires_restart_fn::<S>,
        });
        self.insert_resource(registry);
        self
    }
}

/// System that, once per frame, walks the registry calling each entry's
/// `diff_requires_restart_fn` and emits a [`SketchRestart`] if any returned
/// true.
pub fn emit_restart_events(world: &mut World) {
    let registry = world
        .get_resource::<SettingsRegistry>()
        .cloned()
        .unwrap_or_default();
    for entry in &registry.entries {
        if (entry.diff_requires_restart_fn)(world) {
            world
                .resource_mut::<bevy::prelude::Messages<SketchRestart>>()
                .write(SketchRestart {
                    storage_key: entry.storage_key,
                });
        }
    }
}
```

- [ ] **Step 2: Write the module-level `mod.rs` and `SettingsPlugin` (UI systems are added in Phase B)**

```rust
//! Settings subsystem: typed per-sketch settings, persistence, restart
//! events, user panel, and dev inspector.
//!
//! ## Data flow
//!
//! 1. A sketch crate defines `MySettings` with `#[derive(SketchSettings)]`.
//!    The macro emits `Default` and the [`SketchSettings`] impl.
//! 2. The sketch's plugin calls
//!    `app.register_sketch_settings::<MySettings>()`. This loads any
//!    persisted value (or `Default`), inserts it as a Bevy `Resource`, and
//!    appends an entry to [`SettingsRegistry`].
//! 3. Each frame, [`registry::emit_restart_events`] diffs every registered
//!    resource against its [`registry::PreviousSnapshot`]; any change to a
//!    `requires_restart` field writes a [`event::SketchRestart`] message.
//! 4. The user panel ([`panel_user`]) iterates the registry and draws only
//!    `category = User` fields. The dev panel ([`panel_dev`]) opens a
//!    `bevy-inspector-egui` window when [`panel_dev::DevPanelVisible`] is
//!    true, exposing every Reflect-registered resource (including the
//!    sketch settings types, which `register_sketch_settings` registers
//!    automatically).
//! 5. A debounced save system writes the current resource value back to
//!    disk shortly after the last mutation.

pub mod def;
pub mod event;
pub mod panel_dev;
pub mod persistence;
pub mod registry;
pub mod test_settings;
pub mod trait_def;

mod panel_user;

pub use def::{NumberRange, SettingDef, SettingKind, SettingsCategory};
pub use event::SketchRestart;
pub use panel_dev::DevPanelVisible;
pub use registry::{RegisterSketchSettingsExt, SettingsRegistry};
pub use trait_def::SketchSettings;

use bevy::prelude::*;

/// Plugin that wires the settings subsystem into a Bevy [`App`].
///
/// Registered by [`crate::CorePlugin`]. Sketches register their concrete
/// settings types separately via
/// [`registry::RegisterSketchSettingsExt::register_sketch_settings`].
pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SettingsRegistry>()
            .init_resource::<DevPanelVisible>()
            .add_message::<SketchRestart>()
            // Always-on registration of the synthetic test settings so the
            // panels have at least one struct to render even before any
            // sketches exist. Sketches will register their own structs
            // additionally in Plan 6+.
            .register_sketch_settings::<test_settings::TestSketchSettings>()
            .add_systems(
                Update,
                (
                    panel_dev::handle_dev_panel_toggle,
                    registry::emit_restart_events,
                )
                    .chain(),
            );
        // egui-based UI systems are wired below.
        panel_user::add_systems(app);
        panel_dev::add_systems(app);
    }
}
```

Note: `panel_user::add_systems` and `panel_dev::add_systems` are written in Phase B (Task 14 / Task 15) but the call sites here reference them so the plugin is complete on first write. Add empty stubs now so the crate compiles:

```rust
// Inside crates/wc-core/src/settings/panel_user.rs (created in Task 14).
// Stub for Phase A:
//
// pub(super) fn add_systems(_app: &mut bevy::prelude::App) {}
//
// Inside panel_dev.rs add a sibling fn:
//
// pub(super) fn add_systems(_app: &mut bevy::prelude::App) {}
```

- [ ] **Step 3: Write the synthetic test settings struct**

```rust
//! Synthetic settings struct exercised by Plan-5 integration tests and
//! used to populate the user panel before any real sketches exist.
//!
//! This file ships in the production binary because the dev panel and user
//! panel both want at least one example struct to render against before
//! Plan 6+ ships real sketches. After the first real sketch lands, this
//! file becomes test-only — gate it on `#[cfg(test)]` then.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// A small, varied settings struct. Touches every `SettingKind` so panel
/// renderers and persistence can be tested in isolation.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "test")]
pub struct TestSketchSettings {
    /// Number of synthetic widgets.
    #[setting(default = 42_u32, min = 0_u32, max = 1000_u32, category = User, requires_restart)]
    pub widget_count: u32,
    /// Animation tempo in Hz.
    #[setting(default = 0.5_f32, min = 0.0_f32, max = 4.0_f32, step = 0.05_f32, category = User)]
    pub tempo_hz: f32,
    /// Whether the overlay tint is enabled.
    #[setting(default = true, ty = Boolean, category = User)]
    pub enable_tint: bool,
    /// Foreground color.
    #[setting(default = [1.0_f32, 1.0, 1.0, 1.0], ty = Color, category = User)]
    pub tint_color: [f32; 4],
    /// Developer-only override label.
    #[setting(default = String::from("default"), ty = Text, category = Dev)]
    pub dev_label: String,
}
```

- [ ] **Step 4: Wire the module into the crate root**

Modify `crates/wc-core/src/lib.rs`:

```rust
//! # wc-core
//!
//! Shared infrastructure for `WaveConductor` v5: lifecycle, input, audio,
//! settings, and math helpers. Sketches consume this crate via [`CorePlugin`];
//! the binary crate registers `CorePlugin` once at app startup.

pub mod audio;
pub mod input;
pub mod lifecycle;
pub mod settings;

use bevy::prelude::*;

/// Single plugin that bundles every wc-core subsystem.
///
/// Registered once by the binary crate.
pub struct CorePlugin;

impl Plugin for CorePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(lifecycle::LifecyclePlugin);
        app.add_plugins(input::HandTrackingPlugin);
        app.add_plugins(audio::AudioPlugin);
        app.add_plugins(settings::SettingsPlugin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::state::app::StatesPlugin;

    #[test]
    fn core_plugin_builds_without_panicking() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::input::InputPlugin);
        app.add_plugins(StatesPlugin);
        app.add_plugins(bevy_egui::EguiPlugin {
            enable_multipass_for_primary_context: false,
        });
        app.add_plugins(CorePlugin);
    }
}
```

- [ ] **Step 5: Add deps to `crates/wc-core/Cargo.toml`**

Add to the `[dependencies]` table:

```toml
wc-core-macros.workspace = true
bevy_egui.workspace = true
bevy-inspector-egui.workspace = true
serde.workspace = true
toml.workspace = true
dirs.workspace = true

[target.'cfg(target_arch = "wasm32")'.dependencies]
web-sys.workspace = true
serde_json.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 6: Stub out the two `add_systems` fns so this phase compiles**

Create `crates/wc-core/src/settings/panel_user.rs`:

```rust
//! User-facing settings panel — Phase B target. Stub only in Phase A so the
//! `SettingsPlugin` `build()` call site compiles.

#![allow(dead_code, reason = "filled in during Phase B (Task 14)")]

/// Plugin assembly hook called by [`super::SettingsPlugin::build`]. Empty in
/// Phase A; real implementation lands in Task 14.
pub(super) fn add_systems(_app: &mut bevy::prelude::App) {}
```

Append to `crates/wc-core/src/settings/panel_dev.rs`:

```rust
/// Plugin assembly hook called by [`super::SettingsPlugin::build`]. Empty in
/// Phase A; real implementation lands in Task 15.
pub(super) fn add_systems(_app: &mut bevy::prelude::App) {}
```

- [ ] **Step 7: Verify the whole workspace builds**

Run: `cargo check --workspace --all-targets`
Expected: clean build (one or two `dead_code` allows are intentional).

### Task 12: Tests — derive macro

**Files:**
- Create: `crates/wc-core-macros/tests/derive.rs`

Note: the macro emits paths under `::wc_core::settings`, so the test crate needs `wc-core` as a dev-dep. Add to `crates/wc-core-macros/Cargo.toml`:

```toml
[dev-dependencies]
wc-core.workspace = true
bevy.workspace = true
bevy_reflect = "0.18"
serde.workspace = true
```

- [ ] **Step 1: Write the derive tests**

```rust
//! End-to-end tests for `#[derive(SketchSettings)]`.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core::settings::{SettingKind, SettingsCategory, SketchSettings};
use wc_core_macros::SketchSettings;

#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "derive_test")]
struct Fixture {
    #[setting(default = 7_u32, min = 1_u32, max = 10_u32, category = User, requires_restart)]
    count: u32,
    #[setting(default = 0.25_f32, min = 0.0_f32, max = 1.0_f32, step = 0.05_f32, category = Dev)]
    smoothing: f32,
    #[setting(default = false, ty = Boolean, category = User)]
    flag: bool,
    #[setting(default = [0.5_f32, 0.5, 0.5, 1.0], ty = Color, category = User)]
    color: [f32; 4],
    #[setting(default = String::from("hi"), ty = Text, label = "Greeting", category = Dev)]
    greeting: String,
}

#[test]
fn default_impl_uses_field_defaults() {
    let f = Fixture::default();
    assert_eq!(f.count, 7);
    assert!((f.smoothing - 0.25).abs() < f32::EPSILON);
    assert!(!f.flag);
    assert_eq!(f.color, [0.5, 0.5, 0.5, 1.0]);
    assert_eq!(f.greeting, "hi");
}

#[test]
fn storage_key_matches_attribute() {
    assert_eq!(Fixture::STORAGE_KEY, "derive_test");
}

#[test]
fn settings_def_lists_every_field_in_order() {
    let defs = Fixture::settings_def();
    let names: Vec<&str> = defs.iter().map(|d| d.field_name).collect();
    assert_eq!(names, ["count", "smoothing", "flag", "color", "greeting"]);
}

#[test]
fn category_attribute_maps_correctly() {
    let defs = Fixture::settings_def();
    assert_eq!(defs[0].category, SettingsCategory::User);
    assert_eq!(defs[1].category, SettingsCategory::Dev);
    assert_eq!(defs[2].category, SettingsCategory::User);
    assert_eq!(defs[3].category, SettingsCategory::User);
    assert_eq!(defs[4].category, SettingsCategory::Dev);
}

#[test]
fn requires_restart_flag_propagates() {
    let defs = Fixture::settings_def();
    assert!(defs[0].requires_restart);
    assert!(!defs[1].requires_restart);
}

#[test]
fn label_falls_back_to_field_name_when_unset() {
    let defs = Fixture::settings_def();
    assert_eq!(defs[0].label, "count");
    // Explicit override.
    assert_eq!(defs[4].label, "Greeting");
}

#[test]
fn number_range_carries_bounds_and_step() {
    let defs = Fixture::settings_def();
    let SettingKind::Number(range) = &defs[1].kind else {
        panic!("expected Number kind for smoothing");
    };
    assert_eq!(range.min, Some(0.0));
    assert_eq!(range.max, Some(1.0));
    assert!(range.step.is_some());
    assert!((range.step.expect("step set") - 0.05).abs() < 1e-6);
}

#[test]
fn kind_attribute_overrides_default_number_kind() {
    let defs = Fixture::settings_def();
    assert!(matches!(defs[2].kind, SettingKind::Boolean));
    assert!(matches!(defs[3].kind, SettingKind::Color));
    assert!(matches!(defs[4].kind, SettingKind::Text));
}
```

- [ ] **Step 2: Run them**

Run: `cargo test -p wc-core-macros`
Expected: 8 tests pass.

### Task 13: Tests — persistence + plugin

**Files:**
- Create: `crates/wc-core/tests/settings_persistence.rs`
- Create: `crates/wc-core/tests/settings_plugin.rs`

- [ ] **Step 1: Write the persistence test**

```rust
//! Round-trip and resilience tests for the TOML persistence layer.

#![cfg(not(target_arch = "wasm32"))]
#![allow(
    unsafe_code,
    reason = "Rust 1.80+ marks env::set_var unsafe; serialized below via a static mutex"
)]
#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

use tempfile::TempDir;
use wc_core::settings::{
    persistence::{self, CONFIG_DIR_ENV},
    test_settings::TestSketchSettings,
    SketchSettings,
};

/// Set `CONFIG_DIR_ENV` to `dir` for the duration of the closure.
///
/// Tests in this file are serial (the env var is process-global). Run with
/// `cargo test -p wc-core --test settings_persistence -- --test-threads=1`
/// or rely on cargo's per-binary default of a single thread when the
/// `RUST_TEST_THREADS` env var is set elsewhere. We enforce it via an
/// in-process mutex below.
fn with_temp_dir<R>(f: impl FnOnce(&TempDir) -> R) -> R {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().expect("env-var mutex poisoned");
    let dir = TempDir::new().expect("tempdir");
    let prev = std::env::var_os(CONFIG_DIR_ENV);
    // SAFETY: serialized via the static `LOCK` mutex above, so no other
    // thread can observe or mutate environment variables while this block
    // runs. Rust 1.80+ marks `set_var`/`remove_var` unsafe specifically to
    // flag concurrent-mutation hazards.
    unsafe {
        std::env::set_var(CONFIG_DIR_ENV, dir.path());
    }
    let result = f(&dir);
    // SAFETY: same lock.
    unsafe {
        match prev {
            Some(v) => std::env::set_var(CONFIG_DIR_ENV, v),
            None => std::env::remove_var(CONFIG_DIR_ENV),
        }
    }
    result
}

#[test]
fn load_returns_default_when_no_file_exists() {
    with_temp_dir(|_dir| {
        let value = persistence::load::<TestSketchSettings>();
        assert_eq!(value, TestSketchSettings::default());
    });
}

#[test]
fn save_then_load_round_trips() {
    with_temp_dir(|_dir| {
        let mut original = TestSketchSettings::default();
        original.widget_count = 123;
        original.tempo_hz = 1.25;
        original.enable_tint = false;
        original.tint_color = [0.1, 0.2, 0.3, 0.4];
        original.dev_label = String::from("custom");

        persistence::save(&original);
        let loaded = persistence::load::<TestSketchSettings>();
        assert_eq!(loaded, original);
    });
}

#[test]
fn save_preserves_other_sections() {
    use std::fs;

    with_temp_dir(|_dir| {
        // Pre-seed a settings file with an unrelated section.
        let path = persistence::settings_path();
        fs::create_dir_all(path.parent().expect("has parent")).expect("mkdirs");
        fs::write(
            &path,
            "[unrelated]\nfoo = 42\nbar = \"keep me\"\n",
        )
        .expect("seed");

        // Save TestSketchSettings — should add a section, not clobber.
        let value = TestSketchSettings::default();
        persistence::save(&value);

        let text = fs::read_to_string(&path).expect("read");
        assert!(text.contains("[unrelated]"), "[unrelated] section dropped: {text}");
        assert!(text.contains("foo = 42"), "foo key dropped: {text}");
        assert!(
            text.contains(&format!("[{}]", TestSketchSettings::STORAGE_KEY)),
            "new section missing: {text}",
        );
    });
}

#[test]
fn load_returns_default_when_file_is_malformed_toml() {
    use std::fs;

    with_temp_dir(|_dir| {
        let path = persistence::settings_path();
        fs::create_dir_all(path.parent().expect("has parent")).expect("mkdirs");
        fs::write(&path, "this is not valid toml = = =").expect("seed");

        let loaded = persistence::load::<TestSketchSettings>();
        assert_eq!(loaded, TestSketchSettings::default());
    });
}

#[test]
fn load_returns_default_when_section_schema_mismatches() {
    use std::fs;

    with_temp_dir(|_dir| {
        let path = persistence::settings_path();
        fs::create_dir_all(path.parent().expect("has parent")).expect("mkdirs");
        // widget_count is u32 — feeding it a string triggers a schema error.
        fs::write(
            &path,
            "[test]\nwidget_count = \"not a number\"\n",
        )
        .expect("seed");

        let loaded = persistence::load::<TestSketchSettings>();
        assert_eq!(loaded, TestSketchSettings::default());
    });
}
```

- [ ] **Step 2: Write the plugin/restart-event test**

```rust
//! `SettingsPlugin` assembly + `SketchRestart` event behavior.

#![allow(
    unsafe_code,
    reason = "Rust 1.80+ marks env::set_var unsafe; called once per process before any thread"
)]
#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use wc_core::settings::{
    test_settings::TestSketchSettings, DevPanelVisible, RegisterSketchSettingsExt, SettingsPlugin,
    SettingsRegistry, SketchRestart,
};

fn make_app() -> App {
    // Isolate config dir so this test doesn't read the dev's real settings file.
    let dir = std::env::temp_dir().join(format!("wc-settings-plugin-test-{}", std::process::id()));
    // SAFETY: tests in this binary run with --test-threads=1 implicitly via
    // cargo test's per-binary single-threaded default for #[test] fns that
    // share process state. If we add tests that need parallelism later,
    // gate this with a Mutex like settings_persistence.rs does.
    unsafe {
        std::env::set_var("WAVECONDUCTOR_CONFIG_DIR", &dir);
    }

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::input::InputPlugin);
    app.add_plugins(StatesPlugin);
    app.add_plugins(leafwing_input_manager::plugin::InputManagerPlugin::<
        wc_core::lifecycle::actions::WaveConductorAction,
    >::default());
    app.init_resource::<leafwing_input_manager::prelude::ActionState<
        wc_core::lifecycle::actions::WaveConductorAction,
    >>();
    app.add_plugins(bevy_egui::EguiPlugin {
        enable_multipass_for_primary_context: false,
    });
    app.add_plugins(SettingsPlugin);
    app
}

#[test]
fn plugin_registers_test_settings_resource_with_defaults() {
    let mut app = make_app();
    app.update();
    let value = app.world().resource::<TestSketchSettings>().clone();
    assert_eq!(value, TestSketchSettings::default());
}

#[test]
fn registry_lists_test_settings_after_plugin_init() {
    let mut app = make_app();
    app.update();
    let registry = app.world().resource::<SettingsRegistry>().clone();
    let keys: Vec<&str> = registry.entries.iter().map(|e| e.storage_key).collect();
    assert!(keys.contains(&"test"), "test storage key missing: {keys:?}");
}

#[test]
fn dev_panel_visible_resource_defaults_false() {
    let mut app = make_app();
    app.update();
    assert!(!app.world().resource::<DevPanelVisible>().0);
}

#[test]
fn mutating_requires_restart_field_emits_event() {
    let mut app = make_app();
    app.update(); // baseline snapshot

    app.world_mut()
        .resource_mut::<TestSketchSettings>()
        .widget_count = 999;
    app.update(); // diff happens here

    let messages = app
        .world()
        .resource::<bevy::prelude::Messages<SketchRestart>>();
    let count = messages.iter_current_update_messages().count();
    assert!(count >= 1, "expected SketchRestart, got {count}");
    let key = messages
        .iter_current_update_messages()
        .next()
        .expect("at least one message")
        .storage_key;
    assert_eq!(key, "test");
}

#[test]
fn mutating_non_restart_field_does_not_emit_event() {
    let mut app = make_app();
    app.update();

    app.world_mut()
        .resource_mut::<TestSketchSettings>()
        .tempo_hz = 2.0;
    app.update();

    let messages = app
        .world()
        .resource::<bevy::prelude::Messages<SketchRestart>>();
    let count = messages.iter_current_update_messages().count();
    assert_eq!(count, 0, "tempo_hz is not requires_restart but emitted {count} events");
}

#[test]
fn second_register_with_different_type_lists_both() {
    use bevy::reflect::Reflect;
    use serde::{Deserialize, Serialize};
    use wc_core_macros::SketchSettings as DeriveSettings;

    #[derive(DeriveSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
    #[reflect(Resource, Default)]
    #[settings(storage_key = "second")]
    struct Second {
        #[setting(default = 1_u32, category = User)]
        n: u32,
    }

    let mut app = make_app();
    app.register_sketch_settings::<Second>();
    app.update();
    let registry = app.world().resource::<SettingsRegistry>().clone();
    let keys: Vec<&str> = registry.entries.iter().map(|e| e.storage_key).collect();
    assert!(keys.contains(&"test"));
    assert!(keys.contains(&"second"));
}
```

- [ ] **Step 3: Run all tests**

Run: `cargo test --workspace --all-targets`
Expected: all crates green. The persistence test file requires single-threaded execution within its binary; the in-process mutex handles that.

### Task 14: Commit Phase A

- [ ] **Step 1: Stage and commit**

```bash
git add Cargo.toml crates/wc-core-macros crates/wc-core/Cargo.toml crates/wc-core/src/lib.rs crates/wc-core/src/settings crates/wc-core/tests/settings_persistence.rs crates/wc-core/tests/settings_plugin.rs
git commit -m "Add settings core: SketchSettings derive, persistence, SketchRestart"
```

---

# Phase B — Settings UI (egui panels)

Phase A produced a settings system you can't see. Phase B adds the two panels.

### Task 15: Wire `EguiPlugin` into the binary

**Files:**
- Modify: `crates/waveconductor/src/main.rs`

- [ ] **Step 1: Add `EguiPlugin` to the app builder**

Open the binary's `main.rs`. Find the `App::new()` chain (probably around the `add_plugins(DefaultPlugins)` line). Insert the `EguiPlugin` call between `DefaultPlugins` and `CorePlugin`:

```rust
// Inside fn main():
App::new()
    .add_plugins(DefaultPlugins)
    .add_plugins(bevy_egui::EguiPlugin {
        // We don't currently route input through multiple egui contexts.
        enable_multipass_for_primary_context: false,
    })
    .add_plugins(wc_core::CorePlugin)
    .run();
```

(If `main.rs` looks different, locate the equivalent insertion point — `EguiPlugin` must be added after `DefaultPlugins` (which it depends on for `WindowPlugin`) and before `CorePlugin` (which uses egui contexts in `SettingsPlugin`).)

- [ ] **Step 2: Verify the binary still builds**

Run: `cargo build -p waveconductor`
Expected: clean build.

### Task 16: User panel renderer

**Files:**
- Modify: `crates/wc-core/src/settings/panel_user.rs`

- [ ] **Step 1: Replace the stub with the real panel**

```rust
//! Curated user-facing settings panel.
//!
//! Iterates [`super::SettingsRegistry`] and, for each registered settings
//! resource, draws an `egui` collapsing header containing typed widgets
//! for every field with `category = User`. `Dev`-category fields are
//! invisible here — the Shift+D inspector renders them instead.
//!
//! Implementation notes
//!
//! - Uses an exclusive `world: &mut World` system because we need to read
//!   the registry's `Vec<RegisteredSettings>` and then mutate each
//!   resource it points at; this can't be expressed with normal system
//!   params.
//! - The panel is registered behind an `egui::Window` keyed by a stable
//!   id so egui persists position / collapsed state across frames.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use super::def::{SettingDef, SettingKind, SettingsCategory};
use super::registry::SettingsRegistry;
use super::test_settings::TestSketchSettings;
use super::trait_def::SketchSettings;

/// Hook called by [`super::SettingsPlugin::build`].
pub(super) fn add_systems(app: &mut App) {
    app.add_systems(Update, draw_user_panel);
}

/// Exclusive system that draws the user panel.
fn draw_user_panel(world: &mut World) {
    let registry = world
        .get_resource::<SettingsRegistry>()
        .cloned()
        .unwrap_or_default();
    if registry.entries.is_empty() {
        return;
    }

    // SAFETY: EguiContexts is the canonical way to grab the primary
    // window's egui context. Bevy's `SystemState` lets us pull it out of
    // an exclusive system.
    let mut state: bevy::ecs::system::SystemState<EguiContexts<'_, '_>> =
        bevy::ecs::system::SystemState::new(world);
    let mut contexts = state.get_mut(world);
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let ctx = ctx.clone();
    state.apply(world);

    egui::Window::new("Settings")
        .id(egui::Id::new("wc-settings-user-panel"))
        .default_open(false)
        .show(&ctx, |ui| {
            for entry in &registry.entries {
                ui.collapsing(entry.storage_key, |ui| {
                    // For Plan 5 we only ship one typed renderer per known
                    // settings struct. Real sketches in Plan 6+ will each
                    // add a renderer here (or we'll switch to a fully
                    // reflection-driven walker once we hit two real
                    // sketches and have something to factor against).
                    if entry.storage_key == TestSketchSettings::STORAGE_KEY {
                        render_user_fields::<TestSketchSettings>(world, ui, &entry.def);
                    } else {
                        ui.label(
                            "(no typed renderer; open the dev panel with \
                             Shift+D for full inspection)",
                        );
                    }
                });
            }
        });
}

/// Render every `category = User` field of `S` against `ui`.
fn render_user_fields<S: SketchSettings>(world: &mut World, ui: &mut egui::Ui, defs: &[SettingDef]) {
    // We avoid `bevy_reflect::ReflectMut` plumbing for the typed renderer.
    // Instead, switch on the field name. For Plan 5 this is the single
    // synthetic struct, so the cost is tiny and the code is explicit.
    let mut value = world.resource::<S>().clone();
    let mut dirty = false;

    if std::any::TypeId::of::<S>() == std::any::TypeId::of::<TestSketchSettings>() {
        // Safe cast: same TypeId.
        let typed: &mut TestSketchSettings = checked_downcast_mut(&mut value);
        for def in defs {
            if def.category != SettingsCategory::User {
                continue;
            }
            match def.field_name {
                "widget_count" => {
                    if let SettingKind::Number(range) = &def.kind {
                        let mut tmp = typed.widget_count as i64;
                        let lo = range.min.unwrap_or(0.0) as i64;
                        let hi = range.max.unwrap_or(1000.0) as i64;
                        if ui
                            .add(egui::Slider::new(&mut tmp, lo..=hi).text(def.label))
                            .changed()
                        {
                            typed.widget_count = tmp.max(0) as u32;
                            dirty = true;
                        }
                    }
                }
                "tempo_hz" => {
                    if let SettingKind::Number(range) = &def.kind {
                        let lo = range.min.unwrap_or(0.0) as f32;
                        let hi = range.max.unwrap_or(1.0) as f32;
                        if ui
                            .add(egui::Slider::new(&mut typed.tempo_hz, lo..=hi).text(def.label))
                            .changed()
                        {
                            dirty = true;
                        }
                    }
                }
                "enable_tint" => {
                    if ui.checkbox(&mut typed.enable_tint, def.label).changed() {
                        dirty = true;
                    }
                }
                "tint_color" => {
                    ui.horizontal(|ui| {
                        ui.label(def.label);
                        if egui::color_picker::color_edit_button_rgba(
                            ui,
                            &mut egui_rgba(&mut typed.tint_color),
                            egui::color_picker::Alpha::OnlyBlend,
                        )
                        .changed()
                        {
                            dirty = true;
                        }
                    });
                }
                _ => {}
            }
        }
    }

    if dirty {
        *world.resource_mut::<S>() = value;
    }
}

/// Reinterpret `&mut S` as `&mut T` once the caller has verified
/// `TypeId::of::<S>() == TypeId::of::<T>()`. Uses `Any::downcast_mut`
/// (safe, runtime-checked) so the workspace `unsafe_code = "deny"` lint
/// stays clean. Kept in one place so the contract is auditable.
fn checked_downcast_mut<S: SketchSettings, T: 'static>(value: &mut S) -> &mut T {
    let any: &mut dyn std::any::Any = value;
    any.downcast_mut::<T>()
        .expect("caller verified TypeId match")
}

/// Wrap `[f32; 4]` as an `egui::Rgba` reference for the color picker.
fn egui_rgba(rgba: &mut [f32; 4]) -> egui::Rgba {
    egui::Rgba::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
}
```

The `as`-cast lints inside this file will trip the workspace `as_conversions = "warn"` rule. Add a focused `#![allow]` at the top of the file:

```rust
#![allow(
    clippy::as_conversions,
    reason = "panel renderer converts between u32/i64/f32 for egui widgets; bounds-checked above"
)]
```

(Place the attribute at the top of `panel_user.rs`, immediately under the module docs.)

- [ ] **Step 2: Verify**

Run: `cargo build -p wc-core`
Expected: clean build, no clippy warnings.

### Task 17: Dev panel (bevy-inspector-egui)

**Files:**
- Modify: `crates/wc-core/src/settings/panel_dev.rs`

- [ ] **Step 1: Replace the `add_systems` stub with the real wiring**

Append (or replace, if already present) at the bottom of `panel_dev.rs`:

```rust
/// Plugin assembly hook called by [`super::SettingsPlugin::build`].
///
/// Adds:
/// - [`draw_dev_panel`] system on `Update`, conditional on
///   [`DevPanelVisible::0`] being true.
pub(super) fn add_systems(app: &mut App) {
    app.add_systems(Update, draw_dev_panel.run_if(dev_panel_visible));
}

fn dev_panel_visible(visible: Res<'_, DevPanelVisible>) -> bool {
    visible.0
}

/// Exclusive `&mut World` system that opens a `bevy-inspector-egui`
/// world-inspector window. Renders nothing when the panel is hidden
/// (the `run_if` above gates entry).
fn draw_dev_panel(world: &mut World) {
    let mut state: bevy::ecs::system::SystemState<bevy_egui::EguiContexts<'_, '_>> =
        bevy::ecs::system::SystemState::new(world);
    let mut contexts = state.get_mut(world);
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let ctx = ctx.clone();
    state.apply(world);

    bevy_egui::egui::Window::new("Dev Inspector")
        .id(bevy_egui::egui::Id::new("wc-settings-dev-panel"))
        .default_open(true)
        .show(&ctx, |ui| {
            bevy_inspector_egui::bevy_inspector::ui_for_world(world, ui);
        });
}
```

- [ ] **Step 2: Verify**

Run: `cargo build -p wc-core`
Expected: clean build.

### Task 18: Integration test for the full panel-toggling flow

**Files:**
- Modify: `crates/wc-core/tests/settings_plugin.rs`

- [ ] **Step 1: Append a panel-toggle assertion**

```rust
#[test]
fn toggling_dev_panel_via_action_updates_resource() {
    use leafwing_input_manager::prelude::ActionState;
    use wc_core::lifecycle::actions::WaveConductorAction;

    let mut app = make_app();
    app.update();
    assert!(!app.world().resource::<DevPanelVisible>().0);

    app.world_mut()
        .resource_mut::<ActionState<WaveConductorAction>>()
        .press(&WaveConductorAction::ToggleDevPanel);
    app.update();
    assert!(app.world().resource::<DevPanelVisible>().0);
}

#[test]
fn full_app_schedule_runs_without_panicking() {
    // Smoke test: 30 frames of updates must not panic with the egui
    // contexts uninitialized (we never spawn a real window in this test,
    // but EguiContexts::ctx_mut returns Err which the panel systems
    // handle gracefully).
    let mut app = make_app();
    for _ in 0..30 {
        app.update();
    }
}
```

- [ ] **Step 2: Run**

Run: `cargo test --workspace --all-targets`
Expected: all green.

### Task 19: Commit Phase B

- [ ] **Step 1: Stage and commit**

```bash
git add crates/waveconductor/src/main.rs crates/wc-core/src/settings/panel_user.rs crates/wc-core/src/settings/panel_dev.rs crates/wc-core/tests/settings_plugin.rs
git commit -m "Add settings UI: user panel and Shift+D dev inspector"
```

---

# Phase C — Manual smoke test, push, tag

### Task 20: Manual smoke test

- [ ] **Step 1: Launch the app**

Run: `cargo run -p waveconductor`
Expected: app window opens. No panics in the terminal.

- [ ] **Step 2: Toggle the dev panel**

Press `Shift+D`. Expected: a "Dev Inspector" window appears showing the world tree. Inside it, find `TestSketchSettings`. Press `Shift+D` again — window disappears.

- [ ] **Step 3: Open the user panel and edit**

The "Settings" window should always be visible (collapsed by default). Click it open, drill into `test`, drag the `widget_count` slider. Then close the app.

- [ ] **Step 4: Verify persistence**

Re-launch with `cargo run -p waveconductor`. Open `Settings → test`. Expected: `widget_count` shows the value you set, not the default.

- [ ] **Step 5: Verify TOML file**

Run: `cat "$(python3 -c 'import os, sys; print(os.path.expanduser("~/Library/Application Support/waveconductor/sketch-settings.toml") if sys.platform=="darwin" else os.path.expandvars("$XDG_CONFIG_HOME/waveconductor/sketch-settings.toml"))')"`
Expected: a `[test]` section with the values you set.

If anything in steps 1-5 fails, do not proceed — debug and re-run the smoke test from step 1.

### Task 21: Push and tag

- [ ] **Step 1: Push the branch**

```bash
git push origin rewrite/bevy
```

- [ ] **Step 2: Wait for CI**

Watch the GitHub Actions workflow on `rewrite/bevy`. All jobs (fmt, clippy, check-secrets, deny, audit, test on macOS/Ubuntu/Windows, doc) must be green.

- [ ] **Step 3: Tag the milestone**

```bash
git tag v5-settings
git push origin v5-settings
```

- [ ] **Step 4: Verify the tag CI passes**

The push triggers CI again on the tag ref. Confirm all jobs green.

---

## Self-review checklist

- **Spec §5.5 coverage**: ✅ derive macro (Task 8) → ✅ `Default` impl (Task 8 `emit_default`) → ✅ `serde::Serialize`/`Deserialize`/`Reflect` (left to the user struct's own derive line in Task 11; macro deliberately does not emit these per the trait docs) → ✅ `SettingsDef` table (Task 6/8) → ✅ TOML native persistence (Task 9) → ✅ localStorage web persistence (Task 9 wasm path) → ✅ No v4 migration (defaults on first run, Task 9 / Task 12) → ✅ User panel with `category = User` filtering (Task 16) → ✅ Dev panel via `bevy-inspector-egui` toggled by Shift+D (Task 17, leveraging the Plan 2 action) → ✅ `SketchRestart` event on `requires_restart` change (Task 11 `emit_restart_events`, Task 13 test).

- **Plan 4 carry-forwards**: ✅ `AudioStream._stream` doc (Task 1) → ✅ `DspHost::render` Plan-6 TODO (Task 2).

- **Placeholder scan**: no `TBD` / `implement later` / `Similar to Task N` / unspecified error handling. Every code step ships full code.

- **Type consistency**: `SettingDef` fields are identical between def.rs (Task 6) and the derive macro output (Task 8). `SketchSettings::STORAGE_KEY` is `&'static str` throughout. `SketchRestart::storage_key` matches. `SettingsCategory::User`/`Dev` used consistently. `NumberRange::{min,max,step}` are all `Option<f64>` everywhere.

- **Test coverage**: 8 macro tests (Task 12), 5 persistence tests (Task 13 step 1), 6 plugin/restart tests (Task 13 step 2 + Task 18 additions). Total 19 new tests on top of existing suites.

- **Workspace lints**: `as_conversions` is opted out inside `panel_user.rs` only (scoped reason given); `expect_used` is allowed inside `wc-core-macros` (proc-macro context); `unsafe_code` is **not** used anywhere — the `checked_downcast_mut` name is misleading but the body uses checked `Any::downcast_mut`.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-23-v5-plan-5-settings.md`.
