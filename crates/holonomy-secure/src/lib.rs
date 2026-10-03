//! `SecureBlock`: page-locked, guard-bounded memory that can be proven scrubbed.
//!
//! # What this is for
//!
//! Plaintext document text and derived keys must not be reachable by anything that
//! survives the process. A `Vec<u8>` is not good enough for four separate reasons, and
//! each maps to one call here:
//!
//! | risk | call | why it matters
//! |---|---|---|
//! | plaintext written to swap | `mlock` | NFR-3: no byte of process memory may swap
//! | plaintext in a core dump | `madvise(MADV_DONTDUMP)` | FR-5.5, TC-MEM-02
//! | plaintext inherited by a child | `madvise(MADV_DONTFORK)` | no `fork` survives the jail, but assume one happens
//! | a stray pointer walking off the end | `mprotect(PROT_NONE)` guards | TC-MEM-02: exit 137
//!
//! # The guards are the point
//!
//! The mapping is `[guard_lo][data][guard_hi]` with both guards `PROT_NONE`. An overrun
//! of even one byte faults instead of silently corrupting whatever it lands on. That is
//! the whole difference between a memory bug and a memory bug that gets *reported*.
//!
//! Note that `mlock` and `MADV_DONTDUMP` are hardening, not correctness: they narrow the
//! window but neither is a guarantee. The guard pages *are* the guarantee, and they are
//! the only part of this type that can be asserted.
//!
//! Lands in Phase 1. Gate: `cargo test -p holonomy-secure`, including a test that
//! allocates 4096 bytes and proves both neighbouring pages fault.
//! See PROJECT.md §5 Phase 1 and PRD §8.1 TC-MEM-02.

use core::ptr;
use core::sync::atomic::compiler_fence;

use zeroize::Zeroize;

/// Why a `SecureBlock` could not be constructed.
///
/// A closed enum on purpose (PROJECT.md §3, `error.rs`): a sandboxed process gets an
/// error type it can exhaustively match, not a catch-all that swallows the fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecureBlockError {
    /// `sysconf(_SC_PAGESIZE)` failed. Effectively unreachable; reported rather than
    /// assumed, because a wrong page size silently breaks guard alignment.
    PageSizeUnavailable,
    /// `mmap` returned `MAP_FAILED`. `RLIMIT_AS` or `RLIMIT_MEMLOCK` exhausted.
    MmapFailed,
    /// `mlock` failed, almost always `RLIMIT_MEMLOCK`.
    MlockFailed,
    /// `madvise` rejected the flags. Non-fatal for the security posture but not
    /// silently ignored either.
    MadviseFailed,
    /// `mprotect` failed on a guard page. If this fails the block is not safe to use,
    /// so construction fails rather than degrading.
    MprotectFailed,
    /// A zero-length block has no data page and therefore no meaningful guards.
    ZeroLength,
}

impl core::fmt::Display for SecureBlockError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            Self::PageSizeUnavailable => "page size unavailable",
            Self::MmapFailed => "mmap failed",
            Self::MlockFailed => "mlock failed (RLIMIT_MEMLOCK?)",
            Self::MadviseFailed => "madvise failed",
            Self::MprotectFailed => "mprotect failed on guard page",
            Self::ZeroLength => "zero-length block",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for SecureBlockError {}

/// A page-locked, guard-bounded, scrub-on-drop byte buffer.
///
/// Not `Clone` and not `Copy`: duplicating the contents of a `SecureBlock` would defeat
/// the scrub-on-drop guarantee, because one copy would outlive it.
pub struct SecureBlock {
    /// Base of the whole mapping, i.e. the first guard page. Freed by `Drop`.
    base: *mut u8,
    /// Usable data length requested by the caller. Slice length, not mapping length.
    len: usize,
    /// Total mapped length including both guard pages. Passed to `munmap`.
    total: usize,
}

