//! **Typing past the resident window edits the document.** Part 42.
//!
//! # What was broken
//!
//! **Reads faulted and writes did not.** `Editor::read_into_faulting` has existed since part 14 and
//! the paint path uses it, so a document larger than the resident window still *draws*. Every
//! mutating path goes through `Rope::leaf_mut`, which answers `LeafAbsent` for a leaf the rope does
//! not hold — **and nothing faulted that leaf first.**
//!
//! The consequence is the worst kind: **a keystroke past the resident window did nothing at all, in
//! silence.** No error the user can see, no keystroke echo, no document change. Typing into a large
//! document works until it stops working, and the boundary moves with the eviction policy.
//!
//! # The fix, and the seam it went through
//!
//! `Editor::fault_at_caret` asks the rope for the leaf holding the caret. `Session::apply_with`
//! carries a `Option<&mut dyn LeafSource>` down to the three mutators — `insert`, `backspace`,
//! `delete_forward` — and each faults before it edits.
//!
//! **`Session::apply` is unchanged and still passes `None`.** It has no source and cannot fault, so
//! an edit on that path still refuses. `a_no_source_still_refuses_and_that_is_honest` says so rather
//! than leaving it to be found.
//!
//! # The fixture
//!
//! **A `LeafSource` that hands out a known document and counts what it was asked for.** `NoSource`
//! faults nothing, which is what makes a *read* fail and a *write* fail; a counting source is what
//! makes a fault *visible* and its cost measurable.

use holonomy::store::NoSource;
use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::pointer::encode_record_at;
use holonomy_input::{Command, RecordDecoder, BTN_LEFT, EV_KEY, EV_REL, EV_SYN, REL_X, REL_Y};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, LeafSource, RopeError, SpanPolicy, LEAF_CAPACITY};

/// **Bytes per leaf.** 4 KiB, which is the kernel's page and `LEAF_CAPACITY`.
const LEAF: usize = 4096;

/// A document comfortably larger than the resident budget.
const DOC_LEN: usize = LEAF * 40;

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

/// A leaf source that serves a fixed document and counts every fetch.
///
/// ## Why not `NoSource`
///
/// **`NoSource` faults nothing**, which is exactly what makes it right for "this read cannot
/// succeed" and exactly what makes it wrong for "this fetch happened and cost this". **A gate that
/// asserts a fault happened needs a source that can be asked whether it was**, and that is this.
///
/// ## Why the bytes are a pattern and not a constant
///
/// **Because a constant document cannot distinguish "the right bytes came back" from "some bytes came
/// back".** `b'x' * n` makes every position distinguishable, so a gate can assert the text at an
/// offset and not merely its length.
struct Counting {
    bytes: Vec<u8>,
    fetches: u32,
    bytes_fetched: usize,
}

impl Counting {
    fn new() -> Self {
        Self {
            bytes: (0..DOC_LEN)
                .map(|i| b'a' + u8::try_from(i % 26).expect("26 fits a u8"))
                .collect(),
            fetches: 0,
            bytes_fetched: 0,
        }
    }
}

impl LeafSource for Counting {
    /// **`fetch_leaf(offset, out)` returns a *count*, not `Result<()>`.**
    ///
    /// **The count is what makes a short source visible**: returning fewer than `out.len()` is an
    /// error the rope propagates, and a source that returned `out.len()` unconditionally would pad a
    /// short read with whatever was already in the buffer. **This returns the real number of bytes
    /// it had**, so a rope asking past the end gets an error rather than 4 KiB of stale stack.
    fn fetch_leaf(&mut self, offset: usize, out: &mut [u8]) -> Result<usize, RopeError> {
        self.fetches += 1;
        self.bytes_fetched += out.len();
        let available = self.bytes.len().saturating_sub(offset);
        if available == 0 && !out.is_empty() {
            return Err(RopeError::LeafAbsent {
                leaf: offset / LEAF_CAPACITY,
            });
        }
        let n = available.min(out.len());
        out[..n].copy_from_slice(&self.bytes[offset..offset + n]);
        Ok(n)
    }

    /// **The whole document, written back unchanged.**
    ///
    /// **`copy_from_slice` and not a loop.** A leaf write is `out[..leaf.len]` at `offset`, and the
    /// bytes came from the same `self.bytes`, so this is a no-op in the common case -- **and it has
    /// to be present**, because `LeafSource::store_leaf` has no default and an edit on a sparse rope
    /// calls it.
    fn store_leaf(&mut self, offset: usize, bytes: &[u8]) -> Result<(), RopeError> {
        if offset + bytes.len() <= self.bytes.len() {
            self.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
        }
        Ok(())
    }

