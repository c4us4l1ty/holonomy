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

#[cfg(feature = "desktop")]
pub mod desktop;
pub mod drm;
pub mod frame;
pub mod headless;
pub mod paint;

#[cfg(feature = "desktop")]
pub use desktop::{Desktop, DesktopError};
pub use frame::{Frame, FrameError, PixelFormat, PIXEL_BYTES};
// The `Scanout` trait's `present_damage` takes a `DamageRect`, so the crate that defines the trait
// re-exports the type. `holonomy_render::DamageRect` is the one; there is no second.
pub use headless::HeadlessScanout;
pub use holonomy_render::DamageRect;
pub use paint::{PaintStats, Painter};

/// Somewhere a finished frame goes.
///
/// One method, because that is the whole contract: by the time a frame is presented, rendering is
/// over. Anything a backend needs to *prepare* -- allocating a buffer, mapping dumb memory, picking a
/// mode -- happens in its constructor, where it can fail loudly instead of during presentation.
///
/// # Why `Any`
///
/// A backend that needs to be *driven* as well as presented -- a window, whose connection is also where
/// its key events come from -- is owned by the session, because the session presents through it. So the
/// driver needs it back, and `Any` is what makes `Box<dyn Scanout>` downcastable to the concrete
/// backend. It costs one vtable slot and no behaviour, and it is the only alternative to sharing the
/// backend between the session and the loop, which is two mutable owners of one object.
pub trait Scanout: std::any::Any {
    /// Present a frame, copying or presenting it as the backend sees fit.
    ///
    /// Returns how many pixels were written, which for a headless backend is the whole frame and for
    /// a DRM backend is whatever `MAP_DUMB` accepted. A short count is reported rather than rounded,
    /// because a partially presented frame is a bug in the renderer's bounds and the backend should not
    /// paper over it.
    fn present(&mut self, frame: &Frame) -> Result<u64, FrameError>;

    /// Change the size of what this presents into. Returns whether it changed.
    ///
    /// **The default is "I have a fixed size", which is true of a DRM panel** and is the honest answer
    /// for one: a panel's size is its mode, and changing it is a mode set, not a resize. A backend
    /// whose size can change -- a window -- overrides this.
    ///
    /// This exists so that a resize has one owner. [`Session::resize`](../../holonomy/session/struct.Session.html#method.resize)
    /// calls this *before* it rebuilds its own frame, because `present` checks the frame's size against
    /// the backend's and refuses a mismatch; and it checks the two agree afterwards, so a caller cannot
    /// leave a session and a backend at different sizes even by accident. Two halves, one method, one
    /// order.
    fn resize(&mut self, _width: u32, _height: u32) -> Result<bool, FrameError> {
        Ok(false)
    }

    /// Present only `damage` of a frame, where the backend can.
    ///
    /// The session rasterises a damaged rectangle and then hands the backend the whole frame, because
    /// the frame is where the pixels that did not change still live. A backend that pushes all of it
    /// therefore spends 4 MiB of socket per keystroke to move 18 rows. This is the hook that lets a
    /// bandwidth-bound backend -- a window on a desktop, eventually a display controller -- push only
    /// the rectangle.
    ///
    /// **The default is the whole frame**, so a backend that says nothing gets exactly the behaviour it
    /// had before this method existed, and `HeadlessScanout`'s numbers are unchanged. `None` also means
    /// the whole frame: "no damage recorded" is not "nothing changed", it is "we do not know", and the
    /// safe reading of that is everything.
    fn present_damage(
        &mut self,
        frame: &Frame,
        damage: Option<DamageRect>,
    ) -> Result<u64, FrameError> {
        let _ = damage;
        self.present(frame)
    }

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