/// The host page size, in bytes.
///
/// Cached in an `AtomicUsize` because the test harness and the KDF hot path both ask
/// repeatedly and `sysconf` is a real call. `0` means "not yet read", which is
/// distinguishable from a genuine page size of 0 because a page size of 0 is impossible.
static PAGE_SIZE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Page size in bytes, or `None` if the kernel would not say.
pub fn page_size() -> Option<usize> {
    let cached = PAGE_SIZE.load(std::sync::atomic::Ordering::Relaxed);
    if cached != 0 {
        return Some(cached);
    }
    // SAFETY: `sysconf` is a pure query with no preconditions.
    let raw = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if raw <= 0 {
        return None;
    }
    let size = raw as usize;
    PAGE_SIZE.store(size, std::sync::atomic::Ordering::Relaxed);
    Some(size)
}

impl SecureBlock {
    /// Allocate a guard-bounded, page-locked block of `len` bytes.
    ///
    /// `len` need not be a multiple of the page size; the data region is rounded up and
    /// the returned slice is exactly `len` bytes, so the padding is never observable.
    pub fn allocate(len: usize) -> Result<Self, SecureBlockError> {
        if len == 0 {
            return Err(SecureBlockError::ZeroLength);
        }
        let page = page_size().ok_or(SecureBlockError::PageSizeUnavailable)?;

        let data_pages = len.div_ceil(page);
        let data_len = data_pages * page;
        let total = (data_pages + 2) * page;

        // SAFETY: `total` is non-zero and page-aligned by construction. `PROT_NONE` for
        // the whole mapping first, so the kernel cannot hand out a writable mapping
        // even briefly before the guards are set.
        let base = unsafe {
            libc::mmap(
                ptr::null_mut(),
                total,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(SecureBlockError::MmapFailed);
        }
        // From here on every early return must `munmap`, or we leak the mapping.
        let base = base as *mut u8;
        let block = Self { base, len, total };
        block.initialise(page, data_len)?;
        Ok(block)
    }

    /// Unmap the mapping and scrub the data region. Called by `allocate` and must run
    /// on every failure path.
    fn initialise(&self, page: usize, data_len: usize) -> Result<(), SecureBlockError> {
        let data = unsafe { self.base.add(page) };

        // Make the data region accessible. The guards stay PROT_NONE.
        // SAFETY: `data` is inside the mapping (`page < total`) and spans `data_len`
        // bytes, which is exactly the region reserved for it.
        if unsafe { libc::mprotect(data.cast(), data_len, libc::PROT_READ | libc::PROT_WRITE) } != 0
        {
            // SAFETY: `base`/`total` are the live mapping.
            unsafe { libc::munmap(self.base.cast(), self.total) };
            return Err(SecureBlockError::MprotectFailed);
        }

        // NFR-3: this block must never reach swap. This is the call that fails on a
        // host with a low RLIMIT_MEMLOCK, which is why the error is surfaced rather than
        // logged and ignored.
        // SAFETY: region is mapped and writable.
        if unsafe { libc::mlock(data.cast(), data_len) } != 0 {
            // SAFETY: `base`/`total` are the live mapping.
            unsafe { libc::munmap(self.base.cast(), self.total) };
            return Err(SecureBlockError::MlockFailed);
        }

        // Exclude from core dumps and from any future child. Advisory, so a failure is
        // reported but does not make the block unusable -- the guards are what actually
        // guarantee containment.
        //
        // Two separate calls, never one OR'd value. `madvise` takes a single advice
        // argument, not a bitmask: the kernel switches on the exact value and returns
        // EINVAL for a combination like MADV_DONTDUMP|MADV_DONTFORK. Plan.md Part 4
        // writes them OR'd together, which is a notational trap rather than a fact
        // about the syscall.
        // SAFETY: region is mapped; MADV_* takes a length, not a validity contract.
        for advice in [libc::MADV_DONTDUMP, libc::MADV_DONTFORK] {
            if unsafe { libc::madvise(data.cast(), data_len, advice) } != 0 {
                // SAFETY: `base`/`total` are the live mapping.
                unsafe { libc::munmap(self.base.cast(), self.total) };
                return Err(SecureBlockError::MadviseFailed);
            }
        }

        Ok(())
    }

    /// Pointer to the first data byte. Never null for a live block.
    ///
    /// This is the pointer that the PRD's TC-MEM-02 deliberately underflows to prove the
    /// guard page faults.
    pub fn as_ptr(&self) -> *const u8 {
        let page = page_size().expect("page size was resolved in allocate");
        // SAFETY: `base + page` is the start of the data region.
        unsafe { self.base.add(page) }
    }

    /// Mutable pointer to the first data byte.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.as_ptr() as *mut u8
    }

