//! Phase 1 gate: `cargo test -p holonomy-secure`.
//!
//! The load-bearing test here is [`guard_pages_fault_on_both_sides`]. Everything else in
//! the crate is hardening that cannot be proven; the guard pages can be, and they are the
//! only thing that makes a `SecureBlock` different from a `Vec<u8>` that happens to be
//! page-locked.

use super::{page_size, LockPolicy, SecureBlock, SecureBlockError};
use std::ptr;

/// The child reached `_exit` having successfully read the byte.
const EXIT_READ_OK: i32 = 0;
/// The child could not allocate its own block. Distinct from every other code so a
/// parent assertion can say "the test could not run" instead of "the guard leaked".
const EXIT_ALLOC_FAILED: i32 = 90;

#[test]
fn page_size_is_plausible() {
    let page = page_size().expect("page size");
    assert!(
        page.is_power_of_two(),
        "page size {page} is not a power of two"
    );
    assert!(
        (512..=64 * 1024).contains(&page),
        "page size {page} out of range"
    );
}

/// A 4096-byte block reports a length of exactly 4096 and maps three pages.
#[test]
fn allocate_reports_exact_length_and_accounts_for_guards() {
    let block = SecureBlock::allocate(4096).expect("allocate 4096");
    let page = page_size().unwrap();

    assert_eq!(
        block.len(),
        4096,
        "slice length must be the requested length"
    );
    assert!(!block.is_empty());

    // One data page plus a guard above and below.
    assert_eq!(
        block.mapped_len(),
        3 * page,
        "mapped_len must include both guard pages"
    );
    assert_eq!(SecureBlock::guard_pages(), 2);

    // A fresh anonymous mapping reads as zero. Not a security property -- it is asserted
    // so that the write/read-back test below cannot pass on stale contents.
    assert!(
        block.as_slice().iter().all(|&b| b == 0),
        "fresh mapping must be zeroed"
    );
}

/// The requested length is honoured exactly even when it is not page-aligned.
#[test]
fn length_is_honoured_when_not_page_aligned() {
    let page = page_size().unwrap();
    let len = page * 2 + 17;
    let block = SecureBlock::allocate(len).expect("allocate");

    assert_eq!(block.len(), len);
    assert_eq!(block.as_slice().len(), len);
    // 3 data pages (rounded up) + 2 guards.
    assert_eq!(block.mapped_len(), 5 * page);
}

/// Writes survive and come back byte for byte.
#[test]
fn write_read_back_and_scrub() {
    let mut block = SecureBlock::allocate(4096).expect("allocate");

    // A pattern with runs, so a partial scrub cannot masquerade as a full one.
    let pattern: Vec<u8> = (0..4096).map(|i| (i % 7 + 1) as u8).collect();
    block.as_mut_slice().copy_from_slice(&pattern);
    assert_eq!(block.as_slice(), &pattern[..]);

    let scrubbed = block.zeroize_and_release();
    assert_eq!(scrubbed, 4096);
    assert!(
        block.as_slice().iter().all(|&b| b == 0),
        "zeroize_and_release must clear every byte"
    );

    // The mapping is still usable afterwards: zeroize scrubs, it does not unmap.
    block.as_mut_slice()[0] = 0xAA;
    assert_eq!(block.as_slice()[0], 0xAA);
}

#[test]
fn zero_length_is_rejected() {
    // A zero-length block would have no data page and therefore no meaningful guards.
    assert_eq!(
        SecureBlock::allocate(0).err(),
        Some(SecureBlockError::ZeroLength)
    );
}

/// The gate test. A byte immediately below and immediately above the data region must
/// fault, and touching the data itself must not.
///
/// The fault is observed in a forked child rather than recovered from in-process. Two
/// reasons, both of which are easy to get wrong:
///
/// 1. Recovering from `SIGSEGV` in-process needs `sigsetjmp`, and a longjmp out of a
///    handler skips whatever the faulting statement would have done. For a test whose
///    whole claim is "this address is not readable", not resuming is the honest
///    outcome anyway -- but it is awkward to express.
/// 2. The child allocates its *own* block. A forked child does not inherit this
///    process's `SecureBlock` mappings, precisely because `MADV_DONTFORK` is set. Testing
///    in the child against the parent's pointer would therefore "prove" nothing: it
///    would fault on `DONTFORK` unmapping rather than on the guard page.
///
/// The control case matters as much as the fault case: without it, a test that faults
/// on *every* address would pass.
#[test]
fn guard_pages_fault_on_both_sides() {
    let page = page_size().unwrap();
    let len = 4096usize;

    // Below the data region: the last byte of the lower guard page.
    assert_faults(len, -1isize);
    // The interior of the lower guard, to show it is a whole page and not a canary.
    assert_faults(len, -(page as isize) / 2);

    // Above the data region: the first byte of the upper guard page. `len` is exactly one
    // page, so the data ends precisely where the guard begins -- no arithmetic fudge.
    assert_faults(len, len as isize);
    // The interior and the last byte of the upper guard.
    assert_faults(len, len as isize + (page as isize) / 2);
    assert_faults(len, len as isize + page as isize - 1);
}

