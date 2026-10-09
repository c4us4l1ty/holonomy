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

#[allow(unused_imports)]
use holonomy_text::edit_record::SavedRun;
use holonomy_text::edit_record::{Edit, EditRecord, ReplayError};

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
    assert_eq!(
        rec.net_delta() as i64,
        current.len() as i64 - saved_len as i64
    );

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
    check(
        &[
            Edit::delete(0, b"gone from the very start"),
            Edit::delete(5, b"and a bit more"),
        ],
        200,
    );

    // Mixed, with a replacement -- the one case where inserted and removed are both non-empty.
    check(
        &[
            Edit::replace(40, b"old text", b"new"),
            Edit::insert(60, b"++"),
            Edit::delete(80, b"xy"),
        ],
        200,
    );

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
    assert_eq!(
        rec.to_saved(want_at),
        Some(28),
        "current {want_at} is saved 28"
    );

    // **Current offset 7, asked of the record and of the model, so neither is a remembered number.**
    let m = model(saved.len(), &edits, current.len());
    assert_eq!(rec.to_saved(7), m[7], "current 7, record and model agree");
    // And it is *not* 25 -- which is what part 9's forward fold returned for this input.
    assert_ne!(
        rec.to_saved(7),
        Some(25),
        "the forward fold's answer, which is why part 9 removed it"
    );

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
    assert_eq!(
        rec.to_saved(5),
        None,
        "a typed byte has no saved counterpart"
    );
    assert_eq!(rec.to_saved(4), Some(4), "and the one before it is itself");
    // **Offset 50 is not inside the first edit's run** -- after the insert at 5 shifts everything up by
    // four, that run lands at current [54, 62). So current 50 is the byte before it, which was saved 46.
    // A walk that stopped at the first entry past the read would have answered 50 and been wrong by four.
    assert_eq!(
        rec.to_saved(50),
        Some(46),
        "four bytes of insertion earlier slid this one along"
    );
    assert_eq!(
        rec.to_saved(54),
        None,
        "and current 54 *is* the first inserted byte of the earlier edit"
    );
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
        assert_eq!(
            rec.to_saved(q),
            None,
            "offset {q} is inside the inserted run"
        );
    }
    assert_eq!(
        rec.to_saved(43),
        Some(40),
        "and past it, saved 40 is what is at 43"
    );
    assert_eq!(rec.to_saved(39), Some(39), "before it, unchanged");
}

/// **Symmetrically, a deleted byte has no current offset** — the other direction's `None`.
#[test]
fn a_deleted_byte_has_no_current_offset() {
    let mut rec = EditRecord::new();
    rec.push(Edit::delete(20, b"0123456789"));
    for sq in 20..30 {
        assert_eq!(
            rec.to_current(sq),
            None,
            "saved {sq} was deleted, so nothing is there now"
        );
    }
    assert_eq!(
        rec.to_current(30),
        Some(20),
        "and past the deletion the offset moved down by ten"
    );
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
    assert_eq!(
        left, 1,
        "both of the first two are wholly before 54, so one survivor"
    );
    assert_eq!(rec.edits()[0].at, 90, "and it is the one at 90");

    // The survivors' translation is still right for what they cover.
    // Recomputed against what is left: only the insert at 90 remains, so its run is current [90, 94).
    assert_eq!(rec.to_saved(90), None, "inside the surviving insert's run");
    assert_eq!(rec.to_saved(93), None, "and so is the last byte of it");
    // Past the run, the offset steps back by the four inserted bytes.
    assert_eq!(
        rec.to_saved(94),
        Some(90),
        "past the run, four fewer in the saved document"
    );
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
    assert_eq!(
        &e.removed[..],
        removed,
        "the bytes undo needs to put back are here"
    );
    assert_eq!(
        &e.inserted[..],
        inserted,
        "and the bytes undo needs to take away"
    );
    // Net length change is derived from the bytes, not carried separately -- so it cannot disagree with them.
    assert_eq!(rec.net_delta(), 2, "19 in, 17 out");
    rec.push(Edit::insert(0, b"x"));
    assert_eq!(
        rec.net_delta(),
        (inserted.len() - removed.len() + 1) as isize
    );
}
// ---------------------------------------------------------------------------
// Part 11, attempt three: content replay with **no accumulator at all**.
//
// Parts 9 and 11 each failed because a running `delta` was applied
// unconditionally across edits. An edit at offset 50 does not shift a
// position at 10, so any accumulator that does not test position is wrong the
// moment two edits are out of order -- which in a text editor is the normal
// case. This implementation has no accumulator: every byte of the window is
// resolved by asking a question about *that byte*, using the two primitives
// part 10 already gated exhaustively.
// ---------------------------------------------------------------------------

