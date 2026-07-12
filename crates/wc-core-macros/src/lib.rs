//! # wc-core-macros
//!
//! Proc-macro crate paired with `wc-core`. Exports the
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
//!
//!     #[setting(default = Quality::High, ty = Enum, category = User)]
//!     pub quality: Quality,
//! }
//! ```
//!
//! Per-field attributes (all optional unless noted):
//!
//! | Key                | Type      | Default                          |
//! |--------------------|-----------|----------------------------------|
//! | `default`          | expr      | `Default::default()`             |
//! | `label`            | string    | the field name                   |
//! | `unit`             | string    | `""` (suffix on `Number` values, e.g. `"ms"`) |
//! | `section`          | string    | `""` (no section header)         |
//! | `category`         | `User` \| `Dev` | `Dev`                       |
//! | `ty`               | `Number` \| `Boolean` \| `Color` \| `Text` \| `TextList` \| `FilePath` \| `TemplateLibrary` \| `Enum` \| `RuntimeEnum` | `Number` |
//! | `min`, `max`, `step` | numeric expr | none (only meaningful on `Number`) |
//! | `extensions`       | `["ext", ...]` | none (only meaningful on `FilePath`) |
//! | `filter_label`     | string    | `"File"` (only meaningful on `FilePath`) |
//! | `options_key`      | string    | none (**required** on `RuntimeEnum`) |
//! | `requires_restart` | flag      | absent                           |
//!
//! ## `ty = Enum`
//!
//! The field's type must be a `Reflect`-derived enum with **unit variants
//! only** (no tuple or struct payloads). No variant list appears in the
//! attribute: the expansion calls `wc_core::settings::enum_variant_names`,
//! which reads the names from the enum's `bevy_reflect::TypeInfo`, so the
//! `SettingKind::Enum { variants }` metadata always matches the enum
//! definition. A proc macro cannot inspect the field type's definition, so
//! the unit-variants-only rule is enforced at runtime instead of compile
//! time: `enum_variant_names` fires a `debug_assert!` the first time
//! `settings_def()` runs (settings registration). Variant names double as
//! the persisted values — serde serializes unit variants as their name
//! string, so avoid `#[serde(rename...)]` on enum-setting types (the panel
//! writes back through reflection, which always uses the Rust identifiers).
//! The variant names are also the panel's display strings: there is no
//! per-variant label mapping yet, so pick variant identifiers that read well
//! in a dropdown.
//!
//! ## `ty = RuntimeEnum`
//!
//! The field's Rust type must be `String` (checked only at render time via
//! `try_downcast_mut`, exactly like `ty = Text` — the macro cannot verify
//! field types beyond the `bool` special-case in `default_kind_for_type`).
//! Unlike `ty = Enum`, whose variant list can only be checked at runtime (a
//! proc macro cannot see a field type's own definition), `options_key` is a
//! literal string in the attribute itself, so the macro checks it directly:
//! `ty = RuntimeEnum` without `options_key = "..."` is a **compile error**.
//! The live option list at that key comes from whichever
//! `wc_core::settings::RuntimeEnumOptionsSource` a module registers via
//! `wc_core::settings::RegisterRuntimeEnumOptionsExt::register_runtime_enum_options`
//! — see that trait's docs for the registration side. Use this instead of
//! `ty = Enum` whenever the candidate list is only known at runtime (an
//! enumerated audio device, a connected monitor), not fixed by a Rust enum.

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
    validate_fields(&fields)?;

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
    /// Editable list of short strings, backed by a `Vec<String>` field.
    TextList,
    FilePath,
    /// Managed image template library; same `filter_label`/`extensions`
    /// attributes as `FilePath`, distinct `SettingKind`.
    TemplateLibrary,
    /// Unit-variant enum rendered as a `ComboBox`. Variant names are derived
    /// from the field type's reflection info at runtime, not listed in the
    /// attribute — see the module docs (`## ty = Enum`).
    Enum,
    /// `String`-valued `ComboBox` whose options come from a
    /// runtime-registered `RuntimeEnumOptionsSource`, not a Rust enum. See
    /// the module docs (`## ty = RuntimeEnum`).
    RuntimeEnum,
}

