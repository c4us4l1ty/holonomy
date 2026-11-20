//! Block 4's second gate: *"Keystroke at offset 0 and at offset `N-1`: statistically identical
//! latency curves."* Measured, in one process, on the same document, alternately.
//!
//! The claim the gate exists to test is that a keystroke's cost does not know where in the document
//! it landed — that `locate` is a descent over cached aggregates, not a walk from the start. So the
//! document is a thirty-million-word one (180 MB of geometry, 44,000 leaves), and the two offsets
//! measured are the first byte and the last.
//!
//! # How the comparison is made
//!
//! Two curves measured one after the other are two measurements of the *machine* as well as the
//! code: a shared runner that slows down in the second half manufactures a difference. This harness
//! therefore interleaves — a block of samples at offset 0, then a block at offset `N-1`, repeated —
//! so drift moves both curves together, and a difference that survives interleaving is the code's.
//! `holonomy_ledger::timing::summarise` exists for exactly this: the caller owns the loop, the
//! ledger owns the distribution.
//!
//! Each timed sample is **one keystroke** — `Spine::locate` followed by an in-place edit of the
//! resident slab, the path AUDIT D-2 defines as the keystroke path. Insert and backspace alternate,
//! so that the measured leaf returns to its sealed length and the fixture never drifts; both are
//! allocation-free keystrokes, and the per-offset mix of the two is identical by construction.
//!
//! # What is not claimed here
//!
//! - **Not** the absolute echo bound, L-4's 8 ms p99 on reference hardware. It is asserted as a
//!   ceiling, because a keystroke that costs milliseconds anywhere is already a defect, but the
//!   runner this measures on is a shared CI machine and a shared machine cannot certify a bound.
//! - **Not** a fault-in. The first keystroke at each offset faults its leaf in, and a fault-in is
//!   slab-boundary work (D-2), not the keystroke; both ends are warmed before the first sample.
//! - **Not** a "curve" in the statistical sense of a fitted distribution. What is compared is the
//!   shape at p50 and p99, against a stated band. The band's job is to reject the failure mode the
//!   gate names — cost proportional to the offset, which is a factor of thousands — not to certify
//!   that two nanosecond figures are equal, which no shared machine could.
//!
//! The control at the end is what keeps the comparison honest: a whole-document walk is timed in
//! the same process, with the same clock, and must cost the keystroke's price hundreds of times
//! over. If the counters, the clock or the fixture were fake, that control would say so.

use std::hint::black_box;
use std::time::{Duration, Instant};

use holonomy_ledger::timing::{measure, summarise, Summary};
use holonomy_text::{EditOutcome, ResidentSet, SegmentId, SegmentSource, Slab, Spine};

/// Thirty million words at six bytes each — the size the gate names.
const WORDS: u64 = 30_000_000;
const BYTES_PER_WORD: u64 = 6;
/// A leaf holds an ordinary run of text, as the Eye's sweep assumes.
const LEAF_BYTES: u32 = 4_096;
/// The resident budget: viewport-sized, and never derived from the document.
const BUDGET: usize = 4 * 1024 * 1024;
/// Slack reserved per resident segment for in-place editing.
const SLACK: usize = 64;
/// Timed samples per offset.
const SAMPLES: usize = 4_096;
/// Samples taken at one offset before switching to the other, so drift is common-mode.
const BLOCK: usize = 256;
/// Keystrokes at both ends before the first timed sample: the fault-in is not the keystroke.
const WARMUP: usize = 64;
/// How far apart two curves may sit at p50 and still be called the same shape.
const P50_FACTOR: u32 = 2;
/// The same at p99, where a shared runner's tail is allowed to be rougher.
const P99_FACTOR: u32 = 4;
/// L-4's echo bound, asserted as a ceiling on any single keystroke's p99.
const ECHO_CEILING: Duration = Duration::from_millis(8);
/// How much a whole-document walk must cost over a keystroke, for the control to mean anything.
const WALK_FACTOR: u32 = 100;

/// A stand-in for the world-line: ASCII pages of a fixed size, any segment id.
struct Pages {
    bytes: usize,
}

impl SegmentSource for Pages {
    fn load(&mut self, _segment: SegmentId) -> Result<Vec<u8>, String> {
        Ok(vec![b'a'; self.bytes])
    }
}

/// One keystroke at `at`: a descent, then an in-place edit of the resident slab.
///
/// Returns the leaf's length after the edit, so the caller can black-box something with the work in
/// it and assert the fixture did not drift.
fn stroke(spine: &Spine, set: &mut ResidentSet<Pages>, at: u64, insert: bool) -> usize {
    let hit = spine.locate(at).expect("in range");
    let text = set
        .fault_in_mut(hit.slab().segment())
        .expect("the source supplies every segment");
    let within = hit.within() as usize;
    if insert {
        let outcome = text.insert(within, b"k").expect("in range");
        assert_eq!(
            outcome,
            EditOutcome::InPlace,
            "a keystroke must not need a split"
        );
    } else {
        text.delete(within, within + 1).expect("in range");
    }
    text.len()
}

/// Whether two curves are within `factor` of each other, in both directions.
fn within_factor(one: Duration, other: Duration, factor: u32) -> bool {
    one <= other * factor && other <= one * factor
}

/// One printed curve, so CI's log carries the numbers the assertions were made against.
fn report(name: &str, summary: &Summary) {
    println!(
        "HOLONOMY_KEYSTROKE_LATENCY at={} samples={} mean_ns={} p50_ns={} p99_ns={} max_ns={}",
        name,
        summary.samples,
        summary.mean.as_nanos(),
        summary.p50.as_nanos(),
        summary.p99.as_nanos(),
        summary.max.as_nanos()
    );
}

