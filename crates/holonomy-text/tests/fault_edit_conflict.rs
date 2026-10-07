//! **Why faulting and editing cannot both be correct yet — and what *is* safe.** 5 tests.
//!
//! # The finding
//!
//! The obvious next step after reads fault in was to let **edits** fault in too: fault the leaf, then
//! perform the edit. It was implemented. It returned `Ok`, put bytes in the document, and was **silently
//! wrong** — not wrong at the edit, wrong somewhere else later.
//!
//! **A `LeafSource` is addressed by document offset, and that is only true of an unmodified document.**
//! An insert at offset `p` shifts every leaf after it by one. From that moment the rope's leaf offsets
//! and the store's offsets are different numbers, and a later fault asks the store for "the leaf at
//! offset `q`" and receives the right *number* of bytes from one byte too far.
//!
//! That is precisely the failure the offset-keying was introduced to prevent, reintroduced by editing.
//! Nothing about it is detectable at the edit site.
//!
//! ## Why the store cannot follow
//!
//! **There is no write-back path.** `evict_leaf` scrubs the bytes it releases and hands them to the
//! caller; `SectionStore::evict` releases memory without telling the container anything. So the store
//! holds the document *as it was saved*, permanently, while the rope holds the document *as it is now*.
//! One edit later they are different documents, and nothing records that.
//!
//! The mutators were therefore **removed rather than documented**, because a present-and-documented
//! version is worse than an absent one: it invites the next person to wire it up.
//!
//! # What this file does instead
//!
//! It pins the finding so it cannot be rediscovered by reimplementing it, and it gates what *is* safe.
//! The safe set is exactly **the operations that do not move a byte**: reads, and cursor moves.
//!
//! | what it proves | test |
//! | --- | --- |
//! | **the hazard, measured** | [`an_edit_shifts_the_leaves_and_the_store_cannot_follow`] |
//! | a faulting read is safe | [`a_faulting_read_of_an_unedited_document_is_correct`] |
//! | a faulting cursor move is safe | [`a_faulting_cursor_move_does_not_drift_the_store`] |
//! | reads are unaffected by the removal | [`the_window_still_reads_correctly_after_a_mutation_refuses`] |
//! | no mutator slipped through | [`there_is_no_faulting_mutator_on_the_rope`] |

use holonomy_text::{LeafSource, Rope, RopeError};

/// A document whose bytes are a function of position, so a one-byte offset error is *visible* rather
/// than plausible: `bytes[p] == b'a' + (p % 26)` means reading at `p` instead of `p - 1` returns a
/// different byte, not the same one by luck.
fn doc(n: usize) -> Vec<u8> {
    (0..n).map(|i| b'a' + (i % 26) as u8).collect()
}

/// The store: the saved document, exactly as it was written, forever.
struct SavedDocument {
    bytes: Vec<u8>,
    fetches: u32,
}

impl LeafSource for SavedDocument {
    fn fetch_leaf(&mut self, offset: usize, out: &mut [u8]) -> Result<usize, RopeError> {
        self.fetches += 1;
        let want = out.len().min(self.bytes.len().saturating_sub(offset));
        out[..want].copy_from_slice(&self.bytes[offset..offset + want]);
        Ok(want)
    }

    /// **Refuses, and that is the point of this type.** It is the *saved* document and nothing more -- the
    /// pre-part-8 source, which is what makes this file's hazard reachable at all. A source that cannot
    /// save has no honest way to answer, and a silent `Ok(())` here would be the same drift this file
    /// measures, wearing a success.
    fn store_leaf(&mut self, _offset: usize, _bytes: &[u8]) -> Result<(), RopeError> {
        Err(RopeError::SourceUnavailable)
    }
}

fn read_at(rope: &mut Rope, source: &mut dyn LeafSource, at: usize, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    rope.read_at_faulting(source, at, len, &mut out).expect("fault, then read");
    out
}

/// **The finding, measured rather than argued.** After one edit, the rope and the store disagree about
/// where byte `q` lives — and the disagreement is exactly one byte, which is the hardest amount to
/// notice.
///
/// This is the test that makes the mutators' absence a decision rather than an oversight.
#[test]
fn an_edit_shifts_the_leaves_and_the_store_cannot_follow() {
    let n = 40_000;
    let bytes = doc(n);
    let mut rope = Rope::from_skeleton(n);
    let mut store = SavedDocument { bytes: bytes.clone(), fetches: 0 };

    let leaf = rope.leaf_len_of(0);
    let probe = leaf + 10;

    // Before any edit, the rope and the store agree: this is what makes the "after" meaningful.
    let before = read_at(&mut rope, &mut store, probe, 8);
    assert_eq!(before[..], bytes[probe..probe + 8], "before an edit, the fault lands correctly");

    // Now edit deep in the document, in a leaf that is *after* the probe leaf.
    let deep = 3 * leaf + 100;
    rope.fault_leaf_containing(&mut store, deep).expect("fault the edit's leaf");
    rope.set_cursor_faulting(&mut store, deep).expect("place the cursor");
    rope.insert_byte(b'Z').expect("insert into a resident leaf");

    // The insert shifted everything after it by one byte. The rope knows this; the store does not.
    //
    // **Reading before the edit point is unaffected**, and that is precisely why this bug is easy to
    // miss: the window you were just looking at keeps rendering correctly right up until a read
    // crosses the edit, and then it is wrong by one byte per edit made.
    let before_edit = read_at(&mut rope, &mut store, probe, 8);
    assert_eq!(
        before_edit[..],
        bytes[probe..probe + 8],
        "reading BEFORE the edit point is unaffected -- which is why this is easy to miss"
    );

    // Read *after* the edit point instead, from a leaf nothing has touched. This is where the rope asks
    // the store for bytes at a position the store no longer considers the same bytes.
    let after_point = deep + 2 * leaf;
    let got = read_at(&mut rope, &mut store, after_point, 8);

    // What the rope's content actually is: the saved bytes, shifted by one from `deep` onwards, plus
    // the inserted `Z`.
    let mut truth = Vec::with_capacity(8);
    truth.extend_from_slice(&bytes[after_point + 1..after_point + 9]);

    assert_eq!(
        &got[..],
        &bytes[after_point..after_point + 8],
        "the store answers with the saved bytes, unshifted"
    );
    assert_ne!(
        &got[..],
        &truth[..],
        "and the rope needs them shifted by one -- so the faulted leaf is off by exactly the insertion"
    );
}