struct FieldInfo {
    ident: Ident,
    /// The field's declared type. Needed by `Kind::Enum` emission, which
    /// turbofishes it into `enum_variant_names::<#ty>()`.
    ty: syn::Type,
    default: Option<Expr>,
    label: Option<String>,
    /// Unit suffix for numeric fields (e.g. `"ms"`). `None` serialises to `""`.
    unit: Option<String>,
    /// Section group name. `None` serialises to `""` (no header).
    section: Option<String>,
    category: Category,
    requires_restart: bool,
    kind: Kind,
    min: Option<Expr>,
    max: Option<Expr>,
    step: Option<Expr>,
    /// File extensions for `Kind::FilePath`. None for other kinds.
    extensions: Option<Vec<String>>,
    /// Human-facing filter label for `Kind::FilePath`. None for other kinds.
    filter_label: Option<String>,
    /// Options-source key for `Kind::RuntimeEnum`. `None` for other kinds;
    /// required (checked in `validate_fields`) when `kind` is
    /// `Kind::RuntimeEnum`.
    options_key: Option<String>,
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
            ty: field.ty.clone(),
            default: None,
            label: None,
            unit: None,
            section: None,
            category: Category::Dev,
            requires_restart: false,
            // Inferred from the field type so a `bool` renders as a checkbox
            // even without an explicit `ty = Boolean`; an explicit `ty` parsed
            // from the attributes below still overrides this.
            kind: default_kind_for_type(&field.ty),
            min: None,
            max: None,
            step: None,
            extensions: None,
            filter_label: None,
            options_key: None,
        };

        for attr in &field.attrs {
            if !attr.path().is_ident("setting") {
                continue;
            }
            attr.parse_nested_meta(|meta| parse_setting_attr(meta, &mut info))?;
        }

        out.push(info);
    }
    Ok(out)
}

/// Attribute combinations `parse_setting_attr` cannot reject on its own
/// because they depend on more than one attribute key at once --
/// `options_key` is only meaningful together with `ty = RuntimeEnum`, and
/// unlike `ty = Enum`'s variant-list contract (checked only at runtime; see
/// `enum_variant_names`'s docs in `wc_core::settings::def` for why), this one
/// the macro can and does check here, because `options_key` is a literal in
/// the attribute itself rather than something requiring the field type's own
/// definition.
fn validate_fields(fields: &[FieldInfo]) -> syn::Result<()> {
    for f in fields {
        if matches!(f.kind, Kind::RuntimeEnum) && f.options_key.is_none() {
            return Err(syn::Error::new(
                f.ident.span(),
                format!(
                    "`ty = RuntimeEnum` on `{}` requires `options_key = \"...\"`",
                    f.ident
                ),
            ));
        }
    }
    Ok(())
}

/// Parse a single `key = value` (or bare flag) inside `#[setting(...)]`.
/// Mutates `info` in place; returns an error for unknown keys.
fn parse_setting_attr(
    meta: syn::meta::ParseNestedMeta<'_>,
    info: &mut FieldInfo,
) -> syn::Result<()> {
    if meta.path.is_ident("default") {
        info.default = Some(meta.value()?.parse::<Expr>()?);
    } else if meta.path.is_ident("label") {
        let value: LitStr = meta.value()?.parse()?;
        info.label = Some(value.value());
    } else if meta.path.is_ident("unit") {
        let value: LitStr = meta.value()?.parse()?;
        info.unit = Some(value.value());
    } else if meta.path.is_ident("section") {
        let value: LitStr = meta.value()?.parse()?;
        info.section = Some(value.value());
    } else if meta.path.is_ident("filter_label") {
        let value: LitStr = meta.value()?.parse()?;
        info.filter_label = Some(value.value());
    } else if meta.path.is_ident("options_key") {
        let value: LitStr = meta.value()?.parse()?;
        info.options_key = Some(value.value());
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
            "TextList" => Kind::TextList,
            "FilePath" => Kind::FilePath,
            "TemplateLibrary" => Kind::TemplateLibrary,
            "Enum" => Kind::Enum,
            "RuntimeEnum" => Kind::RuntimeEnum,
            other => {
                return Err(meta.error(format!(
                    "unknown ty `{other}` (expected `Number`, `Boolean`, `Color`, `Text`, `TextList`, `FilePath`, `TemplateLibrary`, `Enum`, or `RuntimeEnum`)"
                )))
            }
        };
    } else if meta.path.is_ident("min") {
        info.min = Some(meta.value()?.parse::<Expr>()?);
    } else if meta.path.is_ident("max") {
        info.max = Some(meta.value()?.parse::<Expr>()?);
    } else if meta.path.is_ident("step") {
        info.step = Some(meta.value()?.parse::<Expr>()?);
    } else if meta.path.is_ident("extensions") {
        let value = meta.value()?;
        let arr: syn::ExprArray = value.parse()?;
        let mut exts: Vec<String> = Vec::with_capacity(arr.elems.len());
        for elem in &arr.elems {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = elem
            {
                exts.push(s.value());
            } else {
                return Err(syn::Error::new_spanned(
                    elem,
                    "`extensions` must be an array of string literals",
                ));
            }
        }
        info.extensions = Some(exts);
    } else if meta.path.is_ident("requires_restart") {
        info.requires_restart = true;
    } else {
        return Err(meta.error("unknown #[setting(...)] key"));
    }
    Ok(())
}

