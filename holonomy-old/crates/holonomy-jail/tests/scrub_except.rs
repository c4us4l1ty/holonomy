//! `registry::scrub_all_except`, in a process where the registry is private.
//!
//! # Why its own test binary
//!
//! The registry is **process-wide** and `scrub_all` reaches *every* slot in it. So a test that calls
//! it from a shared binary does two things at once: it races every sibling test's
//! `SecureBlock`, and it asserts against a total that includes blocks it never allocated.
//!
//! `tests/guard_page.rs` exercises this the right way already -- in a **subprocess**, whose registry
//! contains exactly the two blocks that subprocess allocated, and whose `expected_scrubbed` pins the
//! total. This file covers the two properties that needs a subprocess to observe but does not fault
//! on: which block's bytes survive, and what the returned total counts.
//!
//! One test per binary is deliberate. Two would share the registry and reintroduce the problem.

use holonomy_jail::registry;
use holonomy_secure::SecureBlock;

const PLAINTEXT: u8 = 0xA5;

/// The block named by `skip_base` keeps its bytes; every other block is scrubbed.
#[test]
fn the_skipped_block_survives_and_the_other_is_scrubbed() {
    let mut first = SecureBlock::allocate(4096).expect("allocate the first block");
    let mut second = SecureBlock::allocate(8192).expect("allocate the second block");
    first.as_mut_slice().fill(PLAINTEXT);
    second.as_mut_slice().fill(PLAINTEXT);

    // The skip is a *payload address*, which is what the tripwire passes: `AltStack::base()` is the
    // address the kernel was given, which is the same pointer `register` recorded.
    let skip = second.as_ptr() as usize;
    let total = registry::scrub_all_except(skip);

    assert!(
        first.as_slice().iter().all(|&byte| byte == 0),
        "the block that was not skipped must be scrubbed"
    );
    assert!(
        second.as_slice().iter().all(|&byte| byte == PLAINTEXT),
        "scrub_all_except wiped the very block it was told to leave alone -- and on the fault path \
         that block is the stack the handler is running on, so this is the SIGILL bug"
    );
    assert_eq!(
        total,
        4096 + 8192,
        "the skipped block's length is counted: it *is* scrubbed, just later, by the handler's final \
         exit_group block. A total that excluded it would make expected_scrubbed a statement about \
         what could be scrubbed rather than about what was."
    );
}