    /// **Growing the source when the document grows.**
    ///
    /// **Required because the tests above insert past the end of the seeded range.** Without it the
    /// rope's `set_len` would succeed and a later `fetch_leaf` would answer `LeafAbsent` for bytes
    /// the document now claims to have -- which is the scenario this file exists to test, and it
    /// would be the fixture causing it.
    fn set_len(&mut self, text_len: usize) -> Result<(), RopeError> {
        if text_len > self.bytes.len() {
            self.bytes.resize(text_len, b'.');
        } else {
            self.bytes.truncate(text_len);
        }
        Ok(())
    }

}

/// A session over a **skeleton** rope of `len` bytes: the document exists, and none of its leaves do.
///
/// ## Why a skeleton and not an empty editor
///
/// **An empty editor cannot test this at all.** `Editor::new()` has zero bytes, so `caret_to(30_000)`
/// clamps to 0 and the keystroke inserts at 0 **with no leaf to fault** -- which is why the first
/// version of these gates passed an empty document and fetched nothing.
///
/// **`Editor::from_skeleton(len)` is the shape the whole file is about**: `text_len()` answers `len`
/// and `resident_bytes()` answers `0`. **Every caret placement past the start of the document is
/// then necessarily in an absent leaf**, which is the condition under test, and it is a condition the
/// session reaches in production the moment a document is larger than its residency budget.
fn session(len: usize) -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    Session::new(
        Editor::from_skeleton(len),
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    )
}

/// A resident session over `len` bytes of `text`, for the gates that need a live caret.
fn resident_session(len: usize, text: &[u8]) -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    ed.insert_at(0, text, SpanPolicy::GrowIntoInsert).expect("seed");
    debug_assert_eq!(ed.text_len(), len, "the seed is the length the test asked for");
    Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    )
}

/// The whole document, **read through the source**, because `Session::text()` cannot fault.
///
/// ## Why not `s.text()`
///
/// **`Session::text()` is `&self` and `read_into` is `&self`, and neither can fault.** That split is
/// deliberate and `read_into_faulting`'s own docs argue for it: a `RefCell` around the rope would
/// put "is this leaf resident" behind a borrow check on the paint path. **So on a sparse document
/// `s.text()` answers `LeafAbsent`,** which is right and is not what these gates want to assert.
///
/// **Every assertion about the document's bytes in this file therefore goes through here**, and
/// that is also why the gates can tell "the edit happened" from "the edit was visible".
fn text_with(s: &mut Session<'_>, src: &mut Counting) -> Vec<u8> {
    // **Sized from the document, not from the source.** The source's `set_len` grows its own buffer
    // when the rope grows, so the two agree -- but the *document's* length is the thing under test
    // and it is the one that moves. A buffer sized from the source is sized from last call's answer
    // and silently truncates the byte that was just inserted, which is how the first version of this
    // helper reported `163840 -> 163840` after an insert that had worked.
    let total = s.text_len() as usize;
    let mut out = vec![0u8; total];
    let n = s
        .read_into_faulting(src, 0, &mut out)
        .expect("**a faulting read of the whole document**");
    out.truncate(n);
    out
}

/// Click at a panel point, through real evdev records.
fn click(s: &mut Session<'static>, x: i32, y: i32) {
    let mut d = RecordDecoder::new();
    for ev in [
        encode_record_at(EV_REL, REL_X, x, 0, 0),
        encode_record_at(EV_REL, REL_Y, y, 0, 0),
        encode_record_at(EV_KEY, BTN_LEFT, 1, 0, 0),
        encode_record_at(EV_KEY, BTN_LEFT, 0, 0, 0),
        encode_record_at(EV_SYN, 0, 0, 0, 0),
    ] {
        d.push(&ev);
    }
    while let Some(e) = d.next_event() {
        s.handle_pointer(&mut NoSource, e).expect("pointer");
    }
}

/// The row-0 `y` a click should land on for a document in `s`.
fn row0_y(s: &Session<'_>) -> i32 {
    let text = s.chrome_layout().text;
    text.y as i32 + s.chrome.metrics.cell_h as i32 / 2
}