/// The [`Kind`] a field defaults to based on its declared type, before any
/// `#[setting(ty = ...)]` attribute is parsed (which overrides this).
///
/// A `bool` field renders as a checkbox, so it defaults to [`Kind::Boolean`];
/// every other type defaults to [`Kind::Number`] (the numeric slider), matching
/// the prior behaviour. This stops a `bool` setting from silently falling
/// through to the numeric renderer — which shows "(unsupported number type)"
/// because a `bool` cannot downcast to `u32`/`f32`/etc. — when the author omits
/// `ty = Boolean`.
fn default_kind_for_type(ty: &syn::Type) -> Kind {
    if let syn::Type::Path(type_path) = ty {
        if type_path.qself.is_none() {
            if let Some(segment) = type_path.path.segments.last() {
                if segment.ident == "bool" {
                    return Kind::Boolean;
                }
            }
        }
    }
    Kind::Number
}

fn emit_default(struct_name: &Ident, fields: &[FieldInfo]) -> TokenStream2 {
    let inits = fields.iter().map(|f| {
        let ident = &f.ident;
        if let Some(expr) = &f.default {
            quote! { #ident: #expr }
        } else {
            quote! { #ident: ::core::default::Default::default() }
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

/// Convert a `snake_case` field name to `Title Case` for display in the UI.
///
/// Splits on `_`, capitalises the first letter of each word, joins with spaces.
/// Example: `particle_density` → `"Particle Density"`.
///
/// The macro defaults to this transform when no explicit `label = "..."` is
/// provided in the `#[setting(...)]` attribute.
fn title_case(snake: &str) -> String {
    snake
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().to_string() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn emit_trait_impl(struct_name: &Ident, storage_key: &str, fields: &[FieldInfo]) -> TokenStream2 {
    let setting_defs = fields.iter().map(|f| {
        let field_name = f.ident.to_string();
        // Default label: title-case the field name (`particle_density` →
        // `"Particle Density"`). Explicit `label = "..."` in the attribute
        // overrides this. This makes the user panel readable without requiring
        // every field to carry an explicit label.
        let label = f.label.clone().unwrap_or_else(|| title_case(&field_name));
        let unit = f.unit.clone().unwrap_or_default();
        let section = f.section.clone().unwrap_or_default();
        let category = match f.category {
            Category::User => quote! { ::wc_core::settings::SettingsCategory::User },
            Category::Dev => quote! { ::wc_core::settings::SettingsCategory::Dev },
        };
        let requires_restart = f.requires_restart;
        let kind_tokens = emit_kind_tokens(f);
        quote! {
            ::wc_core::settings::SettingDef {
                field_name: #field_name,
                label: #label,
                unit: #unit,
                section: #section,
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

/// Build the `SettingKind::...` construction tokens for one field, dispatched
/// on its parsed [`Kind`]. Split out of [`emit_trait_impl`] (which maps this
/// over every field): the per-`Kind` token emission is a coherent unit in its
/// own right — one arm per setting kind, each building the tokens for that
/// kind's `SettingKind` variant — and factoring it out keeps `emit_trait_impl`
/// itself and this function each fitting on one screen, per `AGENTS.md`'s
/// function-body guidance. It also happens to keep this function's body under
/// Clippy's `too_many_lines` threshold, though that follows from the split
/// rather than motivating it.
fn emit_kind_tokens(f: &FieldInfo) -> TokenStream2 {
    match f.kind {
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
        Kind::TextList => quote! { ::wc_core::settings::SettingKind::TextList },
        Kind::FilePath => {
            let filter_label = f.filter_label.clone().unwrap_or_else(|| "File".to_string());
            let exts: Vec<&str> = f
                .extensions
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(String::as_str)
                .collect();
            quote! {
                ::wc_core::settings::SettingKind::FilePath {
                    filter_label: #filter_label,
                    extensions: &[ #( #exts, )* ],
                }
            }
        }
        Kind::TemplateLibrary => {
            let filter_label = f.filter_label.clone().unwrap_or_else(|| "File".to_string());
            let exts: Vec<&str> = f
                .extensions
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(String::as_str)
                .collect();
            quote! {
                ::wc_core::settings::SettingKind::TemplateLibrary {
                    filter_label: #filter_label,
                    extensions: &[ #( #exts, )* ],
                }
            }
        }
        Kind::Enum => {
            // Variant names come from the field type's reflection info at
            // runtime — `enum_variant_names` returns the `&'static` slice
            // baked into the enum's `TypeInfo`, and debug-asserts the
            // unit-variants-only contract (a proc macro cannot see the
            // enum definition, so this cannot be a compile error).
            let field_ty = &f.ty;
            quote! {
                ::wc_core::settings::SettingKind::Enum {
                    variants: ::wc_core::settings::enum_variant_names::<#field_ty>(),
                }
            }
        }
        Kind::RuntimeEnum => {
            // `validate_fields` already rejected a missing `options_key`
            // for this kind -- unlike `Kind::Enum`'s variant list, which
            // must be checked at runtime because a proc macro cannot see
            // the field type's own definition, `options_key` is a
            // literal in the attribute itself, so the macro checks it
            // here at compile time instead.
            let options_key = f
                .options_key
                .as_deref()
                .expect("validate_fields already rejected a missing options_key");
            quote! {
                ::wc_core::settings::SettingKind::RuntimeEnum {
                    options_key: #options_key,
                }
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
    if let Some(expr) = opt {
        quote! { ::core::option::Option::Some(::core::convert::From::from(#expr)) }
    } else {
        quote! { ::core::option::Option::None }
    }
}

#[cfg(test)]
mod tests {
    use super::expand;

    /// A `ty = RuntimeEnum` field that also supplies `options_key` expands
    /// without error. This is the positive control for the negative test
    /// below: it confirms the failure asserted there is actually caused by
    /// the missing `options_key`, not by some unrelated part of the input.
    #[test]
    fn runtime_enum_with_options_key_expands_ok() {
        let input: syn::DeriveInput = syn::parse_quote! {
            #[settings(storage_key = "t")]
            struct S {
                #[setting(default = String::new(), ty = RuntimeEnum, options_key = "audio_devices", category = User)]
                device: String,
            }
        };
        expand(&input).expect("a ty = RuntimeEnum field with options_key must be accepted");
    }

    /// `ty = RuntimeEnum` without `options_key` must be rejected at macro
    /// expansion time -- this is the headline compile-time check the
    /// `RuntimeEnum` setting kind exists to provide (see the module docs,
    /// `## ty = RuntimeEnum`). `expand` touches only `syn`/`proc_macro2`
    /// types, never `proc_macro::TokenStream`, so it is callable directly
    /// from a unit test without a `trybuild`-style compile-fail harness.
    #[test]
    fn runtime_enum_without_options_key_is_rejected() {
        let input: syn::DeriveInput = syn::parse_quote! {
            #[settings(storage_key = "t")]
            struct S {
                #[setting(default = String::new(), ty = RuntimeEnum, category = User)]
                device: String,
            }
        };
        let err = expand(&input)
            .expect_err("a ty = RuntimeEnum field with no options_key must be rejected");
        let message = err.to_string();
        assert!(
            message.contains("device"),
            "error should name the offending field: got {message}"
        );
        assert!(
            message.contains("options_key"),
            "error should mention options_key: got {message}"
        );
    }

    /// A struct with no `#[settings(storage_key = "...")]` attribute at all
    /// must be rejected by `parse_storage_key`'s error branch, exercised
    /// here through `expand`.
    #[test]
    fn missing_storage_key_attribute_is_rejected() {
        let input: syn::DeriveInput = syn::parse_quote! {
            struct S {
                #[setting(default = 0_u32)]
                count: u32,
            }
        };
        let err = expand(&input)
            .expect_err("a struct with no #[settings(storage_key = ...)] must be rejected");
        assert!(err.to_string().contains("storage_key"), "got: {err}");
    }
}
