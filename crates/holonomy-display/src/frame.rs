//! A frame of pixels: `0x00RRGGBB` per pixel, top-left origin.
//!
//! # Why a `u32` per pixel
//!
//! One `u32` is one pixel, so a row is `&mut [u32]`, a rectangle is a row range plus a column range,
//! and `blit_glyph` in `holonomy-assets` writes straight into it with no conversion. The alternative --
//! packed `RGB888` bytes -- saves nothing measurable and costs an alignment assumption at every call
//! site, which is the kind of thing that is wrong on the framebuffer's first row and not on the second.
//!
//! # The byte order, and why it is the one that is not ARGB
//!
//! `0x00RRGGBB` puts red in the most significant byte. Linux DRM dumb buffers and the `xRGB8888`
//! fbdev format are both little-endian `u32` with red high, so this *is* the framebuffer's order -- a
//! `memcpy` to a dumb buffer needs no byte swap. PPM, which the headless backend writes, is
//! byte-ordered `R,G,B` in that order, which also needs no swap. The one format that would have been
//! convenient and wrong is `0xAABBGGRR`, which is what a naive little-endian `u32` gives you for a
//! red/green/blue triple; [`Frame::to_ppm`] documents where that trap is.
//!
//! # The alpha byte
//!
//! Always zero and never read. It exists to make the word a whole number of bytes on every platform and
//! to leave the high byte free for a backend that wants a real alpha (a `DRM_FORMAT_ARGB8888` dumb
//! buffer would). Nothing in the renderer writes it, and [`Frame::pixel`] masks it off so a caller that
//! writes one cannot change what a later reader sees.

use std::io::{self, Write};

/// Bytes per pixel: three colour bytes plus one the renderer leaves alone.
pub const PIXEL_BYTES: usize = 4;

/// The pixel layouts a [`Frame`] can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// `0x00RRGGBB`. The framebuffer's own order, and what [`Frame`] stores.
    Rgb888x,
}

