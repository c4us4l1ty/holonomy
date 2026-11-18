//! The text behind a viewport: where the height index meets the resident set.
//!
//! [`byte_window`] answers *which bytes* are on screen from the height index and the spine alone;
//! [`window_text`] answers *what those bytes say*, faulting in exactly the leaves the viewport
//! covers. Together they are the caller Block 5's second gate names — the whole chain from "here is
//! a scroll position" to "here is the text to shape" — with one half still owed: the clock, which
//! needs a real shaper and a font (AUDIT OQ-12).
//!
//! What is measured here is the half that is a number: how many segments a screen costs, that the
//! same screen costs the same on a document 512 times longer, that a second look costs nothing, and
//! that the pending buffer is read as text rather than faulted.

use holonomy_eye::{byte_window, window_text, Fixed, HeightModel, WindowTextError};
use holonomy_text::{
    Appended, Draft, OpenSources, ResidentSet, SegmentId, SegmentSink, SegmentSource, Slab, Spine,
    Typing,
};
use std::collections::BTreeMap;

/// Bytes per leaf, so a screen of a few leaves is a byte range a test can name.
const LEAF_BYTES: u32 = 8;
/// Height per leaf, in logical units.
const LEAF_UNITS: i64 = 4;

/// The world-line as plaintext by segment id, counting how often it is opened.
#[derive(Debug, Default)]
struct World {
    segments: BTreeMap<u64, Vec<u8>>,
    loads: usize,
}

impl World {
    /// `leaves` segments of `LEAF_BYTES` bytes, each filled with a per-segment ASCII letter.
    fn of(leaves: u64) -> Self {
        let mut segments = BTreeMap::new();
        for leaf in 0..leaves {
            let byte = b'a' + u8::try_from(leaf % 26).expect("a remainder below 26 fits");
            segments.insert(leaf, vec![byte; LEAF_BYTES as usize]);
        }
        Self { segments, loads: 0 }
    }
}

impl SegmentSource for World {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        self.loads += 1;
        self.segments
            .get(&segment.get())
            .cloned()
            .ok_or_else(|| format!("no {segment}"))
    }
}

/// A sink that keeps what it is handed, so a session can be driven without a world-line.
#[derive(Debug, Default)]
struct Recorder {
    segments: Vec<Vec<u8>>,
}

impl SegmentSink for Recorder {
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
        let segment = SegmentId::new(self.segments.len() as u64);
        self.segments.push(plaintext.to_vec());
        Ok(Appended {
            segment,
            witness_sequence: self.segments.len() as u64,
            segment_root: [0u8; 32],
            bytes_written: plaintext.len() as u64,
        })
    }
}

/// A document of `leaves` leaves, one segment each, with every height measured.
fn document(leaves: u64) -> (Spine, HeightModel) {
    let spine = Spine::from_slabs(
        (0..leaves)
            .map(|leaf| Slab::new(SegmentId::new(leaf), 0, LEAF_BYTES))
            .collect::<Vec<_>>(),
    );
    let mut heights = HeightModel::new(spine.slab_count() as usize);
    for leaf in 0..spine.slab_count() as usize {
        heights.set(leaf, Fixed::units(LEAF_UNITS));
    }
    (spine, heights)
}

fn residency(world: World) -> ResidentSet<World> {
    ResidentSet::new(world, 1 << 20, 0).expect("a workable budget")
}

#[test]
fn a_viewport_reads_exactly_the_bytes_it_covers() {
    let (spine, heights) = document(40);
    let mut residents = residency(World::of(40));
    // A screen two leaves tall, in the middle of the document.
    let top = Fixed::units(5 * LEAF_UNITS);
    let window = byte_window(&spine, &heights, top, Fixed::units(2 * LEAF_UNITS));
    assert_eq!(window.leaves().leaves(), 5..7);
    assert_eq!(window.bytes(), 40..56);

    let read = window_text(
        &mut residents,
        &spine,
        &Draft::new(),
        &OpenSources::new(),
        window,
        b"",
    )
    .expect("readable");
    assert_eq!(read.origin(), 40);
    assert_eq!(read.end(), 56);
    assert_eq!(read.len(), 16);
    assert_eq!(
        read.text(),
        "ffffffffgggggggg",
        "leaf five is the sixth segment, and the text starts at its first byte"
    );
    assert_eq!(read.leaves().leaves(), 5..7);
    assert_eq!(residents.resident_segments(), 2, "two leaves, two segments");
    assert_eq!(residents.source().loads, 2, "one fault per leaf");
}

#[test]
fn reading_the_same_viewport_again_touches_no_segment() {
    let (spine, heights) = document(40);
    let mut residents = residency(World::of(40));
    let window = byte_window(
        &spine,
        &heights,
        Fixed::units(0),
        Fixed::units(3 * LEAF_UNITS),
    );
    let first = window_text(
        &mut residents,
        &spine,
        &Draft::new(),
        &OpenSources::new(),
        window,
        b"",
    )
    .expect("readable");
    let loads = residents.source().loads;
    let second = window_text(
        &mut residents,
        &spine,
        &Draft::new(),
        &OpenSources::new(),
        window,
        b"",
    )
    .expect("readable");
    assert_eq!(second.text(), first.text());
    assert_eq!(
        residents.source().loads,
        loads,
        "the resident set answered, and the world-line was not asked again"
    );
}

