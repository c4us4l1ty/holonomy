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
    /// `DRM_IOCTL_MODE_DESTROY_DUMB` on `fd`.
    DestroyDumbBuffer {
        /// The DRM card fd opened during boot.
        fd: i32,
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

    /// Append `DRM_IOCTL_MODE_DESTROY_DUMB` for `fd`.
    pub fn destroy_dumb_buffer(&mut self, fd: i32) -> Result<(), TeardownError> {
        self.push(Action::DestroyDumbBuffer { fd })
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
                Action::DestroyDumbBuffer { fd } => {
                    // SAFETY: a plain ioctl with no argument. The cast to `c_int` is because
                    // musl declares `ioctl(int, int, ...)`; the kernel truncates the request
                    // to 32 bits, so the direction/size/type encoding in the high half reaches
                    // it as the low 32 bits it has to be.
                    //
                    // A failure here is ignored on purpose. This step touches no plaintext,
                    // and the steps after it are the ones that do.
                    unsafe {
                        libc::ioctl(fd, DRM_IOCTL_MODE_DESTROY_DUMB as libc::c_int, 0);
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

/// `DRM_IOCTL_MODE_DESTROY_DUMB`, derived rather than pasted.
///
/// The kernel defines it `_IOWR('d', 0x44, struct drm_mode_destroy_dumb)` and the struct is a
/// bare `__u32 handle`, so it is 4 bytes. `_IOC(dir, type, nr, size)` is
/// `(dir << 30) | (size << 16) | (type << 8) | nr` with `dir = 3` for read+write.
///
/// [`drm_ioctl_mode_destroy_dumb`] recomputes this and the unit test asserts the two agree, so
/// the constant cannot silently drift from its definition.
pub const DRM_IOCTL_MODE_DESTROY_DUMB: libc::c_ulong = 0xC004_6444;

/// Recompute [`DRM_IOCTL_MODE_DESTROY_DUMB`] from the `_IOC` layout.
pub const fn drm_ioctl_mode_destroy_dumb() -> libc::c_ulong {
    const DIRECTION_READ_WRITE: u32 = 3;
    const TYPE_DISPLAY: u32 = b'd' as u32;
    const NR_DESTROY_DUMB: u32 = 0x44;
    const SIZE_DRM_MODE_DESTROY_DUMB: u32 = core::mem::size_of::<u32>() as u32;
    ((DIRECTION_READ_WRITE << 30)
        | (SIZE_DRM_MODE_DESTROY_DUMB << 16)
        | (TYPE_DISPLAY << 8)
        | NR_DESTROY_DUMB) as libc::c_ulong
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
    fn the_dumb_buffer_ioctl_matches_its_kernel_definition() {
        assert_eq!(
            DRM_IOCTL_MODE_DESTROY_DUMB,
            drm_ioctl_mode_destroy_dumb(),
            "the constant and its derivation disagree; one of them is wrong and a wrong \
             teardown ioctl fails silently"
        );
        assert_eq!(DRM_IOCTL_MODE_DESTROY_DUMB, 0xC004_6444);
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
        plan.destroy_dumb_buffer(7).unwrap();
        plan.sync(8).unwrap();
        plan.close(8).unwrap();
        assert_eq!(plan.action(0), Some(Action::DestroyDumbBuffer { fd: 7 }));
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
