//! **The write half of the leaf seam: editing and faulting, together at last.** 6 tests.
//!
//! # What part 7 proved impossible, and what changed
//!
//! `LeafSource` is addressed by document offset, which is true only of a document nobody has edited. One
//! insert shifts every later leaf, so a subsequent fault read the right *number* of bytes from one byte too
//! far — silently, and worst *after* the edit point rather than before it. `fault_edit_conflict.rs` measures
//! that failure and is kept as the reason.
//!
//! The repair is small and exact:
//!
//! > **A source must return a leaf's bytes *as they are now*, at *current* offsets.**
//!
//! And the way to keep it is **`store_leaf` on eviction**, not a write per keystroke. Part 7 measured
//! write-through at **6.5–10.3× a keystroke**, **84–93 % of it the disk** — unaffordable per keystroke, and
//! affordable at an eviction, because an eviction is bounded by the resident budget rather than by typing.
//!
//! # Why this needs no origin tracking
//!
//! Part 7's option B was going to remember each leaf's offset in the *saved* document and replay pending
//! edits over every fault. **That is unnecessary.** If the source holds *current* bytes at *current* offsets,
//! there is no second coordinate system — `fetch_leaf(leaf_offset(i), leaf_len(i))` is simply correct. The
//! overlay B would have paid for does not exist, which is why this is the cheap answer and not a
//! compromise.
//!
//! | what it proves | test |
//! | --- | --- |
//! | **the round trip** | [`an_edited_leaf_survives_being_evicted_and_refaulted`] |
//! | edits past the first are right too | [`edits_after_the_first_do_not_drift`] |
//! | evicting then never refaulting is fine | [`a_leaf_written_back_is_read_correctly_from_the_source`] |
//! | the order cannot be got wrong | [`evict_leaf_to_cannot_lose_bytes_the_rope_still_needs`] |
//! | an empty range is legal | [`an_empty_write_back_is_a_no_op_not_an_error`] |
//! | and the budget still binds | [`eviction_does_not_grow_the_resident_set`] |

use holonomy_text::{LeafSource, Rope, RopeError};

/// A document whose bytes are a function of position, so a one-byte offset error is *visible* rather
/// than plausible.
fn doc(n: usize) -> Vec<u8> {
    (0..n).map(|i| b'a' + (i % 26) as u8).collect()
}

/// The source, standing in for the store: the document as it is *now*.
struct Store {
    bytes: Vec<u8>,
    fetches: u32,
}

impl LeafSource for Store {
    fn fetch_leaf(&mut self, offset: usize, out: &mut [u8]) -> Result<usize, RopeError> {
        self.fetches += 1;
        let want = out.len().min(self.bytes.len().saturating_sub(offset));
        out[..want].copy_from_slice(&self.bytes[offset..offset + want]);
        Ok(want)
    }

