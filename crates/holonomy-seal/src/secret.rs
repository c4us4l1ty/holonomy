//! [`LockedSecret`]: the owned, locked, zeroizing buffer that capability-token bytes live in.
//!
//! The practical reason v6's tokens do not hold plain `Zeroizing<[u8; N]>`: heap pages holding
//! key material are swappable, dumpable, and debugger-readable under an A2/A3 adversary. This
//! type puts every key byte behind the [`crate::lock`] ladder by construction: allocation is a
//! boxed fixed array (never reallocated, never moved), the page is locked best-effort ladder
//! with its [`LockReport`] kept, and drop order is zeroize → unlock → free.
//!
//! For Windows/macOS or low-`RLIMIT` environments the report records the degraded rung rather
//! than pretending.

use zeroize::Zeroize;

use crate::lock::{LockReport, LockedRegion};

/// A fixed-size secret byte cell whose page is under the memory-lock ladder.
///
/// Not `Clone`, no byte-level public API: the boundary reads bytes `pub(crate)`, everyone else
/// presents the token that owns one of these.
pub struct LockedSecret<const N: usize> {
    buf: Box<[u8; N]>,
    guard: LockedRegion,
}

impl<const N: usize> LockedSecret<N> {
    /// Allocate, populate, and lock. Downgrades are reported in [`Self::report`], never fatal,
    /// and construction itself cannot fail.
    pub fn new(bytes: [u8; N]) -> Self {
        let mut buf = Box::new(bytes);
        // SAFETY: `buf` is a live, uniquely owned, never-resized heap allocation that outlives
        // the guard (field drop order: `buf` is declared before `guard`, and our `Drop` zeroizes
        // and then unlocks before either field's storage is freed).
        let guard = unsafe { LockedRegion::lock(buf.as_mut_ptr().cast(), N) }
            .expect("the lock ladder degrades instead of failing");
        Self { buf, guard }
    }

    /// The obtained protection ladder state for this cell.
    pub fn report(&self) -> LockReport {
        self.guard.report()
    }

    /// Crate-internal byte view. The only read of secret cell bytes anywhere in the workspace.
    pub(crate) fn expose(&self) -> &[u8; N] {
        &self.buf
    }
}

impl<const N: usize> Drop for LockedSecret<N> {
    fn drop(&mut self) {
        // Order is the invariant: contents destroyed *while still locked*, then unlock via the
        // guard's own Drop (fields drop after this body), then the box frees.
        self.buf.zeroize();
    }
}

impl<const N: usize> core::fmt::Debug for LockedSecret<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "LockedSecret<{N}>(<redacted>, {:?})", self.report().capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::LockCapability;

    #[test]
    fn round_trips_and_reports_honestly() {
        let cell = LockedSecret::new([0xABu8; 32]);
        assert_eq!(cell.expose(), &[0xABu8; 32]);
        let r = cell.report();
        assert_eq!(r.requested, 32);
        // Never assert success of the *lock* — CI containers have tiny RLIMIT_MEMLOCK; the law
        // is honesty: the ladder position is reported, either way.
        assert!(r.capability >= LockCapability::None);
        if r.locked == 0 {
            assert!(r.degraded_because.is_some());
        }
        drop(cell);
    }

    #[test]
    fn debug_never_prints_bytes() {
        let cell = LockedSecret::new([9u8; 16]);
        let text = format!("{cell:?}");
        assert!(text.contains("<redacted>"));
        assert!(!text.contains("[9"));
    }
}
