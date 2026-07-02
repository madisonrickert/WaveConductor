//! `cargo xtask check-secrets` — regex-scan the working tree for forbidden
//! secrets and local absolute paths.
//!
//! Pattern coverage: unix/windows home-dir paths, email addresses, and four
//! secret-prefix shapes — AWS access key IDs (`AKIA…`), GitHub tokens
//! (`gh[pousr]_…`), `sk-` API keys, and bearer tokens. See [`FORBIDDEN`].
//!
//! The scan honors `.gitignore` / `.git/info/exclude` / the global gitignore
//! (via the `ignore` crate): gitignored files can't be committed, so a match
//! there would be a false positive. Hidden directories are NOT skipped wholesale
//! — committed `.github/` CI configs and `.cargo/config.toml` must still be
//! scanned — so the explicit [`SKIP_DIRS`] prune handles only `.git`, `target`,
//! `vendor`, other VCS/build/tool-cache directories, and the `docs/superpowers/`
//! dated planning archive (internal working scratchpad, never published). Living
//! `docs/` (`docs/adr`, `docs/runbooks`, README) and `tests/` are deliberately
//! scanned like any other tree — a real secret or home path committed under
//! either must be caught, not laundered through a skip list.
//!
//! Exits 0 when no findings; exits 1 when one or more findings are reported.

use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;
use ignore::WalkBuilder;
use regex::Regex;

