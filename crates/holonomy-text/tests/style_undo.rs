//! The styling side-channel that goes with undo: `style_undo`.
//!
//! # The bug this file is about
//!
//! [`UndoStack`] is *total*: every delete pushes an action, so `undo` can always find what it
//! needs. `style_undo` was **not**. [`Editor::delete_at`] recorded the styling a delete removed
//! only when there *was* styling to record, on the theory that a plain delete has nothing to
//! remember. But [`Editor::undo`] popped it unconditionally on any Delete-kind action.
//!
//! The two halves disagreed, and the disagreement is not a crash -- it is a document that comes
//! back from undo with the wrong words bold:
//!
//! ```text
//!   "ABCD" bold, delete [0,4)   ->  style_undo: [ [0,4) bold ]   (pushed)
//!   plain,      delete [0,1)   ->  style_undo: unchanged        (not pushed)
//!   undo        ->  pops [0,4) bold, and restores it over the wrong region
//! ```
//!
//! The second undo then finds an empty `style_undo` and silently loses the styling that was there
//! all along. Both errors are silent, and neither is reachable by testing deletes one at a time --
//! which is how it survived Phase 6's gate.
//!
//! The fix is in [`Editor::delete_at`] and [`Editor::redo`]: `style_undo` is now *total*, one entry
//! per Delete action, with an empty `Vec` standing for "this delete removed nothing styled". The
//! empty `Vec` costs nothing -- `Vec::new()` does not allocate -- and it makes push and pop
//! pairable one-to-one, which is the property that was actually missing.
//!
//! Totality buys a second thing, which is a fix of its own: an empty record is now *informative*.
//! It means the deleted bytes were plain. [`Editor::undo`] uses that to restore them plain instead of
//! taking [`SpanPolicy::GrowIntoInsert`]'s guess from the neighbouring run, so undoing a plain delete
//! that sat against a bold run no longer hands back bold text.
//!
//! [`UndoStack`]: holonomy_text::UndoStack
//! [`Editor::delete_at`]: holonomy_text::Editor::delete_at
//! [`Editor::undo`]: holonomy_text::Editor::undo
//! [`Editor::redo`]: holonomy_text::Editor::redo
//! [`SpanPolicy::GrowIntoInsert`]: holonomy_text::SpanPolicy::GrowIntoInsert

use holonomy_text::{ActionKind, Editor, EditorError, SpanPolicy, STYLE_BOLD, UNDO_DEPTH};

/// The `style_at` flags at `offset`, as `u16`.
fn flags(ed: &Editor, offset: u32) -> u16 {
    ed.style_at(offset).style_flags
}

/// The document as a `String`, for readable assertions.
fn text(ed: &Editor) -> String {
    String::from_utf8(ed.text().expect("valid utf-8")).expect("valid utf-8")
}

/// A plain delete must not steal a *previous* delete's styling on undo.
#[test]
fn a_plain_delete_does_not_restore_an_earlier_deletess_styling() {
    let mut ed = Editor::from_text(b"ABCDEFGH").expect("load");
    ed.style_range(0, 4, STYLE_BOLD, 0).expect("bold ABCD");

    // Removes styled content -> the old code pushes a record here.
    ed.delete_at(0, 4).expect("delete ABCD");
    assert_eq!(text(&ed), "EFGH");

    // Removes plain content -> the old code pushed nothing.
    let out = ed.delete_at(0, 1).expect("delete E");
    assert_eq!(out.kind, ActionKind::Delete);
    assert_eq!(text(&ed), "FGH");

    // Undo the plain delete. It re-inserts "E", which was *never* bold.
    ed.undo().expect("undo the E delete");
    assert_eq!(text(&ed), "EFGH");

    assert_eq!(
        flags(&ed, 0),
        0,
        "the restored 'E' inherited bold from the earlier delete's styling record"
    );
}

