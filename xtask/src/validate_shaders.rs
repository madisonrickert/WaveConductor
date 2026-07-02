//! `cargo xtask validate-shaders` — parse + validate the project's WGSL shaders
//! with `naga` (the same WGSL frontend wgpu and Bevy use), catching syntax and
//! type errors before they reach runtime pipeline creation.
//!
//! ## Scope: self-contained shaders
//!
//! Shaders that use Bevy's `#import` preprocessor (resolved by `naga_oil`, not
//! `naga`) cannot be validated standalone — the imported symbols are undefined
//! without composition. Those are SKIPPED here (and listed), and remain
//! runtime-validated when Bevy compiles them (`cargo rund` / `cargo xtask
//! capture`). Every shader with no `#import` is fully parsed and validated.
//!
//! Exits 0 when all validated shaders pass; exits 1 on any parse/validation error.

use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;
use ignore::WalkBuilder;

use crate::util::json_escape;

/// Arguments for the validate-shaders subcommand.
#[derive(ClapArgs)]
pub struct Args {
    /// Shader directory to scan (defaults to `<workspace>/assets/shaders`).
    #[arg(long)]
    pub root: Option<PathBuf>,

    /// Output results as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Validation outcome for one shader file.
enum Outcome {
    /// Parsed and validated cleanly.
    Validated,
    /// Uses Bevy `#import`; skipped (runtime-validated only).
    SkippedImport,
    /// Parse or validation error (rendered, source-annotated message).
    Failed(String),
}

/// Execute the validate-shaders subcommand.
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let root = args.root.unwrap_or_else(default_shader_root);

    let mut results: Vec<(PathBuf, Outcome)> = Vec::new();
    for entry in WalkBuilder::new(&root).build().filter_map(Result::ok) {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wgsl") {
            continue;
        }
        results.push((path.to_path_buf(), validate_one(path)));
    }
    results.sort_by(|a, b| a.0.cmp(&b.0));

    report(&root, &results, args.json);

    let failures = results
        .iter()
        .filter(|(_, outcome)| matches!(outcome, Outcome::Failed(_)))
        .count();
    if failures == 0 {
        Ok(())
    } else {
        Err(format!("{failures} shader(s) failed validation — see output above").into())
    }
}

/// Default shader root: `<workspace>/assets/shaders` (the xtask crate's parent).
fn default_shader_root() -> PathBuf {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").map_or_else(|_| PathBuf::from("."), PathBuf::from);
    manifest_dir
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
        .join("assets")
        .join("shaders")
}

/// Parse + validate one `.wgsl` file, or skip it when it uses Bevy `#import`.
fn validate_one(path: &Path) -> Outcome {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(format!("read error: {e}")),
    };
    // `#import` / `#define_import_path` are naga_oil preprocessor directives, not
    // WGSL — naga can't resolve the imported symbols standalone, so skip them.
    let uses_import = source.lines().any(|line| {
        matches!(
            line.split_whitespace().next(),
            Some("#import" | "#define_import_path")
        )
    });
    if uses_import {
        return Outcome::SkippedImport;
    }

    let module = match naga::front::wgsl::parse_str(&source) {
        Ok(module) => module,
        Err(e) => return Outcome::Failed(e.emit_to_string(&source)),
    };
    match naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    {
        Ok(_) => Outcome::Validated,
        Err(e) => Outcome::Failed(e.emit_to_string(&source)),
    }
}

/// Print per-file results (human table or JSON) and a summary line.
fn report(root: &Path, results: &[(PathBuf, Outcome)], json: bool) {
    let rel = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();

    if json {
        let items: Vec<String> = results
            .iter()
            .map(|(path, outcome)| {
                let (status, error) = match outcome {
                    Outcome::Validated => ("validated", String::new()),
                    Outcome::SkippedImport => ("skipped_import", String::new()),
                    Outcome::Failed(msg) => ("failed", msg.clone()),
                };
                format!(
                    "  {{\"file\": \"{}\", \"status\": \"{}\", \"error\": \"{}\"}}",
                    json_escape(&rel(path)),
                    status,
                    json_escape(&error),
                )
            })
            .collect();
        println!("[\n{}\n]", items.join(",\n"));
        return;
    }

    let (mut validated, mut skipped, mut failed) = (0_usize, 0_usize, 0_usize);
    for (path, outcome) in results {
        match outcome {
            Outcome::Validated => {
                validated += 1;
                println!("ok     {}", rel(path));
            }
            Outcome::SkippedImport => {
                skipped += 1;
                println!("skip   {}  (uses #import — runtime-validated)", rel(path));
            }
            Outcome::Failed(msg) => {
                failed += 1;
                eprintln!("FAIL   {}\n{}", rel(path), msg);
            }
        }
    }
    println!("\n{validated} validated, {skipped} skipped (#import), {failed} failed");
}
