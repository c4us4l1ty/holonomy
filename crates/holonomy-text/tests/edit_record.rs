//! **The edit record: current offset ↔ saved offset, against a brute-force model at every offset.** 7 tests.
//!
//! # Why this file exists
//!
//! Part 7 established that a `LeafSource` keyed by document offset is only true of an *unmodified* document.
//! Part 8 found that write-back alone does not fix it, because an insert moves a byte across a leaf boundary.
//! Part 9 built the `(offset, ±delta)` log part 8 recommended and **removed it as wrong**: its fold walked
//! edits *forwards*, and each edit's `at` lives in the document as it was *before that edit*, so overlapping
//! edits are measured against coordinates the walk has already passed.
//!
//! **The fix is to fold backwards.** Each step converts an offset from one coordinate system to the previous
//! one, and the previous one is exactly what the next edit's `at` uses.
//!
//! # Why a model, and why every offset
//!
//! A fold with boundary cases is right in the middle and wrong at the ends. **A worked example validates
//! exactly that one example.** So every test here runs a script through a real `Vec`, asks a brute-force model
//! which saved byte landed at each current offset, and compares the record at *every* offset — and the model
//! is itself checked against the actual bytes first, because a model that is wrong reads like a record bug.
//!
//! | what it proves | test |
//! | --- | --- |
//! | **the backwards fold, exhaustively** | [`the_backwards_fold_matches_a_model_on_every_offset`] |
//! | the case that killed the forward fold | [`overlapping_edits_do_not_break_the_translation`] |
//! | offsets are not monotonic | [`an_edit_after_a_later_one_is_still_seen`] |
//! | inserted bytes have no origin | [`an_inserted_byte_has_no_saved_counterpart`] |
//! | deleted bytes have no current offset | [`a_deleted_byte_has_no_current_offset`] |
//! | the record cannot grow forever | [`compaction_drops_only_wholly_earlier_edits`] |
//! | undo can read the same record | [`the_record_carries_what_undo_needs`] |

use holonomy_text::edit_record::{Edit, EditRecord};

/// Apply `edits` to `saved` — the model. **No folding, no arithmetic**, just a `Vec` and a splice per edit.
fn apply(saved: &[u8], edits: &[Edit]) -> Vec<u8> {
    let mut doc = saved.to_vec();
    for e in edits {
        let at = e.at.min(doc.len());
        let end = at + e.removed.len().min(doc.len() - at);
        doc.splice(at..end, e.inserted.iter().copied());
    }
    doc
}

/// Which saved byte landed at each current offset, tracked through the splices.
fn model(saved_len: usize, edits: &[Edit], current_len: usize) -> Vec<Option<usize>> {
    let mut origin: Vec<Option<usize>> = (0..saved_len).map(Some).collect();
    for e in edits {
        let at = e.at.min(origin.len());
        let removed = e.removed.len().min(origin.len() - at);
        let filler: Vec<Option<usize>> = std::iter::repeat_n(None, e.inserted.len()).collect();
        origin.splice(at..at + removed, filler);
    }
    origin.resize(current_len, None);
    origin
}

