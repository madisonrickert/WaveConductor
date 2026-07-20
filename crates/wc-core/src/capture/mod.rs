//! In-app deterministic frame-capture scaffold (debug builds only).
//!
//! ## Role
//!
//! Activated at runtime by the `WC_CAPTURE` env var (parsed once into
//! [`config::CaptureConfig`]). Pins a fixed virtual-time `dt`, waits for the
//! sketch's assets to be ready plus a settle window, then screenshots the
//! primary window at the scheduled sim-frame indices, writes a self-describing
//! `run.json`, and requests `AppExit`. See [`system`] for the determinism
//! contract.
//!
//! ## Release safety (Option A hybrid gating)
//!
//! This whole module is `#[cfg(debug_assertions)]`-gated at its `lib.rs`
//! declaration. It must never compile into release: capture relies on
//! `debug-assertions = false` in the release/soak profiles. If you ever flip
//! `debug-assertions = true` on a release-class profile, this system and its
//! per-frame work reappear — don't. See the guard comment on `[profile.release]`
//! in the workspace `Cargo.toml`.

pub mod config;
pub mod system;

use bevy::prelude::*;

use config::parse_wc_capture;
use system::{detect_assets_ready, drive_capture, pin_capture_timestep, CaptureState};

/// Parses `WC_CAPTURE` once at build and, when present, wires the capture
/// systems + state. When `WC_CAPTURE` is unset the plugin inserts nothing and
/// every capture system early-returns on its missing [`config::CaptureConfig`].
///
/// ## Signal flow
///
/// `WC_CAPTURE` -> [`config::CaptureConfig`] (parse-once). Each `Update`:
/// [`detect_assets_ready`] flips the readiness gate on sketch entry;
/// [`pin_capture_timestep`] pins `Time<Virtual>` to the fixed `dt` once ready;
/// [`drive_capture`] advances the settle/frame counter, screenshots scheduled
/// frames, and requests `AppExit` after the last one.
pub struct CapturePlugin;

impl Plugin for CapturePlugin {
    fn build(&self, app: &mut App) {
        let Ok(raw) = std::env::var("WC_CAPTURE") else {
            return;
        };
        match parse_wc_capture(&raw) {
            Ok(mut config) => {
                // Fold in the sibling env signals (see `config` module docs):
                // the launcher's window-size override (recorded in `run.json`)
                // and whether the run stays on the Home screen — derived from
                // the SAME env var the binary's startup override reads, so the
                // readiness gate can never disagree with where the app
                // actually starts (unset / `home` / unknown all fall back to
                // Home there).
                config.resolution = std::env::var("WC_CAPTURE_RESOLUTION")
                    .ok()
                    .and_then(|v| config::parse_resolution(&v));
                config.expect_home = match std::env::var("WAVECONDUCTOR_START_SKETCH") {
                    // Any value that does not parse to a sketch (including the
                    // explicit `home`) leaves the app on the Home screen.
                    Ok(value) => crate::lifecycle::state::AppState::from_name(&value).is_none(),
                    // Unset -> the app starts (and stays) at Home.
                    Err(_) => true,
                };
                tracing::info!(?config, "WC_CAPTURE active");
                app.insert_resource(config);
                app.init_resource::<CaptureState>();
                app.add_systems(
                    Update,
                    (detect_assets_ready, pin_capture_timestep, drive_capture).chain(),
                );
            }
            Err(err) => tracing::error!(%err, "WC_CAPTURE parse failed; capture disabled"),
        }
    }
}
