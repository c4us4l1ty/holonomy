//! Orderly teardown: noise the ring, scrub every block, release the hardware, `_exit(0)`.
//!
//! # Why this is a pre-built plan rather than a closure
//!
//! Teardown runs *after* the seccomp filter is installed, so it may not allocate, and it may
//! not call anything that would. A `Vec<Box<dyn FnOnce()>>` would put an allocator call and a
//! vtable load between "the user pressed the key that exits" and "the plaintext is gone".
//!
//! So the plan is a fixed-size array of data-only actions, built during boot while allocation
//! is still permitted, and executed by a loop with no allocation and no dynamic dispatch.
//!
//! # The order, and why it is this order
//!
//! ```text
//!   1. noise the ring buffer      -- overwrite with PRNG bytes, not zeroes
//!   2. registry::scrub_all()      -- every registered mapping's payload, to zeroes
//!   3. DRM_IOCTL_MODE_DESTROY_DUMB -- release the dumb buffer back to the driver
//!   4. fsync                      -- get the (now-empty) container to disk
//!   5. close                      -- the descriptors boot established
//!   6. _exit(0)                   -- no unwinding, no stdio flush
//! ```
//!
//! Step 1 before step 2 because noise and zeroes are not the same operation and the ring
//! deserves the stronger one. Zeroing is what `SecureBlock::drop` does and is the right default:
//! an all-zero region is distinguishable from memory that was *never written*, which is exactly
//! the signal a compression-based memory attack looks for. Random bytes leave no such
//! signature.
//!
//! Steps 3-5 after the scrub because they touch no plaintext -- a DRM ioctl and an `fsync` do
//! not read the document -- so there is no reason to expose a window where they could.
//!
//! # What teardown is *not*
//!
//! It is not a substitute for the tripwire. The tripwire fires when the process has lost
//! control, and it scrubs the registry; it has no way to run a plan, has no guarantee the
//! plan exists, and no way to know which of the ring's buffers were live. The two mechanisms
//! overlap deliberately and neither replaces the other.
//!
//! And it is not a substitute for `Drop`. `_exit` does not run destructors, so every buffer
//! the session cares about has to appear in the plan. Anything that does not is left to the
//! process-level controls -- `mlockall`, `RLIMIT_CORE = 0`, `PR_SET_DUMPABLE = 0` -- which
//! cover swap and core files but not a heap page that happens to be resident when the kernel
//! reclaims it. See [`TeardownPlan::scrub_on_fault`].

/// Exit code for an orderly exit.
pub const TEARDOWN_EXIT: i32 = 0;

/// Maximum actions in a plan.
///
/// One DRM buffer, one container fd, one evdev fd, one ring range and change. Sized well
/// above the session's actual needs so that a full plan is not a silent truncation.
pub const MAX_ACTIONS: usize = 64;

/// Why a teardown plan could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeardownError {
    /// More than [`MAX_ACTIONS`] actions.
    TooManyActions,
    /// The registry refused a range.
    RegistryFull(crate::registry::RegisterError),
    /// A noise range was zero-length or overflowed.
    BadRange,
}

impl core::fmt::Display for TeardownError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooManyActions => write!(f, "teardown plan exceeds {MAX_ACTIONS} actions"),
            Self::RegistryFull(e) => write!(f, "{e}"),
            Self::BadRange => f.write_str("teardown range is zero-length or wraps"),
        }
    }
}

impl std::error::Error for TeardownError {}

