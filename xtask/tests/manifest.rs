//! Integration tests for `cargo xtask manifest`.
//!
//! The manifest is hand-maintained alongside `Command` in `xtask/src/main.rs`.
//! These tests enforce that the manifest exposes a canonical, up-to-date list
//! of subcommands. If you add a new subcommand to `Command`, you must:
//!
//! 1. Add it to `SUBCOMMANDS` in `xtask/src/manifest.rs`.
//! 2. Add its name to `EXPECTED_SUBCOMMANDS` below.

#![allow(clippy::expect_used, reason = "expect is appropriate in test code")]

use std::process::Command;

/// Canonical list of subcommands the manifest should expose. Update both this
/// list and `xtask/src/manifest.rs::SUBCOMMANDS` together whenever `Command` in
/// `xtask/src/main.rs` gains or loses a variant.
const EXPECTED_SUBCOMMANDS: &[&str] = &["manifest", "check-secrets", "capture", "bundle-mac"];

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
