//! `cargo xtask` dispatcher.
//!
//! Single binary providing the agent-friendly subcommands documented in the
//! workspace design spec (§5.10). Every subcommand accepts `--json` for
//! machine-readable output. New subcommands are added as modules under
//! `xtask/src/` and registered in [`Cli`].

#![allow(clippy::print_stdout, reason = "xtask is a CLI; printing is its job")]

mod bundle_mac;
mod capture;
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
    /// Deterministic visual capture + baseline regression for a scenario.
    Capture(capture::Args),
    /// Build the release binary and assemble a self-contained WaveConductor.app.
    BundleMac(bundle_mac::Args),
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Manifest(ref args) => {
            manifest::run(args);
            Ok(())
        }
        Command::CheckSecrets(args) => check_secrets::run(args),
        Command::Capture(args) => capture::run(args),
        Command::BundleMac(args) => bundle_mac::run(args),
    };
    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
