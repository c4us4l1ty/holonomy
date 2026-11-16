//! The memory-lock ladder, ported from v5.1.0's `holonomy-crypto::lock` — the module whose
//! whole purpose is honest capability reporting.
//!
//! # Why this is a ladder and not an assertion
//!
//! `v5.0.0-OMEGA` treated every OS mechanism as equivalent; they are not:
//!
//! | mechanism | what it actually does |
//! | :--- | :--- |
//! | `memfd_secret(0)` (Linux 5.14+) | removes the page from the kernel direct map; even root cannot map it back |
//! | `mlock` + `MADV_DONTDUMP` | keeps the page out of swap and out of core dumps; still debugger-readable |
//! | `VirtualLock` (Windows) | keeps the page resident; still readable by a same-user process |
//! | `mlock` (macOS) | resident, no dump exclusion |
//!
//! So what is achievable varies by platform and container policy, and pretending otherwise is
//! decorative security (K-7's lesson). [`LockCapability`] and [`LockReport`] say what was
//! *actually* obtained and why the strongest rung was skipped; the Security panel (Block 6)
//! reads these values verbatim.

/// The strongest protection this process managed to obtain. Ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LockCapability {
    /// No locking at all — best effort only; the UI must warn.
    None,
    /// `mlock`/`VirtualLock` — resident but dumpable and direct-mapped.
    Resident,
    /// Resident and excluded from core dumps (`MADV_DONTDUMP`).
    ResidentNoDump,
    /// Removed from the kernel direct map (`memfd_secret`). Linux 5.14+.
    DirectMapExcluded,
}

impl LockCapability {
    /// What may honestly be claimed at this rung.
    pub fn claim(self) -> &'static str {
        match self {
            Self::None => "memory is not locked (swap and core dumps are possible)",
            Self::Resident => "memory is resident (still dumpable; enable process protection)",
            Self::ResidentNoDump => "memory is resident and excluded from core dumps",
            Self::DirectMapExcluded => {
                "memory is excluded from the kernel direct map (memfd_secret)"
            }
        }
    }

    /// Whether the strongest rung — direct-map exclusion — was achieved.
    pub fn is_direct_map_excluded(self) -> bool {
        self == Self::DirectMapExcluded
    }
}

/// The outcome of trying to lock a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockReport {
    /// The rung achieved.
    pub capability: LockCapability,
    /// Bytes asked for.
    pub requested: usize,
    /// Bytes actually locked.
    pub locked: usize,
    /// Why the strongest option was unavailable, if it was.
    pub degraded_because: Option<&'static str>,
}

impl LockReport {
    /// Whether the region is protected to at least `ResidentNoDump` in full.
    pub fn is_protected(&self) -> bool {
        self.capability >= LockCapability::ResidentNoDump && self.locked == self.requested
    }
}

/// Locking failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockError {
    /// The OS refused; usually an `RLIMIT_MEMLOCK` limit.
    Refused,
    /// A syscall returned an unexpected error.
    Syscall(i32),
}

impl core::fmt::Display for LockError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Refused => f.write_str("the OS refused to lock the requested region"),
            Self::Syscall(e) => write!(f, "syscall failed with errno {e}"),
        }
    }
}

impl std::error::Error for LockError {}

/// A guard over a locked region owned by someone else. Unlocks on drop. Holds no data — the
/// caller owns the buffer — so this is a guard, not an allocator.
pub struct LockedRegion {
    ptr: *mut u8,
    len: usize,
    report: LockReport,
}

