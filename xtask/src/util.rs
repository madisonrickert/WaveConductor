//! Small helpers shared by more than one xtask subcommand.
//!
//! Two families live here:
//!
//! - **JSON escaping.** Each subcommand hand-emits its own JSON (rather than
//!   depending on a JSON value tree crate for a handful of flat objects), so
//!   the one genuinely shared piece is string escaping. Centralized here so
//!   `check-secrets`, `validate-shaders`, and `manifest` can't drift out of
//!   sync on quoting rules the way the pre-extraction copies did.
//! - **Launching the app.** `capture` and `soak-test` both drive the pre-built
//!   debug `waveconductor` binary with env vars and tee its output to a log.
//!   The binary resolution (with its fail-fast build directive), the
//!   stale-build warning, the git provenance stamp, and the log tee are shared
//!   here so the two subcommands cannot drift apart on the operator-facing
//!   behaviour that matters most: what happens when the binary isn't built.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Escape a string for inclusion as a JSON string value.
///
/// Handles the characters that are illegal unescaped inside a JSON string
/// (`"`, `\`, and the C0 control characters) plus the common `\n`/`\r`/`\t`
/// shorthands; everything else (including all non-ASCII `char`s) passes
/// through unchanged, which is valid per the JSON spec.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// Workspace root: parent of the xtask crate dir (`CARGO_MANIFEST_DIR`).
pub fn workspace_root() -> PathBuf {
    std::env::var("CARGO_MANIFEST_DIR")
        .ok()
        .and_then(|d| PathBuf::from(d).parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Directory Cargo writes build artifacts to: `$CARGO_TARGET_DIR` when set,
/// otherwise `<root>/target`.
pub fn target_dir(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from)
}

/// Path to the debug `waveconductor` binary within `target_dir`, with the
/// platform executable suffix (e.g. `.exe` on Windows).
pub fn app_binary_path(target_dir: &Path) -> PathBuf {
    target_dir
        .join("debug")
        .join(format!("waveconductor{}", std::env::consts::EXE_SUFFIX))
}

/// Resolve the pre-built debug `waveconductor` binary under `<root>`'s target
/// dir, or fail fast with a directive to build it. `tool` names the calling
/// subcommand so the error reads in its voice (`capture: …` / `soak-test: …`).
///
/// Neither subcommand builds the app itself: building is a separate, watchable
/// step the operator (or a coding agent) runs and observes. Folding a cold,
/// minutes-long `cargo run` build into the launch step would let a wall-clock
/// timeout safety net fire *during the build*, reported misleadingly as "app
/// did not exit" when the app never started. Requiring a pre-built binary keeps
/// the launch fast, bounded, and predictable.
pub fn resolve_built_binary(
    root: &Path,
    tool: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    resolve_built_binary_in(&target_dir(root), tool)
}

/// [`resolve_built_binary`] against an explicit target dir (split out so the
/// fail-fast path is testable without depending on `$CARGO_TARGET_DIR`).
pub fn resolve_built_binary_in(
    target_dir: &Path,
    tool: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let bin = app_binary_path(target_dir);
    if bin.is_file() {
        return Ok(bin);
    }
    Err(format!(
        "{tool}: the waveconductor binary is not built at {}.\n       \
         Build it first (a separate, watchable step), then re-run {tool}:\n       \
         cargo build -p waveconductor",
        bin.display()
    )
    .into())
}

/// Newest modification time among `.rs` / `.wgsl` files under `crates/` and
/// `assets/shaders/` — the source that affects the app's behaviour. `None` if
/// neither tree has such a readable file.
fn newest_source_mtime(root: &Path) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    for dir in [root.join("crates"), root.join("assets").join("shaders")] {
        if !dir.exists() {
            continue;
        }
        for entry in ignore::WalkBuilder::new(&dir)
            .build()
            .filter_map(Result::ok)
        {
            let ext = entry.path().extension().and_then(std::ffi::OsStr::to_str);
            if !matches!(ext, Some("rs" | "wgsl")) {
                continue;
            }
            if let Some(mtime) = entry.metadata().ok().and_then(|m| m.modified().ok()) {
                newest = Some(newest.map_or(mtime, |n| n.max(mtime)));
            }
        }
    }
    newest
}