/// The source, as a `fetch` closure over the saved document.
fn source(saved: &[u8]) -> impl FnMut(usize, usize) -> Vec<u8> + '_ {
    move |at, n| {
        let want = n.min(saved.len().saturating_sub(at));
        saved[at..at + want].to_vec()
    }
}

/// Record the scripts and build a record from them.
fn record(edits: &[Edit]) -> EditRecord {
    let mut r = EditRecord::new();
    for e in edits {
        r.push(e.clone());
    }
    r
}

/// **The load-bearing test**: every window of every length, over four scripts, against the real document.
///
/// Part 10's lesson applies twice over -- a fold can be exhaustively right and *still* wrong where two edits
/// overlap -- so this checks **every `(at, len)` pair**, not a sample. And a typed byte is included rather
/// than skipped, because skipping it is what would have hidden the last two bugs.
#[test]
fn replay_reproduces_the_document_at_every_window() {
    let scripts: Vec<Vec<Edit>> = vec![
        vec![
            Edit::insert(50, b"hello"),
            Edit::insert(10, b"XX"),
            Edit::delete(30, b"gone!"),
        ],
        vec![
            Edit::delete(0, b"from the start"),
            Edit::insert(5, b"and back"),
        ],
        vec![
            Edit::replace(40, b"old text here", b"new"),
            Edit::insert(70, b"++"),
            Edit::delete(90, b"xy"),
        ],
        vec![
            Edit::insert(30, b"aaaaaaaaaaaaaaaa"),
            Edit::delete(0, b"zzzzzzzz"),
            Edit::insert(60, b"b"),
            Edit::delete(50, b"yyyyyyyyyyyyyyyyyyyy"),
            Edit::insert(10, b"cccccccccccccccc"),
            Edit::delete(90, b"d"),
        ],
        // **Out-of-offset-order by a long way**: an edit at 190 then one at 2. A global delta would place
        // the second one 3 bytes late, which is what part 11's attempt got wrong on a much smaller case.
        vec![
            Edit::insert(190, b"end"),
            Edit::insert(2, b"start!"),
            Edit::delete(100, b"mid"),
        ],
    ];

    for edits in scripts {
        let saved = saved_doc(200);
        let want = apply(&saved, &edits);
        let rec = record(&edits);

        for at in 0..want.len() {
            for len in [1usize, 2, 3, 16, 64, 257] {
                if at + len > want.len() {
                    continue;
                }
                match rec.replay(at, len, source(&saved)) {
                    Ok(got) => assert_eq!(
                        got,
                        want[at..at + len],
                        "replay({at}, {len}) -- {} edits, document length {}",
                        edits.len(),
                        want.len()
                    ),
                    // **`Unresolvable` is a refusal, and a refusal is correct here** -- see below. The gate
                    // asserts it is *exactly* the case the record cannot describe, so a refusal can never
                    // quietly become a way of avoiding the comparison.
                    Err(ReplayError::Unresolvable { at: bad }) => {
                        assert!(
                            rec.edits().iter().any(|e| {
                                !e.inserted.is_empty() && rec.to_current(e.at).is_none()
                            }),
                            "replay({at}, {len}) refused at {bad}, but every edit's insertion point is \
                             derivable -- so this refusal is unexplained and would be a hole in the record"
                        );
                    }
                    Err(other) => panic!("replay({at}, {len}) refused: {other:?}"),
                }
            }
        }
    }
}

/// **A typed byte costs no source call at all** — the rope already has it, and asking the source for it would
/// be asking a question with no answer. Counted, because "we did not call the source" is the claim and a
/// caller can only believe it if something counts.
#[test]
fn a_typed_byte_is_served_without_asking_the_source() {
    let saved = saved_doc(200);
    let edits = vec![Edit::insert(40, b"XYZ")];
    let rec = record(&edits);
    let want = apply(&saved, &edits);

    let mut calls = 0usize;
    let out = rec
        .replay(40, 3, |at, n| {
            calls += 1;
            let w = n.min(saved.len().saturating_sub(at));
            saved[at..at + w].to_vec()
        })
        .expect("the three typed bytes");
    assert_eq!(out, want[40..43], "and they are the typed bytes");
    assert_eq!(calls, 0, "with zero source calls");

    // A window that *starts* typed and runs into saved bytes needs exactly one call.
    let calls = std::cell::Cell::new(0usize);
    let out = rec
        .replay(40, 10, |at, n| {
            calls.set(calls.get() + 1);
            let w = n.min(saved.len().saturating_sub(at));
            saved[at..at + w].to_vec()
        })
        .expect("typed then saved");
    assert_eq!(out, want[40..50]);
    assert_eq!(
        calls.get(),
        1,
        "one run of saved bytes, fetched in a single call"
    );
}

