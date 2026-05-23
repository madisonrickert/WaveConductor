//! User-facing settings panel — Phase B target. Stub only in Phase A so the
//! `SettingsPlugin` `build()` call site compiles.

#![allow(dead_code, reason = "filled in during Phase B (Task 14)")]

/// Plugin assembly hook called by [`super::SettingsPlugin::build`]. Empty in
/// Phase A; real implementation lands in Task 14.
pub(super) fn add_systems(_app: &mut bevy::prelude::App) {}
