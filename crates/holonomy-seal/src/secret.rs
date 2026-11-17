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

use zeroize::{Zeroize, Zeroizing};

use crate::lock::{LockReport, LockedRegion};

// Linux/macOS/Windows page sizes are 4, 16, or 64 KiB on the supported targets. Give each
// secret its own 64 KiB-aligned allocation so two small cells cannot share an OS lock page:
// `munlock` is page-granular and otherwise dropping one box could unlock a neighboring key.
#[repr(C, align(65536))]
struct PageAlignedCell<const N: usize>([u8; N]);

/// A fixed-size secret byte cell whose page is under the memory-lock ladder.
///
/// The padded, 64 KiB-aligned allocation isolates its OS page-lock unit from other cells.
/// Not `Clone`, no byte-level public API: the boundary reads bytes `pub(crate)`, everyone else
/// presents the token that owns one of these.
pub struct LockedSecret<const N: usize> {
    // Struct fields drop in declaration order. Keep the guard first so it unlocks the still-live
    // allocation after this type's Drop body has zeroized it, and only then free the box.
    guard: LockedRegion,
    buf: Box<PageAlignedCell<N>>,
}

impl<const N: usize> LockedSecret<N> {
    /// Allocate, populate, and lock. Downgrades are reported in [`Self::report`], never fatal,
    /// and construction itself cannot fail.
    pub fn new(mut bytes: [u8; N]) -> Self {
        let input = Zeroizing::new(bytes);
        bytes.zeroize();
        let mut buf = Box::new(PageAlignedCell([0u8; N]));
        // Lock the isolated page before copying in the key; a refused rung remains visible in
        // the report, but the normal path never leaves a populated heap cell unlocked.
        // SAFETY: `buf` is a live, uniquely owned, never-resized heap allocation that outlives
        // the guard. `guard` is declared first and therefore drops after this type's Drop body
        // has wiped `buf.0`, but before the box frees its storage.
        let guard = unsafe { LockedRegion::lock(buf.0.as_mut_ptr(), N) }
            .expect("the lock ladder degrades instead of failing");
        buf.0.copy_from_slice(&input[..]);
        // Erase the guarded input copy before returning the new token.
        drop(input);
        #[cfg(test)]
        crate::allocation_audit::watch(buf.0.as_ptr(), N);
        Self { guard, buf }
    }

    /// The obtained protection ladder state for this cell.
    pub fn report(&self) -> LockReport {
        self.guard.report()
    }

    /// Crate-internal byte view. The only read of secret cell bytes anywhere in the workspace.
    pub(crate) fn expose(&self) -> &[u8; N] {
        &self.buf.0
    }
}

impl<const N: usize> Drop for LockedSecret<N> {
    fn drop(&mut self) {
        // Order is the invariant: contents destroyed *while still locked*, then unlock via the
        // guard's own Drop (fields drop after this body), then the box frees.
        self.buf.0.zeroize();
    }
}

impl<const N: usize> core::fmt::Debug for LockedSecret<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "LockedSecret<{N}>(<redacted>, {:?})",
            self.report().capability
        )
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
    fn secret_cells_have_disjoint_page_lock_units() {
        let first = LockedSecret::new([0x11u8; 32]);
        let second = LockedSecret::new([0x22u8; 32]);
        let first_address = first.buf.0.as_ptr() as usize;
        let second_address = second.buf.0.as_ptr() as usize;
        assert_eq!(first_address % 65_536, 0);
        assert_eq!(second_address % 65_536, 0);
        assert_ne!(first_address / 65_536, second_address / 65_536);
    }

    #[test]
    fn debug_never_prints_bytes() {
        let cell = LockedSecret::new([9u8; 16]);
        let text = format!("{cell:?}");
        assert!(text.contains("<redacted>"));
        assert!(!text.contains("[9"));
    }
}
