//! String-addressed setting mutation, for the dev console's `set` command.
//!
//! [`set_setting`] resolves `storage_key.field` against the [`SettingsRegistry`]
//! and the type registry, then parses the string value against the field's
//! [`SettingKind`] and writes it through reflection — so a single command can
//! drive any registered setting without per-field plumbing. The write goes
//! through the same `Mut<dyn Reflect>` path the panel uses, so Bevy change
//! detection, autosave, and restart diffing all fire identically.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "console values parse to f64 then cast to the field's native numeric type, bounds-clamped against the SettingDef range"
)]

use std::any::TypeId;

use bevy::ecs::reflect::ReflectResource;
use bevy::prelude::*;
use bevy::reflect::{DynamicEnum, DynamicVariant, PartialReflect, ReflectMut};

use super::def::{NumberRange, SettingKind};
use super::registry::{SettingsRegistry, SettingsTypeKey};

/// Set `storage_key.field` to `value` (parsed against the field's kind) via
/// reflection. Returns a human-readable confirmation, or an error message
/// suitable for printing back to the console.
///
/// # Errors
/// Returns `Err(message)` when the setting is unknown, the value does not parse
/// for the field's kind, or the kind is not settable from a string.
pub fn set_setting(
    world: &mut World,
    storage_key: &str,
    field: &str,
    value: &str,
) -> Result<String, String> {
    // The SettingDef tells us the field's kind (and numeric range) without
    // touching the live value.
    let def = world
        .get_resource::<SettingsRegistry>()
        .and_then(|r| r.entries.iter().find(|e| e.storage_key == storage_key))
        .and_then(|e| e.def.iter().find(|d| d.field_name == field).cloned())
        .ok_or_else(|| format!("unknown setting `{storage_key}.{field}` (try `settings`)"))?;

    let type_id = type_id_for_key(world, storage_key)
        .ok_or_else(|| format!("settings type `{storage_key}` is not registered"))?;

    // Clone the registry handle so its read guard borrows the clone, not
    // `world` — leaving `world` free for `reflect_mut` while the guard is held
    // (mirrors the user panel's reflection path).
    let app_registry = world.resource::<AppTypeRegistry>().clone();
    let reg_read = app_registry.read();
    let Some(reflect_resource) = reg_read.get_type_data::<ReflectResource>(type_id) else {
        return Err(format!("`{storage_key}` has no ReflectResource"));
    };
    let reflect_result = reflect_resource.reflect_mut(world);
    drop(reg_read);
    let mut reflect_mut = reflect_result.map_err(|_| format!("`{storage_key}` not present"))?;

    let ReflectMut::Struct(struct_mut) = reflect_mut.reflect_mut() else {
        return Err(format!("`{storage_key}` is not a struct"));
    };
    let field_ref = struct_mut
        .field_mut(field)
        .ok_or_else(|| format!("`{storage_key}` has no field `{field}`"))?;

    match &def.kind {
        SettingKind::Number(range) => apply_number(field_ref, range, value)?,
        SettingKind::Boolean => apply_bool(field_ref, value)?,
        SettingKind::Enum { variants } => apply_enum(field_ref, variants, value)?,
        // A file path / template path is stored as a plain String, so a string
        // value works.
        SettingKind::Text
        | SettingKind::FilePath { .. }
        | SettingKind::TemplateLibrary { .. } => apply_text(field_ref, value)?,
        SettingKind::Color => {
            return Err("color settings can't be set from the console; use the panel".to_owned());
        }
    }
    Ok(format!("{storage_key}.{field} = {value}"))
}

/// Resolve a settings type's `TypeId` from its storage key via the
/// [`SettingsTypeKey`] type-data the registry installs.
fn type_id_for_key(world: &World, storage_key: &str) -> Option<TypeId> {
    world
        .resource::<AppTypeRegistry>()
        .read()
        .iter()
        .find_map(|reg| {
            reg.data::<SettingsTypeKey>()
                .filter(|key| key.0 == storage_key)
                .map(|_| reg.type_id())
        })
}

