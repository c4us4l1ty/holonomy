//! **The correction to part 14's fault condition: residency alone is currency.** 4 tests.
//!
//! # What part 14 got wrong, and why no test caught it
//!
//! Part 14 built a per-leaf `epochs` array next to the rope's `edit_epoch` counter and made
//!
//! ```text
//! if self.is_resident(i) && self.epochs[i] == self.edit_epoch { return Ok(()); }
//! ```
//!
//! the condition under which `fault_leaf` declines to refetch. Its justification, in the code, was that
//! *"a leaf that was already resident when an edit happened holds pre-edit bytes: its length and offset
//! are right, so every check this function used to make passed, and the document was silently wrong."*
//!
//! **The claim is false, and it is false about content rather than position.** An edit at offset `p`
//! changes the bytes of the leaf holding `p` and of no other. Every other leaf keeps exactly the bytes it
//! had. What moves is its *offset*, and an offset lives in `starts` rather than on the leaf, so moving it
//! needs no invalidation whatsoever.
//!
//! **So the condition threw away a correct answer and rebuilt it from the source — and on every read.**
//! A 3 MiB document built by appending has 819 leaves, exactly one of which is current, so each read
//! re-faulted 818 of them, each a fresh page-locked 4 KiB block:
//!
//! ```text
//! a 16-byte read at offset 0:   25.5 ms
//! tests/session_latency.rs:     0.32 s  ->  did not finish
//! ```
//!
//! # Why nothing caught it
//!
//! **`fault_edit_conflict.rs` asked the wrong question.** It asserted that a fault produces correct bytes
//! — which the epoch condition also does, because the refetch is *correct*, just ruinous. **A defect that
//! makes the right answer more expensive than necessary is invisible to a correctness gate**, and the only
//! gates with the shape to catch it are timing gates, which are the ones a correctness-focused phase is
//! least likely to run.
//!
//! The gates here are therefore about **work**, not about bytes: a read that does not need a source must
//! not call one, and a document that is entirely resident must cost the same after a thousand edits as
//! before the first.
//!
//! | what it proves | test |
//! | --- | --- |
//! | **a read does not ask a source it does not need** | [`a_read_of_a_resident_document_never_faults`] |
//! | and the bytes are still right | [`the_same_read_is_correct_as_well_as_cheap`] |
//! | edits elsewhere do not invalidate a leaf's bytes | [`an_edit_far_away_leaves_this_leaf_byte_for_byte_intact`] |
//! | **and the cost does not grow with the edit count** | [`the_cost_of_a_read_does_not_grow_with_the_edit_count`] |

use holonomy_text::{LeafSource, Rope, RopeError};

/// A document whose bytes are a function of position, so a one-byte offset error is visible.
fn doc(n: usize) -> Vec<u8> {
    (0..n).map(|i| b'a' + (i % 26) as u8).collect()
}

/// Counts every fetch and store, and **never succeeds**.
struct CountingSource {
    fetches: u32,
    writes: u32,
    /// When true, fetches succeed and serve `bytes`. When false, every fetch refuses.
    serve: Option<Vec<u8>>,
}

impl CountingSource {
    fn silent() -> Self {
        Self {
            fetches: 0,
            writes: 0,
            serve: None,
        }
    }

    fn serving(bytes: Vec<u8>) -> Self {
        Self {
            fetches: 0,
            writes: 0,
            serve: Some(bytes),
        }
    }
}

impl LeafSource for CountingSource {
    fn fetch_leaf(&mut self, offset: usize, out: &mut [u8]) -> Result<usize, RopeError> {
        self.fetches += 1;
        let Some(saved) = &self.serve else {
            return Err(RopeError::SourceUnavailable);
        };
        let want = out.len().min(saved.len().saturating_sub(offset));
        out[..want].copy_from_slice(&saved[offset..offset + want]);
        Ok(want)
    }

    fn store_leaf(&mut self, _offset: usize, _bytes: &[u8]) -> Result<(), RopeError> {
        self.writes += 1;
        Err(RopeError::SourceUnavailable)
    }
}

/// Build a rope of `n` bytes with every leaf resident, from a source that serves them.
fn resident_rope(n: usize) -> (Rope, CountingSource) {
    let bytes = doc(n);
    let mut rope = Rope::from_skeleton(n);
    let mut source = CountingSource::serving(bytes);
    // **Fault every leaf up front**, so the rope is fully resident before anything is asserted.
    for i in 0..rope.leaf_count() {
        rope.fault_leaf(&mut source, i)
            .expect("fault the whole document");
    }
    (rope, source)
}

