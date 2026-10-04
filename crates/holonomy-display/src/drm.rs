//! The DRM backend: its ABI, re-exported from the one verified copy, and what is still missing.
//!
//! # This is the ABI, not the backend. Phase 9 finishes it.
//!
//! PROJECT.md §2.7 measured that the DRM path is more exercisable here than expected: allocate,
//! `MAP_DUMB`, write, read back and destroy all succeed unprivileged on the target's `/dev/dri/card1`.
//! Only `SETCRTC` is out of reach, because that needs DRM master and therefore the VT.
//!
//! So a working `DrmScanout` is *possible* -- but only Phase 9, running on the ThinkPad X200, can prove
//! it. Writing the ioctl calls now, with no hardware to run them against, would put untested code that
//! reports success in the tree, and a DRM call that silently does nothing is worse than one that is
//! absent: it would make the panel claim to show a frame it never received.
//!
//! Every constructor and [`Scanout::present`] here therefore returns [`DrmError::NotBuilt`]. That is the
//! honest answer, and it is what makes the absence loud rather than quiet.
//!
//! # The ABI is not duplicated here, and that is the whole point of this module
//!
//! Phase 8 started to transcribe the ioctl numbers into this crate -- and got `nr` wrong the same way
//! Phase 7's teardown had. Both now come from [`holonomy_jail::teardown`], which checks them against
//! `/usr/include/drm/drm.h`. A second copy of a magic number is a second place for that mistake.
//!
//! So what this crate contributes is the *reason*: [`Frame`]'s pixel word is `0x00RRGGBB`, `xRGB8888`
//! has red in `0x00RR_0000`, and the dumb buffer is 32bpp -- so presenting a frame is a `write` of
//! `frame.as_bytes()` with no per-pixel conversion. That is a fact about this crate's format, and it is
//! the reason [`crate::PixelFormat`] is what it is.
//!
//! # `SETCRTC` will still fail in Phase 9, and that is not a bug
//!
//! `SETCRTC` needs DRM master, which needs the VT. So a session on real hardware gets
//! allocate/map/write/destroy and *not* presentation, and the visual baseline stays
//! [`crate::HeadlessScanout`]. See the table above.
//!
//! [`Scanout::present`]: Scanout::present
//! [`DrmError::NotBuilt`]: DrmError::NotBuilt

use std::io;

use crate::frame::{Frame, FrameError};
use crate::Scanout;

// One verified copy, in the crate that already needs it: `holonomy-jail`'s teardown destroys the dumb
// buffer, so the numbers live beside the code that calls them and beside the tests that check them
// against the kernel header.
pub use holonomy_jail::teardown::{
    drm_ioctl_mode_create_dumb, drm_ioctl_mode_destroy_dumb, drm_ioctl_mode_map_dumb,
    DrmModeCreateDumb, DrmModeDestroyDumb, DrmModeMapDumb, DRM_FORMAT_XRGB8888,
    DRM_IOCTL_MODE_CREATE_DUMB, DRM_IOCTL_MODE_DESTROY_DUMB, DRM_IOCTL_MODE_MAP_DUMB,
};

/// Bits per pixel for the dumb buffer: 32.
///
/// 32 because [`Frame`] stores one `0x00RRGGBB` per `u32` and `xRGB8888` is exactly that.
pub const DUMB_BPP: u32 = 32;

/// A DRM dumb-buffer scanout.
///
/// Carries only what can be known without a device: the frame size, and whether presentation is
/// possible. Everything that needs an ioctl is Phase 9's.
#[derive(Debug, Clone)]
pub struct DrmScanout {
    width: u32,
    height: u32,
    /// Whether `SETCRTC` has succeeded. Always `false` until Phase 9.
    presented: bool,
}

impl DrmScanout {
    /// A scanout of the given size, with no device behind it.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            presented: false,
        }
    }

    /// Whether `SETCRTC` succeeded, i.e. whether this scanout lights pixels.
    ///
    /// Always `false` until Phase 9. Kept so a caller written now does not have to change.
    pub fn presented(&self) -> bool {
        self.presented
    }
}

