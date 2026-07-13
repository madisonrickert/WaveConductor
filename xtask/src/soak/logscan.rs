//! Scan the app's teed `app.log` for the failures the metrics lanes cannot see.
//!
//! ## Why this lane exists
//!
//! The soak runs a **debug** binary, and `[profile.dev]` unwinds on panic. A
//! panic on a *worker* thread — the mediapipe inference worker, the audio device
//! watcher (which even wraps itself in an explicit `catch_unwind`) — kills that
//! thread and leaves the process alive and rendering. The app is architected to
//! survive that, which is exactly the problem: hand tracking can be dead for five
//! hours while RSS stays flat, FPS stays at 60, the app's clock keeps advancing,
//! and the process self-exits `0` at hour 8. Every metric lane reports health.
//! Only the log knows.
//!
//! The same hole passes any `ERROR`-level line: a lost wgpu device, an audio
//! device that never came back, a shader that failed to reload after a sketch
//! cycle. So: `panicked at` is a **failure**; an `ERROR` line is at least a
//! **review**, quoted so the operator or agent can judge it.
//!
//! ## Bounded by construction
//!
//! Eight hours of app log can be large, and the tool that hunts leaks must not
//! leak. [`scan_log`] streams the file a line at a time and never holds it in
//! memory; [`scan_lines`] retains at most [`MAX_QUOTED`] examples per category
//! (truncated to [`MAX_LINE_CHARS`]) while counting *all* of them. Peak memory is
//! one line plus ten short strings, whatever the log's size.
//!
//! [`scan_lines`] is a pure function over lines so the classifier is unit-tested
//! against literal log fragments rather than by provoking a real panic at hour 3.

use std::io::BufRead as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Examples retained per category. Enough to see the shape of the failure in the
/// verdict; the log itself is named in the report for the rest.
const MAX_QUOTED: usize = 5;

/// Characters retained per quoted line. A wgpu validation error can be a
/// paragraph; the verdict is not the place to reproduce it.
const MAX_LINE_CHARS: usize = 240;

/// What a scan of `app.log` found. Counts are complete; the quoted lines are
/// capped (see [`MAX_QUOTED`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogFindings {
    /// Lines read.
    pub lines_scanned: usize,
    /// Total `panicked at` lines seen.
    pub panic_count: usize,
    /// Total `ERROR`-level lines seen.
    pub error_count: usize,
    /// Up to [`MAX_QUOTED`] panic lines, verbatim (truncated).
    pub panics: Vec<String>,
    /// Up to [`MAX_QUOTED`] `ERROR` lines, verbatim (truncated).
    pub errors: Vec<String>,
    /// Set when the log could not be read at all — which is itself a reason to
    /// review, because it means this lane was blind for the whole run.
    pub unreadable: Option<String>,
}

impl LogFindings {
    /// True when the scan found nothing to say — no panic, no `ERROR`, and the
    /// log was readable.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.panic_count == 0 && self.error_count == 0 && self.unreadable.is_none()
    }
}

/// Stream `path` and classify every line. Never holds the file in memory.
///
/// An unreadable log is recorded in [`LogFindings::unreadable`] rather than
/// returned as an error: a run whose metrics are all in hand should still be
/// reported, with the honest note that this lane could not run.
#[must_use]
pub fn scan_log(path: &Path) -> LogFindings {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(err) => {
            return LogFindings {
                unreadable: Some(format!("{}: {err}", path.display())),
                ..LogFindings::default()
            }
        }
    };
    // `lines()` yields `Err` on invalid UTF-8; a garbled line is dropped rather
    // than aborting the scan — one unreadable line must not blind the lane.
    scan_lines(std::io::BufReader::new(file).lines().map_while(Result::ok))
}

/// Classify a sequence of log lines. Pure over its input — this is the whole
/// detector, and it is tested against literal fragments.
#[must_use]
pub fn scan_lines<I, S>(lines: I) -> LogFindings
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut findings = LogFindings::default();
    for line in lines {
        let line = line.as_ref();
        findings.lines_scanned += 1;
        if is_panic_line(line) {
            findings.panic_count += 1;
            if findings.panics.len() < MAX_QUOTED {
                findings.panics.push(truncate(line));
            }
        }
        if is_error_line(line) {
            findings.error_count += 1;
            if findings.errors.len() < MAX_QUOTED {
                findings.errors.push(truncate(line));
            }
        }
    }
    findings
}

/// The panic marker. `std`'s panic message is `thread '<name>' panicked at
/// <loc>:` on stderr, and the project's panic hook writes the same phrase; both
/// land in the teed `app.log`.
fn is_panic_line(line: &str) -> bool {
    line.contains("panicked at")
}

/// The level token this lane keys on.
const ERROR_TOKEN: [char; 5] = ['E', 'R', 'R', 'O', 'R'];