/// **Replay and `to_saved` are the same fact reached two ways**, so the record has one coordinate system
/// rather than two that could drift apart.
#[test]
fn replay_agrees_with_to_saved_at_every_offset() {
    let saved = saved_doc(200);
    let edits = vec![
        Edit::insert(50, b"seven!!"),
        Edit::delete(10, b"abc"),
        Edit::delete(5, b"01234567890123456789"),
    ];
    let want = apply(&saved, &edits);
    let rec = record(&edits);
    for at in 0..want.len() {
        if let Some(from) = rec.to_saved(at) {
            let one = rec.replay(at, 1, source(&saved)).expect("one byte");
            assert_eq!(
                one[0],
                saved[from.min(saved.len() - 1)],
                "to_saved({at}) says {from}"
            );
        }
    }
}

/// **A short fetch is refused rather than padded** — zeros in a document are indistinguishable from real
/// text, which is the same reason `Rope::fault_leaf` refuses.
#[test]
fn replay_refuses_a_short_fetch() {
    // **No saved document.** The closure below ignores its arguments and returns nothing, which is the whole
    // point: a source that has no bytes at all must still be caught rather than believed.
    let mut rec = EditRecord::new();
    rec.push(Edit::insert(40, b"XYZ"));
    assert_eq!(
        rec.replay(0, 16, |_at, _n| Vec::new()),
        Err(ReplayError::ShortFetch { want: 16, got: 0 }),
        "a source that returns nothing must not be believed"
    );
}

/// **An insertion anchored inside a *deleted* region — which part 12 called undecidable and part 15
/// proved is not.**
///
/// This test used to assert `Unresolvable`. **It now asserts the record gets it right**, because the
/// backwards walk in `typed_byte` reaches the insertion's position without forming a run, and the old
/// positional scheme's inability to do that was a bug in the scheme rather than a limit of the record.
///
/// The script is `delete(0, 15 bytes)` then `insert(5, "and back")`: the insert's anchor lands inside the
/// region the delete removed, so `to_current(5)` is `None` and there is **no surviving byte to measure the
/// insertion against**. Part 12's own doc comment named the fix as a fourth primitive — *"the final offset
/// of the first saved byte at or after `e.at` that survives"*. **The backwards fold is that primitive**,
/// arrived at from the other direction: it does not need an anchor at all, because it accumulates `q`
/// through every edit rather than converting an anchor forward.
///
/// **The document is well defined and the record now describes it**, which is the difference. Kept as a test
/// because the case is still the interesting one — an insertion with no anchor — and a future
/// "optimisation" that reintroduces position arithmetic would fail here.
#[test]
fn an_insertion_anchored_inside_a_deleted_region_is_still_reproduced() {
    let saved = saved_doc(200);
    let edits = vec![
        Edit::delete(0, b"fifteen bytes!!"),
        Edit::insert(5, b"and back"),
    ];
    let rec = record(&edits);
    let want = apply(&saved, &edits);

    // **The anchor is still undeliverable.** This has not changed and is the point: the record cannot use
    // it, and must not pretend to.
    assert!(
        rec.to_current(5).is_none(),
        "offset 5 was inside the deleted run"
    );

    assert!(
        want.windows(8).any(|w| w == b"and back"),
        "and the typed bytes are in the document"
    );

    // **And the record reproduces the window containing them anyway**, by walking backwards.
    let at = want
        .iter()
        .position(|b| *b == b'a')
        .expect("the inserted run starts somewhere");
    let got = rec
        .replay(at, 8, source(&saved))
        .unwrap_or_else(|e| panic!("the record can describe this: {e:?}"));
    assert_eq!(
        got,
        want[at..at + 8],
        "the window across the unanchored insertion"
    );
}

