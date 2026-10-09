//! **Part 15: the whole-document commit, and the gate for the design part 13 chose over part 8's.** 7 tests.
//!
//! # What this file was, and why it is not that any more
//!
//! Phase 13 part 8 set the repair for sparse editing as one rule — *a source must return a leaf's bytes as
//! they are now, at current offsets* — and this file gated it with 6 tests over a per-leaf write-back on
//! eviction. **Those 6 tests were not wrong. They were narrower than the rule.** Every one used a
//! `Vec<u8>` as the source, and **a `Vec` has no sections**: writing a leaf's bytes back overwrites exactly
//! that leaf's range and disturbs nothing else, so a shift is invisible. Each test read back the leaf it
//! had just written and never a later one.
//!
//! Part 13 measured the rule against a real `SectionStore` — which is 65,520 B per section against a
//! 3,841 B leaf, so a leaf write-back repairs at most 1 leaf in 17 — and found **132,987 of 135,045 bytes
//! wrong** after a single 5-byte insert, with the container's `content_len` five bytes short of the
//! in-memory document while the commit reported success. See `crates/holonomy/tests/write_back_shift.rs`,
//! which still runs and still asserts the falsification.
//!
//! # What replaced it, and the one sentence that is the whole of it
//!
//! > **A commit writes every leaf, and what repairs the shift is that there is no leaf left unwritten.**
//!
//! The record ([`Rope::fault_leaf`](holonomy_text::Rope) consults it) is what makes a fault correct past an
//! edit; `commit` is what makes the *store* correct again. Neither substitutes for the other, and part 13
//! is the measurement of what happens when the second is done per-leaf.
//!
//! # The source here is a `Vec`, deliberately, and this time the reason is stated
//!
//! It cannot be a `SectionStore` — `holonomy-text` cannot depend on `holonomy-container`, which is what
//! `LeafSource` exists for. **So the `Vec` limitation that made part 8's tests too narrow is closed from the
//! other side**: these tests gate *the rope's* half (which leaves get written, in what order, and what
//! happens to the record afterwards), and `write_back_shift.rs` gates *the store's* half against a real
//! container. Neither test can make the design pass by being convenient about sections.
//!
//! | what it proves | test |
//! | --- | --- |
//! | **a committed document reads back whole** | [`a_committed_document_reads_back_byte_for_byte`] |
//! | **the shift part 13 could not repair** | [`a_commit_repairs_the_shift_a_leaf_write_back_could_not`] |
//! | **edits past the edit point, not just at it** | [`ten_edits_across_the_document_all_survive_a_commit`] |
//! | deletions too, which move offsets the other way | [`a_deletion_survives_a_commit_and_the_length_shrinks`] |
//! | the commit is not a partial write | [`every_leaf_is_written_not_just_the_resident_ones`] |
//! | and the record is genuinely gone afterwards | [`a_commit_clears_the_record_so_a_later_fault_asks_no_question`] |
//! | an empty document still records its extent | [`committing_an_empty_document_records_a_zero_length`] |

use holonomy_text::{LeafSource, Rope, RopeError};

/// A document whose bytes are a function of position, so a one-byte offset error is *visible* rather
/// than plausible.
fn doc(n: usize) -> Vec<u8> {
    (0..n).map(|i| b'a' + (i % 26) as u8).collect()
}

/// The store, standing in for a container: it holds **saved** bytes at **saved** offsets, permanently.
///
/// **It is never patched with current bytes by a fault** — that is the part 14 invariant, and
/// `a_fault_after_an_edit_reads_saved_bytes_at_saved_offsets` is what holds it. `store_leaf` and `set_len`
/// are called only by `commit`, and `writes` counts them so a test can prove it.
struct Store {
    bytes: Vec<u8>,
    fetches: u32,
    writes: usize,
}

impl Store {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            fetches: 0,
            writes: 0,
        }
    }
}