/// A plain delete whose bytes sit directly against a bold run must come back plain, not bold.
///
/// The distinct case from the one above: here there is no stale record to blame, only
/// `GrowIntoInsert` taking the neighbouring run's style for an empty one.
#[test]
fn a_plain_delete_against_a_bold_run_restores_plain() {
    let mut ed = Editor::from_text(b"ABCDwxyz").expect("load");
    ed.style_range(0, 4, STYLE_BOLD, 0).expect("bold ABCD");

    // Offset 4 is the first plain byte, and its preceding run is bold.
    ed.delete_at(4, 1).expect("delete w");
    assert_eq!(text(&ed), "ABCDxyz");

    ed.undo().expect("undo");
    assert_eq!(text(&ed), "ABCDwxyz");
    assert_eq!(
        flags(&ed, 0),
        STYLE_BOLD,
        "ABCD must not have lost its bold"
    );
    assert_eq!(
        flags(&ed, 4),
        0,
        "the restored 'w' took the preceding bold run via GrowIntoInsert"
    );
}

/// Undoing a *styled* delete after a plain one must still find its styling.
#[test]
fn a_styled_delete_after_a_plain_one_still_gets_its_styling_back() {
    let mut ed = Editor::from_text(b"ABCDEFGH").expect("load");
    ed.style_range(0, 4, STYLE_BOLD, 0).expect("bold ABCD");

    // Plain delete first.
    ed.delete_at(4, 4).expect("delete EFGH");
    assert_eq!(text(&ed), "ABCD");

    // Now a styled delete.
    ed.delete_at(0, 4).expect("delete ABCD");
    assert_eq!(text(&ed), "");

    // Undo the styled delete: ABCD comes back bold.
    ed.undo().expect("undo the ABCD delete");
    assert_eq!(text(&ed), "ABCD");
    assert_eq!(
        flags(&ed, 0),
        STYLE_BOLD,
        "undoing the styled delete lost the bold, because the plain delete's \
         non-record had shifted the pairing"
    );

    // Undo the plain delete too. EFGH was never styled, so it must come back plain -- even though it
    // lands directly after a bold run.
    ed.undo().expect("undo the EFGH delete");
    assert_eq!(text(&ed), "ABCDEFGH");
    assert_eq!(flags(&ed, 0), STYLE_BOLD);
    assert_eq!(
        flags(&ed, 4),
        0,
        "EFGH was plain before the delete and must be plain after"
    );
}

/// Interleaving styled and plain deletes, checked at every step.
#[test]
fn interleaved_deletes_keep_their_own_styling() {
    let mut ed = Editor::from_text(b"aabbccdd").expect("load");
    ed.style_range(0, 8, STYLE_BOLD, 0).expect("bold the lot");
    ed.style_range(4, 8, 0, 0).expect("plain ccdd");
    assert_eq!(flags(&ed, 0), STYLE_BOLD);
    assert_eq!(flags(&ed, 4), 0);

    // Delete a plain byte, then a bold one, then plain again.
    for (offset, len) in [(4usize, 1usize), (0, 4), (2, 1)] {
        ed.delete_at(offset as u32, len as u32).expect("delete");
    }
    assert_eq!(text(&ed), "cd");

    // Unwind all three, checking the styling as it goes.
    ed.undo().expect("undo the plain");
    assert_eq!(text(&ed), "cdd");
    assert_eq!(flags(&ed, 0), 0);

    ed.undo().expect("undo the bold");
    assert_eq!(text(&ed), "aabbcdd");
    assert_eq!(flags(&ed, 0), STYLE_BOLD);
    assert_eq!(flags(&ed, 4), 0);

    ed.undo().expect("undo the plain again");
    assert_eq!(text(&ed), "aabbccdd");
    assert_eq!(flags(&ed, 0), STYLE_BOLD);
    assert_eq!(flags(&ed, 3), STYLE_BOLD);
    assert_eq!(flags(&ed, 4), 0);
    assert_eq!(flags(&ed, 7), 0);
}