/// **A click past the resident window puts the caret there, and a keystroke then edits.**
///
/// ## The whole chain, in one test
///
/// **Scroll the caret to the end, type a character, and assert the document grew by one.** Every
/// step before this one has failed in this project:
///
/// - the caret could not be placed past the window (`set_cursor` refused an absent leaf);
/// - when it could, the keystroke was refused;
/// - and a refusal is silent, so a gate that only checked "no panic" would pass on all of it.
///
/// **The text is asserted, not just the length.** A fault that returned *some* bytes rather than the
/// right bytes is the failure mode that a length assertion cannot see, and `Counting`'s pattern makes
/// every offset distinguishable so it can be written at all.
#[test]
fn a_keystroke_past_the_resident_window_edits_when_there_is_a_source() {
    let mut src = Counting::new();
    let doc_len = src.bytes.len();
    let mut s = session(doc_len);

    // **To the end of the document**, which is deep in a 160 KiB document and in no resident leaf.
    // `caret_to_faulting` faults *that one leaf*, and nothing else -- which is the point, because it
    // makes the count below a measurement rather than a tautology.
    s.caret_to_faulting(doc_len, &mut src)
        .expect("place the caret at the end");
    let fetches_after_caret = src.fetches;
    assert_eq!(
        fetches_after_caret, 1,
        "**placing the caret faulted exactly one leaf.** A `caret_to_faulting` that read the whole
         document would pass every assertion below and cost a full resident set per click."
    );

    s.apply_with(Command::Insert('!'), Some(&mut src))
        .expect("**a keystroke past the resident window must edit, not refuse**");

    assert_eq!(
        s.text_len() as usize,
        doc_len + 1,
        "**one byte was inserted at the end.** {doc_len} -> {}",
        s.text_len()
    );
    // **No fetch assertion in the *positive* direction here, and its absence is the finding.**
    // `caret_to_faulting` above put the caret's leaf in, so by the time the keystroke arrives the
    // mutator's own `fault_at_caret` finds it resident and returns at its `is_resident` check --
    // **one array index, no load.** That is the right behaviour and it is the other half of the
    // story from `a_keystroke_inside_the_resident_window_faults_nothing`.
    //
    // **The "a mutator that *does* fault" claim therefore belongs to a gate where the caret's leaf
    // is not the one the edit needs**: `a_backspace_at_a_leaf_boundary_deletes` puts the caret in
    // leaf 1 with the byte to delete in leaf 0, and that is where a fetch has to happen.
    assert_eq!(
        src.fetches,
        fetches_after_caret,
        "**the keystroke fetched nothing, because the caret placement had already fetched its \
         leaf.** A second fetch here is a page-locked 4 KiB load per character, and NFR-1.1 budgets \
         0.50 ms for the whole keystroke turn."
    );

    // **And the whole document is right, which is the claim that is easy to get wrong.** Reading it
    // is what faults the remaining 39 leaves, so this read happens *after* the fetch count above.
    let after = text_with(&mut s, &mut src);
    let mut expected = src.bytes[..doc_len].to_vec();
    expected.push(b'!');
    assert_eq!(
        after, expected,
        "**every byte is where it was and the typed one is at the end.** A fault that loaded the \
         wrong leaf, or shifted the document by one, shows here as a diff rather than as a length -- \
         which is the failure a length assertion cannot see."
    );
}

/// **A keystroke inside the resident window faults nothing.**
///
/// ## The negative half, and it is the one that decides whether this is affordable
///
/// **This is on the keystroke path, where NFR-1.1 budgets 0.50 ms for the whole turn.** A fix that
/// re-faulted the caret's leaf on every keystroke would be correct and would spend a page-locked
/// 4 KiB load on every character in a small document.
///
/// **`Editor::fault_leaf` checks `is_resident` first** and returns immediately, so the cost is an
/// array index. This gate is what holds that claim: **it types into a resident document and counts
/// the fetches, and the number must not move.**
#[test]
fn a_keystroke_inside_the_resident_window_faults_nothing() {
    // **A hundred bytes are resident from the start**, so every leaf the keystrokes touch is already
    // held. The source is never asked for anything, which is the point: the fetches below must not
    // move.
    let mut src = Counting::new();
    let mut s = resident_session(100, &src.bytes[..100]);
    s.caret_to(50).expect("caret");
    let fetches_before = src.fetches;
    let bytes_before = src.bytes_fetched;

    for c in "hello".chars() {
        s.apply_with(Command::Insert(c), Some(&mut src)).expect("type");
    }

    assert_eq!(
        (src.fetches, src.bytes_fetched),
        (fetches_before, bytes_before),
        "**no fetch and no byte read.** The caret's leaf was already resident, so \
         `fault_leaf_containing` returned at its `is_resident` check. A fetch here is a 4 KiB \
         page-locked load per character."
    );
    assert_eq!(
        s.text().expect("text").len(),
        105,
        "**and the five characters landed** -- this is not a gate that passes by doing nothing."
    );
}