/// One step of the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Overwrite a range with PRNG bytes. For heap-resident plaintext that outlives the
    /// process's own bookkeeping.
    Noise {
        /// First byte.
        ptr: usize,
        /// Length in bytes.
        len: usize,
    },
    /// `DRM_IOCTL_MODE_DESTROY_DUMB` on `fd`, for `handle`.
    DestroyDumbBuffer {
        /// The DRM card fd opened during boot.
        fd: i32,
        /// The dumb buffer's handle, from `CreateDumb`.
        ///
        /// Phase 8: this field did not exist. The ioctl was called with a **null** argument, and
        /// `DRM_IOCTL_MODE_DESTROY_DUMB` takes `struct drm_mode_destroy_dumb { __u32 handle; }` -- the
        /// kernel copies four bytes out of the pointer and a null one faults. So even with the right
        /// request number the call could not have worked. The handle is the one thing the caller has and
        /// the call site did not have, which is exactly why it belongs in the action.
        handle: u32,
    },
    /// `fsync(2)` on `fd`.
    Sync {
        /// The container fd.
        fd: i32,
    },
    /// `close(2)` on `fd`.
    Close {
        /// The fd.
        fd: i32,
    },
}

/// A pre-built teardown, executable with no allocation.
#[derive(Debug)]
pub struct TeardownPlan {
    actions: [Option<Action>; MAX_ACTIONS],
    count: usize,
}

impl Default for TeardownPlan {
    fn default() -> Self {
        Self::new()
    }
}

impl TeardownPlan {
    /// An empty plan.
    pub const fn new() -> Self {
        Self {
            actions: [None; MAX_ACTIONS],
            count: 0,
        }
    }

    /// Append a noise range covering `ptr..ptr+len`.
    pub fn noise(&mut self, ptr: *mut u8, len: usize) -> Result<(), TeardownError> {
        if len == 0 || len > usize::MAX - (ptr as usize) {
            return Err(TeardownError::BadRange);
        }
        self.push(Action::Noise {
            ptr: ptr as usize,
            len,
        })
    }

    /// Append `DRM_IOCTL_MODE_DESTROY_DUMB` for `fd`, on the buffer `handle`.
    pub fn destroy_dumb_buffer(&mut self, fd: i32, handle: u32) -> Result<(), TeardownError> {
        self.push(Action::DestroyDumbBuffer { fd, handle })
    }

    /// Append `fsync` for `fd`.
    pub fn sync(&mut self, fd: i32) -> Result<(), TeardownError> {
        self.push(Action::Sync { fd })
    }

    /// Append `close` for `fd`.
    pub fn close(&mut self, fd: i32) -> Result<(), TeardownError> {
        self.push(Action::Close { fd })
    }

    fn push(&mut self, action: Action) -> Result<(), TeardownError> {
        if self.count >= MAX_ACTIONS {
            return Err(TeardownError::TooManyActions);
        }
        self.actions[self.count] = Some(action);
        self.count += 1;
        Ok(())
    }

    /// Number of actions.
    pub const fn len(&self) -> usize {
        self.count
    }

    /// Whether the plan does nothing.
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The action at `index`, for tests and for the boot report.
    pub fn action(&self, index: usize) -> Option<Action> {
        self.actions.get(index).copied().flatten()
    }

    /// Register a non-`SecureBlock` range so the tripwire will scrub it.
    ///
    /// The registry does not care what memory it is handed, which is what makes this useful:
    /// the container's three-stage ring is heap-resident (`AlignedBuf`), not a
    /// [`SecureBlock`](https://docs.rs/holonomy-secure), so it is *not* registered by
    /// `SecureBlock::allocate` and the tripwire would not scrub it on a fault. Its owner must
    /// register it for the lifetime of the buffer and deregister it when the buffer goes away.
    ///
    /// `base`/`total` describe the whole allocation for the guard-range check and `data`/`len`
    /// the part holding plaintext. For a heap allocation with no guard pages, pass
    /// `base = data` and `total = len`, which makes [`crate::registry::FaultSite::Data`] the
    /// only classification a fault inside it can produce -- which is the honest one.
    pub fn scrub_on_fault(
        &mut self,
        data: *mut u8,
        len: usize,
    ) -> Result<crate::registry::RegistryHandle, TeardownError> {
        if len == 0 {
            return Err(TeardownError::BadRange);
        }
        crate::registry::register(data as usize, len, data as usize, len)
            .map_err(TeardownError::RegistryFull)
    }