/// Why a frame operation failed.
#[derive(Debug)]
pub enum FrameError {
    /// The destination was the wrong size for the source.
    SizeMismatch {
        /// What the source had.
        want: (u32, u32),
        /// What the destination had.
        got: (u32, u32),
    },
    /// A rectangle ran off the frame.
    OutOfBounds {
        /// The rectangle.
        rect: (i64, i64, u32, u32),
        /// The frame's size.
        size: (u32, u32),
    },
    /// The sink failed.
    Io(io::Error),
    /// A backend rejected the frame for a reason it could not express otherwise.
    ///
    /// Carries `errno` where there is one, which is how a short `MAP_DUMB` write is reported.
    Backend(i32),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SizeMismatch { want, got } => {
                write!(f, "frame is {want:?}, destination is {got:?}")
            }
            Self::OutOfBounds { rect, size } => {
                let (x, y, w, h) = rect;
                write!(f, "rectangle {x},{y} {w}x{h} runs off a {size:?} frame")
            }
            Self::Io(e) => write!(f, "writing a frame: {e}"),
            Self::Backend(e) => write!(f, "the scanout backend failed: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl PartialEq for FrameError {
    /// Compares by variant and payload where the payload is comparable, and treats two `Io` errors as
    /// equal when their `ErrorKind`s match.
    ///
    /// Needed because the tests assert on `Err(FrameError::...)` values, and `io::Error` is not
    /// `PartialEq`. Comparing `kind()` rather than the whole error is the right granularity: the kind
    /// is what a caller can act on.
    fn eq(&self, other: &Self) -> bool {
        use FrameError::*;
        match (self, other) {
            (SizeMismatch { want: a1, got: b1 }, SizeMismatch { want: a2, got: b2 }) => {
                a1 == a2 && b1 == b2
            }
            (OutOfBounds { rect: a1, size: b1 }, OutOfBounds { rect: a2, size: b2 }) => {
                a1 == a2 && b1 == b2
            }
            (Io(a), Io(b)) => a.kind() == b.kind(),
            (Backend(a), Backend(b)) => a == b,
            _ => false,
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// A frame: a width, a height, and `width * height` pixels.
///
/// A plain `Vec<u32>` rather than anything cleverer. The renderer's damage model already avoids touching
/// most of a frame, so a background buffer buys nothing, and inside the jail every allocation is
/// expensive -- a frame is one allocation, taken in [`Frame::new`], before anything else is measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

impl Frame {
    /// A frame filled with `fill`.
    ///
    /// `fill` is masked to `0x00RRGGBB`, so an alpha byte in the constant does not leak into the frame --
    /// which matters because a caller writing `0xFF_00_00_00` for "opaque red" would otherwise get a
    /// frame whose high byte was set, and every later equality check and PPM dump would differ.
    pub fn new(width: u32, height: u32, fill: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![fill & 0x00FF_FFFF; (width as usize) * (height as usize)],
        }
    }

    /// A frame of the given size, filled with black.
    pub fn black(width: u32, height: u32) -> Self {
        Self::new(width, height, 0)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// `(width, height)`.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Every pixel, row-major from the top left.
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Every pixel, mutable.
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
    }

    /// The whole frame's bytes, in the framebuffer's order.
    ///
    /// Native-endian, so a `MAP_DUMB` buffer on a little-endian target is a straight `memcpy`. `PPM`
    /// wants the other order and [`to_ppm`](Self::to_ppm) does that conversion.
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck_bytes(&self.pixels)
    }

    /// The pixel at `(x, y)`, masked to `0x00RRGGBB`.
    ///
    /// Out of bounds gives black rather than panicking: a renderer's last rectangle routinely reaches
    /// one pixel past the edge of a clipped area, and a bounds check that panics turns a two-pixel
    /// rounding difference into a crash.
    #[inline]
    pub fn pixel(&self, x: u32, y: u32) -> u32 {
        match self.pixels.get(self.index(x, y)) {
            Some(p) => p & 0x00FF_FFFF,
            None => 0,
        }
    }

    /// Set the pixel at `(x, y)`, ignoring anything outside the frame.
    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, colour: u32) {
        let at = self.index(x, y);
        if let Some(p) = self.pixels.get_mut(at) {
            *p = colour & 0x00FF_FFFF;
        }
    }

    /// The row at `y`, or an empty slice if `y` is off the frame.
    #[inline]
    pub fn row(&self, y: u32) -> &[u32] {
        let w = self.width as usize;
        match self.pixels.get(y as usize * w..y as usize * w + w) {
            Some(r) => r,
            None => &[],
        }
    }

    /// The row at `y`, mutable.
    #[inline]
    pub fn row_mut(&mut self, y: u32) -> &mut [u32] {
        let w = self.width as usize;
        if y as usize * w + w <= self.pixels.len() {
            &mut self.pixels[y as usize * w..y as usize * w + w]
        } else {
            &mut []
        }
    }

    /// Fill a rectangle, clipped to the frame.
    ///
    /// Returns how many pixels were written, which is `width * height` after clipping. A caller that
    /// wanted the whole rectangle will notice a short count.
    pub fn fill_rect(&mut self, x: i64, y: i64, width: u32, height: u32, colour: u32) -> u64 {
        // Clip on both edges independently. `clamp` on the *end* matters as much as on the start: a
        // rectangle that starts inside and runs off the right would otherwise produce `x1 > width` and
        // index past the end, which is the panic a renderer hits the first time a glyph's advance is
        // rounded up.
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = (x + i64::from(width)).clamp(0, i64::from(self.width)) as u32;
        let y1 = (y + i64::from(height)).clamp(0, i64::from(self.height)) as u32;
        if x1 <= x0 || y1 <= y0 {
            return 0;
        }
        let colour = colour & 0x00FF_FFFF;
        let w = self.width as usize;
        let span = (x1 - x0) as usize;
        for yy in y0..y1 {
            let a = yy as usize * w + x0 as usize;
            self.pixels[a..a + span].fill(colour);
        }
        u64::from(x1 - x0) * u64::from(y1 - y0)
    }

    /// Copy `other` in at `(x, y)`, clipped.
    ///
    /// Returns how many pixels were written.
    pub fn blit(&mut self, other: &Frame, x: i64, y: i64) -> u64 {
        // A negative destination offset means the source's top-left corner is off-frame. So the source
        // has to be clipped by `-x`/`-y` and the destination is clamped at 0 -- which is the case that
        // silently drops the wrong half of an image if only one of the two is handled.
        let dx = x.max(0) as u32;
        let dy = y.max(0) as u32;
        let sx = (-x).max(0) as u32;
        let sy = (-y).max(0) as u32;
        let w = other
            .width
            .min(self.width.saturating_sub(dx))
            .saturating_sub(sx);
        let h = other
            .height
            .min(self.height.saturating_sub(dy))
            .saturating_sub(sy);
        if w == 0 || h == 0 {
            return 0;
        }
        let ow = other.width as usize;
        let dw = self.width as usize;
        for row in 0..h {
            let from = (sy + row) as usize * ow + sx as usize;
            let to = (dy + row) as usize * dw + dx as usize;
            self.pixels[to..to + w as usize]
                .copy_from_slice(&other.pixels[from..from + w as usize]);
        }
        u64::from(w) * u64::from(h)
    }

    /// How many pixels differ from `other`, and the first place they do.
    ///
    /// The comparison a visual baseline needs: "is this frame the one the baseline recorded", answered
    /// as a count and a location rather than a boolean, so a failure says *where*.
    ///
    /// A size mismatch is an error rather than a count, because two frames of different sizes have no
    /// meaningful difference.
    pub fn diff(&self, other: &Frame) -> Result<Diff, FrameError> {
        if self.size() != other.size() {
            return Err(FrameError::SizeMismatch {
                want: self.size(),
                got: other.size(),
            });
        }
        let mut differing = 0u64;
        let mut first = None;
        for (i, (a, b)) in self.pixels.iter().zip(other.pixels.iter()).enumerate() {
            if a & 0x00FF_FFFF != b & 0x00FF_FFFF {
                differing += 1;
                if first.is_none() {
                    first = Some((i as u32 % self.width, i as u32 / self.width));
                }
            }
        }
        Ok(Diff { differing, first })
    }

    /// Whether two frames are identical, ignoring the unused alpha byte.
    pub fn matches(&self, other: &Frame) -> bool {
        self.diff(other).map(|d| d.differing == 0).unwrap_or(false)
    }

    /// Write a binary PPM (`P6`) of this frame.
    ///
    /// PPM is top-down and byte-ordered `R,G,B` in that order, so a big-endian `0x00RRGGBB` word is
    /// written as its three high bytes in order. Concretely, red `0x00RR0000` becomes the bytes
    /// `RR 00 00` -- and the naive thing, splitting the little-endian word into bytes, would give
    /// `00 00 RR`, which is blue. That inversion is the whole reason this function exists rather than a
    /// `transmute`.
    ///
    /// A **binary** P6 rather than ASCII P3: a 1024x600 frame is 1.8 MB of bytes against 14 MB of
    /// decimal, and the gate hashes the file.
    pub fn to_ppm<W: Write>(&self, sink: &mut W) -> Result<u64, FrameError> {
        // The header goes through a `String` rather than straight into the sink, because `write!`
        // reports no length and the return value here is the *byte count*. The first version wrote the
        // header with `write!` and counted it as 1, so `to_ppm` reported `1 + body` for every frame
        // whose header was not one byte long -- and `HeadlessScanout::dump` passes that number straight
        // through as "bytes written", which is the number a test would compare against `out.len()`.
        use std::fmt::Write as _;
        let mut header = String::with_capacity(24);
        // Writing into a `String` cannot fail, so the `Result` is unwrapped rather than propagated: the
        // alternative is to pretend a formatting failure is an I/O error, which it is not.
        writeln!(header, "P6").expect("a String write cannot fail");
        writeln!(header, "{} {}", self.width, self.height).expect("ditto");
        writeln!(header, "255").expect("ditto");
        sink.write_all(header.as_bytes())?;
        let mut total = header.len() as u64;
        // One row at a time, so the conversion costs one row of scratch rather than a second frame.
        for y in 0..self.height {
            let mut row_bytes = Vec::with_capacity(self.width as usize * 3);
            for p in self.row(y) {
                let v = p & 0x00FF_FFFF;
                row_bytes.push((v >> 16) as u8);
                row_bytes.push((v >> 8) as u8);
                row_bytes.push(v as u8);
            }
            sink.write_all(&row_bytes)?;
            total += row_bytes.len() as u64;
        }
        Ok(total)
    }

    #[inline]
    fn index(&self, x: u32, y: u32) -> usize {
        y as usize * self.width as usize + x as usize
    }
}

/// The result of [`Frame::diff`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Diff {
    /// Pixels that differ.
    pub differing: u64,
    /// The first differing pixel, as `(x, y)`. `None` when none do.
    pub first: Option<(u32, u32)>,
}

