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

fn emit_trait_impl(struct_name: &Ident, storage_key: &str, fields: &[FieldInfo]) -> TokenStream2 {
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
    if let Some(expr) = opt {
        quote! { ::core::option::Option::Some(::core::convert::From::from(#expr)) }
    } else {
        quote! { ::core::option::Option::None }
    }
}