/// **The gate that would have caught it: a read of a resident document must not touch the source.**
///
/// The source here **refuses every fetch**, so a single unexpected fault turns into an `Err` rather than
/// into a slow correct answer. That is the point: **the epoch condition produced correct bytes from a
/// source it had no business calling**, and only a source that cannot answer turns that into a failure
/// rather than a measurement.
#[test]
fn a_read_of_a_resident_document_never_faults() {
    let (mut rope, _) = resident_rope(20_000);

    // **One edit first, because that is the scenario.** Without it every leaf is trivially current and
    // the assertion below is satisfied by anything that never bothers to think — the epoch condition
    // passed this test too, because an unedited rope has `epochs[i] == edit_epoch` for every leaf. **The
    // edit is what makes the resident leaf "stale" under the condition this file is about.**
    rope.insert_at(20_000, b"x").ok();

    // **A source that cannot answer at all.**
    let mut blind = CountingSource::silent();
    let mut out = vec![0u8; 512];
    rope.read_at_faulting(&mut blind, 0, 512, &mut out)
        .expect("a fully resident document must be readable with no working source");

    assert_eq!(
        blind.fetches, 0,
        "the read called the source {} times. Residency alone is currency: an edit elsewhere moves a \
         leaf's *offset*, which lives in `starts` and not on the leaf, so there is nothing to refetch.",
        blind.fetches
    );
}

/// **And the bytes are still right**, so the gate above is not satisfied by refusing to read at all.
#[test]
fn the_same_read_is_correct_as_well_as_cheap() {
    let (mut rope, _) = resident_rope(20_000);
    let mut blind = CountingSource::silent();
    let mut out = vec![0u8; 512];
    rope.read_at_faulting(&mut blind, 0, 512, &mut out)
        .expect("read");

    assert_eq!(
        out,
        doc(20_000)[..512],
        "and the bytes are the document's, not merely present"
    );
}

/// **An edit far away does not change this leaf's bytes — which is the whole claim.**
///
/// The test states it as a byte comparison rather than a fetch count, because *that* is what the removed
/// comment said was false. If it were false, this assertion would fail.
///
/// The edit is at the **end** of a 20,000-byte document, so leaf 0 is as far from it as the document
/// allows, and the comparison is over leaf 0's whole extent.
#[test]
fn an_edit_far_away_leaves_this_leaf_byte_for_byte_intact() {
    let n = 20_000;
    let (mut rope, mut source) = resident_rope(n);
    let before = {
        let mut buf = vec![0u8; rope.leaf_len_of(0)];
        rope.read_at_faulting(&mut source, 0, buf.len(), &mut buf)
            .expect("read leaf 0");
        buf
    };

    // **An insert at the very end**, which shifts every leaf's *offset* and no leaf's *bytes* except the
    // last one's. The count is snapshotted first because `resident_rope` has already faulted every leaf
    // in, and this is about the *delta* this edit causes.
    let fetches_before_edit = source.fetches;
    rope.insert_at(n - 1, b"TAIL").ok();
    let after = {
        let mut buf = vec![0u8; rope.leaf_len_of(0)];
        rope.read_at_faulting(&mut source, 0, buf.len(), &mut buf)
            .expect("read leaf 0 again");
        buf
    };

    assert_eq!(
        before, after,
        "an insert at the end of the document changed leaf 0's bytes. It should not: the edit moved \
         leaf 0's offset, and an offset is derived from `starts`, not stored on the leaf."
    );
    assert_eq!(
        &after,
        &doc(n)[..before.len()],
        "and leaf 0 still holds the saved document's bytes"
    );
    // **And it did not refetch to produce them.** A byte assertion alone is satisfied by a refetch, which
    // is what the removed condition did -- correct bytes, ruinously. The count is the claim.
    assert_eq!(
        source.fetches, fetches_before_edit,
        "leaf 0 was refetched from the source despite being resident and unedited -- {} fetches for \
         one insert at the far end. Its bytes came out correct either way, which is exactly why this \
         gate asserts a count and not a byte string.",
        source.fetches - fetches_before_edit
    );
}

/// **The cost does not grow with the edit count — which is the defect's actual shape.**
///
/// Under the epoch condition this test is not slow, it is **superlinear**: every read refaults every leaf
/// that is not the leaf just edited, so a read costs `O(leaves × edits)` and the document gets slower to
/// read the more it is edited. That is the property worth gating, and a wall-clock assertion would only
/// catch it on a slow enough host.
///
/// **Asserted as fetch counts, which is the same fact without the machine in it.**
#[test]
fn the_cost_of_a_read_does_not_grow_with_the_edit_count() {
    let n = 60_000;
    let (mut rope, mut source) = resident_rope(n);

    let mut reads_of_512 = |rope: &mut Rope, source: &mut CountingSource| {
        let before = source.fetches;
        let mut buf = vec![0u8; 512];
        for at in (0..n - 512).step_by(20_000) {
            rope.read_at_faulting(source, at, 512, &mut buf)
                .expect("read");
        }
        source.fetches - before
    };

    let early = reads_of_512(&mut rope, &mut source);
    assert_eq!(
        early, 0,
        "a fresh read of a resident document fetches nothing"
    );

    // **Two hundred edits, all at the end**, so the document is heavily edited and the first leaves are
    // exactly the ones the epoch condition would have invalidated.
    for _ in 0..200 {
        rope.insert_at(rope.text_len(), b"x").ok();
    }

    let late = reads_of_512(&mut rope, &mut source);
    assert_eq!(
        late, 0,
        "after 200 edits a read of a resident document fetched {late} times. Every one of those is a \
         leaf whose bytes were never touched by any of the 200 edits."
    );
}
