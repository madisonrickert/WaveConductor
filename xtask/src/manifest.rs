//! `cargo xtask manifest` — list all xtask subcommands with descriptions.

use clap::Args as ClapArgs;

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

const SUBCOMMANDS: &[Entry] = &[
    Entry {
        name: "manifest",
        description: "List all xtask subcommands with descriptions.",
    },
    Entry {
        name: "check-secrets",
        description: "Regex-scan the working tree for forbidden secrets and local paths.",
    },
];

/// Execute the manifest subcommand.
pub fn run(args: &Args) {
    if args.json {
        let mut out = String::from("[\n");
        for (i, entry) in SUBCOMMANDS.iter().enumerate() {
            let comma = if i + 1 < SUBCOMMANDS.len() { "," } else { "" };
            out.push_str(&format!(
                "  {{\"name\": \"{}\", \"description\": \"{}\"}}{}\n",
                entry.name, entry.description, comma,
            ));
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