#[test]
fn an_open_leaf_reads_from_the_pending_buffer_and_faults_nothing() {
    let mut typing = Typing::new(Recorder::default(), 64);
    typing.insert(b"hello world").expect("typed");
    let mut heights = HeightModel::new(typing.spine().slab_count() as usize);
    heights.set(0, Fixed::units(LEAF_UNITS));
    let window = byte_window(
        typing.spine(),
        &heights,
        Fixed::ZERO,
        Fixed::units(LEAF_UNITS),
    );
    assert_eq!(window.bytes(), 0..11);

    let mut residents = residency(World::default());
    let read = window_text(
        &mut residents,
        typing.spine(),
        typing.draft(),
        typing.sources(),
        window,
        typing.pending(),
    )
    .expect("readable");
    assert_eq!(read.text(), "hello world");
    assert_eq!(
        residents.source().loads,
        0,
        "nothing has been sealed, so there is nothing to fault"
    );
}

#[test]
fn a_window_that_spans_sealed_text_and_the_open_buffer_reads_both() {
    let mut typing = Typing::new(Recorder::default(), 64);
    typing.insert(b"0123456789").expect("typed");
    typing.flush().expect("flushed");
    typing.insert(b"xy").expect("typed");
    let mut heights = HeightModel::new(typing.spine().slab_count() as usize);
    for leaf in 0..heights.len() {
        heights.set(leaf, Fixed::units(LEAF_UNITS));
    }
    let window = byte_window(
        typing.spine(),
        &heights,
        Fixed::ZERO,
        Fixed::units(2 * LEAF_UNITS),
    );
    assert_eq!(window.bytes(), 0..12, "the sealed ten and the pending two");

    let mut world = World::default();
    world.segments.insert(0, b"0123456789".to_vec());
    let mut residents = residency(world);
    let read = window_text(
        &mut residents,
        typing.spine(),
        typing.draft(),
        typing.sources(),
        window,
        typing.pending(),
    )
    .expect("readable");
    assert_eq!(read.text(), "0123456789xy");
    assert_eq!(
        residents.source().loads,
        1,
        "one sealed leaf was faulted; the open one was already in hand"
    );
}

#[test]
fn a_window_edge_inside_a_character_is_refused_with_its_offset() {
    // A leaf whose segment holds `é` and which claims one byte. The geometry can produce that — a
    // cut landing inside a character is OQ-15's asymmetry, and it is unchecked on the sealed side —
    // so the reader is where it is finally *detected*: half a character is refused with the offset
    // rather than handed to a renderer.
    let mut world = World::default();
    world.segments.insert(0, "é".as_bytes().to_vec());
    let spine = Spine::from_slabs(vec![Slab::new(SegmentId::new(0), 0, 1)]);
    let mut heights = HeightModel::new(1);
    heights.set(0, Fixed::units(LEAF_UNITS));
    let window = byte_window(&spine, &heights, Fixed::ZERO, Fixed::units(LEAF_UNITS));
    assert_eq!(window.bytes(), 0..1);

    let mut residents = residency(world);
    assert_eq!(
        window_text(
            &mut residents,
            &spine,
            &Draft::new(),
            &OpenSources::new(),
            window,
            b""
        ),
        Err(WindowTextError::NotText { at: 0 })
    );
}

#[test]
fn what_a_viewport_faults_does_not_depend_on_the_document_length() {
    let mut measured = Vec::new();
    for exponent in [3u32, 8, 12] {
        let leaves = 1u64 << exponent;
        let (spine, heights) = document(leaves);
        let mut residents = residency(World::of(leaves));
        // The same three-leaf screen, halfway down a document of 8, 256 and 4096 leaves.
        let top = Fixed::units((leaves as i64 / 2) * LEAF_UNITS);
        let window = byte_window(&spine, &heights, top, Fixed::units(3 * LEAF_UNITS));
        let read = window_text(
            &mut residents,
            &spine,
            &Draft::new(),
            &OpenSources::new(),
            window,
            b"",
        )
        .expect("readable");
        assert_eq!(read.len(), window.len());
        let loads = residents.source().loads;
        assert!(
            loads <= 4,
            "{loads} segments for a three-leaf screen on {leaves} leaves"
        );
        assert!(residents.resident_segments() <= 4);
        measured.push(loads);
        if exponent == 12 {
            // The text is the one at *that* position, not the document's first screen: leaf 2048
            // is the twentieth letter after a whole alphabet of leaves.
            assert_eq!(read.text(), "uuuuuuuuvvvvvvvvwwwwwwww");
        }
    }
    assert_eq!(
        measured[0], measured[2],
        "the same screen faults the same count on a document 512 times longer: {measured:?}"
    );
}
