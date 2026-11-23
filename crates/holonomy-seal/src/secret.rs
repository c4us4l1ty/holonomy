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

use core::cell::UnsafeCell;
use core::ptr::NonNull;
use core::sync::atomic::{compiler_fence, Ordering};
use std::sync::Arc;

use zeroize::{Zeroize, Zeroizing};

use crate::lock::{LockReport, LockedRegion};
use crate::reaper::{ArenaSlot, SecretArenas};

// Linux/macOS/Windows page sizes are 4, 16, or 64 KiB on the supported targets. Give each
// secret its own 64 KiB-aligned allocation so two small cells cannot share an OS lock page:
// `munlock` is page-granular and otherwise dropping one box could unlock a neighboring key.
#[repr(C, align(65536))]
struct PageAlignedCell<const N: usize>(UnsafeCell<[u8; N]>);

/// A fixed-size secret byte cell whose page is under the memory-lock ladder.
///
/// The padded, 64 KiB-aligned allocation isolates its OS page-lock unit from other cells.
/// Stored behind a raw [`NonNull`] over an [`UnsafeCell`] rather than `Box` so moving an armed
/// token by value or forming `&LockedSecret<N>` never retags the heap allocation as `Unique` or
/// `readonly` while [`SecretArenas`] holds its pointer.
pub struct LockedSecret<const N: usize> {
    arena: Option<(Arc<SecretArenas>, ArenaSlot)>,
    guard: Option<LockedRegion>,
    ptr: NonNull<PageAlignedCell<N>>,
}

impl<const N: usize> LockedSecret<N> {
    /// Allocate, populate, and lock. Downgrades are reported in [`Self::report`], never fatal,
    /// and construction itself cannot fail.
    pub fn new(mut bytes: [u8; N]) -> Self {
        let input = Zeroizing::new(bytes);
        bytes.zeroize();
        let raw = Box::into_raw(Box::new(PageAlignedCell(UnsafeCell::new([0u8; N]))));
        let ptr = NonNull::new(raw).expect("Box::into_raw is non-null");
        // SAFETY: `ptr` owns a live `#[repr(C, align(65536))]` heap allocation whose field `0`
        // starts at offset `0`.
        let byte_ptr = unsafe { (*ptr.as_ptr()).0.get().cast::<u8>() };
        // Lock the isolated page before copying in the key; a refused rung remains visible in
        // the report, but the normal path never leaves a populated heap cell unlocked.
        // SAFETY: `byte_ptr` points to `N` writable bytes in a never-resized heap allocation that
        // outlives `guard`; `Drop` zeroizes `[byte_ptr, byte_ptr + N)` before dropping `guard` and
        // before freeing `ptr`.
        let guard = unsafe { LockedRegion::lock(byte_ptr, N) }
            .expect("the lock ladder degrades instead of failing");
        if N > 0 {
            // SAFETY: `input` and `byte_ptr` are valid, non-overlapping `N`-byte regions.
            unsafe { core::ptr::copy_nonoverlapping(input.as_ptr(), byte_ptr, N) };
        }
        // Erase the guarded input copy before returning the new token.
        drop(input);
        #[cfg(test)]
        crate::allocation_audit::watch(byte_ptr.cast_const(), N);
        Self {
            arena: None,
            guard: Some(guard),
            ptr,
        }
    }

    fn byte_ptr(&self) -> *mut u8 {
        // SAFETY: `self.ptr` is live until `Drop` reconstructs and drops the `Box`.
        unsafe { (*self.ptr.as_ptr()).0.get().cast::<u8>() }
    }

    /// Arms this cell in `arenas` so an emergency reaper tripwire volatile-zeroes the live key
    /// page in place without exposing key bytes outside `holonomy-seal`.
    pub(crate) fn arm_reaper(&mut self, arenas: &Arc<SecretArenas>) -> bool {
        if self.arena.is_some() || N == 0 {
            return false;
        }
        let ptr = self.byte_ptr();
        // SAFETY: `ptr` has raw-pointer provenance over `N` writable bytes inside `UnsafeCell`,
        // unaffected by moving `Self`, and `Drop` unregisters `slot` before unlocking and freeing.
        if let Some(slot) = unsafe { arenas.register(ptr, N) } {
            self.arena = Some((Arc::clone(arenas), slot));
            true
        } else {
            false
        }
    }

    /// Volatile-reads every byte of the cell to check whether an emergency wipe zeroed it.
    pub(crate) fn is_zeroized(&self) -> bool {
        let ptr = self.byte_ptr().cast_const();
        let mut acc = 0u8;
        for offset in 0..N {
            // SAFETY: `self.ptr` owns `N` initialized bytes.
            acc |= unsafe { core::ptr::read_volatile(ptr.add(offset)) };
        }
        core::hint::black_box(acc) == 0
    }

    /// The obtained protection ladder state for this cell.
    pub fn report(&self) -> LockReport {
        self.guard
            .as_ref()
            .expect("guard lives until Drop")
            .report()
    }

    /// Crate-internal byte view. The only read of secret cell bytes anywhere in the workspace.
    pub(crate) fn expose(&self) -> &[u8; N] {
        // SAFETY: `self.ptr` owns `N` initialized bytes and callers hold no concurrent reaper trip
        // while synchronously reading key bytes inside `holonomy-seal`.
        unsafe { &*(*self.ptr.as_ptr()).0.get() }
    }
}

impl<const N: usize> Drop for LockedSecret<N> {
    fn drop(&mut self) {
        if let Some((arenas, slot)) = self.arena.take() {
            arenas.unregister(slot);
        }
        // Order is the invariant: contents destroyed *while still locked*, then `munlock` via
        // dropping `guard`, and only then reconstruct and free the heap allocation so
        // `AuditAllocator::dealloc` observes all-zero bytes.
        let ptr = self.byte_ptr();
        for offset in 0..N {
            // SAFETY: `ptr` owns `N` writable bytes and `unregister` completed above.
            unsafe { ptr.add(offset).write_volatile(0) };
        }
        compiler_fence(Ordering::SeqCst);
        drop(self.guard.take());
        // SAFETY: `self.ptr` came from `Box::into_raw` in `Self::new` and is dropped once here.
        unsafe { drop(Box::from_raw(self.ptr.as_ptr())) };
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
        let first_address = first.byte_ptr() as usize;
        let second_address = second.byte_ptr() as usize;
        assert_eq!(first_address % 65_536, 0);
        assert_eq!(second_address % 65_536, 0);
        assert_ne!(first_address / 65_536, second_address / 65_536);
    }

    #[test]
    fn moving_armed_secret_preserves_reaper_wipe() {
        let arenas = SecretArenas::new();
        let mut cell = LockedSecret::new([0x5Au8; 32]);
        assert!(cell.arm_reaper(&arenas));
        let mut moved = vec![core::hint::black_box(cell)];
        let kept = moved.pop().expect("one armed secret");
        assert!(!kept.is_zeroized());
        let stats = arenas.wipe();
        assert_eq!(stats.arenas_wiped, 1);
        assert_eq!(stats.bytes_wiped, 32);
        assert!(kept.is_zeroized());
    }

    #[test]
    fn debug_never_prints_bytes() {
        let cell = LockedSecret::new([9u8; 16]);
        let text = format!("{cell:?}");
        assert!(text.contains("<redacted>"));
        assert!(!text.contains("[9"));
    }
}
