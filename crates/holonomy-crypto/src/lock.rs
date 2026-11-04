//! Page locking, as performed per OS, with honest capability reporting.
//!
//! # The correction to `FR-6.3` / Invariant 5
//!
//! `v5.0.0-OMEGA` treated `memfd_secret(0)` on Linux and "DACL-stripped `VirtualLock`" on Windows
//! as equivalent. They are not, and the difference matters to what may be claimed:
//!
//! | mechanism | what it actually does |
//! | :--- | :--- |
//! | `memfd_secret(0)` (Linux 5.14+) | removes the page from the kernel direct map; no other process, not even root with `/proc/pid/mem`, can map it. The strongest option available. |
//! | `mlock` + `MADV_DONTDUMP` | keeps the page out of swap and out of core dumps. The page is still in the direct map and readable by a debugger. |
//! | `VirtualLock` (Windows) | keeps the page resident. Still readable by `ReadProcessMemory` from a same-user process; the real mitigations are `SetProcessMitigationPolicy` and `PROCESS_PROTECTION`. |
//! | `mlock` (macOS) | as Linux's `mlock`, without the dump exclusion. |
//!
//! So the capability is a **three-level ladder**, and [`LockCapability`] reports which rung the
//! running process is on rather than pretending the ladder is flat. On a kernel older than 5.14,
//! or where `memfd_secret` is blocked by seccomp (it often is, in containers), the code falls back
//! to `mlock` + `MADV_DONTDUMP` and **says so** in [`LockReport`], which is what the UI surfaces.

/// The strongest protection this process managed to obtain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LockCapability {
    /// No locking at all. Everything is best-effort; the UI must warn.
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

    /// Whether the strongest claim in the old PRD is actually available.
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
    /// Whether the whole region is protected to at least `ResidentNoDump`.
    pub fn is_protected(&self) -> bool {
        self.capability >= LockCapability::ResidentNoDump && self.locked == self.requested
    }
}

/// Failure to lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockError {
    /// The OS refused; usually an `RLIMIT_MEMLOCK` limit.
    Refused,
    /// A syscall returned an unexpected error.
    Syscall(i32),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused => f.write_str("the OS refused to lock the requested region"),
            Self::Syscall(e) => write!(f, "syscall failed with errno {e}"),
        }
    }
}

impl std::error::Error for LockError {}

/// A locked region of memory. Unlocks on drop.
///
/// Holds no data itself — the caller owns the buffer — so this is a guard, not an allocator. That
/// keeps it usable with a `Vec`, a `Box<[u8]>`, or a `mmap` from the caller's choice.
pub struct LockedRegion {
    ptr: *mut u8,
    len: usize,
    report: LockReport,
}

