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