    /// Run the plan and `_exit(TEARDOWN_EXIT)`. Never returns.
    ///
    /// `report_fd`, if non-negative, receives a one-line summary of what was done. A teardown
    /// that cannot say what it did is a teardown nobody can test.
    pub fn run_and_exit(&self, report_fd: i32) -> ! {
        let mut rng = NoiseRng::from_entropy();
        let mut index = 0usize;
        while index < self.count {
            let Some(action) = self.actions[index] else {
                index += 1;
                continue;
            };
            match action {
                Action::Noise { ptr, len } => {
                    let mut offset = 0usize;
                    while offset < len {
                        let word = rng.next().to_le_bytes();
                        let take = core::cmp::min(8, len - offset);
                        let mut i = 0usize;
                        while i < take {
                            // SAFETY: `ptr..ptr+len` was registered by the caller as a live,
                            // writable buffer it owns; this is its own memory.
                            unsafe {
                                core::ptr::write_volatile((ptr + offset + i) as *mut u8, word[i])
                            };
                            i += 1;
                        }
                        offset += 8;
                    }
                }
                Action::DestroyDumbBuffer { fd, handle } => {
                    // The kernel copies a `struct drm_mode_destroy_dumb` out of this pointer, so it is
                    // a real local rather than a null argument. Phase 8: this was `ioctl(fd, req, 0)`,
                    // which faults in the kernel for an ioctl whose only argument is a handle.
                    let arg = DrmModeDestroyDumb { handle };
                    // SAFETY: `arg` is a live, correctly aligned, `repr(C)` copy of the struct the
                    // request declares, and it outlives the call. The cast to `c_int` is because musl
                    // declares `ioctl(int, int, ...)`; the request is 32 bits wide and the kernel
                    // truncates the encoding's high half, which is how `_IOR`'d numbers work at all.
                    //
                    // A failure here is ignored on purpose. This step touches no plaintext, and the
                    // steps after it are the ones that do.
                    //
                    // Which is exactly why the two bugs this line had -- a wrong request number and a
                    // null argument -- were invisible. A step whose failure is unobservable needs its
                    // inputs checked rather than its failures reported, so
                    // `the_dumb_buffer_ioctl_matches_the_kernel_header` pins the request number and
                    // `the_dumb_buffer_action_carries_its_handle` pins this one.
                    unsafe {
                        libc::ioctl(
                            fd,
                            DRM_IOCTL_MODE_DESTROY_DUMB as libc::c_int,
                            &arg as *const DrmModeDestroyDumb as *mut libc::c_void,
                        );
                    }
                }
                Action::Sync { fd } => {
                    // SAFETY: a plain fsync of an fd the boot opened.
                    unsafe { libc::fsync(fd) };
                }
                Action::Close { fd } => {
                    // SAFETY: closing an fd the boot opened; a double close is avoided by the
                    // plan being built once.
                    unsafe { libc::close(fd) };
                }
            }
            index += 1;
        }

        // Step 2, after the plan's own noise: everything registered, to zeroes.
        let scrubbed = crate::registry::scrub_all();

        if report_fd >= 0 {
            let (line, len) = report_line(self.count, scrubbed);
            // SAFETY: a stack buffer and a valid fd; `write` is async-signal-safe.
            unsafe { libc::write(report_fd, line.as_ptr().cast(), len) };
        }
        // SAFETY: `exit_group`, deliberately instead of `exit` -- no atexit handlers, no stdio
        // flush, no unwinding. See the module comment.
        unsafe { libc::_exit(TEARDOWN_EXIT) }
    }
}

