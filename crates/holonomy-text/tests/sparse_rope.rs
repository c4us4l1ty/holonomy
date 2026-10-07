//! **Phase 13 part 3's gate: a rope can hold absent leaves, and it stays the same document.** 9 tests.
//!
//! # The claim
//!
//! A rope whose leaves have been evicted **still reports the same `text_len()`, the same offsets, and the
//! same bytes** — and a read that needs an evicted leaf gets it back from a source.
//!
//! | what it proves | test |
//! | --- | --- |
//! | eviction does not change the document | [`evicting_a_leaf_does_not_change_the_document_length`] |
//! | nor its offsets | [`offsets_are_identical_before_and_after_eviction`] |
//! | the fast path *refuses* rather than lies | [`read_at_refuses_an_absent_leaf_and_names_it`] |
//! | a faulting read gets the bytes back | [`a_faulting_read_recovers_an_evicted_leaf`] |
//! | any range, not just whole leaves | [`every_byte_of_a_sparse_rope_reads_identically`] |
//! | the audit gate refuses to look through absence | [`the_scrub_audit_refuses_an_absent_leaf`] |
//! | a mutation refuses rather than corrupts | [`editing_an_absent_leaf_is_refused_not_applied`] |
//! | and a double-evict is visible | [`evicting_an_absent_leaf_twice_is_an_error`] |
//! | a short source is refused, not zero-filled | [`a_short_source_is_refused_rather_than_zero_filled`] |
//!
//! # The one test that matters most
//!
//! [`evicting_a_leaf_does_not_change_the_document_length`] is the load-bearing one. `LeafSlot` carries a
//! length in the absent case *specifically* because `Rope::text_len` asks the last slot for its length: if
//! it did not, evicting the last leaf would silently shorten the document. **That is a corruption, not a
//! missed read**, and it would not be caught by any test that only reads resident bytes — so the gate
//! evicts the *last* leaf first, which is the case that would break.
//!
//! # Two things the gate caught that were not obvious
//!
//! **1. The seam must be keyed by document offset, not by leaf index.** The first version of the source
//! here was `fetch_leaf(index, …)` mapping `index * FILL`. That is wrong, and wrong *silently*: a rope's
//! leaf boundaries are wherever its splits and merges left them, so the four leaves of an 11,643-byte
//! document start at **0, 3,841, 7,682, 11,523** — not at multiples of anything. Indexing read the right
//! number of bytes from the wrong place, and no length check catches that. `starts[i]` is the only stable
//! address a leaf has, and it is also what maps to a section (`offset / CHUNK_PLAINTEXT`).
//!
//! **2. Two of these tests were passing for the wrong reason until they were not.** The scrub-audit test
//! asked about `b'z'`, which `doc()` cycles everywhere — so the first resident leaf answered `true` and
//! the absent leaf was never consulted. A test that proves nothing is worse than no test, because it is
//! counted. It now uses a needle the fixture provably does not contain, and asserts that.

use holonomy_text::{LeafSource, Rope, RopeError, GAP_MINIMUM, LEAF_CAPACITY};

const FILL: usize = LEAF_CAPACITY - GAP_MINIMUM;

/// A `LeafSource` over a plain `Vec<u8>`, standing in for a container.
///
/// **A `Vec` rather than a real store, on purpose.** `holonomy-text` cannot depend on
/// `holonomy-container`, which is why `LeafSource` exists at all — so this gate runs with no crypto and no
/// file, and the real implementation (`SectionStore`) has to satisfy the same two-method trait to be
/// usable. It also makes the source's behaviour *controllable*, which is what lets
/// `a_short_source_is_refused_rather_than_zero_filled` be written at all.
struct VecSource {
    bytes: Vec<u8>,
    fetches: u32,
    /// Report this many bytes instead of the leaf's real length, to drive the short-source path.
    truncate_to: Option<usize>,
}

impl VecSource {
    fn new(bytes: Vec<u8>) -> Self {
        Self { bytes, fetches: 0, truncate_to: None }
    }
}

