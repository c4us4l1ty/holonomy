//! The deterministic backend: a frame in memory, a PPM file out.
//!
//! # Why the gate's visual baseline is a PPM and not a panel
//!
//! A PPM has a hash. A panel does not. Every claim the gate makes -- "the final frame matches the
//! baseline" -- needs a byte-exact answer, and reading back a DRM dumb buffer to get one would make CI
//! depend on an i915 driver and a specific mode. So [`HeadlessScanout`] is the reference
//! implementation and [`crate::drm::DrmScanout`] is the one that lights up pixels.
//!
//! # The dump path, and why it is opened before the jail
//!
//! [`dump_to_path`](Self::dump_to_path) is the **only** thing in this crate that opens a file, and it is
//! the only one that is not usable after [`holonomy_jail::Sealed`] -- which has no `open` in its
//! allowlist. So a session inside the jail hands the dump a pre-opened [`std::fs::File`] via
//! [`dump`](Self::dump), and `dump_to_path` is the convenience form for a pre-boot run and for tests.
//! Calling it after sealing is a `SIGSYS` and an exit 137, which is the jail working.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::frame::{Frame, FrameError};
use crate::Scanout;

/// An in-memory [`Scanout`] that keeps the last frame and can dump it as a PPM.
#[derive(Debug, Clone)]
pub struct HeadlessScanout {
    width: u32,
    height: u32,
    /// The most recent frame, kept so a caller can inspect what was presented.
    last: Frame,
    presented: u64,
}

impl HeadlessScanout {
    /// A scanout of the given size, with a black frame.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            last: Frame::black(width, height),
            presented: 0,
        }
    }

    /// The most recently presented frame.
    ///
    /// A copy rather than a reference, because the caller usually wants to keep it past the next
    /// `present` and a borrow that outlives the borrow checker would be a fight.
    pub fn last_frame(&self) -> &Frame {
        &self.last
    }

    /// How many frames have been presented.
    pub fn present_count(&self) -> u64 {
        self.presented
    }

    /// Write the last frame to `sink` as a binary PPM. Returns the bytes written.
    pub fn dump<W: Write>(&self, sink: &mut W) -> Result<u64, FrameError> {
        self.last.to_ppm(sink)
    }

    /// Write the last frame to a pre-opened file. Returns the bytes written.
    ///
    /// The form a sealed session uses: the file is opened during boot, when `open` still exists.
    pub fn dump_to_file(&self, file: &mut File) -> Result<u64, FrameError> {
        let mut w = BufWriter::new(file);
        let n = self.dump(&mut w)?;
        w.flush()?;
        Ok(n)
    }

    /// Open `path` and write the last frame to it.
    ///
    /// **Only valid before the jail is sealed.** See the module docs.
    pub fn dump_to_path(&self, path: &Path) -> Result<u64, FrameError> {
        let mut f = File::create(path)?;
        self.dump_to_file(&mut f)
    }
}

