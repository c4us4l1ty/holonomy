//! What opening a thirty-million-word document costs: Block 4's first gate, measured.
//!
//! The gate says: *"Open a synthetic 30 M-word document: memory at open is within a fixed bound
//! that identical input of 1 page also satisfies."* This binary measures both halves of that
//! sentence instead of arguing about them, and it finds that **only one half holds**.
//!
//! What is counted:
//!
//! 1. **The index**, by the counting allocator: the live bytes the spine holds once it is built
//!    from the document's leaves — one leaf per segment, as a world-line would describe it.
//! 2. **The resident plaintext**, under pressure: a `ResidentSet` with a viewport budget is asked
//!    for four thousand segments of the big document and must evict rather than grow.
//!
//! What the run proves is the *bounds* asserted below, not the digits of a printed figure: the
//! numbers are printed per size as `HOLONOMY_OPEN_MEMORY`, and every one of them is also an
//! assertion, so a green run is the measurement.
//!
//! - Resident plaintext is exactly what L-2 claims: bounded by a budget fixed at construction,
//!   and a document a thousand times longer does not move it.
//! - The **index is not fixed**. It is two tree nodes per leaf — a measured, small constant, and
//!   under three percent of the document's own bytes — but it grows with the number of leaves, so
//!   a thirty-million-word open costs megabytes where a one-page open costs tens of bytes.
//!   That is the price of `locate` being a descent instead of a scan (AUDIT D-1), and no amount of
//!   care in the resident set makes it zero. The gate's literal wording therefore does not hold
//!   for total memory; AUDIT D-36 records the measurement and amends the clause to say which
//!   memory it means.
//!
//! This binary installs the counting allocator as its global allocator and holds a single test:
//! counters are process-global, so a sibling test's allocations would be attributed to the open
//! (see `holonomy_ledger::alloc`'s module docs).

use holonomy_ledger::alloc::{AllocStats, Counting};
use holonomy_text::{ResidentSet, SegmentId, SegmentSource, Slab, Spine};

#[global_allocator]
static COUNTING: Counting = Counting;

/// Thirty million words at six bytes each — the size the gate names.
const WORDS: u64 = 30_000_000;
const BYTES_PER_WORD: u64 = 6;
/// One page of text, for the small side of the comparison.
const PAGE_BYTES: u64 = 4_096;
/// A leaf holds an ordinary run of text, as the Eye's sweep assumes.
const LEAF_BYTES: u32 = 4_096;
/// A screen's worth of leaves, from the same sweep.
const SCREEN_LEAVES: u64 = 300;
/// The resident budget: viewport-sized, fixed at construction, never derived from the document.
const BUDGET: usize = 4 * 1024 * 1024;
/// Slack reserved per resident segment for in-place editing.
const SLACK: usize = 64;
/// More segments than the budget can hold, so the set has to choose.
const PRESSURE_SEGMENTS: u64 = 4_000;
/// What a leaf is allowed to cost the index. A balanced tree over `n` leaves holds `2n - 1` nodes,
/// each one `Arc<Node>`: a 16-byte header over two child pointers and three aggregates, so about
/// fifty-six bytes a node and a hundred and twelve a leaf. That 112 is the layout's arithmetic;
/// what CI proves is the **band** below, so the claim does not rest on the prediction being exact
/// and a legitimate node change of a few bytes does not red the build. Sixteen bytes a node under
/// the arithmetic and the same again over it is a factor the platform's `Arc` cannot produce.
const PER_LEAF_MIN: u64 = 64;
const PER_LEAF_MAX: u64 = 192;

/// A stand-in for the world-line: ASCII pages of a fixed size, any segment id.
struct Pages {
    bytes: usize,
}

impl SegmentSource for Pages {
    fn load(&mut self, _segment: SegmentId) -> Result<Vec<u8>, String> {
        Ok(vec![b'a'; self.bytes])
    }
}

/// One document, opened, with what it cost to hold and what a screen of it cost to look at.
struct Opened {
    /// Bytes of text in the document.
    bytes: u64,
    /// Leaves the spine carries.
    leaves: u64,
    /// Live bytes the spine holds after construction.
    index: u64,
    /// `index` in thousandths of a byte per leaf, so the report stays integral.
    millibytes_per_leaf: u64,
    /// Capacity the resident set held after one screen was faulted in.
    resident_screen: usize,
    /// Capacity it held after four thousand segments — the bound, under pressure.
    resident_pressure: usize,
    /// How many segments were still resident at that point.
    segments_pressure: usize,
}

