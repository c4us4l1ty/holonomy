//! Secret arenas: the memory the reaper is required to zero within its budget.
//!
//! # Why this is its own type
//!
//! The reaper's [`EmergencyActions::wipe_secrets`](crate::reaper::EmergencyActions::wipe_secrets)
//! has to zero "every secret" in a bounded, allocation-free, idempotent way from a thread that may
//! be the only one still runnable. That rules out walking a `Vec<Box<[u8]>>` of owned allocations:
//! the `Vec` itself can be mid-`realloc` when the fault arrives.
//!
//! So arenas are registered up front into a fixed-size table of raw `(ptr, len)` pairs behind
//! atomics, and wiping is a volatile byte-store loop over at most [`MAX_ARENAS`] entries. The
//! fixed bound is what makes the worst-case wipe time computable, which is what makes the 15 ms
//! ceiling a number rather than a hope.
//!
//! # What it does not do
//!
//! It does not free anything, map anything, or lock anything. Locking is `holonomy-crypto`'s
//! [`lock`](holonomy_crypto::lock) module, deliberately separate: the wipe path must not call
//! `munlock`, because `munlock` can take the mm semaphore and park the reaper.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// Maximum simultaneously registered arenas.
pub const MAX_ARENAS: usize = 8;

/// A fixed table of secret regions. One per process.
pub struct SecretAreas {
    /// Base addresses. `0` means free.
    bases: [AtomicUsize; MAX_ARENAS],
    /// Lengths in bytes. Published last, and only when non-zero does the slot count as live.
    lens: [AtomicUsize; MAX_ARENAS],
    /// Total bytes under management, maintained on register/unregister.
    total_bytes: AtomicU64,
    /// Set once wiped, so a second wipe is a no-op and a post-wipe register is refused.
    wiped: AtomicBool,
}

impl SecretAreas {
    /// An empty table.
    pub const fn new() -> Self {
        // `AtomicUsize` is not `Copy`, so the arrays are written out. Eight entries is small
        // enough that the repetition is clearer than a macro.
        Self {
            bases: [
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
            ],
            lens: [
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
            ],
            total_bytes: AtomicU64::new(0),
            wiped: AtomicBool::new(false),
        }
    }

    /// Register a region.
    ///
    /// # Safety
    ///
    /// `ptr` must be valid for `len` bytes, and must remain so until either
    /// [`unregister`](Self::unregister) is called or the process exits. The reaper may write to
    /// it at any time, from another thread.
    pub unsafe fn register(&self, ptr: *mut u8, len: usize) -> Result<(), ScrubError> {
        if self.wiped.load(Ordering::Acquire) {
            return Err(ScrubError::AlreadyWiped);
        }
        if len == 0 {
            return Err(ScrubError::Empty);
        }
        for i in 0..MAX_ARENAS {
            // Claim by moving base from 0 to the real address, then publish the length with
            // `Release`. A reader loads the length first (`Acquire`), so it never sees an address
            // without a length or vice versa.
            if self.bases[i]
                .compare_exchange(0, ptr as usize, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.lens[i].store(len, Ordering::Release);
                self.total_bytes.fetch_add(len as u64, Ordering::AcqRel);
                return Ok(());
            }
        }
        Err(ScrubError::TableFull)
    }

    /// Remove a region. Does not wipe it; call [`wipe`](Self::wipe) first if that is the intent.
    pub fn unregister(&self, ptr: *mut u8) {
        for i in 0..MAX_ARENAS {
            if self.bases[i].load(Ordering::Acquire) == ptr as usize {
                let len = self.lens[i].swap(0, Ordering::AcqRel);
                self.bases[i].store(0, Ordering::Release);
                self.total_bytes.fetch_sub(len as u64, Ordering::AcqRel);
                return;
            }
        }
    }

