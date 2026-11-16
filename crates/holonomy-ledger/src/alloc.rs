//! A global allocator that counts, so allocation claims are measured rather than asserted.
//!
//! # How it is used
//!
//! A binary (test or app) installs it once:
//!
//! ```rust,ignore
//! #[global_allocator]
//! static COUNTING: holonomy_ledger::alloc::Counting = holonomy_ledger::alloc::Counting;
//! ```
//!
//! Then a claim is expressed with [`claim_quiet`] around the path under test. Blocks 4 and 6
//! use this to turn GATES law L-2 (zero keystroke-path allocation) from prose into a CI gate.
//!
//! # Why claims are scoped globally
//!
//! Counters are process-global: an allocator cannot see call sites. Scopes therefore measure
//! "everything the process allocated between two snapshots", which means counting tests must be
//! the only test in their binary, or run under `--test-threads=1`, to avoid racing
//! allocations from sibling tests being attributed to them. The integration tests that prove
//! this machinery are arranged that way on purpose.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::{Mutex, OnceLock};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static DEALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static LIVE_BYTES: AtomicU64 = AtomicU64::new(0);

/// Serializes counting scopes inside one binary; see the module docs for why this is only a
/// partial remedy and dedicated binaries are the real one.
fn scope_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// A global allocator that counts every allocation and deallocation through the system
/// allocator. Install as `#[global_allocator]`; costs one relaxed atomic per event.
pub struct Counting;

// SAFETY: event counters are relaxed atomics and only ever touched with `usize` amounts that
// the allocator contract guarantees describe live regions; the accounting cannot affect the
// soundness of the underlying system calls, which handle all actual memory guarantees.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim to the system allocator with the caller's layout.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            LIVE_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        LIVE_BYTES.fetch_sub(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: `ptr`/`layout` satisfy the contract because the caller of `dealloc` is bound
        // by the same contract as for any global allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}

/// A point-in-time counter snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocStats {
    /// Completed `alloc` calls since process start.
    pub allocations: u64,
    /// Completed `dealloc` calls since process start.
    pub deallocations: u64,
    /// Bytes currently handed out and not yet returned.
    pub live_bytes: u64,
}

impl AllocStats {
    /// Snapshot the global counters.
    pub fn snapshot() -> Self {
        Self {
            allocations: ALLOCATIONS.load(Ordering::Relaxed),
            deallocations: DEALLOCATIONS.load(Ordering::Relaxed),
            live_bytes: LIVE_BYTES.load(Ordering::Relaxed),
        }
    }

    /// What happened between `self` (earlier) and `later`.
    pub fn delta(&self, later: &AllocStats) -> AllocDelta {
        AllocDelta {
            allocations: later.allocations - self.allocations,
            deallocations: later.deallocations - self.deallocations,
        }
    }
}

/// The measured difference between two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocDelta {
    /// `alloc` calls observed in the window.
    pub allocations: u64,
    /// `dealloc` calls observed in the window.
    pub deallocations: u64,
}

impl fmt::Display for AllocDelta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} allocation(s), {} deallocation(s) in scope",
            self.allocations, self.deallocations
        )
    }
}

/// Run `f` and return its result, asserting the process performed **zero** allocations while
/// it ran. This is the gate primitive behind GATES law L-2 (zero keystroke-path allocation).
///
/// # Panics
///
/// Panics with the measured [`AllocDelta`] if any allocation occurred.
///
/// # Reliability
///
/// Counters are process-global. Use in a dedicated test binary or under
/// `--test-threads=1`; sibling tests running concurrently can allocate in the window. The
/// caller holds a mutex serializing overlapping [`claim_quiet`] scopes, but only real code —
/// not the tester — must allocate in a passing run anyway.
pub fn claim_quiet<T>(f: impl FnOnce() -> T) -> T {
    let _guard = scope_lock().lock().expect("scope lock");
    let before = AllocStats::snapshot();
    let out = f();
    let delta = before.delta(&AllocStats::snapshot());
    assert!(
        delta.allocations == 0,
        "allocation-free claim failed: {delta}"
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hint::black_box;

    // NOTE: counting is proven in `tests/alloc_counting.rs`, which installs `Counting` as the
    // binary's `#[global_allocator]`. A unit test here would run under the default allocator
    // and measure nothing — the crate-not-installed failure mode bit exactly that way in CI.

    #[test]
    fn quiet_arithmetic_passes() {
        let sum = claim_quiet(|| {
            let mut acc = 0u64;
            for i in 0..1000 {
                acc = black_box(acc.wrapping_add(black_box(i)));
            }
            acc
        });
        assert_eq!(sum, 499_500);
    }
}
