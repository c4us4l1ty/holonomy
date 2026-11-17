//! The keystroke gate: GATES law L-2's "zero allocations on the keystroke path", measured.
//!
//! This is deliberately an **integration** test binary that installs the counting allocator
//! itself. A unit test would run under the default allocator and measure nothing — the failure
//! mode Block 1 recorded after CI caught it. Counters are process-global, so this binary holds one
//! test function and no siblings: a concurrent test could allocate inside the measured window and
//! be blamed for it (`holonomy_ledger::alloc` module docs).
//!
//! Two things are measured here, and the second is why the first means anything:
//!
//! 1. The keystroke — `Spine::locate` then an in-place edit of the resident slab — performs
//!    **zero** allocations. `locate` is a descent over cached aggregates, the hit path only
//!    refreshes a recency counter, and `SlabText::insert` moves bytes into capacity that already
//!    exists.
//! 2. The control — faulting a segment in on a miss — is slab-boundary work and **is** seen
//!    allocating. If this control ever stops allocating, the counters are not being installed and
//!    every "zero" above is a lie, which is exactly the trap this file exists to close.

use std::hint::black_box;

use holonomy_ledger::alloc::{claim_quiet, AllocStats, Counting};
use holonomy_text::{EditOutcome, ResidentSet, SegmentId, SegmentSource, Slab, Spine};

#[global_allocator]
static COUNTING: Counting = Counting;

/// A stand-in for the world-line: ASCII pages of a fixed size, any segment id.
struct Pages {
    bytes: usize,
}

impl SegmentSource for Pages {
    fn load(&mut self, _segment: SegmentId) -> Result<Vec<u8>, String> {
        Ok(vec![b'a'; self.bytes])
    }
}

#[test]
fn the_keystroke_path_allocates_nothing_and_the_control_proves_it() {
    // A one-page document, as geometry: segment 0 holds 64 bytes.
    let spine = Spine::from_slabs([Slab::new(SegmentId::new(0), 0, 64)]);
    let mut resident =
        ResidentSet::new(Pages { bytes: 64 }, 4 * 65_536, 4_096).expect("non-zero budget");

    let target = 10u64;
    let hit = spine.locate(target).expect("in range");
    let segment = hit.slab().segment();
    let within = hit.within() as usize;
    assert_eq!(segment, SegmentId::new(0));

    // Warm the viewport. Faulting in is slab-boundary work; it may allocate, and it must be seen
    // doing so, or the counters are not installed and the quiet claims below are worthless.
    let before_fault = AllocStats::snapshot();
    resident.fault_in(segment).expect("source works");
    let fault_delta = before_fault.delta(&AllocStats::snapshot());
    assert!(
        fault_delta.allocations >= 1,
        "the control must allocate on a fault-in, otherwise nothing is being counted: {fault_delta}"
    );
    assert!(resident.contains(segment));

    // 1. A typed character: locate, hit a resident slab, insert in place.
    claim_quiet(|| {
        let hit = spine.locate(target).expect("in range");
        let text = resident
            .fault_in_mut(hit.slab().segment())
            .expect("resident");
        let outcome = text.insert(hit.within() as usize, b"k").expect("in range");
        assert_eq!(outcome, EditOutcome::InPlace);
        black_box(outcome);
    });

    let typed = resident.text(segment).expect("still resident");
    assert_eq!(typed.len(), 65);
    assert_eq!(typed.as_bytes()[within], b'k');

    // 2. A backspace: also allocation-free.
    claim_quiet(|| {
        let text = resident.fault_in_mut(segment).expect("resident");
        text.delete(within, within + 1).expect("in range");
        black_box(text.len());
    });
    assert_eq!(
        resident.text(segment).expect("still resident").len(),
        64,
        "the delete must have taken effect"
    );

    // 3. Filling the slack, and the refusal when it is gone: neither may allocate. The refusal is
    //    the point — a buffer that grew instead would hide an allocator call inside the keystroke.
    let spare = resident.text(segment).expect("resident").spare();
    assert!(spare > 0, "the fixture must have slack to fill");
    let filler = vec![b'x'; spare];
    claim_quiet(|| {
        let text = resident.fault_in_mut(segment).expect("resident");
        assert_eq!(
            text.insert(0, &filler).expect("in range"),
            EditOutcome::InPlace
        );
        assert_eq!(text.spare(), 0);
        assert!(
            matches!(text.insert(0, b"!"), Ok(EditOutcome::WouldSplit { .. })),
            "a full slab must refuse, not grow"
        );
    });
    assert_eq!(resident.text(segment).expect("resident").spare(), 0);

    // The set never exceeded its budget through any of it.
    assert!(resident.allocated_bytes() <= resident.budget_bytes());
    assert_eq!(resident.resident_segments(), 1);
}
