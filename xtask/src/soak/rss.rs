//! Resident-set-size probe for the launched app, measured from *outside* the
//! process.
//!
//! ## Why from outside
//!
//! Reading one's own RSS portably means either a dependency (`sysinfo` pulls
//! ~30 crates — the workspace already declined it once, for the thermal sensor)
//! or `unsafe` mach/`/proc` FFI inside the render process. The launcher already
//! owns the child's PID, so it can ask the OS the same question with zero new
//! dependencies and zero code in the hot process: `ps` on unix, `tasklist` on
//! Windows, once per sample interval (default 30 s).
//!
//! The parsers are pure functions over the tools' output so they are unit-
//! testable without spawning anything.
//!
//! ## What this does not measure
//!
//! GPU memory. Neither `ps` nor `tasklist` sees it, and every portable route to
//! it is a new dependency or a platform API. RSS still catches the CPU-side
//! half of a leaked GPU resource (the wgpu handle, its bind group, and the
//! `Arc`s it holds), which is the shape of every GPU-resource leak this project
//! has actually fixed. A true VRAM watch stays a manual step: keep an eye on
//! the GPU tab of Activity Monitor / `nvtop` during the run.

use std::process::Command;

/// Sample the resident set size of `pid`, in KiB. `None` when the platform
/// probe is unavailable or the process has already exited.
#[must_use]
pub fn sample_rss_kib(pid: u32) -> Option<u64> {
    if cfg!(windows) {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .ok()?;
        parse_tasklist_rss_kib(&String::from_utf8_lossy(&out.stdout))
    } else {
        let out = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        parse_ps_rss_kib(&String::from_utf8_lossy(&out.stdout))
    }
}

/// Parse `ps -o rss= -p <pid>` output: a single number in KiB, padded with
/// whitespace. Empty output means the process is gone.
#[must_use]
pub fn parse_ps_rss_kib(out: &str) -> Option<u64> {
    out.split_whitespace().next()?.parse::<u64>().ok()
}

/// Parse `tasklist /FI "PID eq <pid>" /FO CSV /NH` output. The last CSV field
/// is the memory usage, e.g. `"123,456 K"` — comma-grouped kilobytes with a
/// trailing unit. A "no tasks" banner (or any row that doesn't parse) yields
/// `None`.
#[must_use]
pub fn parse_tasklist_rss_kib(out: &str) -> Option<u64> {
    let line = out.lines().find(|l| l.starts_with('"'))?;
    // Split on the quotes rather than the commas: the memory field's own
    // thousands separators are commas too, so a comma split would shred it.
    // The last non-blank quoted field is the memory usage.
    let mem = line.split('"').rfind(|s| !s.trim().is_empty())?;
    let digits: String = mem.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ps_output() {
        assert_eq!(parse_ps_rss_kib("  412345\n"), Some(412_345));
        assert_eq!(parse_ps_rss_kib("412345"), Some(412_345));
    }

    #[test]
    fn ps_output_for_a_dead_process_is_none() {
        assert_eq!(parse_ps_rss_kib(""), None);
        assert_eq!(parse_ps_rss_kib("\n"), None);
    }

    #[test]
    fn ps_garbage_is_none() {
        assert_eq!(parse_ps_rss_kib("not-a-number"), None);
    }

    #[test]
    fn parses_tasklist_csv_row_with_thousands_separators() {
        let row = "\"waveconductor.exe\",\"4242\",\"Console\",\"1\",\"412,345 K\"\r\n";
        assert_eq!(parse_tasklist_rss_kib(row), Some(412_345));
    }

    #[test]
    fn parses_tasklist_csv_row_without_separators() {
        let row = "\"waveconductor.exe\",\"4242\",\"Console\",\"1\",\"984 K\"\r\n";
        assert_eq!(parse_tasklist_rss_kib(row), Some(984));
    }

    #[test]
    fn tasklist_no_tasks_banner_is_none() {
        assert_eq!(
            parse_tasklist_rss_kib("INFO: No tasks are running which match the criteria.\r\n"),
            None
        );
    }
}