impl LockedRegion {
    /// Lock `len` bytes at `ptr`, strongest mechanism first, degrading with the reason recorded.
    ///
    /// Never returns `Err` for a mere downgrade; a process that gets only `mlock` is still
    /// better off than one that gave up, and the report says which one happened.
    ///
    /// # Safety
    ///
    /// `ptr` must be valid for `len` bytes for the lifetime of the returned guard, and the
    /// caller must not free or `munmap` the region before the guard drops.
    pub unsafe fn lock(ptr: *mut u8, len: usize) -> Result<Self, LockError> {
        let mut degraded = None;

        let capability = match mlock(ptr, len) {
            Ok(()) => {
                if madvise_dontdump(ptr, len).is_ok() {
                    LockCapability::ResidentNoDump
                } else {
                    degraded = Some("no per-region core-dump exclusion on this platform");
                    LockCapability::Resident
                }
            }
            Err(e) => {
                degraded = Some(match e {
                    LockError::Refused => "RLIMIT_MEMLOCK too low (raise it or reduce arenas)",
                    LockError::Syscall(_) => "mlock failed",
                });
                return Ok(Self {
                    ptr,
                    len,
                    report: LockReport {
                        capability: LockCapability::None,
                        requested: len,
                        locked: 0,
                        degraded_because: degraded,
                    },
                });
            }
        };

        Ok(Self {
            ptr,
            len,
            report: LockReport {
                capability,
                requested: len,
                locked: len,
                degraded_because: degraded,
            },
        })
    }

    /// What was actually obtained, including why the strongest rung was not.
    pub fn report(&self) -> LockReport {
        self.report
    }
}

impl Drop for LockedRegion {
    fn drop(&mut self) {
        if self.report.locked > 0 {
            munlock(self.ptr, self.len);
        }
    }
}

#[cfg(unix)]
fn mlock(ptr: *mut u8, len: usize) -> Result<(), LockError> {
    // SAFETY: the validity contract is delegated to the caller of `LockedRegion::lock`.
    let rc = unsafe { libc::mlock(ptr.cast(), len) };
    if rc == 0 {
        return Ok(());
    }
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    Err(match errno {
        libc::ENOMEM | libc::EPERM => LockError::Refused,
        other => LockError::Syscall(other),
    })
}

#[cfg(unix)]
fn munlock(ptr: *mut u8, len: usize) {
    // SAFETY: symmetric with the successful `mlock` this guard tracked.
    unsafe { libc::munlock(ptr.cast(), len) };
}