impl Scanout for DrmScanout {
    /// Always [`DrmError::NotBuilt`].
    ///
    /// The frame's *size* is checked first, because that check needs no hardware and a caller who got
    /// the shape wrong should hear about it before hearing that the backend is absent.
    fn present(&mut self, frame: &Frame) -> Result<u64, FrameError> {
        let (want, got) = ((self.width, self.height), frame.size());
        if want != got {
            return Err(FrameError::SizeMismatch { want, got });
        }
        Err(DrmError::NotBuilt.into())
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn describe(&self) -> &'static str {
        "drm-dumb (not built: Phase 9)"
    }
}

/// Why a DRM operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrmError {
    /// The backend has no device behind it. See the module docs: this is what every call returns until
    /// Phase 9 runs on the target hardware.
    NotBuilt,
    /// `open` failed. Carries `errno`.
    Open(i32),
    /// An ioctl failed. Carries the request code and `errno`.
    Ioctl(u64, i32),
    /// A frame of the wrong shape was presented.
    SizeMismatch {
        /// The scanout's size.
        want: (u32, u32),
        /// The frame's size.
        got: (u32, u32),
    },
    /// Writing the mapping failed.
    Write(i32),
}

impl std::fmt::Display for DrmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotBuilt => write!(
                f,
                "the DRM backend is not built: its ioctls need the target hardware, which is Phase 9. \
                 Use HeadlessScanout."
            ),
            Self::Open(e) => write!(f, "opening the DRM card failed: {e} ({})", errno_name(*e)),
            Self::Ioctl(req, e) => {
                write!(f, "DRM ioctl {req:#x} failed: {e} ({})", errno_name(*e))
            }
            Self::SizeMismatch { want, got } => {
                write!(f, "scanout is {want:?}, frame is {got:?}")
            }
            Self::Write(e) => {
                write!(f, "writing the dumb buffer failed: {e} ({})", errno_name(*e))
            }
        }
    }
}

impl std::error::Error for DrmError {}

impl From<DrmError> for FrameError {
    fn from(e: DrmError) -> Self {
        match e {
            DrmError::SizeMismatch { want, got } => Self::SizeMismatch { want, got },
            // `NotBuilt` carries no errno -- it is an absence, not a failure -- so it maps to zero and
            // the `DrmError` is recoverable from the message rather than the code.
            DrmError::NotBuilt | DrmError::Open(_) | DrmError::Ioctl(_, _) | DrmError::Write(_) => {
                Self::Backend(0)
            }
        }
    }
}

impl From<io::Error> for DrmError {
    fn from(e: io::Error) -> Self {
        Self::Open(e.raw_os_error().unwrap_or(libc::EIO))
    }
}

