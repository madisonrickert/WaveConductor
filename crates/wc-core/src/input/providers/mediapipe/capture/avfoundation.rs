//! macOS webcam capture via `AVFoundation` on the maintained `objc2` framework
//! crates. Replaces nokhwa's `core-video-sys`/`objc 0.2` backend on macOS while
//! nokhwa keeps Linux/Windows. Frames arrive on a dispatch-queue delegate and
//! are drained by the worker through a single-slot [`LatestFrame`].
#![allow(dead_code)] // backend wired into `open_camera_source` in Task 7.

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
    use super::*;

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
}
