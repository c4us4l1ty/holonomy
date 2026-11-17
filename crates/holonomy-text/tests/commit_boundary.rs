//! Seal → append → witness → new root, against a real world-line.
//!
//! The unit tests in `src/commit.rs` prove the commit boundary's logic against a stand-in sink.
//! This file proves the seam it exists for: an edited resident slab is sealed by `holonomy-line`
//! itself, committed with a witness, and the spine is re-rooted onto the segment the world-line
//! actually wrote. Nothing here mocks the container, the seal, the durability barrier or the MMR;
//! the only synthetic part is the passphrase.
//!
//! `holonomy-text` does not depend on `holonomy-line` in either direction at build time: the spine
//! speaks to the world-line through [`SegmentSink`] and [`SegmentSource`], exactly as it speaks to
//! the Eye. The two adapters below are what an application crate will write. They live in this
//! test so that the seam is executed by CI instead of described in prose.

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;

use holonomy_line::{LineHeader, WorldLine};
use holonomy_seal::{
    create_capsule_for, generate_container_id, Capsule, DEMKey, KdfParamsRecord, MasterKeys,
    ParamsClass,
};
use holonomy_text::{
    commit_slab, Appended, CommitError, Committed, Draft, EditOutcome, History, ResidentSet,
    SegmentId, SegmentSink, SegmentSource, Slab, SlabText, Spine,
};

/// A world-line path that is deleted when the test ends.
struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn temp_path() -> Temp {
    let id = generate_container_id().expect("entropy");
    let suffix = id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Temp(std::env::temp_dir().join(format!("holonomy-commit-{suffix}.wavefunction")))
}

/// The capsule for the header and the DEM key that seals segments.
fn material(passphrase: &[u8], salt: [u8; 64]) -> (Capsule, DEMKey) {
    let keys = MasterKeys::derive(passphrase, &salt, None, ParamsClass::Test).expect("derives");
    create_capsule_for(keys.unwrap_key(), None).expect("a capsule")
}

fn create_line<'key>(path: &Temp, capsule: Capsule, dem: &'key DEMKey) -> WorldLine<'key> {
    let params = KdfParamsRecord {
        m_kib: 8_192,
        t: 1,
        p: 1,
    };
    let header = LineHeader::new(params, [0x5C; 64], capsule).expect("a valid header");
    WorldLine::create(&path.0, header, dem).expect("creates the world-line")
}

/// Reads one committed segment into an editable slab, as a fault-in would.
fn resident(line: &WorldLine<'_>, segment: u64, slack: usize) -> SlabText {
    let bytes = line.open_segment(segment).expect("committed").to_vec();
    SlabText::from_plaintext(bytes, slack).expect("the segment is text")
}

/// The world-line as the commit boundary sees it.
struct LineSink<'a, 'key> {
    line: &'a mut WorldLine<'key>,
}

impl SegmentSink for LineSink<'_, '_> {
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
        let receipt = self
            .line
            .append_segments(&[plaintext])
            .map_err(|e| e.to_string())?;
        Ok(Appended {
            // The appended segment is the last one committed; sequences are dense from zero.
            segment: SegmentId::new(receipt.segment_count - 1),
            witness_sequence: receipt.witness_sequence,
            segment_root: receipt.segment_root,
            bytes_written: receipt.bytes_appended,
        })
    }
}

/// The world-line as the resident set sees it.
struct LineSource<'a, 'key> {
    line: &'a WorldLine<'key>,
}

impl SegmentSource for LineSource<'_, '_> {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        let bytes = self
            .line
            .open_segment(segment.get())
            .map_err(|e| e.to_string())?;
        Ok(bytes.to_vec())
    }
}

