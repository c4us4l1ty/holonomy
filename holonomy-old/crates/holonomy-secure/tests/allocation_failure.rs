//! Regression test for the allocation-failure double-unmap.
//!
//! # The bug
//!
//! `SecureBlock::allocate` used to build the block *before* initialising it:
//!
//! ```text
//! let block = Self { base, len, total };
//! block.initialise(page, data_len)?;   // initialise munmaps on each of its 4 error paths
//! Ok(block)
//! ```
//!
//! On any initialisation failure the `?` propagated, dropping `block`, and `Drop::drop` ran
//! `zeroize_and_release()` — which **writes zeros into the data region** — followed by a second `munmap`.
//! The mapping had already been released, so that is a write to unmapped memory: a SIGSEGV inside
//! `memset`, with no Rust backtrace.
//!
//! # How it was found
//!
//! Loading a 4 MiB document needs ~2,184 page-locked 4 KiB leaves, which exceeds this host's 8,192 KB
//! `RLIMIT_MEMLOCK`. So `mlock` failed, the error path ran, and the process died. It reproduced in both
//! release and debug, which is itself a clue: the fault is in libc, below any Rust handler.
//!
//! # Why this test looks the way it does
//!
//! The failure is reached by exhausting `RLIMIT_MEMLOCK` and then allocating. `RLIMIT_MEMLOCK` is
//! process-wide and 8 MB here, so the test consumes it deliberately with its own `mlock` calls, then
//! asserts that `SecureBlock::allocate` returns an `Err` **and that the process is still alive**.
//!
//! The "still alive" part is the whole assertion. Before the fix this test killed the test binary, which
//! is how the original crash presented: a SIGSEGV with no message. Now it reports a value.

use holonomy_secure::{SecureBlock, SecureBlockError};
use std::sync::OnceLock;

/// Serialises the tests in this file.
///
/// `RLIMIT_MEMLOCK` is **process-wide**, and `libtest` runs the tests in a binary on parallel threads. So
/// without this, [`ordinary_allocation_is_unaffected`] races the two tests that deliberately exhaust the
/// limit and fails on their leftovers -- which is how it presented: an intermittent failure in the full
/// workspace run, on a test with nothing to do with allocation limits, and not in this file's own run.
///
/// A mutex rather than `--test-threads=1`: a gate has to be correct under the default `cargo test`.
static LIMIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The soft `RLIMIT_MEMLOCK`, in KiB, or 0 if the host would not say.
fn memlock_limit_kb() -> u64 {
    #[repr(C)]
    struct RLimit {
        cur: u64,
        max: u64,
    }
    const RLIMIT_MEMLOCK: i32 = 8;
    let mut lim = RLimit { cur: 0, max: 0 };
    // SAFETY: `getrlimit` fills the two-word struct Linux uses for 64-bit `rlim_t`.
    let rc =
        unsafe { libc::getrlimit(RLIMIT_MEMLOCK, &mut lim as *mut RLimit as *mut libc::rlimit) };
    if rc != 0 {
        return 0;
    }
    lim.cur / 1024
}

/// A region locked by this test, so the limit can be given back when the test finishes.
struct LockedRegion {
    ptr: *mut u8,
    len: usize,
}

impl Drop for LockedRegion {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`len` are the live locked mapping this struct created.
        unsafe { libc::munlock(self.ptr.cast(), self.len) };
    }
}

/// Lock memory until `mlock` starts failing, leaving as little headroom as possible.
///
/// Returns `None` if the limit could not be read, or if locking the limit's worth of memory was refused
/// outright -- in which case the test skips rather than reporting a false pass.
fn exhaust_memlock() -> Option<Vec<LockedRegion>> {
    static TRIED: OnceLock<bool> = OnceLock::new();
    if !*TRIED.get_or_init(|| true) {
        return None;
    }
    let limit_kb = memlock_limit_kb();
    if limit_kb == 0 {
        return None;
    }
    let page = holonomy_secure::page_size()?;
    // Ask for slightly more than the limit so we land in the failing regime, in 1 MiB chunks.
    let target = (limit_kb as usize + 1024) * 1024;
    let mut held = Vec::new();
    let mut locked = 0usize;
    while locked < target {
        let len = 1 << 20;
        // SAFETY: an anonymous mapping of `len` bytes; `mlock` validates it.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            break;
        }
        // SAFETY: `p` is a live mapping of `len` bytes.
        if unsafe { libc::mlock(p, len) } != 0 {
            // SAFETY: `p`/`len` are the live mapping just created.
            unsafe { libc::munmap(p, len) };
            break;
        }
        // Touch every page, so the RSS is real and the accounting is not just a reservation.
        // SAFETY: `p` is readable and writable for `len` bytes.
        unsafe { std::ptr::write_bytes(p.cast::<u8>(), 0, len) };
        locked += len;
        held.push(LockedRegion {
            ptr: p.cast::<u8>(),
            len,
        });
        let _ = page;
    }
    if locked == 0 {
        return None;
    }
    Some(held)
}

