//! Phase 9C's gate on `LockPolicy`: the two policies really differ in `mlock`, and only in `mlock`.
//!
//! **Its own binary, on purpose.** The test exhausts `RLIMIT_MEMLOCK`, and that limit is
//! *process-wide*: `libtest` runs a binary's tests on parallel threads, so doing this inside
//! `src/tests.rs` would take the page-lock budget away from every other test in that binary while
//! they ran. It failed exactly that way -- an unrelated test saw `+1` where it expected `+2` -- so
//! this file holds the only test that spends the budget, and it gives the budget back before it
//! exits. `tests/allocation_failure.rs` is the same shape and the same reason.
//!
//! | requirement | test |
//! | --- | --- |
//! | `PageLocked` spends the budget and is refused when it is gone | [`a_page_locked_block_is_refused_once_the_budget_is_gone`] |
//! | `Unlocked` skips `mlock`, which is the whole reason it exists | [`an_unlocked_block_still_allocates_once_the_budget_is_gone`] |
//! | the two differ *only* in that | [`the_lock_policies_differ_only_in_whether_mlock_is_called`] |
//! | `Unlocked` still guards and still maps a real page | [`an_unlocked_block_is_still_a_guarded_mapping`] |

use holonomy_secure::{LockPolicy, SecureBlock, SecureBlockError};
use std::sync::{Mutex, MutexGuard};

/// Serialises this file's tests. A mutex, not `--test-threads=1`, because a gate has to be correct
/// under a plain `cargo test`.
static LIMIT_LOCK: Mutex<()> = Mutex::new(());

/// Take the lock, and print if it was already held -- which would mean two tests spending the
/// budget at once.
fn serialised() -> MutexGuard<'static, ()> {
    LIMIT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A region this file locked, so the limit can be handed back when the test finishes.
struct LockedRegion {
    ptr: *mut u8,
    len: usize,
}

impl Drop for LockedRegion {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`len` is the live locked mapping this struct created.
        unsafe { libc::munlock(self.ptr.cast(), self.len) };
    }
}

/// mlock regions until `mlock` starts failing, so the budget is genuinely gone.
///
/// Returns the held regions, or `None` if the host would not let this test take the budget at all
/// -- in which case callers skip rather than assert something the host never permitted.
fn exhaust_memlock() -> Option<Vec<LockedRegion>> {
    const CHUNK: usize = 1 << 20;
    let mut held = Vec::new();
    loop {
        // SAFETY: a fresh private anonymous mapping of `CHUNK`, or `MAP_FAILED`.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                CHUNK,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            break;
        }
        // SAFETY: `p` is a live mapping of `CHUNK` bytes.
        if unsafe { libc::mlock(p, CHUNK) } != 0 {
            // SAFETY: `p`/`CHUNK` is the live mapping just created, and the mlock failed.
            unsafe { libc::munmap(p, CHUNK) };
            break;
        }
        // Touch every page so the lock is really accounted rather than merely reserved.
        // SAFETY: `p` is readable and writable for `CHUNK` bytes.
        unsafe { std::ptr::write_bytes(p.cast::<u8>(), 0, CHUNK) };
        held.push(LockedRegion {
            ptr: p.cast::<u8>(),
            len: CHUNK,
        });
    }
    if held.is_empty() {
        None
    } else {
        Some(held)
    }
}

/// A budget this file has spent, released in the right order.
///
/// The two fields are held, never read, and that is the point: they are dropped, and *drop order is
/// the contract*. Fields drop in declaration order, so `regions` (which `munlock`s on drop) goes
/// first and `guard` (the mutex) is released second -- so the `mlock`ed bytes are already unlocked
/// by the time the next test can take the mutex. Reversing the two would leave the budget spent
/// while the next test believed it had it back, which is exactly the race this file exists to avoid.
#[allow(
    dead_code,
    reason = "the fields are held for their drop order, not read"
)]
struct Budget {
    regions: Vec<LockedRegion>,
    guard: MutexGuard<'static, ()>,
}

impl Budget {
    fn spend() -> Option<Self> {
        let guard = serialised();
        let regions = exhaust_memlock()?;
        Some(Self { regions, guard })
    }
}

