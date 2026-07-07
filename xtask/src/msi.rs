//! `cargo xtask package-windows-msi` — build a Windows MSI from the staged
//! Windows app directory produced by `bundle-windows`.
//!
//! The staged folder (`target/dist/windows-x86_64/WaveConductor`) is the single
//! source of truth for what ships; this subcommand harvests it into an MSI via
//! the `WiX` Toolset (`cargo wix`), never re-deriving file contents. It expects to
//! run on a Windows runner with `WiX` v3 + `cargo-wix` installed (that is where a
//! Windows release binary and MSI are produced); the version-mapping logic is
//! host-independent and unit-tested on any host.

use std::path::Path;

use clap::Args as ClapArgs;

use crate::bundle::common;

/// The staged Windows app directory, relative to the workspace root.
const STAGED_REL: &str = "target/dist/windows-x86_64/WaveConductor";

/// Arguments for the package-windows-msi subcommand.
#[derive(ClapArgs)]
pub struct Args {
    /// Release tag to stamp into the MSI (e.g. `v5.0.0-alpha.4`). Defaults to the
    /// crate version (`0.0.0` form) when omitted.
    #[arg(long)]
    pub version: Option<String>,

    /// Emit machine-readable JSON instead of the human summary.
    #[arg(long)]
    pub json: bool,
}

/// Map a release tag to a legal MSI `ProductVersion` (`a.b.c.d`, each ≤ 255).
///
/// - Strips a leading `v`.
/// - `MAJOR.MINOR.PATCH` maps to `MAJOR.MINOR.PATCH.0`.
/// - `MAJOR.MINOR.PATCH-<label>.N` maps to `MAJOR.MINOR.PATCH.N`.
///
/// Errors on malformed input or any field > 255.
pub fn msi_version(tag: &str) -> Result<String, String> {
    let tag = tag.strip_prefix('v').unwrap_or(tag);
    let (core, pre) = match tag.split_once('-') {
        Some((core, label)) => (core, Some(label)),
        None => (tag, None),
    };
    let mut fields: Vec<u32> = Vec::new();
    for part in core.split('.') {
        let n: u32 = part
            .parse()
            .map_err(|_| format!("msi_version: non-numeric field in '{tag}'"))?;
        fields.push(n);
    }
    if fields.len() != 3 {
        return Err(format!(
            "msi_version: expected MAJOR.MINOR.PATCH core, got '{core}'"
        ));
    }
    let fourth: u32 = match pre {
        Some(label) => label
            .rsplit('.')
            .next()
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| format!("msi_version: no numeric suffix in prerelease '{tag}'"))?,
        None => 0,
    };
    fields.push(fourth);
    if let Some(bad) = fields.iter().find(|&&f| f > 255) {
        return Err(format!("msi_version: field {bad} exceeds 255 in '{tag}'"));
    }
    Ok(fields
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join("."))
}

/// Resolve the tag to use: explicit `--version`, else the xtask crate version.
fn resolve_tag(explicit: Option<&str>) -> String {
    explicit.map_or_else(|| env!("CARGO_PKG_VERSION").to_owned(), ToOwned::to_owned)
}

/// Execute the package-windows-msi subcommand.
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let root = common::workspace_root();
    let staged = root.join(STAGED_REL);
    if !staged.is_dir() {
        return Err(format!(
            "package-windows-msi: staged dir not found at {}; run `cargo xtask bundle-windows` first",
            staged.display()
        )
        .into());
    }

    let tag = resolve_tag(args.version.as_deref());
    let version = msi_version(&tag)?;
    let out_msi = root.join(format!("WaveConductor-{tag}-x86_64.msi"));

    build_msi(&root, &staged, &version, &out_msi)?;

    let size = std::fs::metadata(&out_msi).map_or(0, |m| m.len());
    if args.json {
        println!(
            "{{\"msi\":\"{}\",\"version\":\"{version}\",\"size_bytes\":{size}}}",
            out_msi.display()
        );
    } else {
        println!("MSI written: {}", out_msi.display());
        println!("  version   {version}");
        println!("  size      {size} bytes");
    }
    Ok(())
}

/// Invoke `cargo wix` against the committed `WiX` source, harvesting the staged
/// dir. `cargo-wix` + `WiX` v3 must be on PATH (CI installs them).
fn build_msi(
    root: &Path,
    staged: &Path,
    version: &str,
    out_msi: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let wxs = root.join("wix").join("waveconductor.wxs");
    let status = std::process::Command::new("cargo")
        .current_dir(root)
        .args(["wix", "--no-build", "--nocapture"])
        .args(["--install-version", version])
        .args(["--output".as_ref(), out_msi.as_os_str()])
        .args(["--include".as_ref(), wxs.as_os_str()])
        .env("WC_MSI_STAGED_DIR", staged)
        .status()?;
    if !status.success() {
        return Err(format!("package-windows-msi: `cargo wix` failed with {status}").into());
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "expect is appropriate in test scaffolding"
)]
mod tests {
    use super::*;

    #[test]
    fn maps_prerelease_tag_to_four_field_numeric() {
        assert_eq!(msi_version("v5.0.0-alpha.4").expect("map"), "5.0.0.4");
    }

    #[test]
    fn maps_plain_release_tag_to_zero_fourth_field() {
        assert_eq!(msi_version("v5.0.0").expect("map"), "5.0.0.0");
        assert_eq!(msi_version("5.1.2").expect("map"), "5.1.2.0");
    }

    #[test]
    fn rejects_field_over_255() {
        assert!(msi_version("v5.0.0-alpha.300").is_err());
        assert!(msi_version("v5.0.256").is_err());
    }

    #[test]
    fn rejects_malformed_tag() {
        assert!(msi_version("v5.0").is_err());
        assert!(msi_version("nonsense").is_err());
    }
}