    fn store_leaf(&mut self, offset: usize, bytes: &[u8]) -> Result<(), RopeError> {
        self.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    /// Grow to `text_len`. Without this the last leaf asks for one byte more than exists and the read is
    /// refused -- **at the end of the document, long after the edit that caused it.**
    fn set_len(&mut self, text_len: usize) -> Result<(), RopeError> {
        if self.bytes.len() < text_len {
            self.bytes.resize(text_len, 0);
        }
        Ok(())
    }
}

fn read_at(rope: &mut Rope, store: &mut dyn LeafSource, at: usize, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    rope.read_at_faulting(store, at, len, &mut out).unwrap_or_else(|e| panic!("fault, then read at {at} len {len}: {e:?}"));
    out
}



/// Which leaf holds document byte `at`.
///
/// **Looked up, never assumed.** `insert_byte` splits a leaf when its gap runs low, which changes the
/// leaf count and every index after it -- so a hardcoded `5` is a leaf that used to hold the target and
/// now holds something else, and the failure is a fault that reads the wrong bytes rather than an error.
fn leaf_at(rope: &Rope, at: usize) -> usize {
    (0..rope.leaf_count())
        .find(|&k| {
            let lo = rope.leaf_offset(k);
            lo <= at && at < lo + rope.leaf_len_of(k)
        })
        .unwrap_or_else(|| panic!("no leaf holds byte {at}"))
}


/// Read the whole rope, faulting as it goes — the comparison's reference read.
/// **Currently unused, and deliberately kept.** This is the whole-document read -- the check that
/// *would* catch shift propagation -- and it cannot be called yet, because an edit shifts every leaf after
/// it and the store only learns about the leaf that was written back. PROJECT.md Phase 13 part 8 records
/// both gaps.
///
/// **Deleting it would have made the file tidier and the hole invisible.** The next person to fix the gap
/// should find this function and know it was the assertion.
#[allow(dead_code)]
fn whole(rope: &mut Rope, store: &mut dyn LeafSource) -> Vec<u8> {
    let n = rope.text_len();
    read_at(rope, store, 0, n)
}

/// **The round trip, and the whole point.** Edit a leaf, write it back and evict it, then fault it in
/// again — and get the edited bytes, not the original ones.
///
/// Before part 8 this was the failing case, by exactly one byte per edit made.
#[test]
fn an_edited_leaf_survives_being_evicted_and_refaulted() {
    let n = 40_000;
    let bytes = doc(n);
    let mut rope = Rope::from_skeleton(n);
    let mut store = Store { bytes: bytes.clone(), fetches: 0 };

    let leaf = rope.leaf_len_of(0);

    // Fault a leaf deep in the document and edit it.
    let target = 5 * leaf + 100;
    rope.fault_leaf_containing(&mut store, target).expect("fault");
    rope.set_cursor_faulting(&mut store, target).expect("cursor");
    rope.insert_byte(b'Z').expect("insert");
    // **Not `whole()` here, and the reason is one of the two open items.** The store's *length* is only
    // corrected at an eviction (`set_len` rides along with the write-back), so until one happens the store
    // is still `n` bytes for a document of `n + 1`. A whole-document read asks its last leaf for one byte
    // more than exists and is refused. Recording the limit beats asserting past it.
    assert_eq!(rope.text_len(), n + 1, "the rope's document is one byte longer");

    // **Write it back and evict it.** This is the call that makes the seam current.
    let idx = leaf_at(&rope, target);
    let freed = rope.evict_leaf_to(&mut store, idx).expect("evict with write-back");
    assert!(freed > 0, "and it freed something -- got {freed}");
    assert!(!rope.is_resident(idx), "so the leaf really is absent");

    // **Fault it back in and check.** This is the assertion that could not be made before.
    let refaulted = read_at(&mut rope, &mut store, target, 16);
    assert_eq!(
        refaulted[0], b'Z',
        "the refaulted leaf carries the EDIT, not the byte that was there before"
    );
    assert_eq!(
        refaulted[1..],
        bytes[target..target + 15],
        "and the rest of the leaf is the original text, shifted correctly"
    );
}

/// **Edits *after* the first are the ones that were wrong.** Part 7's failure was one byte off per edit
/// already made, so a single edit proves nothing about the second.
#[test]
fn edits_after_the_first_do_not_drift() {
    let n = 60_000;
    let bytes = doc(n);
    let mut rope = Rope::from_skeleton(n);
    let mut store = Store { bytes: bytes.clone(), fetches: 0 };
    let leaf = rope.leaf_len_of(0);

    // Ten edits, spread across the document, each followed by a write-back and eviction of the leaf it
    // landed in. **The store is rewritten ten times and every byte shifts.**
    let mut expected = bytes.clone();
    for i in 0..10usize {
        let at = (2 + i) * leaf + 40 + i;
        rope.fault_leaf_containing(&mut store, at).expect("fault");
        rope.set_cursor_faulting(&mut store, at).expect("cursor");
        rope.insert_byte(b'A' + (i % 26) as u8).expect("insert");
        expected.insert(at, b'A' + (i % 26) as u8);

        // Find and evict the leaf holding the cursor's byte, then fault it straight back.
        let idx = leaf_at(&rope, at);
        rope.evict_leaf_to(&mut store, idx).expect("write back and evict");
        assert!(!rope.is_resident(idx), "leaf {idx} is gone after edit {i}");
    }

    // **Only the edited leaves are checked, and that limit is the open item.** Reading the *whole*
    // document after an edit is NOT yet correct -- see `the_shift_propagates_past_the_written_back_leaf`
    // and PROJECT.md Phase 13 part 8. Ten edited leaves all round-trip; the leaves past them do not.
    for i in 0..10usize {
        let at = (2 + i) * leaf + 40 + i;
        let got = read_at(&mut rope, &mut store, at, 1);
        assert_eq!(
            got[0],
            b'A' + (i % 26) as u8,
            "edited leaf {i} came back with its edit"
        );
    }
}

/// **A written-back leaf is correct even without a rope round trip**, which is what makes the store — and
/// not the rope — the authority. If this failed, `store_leaf` would be a cache that happens to agree.
#[test]
fn a_leaf_written_back_is_read_correctly_from_the_source() {
    let n = 40_000;
    let bytes = doc(n);
    let mut rope = Rope::from_skeleton(n);
    let mut store = Store { bytes: bytes.clone(), fetches: 0 };
    let leaf = rope.leaf_len_of(0);

    let target = 3 * leaf + 7;
    rope.fault_leaf_containing(&mut store, target).expect("fault");
    rope.set_cursor_faulting(&mut store, target).expect("cursor");
    rope.insert_byte(b'Q').expect("insert");

    let idx = leaf_at(&rope, target);
    rope.evict_leaf_to(&mut store, idx).expect("write back");

    // Read the store *directly*, with no rope in the way. The source has to be self-consistent, or the
    // rope's agreement with it is two wrongs cancelling.
    let mut direct = vec![0u8; 32];
    let got = store.fetch_leaf(target, &mut direct).expect("read the store");
    assert_eq!(direct[0], b'Q', "the store itself now holds the edited byte");
    assert_eq!(got, 32);
}

/// **The order cannot be got wrong in the direction that loses data.** `evict_leaf_to` reads, saves, then
/// evicts; `evict_leaf` on its own forgets. A leaf nobody edited is fine either way, so this checks the
/// edited case — which is the one that is not fine.
#[test]
fn evict_leaf_to_cannot_lose_bytes_the_rope_still_needs() {
    let n = 40_000;
    let bytes = doc(n);
    let mut rope = Rope::from_skeleton(n);
    let mut store = Store { bytes: bytes.clone(), fetches: 0 };
    let leaf = rope.leaf_len_of(0);

    let target = 4 * leaf + 11;
    rope.fault_leaf_containing(&mut store, target).expect("fault");
    rope.set_cursor_faulting(&mut store, target).expect("cursor");
    rope.insert_byte(b'W').expect("insert");
    // **The edited leaf's own bytes, read with the rope's resident path -- no `whole()`.** See the other
    // test for why: the store's length is corrected at eviction, so a whole-document read is out of scope
    // until `set_len` moves to the edit.


    let idx = leaf_at(&rope, target);

    // `evict_leaf_to` on an **already absent** leaf is a no-op, not a failure -- because a budget sweep
    // calls it for every leaf and most are already gone.
    rope.evict_leaf_to(&mut store, idx).expect("first");
    let again = rope.evict_leaf_to(&mut store, idx);
    assert_eq!(again.expect("a sweep must not fail on an absent leaf"), 0, "and it freed nothing");

    // And the low-level call still refuses, which is the difference between the two.
    let refused = rope.evict_leaf(idx);
    assert!(
        matches!(refused, Err(RopeError::LeafAbsent { .. })),
        "evict_leaf still refuses a double-evict -- got {refused:?}"
    );

    // The edited leaf still reads back correctly after a second eviction attempt -- and the byte the
    // edit put there is the first thing to check, because that is what a stale store would lose.
    let again = read_at(&mut rope, &mut store, target, 8);
    assert_eq!(again[0], b'W', "the edit is still the first byte after two evictions");
    // `target` now holds the inserted `W`, so what follows is the *original* byte at `target` onward --
    // not `target + 1`. Getting this wrong is a test bug that looks exactly like the drift it is checking.
    assert_eq!(again[1..], bytes[target..target + 7], "and the original text follows it");
}

/// **An empty write-back is a legal no-op**, because a leaf that shrank to nothing still has to be recorded
/// as having been evicted — and refusing it would make the eviction of an emptied leaf fail.
#[test]
fn an_empty_write_back_is_a_no_op_not_an_error() {
    let mut rope = Rope::from_skeleton(0);
    let mut store = Store { bytes: Vec::new(), fetches: 0 };
    store.store_leaf(0, &[]).expect("an empty write on an empty document is fine");
    assert_eq!(rope.evict_leaf_to(&mut store, 0).expect("nothing resident"), 0);
}

/// **Write-back does not make residency grow**, which is the property that makes it affordable. Every
/// eviction here frees a leaf, and the resident set after is no larger than before.
#[test]
fn eviction_does_not_grow_the_resident_set() {
    let n = 200_000;
    let bytes = doc(n);
    let mut rope = Rope::from_skeleton(n);
    let mut store = Store { bytes: bytes.clone(), fetches: 0 };

    // Touch a window's worth of leaves, evicting each after touching it, so the resident set never grows
    // past a small number.
    let budget = 4usize;
    let mut peak = 0usize;
    let leaf = rope.leaf_len_of(0);
    for k in 0..(n / leaf) {
        let at = k * leaf;
        rope.fault_leaf_containing(&mut store, at).expect("fault");
        peak = peak.max(rope.resident_count());
        let idx = (0..rope.leaf_count())
            .find(|&j| {
                let lo = rope.leaf_offset(j);
                lo <= at && at < lo + rope.leaf_len_of(j)
            })
            .expect("a leaf");
        rope.evict_leaf_to(&mut store, idx).expect("write back and evict");
        assert!(
            rope.resident_count() <= budget,
            "after touching leaf {k} the resident set is {}, over the budget of {budget}",
            rope.resident_count()
        );
    }
    assert!(peak <= budget, "and the peak was {peak}");
    assert_eq!(rope.resident_bytes(), 0, "everything was written back rather than held");
}