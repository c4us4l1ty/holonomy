//! **A passphrase must not outlive an unlock that failed.** FR-4.5.
//!
//! # What this file is about
//!
//! `derive_root` holds the user's passphrase as NFKD-expanded text on the heap for the whole of the
//! derivation — the longest part of an unlock — and it has four ways to fail. It scrubbed that text
//! by hand, once, on the way to `Ok`. Every one of those four returns therefore freed a heap block
//! that still held the passphrase, and the comment above it claimed the opposite:
//!
//! ```text
//! so the scrub is written at each exit instead: there are exactly four, and
//! the compiler says so if a fifth is added.
//! ```
//!
//! There was one scrub, on the success path, and nothing checked the arithmetic. `argon2id_k_int`
//! had the same shape one level down: a plain `String` scrubbed after the Argon2 call, with the
//! empty-passphrase return above it going straight past the scrub.
//!
//! # Why this is its own binary
//!
//! **The only way to see a freed heap block is to be the allocator that frees it**, and a
//! `#[global_allocator]` is process-global: it would apply to every other test in the file it lived
//! in, including the ones that must not allocate a `String` to check that they do not. So this is a
//! binary with exactly one test in it, and the allocator it declares only ever looks for one
//! passphrase — a marker nothing else in the workspace contains.
//!
//! # What it does and does not prove
//!
//! **It proves the bytes are gone from the block when it is handed back.** It does not prove
//! anything about earlier copies of the same buffer: `normalize_passphrase` grows its `String` by
//! `realloc`, and a `realloc` that moves frees the old block — which is why the allocator here
//! inspects `realloc`'s old block on the same terms as `dealloc`'s. Swap is a different matter
//! entirely and is not this crate's to answer: `mlockall(MCL_FUTURE)` runs at boot before any KDF
//! (`holonomy-jail/src/rlimits.rs`), and `RLIMIT_CORE` is 0.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use holonomy_crypto::envelope::{derive_root, EnvelopeError, ExposeSecret, SALT_LEN};

/// A passphrase nothing else in this binary contains, so a hit on it is that passphrase's and not
/// some other string's. Long enough that no allocation of unrelated data is likely to hold it.
const MARKER: &[u8] = b"holonomy-scrub-probe-8f21c4";

/// Blocks this binary has freed that still held [`MARKER`].
static LEAKS: AtomicUsize = AtomicUsize::new(0);

/// `System`, with the freed block read before it goes back.
struct Watching;

// SAFETY: every method forwards to `System` unchanged, and the only added work is a read of a block
// this allocator is being asked to free — so it is still ours to read, and no pointer outlives the
// call.
unsafe impl GlobalAlloc for Watching {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        watch(ptr, layout.size());
        // SAFETY: forwarded verbatim.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // **The old block is freed by the realloc itself**, so it is inspected here or not at all —
        // and this is exactly the block a grown `String` leaves behind.
        watch(ptr, layout.size());
        // SAFETY: forwarded verbatim.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Count `size` bytes at `ptr` if they still hold the passphrase.
fn watch(ptr: *mut u8, size: usize) {
    if size < MARKER.len() {
        return;
    }
    // SAFETY: `ptr` is a live block of `size` bytes owned by the allocator that is freeing it, and
    // `size >= MARKER.len()`, so the slice is in bounds.
    let block = unsafe { std::slice::from_raw_parts(ptr, size) };
    if block.windows(MARKER.len()).any(|window| window == MARKER) {
        LEAKS.fetch_add(1, Ordering::SeqCst);
    }
}

#[global_allocator]
static ALLOCATOR: Watching = Watching;

/// **A failed unlock must not leave the passphrase in freed heap.**
///
/// The salt is three bytes, which `argon2id_k_int` refuses before it does any work — the second of
/// `derive_root`'s four early returns, and the one where the passphrase is *only* on the heap (by the
/// time the VDF rejects its input, the heap copy had been scrubbed and what is left is stack).
///
/// **The success path is measured on the same counter, immediately after.** That is the path the
/// scrub used to be written on, so leaving it out would leave the regression able to pass by making
/// the happy path worse. Both halves are in one test rather than two because the counter is
/// process-global state: `libtest` runs tests in parallel threads, and two tests sharing it would race
/// on the very number they are asserting.
#[test]
fn the_passphrase_is_not_left_in_freed_heap_on_any_path_out() {
    let passphrase = std::str::from_utf8(MARKER).expect("the marker is ASCII");

    // **The failure path.** `Derived` is deliberately not `Debug` — it holds the root — so the error
    // is matched by hand rather than unwrapped with `expect_err`, which would want to print it.
    LEAKS.store(0, Ordering::SeqCst);
    let Err(err) = derive_root(passphrase, &[0u8; 3], 1) else {
        panic!("three bytes is not a salt");
    };
    assert!(
        matches!(err, EnvelopeError::BadSaltLength),
        "the error is the salt, not something this test has mistaken: {err:?}"
    );
    assert_eq!(
        LEAKS.load(Ordering::SeqCst),
        0,
        "a block this binary freed still held the passphrase, so `derive_root` gave it back to the \
         allocator unscrubbed on the failure path"
    );

    // **And the success path**, which is the one the scrub used to be written on.
    LEAKS.store(0, Ordering::SeqCst);
    let derived = derive_root(passphrase, &[7u8; SALT_LEN], 1)
        .expect("a derivation with a real salt and one squaring");
    assert_eq!(
        LEAKS.load(Ordering::SeqCst),
        0,
        "a block this binary freed still held the passphrase after a *successful* unlock"
    );
    // The root must be real: a scrub that ran before the copy would hand back zeros, which compiles,
    // passes every length assertion, and opens nobody's container.
    assert!(
        !derived.root.expose_secret().k_enc.iter().all(|&b| b == 0),
        "the derived key is all zeros, so the scrub ran before the root was built"
    );
}