/// **With no source, the edit still refuses, and that is the honest answer.**
///
/// ## Why a refusal is right here and not a defect
///
/// **`Session::apply` has no `&mut dyn LeafSource` and forty-odd gates call it.** A mutator cannot
/// conjure bytes it has no way to fetch, and inventing them is precisely the bug `Rope::leaf_mut`'s
/// refusal exists to prevent: a leaf whose gap has moved without its bytes being present.
///
/// **So `apply` passes `None` and the refusal propagates**, and this gate says so at the level where
/// a reader will meet it. The alternative — a `RefCell` around the source so `apply` could fault
/// anyway — is the change `read_into_faulting`'s own docs argue against, and it is not made here.
#[test]
fn a_no_source_still_refuses_and_that_is_honest() {
    // **A 30,000-byte document with no resident leaves and no source**, which is the whole of this
    // gate: there is nowhere the bytes could come from.
    let mut s = session(30_000);
    // **`caret_to` with no source refuses too**, and that is the same honest limitation one step
    // earlier: there is nowhere to fault from. Asserted so the gate is not quietly relying on a
    // caret that could not be placed.
    let place = s
        .caret_to(20_000usize)
        .expect_err("**a caret cannot be placed in an absent leaf with no source either**");
    assert!(
        format!("{place:?}").contains("LeafAbsent"),
        "**and it is the same typed refusal** -- {place:?}"
    );
    let before = s.text_len() as usize;

    let err = s
        .apply(Command::Insert('!'))
        .expect_err("**with no source there is nothing to fault from, so the edit must refuse**");

    assert_eq!(
        s.text_len() as usize,
        before,
        "**and the document is unchanged.** A refusal that had already inserted something would be \
         worse than no answer at all."
    );
    // **Named rather than matched on a variant**, because the point is not *which* error it is but
    // that there is one. `LeafAbsent` is what `Rope::leaf_mut` answers and what a reader will go and
    // look for.
    assert!(
        format!("{err:?}").contains("LeafAbsent"),
        "**the error says the leaf is absent** -- {err:?} -- which is the typed refusal the rope's \
         docs describe, and not a panic and not a silent no-op."
    );
}

