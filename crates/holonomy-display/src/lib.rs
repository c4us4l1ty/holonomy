//! [`Scanout`], and the two backends behind it.
//!
//! # Why a trait at all
//!
//! PROJECT.md §2.7 measured that the DRM path is *more* exercisable here than expected: allocate,
//! `MAP_DUMB`, write, read back and destroy all succeed unprivileged on this machine's `/dev/dri/card1`.
//! Only `SETCRTC` presentation is out of reach, because that needs DRM master and therefore the VT.
//!
//! So `DrmScanout` is a real backend that really runs, and [`HeadlessScanout`] is **not** a fallback --
//! it is the deterministic one. A PPM dump has a hash; a panel does not. The gate's visual baseline is
//! `HeadlessScanout`, and `DrmScanout` is what the same frame goes to on real hardware.
//!
//! Hardware-only paths sit behind the `hardware` feature so CI never depends on them.
//!
//! # Pre-opened descriptors
//!
//! Neither backend opens anything once the process is sealed -- the jail has no `open`. Both are
//! constructed from a descriptor, or from an explicit `open` that only the pre-boot path calls. See
//! [`crate::drm::DrmScanout::open`].
//!
//! # The pixel format is `0x00RRGGBB` in a `u32`, top-left origin
//!
//! One format, chosen so a `u32` is one pixel and a row is a slice of them. `blit_glyph` in
//! `holonomy-assets` already speaks it, so nothing has to convert on the way to the panel. Top-left
//! origin because that is what [`Frame`] indexes and what a PPM dump writes directly -- PPM is
//! top-down, so a flip would be needed at one end or the other and this way it is needed at neither.
//!
//! [`Frame`]: crate::Frame

pub mod drm;
pub mod frame;
pub mod headless;
pub mod paint;

pub use frame::{Frame, FrameError, PixelFormat, PIXEL_BYTES};
pub use headless::HeadlessScanout;
pub use paint::{PaintStats, Painter};

/// Somewhere a finished frame goes.
///
/// One method, because that is the whole contract: by the time a frame is presented, rendering is
/// over. Anything a backend needs to *prepare* -- allocating a buffer, mapping dumb memory, picking a
/// mode -- happens in its constructor, where it can fail loudly instead of during presentation.
pub trait Scanout {
    /// Present a frame, copying or presenting it as the backend sees fit.
    ///
    /// Returns how many pixels were written, which for a headless backend is the whole frame and for
    /// a DRM backend is whatever `MAP_DUMB` accepted. A short count is reported rather than rounded,
    /// because a partially presented frame is a bug in the renderer's bounds and the backend should not
    /// paper over it.
    fn present(&mut self, frame: &Frame) -> Result<u64, FrameError>;

    /// The frame's width in pixels.
    fn width(&self) -> u32;

    /// The frame's height in pixels.
    fn height(&self) -> u32;

    /// A name for a status line and for logs.
    fn describe(&self) -> &'static str;

    /// Bytes per pixel, so a backend can check a frame it was handed is the shape it expected.
    fn pixel_format(&self) -> PixelFormat {
        PixelFormat::Rgb888x
    }
}

/// The panel's width and height, as a backend reports them.
pub fn size_of<S: Scanout + ?Sized>(s: &S) -> (u32, u32) {
    (s.width(), s.height())
}