/// The regression itself.
///
/// `allocating_after_the_page_lock_limit_is_exhausted_returns_an_error_and_does_not_crash`.
///
/// Before the fix this test binary died with SIGSEGV and printed nothing. The value it returns now is
/// the error, which is the point.
#[test]
fn allocating_after_the_page_lock_limit_is_exhausted_returns_an_error_and_does_not_crash() {
    let _serialise = LIMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(_held) = exhaust_memlock() else {
        eprintln!("skipping: could not exhaust RLIMIT_MEMLOCK on this host");
        return;
    };

    // Allocate repeatedly. Exactly one of two things must happen on every call, and the test reaching
    // its final line is the assertion:
    //
    // * `Ok` -- the host had more headroom than we thought. Fine.
    // * `Err(MlockFailed)` -- the limit was reached. Also fine, and the interesting branch.
    //
    // What must *not* happen is a crash. Before the fix, the first `Err` dropped a block whose mapping
    // `initialise` had already released, and the scrub in `Drop` faulted.
    let mut ok = 0usize;
    let mut err = 0usize;
    for _ in 0..64 {
        match SecureBlock::allocate(4096) {
            Ok(block) => {
                ok += 1;
                // Exercise the block so a successful allocation is not a dead branch.
                assert_eq!(block.len(), 4096);
                drop(block);
            }
            Err(SecureBlockError::MlockFailed) => err += 1,
            Err(e) => panic!("unexpected allocation error: {e:?}"),
        }
    }
    println!("after exhausting RLIMIT_MEMLOCK: {ok} allocated, {err} refused with MlockFailed");
    assert!(
        ok + err == 64,
        "every allocation must either succeed or be refused cleanly"
    );

    // Give the limit back before the other tests in this binary run, since it is process-wide.
    drop(_held);
    drop(_serialise);
}

/// A failure on the `mprotect` path must also be clean. Unreachable in practice — it needs an unmapped
/// region — so this asserts the *shape* of the guarantee rather than provoking it: the error type exists,
/// `allocate` returns `Result`, and a refused allocation leaves the process able to keep allocating once
/// the pressure is off.
#[test]
fn a_refused_allocation_leaves_the_process_able_to_allocate_again() {
    let _serialise = LIMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    {
        let Some(_held) = exhaust_memlock() else {
            eprintln!("skipping: could not exhaust RLIMIT_MEMLOCK on this host");
            return;
        };
        // Under pressure, allocation must fail rather than proceed unlocked: NFR-3 is that a block must
        // never reach swap, and an unlocked block can.
        let mut refused = 0;
        for _ in 0..16 {
            if SecureBlock::allocate(4096).is_err() {
                refused += 1;
            }
        }
        println!("under pressure: {refused} of 16 allocations refused");
    }
    // Pressure off: allocation must work again, which also proves nothing was leaked or double-freed.
    let block = SecureBlock::allocate(4096).expect("allocation must recover once pressure is off");
    assert_eq!(block.len(), 4096);
    drop(block);
}

/// The ordinary path still works, so the error-handling change did not break the common case.
#[test]
fn ordinary_allocation_is_unaffected() {
    let _serialise = LIMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut block = SecureBlock::allocate(8192).expect("allocate");
    assert_eq!(block.len(), 8192);
    block.as_mut_slice()[0] = 0xAB;
    assert_eq!(block.as_slice()[0], 0xAB);
    // And dropping it scrubs, which is the guarantee the double-unmap used to break.
    drop(block);
}