/// Opens `bytes` of text as a spine and reads a screen of it through a resident set.
fn open(bytes: u64) -> Opened {
    // Leaves of an ordinary size, and a final partial leaf, so the document's length is exact.
    let full = bytes / u64::from(LEAF_BYTES);
    let tail = bytes % u64::from(LEAF_BYTES);
    let leaves = if tail == 0 { full } else { full + 1 };

    let before = AllocStats::snapshot();
    let spine = Spine::from_slabs((0..leaves).map(|leaf| {
        let len = if leaf + 1 == leaves && tail != 0 {
            u32::try_from(tail).expect("the tail is shorter than a leaf")
        } else {
            LEAF_BYTES
        };
        Slab::new(SegmentId::new(leaf), 0, len)
    }));
    let index = AllocStats::snapshot()
        .live_bytes
        .saturating_sub(before.live_bytes);

    // The spine describes the document exactly: nothing coalesced (every leaf names its own
    // segment), nothing dropped, no byte invented or lost.
    assert_eq!(
        spine.slab_count() as u64,
        leaves,
        "{bytes}: one leaf per segment"
    );
    assert_eq!(spine.len(), bytes, "{bytes}: the document's length");

    let source = Pages {
        bytes: LEAF_BYTES as usize,
    };
    let mut set = ResidentSet::new(source, BUDGET, SLACK).expect("a non-zero budget");
    for leaf in 0..SCREEN_LEAVES {
        let _ = set
            .fault_in(SegmentId::new(leaf))
            .expect("the source supplies every segment");
    }
    let resident_screen = set.allocated_bytes();
    assert!(
        resident_screen <= BUDGET,
        "{bytes}: one screen held {resident_screen} bytes over a {BUDGET}-byte budget"
    );

    // Pressure: ask for far more than the budget can hold. The set must evict, not grow, and it
    // must keep more than a screen — a set that held nothing would satisfy the bound for the
    // wrong reason.
    for leaf in 0..PRESSURE_SEGMENTS {
        let _ = set
            .fault_in(SegmentId::new(leaf))
            .expect("the source supplies every segment");
    }
    let resident_pressure = set.allocated_bytes();
    let segments_pressure = set.resident_segments();
    assert!(
        resident_pressure <= BUDGET,
        "{bytes}: {segments_pressure} resident segments held {resident_pressure} bytes over a \
         {BUDGET}-byte budget"
    );
    assert!(
        segments_pressure as u64 >= SCREEN_LEAVES,
        "{bytes}: only {segments_pressure} segments stayed resident, under a screen"
    );

    Opened {
        bytes,
        leaves,
        index,
        millibytes_per_leaf: index.saturating_mul(1_000) / leaves,
        resident_screen,
        resident_pressure,
        segments_pressure,
    }
}

#[test]
fn opening_thirty_million_words_costs_the_index_and_none_of_the_text() {
    let big_bytes = WORDS * BYTES_PER_WORD;
    let big = open(big_bytes);
    let small = open(PAGE_BYTES);

    for (name, measured) in [("30 M words", &big), ("1 page", &small)] {
        println!(
            "HOLONOMY_OPEN_MEMORY doc={} bytes={} leaves={} index={} mB_per_leaf={} \
             resident_screen={} resident_pressure={} segments_pressure={}",
            name,
            measured.bytes,
            measured.leaves,
            measured.index,
            measured.millibytes_per_leaf,
            measured.resident_screen,
            measured.resident_pressure,
            measured.segments_pressure
        );
    }

    // 1. The document really is the gate's size, and the small one really is a page.
    assert_eq!(big.bytes, 180_000_000, "thirty million words at six bytes");
    assert!(
        big.leaves > 40_000,
        "the big document is {} leaves, not a document-scale one",
        big.leaves
    );
    assert_eq!(small.leaves, 1, "a page is one leaf");

    // 2. The half of the gate that holds: resident plaintext does not know the document's length.
    //    Both documents were asked for the same leaves through the same budget, and the bound is
    //    the budget, not the text behind it.
    assert_eq!(
        big.resident_screen, small.resident_screen,
        "one screen of text costs the same at both sizes"
    );
    assert_eq!(
        big.resident_pressure, small.resident_pressure,
        "the pressure bound is the budget, not the document"
    );
    assert!(
        big.resident_pressure < PRESSURE_SEGMENTS as usize * LEAF_BYTES as usize,
        "the set kept {} bytes of {} asked for: it did not evict",
        big.resident_pressure,
        PRESSURE_SEGMENTS as usize * LEAF_BYTES as usize
    );

    // 3. The half that does not: the index is one node per leaf. Its cost per leaf is small and
    //    bounded, and it is a fixed *fraction* of the document — but it is not fixed in absolute
    //    terms, so "memory at open is within a fixed bound" is false as written. D-36 amends it.
    assert!(
        big.millibytes_per_leaf >= PER_LEAF_MIN.saturating_mul(1_000)
            && big.millibytes_per_leaf <= PER_LEAF_MAX.saturating_mul(1_000),
        "the index costs {} millibytes a leaf, outside the {PER_LEAF_MIN}-{PER_LEAF_MAX} band",
        big.millibytes_per_leaf
    );
    assert!(
        small.index < 1_024,
        "a page's index is {} bytes, which is not a small constant",
        small.index
    );
    assert!(
        big.index > small.index,
        "the index did not grow with the leaves, so the literal gate holds and D-36 is wrong"
    );
    // Under five percent of the document's bytes is the design's own arithmetic: 4096-byte leaves
    // and `2n - 1` nodes of two Arcs and three aggregates. Asserted so a silent change to either is
    // caught here.
    assert!(
        big.index.saturating_mul(20) <= big.bytes,
        "the index is {} bytes of a {} byte document, over five percent",
        big.index,
        big.bytes
    );

    // 4. The screen really held a screen: `SCREEN_LEAVES` segments of text, not fewer, so the
    //    bound above was met by holding a viewport and not by holding nothing.
    assert!(
        big.resident_screen >= SCREEN_LEAVES as usize * LEAF_BYTES as usize,
        "a screen held {} bytes, under the {} a screen of text needs",
        big.resident_screen,
        SCREEN_LEAVES as usize * LEAF_BYTES as usize
    );
}
