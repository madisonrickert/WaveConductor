//! `cargo xtask check-secrets` — regex-scan the working tree for forbidden
//! secrets and local absolute paths.
//!
//! The scan honors `.gitignore` / `.git/info/exclude` / the global gitignore
//! (via the `ignore` crate): gitignored files can't be committed, so a match
//! there would be a false positive. Hidden directories are NOT skipped wholesale
//! — committed `.github/` CI configs and `.cargo/config.toml` must still be
//! scanned — so the explicit [`SKIP_DIRS`] prune handles `.git`, `target`,
//! `vendor`, `docs`, and `tests`.
//!
//! Exits 0 when no findings; exits 1 when one or more findings are reported.

use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;
use ignore::WalkBuilder;
use regex::Regex;

/// Arguments for the check-secrets subcommand.
#[derive(ClapArgs)]
pub struct Args {
    /// Root directory to scan (defaults to the workspace root via `CARGO_MANIFEST_DIR`
    /// or the current working directory when run as `cargo xtask`).
    #[arg(long)]
    pub root: Option<PathBuf>,

    /// Output findings as JSON.
    #[arg(long)]
    pub json: bool,
}

/// A single pattern that must not appear in committed files.
struct Forbidden {
    label: &'static str,
    pattern: &'static str,
    /// Patterns whose match strings are explicitly allowed (checked after the
    /// primary match to allow safe subsets like `noreply.github.com`).
    allowlist: &'static [&'static str],
}

const FORBIDDEN: &[Forbidden] = &[
    Forbidden {
        label: "unix-home-path",
        pattern: r"/(?:Users|home)/[A-Za-z][^/\s]{0,64}/",
        allowlist: &[],
    },
    Forbidden {
        label: "windows-home-path",
        // Match C:\Users\<name>\ (literal backslashes, possibly escaped in source)
        pattern: r"[A-Za-z]:[/\\]{1,2}Users[/\\]{1,2}[A-Za-z]",
        allowlist: &[],
    },
    Forbidden {
        label: "email-address",
        // Simple heuristic: local-part AT domain DOT tld (TLD ≥ 2 chars).
        pattern: r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}",
        // Allow no-reply bot addresses that appear in git metadata and CI.
        // Matched as substrings of the full email address.
        allowlist: &["noreply.github.com"],
    },
];

/// A finding produced by the scanner.
struct Finding {
    file: PathBuf,
    line: usize,
    label: &'static str,
    matched: String,
}

// Directories skipped during scanning.
//
// `docs/` is excluded because planning and design documents legitimately contain
// example forbidden patterns — commit-message templates cite bot email addresses,
// and test-fixture prose shows absolute home paths — used to describe this
// scanner's own behavior. Scanning docs would cause the scanner to flag itself.
//
// `tests/` is excluded because integration-test fixtures intentionally plant
// bad patterns to verify the scanner catches them.
//
// `vendor/` holds git-tracked third-party code (e.g. the Ultraleap LICENSE with
// its public `legal@` contact) that is not ours to scrub — gitignore can't skip
// it because it is tracked, so it is pruned by name here.
//
// The remaining entries (`.git`, `target`, `node_modules`, etc.) are
// VCS/build/cache noise that should never be scanned. Most are also gitignored
// (and skipped on that basis), but pruning them by name avoids descending into
// them even when running with `--root` outside the repo.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".cargo",
    ".venv",
    ".tox",
    ".mypy_cache",
    "__pycache__",
    "node_modules",
    "target",
    "vendor",
    "docs",
    "tests",
];

/// File extensions that are binary or generated and should be skipped.
const SKIP_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "svg", "ico", "wasm", "so", "dylib", "dll", "exe", "a", "pdf",
    "zip", "tar", "gz", "lock",
];

/// File extensions / directory names that are never source files and should be skipped.
fn should_skip(path: &Path) -> bool {
    // Skip well-known VCS/build directories by their exact directory name.
    for component in path.components() {
        if let std::path::Component::Normal(s) = component {
            let s = s.to_string_lossy();
            if SKIP_DIRS.iter().any(|&d| s == d) {
                return true;
            }
        }
    }
    // Skip binary / lock / generated files by extension.
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    SKIP_EXTS.contains(&ext)
}

/// Escape a string for inclusion as a JSON string value.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// Execute the check-secrets subcommand.
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let root = args.root.unwrap_or_else(|| {
        // When run as `cargo xtask`, CARGO_MANIFEST_DIR is the xtask crate;
        // go up one level to reach the workspace root.
        let manifest_dir =
            std::env::var("CARGO_MANIFEST_DIR").map_or_else(|_| PathBuf::from("."), PathBuf::from);
        manifest_dir
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });

    // Compile all patterns once.
    let compiled: Vec<(&Forbidden, Regex)> = FORBIDDEN
        .iter()
        .map(|f| {
            // Invariant: every entry in FORBIDDEN is a valid regex literal authored at
            // compile time; a compile failure here indicates a programming error, not a
            // runtime condition, so panicking is appropriate.
            #[allow(clippy::expect_used)]
            let re = Regex::new(f.pattern).expect("invariant: FORBIDDEN patterns are valid regex");
            (f, re)
        })
        .collect();

    let mut findings: Vec<Finding> = Vec::new();

    // Honor gitignore (defaults: `require_git = true`, so gitignore rules apply
    // only inside a repo — temp-dir tests scan everything). `hidden(false)`
    // keeps committed dotfiles (.github/, .cargo/config.toml) in scope; the
    // `filter_entry` prune drops the `SKIP_DIRS` directories outright.
    let walker = WalkBuilder::new(&root)
        .hidden(false)
        .filter_entry(|entry| {
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    return !SKIP_DIRS.contains(&name);
                }
            }
            true
        })
        .build();

    'file: for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue 'file;
        }
        let path = entry.path();
        if should_skip(path) {
            continue 'file;
        }

        let Ok(contents) = std::fs::read_to_string(path) else {
            continue 'file; // binary or unreadable — skip
        };

        for (line_idx, line_text) in contents.lines().enumerate() {
            for (forbidden, re) in &compiled {
                for mat in re.find_iter(line_text) {
                    let matched = mat.as_str().to_owned();
                    // Check allowlist: if any allowlist entry is a substring of the
                    // matched text, skip this finding.
                    if forbidden
                        .allowlist
                        .iter()
                        .any(|allow| matched.contains(allow))
                    {
                        continue;
                    }
                    findings.push(Finding {
                        file: path.to_path_buf(),
                        line: line_idx + 1,
                        label: forbidden.label,
                        matched: matched.clone(),
                    });
                }
            }
        }
    }

    if args.json {
        let json_findings: Vec<String> = findings
            .iter()
            .map(|f| {
                format!(
                    "  {{\"file\": \"{}\", \"line\": {}, \"label\": \"{}\", \"matched\": \"{}\"}}",
                    json_escape(&f.file.display().to_string()),
                    f.line,
                    f.label,
                    json_escape(&f.matched),
                )
            })
            .collect();
        println!("[\n{}\n]", json_findings.join(",\n"));
    } else {
        for f in &findings {
            eprintln!(
                "FINDING [{}] {}:{}: {:?}",
                f.label,
                f.file.display(),
                f.line,
                f.matched,
            );
        }
        let count = findings.len();
        if count == 0 {
            println!("{count} findings (PASS)");
        } else {
            eprintln!("{count} finding(s) (FAIL)");
        }
    }

    if findings.is_empty() {
        Ok(())
    } else {
        Err(format!("{} finding(s) — see output above", findings.len()).into())
    }
}
