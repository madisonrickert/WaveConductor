//! `cargo xtask package-windows-msi` — build a Windows MSI from the staged
//! Windows app directory produced by `bundle-windows`.
//!
//! The staged folder (`target/dist/windows-x86_64/WaveConductor`) is the single
//! source of truth for what ships; this subcommand harvests it into an MSI by
//! invoking the `WiX` v3 toolset directly (`heat` → `candle` → `light`), never
//! re-deriving file contents. It expects to run on a Windows runner with `WiX`
//! v3 installed (that is where a Windows release binary and MSI are produced);
//! the version-mapping logic is host-independent and unit-tested on any host.

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

/// Resolve a `WiX` v3 tool path. `WiX` sets `%WIX%` to its install root and
/// ships tools under `%WIX%\bin`; fall back to the bare name (PATH) when unset.
fn wix_tool(name: &str) -> std::ffi::OsString {
    match std::env::var_os("WIX") {
        Some(root) => {
            let mut p = std::path::PathBuf::from(root);
            p.push("bin");
            p.push(format!("{name}.exe"));
            p.into_os_string()
        }
        None => std::ffi::OsString::from(name),
    }
}

/// Locate the VC++ v143 CRT merge module shipped with Visual Studio / Build Tools
/// on the build runner, via `vswhere`. Returns the `.msm` path. Windows-only in
/// practice (only called from `build_msi`, past the staged-dir guard), but compiles
/// on any host.
fn find_vc_redist_msm() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use std::path::PathBuf;
    let pf86 =
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".to_owned());
    let vswhere = PathBuf::from(pf86)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    let out = std::process::Command::new(&vswhere)
        .args(["-latest", "-property", "installationPath"])
        .output()
        .map_err(|e| {
            format!(
                "package-windows-msi: cannot run vswhere ({}): {e}",
                vswhere.display()
            )
        })?;
    if !out.status.success() {
        return Err("package-windows-msi: vswhere failed to locate Visual Studio".into());
    }
    let vs_path = String::from_utf8(out.stdout)?.trim().to_owned();
    // Merge modules live under VC\Redist\MSVC\<version>\MergeModules\. The version
    // dir varies; collect all matches and take the highest (lexicographic) one.
    let redist_root = PathBuf::from(&vs_path)
        .join("VC")
        .join("Redist")
        .join("MSVC");
    let mut candidates = Vec::new();
    // Match any CRT x64 merge module regardless of toolset number: the filename
    // is `Microsoft_VC<NNN>_CRT_x64.msm` where NNN tracks the VS version (VC143
    // for VS17, VC144 for VS18, ...). The wxs references the file by path
    // variable, not by name, so whichever version is installed works.
    if let Ok(versions) = std::fs::read_dir(&redist_root) {
        for ver in versions.flatten() {
            let merge_dir = ver.path().join("MergeModules");
            if let Ok(files) = std::fs::read_dir(&merge_dir) {
                for f in files.flatten() {
                    let name = f.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("Microsoft_VC") && name.ends_with("_CRT_x64.msm") {
                        candidates.push(f.path());
                    }
                }
            }
        }
    }
    candidates.sort();
    if let Some(found) = candidates.pop() {
        return Ok(found);
    }

    // Not found: enumerate what IS present under the redist root so the CI log
    // reports the real on-runner layout (turns a blind failure into ground truth).
    let mut listing = Vec::new();
    match std::fs::read_dir(&redist_root) {
        Ok(versions) => {
            for ver in versions.flatten() {
                let merge_dir = ver.path().join("MergeModules");
                match std::fs::read_dir(&merge_dir) {
                    Ok(files) => {
                        for f in files.flatten() {
                            listing.push(f.path().display().to_string());
                        }
                    }
                    Err(_) => {
                        listing.push(format!("{} (no MergeModules subdir)", ver.path().display()));
                    }
                }
            }
        }
        Err(e) => listing.push(format!("cannot read {}: {e}", redist_root.display())),
    }
    let present = if listing.is_empty() {
        "(empty)".to_owned()
    } else {
        listing.join("\n  ")
    };
    Err(format!(
        "package-windows-msi: no Microsoft_VC*_CRT_x64.msm under {}. Present:\n  {present}",
        redist_root.display()
    )
    .into())
}

/// Build the MSI by running the `WiX` v3 toolset directly: `heat` harvests the
/// staged app dir into a `HarvestedComponents` component group (`cargo wix`
/// does not run the directory harvester on its own), `candle` compiles the
/// committed product source plus the harvested fragment, and `light` links
/// them into the final MSI. `WiX` v3 must be installed (CI installs it via
/// `choco install wixtoolset`, which sets `%WIX%`).
fn build_msi(
    root: &Path,
    staged: &Path,
    version: &str,
    out_msi: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let wxs = root.join("wix").join("waveconductor.wxs");
    let build_dir = root.join("target").join("wix");
    std::fs::create_dir_all(&build_dir)?;

    let harvest_wxs = build_dir.join("harvest.wxs");
    let status = std::process::Command::new(wix_tool("heat"))
        .current_dir(root)
        .arg("dir")
        .arg(staged)
        .args(["-cg", "HarvestedComponents"])
        .args(["-dr", "INSTALLDIR"])
        .args(["-srd", "-scom", "-sreg", "-sfrag", "-gg"])
        .args(["-var", "env.WC_MSI_STAGED_DIR"])
        .arg("-out")
        .arg(&harvest_wxs)
        .env("WC_MSI_STAGED_DIR", staged)
        .status()?;
    if !status.success() {
        return Err(format!("package-windows-msi: `heat` failed with {status}").into());
    }

    let vc_msm = find_vc_redist_msm()?;

    let mut candle_out = build_dir.clone().into_os_string();
    candle_out.push(std::path::MAIN_SEPARATOR.to_string());
    let status = std::process::Command::new(wix_tool("candle"))
        .current_dir(root)
        .arg(format!("-dVersion={version}"))
        .arg(format!("-dVCRedistMsm={}", vc_msm.display()))
        .arg(&wxs)
        .arg(&harvest_wxs)
        .arg("-out")
        .arg(&candle_out)
        .env("WC_MSI_STAGED_DIR", staged)
        .status()?;
    if !status.success() {
        return Err(format!("package-windows-msi: `candle` failed with {status}").into());
    }

    let product_wixobj = build_dir.join("waveconductor.wixobj");
    let harvest_wixobj = build_dir.join("harvest.wixobj");
    let status = std::process::Command::new(wix_tool("light"))
        .current_dir(root)
        .arg(&product_wixobj)
        .arg(&harvest_wixobj)
        .arg("-out")
        .arg(out_msi)
        .status()?;
    if !status.success() {
        return Err(format!("package-windows-msi: `light` failed with {status}").into());
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