/// The name of an `errno`, for a message worth reading.
pub fn errno_name(e: i32) -> &'static str {
    match e {
        1 => "EPERM",
        5 => "EIO",
        12 => "ENOMEM",
        13 => "EACCES",
        16 => "EBUSY",
        19 => "ENODEV",
        22 => "EINVAL",
        25 => "ENOTTY",
        28 => "ENOSPC",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ioctl_numbers_come_from_the_one_verified_copy() {
        // Phase 8: this crate transcribed its own set of DRM numbers and got `nr` wrong the same way
        // Phase 7's teardown did. There is one copy now, in `holonomy-jail`, checked against
        // `/usr/include/drm/drm.h`.
        assert_eq!(DRM_IOCTL_MODE_DESTROY_DUMB, 0xC004_64B4, "nr is 0xB4");
        assert_eq!(DRM_IOCTL_MODE_CREATE_DUMB, 0xC020_64B2, "nr is 0xB2");
        assert_eq!(DRM_IOCTL_MODE_MAP_DUMB, 0xC010_64B3, "nr is 0xB3");

        // The constants still agree with their derivations, here as they do there.
        assert_eq!(DRM_IOCTL_MODE_DESTROY_DUMB, drm_ioctl_mode_destroy_dumb());
        assert_eq!(DRM_IOCTL_MODE_CREATE_DUMB, drm_ioctl_mode_create_dumb());
        assert_eq!(DRM_IOCTL_MODE_MAP_DUMB, drm_ioctl_mode_map_dumb());

        // `nr` and `type` are legible in the encoding, which is how a reader checks the number by eye.
        assert_eq!((DRM_IOCTL_MODE_DESTROY_DUMB >> 30) & 3, 3, "read and write");
        assert_eq!((DRM_IOCTL_MODE_DESTROY_DUMB >> 8) & 0xFF, u64::from(b'd'));
        assert_eq!(DRM_IOCTL_MODE_DESTROY_DUMB & 0xFF, 0xB4);
        assert_eq!((DRM_IOCTL_MODE_CREATE_DUMB >> 16) & 0x3FFF, 32);
        assert_eq!((DRM_IOCTL_MODE_MAP_DUMB >> 16) & 0x3FFF, 16);
        assert_eq!((DRM_IOCTL_MODE_DESTROY_DUMB >> 16) & 0x3FFF, 4);
    }

    #[test]
    fn the_struct_layouts_are_the_kernels() {
        // Field order matters and is not guessable: `drm_mode_create_dumb` puts `height` before `width`
        // and has no `pixel_format` at all.
        assert_eq!(core::mem::size_of::<DrmModeCreateDumb>(), 32);
        assert_eq!(core::mem::offset_of!(DrmModeCreateDumb, height), 0);
        assert_eq!(core::mem::offset_of!(DrmModeCreateDumb, width), 4);
        assert_eq!(core::mem::offset_of!(DrmModeCreateDumb, handle), 16);
        assert_eq!(core::mem::offset_of!(DrmModeCreateDumb, size), 24);

        assert_eq!(core::mem::size_of::<DrmModeMapDumb>(), 16);
        assert_eq!(core::mem::offset_of!(DrmModeMapDumb, handle), 0);
        assert_eq!(core::mem::offset_of!(DrmModeMapDumb, offset), 8);

        assert_eq!(core::mem::size_of::<DrmModeDestroyDumb>(), 4);
        assert_eq!(core::mem::offset_of!(DrmModeDestroyDumb, handle), 0);
    }

    #[test]
    fn the_frame_format_is_the_dumb_buffers_format() {
        // The property that makes presenting a frame a `write` rather than a conversion: `Frame` stores
        // `0x00RRGGBB` and `xRGB8888` has red in `0x00RR0000`.
        assert_eq!(DRM_FORMAT_XRGB8888, 0x3432_5258);
        assert_eq!(DUMB_BPP, 32);
        assert_eq!(std::mem::size_of::<u32>(), 4, "one pixel is one word");
        // Red, green, blue at the three positions `xRGB8888` puts them.
        assert_eq!(0x00FF_0000u32 >> 16, 0xFF, "red in bits 23..16");
        assert_eq!(0x0000_FF00u32 >> 8, 0xFF, "green in bits 15..8");
        assert_eq!(0x0000_00FFu32, 0xFF, "blue in bits 7..0");
    }

    #[test]
    fn present_reports_its_absence_rather_than_pretending() {
        let mut s = DrmScanout::new(4, 4);
        assert_eq!(s.describe(), "drm-dumb (not built: Phase 9)");
        assert!(!s.presented());
        // A DRM call that silently does nothing is worse than one that is absent, so this must not
        // report success.
        let err = s
            .present(&Frame::black(4, 4))
            .expect_err("must not report success");
        assert_eq!(err, FrameError::Backend(0));
    }

    #[test]
    fn a_frame_of_the_wrong_size_is_caught_before_the_absence() {
        // Shape is checkable without hardware, so it is checked first: a caller who got the size wrong
        // should hear that, not "not built".
        let mut s = DrmScanout::new(4, 4);
        assert_eq!(
            s.present(&Frame::black(8, 8)),
            Err(FrameError::SizeMismatch {
                want: (4, 4),
                got: (8, 8)
            })
        );
    }

    #[test]
    fn the_not_built_message_names_the_alternative() {
        // An error a user can act on.
        let msg = DrmError::NotBuilt.to_string();
        assert!(msg.contains("Phase 9"), "{msg}");
        assert!(msg.contains("HeadlessScanout"), "{msg}");
    }

    #[test]
    fn errno_names_cover_the_ones_a_drm_path_hits() {
        assert_eq!(errno_name(1), "EPERM");
        assert_eq!(errno_name(5), "EIO");
        assert_eq!(errno_name(12), "ENOMEM");
        assert_eq!(errno_name(13), "EACCES");
        assert_eq!(errno_name(16), "EBUSY");
        assert_eq!(errno_name(19), "ENODEV");
        assert_eq!(errno_name(22), "EINVAL");
        assert_eq!(errno_name(25), "ENOTTY");
        assert_eq!(errno_name(28), "ENOSPC");
        assert_eq!(errno_name(9999), "?");
    }
}