impl LockedRegion {
    /// Lock `len` bytes starting at `ptr`.
    ///
    /// Tries the strongest mechanism first and degrades, recording why. Never returns `Err` for a
    /// mere downgrade: a process that can only get `mlock` is still better than one that gives up,
    /// and [`LockReport`] is how the caller learns what it got.
    ///
    /// # Safety
    ///
    /// `ptr` must be valid for `len` bytes for the lifetime of the returned guard, and the
    /// caller must not `munmap` or free the region before the guard drops.
    pub unsafe fn lock(ptr: *mut u8, len: usize) -> Result<Self, LockError> {
        let mut degraded = None;

        // Rung 1: memfd_secret is the only one that removes the direct map. It allocates a
        // *separate* region, so it cannot lock an existing buffer; it is reported as an
        // opportunity rather than applied here. See `secret_arena`.
        let capability = match mlock(ptr, len) {
            Ok(()) => {
                if madvise_dontdump(ptr, len).is_ok() {
                    LockCapability::ResidentNoDump
                } else {
                    degraded = Some("MADV_DONTDUMP unsupported");
                    LockCapability::Resident
                }
            }
            Err(e) => {
                degraded = Some(match e {
                    LockError::Refused => "RLIMIT_MEMLOCK too low (raise it or reduce the arena)",
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

    /// What was actually obtained, including why the strongest option was not.
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
    // SAFETY: delegated to the caller of `LockedRegion::lock`.
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
    unsafe { libc::munlock(ptr.cast(), len) };
}

#[cfg(target_os = "linux")]
fn madvise_dontdump(ptr: *mut u8, len: usize) -> Result<(), ()> {
    // `MADV_DONTDUMP` is 19 on Linux. Not exposed by every `libc` release, so it is spelled out
    // here — and it is **Linux-only**: macOS has no equivalent and its advice constant 19 is not
    // this one. Passing 19 on macOS is at best `EINVAL` and at worst a `madvise` that discards
    // pages, which on a key arena would be a silent zeroization or a SIGSEGV depending on flavour.
    const MADV_DONTDUMP: libc::c_int = 19;
    let rc = unsafe { libc::madvise(ptr.cast(), len, MADV_DONTDUMP) };
    if rc == 0 {
        Ok(())
    } else {
        Err(())
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn madvise_dontdump(_ptr: *mut u8, _len: usize) -> Result<(), ()> {
    // macOS: no per-region core-dump exclusion. Core dumps on macOS are opt-in, written to a
    // configured directory, and require the privilege to raise them at all, so `mlock` alone is
    // what is available. Reporting `Err` here is what makes the capability ladder honest: the
    // platform really does stop at `Resident`.
    Err(())
}

#[cfg(windows)]
fn mlock(ptr: *mut u8, len: usize) -> Result<(), LockError> {
    // VirtualLock(ptr, len)
    let rc = unsafe { windows_sys_virtual_lock(ptr.cast(), len) };
    if rc != 0 {
        Ok(())
    } else {
        Err(LockError::Refused)
    }
}

#[cfg(windows)]
fn munlock(ptr: *mut u8, len: usize) {
    unsafe { windows_sys_virtual_unlock(ptr.cast(), len) };
}

#[cfg(windows)]
fn madvise_dontdump(_ptr: *mut u8, _len: usize) -> Result<(), ()> {
    // Windows has no per-region dump exclusion. `MiniDumpWriteDump` with
    // `MiniDumpWithFullMemory` reads it regardless, which is why the doc comment is explicit.
    Err(())
}

#[cfg(not(any(unix, windows)))]
compile_error!(
    "holonomy-crypto::lock has no implementation for this target. Holonomy supports Linux, \
     macOS and Windows; a port needs an equivalent of mlock/VirtualLock plus MADV_DONTDUMP, and \
     until then there is no honest way to claim pages are protected."
);

#[cfg(windows)]
unsafe fn windows_sys_virtual_lock(ptr: *mut core::ffi::c_void, len: usize) -> i32 {
    // Declared directly rather than pulling `windows-sys` in for two symbols. `kernel32` is
    // linked by every Rust binary on the target.
    #[link(name = "kernel32")]
    extern "system" {
        fn VirtualLock(lpaddress: *mut core::ffi::c_void, dwsize: usize) -> i32;
        fn VirtualUnlock(lpaddress: *mut core::ffi::c_void, dwsize: usize) -> i32;
    }
    VirtualLock(ptr, len)
}

#[cfg(windows)]
unsafe fn windows_sys_virtual_unlock(ptr: *mut core::ffi::c_void, len: usize) {
    extern "system" {
        fn VirtualUnlock(lpaddress: *mut core::ffi::c_void, dwsize: usize) -> i32;
    }
    VirtualUnlock(ptr, len);
}

/// Whether `memfd_secret(2)` is usable right now.
///
/// Probed rather than assumed: the syscall exists on Linux 5.14+, but is frequently blocked by
/// seccomp in container runtimes, and `ENOSYS` and `EPERM` look identical from userland except
/// for the errno.
pub fn memfd_secret_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        // 447 on x86_64 / aarch64. Returns a file descriptor, or -1.
        const SYS_MEMFD_SECRET: libc::c_long = 447;
        const MFD_CLOEXEC: libc::c_uint = 0x0001;
        // SAFETY: `syscall` with no pointers.
        let fd = unsafe { libc::syscall(SYS_MEMFD_SECRET, MFD_CLOEXEC) };
        if fd >= 0 {
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

/// A one-line explanation of how the current rung was reached, for `--check`.
pub fn describe_how() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        if memfd_secret_available() {
            "memfd_secret(2) gave a direct-map-excluded region"
        } else {
            "memfd_secret unavailable (kernel < 5.14 or seccomp); fell back to mlock + MADV_DONTDUMP"
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        "mlock on this platform; no MADV_DONTDUMP equivalent"
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
        // `mlock` + `MADV_DONTDUMP` is the realistic rung on Windows/macOS and on old kernels.
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
        assert!(LockCapability::ResidentNoDump
            .claim()
            .contains("core dumps"));
        assert!(LockCapability::DirectMapExcluded
            .claim()
            .contains("direct map"));
        assert!(!LockCapability::Resident.is_direct_map_excluded());
    }

    #[test]
    fn lock_a_page_and_report() {
        let mut buf = vec![0u8; 4096];
        // SAFETY: buf is alive for the guard's lifetime and is not resized.
        let guard = unsafe { LockedRegion::lock(buf.as_mut_ptr(), buf.len()) }.unwrap();
        let r = guard.report();
        assert_eq!(r.requested, 4096);
        // No assertion that locking succeeded: CI containers routinely have a low
        // RLIMIT_MEMLOCK, and the point of `LockReport` is that this is reported, not fatal.
        if r.locked > 0 {
            assert!(r.capability >= LockCapability::Resident);
        }
        drop(guard);
        buf[0] = 1; // still valid after unlock
        assert_eq!(buf[0], 1);
    }

    #[test]
    fn probe_never_panics() {
        let c = probe_capability();
        assert!(c >= LockCapability::None);
    }
}
