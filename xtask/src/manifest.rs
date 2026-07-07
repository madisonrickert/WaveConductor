//! `cargo xtask manifest` — list all xtask subcommands with descriptions.

use std::fmt::Write as _;

use clap::Args as ClapArgs;

use crate::util::json_escape;

/// Arguments for the manifest subcommand.
#[derive(ClapArgs)]
pub struct Args {
    /// Output as JSON array.
    #[arg(long)]
    pub json: bool,
}

/// One entry in the manifest table.
struct Entry {
    name: &'static str,
    description: &'static str,
}

// Hand-maintained list of xtask subcommands. The `Command` enum in `main.rs` is
// the authoritative source for what subcommands exist; this table mirrors that
// list for the human-readable manifest output. If you add a subcommand to
// `main.rs`, you MUST add an entry here too, or the manifest will silently
// diverge from the real command surface.
//
// This can't drift silently: `xtask/tests/manifest.rs` cross-checks this table's
// names against both clap's own `--help` subcommand list (derived straight from
// the `Command` enum) and its own canonical `EXPECTED_SUBCOMMANDS`, so all three
// lists are required to agree.
const SUBCOMMANDS: &[Entry] = &[
    Entry {
        name: "manifest",
        description: "List all xtask subcommands with descriptions.",
    },
    Entry {
        name: "check-secrets",
        description: "Regex-scan the working tree for forbidden secrets and local paths.",
    },
    Entry {
        name: "capture",
        description: "Deterministic visual capture + baseline regression for a scenario.",
    },
    Entry {
        name: "bundle-mac",
        description: "Build the release binary and assemble a self-contained WaveConductor.app.",
    },
    Entry {
        name: "bundle-linux",
        description: "Build the release binary and assemble a self-contained Linux staging dir.",
    },
    Entry {
        name: "bundle-windows",
        description: "Build the release binary and assemble a self-contained Windows staging dir.",
    },
    Entry {
        name: "package-windows-msi",
        description: "Package the staged Windows app dir into an MSI installer.",
    },
    Entry {
        name: "validate-shaders",
        description:
            "Parse + validate WGSL shaders with naga (self-contained; #import shaders skipped).",
    },
];

/// Execute the manifest subcommand.
pub fn run(args: &Args) {
    if args.json {
        let mut out = String::from("[\n");
        for (i, entry) in SUBCOMMANDS.iter().enumerate() {
            let comma = if i + 1 < SUBCOMMANDS.len() { "," } else { "" };
            let _ = writeln!(
                out,
                "  {{\"name\": \"{}\", \"description\": \"{}\"}}{}",
                json_escape(entry.name),
                json_escape(entry.description),
                comma,
            );
        }
        out.push(']');
        println!("{out}");
    } else {
        println!("{:<20} DESCRIPTION", "SUBCOMMAND");
        println!("{}", "-".repeat(72));
        for entry in SUBCOMMANDS {
            println!("{:<20} {}", entry.name, entry.description);
        }
    }
}
