//! What a viewport costs on a document nobody would want to open.
//!
//! Block 5's second gate claims that layout cost is invariant in document length. The half of that
//! claim which can be measured without a font decides it: **how many index reads** it takes to find
//! the window, and **how many bytes** the layout would be handed. Both are counted here on a
//! thirty-million-word document — 180 MB of text as geometry, 44,000 leaves — and compared with a
//! document a thousand times smaller.
//!
//! What this does not measure is wall-clock time: shaping needs a real font, and a fake shaper
//! would make a timing number about the fake. Reads and bytes are the algorithm; seconds are the
//! machine.

use holonomy_eye::{byte_window, Fixed, HeightModel};
use holonomy_text::{SegmentId, Slab, Spine};

/// Thirty million words at six bytes each — the size the gate names.
const WORDS: u64 = 30_000_000;
const BYTES_PER_WORD: u64 = 6;
/// A leaf holds an ordinary run of text.
const LEAF_BYTES: u32 = 4096;
/// Four logical units per leaf, and a screen of twelve hundred: three hundred leaves on screen.
const LEAF_UNITS: i64 = 4;
const SCREEN: i64 = 1200;
/// Scroll by more than a screen, so the sweep sees unaligned viewports too.
const SCROLL: i64 = 2000;

/// A document of `bytes` bytes as geometry, with one height per leaf.
fn document(bytes: u64) -> (Spine, HeightModel) {
    let leaves = bytes / u64::from(LEAF_BYTES);
    let slabs: Vec<Slab> = (0..leaves)
        .map(|leaf| Slab::new(SegmentId::new(leaf), 0, LEAF_BYTES))
        .collect();
    let spine = Spine::from_slabs(slabs);
    let mut heights = HeightModel::new(spine.slab_count() as usize);
    for leaf in 0..spine.slab_count() as usize {
        heights.set(leaf, Fixed::units(LEAF_UNITS));
    }
    (spine, heights)
}

/// One viewport per screen down the whole document.
#[derive(Debug)]
struct Sweep {
    /// The worst single viewport's index reads.
    reads: u64,
    /// The mean bytes the layout would be handed per viewport.
    mean_bytes: u64,
    /// The most bytes any one viewport asked for.
    worst_bytes: u64,
    /// How many screens the document holds.
    screens: u64,
}

fn sweep(spine: &Spine, heights: &HeightModel) -> Sweep {
    let total = heights.total();
    let mut top = Fixed::ZERO;
    let mut reads = 0u64;
    let mut bytes = 0u64;
    let mut worst_bytes = 0u64;
    let mut screens = 0u64;
    while top < total {
        heights.reset_reads();
        let window = byte_window(spine, heights, top, Fixed::units(SCREEN));
        reads = reads.max(heights.reads());
        bytes += window.len();
        worst_bytes = worst_bytes.max(window.len());
        screens += 1;
        top += Fixed::units(SCROLL);
    }
    Sweep {
        reads,
        mean_bytes: bytes / screens.max(1),
        worst_bytes,
        screens,
    }
}

#[test]
fn bytes_shaped_do_not_grow_with_the_document() {
    let big_bytes = WORDS * BYTES_PER_WORD;
    let (small_spine, small_heights) = document(big_bytes / 1000);
    let (big_spine, big_heights) = document(big_bytes);

    let small = sweep(&small_spine, &small_heights);
    let big = sweep(&big_spine, &big_heights);

    // A screen is 1,200 units of 4-unit leaves: 300 leaves of 4,096 bytes, so 1.2 MB, plus the two
    // leaves the screen's edges land in. **The bound does not contain the document's length** —
    // that is the whole claim, and it has to hold for the small document too, which is why both are
    // asked the same question.
    let per_screen = u64::try_from(SCREEN / LEAF_UNITS).expect("positive");
    let screen_bytes = per_screen * u64::from(LEAF_BYTES);
    let edge = 2 * u64::from(LEAF_BYTES);
    for (name, measured) in [("180 MB", &big), ("180 KB", &small)] {
        assert!(
            measured.worst_bytes <= screen_bytes + edge,
            "{name}: the worst viewport asked for {} bytes, over the {} a screen needs",
            measured.worst_bytes,
            screen_bytes + edge
        );
        assert!(
            measured.mean_bytes <= screen_bytes + edge,
            "{name}: the mean viewport asked for {} bytes",
            measured.mean_bytes
        );
    }
    // The big sweep really did walk the document, rather than stopping at its first screen.
    assert!(
        big.screens >= 10 * small.screens,
        "the big sweep visited {} screens, the small one {}",
        big.screens,
        small.screens
    );
}

#[test]
fn finding_the_window_costs_a_logarithm_not_a_walk() {
    let big_bytes = WORDS * BYTES_PER_WORD;
    let (small_spine, small_heights) = document(big_bytes / 1000);
    let (big_spine, big_heights) = document(big_bytes);
    let small = sweep(&small_spine, &small_heights);
    let big = sweep(&big_spine, &big_heights);

    // Two descents of a Fenwick tree, so at most twice the depth plus the constant the descent
    // itself spends. 44,000 leaves is a depth of 16; 64 is a ceiling with room to spare.
    assert!(
        big.reads <= 64,
        "a viewport on {} leaves cost {} index reads",
        big_spine.slab_count(),
        big.reads
    );
    assert!(
        small.reads <= 64,
        "a viewport on {} leaves cost {} index reads",
        small_spine.slab_count(),
        small.reads
    );
    // A thousandfold document is about ten more levels, two descents each: the reads may grow by a
    // logarithm, and by nothing else.
    assert!(
        big.reads <= small.reads + 24,
        "{} reads at 1000x the length against {}",
        big.reads,
        small.reads
    );
}
