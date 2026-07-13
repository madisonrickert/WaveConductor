//! In-app soak-test instrumentation (debug builds only).
//!
//! ## Role
//!
//! The counterpart of [`crate::capture`] for the *long* axis. Where capture
//! answers "does frame N look right", soak answers "does this build survive
//! eight unattended hours" — the release gate `AGENTS.md` requires before a
//! tag. Activated at runtime by the `WC_SOAK` env var (parsed once into
//! [`config::SoakConfig`]) and driven by `cargo xtask soak-test`, which is the
//! only thing expected to set it.
//!
//! ## Signal flow
//!
//! `WC_SOAK` -> [`config::SoakConfig`] (parse-once). Each `Update`:
//! [`system::hold_sketch_active`] keeps the interaction timer marked (so the
//! run exercises the active render path, not the screensaver) when the config
//! asks for it; [`system::drive_soak`] republishes `<dir>/health.json` on the
//! configured interval, advances to the next sketch on the cycle interval (this
//! is what exercises the sketch enter/exit lifecycle where GPU-resource leaks
//! live), and requests `AppExit` once the configured duration elapses.
//!
//! The launcher polls `health.json`, pairs each read with an
//! externally-measured RSS, and does all trend analysis and artifact writing;
//! the app's only job is to publish honest readings. There is deliberately no
//! socket and no IPC server — the process that needs the data already owns the
//! process producing it.
//!
//! ## Release safety (Option A hybrid gating)
//!
//! This whole module is `#[cfg(debug_assertions)]`-gated at its `lib.rs`
//! declaration, exactly like [`crate::capture`], and must never compile into
//! release. That means the soak gate measures a *debug* binary. That is the
//! honest, useful trade: the leak/stall failure modes this is hunting
//! (unreleased GPU resources across sketch transitions, an unbounded cache, a
//! thermal stall) are structural and show up in either profile, and a release
//! build carries no instrumentation to report them without reintroducing
//! per-frame work into the shipped binary. Where an absolute release-profile
//! number is needed, run the bundled binary by hand alongside `Activity
//! Monitor` / `htop`.

pub mod config;
pub mod system;

use bevy::prelude::*;

use config::{parse_wc_soak, SoakActivity};
use system::{drive_soak, hold_sketch_active, SoakRuntime};

/// Parses `WC_SOAK` once at build and, when present, wires the soak systems +
/// runtime state. When `WC_SOAK` is unset (every normal run) the plugin inserts
/// nothing and [`drive_soak`] early-returns on its missing
/// [`config::SoakConfig`].
pub struct SoakPlugin;

impl Plugin for SoakPlugin {
    fn build(&self, app: &mut App) {
        let Ok(raw) = std::env::var("WC_SOAK") else {
            return;
        };
        match parse_wc_soak(&raw) {
            Ok(config) => {
                tracing::info!(?config, "WC_SOAK active");
                let hold_active = config.activity == SoakActivity::Active;
                app.insert_resource(SoakRuntime::new(&config));
                app.insert_resource(config);
                app.add_systems(Update, drive_soak);
                if hold_active {
                    app.add_systems(Update, hold_sketch_active);
                }
            }
            Err(err) => {
                tracing::error!(%err, "WC_SOAK parse failed; soak instrumentation disabled");
            }
        }
    }
}