impl Scanout for HeadlessScanout {
    fn present(&mut self, frame: &Frame) -> Result<u64, FrameError> {
        if frame.size() != (self.width, self.height) {
            return Err(FrameError::SizeMismatch {
                want: (self.width, self.height),
                got: frame.size(),
            });
        }
        self.last = frame.clone();
        self.presented += 1;
        Ok(u64::from(self.width) * u64::from(self.height))
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn describe(&self) -> &'static str {
        "headless"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::PixelFormat;

    #[test]
    fn a_new_scanout_is_black_and_the_right_size() {
        let s = HeadlessScanout::new(64, 32);
        assert_eq!((s.width(), s.height()), (64, 32));
        assert_eq!(s.describe(), "headless");
        assert_eq!(s.pixel_format(), PixelFormat::Rgb888x);
        assert_eq!(s.last_frame().pixels().len(), 64 * 32);
        assert!(s.last_frame().pixels().iter().all(|p| *p == 0));
        assert_eq!(s.present_count(), 0);
    }

    #[test]
    fn presenting_keeps_the_frame_and_counts_it() {
        let mut s = HeadlessScanout::new(4, 4);
        let mut f = Frame::black(4, 4);
        f.fill_rect(1, 1, 2, 2, 0x00FF_0000);

        assert_eq!(s.present(&f).expect("present"), 16);
        assert_eq!(s.present_count(), 1);
        assert_eq!(s.last_frame().pixel(1, 1), 0x00FF_0000);

        // A second present replaces it.
        let mut g = Frame::black(4, 4);
        g.fill_rect(0, 0, 4, 4, 0x0000_00FF);
        s.present(&g).expect("present");
        assert_eq!(s.present_count(), 2);
        assert_eq!(s.last_frame().pixel(3, 3), 0x0000_00FF);
    }

    #[test]
    fn a_frame_of_the_wrong_size_is_refused() {
        let mut s = HeadlessScanout::new(4, 4);
        let wrong = Frame::black(8, 8);
        assert_eq!(
            s.present(&wrong),
            Err(FrameError::SizeMismatch {
                want: (4, 4),
                got: (8, 8)
            })
        );
        assert_eq!(s.present_count(), 0, "a refused frame must not count");
        assert!(s.last_frame().pixels().iter().all(|p| *p == 0));
    }

    #[test]
    fn the_dump_is_a_p6_ppm_of_the_last_frame() {
        let mut s = HeadlessScanout::new(2, 2);
        let mut f = Frame::black(2, 2);
        f.fill_rect(0, 0, 2, 2, 0x00CC_8844);
        s.present(&f).expect("present");

        let mut out: Vec<u8> = Vec::new();
        let n = s.dump(&mut out).expect("dump");
        assert_eq!(n as usize, out.len());
        assert_eq!(&out[..11], b"P6\n2 2\n255\n");
        // Four pixels of CC 88 44.
        let want: Vec<u8> = [0xCCu8, 0x88, 0x44]
            .iter()
            .copied()
            .cycle()
            .take(12)
            .collect();
        assert_eq!(&out[11..], want.as_slice());
    }

    #[test]
    fn the_dump_follows_the_last_present_not_the_first() {
        let mut s = HeadlessScanout::new(1, 1);
        let mut red = Frame::black(1, 1);
        red.set_pixel(0, 0, 0x00FF_0000);
        let mut blue = Frame::black(1, 1);
        blue.set_pixel(0, 0, 0x0000_00FF);

        s.present(&red).expect("present");
        s.present(&blue).expect("present");
        let mut out: Vec<u8> = Vec::new();
        s.dump(&mut out).expect("dump");
        assert_eq!(&out[out.len() - 3..], &[0x00, 0x00, 0xFF]);
    }

    #[test]
    fn a_dump_to_a_file_and_a_dump_to_a_path_agree() {
        let mut s = HeadlessScanout::new(3, 3);
        let mut f = Frame::black(3, 3);
        f.fill_rect(0, 0, 3, 3, 0x0012_3456);
        s.present(&f).expect("present");

        let mut a: Vec<u8> = Vec::new();
        s.dump(&mut a).expect("dump");

        let path = std::env::temp_dir().join(format!(
            "holonomy-headless-{}-{}",
            std::process::id(),
            a.len()
        ));
        let mut file = File::create(&path).expect("create");
        s.dump_to_file(&mut file).expect("dump to file");
        drop(file);
        let from_path = std::fs::read(&path).expect("read back");
        std::fs::remove_file(&path).ok();

        let mut b: Vec<u8> = Vec::new();
        s.dump(&mut b).expect("dump");
        assert_eq!(from_path, b);
        assert_eq!(a, b);
    }

    #[test]
    fn two_dumps_of_the_same_frame_are_byte_identical() {
        // The property the visual baseline rests on: nothing about the dump is time- or
        // allocation-dependent.
        let mut s = HeadlessScanout::new(17, 9);
        let mut f = Frame::black(17, 9);
        for i in 0..17u32 {
            // A per-column colour, so the dump differs at every column and a dropped or shifted byte
            // shows up as a difference rather than a coincidence.
            f.set_pixel(i, i % 9, (i & 0xFF) << 16);
        }
        s.present(&f).expect("present");

        let mut first: Vec<u8> = Vec::new();
        s.dump(&mut first).expect("dump");
        for _ in 0..4 {
            let mut again: Vec<u8> = Vec::new();
            s.dump(&mut again).expect("dump");
            assert_eq!(first, again);
        }
    }
}