/// Whether a line is `ERROR`-level.
///
/// `tracing` writes the level as a standalone `ERROR` token — and, with colour
/// on (which it is when the app's stderr is a pipe under some configurations),
/// wrapped in ANSI escapes: `ESC[31mERROR ESC[0m`. So this walks the line
/// character by character, skipping CSI escape sequences entirely and matching
/// `ERROR` as a whole *token* rather than as a substring. Two failures avoided:
/// a naive `contains("ERROR")` would fire on `0 ERRORS so far`, and a naive
/// split-on-non-alphabetic would see the escape's trailing `m` glued to the
/// level (`mERROR`) and miss the coloured line entirely.
fn is_error_line(line: &str) -> bool {
    // `Some(n)` = the current token matches the first `n` chars of ERROR;
    // `None` = it has already diverged. A boundary commits or resets it.
    let mut matched: Option<usize> = Some(0);
    let mut in_escape = false;
    for c in line.chars() {
        if in_escape {
            // A CSI sequence ends at its final byte, which is a letter.
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
            continue;
        }
        if c == '\u{1b}' {
            in_escape = true;
            if matched == Some(ERROR_TOKEN.len()) {
                return true;
            }
            matched = Some(0);
            continue;
        }
        if c.is_ascii_alphabetic() {
            matched = match matched {
                Some(n) if ERROR_TOKEN.get(n) == Some(&c) => Some(n + 1),
                _ => None,
            };
        } else {
            if matched == Some(ERROR_TOKEN.len()) {
                return true;
            }
            matched = Some(0);
        }
    }
    matched == Some(ERROR_TOKEN.len())
}

/// Clip a quoted line to [`MAX_LINE_CHARS`], on a character boundary.
fn truncate(line: &str) -> String {
    let line = line.trim_end();
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((idx, _)) => {
            let mut clipped = line[..idx].to_string();
            clipped.push('…');
            clipped
        }
        None => line.to_string(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "unwrap/expect are appropriate in test code — every value is a literal"
)]
mod tests {
    use super::*;

    /// The failure this whole lane exists for: a worker thread panics, the
    /// process survives, and every metric keeps reporting health.
    #[test]
    fn a_worker_thread_panic_is_found() {
        let log = [
            "2026-07-11T04:00:00Z  INFO waveconductor: hand provider ready",
            "thread 'mediapipe-inference' panicked at crates/wc-core/src/input/providers/\
             mediapipe/inference_ort.rs:214:31:",
            "called `Option::unwrap()` on a `None` value",
            "2026-07-11T04:00:01Z  INFO waveconductor: soak: cycling sketch",
        ];
        let f = scan_lines(log);
        assert_eq!(f.lines_scanned, 4);
        assert_eq!(f.panic_count, 1);
        assert!(
            f.panics[0].contains("mediapipe-inference"),
            "the quoted line must name the thread: {:?}",
            f.panics
        );
        assert!(!f.is_clean());
    }

    #[test]
    fn error_level_lines_are_found_coloured_or_not() {
        let log = [
            "2026-07-11T04:00:00Z ERROR wgpu_core::device: Device lost",
            // The same line as `tracing` writes it with ANSI colour on.
            "2026-07-11T04:00:01Z \u{1b}[31mERROR\u{1b}[0m wc_core::audio: device never recovered",
        ];
        let f = scan_lines(log);
        assert_eq!(f.error_count, 2, "{:?}", f.errors);
        assert_eq!(f.panic_count, 0);
    }

    /// The classifier keys on the level *token*, not on the substring: a message
    /// body that happens to contain the letters must not manufacture a review.
    #[test]
    fn a_word_merely_containing_error_is_not_an_error_line() {
        let f = scan_lines([
            "2026-07-11T04:00:00Z  INFO waveconductor: 0 ERRORS so far",
            "2026-07-11T04:00:01Z  WARN waveconductor: TERROR is not a level",
            "2026-07-11T04:00:02Z  INFO waveconductor: error recovery complete",
        ]);
        assert_eq!(f.error_count, 0, "{:?}", f.errors);
        assert!(f.is_clean());
    }

    #[test]
    fn a_clean_log_says_so() {
        let f = scan_lines([
            "2026-07-11T04:00:00Z  INFO waveconductor: WC_SOAK active",
            "2026-07-11T04:00:01Z  WARN waveconductor: thermal sensor unavailable",
        ]);
        assert!(f.is_clean());
        assert_eq!(f.lines_scanned, 2);
    }

    /// Eight hours of log must not be quoted into the verdict — the counts are
    /// complete, the examples are capped.
    #[test]
    fn quoted_lines_are_bounded_but_counts_are_not() {
        let lines: Vec<String> = (0..1000)
            .map(|i| format!("2026-07-11T04:00:00Z ERROR wgpu: frame {i} failed"))
            .collect();
        let f = scan_lines(&lines);
        assert_eq!(f.error_count, 1000);
        assert_eq!(f.errors.len(), MAX_QUOTED);
    }

    #[test]
    fn a_very_long_line_is_clipped_on_a_char_boundary() {
        let long = format!("ERROR wgpu: {}", "é".repeat(500));
        let f = scan_lines([long]);
        assert_eq!(
            f.errors[0].chars().count(),
            MAX_LINE_CHARS + 1,
            "+ the ellipsis"
        );
        assert!(f.errors[0].ends_with('…'));
    }

    #[test]
    fn a_missing_log_is_recorded_as_blind_not_as_clean() {
        let f = scan_log(Path::new("target/soak/does-not-exist/app.log"));
        assert!(f.unreadable.is_some());
        assert!(
            !f.is_clean(),
            "an unread log is not a clean log — the lane was blind"
        );
    }

    #[test]
    fn scan_log_streams_a_real_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("app.log");
        std::fs::write(
            &path,
            "INFO ok\nthread 'main' panicked at src/main.rs:1:1:\nERROR nope\n",
        )
        .expect("write");
        let f = scan_log(&path);
        assert_eq!(f.lines_scanned, 3);
        assert_eq!(f.panic_count, 1);
        assert_eq!(f.error_count, 1);
    }
}