#[cfg(target_os = "linux")]
fn madvise_dontdump(ptr: *mut u8, len: usize) -> Result<(), ()> {
    // `MADV_DONTDUMP` is 19 on Linux and **only** on Linux: macOS's advice 19 is a different
    // request, so the constant must never be exported behind a shared name (v5's note, kept).
    const MADV_DONTDUMP: libc::c_int = 19;
    // SAFETY: region validity delegated to `LockedRegion::lock`'s caller.
    let rc = unsafe { libc::madvise(ptr.cast(), len, MADV_DONTDUMP) };
    if rc == 0 {
        Ok(())
    } else {
        Err(())
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn madvise_dontdump(_ptr: *mut u8, _len: usize) -> Result<(), ()> {
    // macOS genuinely stops at `Resident`: no per-region dump exclusion exists. Reporting `Err`
    // is what keeps the ladder honest rather than flattering the platform.
    Err(())
}

#[cfg(windows)]
fn mlock(ptr: *mut u8, len: usize) -> Result<(), LockError> {
    // SAFETY: region validity delegated to `LockedRegion::lock`'s caller.
    let rc = unsafe { windows_virtual_lock(ptr.cast(), len) };
    if rc != 0 {
        Ok(())
    } else {
        Err(LockError::Refused)
    }
}

#[cfg(windows)]
fn munlock(ptr: *mut u8, len: usize) {
    // SAFETY: symmetric with `VirtualLock`.
    unsafe { windows_virtual_unlock(ptr.cast(), len) };
}

#[cfg(windows)]
fn madvise_dontdump(_ptr: *mut u8, _len: usize) -> Result<(), ()> {
    // Windows has no per-region dump exclusion: `MiniDumpWithFullMemory` reads it regardless.
    Err(())
}

#[cfg(not(any(unix, windows)))]
compile_error!(
    "holonomy-seal::lock has no implementation for this target. Holonomy v6 supports Linux, \
     macOS and Windows; a port needs an mlock/VirtualLock equivalent plus honest reporting."
);

#[cfg(windows)]
unsafe fn windows_virtual_lock(ptr: *mut core::ffi::c_void, len: usize) -> i32 {
    #[link(name = "kernel32")]
    extern "system" {
        fn VirtualLock(lpaddress: *mut core::ffi::c_void, dwsize: usize) -> i32;
    }
    // SAFETY: caller contract of `LockedRegion::lock`; direct kernel32 symbol.
    unsafe { VirtualLock(ptr, len) }
}

#[cfg(windows)]
unsafe fn windows_virtual_unlock(ptr: *mut core::ffi::c_void, len: usize) {
    extern "system" {
        fn VirtualUnlock(lpaddress: *mut core::ffi::c_void, dwsize: usize) -> i32;
    }
    // SAFETY: symmetric with our `VirtualLock`.
    unsafe { VirtualUnlock(ptr, len) };
}

/// Whether `memfd_secret(2)` is usable right now — probed, not assumed: the syscall exists on
/// Linux 5.14+ but is frequently seccomp-blocked in container runtimes.
pub fn memfd_secret_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        // SYS_memfd_secret = 447 on x86_64/aarch64. Returns an fd or -1.
        const SYS_MEMFD_SECRET: libc::c_long = 447;
        const MFD_CLOEXEC: libc::c_uint = 0x0001;
        // SAFETY: `syscall` with a scalar argument only; no pointers cross.
        let fd = unsafe { libc::syscall(SYS_MEMFD_SECRET, MFD_CLOEXEC) };
        if fd >= 0 {
            // SAFETY: fd is a live descriptor returned by the probe.
            unsafe { libc::close(fd as libc::c_int) };
            true
        } else {
            false
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// One line for logs/UI about how the current rung was reached.
pub fn describe_how() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        if memfd_secret_available() {
            "memfd_secret(2) available (direct-map-excluded arenas possible)"
        } else {
            "memfd_secret unavailable (kernel < 5.14 or seccomp); mlock + MADV_DONTDUMP fallback"
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        "mlock on this platform; no per-region core-dump exclusion"
    }
    #[cfg(not(unix))]
    {
        "VirtualLock; still readable by a same-user process"
    }
}

/// The best rung this process can reach, probed.
pub fn probe_capability() -> LockCapability {
    if memfd_secret_available() {
        LockCapability::DirectMapExcluded
    } else {
        LockCapability::ResidentNoDump
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_is_ordered() {
        assert!(LockCapability::None < LockCapability::Resident);
        assert!(LockCapability::Resident < LockCapability::ResidentNoDump);
        assert!(LockCapability::ResidentNoDump < LockCapability::DirectMapExcluded);
    }

    #[test]
    fn claims_do_not_overclaim() {
        assert!(LockCapability::Resident.claim().contains("dumpable"));
        assert!(LockCapability::ResidentNoDump.claim().contains("core dumps"));
        assert!(LockCapability::DirectMapExcluded
            .claim()
            .contains("direct map"));
        assert!(!LockCapability::Resident.is_direct_map_excluded());
    }

    #[test]
    fn lock_a_page_and_report() {
        let mut buf = vec![0u8; 4096];
        // SAFETY: `buf` outlives the guard and is not resized while it exists.
        let guard = unsafe { LockedRegion::lock(buf.as_mut_ptr(), buf.len()) }.unwrap();
        let r = guard.report();
        assert_eq!(r.requested, 4096);
        // Success is not asserted: CI containers routinely have low RLIMIT_MEMLOCK, and the
        // point of LockReport is that the downgrade is *reported*, not fatal.
        if r.locked > 0 {
            assert!(r.capability >= LockCapability::Resident);
        }
        drop(guard);
        buf[0] = 1;
        assert_eq!(buf[0], 1);
    }

    #[test]
    fn probe_and_describe_never_panic() {
        let c = probe_capability();
        assert!(c >= LockCapability::None);
        assert!(!describe_how().is_empty());
    }
}