/// A redo re-pushes a delete, so it must push a style record too -- into the *same* vector, because
/// `undo` cannot tell the two kinds of Delete action apart.
#[test]
fn redo_of_a_delete_does_not_disturb_the_undo_pairing() {
    let mut ed = Editor::from_text(b"ABCDwxyz").expect("load");
    ed.style_range(0, 4, STYLE_BOLD, 0).expect("bold ABCD");

    ed.delete_at(0, 4).expect("delete ABCD"); // styled
    ed.delete_at(0, 1).expect("delete w"); // plain
    assert_eq!(text(&ed), "xyz");

    ed.undo().expect("undo w");
    assert_eq!(text(&ed), "wxyz");
    ed.undo().expect("undo ABCD");
    assert_eq!(text(&ed), "ABCDwxyz");
    assert_eq!(flags(&ed, 0), STYLE_BOLD);

    // Redo the ABCD delete: it must record its styling again.
    let out = ed.redo().expect("redo ABCD");
    assert_eq!(out.kind, ActionKind::Delete);
    assert_eq!(text(&ed), "wxyz");
    assert_eq!(
        flags(&ed, 0),
        STYLE_BOLD,
        "ABCD came back bold from the redo"
    );

    // Undoing the redo puts ABCD back bold -- this is the pairing that a separate redo-side record
    // vector would have broken, since `undo` pops one `style_undo` for a Delete it cannot attribute.
    ed.undo().expect("undo the redo");
    assert_eq!(text(&ed), "ABCDwxyz");
    assert_eq!(flags(&ed, 0), STYLE_BOLD);

    // The "delete w" action was consumed by the first undo and its redo was consumed by the redo, so
    // there is nothing left to undo. `redo_replaces_the_redo_branch` is the fuller statement of this.
    assert_eq!(ed.undo_depth(), 0);
    assert!(matches!(ed.undo(), Err(EditorError::NothingToUndo)));
}

/// Redo replaces the redo branch: after a redo, undoing again must reach the *redo*, not the action
/// the redo consumed.
#[test]
fn redo_replaces_the_redo_branch() {
    let mut ed = Editor::from_text(b"abcd").expect("load");
    ed.delete_at(0, 2).expect("delete ab");
    ed.delete_at(0, 1).expect("delete c");
    assert_eq!(text(&ed), "d");

    ed.undo().expect("undo c");
    ed.undo().expect("undo ab");
    assert_eq!(text(&ed), "abcd");
    assert_eq!(ed.undo_depth(), 0);
    assert_eq!(ed.redo_depth(), 2);

    ed.redo().expect("redo ab");
    assert_eq!(text(&ed), "cd");
    assert_eq!(ed.undo_depth(), 1);
    assert_eq!(
        ed.redo_depth(),
        1,
        "one redo branch is gone: the 'delete c' redo was consumed, not replaced"
    );

    ed.undo().expect("undo the redo");
    assert_eq!(text(&ed), "abcd");
    assert!(matches!(ed.undo(), Err(EditorError::NothingToUndo)));
}

/// An insert does not touch `style_undo`, so it must not shift the Delete pairing either.
#[test]
fn an_insert_between_two_deletes_does_not_shift_the_pairing() {
    let mut ed = Editor::from_text(b"ABCD").expect("load");
    ed.style_range(0, 4, STYLE_BOLD, 0).expect("bold ABCD");

    // At the *end*, so `Strict` gives the dash nothing to inherit and it is plain.
    ed.insert_at(4, b"-", SpanPolicy::Strict)
        .expect("insert dash");
    assert_eq!(flags(&ed, 4), 0);

    ed.delete_at(0, 4).expect("delete ABCD"); // styled
    ed.delete_at(0, 1).expect("delete dash"); // plain
    assert_eq!(text(&ed), "");

    ed.undo().expect("undo the dash delete");
    assert_eq!(text(&ed), "-");
    assert_eq!(flags(&ed, 0), 0, "the dash was never bold");
    ed.undo().expect("undo the ABCD delete");
    assert_eq!(text(&ed), "ABCD-");
    assert_eq!(flags(&ed, 0), STYLE_BOLD);
    assert_eq!(flags(&ed, 4), 0);
}

