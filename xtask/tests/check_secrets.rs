//! Integration test for `cargo xtask check-secrets`.

#![allow(clippy::expect_used, reason = "expect is appropriate in test code")]

use std::fs;
use std::process::Command;

/// Helper: run check-secrets against a temp directory containing the supplied file.
fn run_against(file_contents: &str) -> (bool, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("planted.rs");
    fs::write(&path, file_contents).expect("write planted file");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("check-secrets")
        .arg("--root")
        .arg(tmp.path())
        .output()
        .expect("spawn xtask");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    (output.status.success(), format!("{stdout}{stderr}"))
}

#[test]
fn clean_tree_passes() {
    let (ok, out) = run_against("fn main() { println!(\"hi\"); }\n");
    assert!(ok, "clean tree should pass: {out}");
}

#[test]
fn home_dir_path_is_flagged() {
    // Build the fixture at runtime so the literal home path isn't embedded in
    // this (now-scanned) test source — see the SKIP_DIRS doc in check_secrets.rs.
    let fixture = format!("// path: /{}/{}/Developer/foo\n", "Users", "alice");
    let (ok, out) = run_against(&fixture);
    assert!(!ok, "home-dir path should be flagged");
    assert!(
        out.contains("/Users/"),
        "report should mention the offending pattern: {out}"
    );
}

#[test]
fn windows_home_dir_path_is_flagged() {
    let (ok, out) = run_against("// path: C:\\\\Users\\\\bob\\\\code\n");
    assert!(!ok, "Windows home-dir path should be flagged");
    let _ = out;
}

#[test]
fn linux_home_dir_path_is_flagged() {
    let fixture = format!("// path: /{}/{}/code\n", "home", "alice");
    let (ok, out) = run_against(&fixture);
    assert!(!ok, "Linux home-dir path should be flagged");
    let _ = out;
}

#[test]
fn email_pattern_is_flagged() {
    let fixture = format!("// contact: {}@{}\n", "alice", "example.com");
    let (ok, _out) = run_against(&fixture);
    assert!(!ok, "real email pattern should be flagged");
}

#[test]
fn noreply_email_is_allowed() {
    let (ok, out) = run_against("// 12345+madisonrickert@users.noreply.github.com\n");
    assert!(ok, "noreply.github.com emails should pass: {out}");
}