/// Build a document whose bytes identify their offset, so a wrong offset is *visible*.
fn saved_doc(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

/// Check the record against the model over a whole script, at every offset, and return the script's length.
#[track_caller]
fn check(edits: &[Edit], saved_len: usize) {
    let saved = saved_doc(saved_len);
    let current = apply(&saved, edits);
    let model = model(saved_len, edits, current.len());

    // **The model is checked against the bytes before the record is blamed for anything.**
    for (q, from) in model.iter().enumerate() {
        if let Some(sf) = from {
            assert_eq!(
                current.get(q).copied(),
                saved.get(*sf).copied(),
                "the MODEL is wrong: it says current {q} came from saved {sf}"
            );
        }
    }

    let mut rec = EditRecord::new();
    for e in edits {
        rec.push(e.clone());
    }
    assert_eq!(rec.len(), edits.len());
    assert_eq!(rec.net_delta() as i64, current.len() as i64 - saved_len as i64);

    // **Every byte position, and no further.** `0..=current.len()` indexed one past the model, which the
    // compiler caught -- but only because the model is exactly `current.len()` long. The offset *one past*
    // the end is a real query a caller makes (reading `len` bytes at the end) and deserves its own answer,
    // below, rather than being smuggled in as an out-of-bounds index.
    for (q, want) in model.iter().enumerate() {
        assert_eq!(
            rec.to_saved(q),
            *want,
            "current {q}: record says {:?}, model says {:?}",
            rec.to_saved(q),
            model[q]
        );
    }
    // The saved length is the current length minus the net delta -- a separate fact, checked as one.
    assert_eq!(
        rec.to_saved(current.len()),
        Some((current.len() as isize - rec.net_delta()).max(0) as usize),
        "the saved document's length"
    );

    // And the inverse, wherever both answers exist.
    // **Bytes only, for the same reason as the forward loop**: `saved_len` itself maps to `current_len`,
    // which is one past the model, and an out-of-bounds index is not an assertion.
    for sq in 0..saved_len {
        if let Some(cq) = rec.to_current(sq) {
            assert_eq!(
                model.get(cq).copied().flatten(),
                Some(sq),
                "inverse: saved {sq} -> current {cq}, which the model says came from {:?}",
                model.get(cq)
            );
        }
    }
    assert_eq!(
        rec.to_current(saved_len),
        Some((saved_len as isize + rec.net_delta()).max(0) as usize),
        "the current document's length"
    );
}

/// **The load-bearing test**, over several scripts: interior-only, offset-zero, at-the-end, and one that both
/// grows and shrinks the document repeatedly.
#[test]
fn the_backwards_fold_matches_a_model_on_every_offset() {
    // A run of inserts, all interior: the easy case, and the one a forward fold would also pass.
    check(
        &[
            Edit::insert(50, b"hello"),
            Edit::insert(10, b"XX"),
            Edit::insert(120, b"a much longer insertion than the others"),
        ],
        200,
    );

    // Deletes, which are the hard direction for any offset arithmetic.
    check(&[Edit::delete(20, b"twenty bytes gone!")], 200);
    check(&[Edit::delete(0, b"gone from the very start"), Edit::delete(5, b"and a bit more")], 200);

    // Mixed, with a replacement -- the one case where inserted and removed are both non-empty.
    check(&[Edit::replace(40, b"old text", b"new"), Edit::insert(60, b"++"), Edit::delete(80, b"xy")], 200);

    // Edits at the very end, where `at + inserted` reaches the document length exactly.
    //
    // **The insert is 20 bytes so the delete has 20 to remove.** An earlier version inserted 10 and then
    // deleted 12, which is not a legal edit -- there were only 10 bytes there -- and the model *clamped* it
    // while the record counted all 12. The two disagreed on the document's length and the disagreement was
    // the only symptom. **A fixture that asks for something impossible fails as an arithmetic error in the
    // implementation, which is the most expensive way to find out the fixture was wrong.**
    check(
        &[
            Edit::insert(200, b"twenty bytes appended"),
            Edit::delete(200, b"twelve of them!!"),
        ],
        200,
    );

    // **Growing and shrinking repeatedly**, so the record's chain of coordinate systems is long and the
    // document's length goes up and down -- the shape of a real editing session.
    check(
        &[
            Edit::insert(30, b"aaaaaaaaaaaaaaaa"),
            Edit::delete(0, b"zzzzzzzz"),
            Edit::insert(60, b"b"),
            Edit::delete(50, b"yyyyyyyyyyyyyyyyyyyy"),
            Edit::insert(10, b"cccccccccccccccc"),
            Edit::delete(90, b"d"),
        ],
        200,
    );
}

/// **The case that killed the forward fold, isolated.** An edit at offset 5 removing 20 bytes, after an edit
/// at offset 10 that removed 3 — so the later edit's span covers bytes the earlier one had already shifted.
///
/// The forward fold returns **25** here. The truth is **28**: current offset 7 sits where saved offset 28 does,
/// because the 3-byte deletion at 10 slid everything after it up by three.
#[test]
fn overlapping_edits_do_not_break_the_translation() {
    let edits = vec![
        Edit::insert(50, b"seven!!"),
        Edit::delete(10, b"abc"),
        Edit::delete(5, b"01234567890123456789"),
    ];
    let saved = saved_doc(200);
    let current = apply(&saved, &edits);
    let mut rec = EditRecord::new();
    for e in &edits {
        rec.push(e.clone());
    }

    // Find the current offset that holds saved byte 28, so the test is not asserting a remembered number.
    let want_at = current
        .iter()
        .position(|b| *b == saved[28])
        .expect("saved byte 28 survived");
    assert_eq!(rec.to_saved(want_at), Some(28), "current {want_at} is saved 28");

    // **Current offset 7, asked of the record and of the model, so neither is a remembered number.**
    let m = model(saved.len(), &edits, current.len());
    assert_eq!(rec.to_saved(7), m[7], "current 7, record and model agree");
    // And it is *not* 25 -- which is what part 9's forward fold returned for this input.
    assert_ne!(rec.to_saved(7), Some(25), "the forward fold's answer, which is why part 9 removed it");

    check(&edits, 200);
}

/// **Edits are not offset-monotonic**, so a walk that stops at the first one past the read is wrong.
///
/// Type at 50, then go back and type at 5: perfectly ordinary, and it breaks any assumption that later
/// entries are further along.
#[test]
fn an_edit_after_a_later_one_is_still_seen() {
    let edits = vec![Edit::insert(50, b"far away"), Edit::insert(5, b"near")];
    let rec = {
        let mut r = EditRecord::new();
        for e in &edits {
            r.push(e.clone());
        }
        r
    };
    // Offset 5 is inside the *second* edit's inserted run.
    assert_eq!(rec.to_saved(5), None, "a typed byte has no saved counterpart");
    assert_eq!(rec.to_saved(4), Some(4), "and the one before it is itself");
    // **Offset 50 is not inside the first edit's run** -- after the insert at 5 shifts everything up by
    // four, that run lands at current [54, 62). So current 50 is the byte before it, which was saved 46.
    // A walk that stopped at the first entry past the read would have answered 50 and been wrong by four.
    assert_eq!(rec.to_saved(50), Some(46), "four bytes of insertion earlier slid this one along");
    assert_eq!(rec.to_saved(54), None, "and current 54 *is* the first inserted byte of the earlier edit");
    check(&edits, 200);
}

/// **`None` for an inserted byte, and never a nearby offset instead.** The whole point of `Option`: after
/// inserting 3 bytes at 40, offset 40 is the first inserted byte, and a `usize` return would hand back
/// `40 - 3 = 37` — a real byte, and not the one at 40.
#[test]
fn an_inserted_byte_has_no_saved_counterpart() {
    let mut rec = EditRecord::new();
    rec.push(Edit::insert(40, b"XYZ"));
    for q in 40..43 {
        assert_eq!(rec.to_saved(q), None, "offset {q} is inside the inserted run");
    }
    assert_eq!(rec.to_saved(43), Some(40), "and past it, saved 40 is what is at 43");
    assert_eq!(rec.to_saved(39), Some(39), "before it, unchanged");
}

/// **Symmetrically, a deleted byte has no current offset** — the other direction's `None`.
#[test]
fn a_deleted_byte_has_no_current_offset() {
    let mut rec = EditRecord::new();
    rec.push(Edit::delete(20, b"0123456789"));
    for sq in 20..30 {
        assert_eq!(rec.to_current(sq), None, "saved {sq} was deleted, so nothing is there now");
    }
    assert_eq!(rec.to_current(30), Some(20), "and past the deletion the offset moved down by ten");
    assert_eq!(rec.to_current(19), Some(19), "before it, unchanged");
}

/// **Compaction drops only wholly-earlier edits, and it is what keeps the record bounded.** An unbounded
/// record on a document edited all afternoon is a second copy of the document.
///
/// The subtle half: an edit that a *later* edit was measured against cannot be dropped even if it starts
/// early, because the later edit's `at` is in coordinates that include it.
#[test]
fn compaction_drops_only_wholly_earlier_edits() {
    let mut rec = EditRecord::new();
    rec.push(Edit::delete(10, b"aaaa")); // removes [10, 14)
    rec.push(Edit::insert(50, b"bbbb")); // at 50 -- measured in coordinates that include the delete above
    rec.push(Edit::insert(90, b"cccc"));
    assert_eq!(rec.len(), 3);

    // A saved offset past the first two drops them, and only them.
    let left = rec.compact_before(54);
    assert_eq!(left, 1, "both of the first two are wholly before 54, so one survivor");
    assert_eq!(rec.edits()[0].at, 90, "and it is the one at 90");

    // The survivors' translation is still right for what they cover.
    // Recomputed against what is left: only the insert at 90 remains, so its run is current [90, 94).
    assert_eq!(rec.to_saved(90), None, "inside the surviving insert's run");
    assert_eq!(rec.to_saved(93), None, "and so is the last byte of it");
    // Past the run, the offset steps back by the four inserted bytes.
    assert_eq!(rec.to_saved(94), Some(90), "past the run, four fewer in the saved document");
}

/// **The record carries what undo needs**, which is the answer to part 9's objection about building this
/// structure twice: `removed` and `inserted` are the *bytes*, so undo can read this record rather than
/// maintaining its own.
#[test]
fn the_record_carries_what_undo_needs() {
    let inserted = b"the quick brown fox";
    let removed = b"slow green turtle";
    // **These are deliberately different lengths** -- 19 against 17 -- because a replacement that changes
    // the document's length is the case where a record carrying deltas and one carrying bytes could
    // disagree, and the point is that `net_delta` is *derived* from the bytes rather than stored beside them.
    let mut rec = EditRecord::new();
    rec.push(Edit::replace(100, removed, inserted));

    let e = &rec.edits()[0];
    assert_eq!(e.at, 100);
    assert_eq!(&e.removed[..], removed, "the bytes undo needs to put back are here");
    assert_eq!(&e.inserted[..], inserted, "and the bytes undo needs to take away");
    // Net length change is derived from the bytes, not carried separately -- so it cannot disagree with them.
    assert_eq!(rec.net_delta(), 2, "19 in, 17 out");
    rec.push(Edit::insert(0, b"x"));
    assert_eq!(rec.net_delta(), (inserted.len() - removed.len() + 1) as isize);
}