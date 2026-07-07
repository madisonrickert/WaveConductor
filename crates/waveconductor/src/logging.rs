//! On-disk logging for release builds.
//!
//! Release builds suppress the console (`windows_subsystem = "windows"`), so
//! stderr goes nowhere; combined with `panic = "abort"` a field crash would be
//! silent. This module adds a rolling on-disk log under the per-user local-data
//! dir and a panic hook, so a deployed build is diagnosable. The writer is
//! non-blocking (file I/O runs on a background thread) to keep disk flushes off
//! the frame/render path, per the "never block in a hot path" rule in AGENTS.md.
//!
//! Cross-platform on purpose: an on-disk log is useful on the macOS deployment
//! too, and keeping one code path means it is covered by the normal test surface
//! rather than a Windows-only branch.

use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};

/// Join the log directory (`<base>/WaveConductor/logs`) onto a data-root base.
///
/// Pure so it can be unit-tested without touching the real `dirs::data_local_dir`.
fn log_dir_in(base: &Path) -> PathBuf {
    base.join("WaveConductor").join("logs")
}

/// Resolve the on-disk log directory: `<data_local_dir>/WaveConductor/logs`,
/// falling back to `./logs` when the platform exposes no local-data dir.
pub fn log_dir() -> PathBuf {
    dirs::data_local_dir().map_or_else(|| PathBuf::from("logs"), |base| log_dir_in(&base))
}

/// Build the non-blocking rolling-file writer and its flush guard.
///
/// Returns `None` (logging to a file is best-effort; stderr and the in-memory
/// buffer still work) if the directory can't be created or the appender can't be
/// built. Rotation is daily with a 7-file retention window.
///
/// The returned [`WorkerGuard`] MUST be held for the process lifetime; dropping
/// it flushes and stops the background writer.
pub fn file_writer() -> Option<(NonBlocking, WorkerGuard)> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("waveconductor")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir)
        .ok()?;
    Some(tracing_appender::non_blocking(appender))
}

/// Format one panic log line. Pure for testability.
fn format_panic(payload: &str, location: Option<&str>) -> String {
    match location {
        Some(loc) => format!("PANIC at {loc}: {payload}"),
        None => format!("PANIC: {payload}"),
    }
}

/// Install a panic hook that appends the panic to `<log_dir>/panic.log`
/// synchronously (before the default hook runs and, in release, the process
/// aborts), then delegates to the previous hook.
pub fn install_panic_hook() {
    let dir = log_dir();
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_owned());
        let location = info.location().map(ToString::to_string);
        let line = format_panic(&payload, location.as_deref());
        if std::fs::create_dir_all(&dir).is_ok() {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("panic.log"))
            {
                let _ = writeln!(f, "{line}");
            }
        }
        default(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn log_dir_in_nests_under_app_and_logs() {
        let got = log_dir_in(Path::new("/base"));
        assert!(got.ends_with("WaveConductor/logs"), "got {got:?}");
        assert!(got.starts_with("/base"), "got {got:?}");
    }

    #[test]
    fn format_panic_includes_location_when_present() {
        let line = format_panic("boom", Some("src/main.rs:12:5"));
        assert_eq!(line, "PANIC at src/main.rs:12:5: boom");
    }

    #[test]
    fn format_panic_omits_location_when_absent() {
        let line = format_panic("boom", None);
        assert_eq!(line, "PANIC: boom");
    }
}