#[test]
fn a_keystroke_becomes_a_sealed_segment_and_a_new_root() {
    let path = temp_path();
    let (capsule, dem) = material(b"commit boundary", [0x5C; 64]);
    let mut line = create_line(&path, capsule, &dem);
    let receipt = line
        .append_segments(&[b"hello world"])
        .expect("one segment");
    assert_eq!(receipt.segment_count, 1);
    assert_eq!(receipt.witness_sequence, 1);

    let spine = Spine::from_slabs([Slab::new(SegmentId::new(0), 0, 11)]);
    let span = spine.spans_in(0, spine.len()).pop().expect("one span");

    // The keystroke: in place, inside the resident slab. `tests/keystroke_allocations.rs` is what
    // measures the absence of allocation; this test asserts the text and the structure.
    let mut text = resident(&line, 0, 32);
    assert_eq!(text.as_str(), "hello world");
    assert_eq!(text.insert(5, b",").expect("fits"), EditOutcome::InPlace);
    assert_eq!(text.as_str(), "hello, world");

    // The commit: seal, append, witness, new root.
    let mut sink = LineSink { line: &mut line };
    let committed = commit_slab(&spine, span, &text, &mut sink).expect("commits");
    let Committed::Sealed {
        spine: after,
        witness,
    } = committed
    else {
        panic!("an edited slab must seal, not vanish");
    };
    assert_eq!(witness.segment, SegmentId::new(1));
    assert_eq!(witness.sequence, 2, "genesis, the first append, then this");
    assert_eq!(witness.segment_root, line.segment_root());

    // The new root names the new segment over the whole extent the old leaf covered.
    let spans = after.spans_in(0, after.len());
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].offset(), 0);
    assert_eq!(spans[0].slab().segment(), SegmentId::new(1));
    assert_eq!((spans[0].slab().start(), spans[0].slab().len()), (0, 12));
    assert_eq!(after.len(), 12, "the comma is in the document now");
    assert_eq!(spine.len(), 11, "the old root still describes its document");

    // Durable and authenticated: a fresh handle sees both segments and the same root.
    let reopened = WorldLine::open(&path.0, &dem).expect("reopens");
    assert_eq!(reopened.segment_count(), 2);
    assert_eq!(reopened.segment_root(), witness.segment_root);
    let original = reopened.open_segment(0).expect("segment 0").to_vec();
    assert_eq!(original, b"hello world", "the old segment is untouched");
    let sealed = reopened.open_segment(1).expect("segment 1").to_vec();
    assert_eq!(
        sealed, b"hello, world",
        "the sealed bytes are what was typed"
    );
    let report = reopened.verify().expect("the file authenticates");
    assert_eq!(report.segments_verified, 2);
    assert_eq!(report.plaintext_bytes_verified, 23);
}

#[test]
fn the_resident_set_reads_the_segment_the_commit_wrote() {
    let path = temp_path();
    let (capsule, dem) = material(b"resident reads the commit", [0x5D; 64]);
    let mut line = create_line(&path, capsule, &dem);
    line.append_segments(&[b"the quick brown fox"])
        .expect("one segment");

    let spine = Spine::from_slabs([Slab::new(SegmentId::new(0), 0, 19)]);
    let span = spine.spans_in(0, spine.len()).pop().expect("one span");
    let mut text = resident(&line, 0, 32);
    text.insert(10, b"very ").expect("fits");
    let mut sink = LineSink { line: &mut line };
    let committed = commit_slab(&spine, span, &text, &mut sink).expect("commits");
    let after = committed.spine().clone();

    // A resident set pointed at the same file must fault in exactly the committed bytes for the
    // segment the new root names. This is the seam the Eye will use.
    let reopened = WorldLine::open(&path.0, &dem).expect("reopens");
    let source = LineSource { line: &reopened };
    let mut set = ResidentSet::new(source, 1 << 20, 64).expect("a usable budget");
    let labelled = after.locate(0).expect("not empty").slab().segment();
    assert_eq!(after.len(), 24);
    assert_eq!(
        set.fault_in(labelled).expect("faults in"),
        b"the quick very brown fox"
    );
}

#[test]
fn a_second_commit_of_the_same_span_is_refused_before_the_append() {
    let path = temp_path();
    let (capsule, dem) = material(b"stale span", [0x5E; 64]);
    let mut line = create_line(&path, capsule, &dem);
    line.append_segments(&[b"abc"]).expect("one segment");

    let spine = Spine::from_slabs([Slab::new(SegmentId::new(0), 0, 3)]);
    let span = spine.spans_in(0, spine.len()).pop().expect("one span");
    let mut text = resident(&line, 0, 8);
    text.insert(3, b"d").expect("fits");

    let mut sink = LineSink { line: &mut line };
    let committed = commit_slab(&spine, span, &text, &mut sink).expect("commits");
    let after = committed.spine().clone();
    assert_eq!(line.segment_count(), 2);

    // Committing the same span again would seal an identical segment and orphan the first. The
    // fence runs before the append, so the world-line does not grow by even a byte.
    let refused = {
        let mut sink = LineSink { line: &mut line };
        commit_slab(&after, span, &text, &mut sink)
    };
    assert!(matches!(
        refused,
        Err(CommitError::StaleSpan { offset: 0, .. })
    ));
    assert_eq!(line.segment_count(), 2, "a refusal must not append");
}