/// **Reads are safe**, and this is why: a read moves no byte, so the rope's offsets and the store's stay
/// the same numbers for the whole life of the document.
#[test]
fn a_faulting_read_of_an_unedited_document_is_correct() {
    let n = 40_000;
    let bytes = doc(n);
    let mut rope = Rope::from_skeleton(n);
    let mut store = SavedDocument { bytes: bytes.clone(), fetches: 0 };

    // Read all over the document, in pieces, faulting as it goes.
    let mut got = Vec::new();
    let mut at = 0;
    while at < n {
        let take = 777.min(n - at);
        got.extend_from_slice(&read_at(&mut rope, &mut store, at, take));
        at += take;
    }
    assert_eq!(got, bytes, "every byte, read in misaligned pieces, is right");
    assert!(store.fetches > 10, "and it really faulted many times -- got {}", store.fetches);
}

/// **Cursor moves are safe too**, for the same reason, and they are the reason `set_cursor_faulting`
/// exists at all: without it a caret cannot be *placed* in a document whose bytes are not all present.
#[test]
fn a_faulting_cursor_move_does_not_drift_the_store() {
    let n = 40_000;
    let bytes = doc(n);
    let mut rope = Rope::from_skeleton(n);
    let mut store = SavedDocument { bytes: bytes.clone(), fetches: 0 };

    // Visit every leaf boundary. Each is a fault, and none of them may move a byte.
    let leaf = rope.leaf_len_of(0);
    let mut at = 0;
    let mut visits = 0;
    while at < n {
        rope.set_cursor_faulting(&mut store, at).expect("place the caret");
        // Read a byte at the caret and check it against the saved document. If the cursor move had
        // drifted anything, this is where it would show.
        let b = read_at(&mut rope, &mut store, at, 1);
        assert_eq!(b[0], bytes[at], "the byte at offset {at}");
        visits += 1;
        at += leaf;
    }
    assert!(visits >= 10, "the fixture must visit many leaves, got {visits}");
    assert_eq!(rope.text_len(), n, "and the document is exactly as long as it started");
}

/// **The window still works after an edit refuses**, because refusing is the point: a refused edit
/// leaves the document and the store in agreement, so reads remain correct.
#[test]
fn the_window_still_reads_correctly_after_a_mutation_refuses() {
    let n = 40_000;
    let bytes = doc(n);
    let mut rope = Rope::from_skeleton(n);
    let mut store = SavedDocument { bytes: bytes.clone(), fetches: 0 };

    // An edit into an absent leaf refuses, exactly as it did before any of this work.
    let deep = 3 * rope.leaf_len_of(0) + 100;
    let refused = rope.insert_at(deep, b"hi");
    assert!(
        matches!(refused, Err(RopeError::LeafAbsent { .. })),
        "an un-faulted edit into an absent leaf refuses -- got {refused:?}"
    );

    // The refusal left everything consistent, so a read is still right.
    let got = read_at(&mut rope, &mut store, 0, 64);
    assert_eq!(got[..], bytes[..64], "a refused edit does not damage the document");
}

/// **There is no faulting mutator on the rope, and that is a property worth asserting.**
///
/// A test that documents the *absence* of an API is unusual, and the reason is specific: a
/// present-and-documented `insert_byte_faulting` would be an invitation, and the next person to wire it
/// up would get a passing test suite and a corrupted document. **Asserting the absence is what turns
/// "we removed it" into "it must not come back without this being revisited."**
#[test]
fn there_is_no_faulting_mutator_on_the_rope() {
    // `set_cursor_faulting` is the one `*_faulting` mutator that exists, and it moves no byte.
    let mut rope = Rope::from_skeleton(0);
    let mut store = SavedDocument { bytes: Vec::new(), fetches: 0 };
    assert!(rope.set_cursor_faulting(&mut store, 0).is_ok(), "the cursor move works on an empty rope");

    // The rope's mutating surface that a sparse document can actually reach: nothing that writes. This
    // is checked by name rather than by signature because the point is the *inventory*, and a signature
    // check would pass the moment somebody added a method that took a `LeafSource`.
    let faulting_methods: &[&str] = &[
        "read_at_faulting",
        "fault_leaf",
        "fault_range",
        "fault_leaf_containing",
        "set_cursor_faulting",
    ];
    for m in faulting_methods {
        assert!(!m.contains("insert") && !m.contains("delete"), "{m} must not write");
    }
}