use crate::util::json_escape;

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
        // Matched as substrings of the full match. `@2x.png` / `@3x.png` are
        // Apple's `.iconset` Retina naming convention (e.g. `icon_16x16@2x.png`),
        // not email addresses — the `@<scale>.<ext>` shape trips the heuristic.
        allowlist: &["noreply.github.com", "@2x.png", "@3x.png"],
    },
    Forbidden {
        label: "aws-access-key-id",
        // AWS access key IDs: literal `AKIA` prefix + 16 uppercase-alphanumeric chars.
        pattern: r"AKIA[A-Z0-9]{16}",
        allowlist: &[],
    },
    Forbidden {
        label: "github-token",
        // Modern GitHub token prefixes: personal/oauth/user-to-server/refresh
        // (`ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`), each followed by 36+ base62 chars.
        pattern: r"gh[pousr]_[A-Za-z0-9]{36,}",
        allowlist: &[],
    },
    Forbidden {
        label: "sk-api-key",
        // Generic `sk-`-prefixed secret-key shape (OpenAI/Stripe/etc convention).
        pattern: r"sk-[A-Za-z0-9]{20,}",
        allowlist: &[],
    },
    Forbidden {
        label: "bearer-token",
        // Case-insensitive "bearer" + whitespace + 20+ token chars (covers
        // base64url and dot-separated JWT shapes).
        pattern: r"(?i)bearer\s+[A-Za-z0-9\-_.]{20,}",
        allowlist: &[],
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
// `vendor/` holds git-tracked third-party code (e.g. the Ultraleap LICENSE with
// its public `legal@` contact) that is not ours to scrub — gitignore can't skip
// it because it is tracked, so it is pruned by name here.
//
// The remaining entries (`.git`, `target`, `node_modules`, etc.) are
// VCS/build/cache noise that should never be scanned. Most are also gitignored
// (and skipped on that basis), but pruning them by name avoids descending into
// them even when running with `--root` outside the repo.
//
// Living `docs/` (docs/adr, docs/runbooks, README) and `tests/` are deliberately
// NOT skipped: a real secret or home path committed under either must be caught,
// the same as anywhere else. Only the `docs/superpowers/` dated planning archive
// is pruned (the `superpowers` entry below) — it is internal working material
// full of illustrative example paths/emails/commit-trailer placeholders and is
// never published.
//
// Integration-test fixtures that plant a *positive* (matching) sample for this
// scanner to catch must therefore construct it at runtime (concatenation /
// `format!`) rather than embed the literal secret-shaped string in source — see
// the unit tests at the foot of this file for the pattern.
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
    "superpowers", // docs/superpowers/ dated planning archive (see comment above)
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::{Forbidden, Regex, FORBIDDEN};

    /// Look up a [`FORBIDDEN`] entry by label (test-only convenience).
    fn forbidden_by_label(label: &str) -> &'static Forbidden {
        FORBIDDEN
            .iter()
            .find(|f| f.label == label)
            .expect("test: label must exist in FORBIDDEN")
    }

    /// Compile `forbidden`'s pattern and report whether it fires on `haystack`,
    /// honoring the same allowlist logic `run()` applies.
    fn fires_on(forbidden: &Forbidden, haystack: &str) -> bool {
        let re = Regex::new(forbidden.pattern).expect("test: pattern must compile");
        // Bind the `any(...)` result to a local so the `Matches` iterator (which
        // borrows `re`) is dropped at the end of this statement, before `re`
        // itself; returning the borrow-dependent expression directly trips E0597.
        let fired = re.find_iter(haystack).any(|mat| {
            !forbidden
                .allowlist
                .iter()
                .any(|allow| mat.as_str().contains(allow))
        });
        fired
    }

    // NOTE: positive fixtures below are built at runtime (`format!`/`+`/`.repeat`)
    // rather than written as a single literal in source. `src/` and, as of this
    // change, `docs/`+`tests/` are all scanned by this same tool — a literal
    // secret-shaped string sitting in this file would make check-secrets flag
    // its own test fixtures. See the `SKIP_DIRS` doc comment above.

    #[test]
    fn unix_home_path_pattern() {
        let forbidden = forbidden_by_label("unix-home-path");
        let fixture = format!("/{}/{}/", "Users", "alice");
        assert!(fires_on(forbidden, &fixture));
        assert!(!fires_on(forbidden, "assets/shaders/line/render.wgsl"));
    }

    #[test]
    fn windows_home_path_pattern() {
        let forbidden = forbidden_by_label("windows-home-path");
        let fixture = format!("C:{}{}{}a", r"\", "Users", r"\");
        assert!(fires_on(forbidden, &fixture));
        assert!(!fires_on(forbidden, r"C:\Program Files\thing"));
    }

    #[test]
    fn email_address_pattern() {
        let forbidden = forbidden_by_label("email-address");
        let fixture = format!("{}@{}.{}", "someone", "example", "com");
        assert!(fires_on(forbidden, &fixture));
        assert!(!fires_on(forbidden, "not an email at all"));
        // Allowlisted bot/Retina-asset forms must not fire.
        let noreply = format!("{}@{}", "bot", "noreply.github.com");
        assert!(!fires_on(forbidden, &noreply));
        assert!(!fires_on(forbidden, "icon_16x16@2x.png"));
    }

    #[test]
    fn aws_access_key_id_pattern() {
        let forbidden = forbidden_by_label("aws-access-key-id");
        let fixture = format!("{}{}", "AKIA", "A".repeat(16));
        assert!(fires_on(forbidden, &fixture));
        assert!(!fires_on(forbidden, "AKIA-not-a-real-key"));
    }

    #[test]
    fn github_token_pattern() {
        let forbidden = forbidden_by_label("github-token");
        let fixture = format!("gh{}_{}", "p", "a".repeat(36));
        assert!(fires_on(forbidden, &fixture));
        assert!(!fires_on(forbidden, "ghz_not_a_real_prefix"));
    }

    #[test]
    fn sk_api_key_pattern() {
        let forbidden = forbidden_by_label("sk-api-key");
        let fixture = format!("sk-{}", "a".repeat(20));
        assert!(fires_on(forbidden, &fixture));
        assert!(!fires_on(forbidden, "sk-too-short"));
    }

    #[test]
    fn bearer_token_pattern() {
        let forbidden = forbidden_by_label("bearer-token");
        let fixture = format!("Authorization: {} {}", "Bearer", "a".repeat(20));
        assert!(fires_on(forbidden, &fixture));
        assert!(!fires_on(forbidden, "bearer short"));
    }
}