/// The load-bearing test: the policies differ in `mlock`, and in nothing else.
///
/// With `RLIMIT_MEMLOCK` exhausted, a `PageLocked` allocation must be **refused** with
/// `MlockFailed` and an `Unlocked` one must **succeed**. That pair is the whole claim in both
/// directions: the locked policy really spends the budget, and the unlocked one really does not.
///
/// PROJECT.md §2.9.3 wanted ~9.1 decoded rasters (8.0 MiB of page-column-width RGBA) resident at
/// once, and §2.9.4's text budget already needs 6.82 MiB of `mlock` -- 14.8 MiB against this host's
/// 8 MiB ceiling, with soft and hard limits equal so it cannot be raised. Without `Unlocked`, the
/// Iceberg cache would hold exactly **one** raster alongside a full document.
#[test]
fn the_lock_policies_differ_only_in_whether_mlock_is_called() {
    let Some(budget) = Budget::spend() else {
        eprintln!("skipping: could not exhaust RLIMIT_MEMLOCK on this host");
        return;
    };

    let locked = SecureBlock::allocate(4096);
    assert!(
        matches!(locked, Err(SecureBlockError::MlockFailed)),
        "with RLIMIT_MEMLOCK exhausted, a PageLocked block must be refused with MlockFailed, got \
         {:?}. If this is Ok then mlock is not actually being called, LockPolicy is a label rather \
         than a behaviour, and the whole justification for Unlocked is missing.",
        locked.as_ref().err()
    );

    let unlocked = SecureBlock::allocate_with(4096, LockPolicy::Unlocked).expect(
        "with RLIMIT_MEMLOCK exhausted an Unlocked block must still allocate: document text has \
         already spent the budget on this host, and an image cache that cannot allocate is not a \
         cache policy",
    );
    assert!(!unlocked.is_locked());

    drop(unlocked);
    drop(budget);
}

#[test]
fn a_page_locked_block_is_refused_once_the_budget_is_gone() {
    let Some(budget) = Budget::spend() else {
        eprintln!("skipping: could not exhaust RLIMIT_MEMLOCK on this host");
        return;
    };
    // Repeatedly, so this is a statement about the limit rather than about one unlucky call.
    let mut refused = 0;
    for _ in 0..16 {
        match SecureBlock::allocate(4096) {
            Err(SecureBlockError::MlockFailed) => refused += 1,
            Ok(block) => drop(block),
            Err(e) => panic!("unexpected allocation error: {e:?}"),
        }
    }
    assert_eq!(
        refused, 16,
        "every PageLocked allocation must be refused while the budget is gone; {refused} of 16 were"
    );
    drop(budget);
}

#[test]
fn an_unlocked_block_still_allocates_once_the_budget_is_gone() {
    let Some(budget) = Budget::spend() else {
        eprintln!("skipping: could not exhaust RLIMIT_MEMLOCK on this host");
        return;
    };
    // Repeatedly, because the image cache allocates one block per raster and must be able to hold
    // the ~9 of them §2.9.3 budgets for.
    let mut live = Vec::new();
    for _ in 0..16 {
        let mut block = SecureBlock::allocate_with(4096, LockPolicy::Unlocked).expect(
            "an Unlocked block must allocate with no page-lock budget at all, which is what makes \
             the Iceberg cache's byte budget independent of document length",
        );
        // Usable, not merely mappable.
        block.as_mut_slice().fill(0x5A);
        assert!(block.as_slice().iter().all(|&b| b == 0x5A));
        live.push(block);
    }
    assert_eq!(live.len(), 16, "all 16 must still be live simultaneously");
    drop(live);
    drop(budget);
}

#[test]
fn an_unlocked_block_is_still_a_guarded_mapping() {
    // The traded property is the swap guarantee and *only* the swap guarantee. Guards, `MADV_DONTDUMP`
    // and registry membership must survive, or the policy would cost far more than `LockPolicy`
    // claims and the tripwire could not scrub a raster on a fault.
    let block = SecureBlock::allocate_with(4096, LockPolicy::Unlocked).expect("allocate");
    let page = holonomy_secure::page_size().expect("page size");
    assert_eq!(
        block.mapped_len(),
        3 * page,
        "an Unlocked block must still map one data page between two PROT_NONE guards"
    );
    let data = block.as_ptr() as usize;
    assert_eq!(
        holonomy_jail::registry::classify(data - page),
        holonomy_jail::FaultSite::Guard,
        "the lower guard must be registered"
    );
    assert_eq!(
        holonomy_jail::registry::classify(data + page),
        holonomy_jail::FaultSite::Guard,
        "the upper guard must be registered, so an overrun past the last raster pixel faults"
    );
}