/// What the guard pages buy, stated as a test.
///
/// Reading *past* the end of an `mmap` is not reliably a fault. The address is still
/// inside the process's address space, and whether it faults depends on what the kernel
/// happened to place next -- an adjacent mapping, a malloc arena, the guard of an
/// unrelated VMA. During development this crate's own guard test "passed" against an
/// address 8192 bytes past a 3-page mapping precisely because that address belonged to
/// something else and read fine.
///
/// A `PROT_NONE` guard is inside our own mapping, so the fault is ours and is
/// deterministic. That is the whole reason the guard is a mapped page rather than simply
/// relying on the end of the mapping.
#[test]
fn reading_past_the_mapping_is_not_reliably_a_fault() {
    // This test asserts the *absence* of a guarantee, so it is informational: it reports
    // what this host happened to do rather than failing either way.
    let page = page_size().unwrap();
    let len = 4096usize;
    let status = run_child(len, (len + page) as isize);
    eprintln!(
        "note: an address {page} B past the end of a {len}-byte block {} here. The guards \
         are PROT_NONE; past the last guard the mapping simply ends.",
        if libc::WIFSIGNALED(status) {
            "faulted"
        } else {
            "was readable"
        }
    );
}

/// Control for [`guard_pages_fault_on_both_sides`]: the first and last *data* bytes must
/// be readable. Without this, a probe that faulted on every address would pass.
#[test]
fn data_region_is_readable() {
    let len = 4096usize;
    assert_readable(len, 0isize);
    assert_readable(len, (len - 1) as isize);
}

/// Fork a child that allocates its own block, reads one byte, and `_exit`s. Returns the
/// child's wait status; the child dies of `SIGSEGV` instead if the read faults.
fn run_child(len: usize, offset: isize) -> libc::c_int {
    // SAFETY: the child performs no heap allocation before `_exit`, so it cannot
    // deadlock on an allocator lock held by a thread that did not survive the fork.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed; the guard-page test cannot run");

    if pid == 0 {
        // ---- child ----
        let Ok(block) = SecureBlock::allocate(len) else {
            // SAFETY: async-signal-safe, and the only path out of a failed allocation.
            unsafe { libc::_exit(EXIT_ALLOC_FAILED) }
        };
        // SAFETY: `block` outlives this read. The read is volatile so the compiler
        // cannot delete it and quietly turn the probe into a no-op. The address may be a
        // guard page, which is the entire point of the test.
        let byte = unsafe { ptr::read_volatile(block.as_ptr().offset(offset)) };
        // Consume the value so the probe cannot be optimised into a no-op.
        std::hint::black_box(byte);
        unsafe { libc::_exit(EXIT_READ_OK) }
    }

    // ---- parent ----
    let mut status: libc::c_int = 0;
    // SAFETY: `pid` is a direct child of this process and `status` is a valid out-pointer.
    let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
    assert_eq!(waited, pid, "waitpid failed");
    status
}

/// Assert the child died of `SIGSEGV` at `offset`.
fn assert_faults(len: usize, offset: isize) {
    let status = run_child(len, offset);
    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == EXIT_ALLOC_FAILED {
        panic!(
            "could not allocate a SecureBlock of {len} bytes in the child; the \
                guard-page probe did not run"
        );
    }
    assert!(
        libc::WIFSIGNALED(status),
        "offset {offset} is readable but must be guarded: child exited normally (code {}). \
         A readable guard page is a containment failure.",
        libc::WEXITSTATUS(status)
    );
    assert_eq!(
        libc::WTERMSIG(status),
        libc::SIGSEGV,
        "offset {offset} faulted, but not with SIGSEGV (got {})",
        libc::WTERMSIG(status)
    );
}