impl Diff {
    /// Whether the frames are identical.
    pub fn is_empty(&self) -> bool {
        self.differing == 0
    }

    /// Differing pixels as a fraction of the frame, for a threshold.
    pub fn fraction(&self, total: u64) -> f64 {
        if total == 0 {
            0.0
        } else {
            self.differing as f64 / total as f64
        }
    }
}

/// The `u32` slice's bytes, without a dependency on `bytemuck`.
///
/// `align_to` is `unsafe` -- it hands back a slice over the same memory under a different type -- so
/// this is the crate's one `unsafe`, and the safety argument is the two lines inside it: `u8` has
/// alignment 1, so the prefix and suffix are empty, and the returned slice borrows the input for as long
/// as the input lives, so no aliasing is introduced. It is here because every DRM backend needs it and
/// `bytemuck` would be a dependency for one call.
fn bytemuck_bytes(pixels: &[u32]) -> &[u8] {
    // SAFETY: `u8` has alignment 1, so `align_to` cannot produce a prefix or a suffix; the middle
    // slice borrows `pixels` for as long as the return value lives.
    let (prefix, words, suffix) = unsafe { pixels.align_to::<u8>() };
    debug_assert!(prefix.is_empty() && suffix.is_empty(), "u8 is 1-aligned");
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_frame_is_the_fill() {
        let f = Frame::new(4, 3, 0x0012_3456);
        assert_eq!(f.size(), (4, 3));
        assert_eq!(f.pixels().len(), 12);
        assert!(f.pixels().iter().all(|p| *p == 0x0012_3456));
    }

    #[test]
    fn the_alpha_byte_of_the_fill_is_masked_off() {
        // A caller writing 0xFF00_0000 for "opaque red" must get the same frame as 0x0000_0000.
        let a = Frame::new(2, 2, 0xFF00_0000);
        let b = Frame::new(2, 2, 0x0000_0000);
        assert_eq!(a.pixels()[0], 0);
        assert!(a.matches(&b));
    }

    #[test]
    fn pixels_are_top_left_first() {
        let mut f = Frame::black(3, 2);
        f.set_pixel(0, 0, 0x00FF_0000);
        f.set_pixel(2, 0, 0x0000_00FF);
        f.set_pixel(1, 1, 0x0000_FF00);
        assert_eq!(f.pixel(0, 0), 0x00FF_0000);
        assert_eq!(f.pixel(2, 0), 0x0000_00FF);
        assert_eq!(f.pixel(1, 1), 0x0000_FF00);
        assert_eq!(f.pixels()[0], 0x00FF_0000);
        assert_eq!(f.pixels()[2], 0x0000_00FF);
        assert_eq!(f.pixels()[4], 0x0000_FF00);
    }

    #[test]
    fn out_of_bounds_reads_black_and_writes_are_ignored() {
        let mut f = Frame::new(2, 2, 0x00AA_BBBB);
        assert_eq!(
            f.pixel(9, 9),
            0,
            "off-frame read should be black, not a panic"
        );
        f.set_pixel(9, 9, 0x00FF_FFFF);
        assert_eq!(f.pixel(9, 9), 0);
        assert_eq!(f.pixels()[0], 0x00AA_BBBB, "the write leaked somewhere");
    }

    #[test]
    fn rows_are_slices_of_the_frame() {
        let mut f = Frame::black(3, 2);
        f.row_mut(1)[2] = 0x00FF_FFFF;
        assert_eq!(f.pixel(2, 1), 0x00FF_FFFF);
        assert_eq!(f.row(1).len(), 3);
        assert!(
            f.row(9).is_empty(),
            "an off-frame row should be empty, not a panic"
        );
        assert!(f.row_mut(9).is_empty());
    }

    #[test]
    fn fill_rect_writes_the_whole_rectangle() {
        let mut f = Frame::black(10, 10);
        let n = f.fill_rect(2, 3, 4, 5, 0x0011_2233);
        assert_eq!(n, 20, "4x5 = 20 pixels");
        for y in 3..8 {
            for x in 2..6 {
                assert_eq!(f.pixel(x, y), 0x0011_2233, "at {x},{y}");
            }
        }
        assert_eq!(f.pixel(1, 3), 0, "left edge leaked");
        assert_eq!(f.pixel(6, 3), 0, "right edge leaked");
        assert_eq!(f.pixel(2, 8), 0, "bottom edge leaked");
    }

    #[test]
    fn fill_rect_clips_rather_than_panicking() {
        let mut f = Frame::black(4, 4);
        // Half off each edge.
        let n = f.fill_rect(-2, -2, 4, 4, 0x00FF_FFFF);
        assert_eq!(n, 4, "only a 2x2 corner is on-frame");
        assert_eq!(f.pixel(0, 0), 0x00FF_FFFF);
        assert_eq!(f.pixel(1, 1), 0x00FF_FFFF);
        assert_eq!(f.pixel(2, 0), 0);
    }

    #[test]
    fn a_wholly_offscreen_fill_writes_nothing() {
        let mut f = Frame::black(4, 4);
        assert_eq!(f.fill_rect(100, 100, 2, 2, 0x00FF_FFFF), 0);
        assert_eq!(f.fill_rect(-100, 0, 2, 2, 0x00FF_FFFF), 0);
        assert_eq!(f.fill_rect(0, 0, 0, 0, 0x00FF_FFFF), 0);
        assert!(f.pixels().iter().all(|p| *p == 0));
    }

    #[test]
    fn blit_copies_at_an_offset() {
        let src = {
            let mut s = Frame::black(2, 2);
            s.fill_rect(0, 0, 2, 2, 0x00FF_0000);
            s
        };
        let mut dst = Frame::black(4, 4);
        assert_eq!(dst.blit(&src, 1, 2), 4);
        assert_eq!(dst.pixel(1, 2), 0x00FF_0000);
        assert_eq!(dst.pixel(2, 3), 0x00FF_0000);
        assert_eq!(dst.pixel(0, 0), 0, "blit landed at the origin");
    }

    #[test]
    fn blit_clips_at_the_edges() {
        let src = {
            let mut s = Frame::black(4, 4);
            s.fill_rect(0, 0, 4, 4, 0x00FF_0000);
            s
        };
        let mut dst = Frame::black(4, 4);
        // Two pixels off the right, one off the bottom: a 2x3 corner of the source lands.
        assert_eq!(dst.blit(&src, 2, 1), 6);
        assert_eq!(dst.pixel(3, 3), 0x00FF_0000);
        assert_eq!(dst.pixel(1, 0), 0);

        // And wholly off: nothing, no panic.
        assert_eq!(dst.blit(&src, 10, 10), 0);
        assert_eq!(dst.blit(&src, -10, -10), 0);
    }

    #[test]
    fn blit_entirely_negative_offsets_still_place_the_visible_part() {
        let src = {
            let mut s = Frame::black(4, 4);
            s.fill_rect(0, 0, 4, 4, 0x00FF_0000);
            s
        };
        let mut dst = Frame::black(4, 4);
        // The source's top-left 2x2 is off-frame; its bottom-right 2x2 lands at the origin.
        assert_eq!(dst.blit(&src, -2, -2), 4);
        assert_eq!(dst.pixel(0, 0), 0x00FF_0000);
        assert_eq!(dst.pixel(1, 1), 0x00FF_0000);
    }

    #[test]
    fn diff_counts_and_locates() {
        let a = Frame::black(4, 4);
        let mut b = a.clone();
        assert_eq!(
            a.diff(&b).expect("same size"),
            Diff {
                differing: 0,
                first: None
            }
        );

        b.set_pixel(2, 1, 0x00FF_FFFF);
        b.set_pixel(3, 3, 0x00FF_FFFF);
        let d = a.diff(&b).expect("same size");
        assert_eq!(d.differing, 2);
        assert_eq!(d.first, Some((2, 1)));
        assert!(!d.is_empty());
        assert_eq!(d.fraction(16), 0.125);
    }

    #[test]
    fn diff_ignores_the_alpha_byte() {
        let a = Frame::new(2, 2, 0x00FF_0000);
        let mut b = a.clone();
        b.pixels_mut()[0] |= 0xFF00_0000;
        assert_eq!(a.diff(&b).expect("same size").differing, 0);
    }

    #[test]
    fn diff_of_different_sizes_is_an_error_not_a_count() {
        let a = Frame::black(4, 4);
        let b = Frame::black(4, 5);
        assert_eq!(
            a.diff(&b),
            Err(FrameError::SizeMismatch {
                want: (4, 4),
                got: (4, 5)
            })
        );
        assert!(!a.matches(&b));
    }

    #[test]
    fn ppm_is_binary_and_correctly_ordered() {
        let mut f = Frame::black(2, 1);
        f.set_pixel(0, 0, 0x00FF_0000); // red
        f.set_pixel(1, 0, 0x0000_0000);

        let mut out: Vec<u8> = Vec::new();
        f.to_ppm(&mut out).expect("write");
        let text = String::from_utf8(out[..11].to_vec()).expect("header is ascii");
        assert!(text.starts_with("P6\n2 1\n255\n"), "header was {text:?}");

        // The bytes after the header: red is FF 00 00 -- *not* 00 00 FF.
        assert_eq!(&out[11..], &[0xFF, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn ppm_orders_green_and_blue_correctly_too() {
        for (colour, want) in [
            (0x0000_FF00u32, [0x00u8, 0xFF, 0x00]),
            (0x0000_00FF, [0x00, 0x00, 0xFF]),
            (0x0080_8040, [0x80, 0x80, 0x40]),
        ] {
            let mut f = Frame::black(1, 1);
            f.set_pixel(0, 0, colour);
            let mut out: Vec<u8> = Vec::new();
            f.to_ppm(&mut out).expect("write");
            let body = &out[out.len() - 3..];
            assert_eq!(body, want, "0x{colour:06x} came out as {body:?}");
        }
    }

    #[test]
    fn a_zero_sized_frame_is_legal() {
        let f = Frame::black(0, 0);
        assert_eq!(f.pixels().len(), 0);
        assert_eq!(f.pixel(0, 0), 0);
        let mut out: Vec<u8> = Vec::new();
        f.to_ppm(&mut out).expect("write");
        // The header is still 11 bytes for a zero-sized frame, and the count is exact.
        assert_eq!(&out, b"P6\n0 0\n255\n");
        assert_eq!(f.to_ppm(&mut Vec::new()).expect("write"), 11);
    }
}