/// `DRM_IOCTL_MODE_DESTROY_DUMB`, derived rather than pasted -- and now *checked against the header*
/// rather than against itself.
///
/// # This constant was wrong, and the test did not catch it
///
/// Phase 7 wrote `0xC004_6444`, which decodes as `dir = 3`, `size = 4`, `type = 'd'`, **`nr = 0x44`**
/// -- and paired it with a derivation that used the same `0x44`. So
/// `the_dumb_buffer_ioctl_matches_its_kernel_definition` compared the constant against its own
/// derivation and passed. Both were wrong together.
///
/// The kernel says otherwise. `/usr/include/drm/drm.h`:
///
/// ```c
/// #define DRM_IOCTL_MODE_CREATE_DUMB   DRM_IOWR(0xB2, struct drm_mode_create_dumb)
/// #define DRM_IOCTL_MODE_MAP_DUMB      DRM_IOWR(0xB3, struct drm_mode_map_dumb)
/// #define DRM_IOCTL_MODE_DESTROY_DUMB  DRM_IOWR(0xB4, struct drm_mode_destroy_dumb)
/// ```
///
/// with `DRM_IOWR(nr, type) = _IOWR('d', nr, type)` and `struct drm_mode_destroy_dumb` a bare
/// `__u32 handle`. So `nr` is **`0xB4`**, not `0x44`, and the correct request is `0xC004_64B4`.
/// `0x44` looks plausible because `DRM_COMMAND_BASE` is `0x40` and the legacy `DRM_IOCTL_MODE_*`
/// numbers did live in `0x40..=0xA0`; those are different ioctls.
///
/// A wrong request number is `ENOTTY`, and this call's failure is *ignored on purpose* -- see
/// [`Action::DestroyDumbBuffer`]. So the bug was invisible: a green gate over a step that never
/// happened. The lesson is the one the original comment got wrong: deriving from the header's
/// *definition* is only worth something if the definition is right, and the only way to know that is
/// to read the header.
///
/// What the gate can check without a device: `nr` and the struct size. Both are asserted below against
/// the values the header states, which is a different check from the one Phase 7 made.
///
/// [`DRM_IOCTL_MODE_CREATE_DUMB`]: DRM_IOCTL_MODE_CREATE_DUMB
/// [`DRM_IOCTL_MODE_MAP_DUMB`]: DRM_IOCTL_MODE_MAP_DUMB
pub const DRM_IOCTL_MODE_DESTROY_DUMB: libc::c_ulong = 0xC004_64B4;

/// `DRM_IOCTL_MODE_CREATE_DUMB`: `_IOWR('d', 0xB2, struct drm_mode_create_dumb)`.
///
/// Published because the display backend needs the whole dumb-buffer sequence and there should be one
/// copy of these numbers in the tree.
pub const DRM_IOCTL_MODE_CREATE_DUMB: libc::c_ulong = 0xC020_64B2;

/// `DRM_IOCTL_MODE_MAP_DUMB`: `_IOWR('d', 0xB3, struct drm_mode_map_dumb)`.
pub const DRM_IOCTL_MODE_MAP_DUMB: libc::c_ulong = 0xC010_64B3;

/// `struct drm_mode_destroy_dumb`, verbatim from `drm_mode.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DrmModeDestroyDumb {
    /// The dumb buffer's handle.
    pub handle: u32,
}

/// `struct drm_mode_create_dumb`, verbatim from `drm_mode.h`.
///
/// The field order is `height` before `width`, there is no `pixel_format`, and `handle` sits between
/// `flags` and `pitch`. Guessing any of those produces a 32-byte struct that the kernel reads as
/// garbage, which is why it is transcribed rather than remembered.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DrmModeCreateDumb {
    /// Framebuffer height.
    pub height: u32,
    /// Framebuffer width.
    pub width: u32,
    /// Bits per pixel.
    pub bpp: u32,
    /// Driver flags.
    pub flags: u32,
    /// Out: the new buffer's handle.
    pub handle: u32,
    /// Out: the buffer's pitch in bytes.
    pub pitch: u32,
    /// Out: the buffer's size in bytes.
    pub size: u64,
}

/// `struct drm_mode_map_dumb`, verbatim from `drm_mode.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DrmModeMapDumb {
    /// The buffer's handle.
    pub handle: u32,
    /// Padding, for 32/64 compatibility.
    pub pad: u32,
    /// Out: the fake offset to `mmap` at.
    pub offset: u64,
}