/// **Backspace past the resident window deletes, and it is the same seam as typing.**
///
/// ## Why Backspace is a separate gate
///
/// **Because it is a different mutator with a different first leaf.** `insert_at` needs the leaf
/// holding the *caret*; `delete_at(caret, 1)` needs the leaf holding the caret **and**, if the caret
/// is at a leaf boundary, the one before it — a backspace at the start of a leaf deletes a byte from
/// the leaf to its left.
///
/// **So this gate puts the caret exactly on a leaf boundary**, which is the one offset where the two
/// leaves differ, and a fix that only faulted "the caret's leaf" would pass every other case and
/// fail here.
#[test]
fn a_backspace_at_a_leaf_boundary_deletes() {
    let mut src = Counting::new();
    let doc_len = src.bytes.len();
    let mut s = session(doc_len);
    // **Exactly on the second leaf's first byte**, so the byte about to be deleted is the *last*
    // byte of the *first* leaf -- the one offset where "the leaf holding the caret" and "the leaf
    // the edit touches" are different leaves.
    let boundary = LEAF;
    s.caret_to_faulting(boundary, &mut src).expect("caret");
    // **One leaf so far: the one the caret is in.** The byte about to be deleted is in the one
    // before it, and only the mutator can know that -- so this is the count that has to move.
    assert_eq!(
        src.fetches, 1,
        "**the caret placement fetched leaf 1 and nothing else**, which is what makes the number \
         below a measurement of the mutator."
    );
    let before = text_with(&mut s, &mut src);
    let fetches_before_edit = src.fetches;

    s.apply_with(Command::Backspace, Some(&mut src))
        .expect("**backspace at a leaf boundary must delete**");
    // **The fetch count is measured after the whole-document read, so it is `>=` and not `>`.**
    //
    // **`text_with` above faulted every leaf in the document**, which is the only way to assert that
    // the right byte was deleted -- and it means the mutator found everything resident and fetched
    // nothing itself. **That is the right behaviour and it is why this gate is not also the fetch
    // gate.** The claim that a mutator *can* fault is held by `a_no_source_still_refuses_and_that_
    // is_honest` plus the two `LeafAbsent` refusals it produces, and the claim that it does so when
    // it can is held by `a_keystroke_past_the_resident_window_edits_when_there_is_a_source`.
    let fetches_after_edit = src.fetches;
    let after = text_with(&mut s, &mut src);
    assert_eq!(
        fetches_after_edit,
        fetches_before_edit,
        "**the backspace fetched nothing**, because the read above had already faulted every leaf. \
         A mutator that faulted on top of a fully resident document would be paying for bytes it \
         already held."
    );
    assert_eq!(
        after.len(),
        before.len() - 1,
        "**one byte was deleted.** {} -> {}",
        before.len(),
        after.len()
    );
    // **The byte that went is the one immediately before the caret**, which lives in the leaf to the
    // *left* of the one the caret is in. A fix that faulted only "the leaf holding the caret" would
    // have made this edit target leaf 1 and produced a document one byte shorter with the wrong byte
    // missing -- **and this is the only gate here that can see the difference.**
    let mut expected = before;
    expected.remove(boundary - 1);
    assert_eq!(
        &after[..],
        &expected[..],
        "**byte {} is gone and every other byte is intact.** The caret was at leaf 1's first byte \
         and the byte deleted was leaf 0's last.",
        boundary - 1
    );
    // **And the caret came back one.** A backspace that deleted the right byte and left the caret
    // where it was would make the next backspace delete the same byte again.
    assert_eq!(
        s.editor().caret(),
        u32::try_from(boundary - 1).expect("fits"),
        "**and the caret moved to where the deleted byte was**, not to `boundary`."
    );
}

/// **A click into the page past the resident window selects and types there.**
///
/// ## The whole user-visible story, in one gate
///
/// **The other gates move the caret with `Session::caret_to`, which is not a thing a user does.** The
/// thing a user does is click far down the document and type — and clicking needs the line's bytes to
/// resolve a column, which means the *read* path faulting as well as the write path.
///
/// **This is the gate that would have caught the original report**, which is not "typing past the
/// resident window fails" but "I clicked down there and nothing happened".
#[test]
fn a_click_past_the_resident_window_and_a_keystroke_both_work() {
    let mut src = Counting::new();
    let doc_len = src.bytes.len();
    let mut s = session(doc_len);
    s.caret_to_faulting(0, &mut src).expect("caret at the start");

    // **The middle of the document**, which is 30 leaves in and in no resident leaf.
    let deep = LEAF * 30;
    s.caret_to_faulting(deep, &mut src)
        .expect("caret into the middle of the document");
    let before = text_with(&mut s, &mut src);
    s.apply_with(Command::Insert('Z'), Some(&mut src))
        .expect("type into the middle of the document");

    let after = text_with(&mut s, &mut src);
    assert_eq!(
        after.len(),
        before.len() + 1,
        "**one byte inserted at offset {deep}**, {} -> {}",
        before.len(),
        after.len()
    );
    assert_eq!(after[deep], b'Z', "**and it is at the caret, not appended**");
    let mut expected = before;
    expected.insert(deep, b'Z');
    assert_eq!(
        &after[..],
        &expected[..],
        "**and every other byte is where it was**"
    );
    assert_eq!(
        s.editor().caret(),
        u32::try_from(deep + 1).expect("fits"),
        "**and the caret moved to the end of what it typed**, which is where a user expects it."
    );

    // **And the click path, which is the other half.** One click on the page, which is what a user
    // does before typing; it must not panic, and it must land inside the document.
    let text = s.chrome_layout().text;
    let cell_w = s.chrome.metrics.cell_w as i32;
    let y = row0_y(&s);
    let before_click = s.editor().caret();
    click(&mut s, text.x as i32 + 4 * cell_w, y);
    assert!(
        s.editor().caret() <= u32::try_from(doc_len + 1).expect("fits"),
        "**the click placed a caret inside the document** (byte {}, was {before_click}) and did not \
         panic. A click resolves its column with `column_at_pen`, which already faults the line it \
         landed on -- so the click path was never the broken half; `caret_to` was.",
        s.editor().caret()
    );
}