    /// Total bytes under management.
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes.load(Ordering::Acquire)
    }

    /// Number of live regions.
    pub fn live_count(&self) -> usize {
        (0..MAX_ARENAS)
            .filter(|&i| self.lens[i].load(Ordering::Acquire) > 0)
            .count()
    }

    /// Zero every registered region, then mark the table wiped.
    ///
    /// Idempotent, allocation-free, and safe to call from the reaper thread. Uses
    /// [`std::ptr::write_volatile`] so the writes are not elided — a plain `memset` over memory
    /// that is never read again may legally be optimised away, and a scrubbed arena that the
    /// compiler deleted is the classic zeroization bug.
    pub fn wipe(&self) {
        for i in 0..MAX_ARENAS {
            let len = self.lens[i].load(Ordering::Acquire);
            if len == 0 {
                continue;
            }
            let base = self.bases[i].load(Ordering::Acquire) as *mut u8;
            if base.is_null() {
                continue;
            }
            for off in 0..len {
                // SAFETY: `register` required the range to be valid for `len` bytes and to stay
                // valid; this loop stays inside it.
                unsafe { std::ptr::write_volatile(base.add(off), 0u8) };
            }
            // Fence so the zeroes are not reordered after the `wiped` flag below.
            std::sync::atomic::fence(Ordering::SeqCst);
        }
        self.wiped.store(true, Ordering::Release);
        self.total_bytes.store(0, Ordering::Release);
    }

    /// Whether the table has been wiped.
    pub fn is_wiped(&self) -> bool {
        self.wiped.load(Ordering::Acquire)
    }
}

impl Default for SecretAreas {
    fn default() -> Self {
        Self::new()
    }
}

/// Registration failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrubError {
    /// All [`MAX_ARENAS`] slots are in use.
    TableFull,
    /// The table was already wiped; the process is on its way out.
    AlreadyWiped,
    /// A zero-length region is a caller bug: it protects nothing and costs a slot.
    Empty,
}

impl std::fmt::Display for ScrubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TableFull => write!(f, "secret arena table is full ({MAX_ARENAS} regions)"),
            Self::AlreadyWiped => f.write_str("secret arenas were already wiped"),
            Self::Empty => f.write_str("cannot register a zero-length arena"),
        }
    }
}

impl std::error::Error for ScrubError {}

/// Worst-case wipe time for `bytes` at an assumed sustained write bandwidth.
///
/// `bandwidth_bytes_per_second` should come from a measurement on the host class, not from a
/// datasheet: DDR2-800 dual-channel is 12.8 GB/s theoretical and roughly 4–6 GB/s for a
/// store-only loop with no read-back. Callers pass the measured figure; [`default_bandwidth`]
/// is a conservative fallback.
pub fn worst_case_wipe_ns(bytes: u64, bandwidth_bytes_per_second: u64) -> u64 {
    if bandwidth_bytes_per_second == 0 {
        return u64::MAX;
    }
    (bytes.saturating_mul(1_000_000_000)) / bandwidth_bytes_per_second
}

/// A deliberately pessimistic bandwidth for a 2008-era dual-channel DDR2 machine.
pub const LEGACY_WIPE_BANDWIDTH_BYTES_PER_SEC: u64 = 4_000_000_000;