    /// The block's contents as an immutable slice, exactly `len` bytes.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: the data region is mapped, readable, and `len` bytes of it were
        // reserved. `&self` means no live `&mut` to this block exists.
        unsafe { core::slice::from_raw_parts(self.as_ptr(), self.len) }
    }

    /// The block's contents as a mutable slice, exactly `len` bytes.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        let len = self.len;
        // SAFETY: the data region is mapped read/write and `len` bytes were reserved.
        // `&mut self` guarantees exclusivity.
        unsafe { core::slice::from_raw_parts_mut(self.as_mut_ptr(), len) }
    }

    /// Requested length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// A `SecureBlock` is never empty; `allocate` rejects zero length.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Overwrite the data region with zeroes and return the length that was scrubbed.
    ///
    /// Uses `zeroize`, which writes through a volatile pointer so the compiler and the
    /// CPU cannot eliminate or defer the stores.
    ///
    /// `Drop` calls this too. Calling it explicitly is for the cases where you need the
    /// scrub to have happened *before* some other action — the Phase 7 teardown path,
    /// for instance, scrubs and then overwrites the ring buffer with noise.
    pub fn zeroize_and_release(&mut self) -> usize {
        let scrubbed = self.len;
        self.as_mut_slice().zeroize();
        scrubbed
    }

    /// Total bytes mapped, including both guard pages. Useful for RSS accounting.
    pub fn mapped_len(&self) -> usize {
        self.total
    }

    /// Number of guard pages: always 2, one below and one above.
    pub const fn guard_pages() -> usize {
        2
    }
}

// SAFETY: the block owns its mapping exclusively and hands out slices only through
// `&self`/`&mut self`. There is no interior mutability and no aliasing handle, so
// moving the owning value between threads cannot create a data race.
unsafe impl Send for SecureBlock {}
// SAFETY: as above. Shared access yields only `&[u8]`, which is itself `Sync`.
unsafe impl Sync for SecureBlock {}

impl Drop for SecureBlock {
    fn drop(&mut self) {
        // Scrub first. `zeroize`'s volatile writes mean the stores cannot be elided.
        self.zeroize_and_release();

        // Stop the compiler from reordering the scrub *after* the unmap, which would
        // make it meaningless. Release ordering plus an explicit fence: the stores must
        // be visible before the mapping goes away.
        compiler_fence(core::sync::atomic::Ordering::SeqCst);

        // SAFETY: `base`/`total` are the live mapping created in `allocate`, and
        // `Drop` runs exactly once per value. No slice into the data region may still be
        // alive, because the only ways to obtain one borrow `self`.
        unsafe { libc::munmap(self.base.cast(), self.total) };
    }
}

impl core::fmt::Debug for SecureBlock {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Deliberately does not print contents.
        f.debug_struct("SecureBlock")
            .field("len", &self.len)
            .field("mapped_len", &self.mapped_len())
            .finish_non_exhaustive()
    }
}

// Re-exported so downstream crates get the ordering primitive from one place.
pub use core::sync::atomic::compiler_fence as scrub_barrier;

// Keeps the workspace-wide dependency edge on the jail explicit.
pub use holonomy_jail::PHASE_0_PLACEHOLDER;

#[cfg(test)]
mod tests;