#[test]
fn an_emptied_slab_leaves_the_spine_and_appends_nothing() {
    let path = temp_path();
    let (capsule, dem) = material(b"emptied slab", [0x5F; 64]);
    let mut line = create_line(&path, capsule, &dem);
    line.append_segments(&[b"gone", b"kept"])
        .expect("two segments");

    let first = Slab::new(SegmentId::new(0), 0, 4);
    let second = Slab::new(SegmentId::new(1), 0, 4);
    let spine = Spine::from_slabs([first, second]);
    let span = spine.spans_in(0, spine.len()).remove(0);
    let mut text = resident(&line, 0, 8);
    text.delete(0, 4).expect("covers the slab");

    let mut sink = LineSink { line: &mut line };
    let committed = commit_slab(&spine, span, &text, &mut sink).expect("commits");
    assert!(matches!(committed, Committed::Removed { .. }));
    assert_eq!(committed.spine().len(), 4);
    assert_eq!(committed.spine().slab_count(), 1);
    assert_eq!(line.segment_count(), 2, "an empty slab seals nothing");
    assert_eq!(line.witness_count(), 2, "and witnesses nothing");

    // The surviving leaf is untouched and still names its own segment.
    let survivor = committed.spine().spans_in(0, 4);
    assert_eq!(survivor.len(), 1);
    assert_eq!(survivor[0].slab().segment(), SegmentId::new(1));
    let kept = line.open_segment(1).expect("segment 1").to_vec();
    assert_eq!(kept, b"kept");
}

#[test]
fn undo_after_a_commit_returns_to_a_root_the_world_line_still_serves() {
    let path = temp_path();
    let (capsule, dem) = material(b"undo after commit", [0x60; 64]);
    let mut line = create_line(&path, capsule, &dem);
    line.append_segments(&[b"first"]).expect("one segment");

    let spine = Spine::from_slabs([Slab::new(SegmentId::new(0), 0, 5)]);
    let mut history = History::new(spine.clone());
    let span = spine.spans_in(0, spine.len()).pop().expect("one span");
    let mut text = resident(&line, 0, 8);
    text.insert(5, b"!").expect("fits");
    let mut sink = LineSink { line: &mut line };
    let committed = commit_slab(&spine, span, &text, &mut sink).expect("commits");
    history.record(committed.spine().clone());
    assert_eq!(history.current().len(), 6);

    // Undo is a pointer swap back to a root whose segment was never rewritten.
    let undone = history.undo().expect("a state to undo to");
    assert_eq!(undone.len(), 5);
    assert_eq!(undone.slab_count(), 1);
    assert_eq!(
        undone.locate(0).expect("in range").slab().segment(),
        SegmentId::new(0)
    );
    assert_eq!(history.redo().map(Spine::len), Some(6));

    let original = line.open_segment(0).expect("segment 0").to_vec();
    assert_eq!(original, b"first", "the sealed segment was never rewritten");
}

#[test]
fn the_document_view_is_correct_before_the_commit_and_identical_after() {
    let path = temp_path();
    let (capsule, dem) = material(b"overlay", [0x61; 64]);
    let mut line = create_line(&path, capsule, &dem);
    line.append_segments(&[b"alpha", b"beta"]).expect("two segments");

    let first = Slab::new(SegmentId::new(0), 0, 5);
    let second = Slab::new(SegmentId::new(1), 0, 4);
    let spine = Spine::from_slabs([first, second]);
    let span = spine.spans_in(0, spine.len()).remove(0);
    let mut text = resident(&line, 0, 16);
    text.insert(5, b"!").expect("fits");

    // Before the commit the spine is stale by design, and the overlay is what makes the document
    // view correct: ten live bytes, the drafted leaf six long, everything after it three later.
    let mut draft = Draft::with_capacity(2);
    draft.record(span, 6).expect("records");
    assert_eq!(draft.document_len(&spine), 10);
    assert_eq!(spine.len(), 9, "the spine has not moved");
    let before: Vec<(u64, u64)> = draft
        .spans_in(&spine, 0, 10)
        .iter()
        .map(|live| (live.offset(), live.len()))
        .collect();
    assert_eq!(before, vec![(0, 6), (6, 4)]);
    let typed = draft.locate(&spine, 5).expect("in range");
    assert!(typed.span().is_draft());
    assert_eq!(typed.within(), 5);
    assert_eq!(typed.segment_offset(), None, "uncommitted bytes have no segment");

    // Commit, then drop the draft. The document view must not move by a single byte.
    let mut sink = LineSink { line: &mut line };
    let committed = commit_slab(&spine, span, &text, &mut sink).expect("commits");
    let after = committed.spine().clone();
    assert!(draft.forget(0));
    assert_eq!(draft.document_len(&after), 10);
    let now: Vec<(u64, u64)> = draft
        .spans_in(&after, 0, 10)
        .iter()
        .map(|live| (live.offset(), live.len()))
        .collect();
    assert_eq!(now, before, "the view is the same before and after the seal");

    // The same caret position now names committed geometry in the segment the commit wrote.
    let sealed = draft.locate(&after, 5).expect("in range");
    assert!(!sealed.span().is_draft());
    assert_eq!(sealed.within(), 5);
    assert_eq!(sealed.span().slab().segment(), SegmentId::new(2));
    assert_eq!(sealed.segment_offset(), Some(5));
}