/// **An insertion into previously *typed* text, which part 12 also called undecidable — and which is not.**
///
/// `insert(0, "abc")` then `insert(1, "X")`: the second insert lands inside the first one's inserted run, so
/// its anchor is a typed byte with no saved origin. Part 12's positional scheme refused this *and* the
/// deleted-region case above, so this one hid inside a limitation that was already known and documented.
///
/// **The backwards walk reproduces it**, because it never needs an anchor: it carries `q` back through every
/// edit rather than converting an anchor forward, so a fragmenting run is not a special case — it is the
/// only case.
///
/// **So `ReplayError::Unresolvable` is no longer reachable from a well-formed record.** That is a real
/// change and it is pinned two ways: this test asserts the document is reproduced, and
/// `to_saved_and_typed_byte_never_disagree` below asserts the two walks cannot come apart. **The error
/// variant stays**, because `Rope::fault_leaf` must still have an answer for a record that is *not* well
/// formed — an invariant this crate does not own — and an unreachable error path that is nonetheless
/// handled is cheaper than a `panic!`.
#[test]
fn an_insertion_anchored_inside_typed_text_is_reproduced() {
    let saved = saved_doc(200);
    let edits = vec![Edit::insert(0, b"abc"), Edit::insert(1, b"X")];
    let rec = record(&edits);
    let want = apply(&saved, &edits);

    // The anchors really are typed bytes -- which is the reason, not an accident of the arithmetic.
    assert_eq!(
        rec.to_saved(0),
        None,
        "offset 0 is inside the first insert's own run"
    );
    assert_eq!(
        &want[0..4],
        b"aXbc",
        "and the document is perfectly well defined"
    );

    let got = rec
        .replay(0, 4, source(&saved))
        .unwrap_or_else(|e| panic!("the record describes this: {e:?}"));
    assert_eq!(
        got,
        want[0..4],
        "the fragmenting window, reproduced exactly"
    );
}

// ---------------------------------------------------------------------------
// Part 14: `saved_runs` + `fill_typed` -- the fault path's view of a window.
// ---------------------------------------------------------------------------

/// **The two-step composition is `replay`, and the order is load-bearing.**
///
/// `replay` was rebuilt on these two, so this is close to a tautology. It is still gated directly, and
/// **the thing being gated is the order**: `Rope::fault_leaf` fetches each run into the destination and
/// *then* fills the typed bytes. Swapped, the typed fill would not clobber the fetched bytes here — but
/// only because `fill_typed` skips saved positions. That skip is a *contract*, and a contract nobody
/// checks is a contract that gets implemented differently somewhere else.
#[test]
fn saved_runs_then_fill_typed_reproduces_the_document() {
    let scripts: Vec<Vec<Edit>> = vec![
        vec![
            Edit::insert(50, b"hello"),
            Edit::insert(10, b"XX"),
            Edit::delete(30, b"gone!"),
        ],
        vec![
            Edit::delete(0, b"from the start"),
            Edit::insert(5, b"and back"),
        ],
        vec![
            Edit::replace(40, b"old text here", b"new"),
            Edit::insert(70, b"++"),
            Edit::delete(90, b"xy"),
        ],
        vec![
            Edit::insert(190, b"end"),
            Edit::insert(2, b"start!"),
            Edit::delete(100, b"mid"),
        ],
    ];
    for edits in scripts {
        let saved = saved_doc(200);
        let want = apply(&saved, &edits);
        let rec = record(&edits);
        for at in 0..want.len() {
            for len in [1usize, 3, 16, 64] {
                if at + len > want.len() {
                    continue;
                }
                // **A window entirely of typed bytes asks the source for nothing** -- which is the empty
                // `saved_runs` case, and the reason an empty answer has to be legal rather than a bug.
                let mut out = vec![0u8; len];
                let runs = rec.saved_runs(at, len);
                for r in &runs {
                    out[r.out_at..r.out_at + r.len]
                        .copy_from_slice(&saved[r.saved_at..r.saved_at + r.len]);
                }
                // **`Unresolvable` is refused, not answered** -- the case part 12 named: an insertion
                // anchored inside an earlier edit's deleted region has no derivable position. The refusal
                // is allowed to appear *only* in that shape, so it cannot become a quiet way of skipping
                // the comparison.
                // **`Unresolvable` must not fire at all**, and the assertion says why that is a change rather
                // than a weakening. Part 15 replaced `typed_byte`'s positional run lookup with the same
                // backwards walk `to_saved` uses, and the two now agree by construction — so a refusal here
                // means the walks have come apart, which is the bug this test exists to catch. The previous
                // version allowed a refusal whenever *some* edit's anchor was underivable, which is not the
                // same condition and let a real hole through: a refusal on a script whose anchors were all
                // fine would have been excused by an unrelated edit elsewhere in the document.
                rec.fill_typed(at, &mut out).unwrap_or_else(|e| {
                    panic!("fill_typed({at}, {len}) refused with {e:?} on {edits:?}")
                });
                // **A window of nothing but typed bytes asks the source zero times**, and it still
                // produces the right bytes. Asserted on the run list rather than on a counter,
                // because the run list is the actual thing the fault path iterates.
                assert_eq!(
                    runs.is_empty(),
                    (0..len).all(|i| rec.to_saved(at + i).is_none()),
                    "an empty run list must mean an entirely typed window, and nothing else"
                );
                assert_eq!(
                    out,
                    want[at..at + len],
                    "saved_runs+fill_typed({at}, {len})"
                );
            }
        }
    }
}

