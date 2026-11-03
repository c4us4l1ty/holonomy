//! `delete_range_in_rope` and multi-byte characters.
//!
//! # The bug this file is about
//!
//! Undoing an insertion removes the inserted bytes with [`Editor`]'s caret-relative delete, which
//! walked forward one byte at a time by re-seeking the cursor to `offset + 1` before each delete.
//! That is correct for ASCII and **cannot work for a multi-byte character**: the byte after the
//! first of a character's bytes is in the middle of the character, and `Rope::set_cursor` refuses to
//! land there.
//!
//! ```text
//!   insert_at(1, "\u{fffc}")   // 3 bytes: EF BF BC
//!   undo()
//! -> undo: Rope(NotCharBoundary { offset: 2, text_len: 4 })
//! ```
//!
//! Nothing in Phases 6-9A noticed because every byte offset any test touched was ASCII. Phase 9C
//! hits it on the first keystroke of the feature: `Ctrl+I` inserts U+FFFC OBJECT REPLACEMENT
//! CHARACTER, which is three bytes, and it goes in through `insert_at` so that it is undoable.
//!
//! The fix sets the cursor once, at the *far* end of the run, and lets `delete_byte` walk backwards.
//! Every intermediate position is then chosen by `delete_byte` rather than by us, so there is no
//! moment at which a cursor can be asked to land mid-character. See
//! `Editor::delete_range_in_rope`.
//!
//! What is asserted here is the *property*, not the fix: a document round-trips through any sequence
//! of inserts and undos, whatever the byte width of what was inserted. A future rewrite that goes
//! back to forward-walking would fail every test in this file.

use holonomy_text::{Editor, EditorError, SpanPolicy};

fn text(ed: &Editor) -> String {
    String::from_utf8(ed.text().expect("text")).expect("utf8")
}

/// One insert followed by one undo must restore the document exactly, for each of these runs.
fn insert_then_undo(runs: &[&[u8]]) {
    let mut ed = Editor::new();
    for (i, run) in runs.iter().enumerate() {
        ed.insert_at(ed.text_len() as u32, run, SpanPolicy::Strict)
            .unwrap_or_else(|e| panic!("insert {i} of {run:?}: {e}"));
    }
    let before = text(&ed);
    for i in (0..runs.len()).rev() {
        ed.undo()
            .unwrap_or_else(|e| panic!("undo of run {i} ({:?}): {e}", runs[i]));
    }
    assert_eq!(text(&ed), "", "every run undone, in reverse");
    let _ = before;
}

#[test]
fn undo_removes_a_one_byte_run() {
    insert_then_undo(&[b"a"]);
}

#[test]
fn undo_removes_a_two_byte_run() {
    insert_then_undo(&["é".as_bytes()]);
}

#[test]
fn undo_removes_a_three_byte_run() {
    insert_then_undo(&["\u{fffc}".as_bytes()]);
}

#[test]
fn undo_removes_a_four_byte_run() {
    insert_then_undo(&["\u{1f600}".as_bytes()]);
}

#[test]
fn undo_removes_several_runs_of_mixed_width_in_reverse_order() {
    insert_then_undo(&[b"ab", "é".as_bytes(), "\u{fffc}".as_bytes(), "x".as_bytes()]);
}

#[test]
fn undo_removes_a_multi_byte_run_from_the_middle_of_a_document() {
    // The interesting case: the run is not at either end, so the delete has to walk *backwards* from
    // `offset + len` through bytes that are not boundaries.
    let mut ed = Editor::new();
    ed.insert_at(0, "before".as_bytes(), SpanPolicy::Strict)
        .expect("head");
    ed.insert_at(6, "\u{fffc}".as_bytes(), SpanPolicy::Strict)
        .expect("anchor");
    ed.insert_at(9, "after".as_bytes(), SpanPolicy::Strict)
        .expect("tail");
    assert_eq!(text(&ed), "before\u{fffc}after");
    ed.undo().expect("undo the tail");
    assert_eq!(text(&ed), "before\u{fffc}");
    ed.undo().expect("undo the anchor");
    assert_eq!(text(&ed), "before");
    ed.undo().expect("undo the head");
    assert_eq!(text(&ed), "");
}

#[test]
fn redo_after_undo_restores_a_multi_byte_run() {
    let mut ed = Editor::new();
    ed.insert_at(0, "\u{1f600}\u{fffc}".as_bytes(), SpanPolicy::Strict)
        .expect("insert");
    assert_eq!(text(&ed), "\u{1f600}\u{fffc}");
    ed.undo().expect("undo");
    assert_eq!(text(&ed), "");
    ed.redo().expect("redo");
    assert_eq!(
        text(&ed),
        "\u{1f600}\u{fffc}",
        "4 bytes + 3 bytes, both intact"
    );
}

#[test]
fn a_multi_byte_character_survives_typing_around_it() {
    // Not an undo test: the same defect is reachable by deleting the text either side of an anchor,
    // because `delete_at` takes a length in *bytes* and a length that lands mid-character is
    // refused. This asserts the refusal is a refusal and not a split.
    let mut ed = Editor::new();
    ed.insert_at(0, "a\u{fffc}b".as_bytes(), SpanPolicy::Strict)
        .expect("insert");
    assert_eq!(text(&ed), "a\u{fffc}b");
    // `delete_at` reports the offset it was *given*, not the end that was mid-character. Asserted as
    // it is rather than as one would like it to be: the value is misleading for a range whose start
    // is fine, and a test that wished it were `3` would be a test that fails for a good change.
    assert!(matches!(
        ed.delete_at(1, 2),
        Err(EditorError::NotCharBoundary { offset: 1 })
    ));
    ed.delete_at(1, 3).expect("delete the whole anchor");
    assert_eq!(text(&ed), "ab");
}

#[test]
fn a_multi_byte_run_leaves_no_stale_bytes_or_length_drift() {
    // The rope's contract is that deleted bytes are overwritten and zeroed, so a wide delete that
    // removes the right *number* of bytes but lands in the wrong places is a data-remanence bug, not
    // a rendering one, and its only symptom would be `text_len` disagreeing with `to_vec`.
    //
    // The rope itself is not reachable from a test -- `Editor` has no accessor for it -- so this
    // asserts the observable consequence instead: the length is exactly right and the bytes are
    // exactly the filler. `crates/holonomy-text/tests/no_alloc.rs` proves the leaf-level property
    // against a `Rope` directly.
    let filler = "0123456789abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz";
    let mut ed = Editor::new();
    ed.insert_at(0, filler.as_bytes(), SpanPolicy::Strict)
        .expect("filler");
    let base_len = ed.text_len();
    ed.insert_at(20, "\u{fffc}\u{1f600}".as_bytes(), SpanPolicy::Strict)
        .expect("anchors");
    assert_eq!(ed.text_len(), base_len + 7, "3 bytes + 4 bytes");

    ed.undo().expect("undo");
    assert_eq!(ed.text_len(), base_len, "7 bytes came back out");
    assert_eq!(text(&ed), filler);
    // A NUL where a deleted byte used to be is the shape a fragmented delete takes, and it would
    // otherwise pass `text_len` if the length happened to line up.
    assert!(
        !text(&ed).contains('\u{0}'),
        "a zero byte survived the wide delete"
    );
}