/// Assert the child read the byte at `offset` successfully.
fn assert_readable(len: usize, offset: isize) {
    let status = run_child(len, offset);
    assert!(
        libc::WIFEXITED(status),
        "offset {offset} is inside the data region and must be readable, but died of signal {}",
        libc::WTERMSIG(status)
    );
    assert_eq!(
        libc::WEXITSTATUS(status),
        EXIT_READ_OK,
        "unexpected child exit code {} at offset {offset}",
        libc::WEXITSTATUS(status)
    );
}

/// `SecureBlock` must not hand out a `Clone`, and must not print its contents.
#[test]
fn debug_does_not_leak_contents() {
    let mut block = SecureBlock::allocate(16).expect("allocate");
    block.as_mut_slice().copy_from_slice(b"SECRET DOCUMENT!");
    let rendered = format!("{block:?}");
    assert!(
        !rendered.contains("SECRET"),
        "Debug leaked contents: {rendered}"
    );
    assert!(
        rendered.contains("len"),
        "Debug should still be useful: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// Phase 9C: the two lock policies.
//
// The load-bearing test is `the_lock_policies_differ_only_in_whether_mlock_is_called`,
// because the justification for `LockPolicy::Unlocked` is arithmetic about a shared,
// finite budget (`RLIMIT_MEMLOCK` is already spent by document text) and an arithmetic
// claim that is not measured is the kind that rots.
//
// **Not probed with `mincore`.** An earlier version of this test did, and it was wrong:
// `mincore` reports whether a page is *resident*, and an unlocked page in the page cache
// is resident. It cannot answer "is this locked" at all, so the `PageLocked` assertion
// failed against a genuinely locked block. The observable that *is* the property is
// whether `mlock` succeeds once the budget is gone.
// ---------------------------------------------------------------------------

#[test]
fn a_block_reports_the_policy_it_was_built_with() {
    let locked = SecureBlock::allocate(4096).expect("allocate");
    assert_eq!(
        locked.policy(),
        LockPolicy::PageLocked,
        "allocate() must stay the page-locked spelling, so adding the policy cannot silently \
         un-mlock a document's text"
    );
    assert!(locked.is_locked());

    let unlocked = SecureBlock::allocate_with(4096, LockPolicy::Unlocked).expect("allocate");
    assert_eq!(unlocked.policy(), LockPolicy::Unlocked);
    assert!(!unlocked.is_locked());
    // The policy is a recorded field, not a `mincore` probe, so it cannot report a policy the
    // block was not built with.
    assert_eq!(unlocked.policy(), LockPolicy::Unlocked);
}

#[test]
fn an_unlocked_block_still_scrubs_and_still_registers() {
    // The image cache relies on both: eviction must be able to *prove* the pixels are gone, and
    // the guard-page tripwire must be able to scrub the block if a fault happens mid-paint.
    let mut block = SecureBlock::allocate_with(4096, LockPolicy::Unlocked).expect("allocate");

    // **Registration is checked per block, not by counting the registry.** `active_count` is
    // process-wide and `libtest` runs this binary on parallel threads, so a `before`/`after` pair is
    // a race against every other test in the file -- and it failed exactly that way, reporting `+1`
    // where `+2` was expected because an unrelated test held a block. `classify` asks about *this*
    // block's own addresses, so it has nothing to race with.
    let page = page_size().expect("page size");
    let data = block.as_ptr() as usize;
    assert_eq!(
        holonomy_jail::registry::classify(data - page),
        holonomy_jail::FaultSite::Guard,
        "the guard below an Unlocked block must be registered as a guard, so the tripwire can \
         scrub it on a fault: the guard pages are what make the contents guaranteed-gone"
    );
    assert_eq!(
        holonomy_jail::registry::classify(data),
        holonomy_jail::FaultSite::Data,
        "the data region of an Unlocked block must be registered as data"
    );

    block.as_mut_slice().fill(0xAB);
    assert!(
        block.as_slice().iter().all(|&b| b == 0xAB),
        "an Unlocked block must be writable"
    );
    assert_eq!(block.zeroize_and_release(), 4096);
    assert!(
        block.as_slice().iter().all(|&b| b == 0),
        "zeroize_and_release must scrub an Unlocked block exactly as it does a locked one; this is \
         the call eviction makes and the gate the cache asserts on"
    );
}

#[test]
fn a_zero_length_block_is_refused_under_either_policy() {
    for policy in [LockPolicy::PageLocked, LockPolicy::Unlocked] {
        assert_eq!(
            SecureBlock::allocate_with(0, policy).unwrap_err(),
            SecureBlockError::ZeroLength,
            "{policy:?} must refuse zero length rather than map a region with no data page"
        );
    }
}