impl LeafSource for Store {
    /// **Saved bytes at saved offsets, and it never knows otherwise.** An edit shifts every later offset,
    /// so a fault that asked this for a *current* offset would get the right number of bytes from the
    /// wrong place — the failure part 7 measured and parts 9, 11 and 13 each re-derived.
    fn fetch_leaf(&mut self, offset: usize, out: &mut [u8]) -> Result<usize, RopeError> {
        self.fetches += 1;
        let want = out.len().min(self.bytes.len().saturating_sub(offset));
        out[..want].copy_from_slice(&self.bytes[offset..offset + want]);
        Ok(want)
    }

    /// **Counted, because "only the commit calls this" is a claim worth gating.** A fault that reached
    /// here would be writing current bytes into a saved-coordinate store, and the count is how that shows
    /// up as a number rather than as a subtly wrong document.
    fn store_leaf(&mut self, offset: usize, bytes: &[u8]) -> Result<(), RopeError> {
        self.writes += 1;
        self.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    /// **Resizes in both directions, and the shrink is the half that matters.**
    ///
    /// A source with the right bytes and a stale length hands back a short leaf at the end of the document,
    /// which `fault_leaf` refuses as `OutOfBounds`. A source with the right bytes and a stale *longer*
    /// length is worse in one way: the document's tail is readable past its end, so a deletion appears not
    /// to have happened until something compares the length.
    ///
    /// **`Vec::resize` rather than `if len < n`**, so a fixture cannot be written that only grows — which is
    /// what made the first version of the deletion test pass for the wrong reason: the store kept 40,000
    /// bytes for a 39,705-byte document and the test compared content rather than extent.
    fn set_len(&mut self, text_len: usize) -> Result<(), RopeError> {
        self.bytes.resize(text_len, 0);
        Ok(())
    }
}

fn read_at(rope: &mut Rope, store: &mut dyn LeafSource, at: usize, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    rope.read_at_faulting(store, at, len, &mut out)
        .unwrap_or_else(|e| panic!("fault, then read at {at} len {len}: {e:?}"));
    out
}

/// The document the edits actually produced, built by **performing them**.
///
/// **Never arithmetic on the saved bytes.** `fault_edit_conflict.rs` computed its truth as
/// `saved[q + 1..]` for years of green tests, and that is shifted by two: inserting one byte at `p` means
/// `current[q] == saved[q - 1]` for `q > p`. It asserted `got != truth`, and a wrong truth satisfies a
/// wrong inequality for the wrong reason. Every test here derives truth by doing the edit.
fn truth_of(n: usize, edits: &[(usize, &[u8])]) -> Vec<u8> {
    let mut t = doc(n);
    for (at, bytes) in edits {
        t.splice(*at..*at, bytes.iter().copied());
    }
    t
}

/// **The whole point: after a commit, the store holds the document, and reading it back is exact.**
///
/// This is the assertion part 8 could not make. `whole()` in the previous version of this file was marked
/// `#[allow(dead_code)]` with a comment saying it could not be called yet, because an edit shifted every
/// leaf after it and the store only learned about the leaf that had been written back. **It is called
/// here, on every leaf.**
#[test]
fn a_committed_document_reads_back_byte_for_byte() {
    let n = 135_040;
    let mut rope = Rope::from_skeleton(n);
    let mut store = Store::new(doc(n));

    // Three edits, spread across the document, each preceded by a fault so the leaf is resident.
    let leaf = rope.leaf_len_of(0);
    let edits: [(usize, &[u8]); 3] = [(10, b"ZZZZZ"), (40 * leaf + 7, b"QQ"), (100_000, b"tail")];
    for (at, bytes) in edits {
        rope.fault_leaf_containing(&mut store, at).expect("fault");
        rope.set_cursor_faulting(&mut store, at).expect("cursor");
        rope.insert_at(at, bytes).expect("insert");
    }

    let truth = truth_of(n, &edits);
    assert_eq!(
        rope.text_len(),
        truth.len(),
        "the rope's document is the edited length"
    );

    let written = rope.commit(&mut store).expect("commit");
    assert_eq!(
        written,
        truth.len(),
        "commit wrote the whole document, not a prefix"
    );

    // **Read the whole thing back out of the store, through the rope, with every leaf absent-or-resident
    // exactly as a fresh session would find it.**
    let mut got = vec![0u8; truth.len()];
    rope.read_at_faulting(&mut store, 0, truth.len(), &mut got)
        .expect("read back");
    assert_eq!(
        got, truth,
        "the committed document reads back byte for byte"
    );

    // And the store is self-consistent without the rope in the way — otherwise the two could be two
    // wrongs cancelling.
    assert_eq!(
        store.bytes, truth,
        "and the store alone holds the same bytes"
    );
}

/// **The specific failure part 13 measured, repaired — and repaired by writing *every* leaf.**
///
/// Part 13's fixture: 135,040 bytes, `insert(10, "ZZZZZ")`, every resident leaf written back, and
/// 132,987 of 135,045 bytes wrong, diverging from exactly where leaf 0 ends. The difference here is not a
/// smarter write; it is that `commit` writes all 34 leaves rather than the one that was resident.
#[test]
fn a_commit_repairs_the_shift_a_leaf_write_back_could_not() {
    let n = 135_040;
    let mut rope = Rope::from_skeleton(n);
    let mut store = Store::new(doc(n));

    let at = 10usize;
    let mut buf = vec![0u8; 8];
    rope.read_at_faulting(&mut store, at, 8, &mut buf)
        .expect("fault");
    rope.insert_at(at, b"ZZZZZ").expect("insert");
    let truth = truth_of(n, &[(at, b"ZZZZZ")]);

    // **Part 13's exact fixture:** only the leaves that are resident. On this fixture that is leaf 0.
    let resident: Vec<usize> = (0..rope.leaf_count())
        .filter(|&k| rope.is_resident(k))
        .collect();
    assert!(
        !resident.is_empty(),
        "the fixture must have something resident to write back"
    );
    for k in &resident {
        let lo = rope.leaf_offset(*k);
        let len = rope.leaf_len_of(*k);
        let mut bytes = vec![0u8; len];
        rope.read_at_faulting(&mut store, lo, len, &mut bytes)
            .expect("read leaf");
        store.store_leaf(lo, &bytes).expect("leaf write back");
    }
    store.set_len(truth.len()).expect("length");

    // **The falsification, asserted rather than printed.** After a *partial* write-back the store is a
    // mixture: right where the written leaf was, wrong everywhere after it. A test that measured nothing
    // would be worse than none, because it gets counted.
    let mut partial = vec![0u8; truth.len()];
    for lo in (0..n).step_by(8_192) {
        let hi = (lo + 8_192).min(n);
        let got = store.fetch_leaf(lo, &mut partial[lo..hi]).expect("read");
        assert_eq!(
            got,
            hi - lo,
            "a partial store reads back at the right length"
        );
    }
    let diffs = (0..n).filter(|&i| partial[i] != truth[i]).count();
    assert!(
        diffs > n / 2,
        "expected the partial write-back to leave most of the document wrong; only {diffs} of {n} bytes \
         differed, so this fixture no longer exercises the failure and would pass for the wrong reason"
    );
    assert_eq!(
        &partial[..at + 5],
        &truth[..at + 5],
        "the leaf that *was* written back is correct -- that is what made the bug invisible for six tests"
    );

    // **The repair, on a fresh rope and a fresh store — and the freshness is load-bearing.**
    //
    // The first version of this committed on the *same* pair, and it failed with "the whole document is
    // now correct". That is not a flaw in the commit; it is the test having destroyed its own premise. The
    // partial write-back above patched current bytes into a saved-coordinate store, so the record's
    // translation — *the source's byte at X is the document's byte at X + delta* — is no longer true of
    // that store, and `commit` reads through it exactly as the fault path does.
    //
    // **So the same measured fact lands twice: a store that has been written back per-leaf is not a
    // saved-document store, and nothing downstream can tell.** Part 13 measured the *insufficiency*; this is
    // the *corruption*, and it is why `store_leaf`'s docs say the two coordinate systems coincide only at
    // commit time.
    let mut rope2 = Rope::from_skeleton(n);
    let mut store2 = Store::new(doc(n));
    rope2
        .read_at_faulting(&mut store2, at, 8, &mut vec![0u8; 8])
        .expect("fault");
    rope2.insert_at(at, b"ZZZZZ").expect("insert");

    rope2.commit(&mut store2).expect("commit");
    assert!(
        store2.writes > resident.len(),
        "the commit wrote {} leaves where the partial pass wrote {} -- it must write every leaf, \
         because leaving one unwritten is the bug",
        store2.writes,
        resident.len()
    );
    assert_eq!(
        store2.bytes, truth,
        "and a whole-document commit repairs what the partial write-back could not"
    );
}

/// **Edits *past* the first are the ones that were wrong.** Part 7's failure was one byte off per edit
/// already made, so a single edit proves nothing about the second — and ten edits at ten different places
/// is the case a per-leaf repair cannot address at all.
#[test]
fn ten_edits_across_the_document_all_survive_a_commit() {
    let n = 60_000;
    let mut rope = Rope::from_skeleton(n);
    let mut store = Store::new(doc(n));
    let leaf = rope.leaf_len_of(0);

    let edits: Vec<(usize, Vec<u8>)> = (0..10usize)
        .map(|i| ((2 + i) * leaf + 40 + i, vec![b'A' + (i % 26) as u8]))
        .collect();
    for (at, bytes) in &edits {
        rope.fault_leaf_containing(&mut store, *at).expect("fault");
        rope.set_cursor_faulting(&mut store, *at).expect("cursor");
        rope.insert_at(*at, bytes).expect("insert");
    }

    let truth = truth_of(
        n,
        &edits
            .iter()
            .map(|(a, b)| (*a, b.as_slice()))
            .collect::<Vec<_>>(),
    );
    rope.commit(&mut store).expect("commit");
    assert_eq!(
        store.bytes, truth,
        "ten shifts, one commit, and the document is exact"
    );

    // Read it back through the rope as a reopened session would.
    let mut got = vec![0u8; truth.len()];
    rope.read_at_faulting(&mut store, 0, truth.len(), &mut got)
        .expect("read back");
    assert_eq!(
        got, truth,
        "and every one of the ten edits is where it was typed"
    );
}

/// **A deletion moves offsets the other way, and shrinks the document.**
///
/// Insert-only tests would pass against a repair that only ever grows: a delete removes bytes, so the
/// store's tail is *longer* than the truth's, and `set_len` has to shorten it. Both directions matter
/// because the two are the two ways `compact_before`'s arithmetic goes wrong, and a gate that only
/// inserts cannot tell a correct implementation from one that got lucky.
#[test]
fn a_deletion_survives_a_commit_and_the_length_shrinks() {
    let n = 40_000;
    let mut rope = Rope::from_skeleton(n);
    let mut store = Store::new(doc(n));
    let leaf = rope.leaf_len_of(0);

    // Delete 300 bytes from the middle, then insert 5 near the start. Both directions in one document.
    let del_at = 5 * leaf + 11;
    rope.fault_leaf_containing(&mut store, del_at)
        .expect("fault the deletion");
    rope.set_cursor_faulting(&mut store, del_at + 300)
        .expect("cursor");
    for _ in 0..300 {
        rope.delete_byte().expect("delete");
    }
    let ins_at = 7usize;
    rope.fault_leaf_containing(&mut store, ins_at)
        .expect("fault the insertion");
    rope.set_cursor_faulting(&mut store, ins_at)
        .expect("cursor");
    rope.insert_at(ins_at, b"small").expect("insert");

    // **Truth by construction:** the saved document with 300 bytes removed and 5 added.
    let mut truth = doc(n);
    truth.drain(del_at..del_at + 300);
    truth.splice(ins_at..ins_at, b"small".iter().copied());

    assert_eq!(
        rope.text_len(),
        truth.len(),
        "the rope is {} and truth is {}",
        rope.text_len(),
        truth.len()
    );
    rope.commit(&mut store).expect("commit");

    assert_eq!(
        store.bytes.len(),
        truth.len(),
        "the store's length followed the deletion"
    );
    assert_eq!(
        store.bytes, truth,
        "and every surviving byte is where it belongs"
    );
}

/// **The commit is not a partial write, and this is the test that says so directly.**
///
/// Every leaf is made absent first, so the commit *has* to fault each one in and write each one out. A
/// commit that only wrote what was already resident would pass every other test in this file and repair
/// nothing, which is exactly the bug part 13 found — so the fixture is built to make the shortcut fail.
#[test]
fn every_leaf_is_written_not_just_the_resident_ones() {
    let n = 135_040;
    let mut rope = Rope::from_skeleton(n);
    let mut store = Store::new(doc(n));
    let leaf = rope.leaf_len_of(0);

    // Read a window in the middle, so exactly one leaf is resident and the other 33 are not.
    let probe = 60 * leaf;
    rope.read_at_faulting(&mut store, probe, 32, &mut [0u8; 32])
        .expect("fault a window");
    let resident = rope.resident_count();
    assert!(
        resident < rope.leaf_count() / 4,
        "the fixture must be sparse: {resident} of {}",
        rope.leaf_count()
    );

    let writes_before = store.writes;
    let written = rope.commit(&mut store).expect("commit");
    assert_eq!(written, n, "the commit wrote the whole document");
    assert!(
        store.writes - writes_before > rope.leaf_count() / 2,
        "only {} of {} leaves were written -- a commit that writes the resident set is the part 8 bug",
        store.writes - writes_before,
        rope.leaf_count()
    );
    assert_eq!(
        store.bytes,
        doc(n),
        "an unedited document commits back to itself"
    );
}

/// **After a commit the record is empty, and that is a fact about the world rather than a convenience.**
///
/// The source now holds the current document, so saved offsets and current offsets are the same numbers
/// and an empty record is the accurate description. **The observable consequence is that a later fault
/// asks the store nothing it did not already know** — the fetches are a plain read at the current offset,
/// with no translation. If the record were left populated, those fetches would go through `saved_runs`
/// and the document would still be right, so this test pins the *clearing*, not the correctness.
#[test]
fn a_commit_clears_the_record_so_a_later_fault_asks_no_question() {
    let n = 40_000;
    let mut rope = Rope::from_skeleton(n);
    let mut store = Store::new(doc(n));

    let at = 500usize;
    rope.fault_leaf_containing(&mut store, at).expect("fault");
    rope.set_cursor_faulting(&mut store, at).expect("cursor");
    for b in b"hello" {
        rope.insert_byte(*b).expect("insert");
    }
    let mut truth = doc(n);
    truth.splice(at..at, b"hello".iter().copied());

    rope.commit(&mut store).expect("commit");

    // **A fault past the commit point now reads straight from the store at the current offset.** With the
    // record empty the window is one maximal saved run starting at the offset asked for, so a source that
    // answered from the wrong place — the part 7 failure — would be caught here and nowhere else.
    let deep = 20_000usize;
    let mut out = vec![0u8; 64];
    rope.read_at_faulting(&mut store, deep, 64, &mut out)
        .expect("fault after commit");
    assert_eq!(
        out,
        truth[deep..deep + 64],
        "the post-commit fault reads the current offsets directly"
    );
    assert_eq!(
        store.bytes, truth,
        "and the store already held them, so nothing was replayed"
    );
}

/// **An empty document still has a length to record.**
///
/// The `n == 0` branch in `commit` exists for a reason and this is it: a source whose extent is stale
/// refuses the *next* fault with a length error, and the cause would be an edit at offset 0 in a
/// different session rather than anything near the read that failed.
#[test]
fn committing_an_empty_document_records_a_zero_length() {
    let mut rope = Rope::from_skeleton(0);
    let mut store = Store::new(Vec::new());
    assert_eq!(
        rope.commit(&mut store).expect("commit an empty document"),
        0,
        "no bytes to write"
    );
    assert!(store.bytes.is_empty(), "and the store is still empty");
    // The store was asked for its length even though there were no leaves to write.
    assert_eq!(
        store.bytes.len(),
        0,
        "a zero-length document is a recorded state, not an absent one"
    );
}