/// Parse `value` as a number and write it into the field's native numeric type,
/// clamped to the `SettingDef` range.
fn apply_number(
    field: &mut dyn PartialReflect,
    range: &NumberRange,
    value: &str,
) -> Result<(), String> {
    let parsed: f64 = value
        .parse()
        .map_err(|_| format!("`{value}` is not a number"))?;
    let clamped = parsed.clamp(
        range.min.unwrap_or(f64::NEG_INFINITY),
        range.max.unwrap_or(f64::INFINITY),
    );
    if let Some(v) = field.try_downcast_mut::<f32>() {
        *v = clamped as f32;
    } else if let Some(v) = field.try_downcast_mut::<f64>() {
        *v = clamped;
    } else if let Some(v) = field.try_downcast_mut::<u32>() {
        *v = clamped.max(0.0) as u32;
    } else if let Some(v) = field.try_downcast_mut::<i32>() {
        *v = clamped as i32;
    } else if let Some(v) = field.try_downcast_mut::<i64>() {
        *v = clamped as i64;
    } else {
        return Err("unsupported numeric field type".to_owned());
    }
    Ok(())
}

/// Parse `value` as `true`/`false` and write it into a `bool` field.
fn apply_bool(field: &mut dyn PartialReflect, value: &str) -> Result<(), String> {
    let parsed: bool = value
        .parse()
        .map_err(|_| format!("`{value}` is not true/false"))?;
    let target = field
        .try_downcast_mut::<bool>()
        .ok_or_else(|| "expected a bool field".to_owned())?;
    *target = parsed;
    Ok(())
}

/// Match `value` (case-insensitively) against the enum's variant list and write
/// the unit variant.
fn apply_enum(
    field: &mut dyn PartialReflect,
    variants: &[&'static str],
    value: &str,
) -> Result<(), String> {
    let matched = variants
        .iter()
        .copied()
        .find(|variant| variant.eq_ignore_ascii_case(value))
        .ok_or_else(|| format!("`{value}` is not one of: {}", variants.join(", ")))?;
    let dynamic = DynamicEnum::new(matched, DynamicVariant::Unit);
    field
        .try_apply(&dynamic)
        .map_err(|e| format!("enum write failed: {e}"))?;
    Ok(())
}

/// Write `value` verbatim into a `String` field (text or file path).
fn apply_text(field: &mut dyn PartialReflect, value: &str) -> Result<(), String> {
    let target = field
        .try_downcast_mut::<String>()
        .ok_or_else(|| "expected a string field".to_owned())?;
    value.clone_into(target);
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact comparison of values set by assignment"
)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;
    use bevy::reflect::Reflect;

    #[test]
    fn number_parses_and_clamps_to_range() {
        let range = NumberRange {
            min: Some(0.0),
            max: Some(10.0),
            step: None,
        };
        let mut v: f32 = 1.0;
        apply_number(&mut v, &range, "7.5").expect("valid number");
        assert_eq!(v, 7.5);
        // Above max clamps.
        apply_number(&mut v, &range, "99").expect("valid number");
        assert_eq!(v, 10.0);
    }

    #[test]
    fn number_rejects_non_numeric() {
        let range = NumberRange::default();
        let mut v: f32 = 1.0;
        assert!(apply_number(&mut v, &range, "abc").is_err());
        assert_eq!(v, 1.0, "value unchanged on parse error");
    }

    #[test]
    fn number_into_integer_field_clamps_and_floors() {
        let range = NumberRange {
            min: Some(0.0),
            max: Some(100.0),
            step: None,
        };
        let mut v: u32 = 5;
        apply_number(&mut v, &range, "42.9").expect("valid");
        assert_eq!(v, 42, "f64 → u32 truncates");
    }

    #[test]
    fn bool_parses_true_false() {
        let mut v = false;
        apply_bool(&mut v, "true").expect("valid bool");
        assert!(v);
        assert!(
            apply_bool(&mut v, "yes").is_err(),
            "only true/false accepted"
        );
    }

    #[derive(Reflect, PartialEq, Debug)]
    enum Mode {
        Slow,
        Fast,
    }

    #[test]
    fn enum_matches_case_insensitively() {
        let mut v = Mode::Slow;
        apply_enum(&mut v, &["Slow", "Fast"], "fast").expect("valid variant");
        assert_eq!(v, Mode::Fast);
        assert!(
            apply_enum(&mut v, &["Slow", "Fast"], "warp").is_err(),
            "unknown variant rejected"
        );
    }

    #[test]
    fn text_sets_string_field() {
        let mut v = String::from("old");
        apply_text(&mut v, "new").expect("string set");
        assert_eq!(v, "new");
    }
}