impl LeafSource for VecSource {
    /// **Keyed by document offset**, which is the whole point of the seam.
    ///
    /// The first version of this was keyed by leaf *index* and mapped `index * per_leaf`, and it was
    /// wrong in a way no length check catches: a rope's leaf boundaries are wherever its splits and
    /// merges left them, so leaf 1 does not begin at 3,840. The gate caught it because the bytes came
    /// back the right length and the wrong content. **The offset is the only stable address a leaf has**,
    /// and it is also what maps to a section: `offset / CHUNK_PLAINTEXT`.
    fn fetch_leaf(&mut self, offset: usize, out: &mut [u8]) -> Result<usize, RopeError> {
        self.fetches += 1;
        let want = out.len().min(self.bytes.len().saturating_sub(offset));
        out[..want].copy_from_slice(&self.bytes[offset..offset + want]);
        Ok(self.truncate_to.unwrap_or(want))
    }

    /// The write half. A `Vec` source is the document itself, so writing back is assigning into it --
    /// which is the point: after this, `fetch_leaf` returns bytes as they are *now*.
    fn store_leaf(&mut self, offset: usize, bytes: &[u8]) -> Result<(), RopeError> {
        self.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    /// Grow to `text_len`. A `Vec` source can grow, so it must -- a source with the right bytes and the
    /// old length hands back a short leaf at the end of the document.
    fn set_len(&mut self, text_len: usize) -> Result<(), RopeError> {
        if self.bytes.len() < text_len {
            self.bytes.resize(text_len, 0);
        }
        Ok(())
    }
}

/// A document long enough to have several leaves, with no byte equal to a filler so a
/// misaddressed leaf is visible rather than plausible.
fn doc(leaves: usize) -> Vec<u8> {
    let n = leaves * FILL + 123;
    (0..n).map(|i| (b'a' + (i % 26) as u8)).collect()
}

/// **The load-bearing test.** Eviction must not move the document's length.
///
/// The last leaf is evicted **first**, because `text_len()` is `starts.last() + last slot's length` — so a
/// slot that forgot its length would shorten the document the moment the last leaf went absent. Every other
/// leaf could be evicted without this showing up.
#[test]
fn evicting_a_leaf_does_not_change_the_document_length() {
    let text = doc(4);
    let mut rope = Rope::from_text(&text).expect("load");
    let before = rope.text_len();
    assert_eq!(before, text.len());
    assert!(rope.leaf_count() > 3, "the fixture needs several leaves, got {}", rope.leaf_count());

    // Every leaf, last one first, so the load-bearing case is exercised first.
    let last = rope.leaf_count() - 1;
    for i in (0..rope.leaf_count()).rev() {
        rope.evict_leaf(i).expect("evict");
        assert_eq!(
            rope.text_len(),
            before,
            "evicting leaf {i} changed the document length from {before} to {}",
            rope.text_len()
        );
    }

    assert_eq!(rope.resident_count(), 0, "everything is evicted");
    assert_eq!(rope.resident_bytes(), 0, "and nothing is held");
    assert_eq!(rope.leaf_count(), last + 1, "but the spine is still the same length");
}

/// Offsets must survive eviction, because `locate` is a binary search over `starts` and no offset should
/// depend on which bytes are held.
#[test]
fn offsets_are_identical_before_and_after_eviction() {
    let text = doc(4);
    let mut rope = Rope::from_text(&text).expect("load");
    let n = rope.leaf_count();
    for i in 0..n {
        assert!(rope.is_resident(i), "leaf {i} starts resident");
    }
    for i in 0..n {
        rope.evict_leaf(i).expect("evict");
    }
    for i in 0..n {
        assert!(!rope.is_resident(i), "leaf {i} is absent");
    }
    // And the document reads back byte-for-byte through a source that does not know about leaves at all.
    let mut src = VecSource::new(text.clone());
    let mut got = vec![0u8; text.len()];
    rope.read_at_faulting(&mut src, 0, got.len(), &mut got).expect("faulting read");
    assert_eq!(got, text, "a fully evicted rope still reads back as the same document");
}

/// The `&self` path cannot fault, so it refuses — **and it names the leaf**, because that is what lets a
/// caller holding a store work out what to fetch.
#[test]
fn read_at_refuses_an_absent_leaf_and_names_it() {
    let text = doc(3);
    let mut rope = Rope::from_text(&text).expect("load");
    let target = 1;
    rope.evict_leaf(target).expect("evict");

    let at = rope.leaf_offset(target);
    let n = rope.leaf_len_of(target);
    // **Sized from the leaf, not from `FILL`.** A leaf holds 3,841 bytes here, not 3,840: the gap is
    // opened at `GAP_TARGET` and closed as bytes arrive, so leaf boundaries land at 0, 3841, 7682 --
    // not at multiples of anything. A test that sized its buffer from `LEAF_CAPACITY - GAP_MINIMUM`
    // would fail with `OutOfBounds` and blame the rope for a buffer that was one byte short.
    let mut buf = vec![0u8; n];
    let err = rope.read_at(at, n, &mut buf).expect_err("must refuse");
    match err {
        RopeError::LeafAbsent { leaf } => assert_eq!(
            leaf, target,
            "the error has to carry the index, or a caller with a store cannot map it to sections"
        ),
        other => panic!("expected LeafAbsent, got {other:?}"),
    }

    // A range that misses the absent leaf entirely still works, with no fault and no error.
    let mut ok = vec![0u8; rope.leaf_len_of(0)];
    let n = rope.leaf_len_of(0);
    rope.read_at(0, n, &mut ok).expect("leaf 0 is resident");
    assert_eq!(&ok[..n], &text[..n], "and returns the right bytes");
}

/// A faulting read recovers an absent leaf, and pays exactly one fetch for it.
#[test]
fn a_faulting_read_recovers_an_evicted_leaf() {
    let text = doc(3);
    let mut rope = Rope::from_text(&text).expect("load");
    let target = 1;
    rope.evict_leaf(target).expect("evict");

    let at = rope.leaf_offset(target);
    let n = rope.leaf_len_of(target);
    let mut src = VecSource::new(text.clone());
    let mut buf = vec![0u8; n];
    rope.read_at_faulting(&mut src, at, n, &mut buf).expect("faulting read");

    assert_eq!(&buf[..n], &text[at..at + n], "right bytes");
    assert_eq!(src.fetches, 1, "one absent leaf, one fetch");
    assert!(rope.is_resident(target), "and the leaf is resident again");
    assert_eq!(rope.text_len(), text.len(), "and the document length never moved");
}

/// The real test: **every byte** of a rope with holes in it, read back identically, through a source whose
/// leaf size does **not** tile the document.
#[test]
fn every_byte_of_a_sparse_rope_reads_identically() {
    let text = doc(5);
    let mut rope = Rope::from_text(&text).expect("load");
    let n = rope.leaf_count();

    // Evict every other leaf, so reads straddle resident/absent boundaries.
    for i in (0..n).step_by(2) {
        rope.evict_leaf(i).expect("evict");
    }
    let holes = rope.leaf_count() - rope.resident_count();
    assert!(holes > 1, "the fixture needs several holes, got {holes}");

    let mut src = VecSource::new(text.clone());
    let mut got = vec![0u8; text.len()];
    rope.read_at_faulting(&mut src, 0, got.len(), &mut got).expect("faulting read");

    assert_eq!(rope.text_len(), text.len(), "length unchanged by all that faulting");
    assert_eq!(
        got, text,
        "a rope with holes in it must read back as the identical document -- this is the byte-level \
         claim, and it is what an offset/index confusion fails"
    );
    assert_eq!(rope.resident_count(), n, "the read made every leaf resident again");
}

/// **The destructive-delete gate must refuse to look through absence.** This is the one place where
/// skipping an absent leaf would be *unsound* rather than merely incomplete: "is this byte anywhere in the
/// rope" answered `false` for a leaf that was never examined is the wrong answer for a scrub audit.
#[test]
fn the_scrub_audit_refuses_an_absent_leaf() {
    let text = doc(3);
    let mut rope = Rope::from_text(&text).expect("load");
    let target = 2;
    rope.evict_leaf(target).expect("evict");

    // **A needle that does not occur anywhere**, so the scan does not stop early at a resident leaf
    // that happens to contain it and never reaches the absent one. `doc()` cycles `a..=z`, so `z` is
    // everywhere and the first resident leaf answers `true` before leaf 2 is consulted -- which is a
    // passing test that proved nothing. `0xFF` is outside the fixture's range.
    const ABSENT_NEEDLE: u8 = 0xFF;
    assert!(
        !text.contains(&ABSENT_NEEDLE),
        "the fixture must not contain the needle, or the audit can answer early"
    );
    let err = rope.any_leaf_contains(ABSENT_NEEDLE).expect_err("must refuse, not report false");
    assert!(
        matches!(err, RopeError::LeafAbsent { leaf } if leaf == target),
        "expected LeafAbsent for leaf {target}, got {err:?}"
    );

    // With the leaf back, the same call answers normally.
    let at = rope.leaf_offset(target);
    let mut src = VecSource::new(text.clone());
    let mut one = [0u8; 1];
    rope.read_at_faulting(&mut src, at, 1, &mut one).expect("fault leaf back");
    assert!(
        !rope.any_leaf_contains(ABSENT_NEEDLE).expect("now answerable"),
        "a fully resident rope answers the audit, and the answer is 'no'"
    );
}

/// A mutation into an absent leaf must refuse, and must leave the document untouched.
///
/// The assertion that matters is the second one: a mutation that half-applied would leave the caret
/// somewhere new and the bytes unchanged, which is a state no test can recover from.
#[test]
fn editing_an_absent_leaf_is_refused_not_applied() {
    let text = doc(3);
    let mut rope = Rope::from_text(&text).expect("load");
    let target = 1;
    let before_len = rope.text_len();
    let before_cursor = rope.cursor();
    rope.evict_leaf(target).expect("evict");

    // The real path: `set_cursor` is what an edit does first, and it is where the leaf is resolved.
    let err = rope.set_cursor(rope.leaf_offset(target));
    assert!(
        matches!(err, Err(RopeError::LeafAbsent { leaf })),
        "an edit into an absent leaf must be a typed refusal, got {err:?}"
    );
    assert_eq!(rope.text_len(), before_len, "and must not have changed the length");
    assert_eq!(rope.cursor(), before_cursor, "nor moved the caret");

    // A mutation into a *resident* leaf still works, so the refusal is about absence and not breakage.
    rope.set_cursor(0).expect("leaf 0 is resident");
    rope.insert_byte(b'Z').expect("the resident path still edits");
    assert_eq!(rope.text_len(), before_len + 1, "and the length grew by exactly one");

    // **And faulting the leaf in makes the same edit succeed**, which is the point of the refusal being
    // a refusal rather than a limitation: the caller has something it can do about it.
    let at = rope.leaf_offset(target);
    let mut src = VecSource::new(text.clone());
    let mut one = [0u8; 1];
    rope.read_at_faulting(&mut src, at, 1, &mut one).expect("fault the leaf in");
    rope.set_cursor(at).expect("now the cursor can go there");
    rope.insert_byte(b'Z').expect("and the edit lands");
    assert_eq!(rope.text_len(), before_len + 2, "two bytes added: one before, one here");
}

/// A double-evict must be visible. Reporting an eviction that freed nothing would make a bug look like
/// progress, and the count is what a residency budget is asserted on.
#[test]
fn evicting_an_absent_leaf_twice_is_an_error() {
    let text = doc(3);
    let mut rope = Rope::from_text(&text).expect("load");
    let first = rope.evict_leaf(1).expect("first evict");
    let resident = rope.resident_count();

    let err = rope.evict_leaf(1).expect_err("second evict must be refused");
    assert!(matches!(err, RopeError::LeafAbsent { leaf } if leaf == 1), "got {err:?}");
    assert_eq!(
        rope.resident_count(),
        resident,
        "the refused eviction must not have changed residency -- it freed nothing"
    );
    assert!(first > 0, "the first eviction reported {} bytes", first);
    assert_eq!(rope.text_len(), text.len(), "and the document is still the right length");
}

/// A short source must be refused, not zero-filled — a zero-filled gap would read as a document full of
/// NULs, which is indistinguishable from real text at this level.
#[test]
fn a_short_source_is_refused_rather_than_zero_filled() {
    let text = doc(3);
    let mut rope = Rope::from_text(&text).expect("load");
    let target = 1;
    rope.evict_leaf(target).expect("evict");

    let at = rope.leaf_offset(target);
    let n = rope.leaf_len_of(target);
    let mut src = VecSource::new(text.clone());
    src.truncate_to = Some(4);
    let mut buf = vec![0u8; n];
    let err = rope
        .read_at_faulting(&mut src, at, n, &mut buf)
        .expect_err("a short source must be refused");
    assert!(matches!(err, RopeError::OutOfBounds { .. }), "got {err:?}");
}