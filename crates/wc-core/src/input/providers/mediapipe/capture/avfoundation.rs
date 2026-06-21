//! macOS webcam capture via `AVFoundation` on the maintained `objc2` framework
//! crates. Replaces nokhwa's `core-video-sys`/`objc 0.2` backend on macOS while
//! nokhwa keeps Linux/Windows. Frames arrive on a dispatch-queue delegate and
//! are drained by the worker through a single-slot [`LatestFrame`].
#![allow(dead_code)] // backend wired into `open_camera_source` in Task 7.

use super::Frame;

/// Single-slot latest-frame handoff: the `AVFoundation` delegate `store`s the
/// newest BGRA frame; the worker drains it via `take_into`/`consume`. Behind an
/// `Arc<Mutex<_>>` shared between the dispatch queue and the worker thread.
#[derive(Default)]
pub(super) struct LatestFrame {
    bgra: Vec<u8>,
    width: u32,
    height: u32,
    bytes_per_row: usize,
    /// Monotonic counter; a reader advances its own `last_gen` to this.
    generation: u64,
}

impl LatestFrame {
    /// Copy the newest BGRA frame in, reusing capacity (no realloc at steady
    /// size). Runs on the delegate's dispatch queue — a hot path; alloc-free.
    pub(super) fn store(&mut self, bgra: &[u8], width: u32, height: u32, bytes_per_row: usize) {
        self.bgra.clear();
        self.bgra.extend_from_slice(bgra);
        self.width = width;
        self.height = height;
        self.bytes_per_row = bytes_per_row;
        self.generation = self.generation.wrapping_add(1);
    }

    /// If a frame newer than `*last_gen` is present, repack it into `out`,
    /// advance `*last_gen`, and return `true`. Else return `false`.
    pub(super) fn take_into(&self, last_gen: &mut u64, out: &mut Frame) -> bool {
        if self.generation == *last_gen {
            return false;
        }
        out.width = self.width;
        out.height = self.height;
        bgra_to_rgb(
            &self.bgra,
            self.bytes_per_row,
            self.width,
            self.height,
            &mut out.rgb,
        );
        *last_gen = self.generation;
        true
    }

    /// Like `take_into` but skips the repack — the worker's over-budget drain.
    pub(super) fn consume(&self, last_gen: &mut u64) -> bool {
        if self.generation == *last_gen {
            return false;
        }
        *last_gen = self.generation;
        true
    }
}

/// Choose which enumerated capture device to open. Returns `Some(index)` when
/// `requested` is in range, or `None` to fall back to the system default video
/// device — parity with nokhwa's `open(camera_index)` graceful fallback.
pub(super) fn select_device_index(device_count: usize, requested: u32) -> Option<usize> {
    let idx = usize::try_from(requested).ok()?;
    (idx < device_count).then_some(idx)
}

/// Human-readable label for the negotiated capture format (dev-panel diagnostics).
pub(super) fn format_label(width: u32, height: u32, fps: u32) -> String {
    format!("{width}x{height} BGRA @{fps}")
}

