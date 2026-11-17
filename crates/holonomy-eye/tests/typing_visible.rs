//! The viewport sees the text being typed, before anything is sealed.
//!
//! This is D-7's central claim, and it is the one an editor actually depends on: a window of leaves
//! maps to a byte range, and the bytes under the caret are in that range even though no segment
//! holds them yet. Before D-7 the same question had no answer that did not invent a third kind of
//! overlay entry, because a leaf could only name sealed text.

use holonomy_eye::{byte_window, Fixed, HeightModel};
use holonomy_text::{Appended, SegmentId, SegmentSink, Typing};

/// A sink that seals without writing anything, so the test can type and seal freely. It hands
/// out increasing ids, as a world-line does — a sink that reused one id is caught by the register.
struct Null {
    next: u64,
}

impl Null {
    fn new() -> Self {
        Self { next: 0 }
    }
}

impl SegmentSink for Null {
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
        let segment = SegmentId::new(self.next);
        self.next += 1;
        Ok(Appended {
            segment,
            witness_sequence: self.next,
            segment_root: [0u8; 32],
            bytes_written: plaintext.len() as u64,
        })
    }
}

/// One height per leaf, all the same, so the window is decided by the leaves alone.
fn heights(spine: &holonomy_text::Spine) -> HeightModel {
    let mut heights = HeightModel::new(spine.slab_count() as usize);
    for leaf in 0..spine.slab_count() as usize {
        heights.set(leaf, Fixed::units(20));
    }
    heights
}

#[test]
fn a_window_covers_text_that_no_segment_holds() {
    let mut typing = Typing::new(Null::new(), 64);
    typing
        .insert(b"the sentence the user is typing")
        .expect("typed");
    assert_eq!(typing.sealed_bytes(), 0, "nothing is sealed yet");

    let spine = typing.spine();
    let window = byte_window(spine, &heights(spine), Fixed::ZERO, Fixed::units(20));
    assert_eq!(window.start(), 0);
    assert_eq!(window.end(), typing.document_len());
    assert_eq!(window.end(), 31, "\"the sentence the user is typing\"");
    assert_eq!(typing.pending().len(), 31);
}

#[test]
fn sealing_does_not_move_the_text_out_of_the_window() {
    // A buffer of eight bytes, so twenty bytes of typing seal twice.
    let mut typing = Typing::new(Null::new(), 8);
    typing.insert(b"0123456789abcdefghij").expect("typed");
    assert_eq!(typing.sealed_bytes(), 16);
    assert_eq!(typing.pending(), b"ghij");

    let spine = typing.spine();
    assert_eq!(spine.slab_count(), 3, "two sealed leaves and the open one");
    // A viewport tall enough for all three leaves.
    let window = byte_window(spine, &heights(spine), Fixed::ZERO, Fixed::units(60));
    // The whole document is in the window, the unsealed four bytes included.
    assert_eq!(window.start(), 0);
    assert_eq!(window.end(), typing.document_len());
    assert_eq!(window.end(), 20);
}

#[test]
fn a_viewport_inside_the_document_still_lands_on_whole_leaves() {
    let mut typing = Typing::new(Null::new(), 8);
    typing.insert(b"0123456789abcdefghij").expect("typed");
    let spine = typing.spine();
    let heights = heights(spine);
    // Three leaves of twenty units each; a viewport over the second one.
    let window = byte_window(spine, &heights, Fixed::units(20), Fixed::units(20));
    assert_eq!(window.leaves().leaves(), 1..2);
    assert_eq!(window.start(), 8);
    assert_eq!(window.end(), 16);
}
