//! Integration test for `cargo xtask manifest`.

#![allow(clippy::expect_used, reason = "expect is appropriate in test code")]

use std::process::Command;

#[test]
fn manifest_lists_subcommands() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("manifest")
        .output()
        .expect("failed to spawn xtask");
    assert!(
        output.status.success(),
        "manifest subcommand failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("manifest output not utf8");
    assert!(
        stdout.contains("manifest"),
        "manifest output should list itself: {stdout}"
    );
    assert!(
        stdout.contains("check-secrets"),
        "manifest output should list check-secrets: {stdout}"
    );
}
