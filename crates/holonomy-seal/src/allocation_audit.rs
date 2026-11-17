//! Test-only allocator that counts allocations and verifies registered secret buffers are zero
//! immediately before they are returned to the system allocator. The production crate does not
//! install this allocator or expose the registration hook.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};
use std::alloc::System;

const WATCH_SLOTS: usize = 4_096;
const RESERVED: usize = usize::MAX;

struct Watched {
    address: AtomicUsize,
    length: AtomicUsize,
}

static WATCHED: [Watched; WATCH_SLOTS] = [
    const {
        Watched {
            address: AtomicUsize::new(0),
            length: AtomicUsize::new(0),
        }
    };
    WATCH_SLOTS
];
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static REGISTERED: AtomicUsize = AtomicUsize::new(0);
static ZEROED_BEFORE_FREE: AtomicUsize = AtomicUsize::new(0);
static NONZERO_BEFORE_FREE: AtomicUsize = AtomicUsize::new(0);
static REGISTRY_OVERFLOW: AtomicUsize = AtomicUsize::new(0);
static SECRET_REALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

/// Test global allocator. Counts all ordinary allocations and checks only explicitly registered
/// secret buffers; those registrations are placed at the owning type's allocation site.
pub(crate) struct AuditAllocator;

// SAFETY: all allocation operations are forwarded to `System`; the extra state consists only
// of atomic counters and pointer/length registrations for live, caller-owned allocations.
unsafe impl GlobalAlloc for AuditAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim to the system allocator.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim to the system allocator.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        verify_registered(pointer);
        // SAFETY: pointer and layout satisfy the GlobalAlloc contract and are forwarded to
        // the same allocator that produced them.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if take_registration(pointer).is_some() {
            // Sensitive buffers in this crate must not move through an un-wiped realloc.
            SECRET_REALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: forwarded verbatim to the system allocator.
        let resized = unsafe { System.realloc(pointer, layout, new_size) };
        if !resized.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        resized
    }
}

/// Register a fully initialized secret-bearing byte range for inspection at deallocation.
/// Empty ranges have no allocation to audit. On overflow, the gate records a failure rather
/// than silently omitting evidence.
pub(crate) fn watch(pointer: *const u8, length: usize) {
    if length == 0 {
        return;
    }
    let address = pointer as usize;
    assert_ne!(address, 0, "cannot watch a null allocation");
    for slot in &WATCHED {
        if slot
            .address
            .compare_exchange(0, RESERVED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            slot.length.store(length, Ordering::Relaxed);
            slot.address.store(address, Ordering::Release);
            REGISTERED.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    REGISTRY_OVERFLOW.fetch_add(1, Ordering::Relaxed);
}

fn take_registration(pointer: *mut u8) -> Option<usize> {
    let address = pointer as usize;
    for slot in &WATCHED {
        if slot
            .address
            .compare_exchange(address, RESERVED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let length = slot.length.swap(0, Ordering::Relaxed);
            slot.address.store(0, Ordering::Release);
            return Some(length);
        }
    }
    None
}

fn verify_registered(pointer: *mut u8) {
    let Some(length) = take_registration(pointer) else {
        return;
    };
    // All registered ranges are initialized byte arrays, String/Vec contents, or Argon2 blocks;
    // their owning wipe guard runs before this allocator hook. Reading these bytes is valid.
    let wiped = (0..length).all(|offset| {
        // SAFETY: the registration was made for this live allocation and length; dealloc is
        // called before System frees it.
        unsafe { pointer.add(offset).read() == 0 }
    });
    if wiped {
        ZEROED_BEFORE_FREE.fetch_add(1, Ordering::Relaxed);
    } else {
        NONZERO_BEFORE_FREE.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Snapshot {
    pub allocations: usize,
    pub deallocations: usize,
    pub registered: usize,
    pub zeroed_before_free: usize,
    pub nonzero_before_free: usize,
    pub registry_overflow: usize,
    pub secret_reallocations: usize,
}

pub(crate) fn snapshot() -> Snapshot {
    Snapshot {
        allocations: ALLOCATIONS.load(Ordering::Relaxed),
        deallocations: DEALLOCATIONS.load(Ordering::Relaxed),
        registered: REGISTERED.load(Ordering::Relaxed),
        zeroed_before_free: ZEROED_BEFORE_FREE.load(Ordering::Relaxed),
        nonzero_before_free: NONZERO_BEFORE_FREE.load(Ordering::Relaxed),
        registry_overflow: REGISTRY_OVERFLOW.load(Ordering::Relaxed),
        secret_reallocations: SECRET_REALLOCATIONS.load(Ordering::Relaxed),
    }
}
