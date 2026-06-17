//! In-app log capture buffer and viewer.
//!
//! [`LogBuffer`] is a bounded ring buffer of recent log records, shared between
//! a `tracing` layer (the writer — installed by the binary at startup, called
//! from any thread) and the dev panel's log view (the reader, on the main
//! thread). The binary owns the `tracing` integration because it owns the
//! subscriber setup; wc-core owns the buffer and the egui rendering so the dev
//! panel can show it.
//!
//! The reader **snapshots** (locks briefly, clones out, unlocks) rather than
//! holding the lock across rendering: egui code on the render thread can itself
//! emit a `tracing` event, and the writer is the same non-reentrant `Mutex`, so
//! holding it during a render would deadlock.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy_egui::egui;
use tracing::Level;

use crate::ui::OverlayStyle;

/// One captured log record.
#[derive(Clone, Debug)]
pub struct LogLine {
    /// Severity, used to colour the row.
    pub level: Level,
    /// Emitting module path (e.g. `wc_core::input`).
    pub target: String,
    /// Rendered `message` field of the event.
    pub message: String,
}

/// Bounded ring buffer of recent [`LogLine`]s.
///
/// Cloning is a refcount bump (the records live behind an `Arc<Mutex<…>>`), so
/// the writer layer holds one clone and the app holds the same buffer as a
/// resource. When full, the oldest record is dropped.
#[derive(Resource, Clone)]
pub struct LogBuffer {
    inner: Arc<Mutex<VecDeque<LogLine>>>,
    capacity: usize,
}

impl LogBuffer {
    /// Create an empty buffer that retains at most `capacity` records.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            capacity: capacity.max(1),
        }
    }

    /// Append a record, dropping the oldest if the buffer is at capacity.
    ///
    /// Cheap and bounded, but it does allocate the record's strings — never
    /// call it from a real-time path (the audio callback must not log). A
    /// poisoned lock is ignored (a log record is not worth a panic).
    pub fn push(&self, line: LogLine) {
        let Ok(mut buf) = self.inner.lock() else {
            return;
        };
        if buf.len() >= self.capacity {
            buf.pop_front();
        }
        buf.push_back(line);
    }

    /// Clone the most recent `max` records into `out` (cleared first), oldest
    /// first. Locks only for the copy, never across rendering.
    pub fn snapshot_recent(&self, max: usize, out: &mut Vec<LogLine>) {
        out.clear();
        let Ok(buf) = self.inner.lock() else {
            return;
        };
        let skip = buf.len().saturating_sub(max);
        out.extend(buf.iter().skip(skip).cloned());
    }
}

/// Render captured log lines as a colour-coded monospace list that sticks to
/// the newest entry. Reads from a pre-taken snapshot (see [`LogBuffer`]'s doc
/// for why the lock is not held here).
pub fn render_log_view(ui: &mut egui::Ui, lines: &[LogLine], style: &OverlayStyle) {
    if lines.is_empty() {
        ui.label(egui::RichText::new("(no log records yet)").color(style.text_faint));
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("wc-log-view")
        .max_height(220.0)
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for line in lines {
                // Wrap long records (paths, multi-clause messages) to the panel
                // width instead of overflowing the dock to the right.
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "{:>5} {}",
                            level_label(line.level),
                            line.message
                        ))
                        .monospace()
                        .size(11.0)
                        .color(level_color(line.level, style)),
                    )
                    .wrap(),
                );
            }
        });
}

/// Fixed-width severity tag for a log row.
fn level_label(level: Level) -> &'static str {
    match level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARN",
        Level::INFO => "INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
    }
}

/// Row colour by severity, from the dock palette.
fn level_color(level: Level, style: &OverlayStyle) -> egui::Color32 {
    match level {
        Level::ERROR => style.error_red,
        Level::WARN => style.warn_amber,
        Level::INFO => style.text_primary,
        Level::DEBUG | Level::TRACE => style.text_faint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(message: &str) -> LogLine {
        LogLine {
            level: Level::INFO,
            target: "test".to_owned(),
            message: message.to_owned(),
        }
    }

    #[test]
    fn push_drops_oldest_past_capacity() {
        let buf = LogBuffer::new(2);
        buf.push(line("a"));
        buf.push(line("b"));
        buf.push(line("c")); // evicts "a"
        let mut out = Vec::new();
        buf.snapshot_recent(10, &mut out);
        let msgs: Vec<&str> = out.iter().map(|l| l.message.as_str()).collect();
        assert_eq!(msgs, ["b", "c"], "oldest record is evicted at capacity");
    }

    #[test]
    fn snapshot_recent_returns_tail_oldest_first() {
        let buf = LogBuffer::new(10);
        for m in ["a", "b", "c", "d"] {
            buf.push(line(m));
        }
        let mut out = Vec::new();
        buf.snapshot_recent(2, &mut out);
        let msgs: Vec<&str> = out.iter().map(|l| l.message.as_str()).collect();
        assert_eq!(
            msgs,
            ["c", "d"],
            "only the newest `max`, in chronological order"
        );
    }

    #[test]
    fn snapshot_clears_the_output_first() {
        let buf = LogBuffer::new(10);
        buf.push(line("only"));
        let mut out = vec![line("stale")];
        buf.snapshot_recent(10, &mut out);
        assert_eq!(out.len(), 1, "prior contents are cleared, not appended to");
        assert_eq!(out[0].message, "only");
    }
}