/// Eviction must not break the pairing: `style_undo` is bounded by count while `undo` is bounded by
/// count *and* arena pressure, so the two evict at different times. Totality is what makes the
/// mismatch harmless.
#[test]
fn the_style_record_stays_paired_past_its_bound() {
    // Big enough that the deletes are spread over two leaves.
    let doc = vec![b'a'; 2000];
    let mut ed = Editor::from_text(&doc).expect("load");
    ed.style_range(0, 1000, STYLE_BOLD, 0)
        .expect("bold the first half");

    // 505 plain deletes out of the *second* half, so `style_undo` wraps more than once and every
    // record evicted is empty.
    for _ in 0..UNDO_DEPTH + 5 {
        ed.delete_at(1000, 1).expect("delete a plain byte");
    }
    assert_eq!(text(&ed).len(), 2000 - (UNDO_DEPTH + 5));

    // One more plain delete, sitting against the bold run, then undo it.
    ed.delete_at(1000, 1).expect("delete a plain byte");
    ed.undo().expect("undo");
    assert_eq!(
        flags(&ed, 1000),
        0,
        "after {} evictions the restored byte picked up a stale or guessed bold",
        UNDO_DEPTH + 5
    );

    // Now delete the bold half and confirm its styling still comes back.
    ed.delete_at(0, 1000).expect("delete the bold half");
    ed.undo().expect("undo the bold delete");
    assert_eq!(
        flags(&ed, 0),
        STYLE_BOLD,
        "the bold record survived eviction"
    );
}

/// `clear_history` must drop the redo side too, or a fresh document could replay a dead action.
#[test]
fn clear_history_drops_redo_as_well() {
    let mut ed = Editor::from_text(b"abcd").expect("load");
    ed.delete_at(0, 4).expect("delete");
    assert_eq!(ed.undo_depth(), 1);
    assert_eq!(ed.redo_depth(), 0);

    ed.undo().expect("undo");
    assert_eq!(ed.undo_depth(), 0);
    assert_eq!(ed.redo_depth(), 1);
    assert!(ed.can_redo());

    ed.clear_history();
    assert_eq!(ed.undo_depth(), 0);
    assert_eq!(ed.redo_depth(), 0);
    assert!(!ed.can_redo());
    assert!(matches!(ed.redo(), Err(EditorError::NothingToRedo)));
}

/// A fresh edit invalidates the redo branch -- the branch every word processor takes.
#[test]
fn a_fresh_edit_drops_the_redo_branch() {
    let mut ed = Editor::from_text(b"abcd").expect("load");
    ed.delete_at(0, 4).expect("delete");
    ed.undo().expect("undo");
    assert_eq!(ed.redo_depth(), 1);

    ed.insert_at(4, b"e", SpanPolicy::Strict).expect("type 'e'");
    assert_eq!(
        ed.redo_depth(),
        0,
        "typing after an undo must discard the redo, or redo would splice at a stale caret"
    );
    assert!(!ed.can_redo());
}

/// And a refused edit must leave the history alone, redo branch included.
#[test]
fn a_refused_edit_leaves_the_redo_branch_intact() {
    let mut ed = Editor::from_text(b"abcd").expect("load");
    ed.delete_at(0, 4).expect("delete");
    ed.undo().expect("undo");
    assert_eq!(ed.redo_depth(), 1);

    // Past the end of the document: refused before any mutation.
    assert!(ed.insert_at(9, b"x", SpanPolicy::Strict).is_err());
    assert_eq!(
        ed.redo_depth(),
        1,
        "a refused insert must not drop the redo branch"
    );
    assert_eq!(text(&ed), "abcd");

    // Mid-character, likewise.
    ed.insert_at(0, "\u{00e9}".as_bytes(), SpanPolicy::Strict)
        .expect("insert a two-byte char");
    assert!(matches!(
        ed.delete_at(1, 1),
        Err(EditorError::NotCharBoundary { offset: 1 })
    ));
}
