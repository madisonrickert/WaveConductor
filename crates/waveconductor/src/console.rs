//! Developer command console (debug builds only).
//!
//! Wraps [`bevy_console`] into a [`DevConsolePlugin`] that the binary adds only
//! under `#[cfg(debug_assertions)]` — like the `WC_DEBUG_*` capture toggles, the
//! console never ships in release/soak builds (see `AGENTS.md`). Toggle it with
//! the backtick/grave key (`` ` ``).
//!
//! The console is themed to the settings-dock palette and registers quick dev
//! levers as console commands. Today that is `provider` (switch the
//! hand-tracking backend live, the same effect as the settings dropdown);
//! `bevy_console`'s built-in `help` / `clear` commands are preserved. Adding a
//! command is a `clap` `Parser` + `ConsoleCommand` derive plus one system —
//! see [`provider_command`].

use bevy::prelude::*;
use bevy_console::{reply, AddConsoleCommand, ConsoleCommand, ConsoleConfiguration, ConsolePlugin};
use clap::Parser;
use wc_core::settings::{HandProviderChoice, HandTrackingSettings, SettingsRegistry};

/// Adds the `bevy_console` dev command console, themed and with the app's
/// commands registered. Debug-only — added under `#[cfg(debug_assertions)]`.
pub struct DevConsolePlugin;

impl Plugin for DevConsolePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ConsolePlugin);

        // Mutate (not replace) the configuration ConsolePlugin just inserted so
        // its built-in commands survive; only restyle the window to the dock
        // palette. The toggle key stays the default backtick/grave.
        if let Some(mut cfg) = app.world_mut().get_resource_mut::<ConsoleConfiguration>() {
            use bevy_egui::egui::Color32;
            "WaveConductor console".clone_into(&mut cfg.title_name);
            "› ".clone_into(&mut cfg.symbol);
            // Heavier tint than the docks (the console has no backdrop blur to
            // carry legibility over the artwork).
            cfg.background_color = Color32::from_black_alpha(230);
            cfg.foreground_color = Color32::from_gray(235);
        }

        app.add_console_command::<ProviderCommand, _>(provider_command)
            .add_console_command::<SetCommand, _>(set_command)
            .add_console_command::<SettingsListCommand, _>(settings_list_command);
    }
}

/// `provider <auto|leap|mediapipe|off>` — switch the hand-tracking backend live,
/// identical to moving the settings "Tracking provider" dropdown.
#[derive(Parser, ConsoleCommand)]
#[command(name = "provider")]
struct ProviderCommand {
    /// Backend to select: `auto`, `leap`, `mediapipe`, or `off`.
    choice: String,
}

/// Handler for [`ProviderCommand`]: parse the choice and write it to
/// [`HandTrackingSettings`]; `apply_provider_choice` reconciles the live
/// registry on the next frame, exactly as a dropdown change does.
fn provider_command(
    mut cmd: ConsoleCommand<'_, ProviderCommand>,
    mut settings: ResMut<'_, HandTrackingSettings>,
) {
    let Some(Ok(ProviderCommand { choice })) = cmd.take() else {
        return;
    };
    let parsed = match choice.to_ascii_lowercase().as_str() {
        "auto" => Some(HandProviderChoice::Auto),
        "leap" => Some(HandProviderChoice::Leap),
        "mediapipe" => Some(HandProviderChoice::MediaPipe),
        "off" => Some(HandProviderChoice::Off),
        _ => None,
    };
    if let Some(provider) = parsed {
        settings.provider = provider;
        reply!(cmd, "tracking provider set to {provider:?}");
        cmd.ok();
    } else {
        reply!(
            cmd,
            "unknown provider '{choice}': use auto, leap, mediapipe, or off"
        );
        cmd.failed();
    }
}

/// `set <key> <field> <value>` — set any registered setting by its storage key
/// and field name. The value is parsed against the field's kind (number, bool,
/// enum, text). The lever for every setting, the same writes the panel makes.
#[derive(Parser, ConsoleCommand)]
#[command(name = "set")]
struct SetCommand {
    /// Settings storage key, e.g. `line`, `hand_tracking` (see `settings`).
    key: String,
    /// Field name within that settings struct.
    field: String,
    /// New value, parsed against the field's type.
    value: String,
}

/// Handler for [`SetCommand`]. Validates the setting exists synchronously (for
/// inline feedback), then queues an exclusive command to apply it: the
/// reflection write needs `&mut World`, which a console command system can't
/// take alongside [`ConsoleCommand`]. The apply's result is logged, so it lands
/// in the dev panel's Log view.
fn set_command(
    mut cmd: ConsoleCommand<'_, SetCommand>,
    registry: Res<'_, SettingsRegistry>,
    mut commands: Commands<'_, '_>,
) {
    let Some(Ok(SetCommand { key, field, value })) = cmd.take() else {
        return;
    };
    let exists = registry
        .entries
        .iter()
        .find(|e| e.storage_key == key)
        .is_some_and(|e| e.def.iter().any(|d| d.field_name == field));
    if !exists {
        reply!(cmd, "unknown setting '{key}.{field}' — run `settings`");
        cmd.failed();
        return;
    }
    let summary = format!("{key}.{field} = {value}");
    commands.queue(move |world: &mut World| {
        match wc_core::settings::set_setting(world, &key, &field, &value) {
            Ok(msg) => tracing::info!("console: set {msg}"),
            Err(err) => tracing::warn!("console: set failed: {err}"),
        }
    });
    reply!(cmd, "set {summary} (result in the Log panel)");
    cmd.ok();
}

/// `settings [key]` — list registered settings (storage key → field names),
/// optionally filtered to one key, so the operator knows what `set` accepts.
#[derive(Parser, ConsoleCommand)]
#[command(name = "settings")]
struct SettingsListCommand {
    /// Optional storage key to show only that struct's fields.
    key: Option<String>,
}

/// Handler for [`SettingsListCommand`].
fn settings_list_command(
    mut cmd: ConsoleCommand<'_, SettingsListCommand>,
    registry: Res<'_, SettingsRegistry>,
) {
    let Some(Ok(SettingsListCommand { key })) = cmd.take() else {
        return;
    };
    for entry in &registry.entries {
        if key.as_deref().is_some_and(|k| k != entry.storage_key) {
            continue;
        }
        let fields: Vec<&str> = entry.def.iter().map(|d| d.field_name).collect();
        reply!(cmd, "{}: {}", entry.storage_key, fields.join(", "));
    }
    cmd.ok();
}