#[test]
fn a_keystroke_costs_the_same_at_the_start_and_at_the_end_of_thirty_million_words() {
    // The document, as geometry: one leaf per segment, a leaf of ordinary text each.
    let leaves = WORDS * BYTES_PER_WORD / u64::from(LEAF_BYTES);
    let bytes = leaves * u64::from(LEAF_BYTES);
    let spine =
        Spine::from_slabs((0..leaves).map(|leaf| Slab::new(SegmentId::new(leaf), 0, LEAF_BYTES)));
    assert_eq!(
        spine.slab_count() as u64,
        leaves,
        "one leaf per segment, nothing coalesced"
    );
    assert_eq!(spine.len(), bytes, "the document's length");
    let last = bytes - 1;

    let first_segment = spine.locate(0).expect("in range").slab().segment();
    let last_segment = spine.locate(last).expect("in range").slab().segment();
    assert_ne!(first_segment, last_segment, "the offsets are far apart");

    let source = Pages {
        bytes: LEAF_BYTES as usize,
    };
    let mut set = ResidentSet::new(source, BUDGET, SLACK).expect("a non-zero budget");

    // Warm both viewports. The first keystroke at each offset faults a leaf in, and that is
    // slab-boundary work (AUDIT D-2), not the keystroke this gate measures.
    for _ in 0..WARMUP {
        let _ = stroke(&spine, &mut set, 0, true);
        let _ = stroke(&spine, &mut set, 0, false);
        let _ = stroke(&spine, &mut set, last, true);
        let _ = stroke(&spine, &mut set, last, false);
    }
    assert!(
        set.contains(first_segment) && set.contains(last_segment),
        "both ends must be resident before the measurement"
    );

    // Interleaved blocks: a machine drifting under load moves both curves, not one of them.
    let mut at_start = Vec::with_capacity(SAMPLES);
    let mut at_end = Vec::with_capacity(SAMPLES);
    let mut lengths = (usize::MAX, 0);
    for _ in 0..SAMPLES / BLOCK {
        for sample in 0..BLOCK {
            let started = Instant::now();
            let length = stroke(&spine, &mut set, 0, sample % 2 == 0);
            at_start.push(started.elapsed());
            lengths = (lengths.0.min(length), lengths.1.max(length));
        }
        for sample in 0..BLOCK {
            let started = Instant::now();
            let length = stroke(&spine, &mut set, last, sample % 2 == 0);
            at_end.push(started.elapsed());
            lengths = (lengths.0.min(length), lengths.1.max(length));
        }
    }

    let start = summarise(at_start);
    let end = summarise(at_end);
    report("0", &start);
    report("N-1", &end);

    // 1. The fixture did real work and did not drift: every timed edit left the leaf at its sealed
    //    length or one byte past it, and nothing else. A leaf that grew would have started refusing
    //    keystrokes; a leaf that shrank would have been deleting bytes it never inserted.
    let leaf = u64::from(LEAF_BYTES) as usize;
    assert_eq!(
        lengths,
        (leaf, leaf + 1),
        "the timed edits moved the fixture: {lengths:?}"
    );
    assert_eq!(start.samples, SAMPLES, "every sample at offset 0 counted");
    assert_eq!(end.samples, SAMPLES, "every sample at N-1 counted");

    // 2. The gate: the two curves have the same shape. p50 within a factor of two and p99 within a
    //    factor of four, which rejects the failure mode the gate names — work proportional to the
    //    offset, a factor of thousands — while acknowledging that a shared runner owns the tail.
    assert!(
        within_factor(start.p50, end.p50, P50_FACTOR),
        "p50: {} ns at offset 0 against {} ns at N-1, over {P50_FACTOR}x apart",
        start.p50.as_nanos(),
        end.p50.as_nanos()
    );
    assert!(
        within_factor(start.p99, end.p99, P99_FACTOR),
        "p99: {} ns at offset 0 against {} ns at N-1, over {P99_FACTOR}x apart",
        start.p99.as_nanos(),
        end.p99.as_nanos()
    );

    // 3. Neither curve is a keystroke anyone would feel: L-4's echo ceiling holds at both ends, on
    //    a shared machine, which is a ceiling and not a certification.
    for (name, summary) in [("0", &start), ("N-1", &end)] {
        assert!(
            summary.p99 < ECHO_CEILING,
            "offset {name}: p99 is {} ms, over the {ECHO_CEILING:?} ceiling",
            summary.p99.as_millis()
        );
    }

    // 4. The control. A whole-document walk, timed by the same clock in the same process, must cost
    //    the keystroke's price hundreds of times over — otherwise the clock, the counters or the
    //    fixture is not measuring what this file says it is.
    let walk = measure(8, || black_box(spine.spans_in(0, spine.len()).len()));
    println!(
        "HOLONOMY_KEYSTROKE_CONTROL walk_p50_ns={} keystroke_p50_ns={}",
        walk.p50.as_nanos(),
        start.p50.as_nanos()
    );
    assert_eq!(walk.samples, 8, "eight walks");
    assert!(
        walk.p50 > start.p50 * WALK_FACTOR && walk.p50 > end.p50 * WALK_FACTOR,
        "a whole-document walk cost {} ns against a keystroke's {} ns and {} ns: under \
         {WALK_FACTOR}x, so the harness is not distinguishing work from position",
        walk.p50.as_nanos(),
        start.p50.as_nanos(),
        end.p50.as_nanos()
    );
}
