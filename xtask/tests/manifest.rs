//! Integration tests for `cargo xtask manifest`.
//!
//! The subcommand list is maintained in three places: the `Command` enum in
//! `xtask/src/main.rs` (authoritative — what clap actually dispatches), the
//! `SUBCOMMANDS` table in `xtask/src/manifest.rs` (mirrors it for the
//! human/JSON manifest output), and `EXPECTED_SUBCOMMANDS` below. These tests
//! cross-check all three so they cannot silently drift apart: the manifest's
//! `--json` output is checked against `EXPECTED_SUBCOMMANDS`, and separately
//! `xtask --help`'s subcommand list — generated directly by clap from the
//! `Command` enum, not hand-copied — is checked against `EXPECTED_SUBCOMMANDS`
//! too. If you add a new subcommand to `Command`, you must:
//!
//! 1. Add it to `SUBCOMMANDS` in `xtask/src/manifest.rs`.
//! 2. Add its name to `EXPECTED_SUBCOMMANDS` below.

#![allow(clippy::expect_used, reason = "expect is appropriate in test code")]

use std::process::Command;

/// Canonical list of subcommands the manifest should expose. Update both this
/// list and `xtask/src/manifest.rs::SUBCOMMANDS` together whenever `Command` in
/// `xtask/src/main.rs` gains or loses a variant.
const EXPECTED_SUBCOMMANDS: &[&str] = &[
    "manifest",
    "check-secrets",
    "capture",
    "bundle-mac",
    "bundle-linux",
    "bundle-windows",
    "package-windows-msi",
    "validate-shaders",
];

#[test]
fn manifest_human_output_lists_every_subcommand() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("manifest")
        .output()
        .expect("spawn xtask");
    assert!(
        output.status.success(),
        "manifest subcommand failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    for name in EXPECTED_SUBCOMMANDS {
        assert!(
            stdout.contains(name),
            "manifest human output missing `{name}`: {stdout}",
        );
    }
}

#[test]
fn manifest_json_output_matches_canonical_list() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("manifest")
        .arg("--json")
        .output()
        .expect("spawn xtask");
    assert!(output.status.success(), "manifest --json failed");
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    // Naive JSON parse — extract every `"name": "..."` or `"name":"..."` value.
    let mut found: Vec<String> = Vec::new();
    let mut rest = stdout.as_str();
    while let Some(idx) = rest.find("\"name\"") {
        // Skip past `"name"`, then optional whitespace, then `:`
        let after_key = &rest[idx + "\"name\"".len()..];
        let colon_offset = after_key
            .find(':')
            .expect("malformed JSON: no colon after \"name\"");
        let after_colon = &after_key[colon_offset + 1..];
        // Skip optional whitespace, then opening `"`
        let trimmed = after_colon.trim_start_matches(|c: char| c.is_ascii_whitespace());
        if let Some(value) = trimmed.strip_prefix('"') {
            if let Some(end) = value.find('"') {
                found.push(value[..end].to_string());
                rest = &value[end + 1..];
            } else {
                break;
            }
        } else {
            // Not a string value; skip past this occurrence
            rest = &rest[idx + 1..];
        }
    }

    assert_eq!(
        found,
        EXPECTED_SUBCOMMANDS
            .iter()
            .map(|s| (*s).to_string())
            .collect::<Vec<_>>(),
        "manifest JSON listed subcommands {found:?} but expected {EXPECTED_SUBCOMMANDS:?}",
    );
}

/// Extract subcommand names from a clap `--help` output's `Commands:` section.
///
/// Only lines indented exactly as far as the first entry are read as name
/// lines — this excludes wrapped description continuation lines (which clap
/// indents further, to the description column) without needing to know the
/// terminal width clap assumed. clap's auto-generated `help` pseudo-subcommand
/// is dropped since it isn't a real entry in the `Command` enum.
fn parse_help_subcommand_names(help_text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_commands_section = false;
    let mut entry_indent: Option<usize> = None;

    for line in help_text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        if indent == 0 {
            // Unindented line: either a new section header or the `Usage:` line.
            in_commands_section = trimmed.trim_end() == "Commands:";
            continue;
        }
        if !in_commands_section || trimmed.is_empty() {
            continue;
        }
        match entry_indent {
            None => entry_indent = Some(indent),
            Some(expected) if indent != expected => continue, // wrapped continuation line
            Some(_) => {}
        }
        if let Some(name) = trimmed.split_whitespace().next() {
            if name != "help" {
                names.push(name.to_string());
            }
        }
    }
    names
}

#[test]
fn cli_help_subcommands_match_canonical_list() {
    // `--help`'s subcommand list is generated by clap directly from the
    // `Command` enum in `main.rs` — unlike the manifest table, it cannot be
    // hand-edited out of sync with what the binary actually dispatches.
    // Checking it against `EXPECTED_SUBCOMMANDS` (already checked against the
    // manifest above) closes the loop between all three lists.
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("--help")
        .output()
        .expect("spawn xtask --help");
    assert!(
        output.status.success(),
        "xtask --help failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    let mut from_help = parse_help_subcommand_names(&stdout);
    from_help.sort_unstable();
    let mut expected: Vec<String> = EXPECTED_SUBCOMMANDS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    expected.sort_unstable();

    assert_eq!(
        from_help, expected,
        "xtask --help subcommands {from_help:?} (derived from the `Command` enum in \
         main.rs) disagree with the canonical EXPECTED_SUBCOMMANDS {expected:?} — keep \
         `Command` in main.rs, `SUBCOMMANDS` in manifest.rs, and EXPECTED_SUBCOMMANDS here \
         in sync",
    );
}