/// **The two walks must never disagree: `to_saved` says a byte is typed, `typed_byte` must locate it.**
///
/// **This is the gate for the bug part 15 found, and it is the one part 12's suite structurally could not
/// write.** `to_saved` and `typed_byte` answer "is this byte typed?" and "which edit typed it, and where?"
/// — the same backwards walk over the same record, differing only in what they keep. Part 12 implemented
/// them separately and gated each on its own terms, so each could be *individually* defensible while the
/// pair disagreed: `to_saved` said "typed" via the backwards walk while `typed_byte` looked for a
/// `(start, len)` run computed forwards, and on any document with two length-changing edits the run was in
/// the wrong place. Every one of part 12's scripts was checked against the *bytes*, which is why the pair
/// was never asked directly.
///
/// **Asserted as a property over every offset of every script, including ones designed to fragment runs** —
/// nested insertions, an insertion inside a deletion, an insertion inside an earlier insertion. Those last
/// two are the shapes no `(start, len)` can describe, and they are here because they are exactly what a
/// positional implementation gets wrong.
#[test]
fn to_saved_and_typed_byte_never_disagree() {
    let scripts: Vec<Vec<Edit>> = vec![
        // Ordinary: two independent edits.
        vec![Edit::insert(50, b"hello"), Edit::insert(10, b"XX")],
        // The part-13 fixture's shape, which is where the positional version first broke.
        vec![
            Edit::insert(10, b"ZZZZZ"),
            Edit::insert(81927, b"QQ"),
            Edit::insert(100000, b"tail"),
        ],
        // Fragmenting: each insertion lands inside the previous one's inserted text.
        vec![
            Edit::insert(0, b"abcdef"),
            Edit::insert(2, b"12"),
            Edit::insert(4, b"XY"),
        ],
        // An insertion anchored inside a deletion.
        vec![
            Edit::delete(0, b"fifteen bytes!!"),
            Edit::insert(5, b"and back"),
        ],
        // A deletion anchored inside an earlier insertion.
        vec![Edit::insert(0, b"abcdef"), Edit::delete(2, b"cd")],
        // Replacements of differing lengths, and an edit at the very end.
        vec![
            Edit::replace(40, b"old text here", b"new"),
            Edit::replace(3, b"xx", b"yyyyy"),
            Edit::insert(199, b"!"),
        ],
        // Deletes only, which move offsets the other way.
        vec![Edit::delete(100, b"ten bytes.."), Edit::delete(20, b"abc")],
    ];

    // **One saved length for every script, and it is large on purpose.** The scripts carry offsets up to
    // 100,000 (the part-13 fixture, reproduced because that is the shape the positional version broke on),
    // and `apply`/`model` *clamp* an offset past the end rather than refusing — so a small fixture would
    // silently turn those edits into appends and the model would disagree with the record about a
    // difference that is the fixture's, not the code's. A test that fails for the wrong reason teaches
    // nothing, and a clamp is the quietest way to arrange that.
    const SAVED_LEN: usize = 120_000;
    for edits in scripts {
        let saved_len = SAVED_LEN;
        let saved = saved_doc(saved_len);
        let want = apply(&saved, &edits);
        let rec = record(&edits);
        // **The model, not the record.** `model` tracks each current offset back to the saved byte it came
        // from by performing the splices, so it is an independent witness for *both* questions: it says
        // which bytes are typed, and it says which saved byte each one came from. Re-deriving provenance
        // here would be the third reading of the record, which is the thing this test exists to prevent.
        let m = model(saved_len, &edits, want.len());
        for q in 0..want.len() {
            let from_saved = rec.to_saved(q);
            assert_eq!(
                from_saved.is_none(),
                m[q].is_none(),
                "to_saved({q})={from_saved:?} but the splices say {:?} -- script {edits:?}",
                m[q]
            );
            match (from_saved, rec.typed_byte_for_test(q)) {
                (None, None) => {
                    panic!("to_saved({q}) says typed and typed_byte cannot locate it -- {edits:?}")
                }
                (Some(s), Some(_)) => {
                    panic!("to_saved({q}) = {s} says saved but typed_byte says typed -- {edits:?}")
                }
                (None, Some((i, idx))) => {
                    let e = &rec.edits()[i];
                    assert!(
                        idx < e.inserted.len(),
                        "index {idx} into a {}-byte insertion",
                        e.inserted.len()
                    );
                    assert_eq!(
                        e.inserted[idx], want[q],
                        "byte at {q} is not what edit {i} inserted there -- script {edits:?}"
                    );
                }
                (Some(s), None) => {
                    // **A saved offset equal to `saved_len` is legal** -- it is the position one past the
                    // last byte, which is what a document that grew at its end reports. It names no byte,
                    // so it must not be indexed.
                    if let Some(&byte) = saved.get(s) {
                        assert_eq!(
                            byte, want[q],
                            "to_saved({q}) = {s} names the wrong saved byte -- script {edits:?}"
                        );
                    }
                }
            }
        }
    }
}

