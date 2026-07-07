//! `cargo xtask` dispatcher.
//!
//! Single binary providing the agent-friendly subcommands documented in the
//! workspace design spec (§5.10). Every subcommand accepts `--json` for
//! machine-readable output. New subcommands are added as modules under
//! `xtask/src/` and registered in [`Cli`].

#![allow(clippy::print_stdout, reason = "xtask is a CLI; printing is its job")]

mod bundle;
mod capture;
mod check_secrets;
mod manifest;
mod msi;
mod util;
mod validate_shaders;

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
    /// Build the release binary and assemble a self-contained WaveConductor.app (macOS).
    BundleMac(bundle::mac::Args),
    /// Build the release binary and assemble a self-contained Linux staging dir.
    BundleLinux(bundle::linux::Args),
    /// Build the release binary and assemble a self-contained Windows staging dir.
    BundleWindows(bundle::windows::Args),
    /// Package the staged Windows app dir into an MSI installer.
    PackageWindowsMsi(msi::Args),
    /// Parse + validate WGSL shaders with naga (self-contained shaders;
    /// `#import` shaders are runtime-validated).
    ValidateShaders(validate_shaders::Args),
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
        Command::BundleMac(args) => bundle::mac::run(args),
        Command::BundleLinux(args) => bundle::linux::run(args),
        Command::BundleWindows(args) => bundle::windows::run(args),
        Command::PackageWindowsMsi(args) => msi::run(args),
        Command::ValidateShaders(args) => validate_shaders::run(args),
    };
    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