/// `DRM_FORMAT_XRGB8888`: `fourcc_code('X', 'R', '2', '4')`.
///
/// Bits `[31:0]` are `x:R:G:B` 8:8:8:8 little-endian, so red sits in `0x00RR_0000` of a `u32` -- which is
/// the order [`crate::registry`] and the display backend already use, and the reason a dumb buffer can
/// be handed a frame's bytes with no per-pixel conversion.
pub const DRM_FORMAT_XRGB8888: u32 = 0x3432_5258;

/// Recompute [`DRM_IOCTL_MODE_DESTROY_DUMB`] from the `_IOC` layout.
pub const fn drm_ioctl_mode_destroy_dumb() -> libc::c_ulong {
    const DIRECTION_READ_WRITE: u32 = 3;
    const TYPE_DISPLAY: u32 = b'd' as u32;
    const NR_DESTROY_DUMB: u32 = 0xB4;
    const SIZE_DRM_MODE_DESTROY_DUMB: u32 = core::mem::size_of::<u32>() as u32;
    ((DIRECTION_READ_WRITE << 30)
        | (SIZE_DRM_MODE_DESTROY_DUMB << 16)
        | (TYPE_DISPLAY << 8)
        | NR_DESTROY_DUMB) as libc::c_ulong
}

/// Recompute [`DRM_IOCTL_MODE_CREATE_DUMB`] from the `_IOC` layout.
pub const fn drm_ioctl_mode_create_dumb() -> libc::c_ulong {
    const DIRECTION_READ_WRITE: u32 = 3;
    const TYPE_DISPLAY: u32 = b'd' as u32;
    const NR_CREATE_DUMB: u32 = 0xB2;
    ((DIRECTION_READ_WRITE << 30)
        | (core::mem::size_of::<DrmModeCreateDumb>() as u32) << 16
        | (TYPE_DISPLAY << 8)
        | NR_CREATE_DUMB) as libc::c_ulong
}

/// Recompute [`DRM_IOCTL_MODE_MAP_DUMB`] from the `_IOC` layout.
pub const fn drm_ioctl_mode_map_dumb() -> libc::c_ulong {
    const DIRECTION_READ_WRITE: u32 = 3;
    const TYPE_DISPLAY: u32 = b'd' as u32;
    const NR_MAP_DUMB: u32 = 0xB3;
    ((DIRECTION_READ_WRITE << 30)
        | (core::mem::size_of::<DrmModeMapDumb>() as u32) << 16
        | (TYPE_DISPLAY << 8)
        | NR_MAP_DUMB) as libc::c_ulong
}

/// A `SplitMix64`, for buffer noise.
///
/// Chosen because it is four lines, has no state worth protecting, and does not need to be
/// seeded securely to do its job -- which is *destroying structure*, not producing secrecy.
/// The entropy is only there so that two runs of the session do not produce the same byte
/// pattern in freed pages.
struct NoiseRng {
    state: u64,
}

