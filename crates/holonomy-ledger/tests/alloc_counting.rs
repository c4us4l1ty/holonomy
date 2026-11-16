//! Integration proof of the counting allocator in its real usage pattern: installed as the
//! process-global allocator by the *consumer* (this test binary), with scopes measured around
//! code under test. Each counting claim lives in its own binary so sibling tests cannot race
//! the counters (see `holonomy_ledger::alloc` module docs).

use std::hint::black_box;

use holonomy_ledger::alloc::{AllocStats, Counting};

#[global_allocator]
static COUNTING: Counting = Counting;

#[test]
fn allocation_claim_is_measured_not_asserted() {
    let before = AllocStats::snapshot();

    let mut v: Vec<u64> = black_box(Vec::with_capacity(black_box(64)));
    v.extend(0..100u64);
    black_box(&v);

    let delta = before.delta(&AllocStats::snapshot());
    assert!(
        delta.allocations >= 1,
        "a growing Vec must have allocated and been counted: {delta}"
    );

    drop(v);
    let after = AllocStats::snapshot();
    assert!(
        after.deallocations > before.deallocations,
        "the drop must have deallocated and been counted"
    );
}

#[test]
fn a_quiet_scope_rejects_allocations() {
    // claim_quiet must fail loudly around code that allocates. This uses the panic as the
    // asserted signal: the gate exists so that a regression is a failing test, never a log line.
    let result = std::panic::catch_unwind(|| {
        holonomy_ledger::alloc::claim_quiet(|| {
            let leakable = vec![1u8; 32];
            black_box(leakable);
        });
    });
    assert!(
        result.is_err(),
        "an allocation inside claim_quiet must panic with the measured delta"
    );
}