/// **Runs are maximal, contiguous in *both* coordinate systems, and cover exactly the saved positions.**
///
/// Maximality is a real property and not a tidiness one: a source is asked one question per run, so a
/// window that fragmented into per-byte runs would make a leaf-sized fault 2,048 source calls instead of
/// one.
#[test]
fn saved_runs_are_maximal_and_contiguous_in_both_systems() {
    let saved = saved_doc(200);
    let rec = record(&[
        Edit::insert(50, b"hello"),
        Edit::insert(10, b"XX"),
        Edit::delete(30, b"gone!"),
    ]);
    let want = apply(
        &saved,
        &[
            Edit::insert(50, b"hello"),
            Edit::insert(10, b"XX"),
            Edit::delete(30, b"gone!"),
        ],
    );

    for at in [0usize, 5, 10, 12, 40, 55, 100] {
        for len in [8usize, 32, 64] {
            if at + len > want.len() {
                continue;
            }
            let runs = rec.saved_runs(at, len);
            // **Ascending and non-overlapping in the output**, which is the property the fault path's
            // loop depends on -- it writes each run into its own slice.
            for w in runs.windows(2) {
                assert!(
                    w[0].out_at + w[0].len <= w[1].out_at,
                    "runs overlap or descend in the output at {at}/{len}"
                );
            }
            // **Maximal: no two adjacent runs could have merged.** Both coordinates must line up for a
            // merge, and checking either alone would allow a run to span a typed byte -- which would
            // write the wrong byte and look plausible.
            for w in runs.windows(2) {
                assert!(
                    w[0].out_at + w[0].len != w[1].out_at
                        || w[0].saved_at + w[0].len != w[1].saved_at,
                    "two runs at {at}/{len} could have been one, so this is not maximal"
                );
            }
            // **A gap in the output between two runs is a typed byte**, and every position in it must
            // really be typed -- asserted, because an unaccounted gap would leave a zero in the document.
            for w in runs.windows(2) {
                for i in (w[0].out_at + w[0].len)..w[1].out_at {
                    assert!(
                        rec.to_saved(at + i).is_none(),
                        "gap position {i} at {at}/{len} claims to be saved, so a run failed to cover it"
                    );
                }
            }
            // Coverage: exactly the positions `to_saved` says are saved.
            let covered: Vec<usize> = runs
                .iter()
                .flat_map(|r| (r.out_at..r.out_at + r.len))
                .collect();
            let expected: Vec<usize> = (0..len)
                .filter(|&i| rec.to_saved(at + i).is_some())
                .collect();
            assert_eq!(covered, expected, "run coverage at {at}/{len}");
            // Ascending.
            assert!(
                runs.windows(2).all(|w| w[0].out_at < w[1].out_at),
                "runs must ascend"
            );
        }
    }
}