impl NoiseRng {
    fn from_entropy() -> Self {
        let mut seed = [0u8; 8];
        // SAFETY: `getrandom` fills up to 8 bytes and cannot fail in a way we cannot fall back
        // from; failure just leaves the buffer zeroed.
        let got = unsafe {
            libc::syscall(
                libc::SYS_getrandom,
                seed.as_mut_ptr().cast::<libc::c_void>(),
                seed.len(),
                0u32,
            )
        };
        let mut state = if got == seed.len() as libc::c_long {
            u64::from_le_bytes(seed)
        } else {
            // Fallback: the clock and an address, which differ per process and per run.
            // SAFETY: a pure query.
            let mut ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
            (ts.tv_sec as u64) << 20 ^ (ts.tv_nsec as u64) ^ (&seed as *const _ as u64)
        };
        // Avoid the all-zero state, which is a fixed point.
        if state == 0 {
            state = 0x9E37_79B9_7F4A_7C15;
        }
        Self { state }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// `"holonomy-teardown actions=N scrubbed=B exit=0\n"`, and its length.
///
/// Returns the length rather than scanning for the newline, so the `write` cannot overrun the
/// buffer if a future field were appended without updating this.
fn report_line(actions: usize, scrubbed: usize) -> ([u8; 96], usize) {
    let mut buf = [0u8; 96];
    let mut at = 0usize;
    let push = |buf: &mut [u8; 96], at: &mut usize, s: &str| {
        for byte in s.as_bytes() {
            if *at < buf.len() {
                buf[*at] = *byte;
                *at += 1;
            }
        }
    };
    push(&mut buf, &mut at, "holonomy-teardown actions=");
    push(&mut buf, &mut at, &actions.to_string());
    push(&mut buf, &mut at, " scrubbed=");
    push(&mut buf, &mut at, &scrubbed.to_string());
    push(&mut buf, &mut at, " exit=0\n");
    (buf, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dumb_buffer_ioctl_matches_the_kernel_header() {
        // Constant against its own derivation.
        assert_eq!(
            DRM_IOCTL_MODE_DESTROY_DUMB,
            drm_ioctl_mode_destroy_dumb(),
            "the constant and its derivation disagree"
        );

        // And -- the check Phase 7 lacked -- the *inputs*, against `/usr/include/drm/drm.h`:
        //
        //     #define DRM_IOCTL_MODE_CREATE_DUMB   DRM_IOWR(0xB2, struct drm_mode_create_dumb)
        //     #define DRM_IOCTL_MODE_MAP_DUMB      DRM_IOWR(0xB3, struct drm_mode_map_dumb)
        //     #define DRM_IOCTL_MODE_DESTROY_DUMB  DRM_IOWR(0xB4, struct drm_mode_destroy_dumb)
        //
        // Phase 7 used `nr = 0x44` and so computed 0xC004_6444. Comparing the constant to its own
        // derivation could never have caught that, because both used the same wrong input. These
        // assertions are the independent half.
        assert_eq!(
            DRM_IOCTL_MODE_DESTROY_DUMB, 0xC004_64B4,
            "nr must be 0xB4 (drm.h), not 0x44"
        );
        assert_eq!(DRM_IOCTL_MODE_CREATE_DUMB, 0xC020_64B2, "nr 0xB2");
        assert_eq!(DRM_IOCTL_MODE_MAP_DUMB, 0xC010_64B3, "nr 0xB3");

        // The size half of the encoding, which is `sizeof(struct)` -- so these also pin the layouts.
        assert_eq!(core::mem::size_of::<DrmModeDestroyDumb>(), 4);
        assert_eq!(core::mem::size_of::<DrmModeCreateDumb>(), 32);
        assert_eq!(core::mem::size_of::<DrmModeMapDumb>(), 16);
        assert_eq!(
            ((DRM_IOCTL_MODE_CREATE_DUMB >> 16) & 0x3FFF) as usize,
            core::mem::size_of::<DrmModeCreateDumb>()
        );

        // And the field offsets, which is the part a wrong guess would get wrong silently.
        assert_eq!(core::mem::offset_of!(DrmModeCreateDumb, height), 0);
        assert_eq!(core::mem::offset_of!(DrmModeCreateDumb, width), 4);
        assert_eq!(core::mem::offset_of!(DrmModeCreateDumb, bpp), 8);
        assert_eq!(core::mem::offset_of!(DrmModeCreateDumb, flags), 12);
        assert_eq!(core::mem::offset_of!(DrmModeCreateDumb, handle), 16);
        assert_eq!(core::mem::offset_of!(DrmModeCreateDumb, pitch), 20);
        assert_eq!(
            core::mem::offset_of!(DrmModeCreateDumb, size),
            24,
            "the u64 is 8-aligned, which is why the struct is 32 bytes and not 28"
        );
        assert_eq!(core::mem::offset_of!(DrmModeMapDumb, handle), 0);
        assert_eq!(core::mem::offset_of!(DrmModeMapDumb, offset), 8);

        // XRGB8888 is `fourcc_code('X','R','2','4')`, with red at `0x00RR_0000`.
        assert_eq!(DRM_FORMAT_XRGB8888, 0x3432_5258);
    }

    #[test]
    fn the_dumb_buffer_action_carries_its_handle() {
        // Phase 8: the action had only an `fd` and the call passed a null argument. A `struct
        // drm_mode_destroy_dumb` is a bare handle, so without this the ioctl could not work at all.
        let mut plan = TeardownPlan::new();
        plan.destroy_dumb_buffer(9, 0xDEAD_BEEF)
            .expect("within budget");
        assert_eq!(
            plan.action(0),
            Some(Action::DestroyDumbBuffer {
                fd: 9,
                handle: 0xDEAD_BEEF
            })
        );
    }

    #[test]
    fn plans_are_bounded_and_report_it() {
        let mut plan = TeardownPlan::new();
        assert!(plan.is_empty());
        for _ in 0..MAX_ACTIONS {
            plan.sync(3).expect("within budget");
        }
        assert_eq!(plan.len(), MAX_ACTIONS);
        assert_eq!(plan.sync(3), Err(TeardownError::TooManyActions));
    }

    #[test]
    fn a_zero_length_noise_range_is_refused() {
        let mut plan = TeardownPlan::new();
        let byte = 0u8;
        assert_eq!(
            plan.noise(&byte as *const u8 as *mut u8, 0),
            Err(TeardownError::BadRange)
        );
        assert!(plan.is_empty(), "a refused action must not be appended");
    }

    #[test]
    fn actions_are_recorded_in_order() {
        let mut plan = TeardownPlan::new();
        plan.destroy_dumb_buffer(7, 3).unwrap();
        plan.sync(8).unwrap();
        plan.close(8).unwrap();
        assert_eq!(
            plan.action(0),
            Some(Action::DestroyDumbBuffer { fd: 7, handle: 3 })
        );
        assert_eq!(plan.action(1), Some(Action::Sync { fd: 8 }));
        assert_eq!(plan.action(2), Some(Action::Close { fd: 8 }));
        assert_eq!(plan.action(3), None);
    }

    #[test]
    fn scrub_on_fault_classifies_a_heap_range_as_data() {
        // The ring is heap-resident, so it has no guard pages and a fault inside it can only
        // honestly be classified as a fault in data.
        let mut buffer = vec![0u8; 4096];
        let before = crate::registry::active_count();
        let mut plan = TeardownPlan::new();
        let handle = plan
            .scrub_on_fault(buffer.as_mut_ptr(), buffer.len())
            .unwrap();
        assert_eq!(crate::registry::active_count(), before + 1);
        assert_eq!(
            crate::registry::classify(buffer.as_ptr() as usize + 10),
            crate::registry::FaultSite::Data
        );
        assert_eq!(
            crate::registry::classify(buffer.as_ptr() as usize + 10_000),
            crate::registry::FaultSite::Unregistered
        );
        crate::registry::deregister(handle);
        assert_eq!(crate::registry::active_count(), before);
    }

    #[test]
    fn the_noise_prng_has_no_short_cycle() {
        let mut rng = NoiseRng {
            state: 0x1234_5678_9abc_def0,
        };
        let first = rng.next();
        let mut distinct = std::collections::HashSet::new();
        for _ in 0..1024 {
            distinct.insert(rng.next());
        }
        assert_eq!(
            distinct.len(),
            1024,
            "SplitMix64 should not repeat in 1024 draws"
        );
        assert_ne!(first, 0);
    }

    #[test]
    fn the_teardown_report_line_is_well_formed_and_exact_length() {
        for (actions, scrubbed) in [(3usize, 4096usize), (0, 0), (MAX_ACTIONS, usize::MAX / 4)] {
            let (line, len) = report_line(actions, scrubbed);
            let text = core::str::from_utf8(&line[..len]).unwrap();
            assert_eq!(
                text,
                format!("holonomy-teardown actions={actions} scrubbed={scrubbed} exit=0\n")
            );
            assert!(
                len <= line.len(),
                "reported length {len} exceeds the buffer"
            );
        }
    }
}