/// Warn (non-fatally) when the built binary is older than the newest source
/// under `crates/` / `assets/shaders/` — i.e. it may not reflect current code.
/// The build being a separate step (see [`resolve_built_binary`]) means the
/// operator owns rebuilds; this catches the "edited but forgot to rebuild" case
/// without blocking the run.
pub fn warn_if_stale(binary: &Path, root: &Path) {
    let Some(bin_mtime) = binary.metadata().ok().and_then(|m| m.modified().ok()) else {
        return;
    };
    if let Some(src_mtime) = newest_source_mtime(root) {
        if src_mtime > bin_mtime {
            eprintln!(
                "warning: {} is older than source under crates/ or assets/shaders/ — this run \
                 may use a stale build. Rebuild with `cargo build -p waveconductor`.",
                binary.display()
            );
        }
    }
}

/// Resolve the short git commit hash for artifact provenance. Returns `None`
/// when git is unavailable or this is not a repository — the run still works.
pub fn git_short_commit(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8(output.stdout).ok()?;
    let hash = hash.trim();
    if hash.is_empty() {
        None
    } else {
        Some(hash.to_string())
    }
}

/// Drain a spawned child's stdout+stderr into `log_path`, returning the join
/// handles. Separate threads per pipe avoid the classic pipe-buffer deadlock
/// (a child blocked writing stderr while the parent waits on stdout).
///
/// The child must have been spawned with both pipes set to `Stdio::piped()`.
pub fn spawn_log_tee(
    child: &mut Child,
    log_path: &Path,
) -> Result<Vec<JoinHandle<()>>, Box<dyn std::error::Error>> {
    let log = Arc::new(Mutex::new(std::fs::File::create(log_path)?));
    // `stdout` and `stderr` are distinct concrete reader types, so box each as
    // `dyn Read` to drain them through the same loop.
    let mut pipes: Vec<Box<dyn std::io::Read + Send>> = Vec::new();
    if let Some(out) = child.stdout.take() {
        pipes.push(Box::new(out));
    }
    if let Some(err) = child.stderr.take() {
        pipes.push(Box::new(err));
    }
    let mut handles = Vec::new();
    for mut reader in pipes {
        let log = Arc::clone(&log);
        handles.push(std::thread::spawn(move || {
            use std::io::Read as _;
            use std::io::Write as _;
            let mut buf = [0_u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut f) = log.lock() {
                    let _ = f.write_all(&buf[..n]);
                }
            }
        }));
    }
    Ok(handles)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;

    #[test]
    fn app_binary_path_adds_platform_exe_suffix() {
        let p = app_binary_path(Path::new("/ws/target"));
        let expected = PathBuf::from(format!(
            "/ws/target/debug/waveconductor{}",
            std::env::consts::EXE_SUFFIX
        ));
        assert_eq!(p, expected);
    }

    #[test]
    fn resolve_built_binary_fails_fast_when_absent() {
        // A target dir with no debug/waveconductor: the caller must refuse
        // rather than silently build (or time out mid-build).
        let tmp = tempfile::tempdir().expect("tempdir");
        let err =
            resolve_built_binary_in(tmp.path(), "soak-test").expect_err("absent binary must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("not built"),
            "message names the problem: {msg}"
        );
        assert!(
            msg.contains("cargo build -p waveconductor"),
            "message gives the exact fix: {msg}"
        );
        assert!(
            msg.contains("soak-test:"),
            "message speaks in the calling subcommand's voice: {msg}"
        );
    }

    #[test]
    fn escapes_quotes_and_backslashes() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn escapes_common_whitespace_shorthands() {
        assert_eq!(json_escape("a\nb\rc\td"), "a\\nb\\rc\\td");
    }

    #[test]
    fn escapes_other_control_characters_as_unicode_sequences() {
        assert_eq!(json_escape("a\u{0001}b"), "a\\u0001b");
    }

    #[test]
    fn passes_through_plain_ascii_and_unicode_unchanged() {
        assert_eq!(json_escape("plain text"), "plain text");
        assert_eq!(json_escape("emoji: 🎛"), "emoji: 🎛");
    }
}