/// Repack camera BGRA (byte order B,G,R,A, possibly row-padded so
/// `bytes_per_row >= width*4`) into tightly-packed RGB8 in `out`.
///
/// `out` is resized to `width*height*3` and reused across frames (the worker
/// owns it). Only the first `width*4` bytes of each row are pixel data; the
/// remainder up to `bytes_per_row` is stride padding and is skipped.
pub(super) fn bgra_to_rgb(
    bgra: &[u8],
    bytes_per_row: usize,
    width: u32,
    height: u32,
    out: &mut Vec<u8>,
) {
    let w = usize::try_from(width).unwrap_or(0);
    let h = usize::try_from(height).unwrap_or(0);
    out.clear();
    out.resize(w * h * 3, 0);
    for row in 0..h {
        let row_start = row * bytes_per_row;
        let src_row = &bgra[row_start..row_start + w * 4];
        let dst_row = &mut out[row * w * 3..(row + 1) * w * 3];
        for (px, rgb) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(3)) {
            rgb[0] = px[2]; // R
            rgb[1] = px[1]; // G
            rgb[2] = px[0]; // B
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Frame;
    use super::*;

    #[test]
    fn store_then_take_into_produces_rgb_once() {
        let mut slot = LatestFrame::default();
        slot.store(&[10, 20, 30, 255], 1, 1, 4);
        let mut last = 0u64;
        let mut out = Frame::default();
        assert!(
            slot.take_into(&mut last, &mut out),
            "first take sees new frame"
        );
        assert_eq!(out.width, 1);
        assert_eq!(out.rgb, vec![30, 20, 10]);
        assert!(!slot.take_into(&mut last, &mut out), "no new frame since");
    }

    #[test]
    fn consume_advances_without_repacking() {
        let mut slot = LatestFrame::default();
        slot.store(&[1, 2, 3, 255], 1, 1, 4);
        let mut last = 0u64;
        assert!(slot.consume(&mut last), "consume sees the stored frame");
        let mut out = Frame::default();
        assert!(
            !slot.take_into(&mut last, &mut out),
            "consume already advanced the generation"
        );
    }

    #[test]
    fn store_reuses_capacity() {
        let mut slot = LatestFrame::default();
        slot.store(&[1, 2, 3, 255], 1, 1, 4);
        let ptr = slot.bgra.as_ptr();
        slot.store(&[4, 5, 6, 255], 1, 1, 4);
        assert_eq!(slot.bgra.as_ptr(), ptr, "same size must not reallocate");
    }

    #[test]
    fn repacks_bgra_dropping_alpha_and_swapping_channels() {
        // 2x1 image, no padding. Pixel0 = B,G,R,A = 10,20,30,255 -> RGB 30,20,10.
        // Pixel1 = 40,50,60,128 -> RGB 60,50,40.
        let bgra = [10u8, 20, 30, 255, 40, 50, 60, 128];
        let mut out = Vec::new();
        bgra_to_rgb(&bgra, 8, 2, 1, &mut out);
        assert_eq!(out, vec![30, 20, 10, 60, 50, 40]);
    }

    #[test]
    fn skips_row_stride_padding() {
        // 1x2 image, bytes_per_row = 8 but width*4 = 4 (4 padding bytes/row).
        // Row0 px = 1,2,3,255 -> 3,2,1 ; padding 99,99,99,99 ignored.
        // Row1 px = 4,5,6,255 -> 6,5,4.
        let bgra = [1u8, 2, 3, 255, 99, 99, 99, 99, 4, 5, 6, 255, 88, 88, 88, 88];
        let mut out = Vec::new();
        bgra_to_rgb(&bgra, 8, 1, 2, &mut out);
        assert_eq!(out, vec![3, 2, 1, 6, 5, 4]);
    }

    #[test]
    fn reuses_buffer_capacity() {
        let bgra = [10u8, 20, 30, 255];
        let mut out = Vec::with_capacity(3);
        bgra_to_rgb(&bgra, 4, 1, 1, &mut out);
        let ptr = out.as_ptr();
        bgra_to_rgb(&bgra, 4, 1, 1, &mut out);
        assert_eq!(out.as_ptr(), ptr, "same dimensions must not reallocate");
    }

    #[test]
    fn device_index_in_range_is_selected() {
        assert_eq!(select_device_index(3, 0), Some(0));
        assert_eq!(select_device_index(3, 2), Some(2));
    }

    #[test]
    fn out_of_range_index_falls_back_to_default() {
        assert_eq!(select_device_index(3, 3), None);
        assert_eq!(select_device_index(0, 0), None);
    }

    #[test]
    fn format_label_reads_like_the_nokhwa_label() {
        assert_eq!(format_label(640, 480, 30), "640x480 BGRA @30");
    }
}