/// Bandwidth to use when nothing was measured.
pub fn default_bandwidth() -> u64 {
    LEGACY_WIPE_BANDWIDTH_BYTES_PER_SEC
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_wipe_and_verify() {
        let mut a = vec![0xAAu8; 4096];
        let mut b = vec![0xBBu8; 8192];
        let areas = SecretAreas::new();
        // SAFETY: both buffers outlive `areas`'s use of them in this test.
        unsafe {
            areas.register(a.as_mut_ptr(), a.len()).unwrap();
            areas.register(b.as_mut_ptr(), b.len()).unwrap();
        }
        assert_eq!(areas.live_count(), 2);
        assert_eq!(areas.total_bytes(), 4096 + 8192);

        areas.wipe();
        assert!(a.iter().all(|&x| x == 0));
        assert!(b.iter().all(|&x| x == 0));
        assert!(areas.is_wiped());
        assert_eq!(areas.total_bytes(), 0);
    }

    #[test]
    fn wipe_is_idempotent() {
        let mut a = vec![7u8; 64];
        let areas = SecretAreas::new();
        // SAFETY: as above.
        unsafe { areas.register(a.as_mut_ptr(), a.len()).unwrap() };
        areas.wipe();
        areas.wipe();
        assert!(a.iter().all(|&x| x == 0));
    }

    #[test]
    fn register_after_wipe_is_refused() {
        let mut a = vec![1u8; 64];
        let areas = SecretAreas::new();
        areas.wipe();
        // SAFETY: the pointer is valid, but registration must be refused.
        let err = unsafe { areas.register(a.as_mut_ptr(), a.len()) }.unwrap_err();
        assert_eq!(err, ScrubError::AlreadyWiped);
    }

    #[test]
    fn empty_registration_is_a_bug_not_a_no_op() {
        let areas = SecretAreas::new();
        let mut placeholder = 0u8;
        // SAFETY: zero length means we never dereference it.
        let err = unsafe { areas.register(&mut placeholder, 0) }.unwrap_err();
        assert_eq!(err, ScrubError::Empty);
    }

    #[test]
    fn table_full_is_reported() {
        let areas = SecretAreas::new();
        let mut bufs: Vec<Vec<u8>> = (0..MAX_ARENAS + 1).map(|_| vec![0u8; 8]).collect();
        for b in bufs.iter_mut().take(MAX_ARENAS) {
            // SAFETY: buffers live for the whole test.
            unsafe { areas.register(b.as_mut_ptr(), b.len()).unwrap() };
        }
        // SAFETY: as above.
        let err = unsafe { areas.register(bufs[MAX_ARENAS].as_mut_ptr(), 8) }.unwrap_err();
        assert_eq!(err, ScrubError::TableFull);
        assert!(err.to_string().contains("full"));
    }

    #[test]
    fn unregister_releases_the_slot() {
        let mut a = vec![0u8; 32];
        let areas = SecretAreas::new();
        // SAFETY: buffer outlives the guard.
        unsafe { areas.register(a.as_mut_ptr(), a.len()).unwrap() };
        areas.unregister(a.as_mut_ptr());
        assert_eq!(areas.live_count(), 0);
        assert_eq!(areas.total_bytes(), 0);
    }

    #[test]
    fn volatile_stores_are_not_elided() {
        // `write_volatile` must survive optimisation. In debug this proves nothing; in release it
        // is the difference between a scrub and a comment. `black_box` on the buffer after wiping
        // forces observation so the optimizer cannot delete the arena entirely.
        let mut a = vec![0xFFu8; 1024];
        let areas = SecretAreas::new();
        // SAFETY: buffer outlives the guard's use.
        unsafe { areas.register(a.as_mut_ptr(), a.len()).unwrap() };
        areas.wipe();
        std::hint::black_box(&a);
        assert_eq!(a.iter().filter(|&&b| b == 0).count(), 1024);
    }

    #[test]
    fn wipe_budget_for_the_real_arena() {
        // 180 MiB steady state at the pessimistic legacy bandwidth. This is the number the 15 ms
        // ceiling has to accommodate alongside the scanout zero and the compositor flush, and it
        // is why the reaper wipes *registered secrets* rather than "all of RAM".
        let bytes = 180 * 1024 * 1024;
        let ns = worst_case_wipe_ns(bytes, default_bandwidth());
        let ms = ns as f64 / 1e6;
        assert!(
            ms > 40.0,
            "180 MiB cannot be wiped in under 40 ms at 4 GB/s (got {ms:.1})"
        );
        // And the much smaller realistic secret set — the container's key material and the
        // scanout — is comfortably inside the budget.
        let realistic = 8 * 1024 * 1024;
        assert!(worst_case_wipe_ns(realistic, default_bandwidth()) < 5_000_000);
    }
}
