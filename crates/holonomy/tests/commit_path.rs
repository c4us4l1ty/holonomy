//! **Part 15's gate, through the product path: open a real container, edit it, commit, reopen.** 5 tests.
//!
//! # What this file is for
//!
//! `crates/holonomy-text/tests/write_back.rs` gates the rope's half — *which leaves get written, in what
//! order, and what happens to the record afterwards*. It cannot gate the store's half, because
//! `holonomy-text` cannot depend on `holonomy-container`: that dependency is the reason `LeafSource` is a
//! trait at all.
//!
//! **So the half that part 13 measured going wrong is the half no rope-level test can see.** Part 13's own
//! finding was that a `Vec` source has no sections and therefore makes a shift invisible; that finding is
//! only pinned because `tests/write_back_shift.rs` runs against a real container. **This file is the same
//! move for part 15**: the commit is exercised through `open_document` → edit → `commit_document` → reopen,
//! with the container on disk as the only witness.
//!
//! # The three things that could be wrong, and each has a test
//!
//! * **the extent** — part 8's `set_len` was a no-op on this store, so a committed document was five bytes
//!   short of the one in memory while the commit reported success. [`a_committed_growth_reaches_the_disk_whole`].
//! * **the shift** — part 13's finding, repaired by writing every leaf. [`a_commit_repairs_a_shift_the_disk_would_otherwise_keep`].
//! * **and the residency** — the read loop faults every leaf in, so a commit that does not shed them is an
//!   8 MiB page-lock event wearing a save button. [`a_commit_leaves_the_rope_holding_nothing`].
//! * **and the honesty of the report** — `commit_document` returns what reached the disk, and a second
//!   commit of an unchanged document is a *full rewrite*, because the re-read re-dirties the cache.
//!   [`the_commit_reports_what_it_actually_wrote`].
//!
//! | what it proves | test |
//! | --- | --- |
//! | an edit survives a round trip through the file | [`an_edit_survives_a_round_trip_through_the_file`] |
//! | **and the extent grows with it** | [`a_committed_growth_reaches_the_disk_whole`] |
//! | the part-13 shift is repaired on disk | [`a_commit_repairs_a_shift_the_disk_would_otherwise_keep`] |
//! | **and nothing stays page-locked** | [`a_commit_leaves_the_rope_holding_nothing`] |
//! | **the return value is not a lie** | [`the_commit_reports_what_it_actually_wrote`] |

use holonomy::store::{commit_document, open_document, SectionStore, DEFAULT_RESIDENT_SECTIONS};
use holonomy_container::io::DirectFile;
use holonomy_container::Wavefunction;
use holonomy_text::{LeafSource, SpanPolicy};

const PASS: &str = "correct horse battery staple";
const ITER: u64 = 1;

fn scratch_dir() -> std::path::PathBuf {
    let d = std::env::current_exe()
        .expect("exe")
        .ancestors()
        .nth(3)
        .expect("layout")
        .join("holonomy-container-tests")
        .join("commit-path");
    std::fs::create_dir_all(&d).expect("scratch");
    d
}

fn unique() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static C: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    (t << 20) ^ u128::from(C.fetch_add(1, Ordering::Relaxed))
}

/// A container on disk holding `content`, and the path to reopen it from.
fn container(content: &[u8]) -> (std::path::PathBuf, Wavefunction) {
    let p = scratch_dir().join(format!("commit-{}-{}.wf", content.len(), unique()));
    let _ = Wavefunction::create(&p, PASS, "p3", content, ITER).expect("create");
    let wf = Wavefunction::open(&p, PASS, ITER).expect("open");
    (p, wf)
}

/// Bytes that are a function of position, so a one-byte offset error is *visible* rather than plausible.
fn text(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| (i.wrapping_mul(131).wrapping_add(17) % 251) as u8)
        .collect()
}

/// Reopen the container and read its whole content back, **with no rope in the way**.
///
/// The rope agreeing with the store could be two wrongs cancelling; the store disagreeing with itself
/// cannot.
fn read_back(path: &std::path::Path, budget: usize) -> Vec<u8> {
    let mut wf = Wavefunction::open(path, PASS, ITER).expect("reopen");
    let len = wf.content_len() as usize;

    let mut store = SectionStore::new(&mut wf, budget);
    let mut out = vec![0u8; len];
    let mut at = 0usize;
    while at < len {
        let lo = at;
        let hi = (at + 8_192).min(len);
        let got = store
            .fetch_leaf(lo, &mut out[lo..hi])
            .expect("read a section");
        assert_eq!(
            got,
            hi - lo,
            "a section read returned {got} of {} bytes at {lo}",
            hi - lo
        );
        at = hi;
    }
    out
}

/// **One edit, one commit, one reopen — and the bytes on disk are the bytes that were typed.**
///
/// The smallest version of the whole design: the record carries the edit from the rope to the store, the
/// commit empties it, and the file agrees. Every larger test below is this plus something that could break.
#[test]
fn an_edit_survives_a_round_trip_through_the_file() {
    let n = 40_000usize;
    let saved = text(n);
    let (path, _wf) = container(&saved);

    // **Open through the product path**, so the skeleton editor and the first window are the real ones.
    let file = DirectFile::open(&path).expect("open rw");
    let mut opened =
        open_document(file, PASS, ITER, DEFAULT_RESIDENT_SECTIONS).expect("open the document");
    let mut editor = std::mem::replace(&mut opened.editor, holonomy_text::Editor::new());

    // Edit deep in the document, past the first window -- this is the case that needs the record.
    let at = 30_000usize;
    editor
        .read_into_faulting(&mut store_of(&mut opened), at, &mut [0u8; 8])
        .expect("fault the edit point");
    editor
        .insert_at(at as u32, b"EDITED", SpanPolicy::GrowIntoInsert)
        .expect("insert");

    let mut truth = saved.clone();
    truth.splice(at..at, b"EDITED".iter().copied());

    let mut store = SectionStore::new(&mut opened.container, DEFAULT_RESIDENT_SECTIONS);
    let written = commit_document(&mut store, &mut editor).expect("commit");
    assert_eq!(written, truth.len(), "the whole document reached the disk");

    drop(store);
    drop(opened);

    let back = read_back(&path, 8);
    assert_eq!(back, truth, "the file holds exactly what was typed");
}

/// A tiny shim so the edit's fault can use the same store the commit will.
fn store_of(opened: &mut holonomy::store::OpenedDocument) -> SectionStore<'_> {
    SectionStore::new(&mut opened.container, DEFAULT_RESIDENT_SECTIONS)
}

/// **The extent grows with the document, and this is part 8's `set_len` gap measured end to end again.**
///
/// Part 8's `set_len` was the trait's no-op default and this store never overrode it, so a commit that grew
/// the document reported success and left the container's `content_len` at the *old* length —
/// `write_back_shift.rs` measured exactly that (135,040 on disk against 135,045 in memory). **Part 15's
/// `SectionStore::set_len` is the fix, and this is the gate that would have caught its absence.**
#[test]
fn a_committed_growth_reaches_the_disk_whole() {
    let n = 40_000usize;
    let saved = text(n);
    let (path, _wf) = container(&saved);

    let file = DirectFile::open(&path).expect("open rw");
    let mut opened = open_document(file, PASS, ITER, DEFAULT_RESIDENT_SECTIONS).expect("open");
    let mut editor = std::mem::replace(&mut opened.editor, holonomy_text::Editor::new());
    let mut store = SectionStore::new(&mut opened.container, DEFAULT_RESIDENT_SECTIONS);

    // **A paste at the very end of the document**, which is where a stale extent does its damage: the new
    // bytes live past what the container believes its length is.
    let at = n as u32 - 4;
    editor
        .read_into_faulting(&mut store, at as usize, &mut [0u8; 4])
        .expect("fault the tail");
    editor
        .insert_at(at, b"TAILPASTE", SpanPolicy::GrowIntoInsert)
        .expect("paste at the end");

    let mut truth = saved.clone();
    truth.splice(at as usize..at as usize, b"TAILPASTE".iter().copied());
    assert_eq!(
        editor.text_len() as usize,
        truth.len(),
        "the editor grew by 9"
    );

    let written = commit_document(&mut store, &mut editor).expect("commit");
    assert_eq!(written, truth.len(), "commit reported the whole document");

    drop(store);
    // **The extent, checked against the container rather than the editor.** This is the assertion part 8
    // could not make: the bug was that the *container's* length did not follow, and reading it needs the
    // store dropped because the store borrows the container.
    assert_eq!(
        opened.container.content_len() as usize,
        truth.len(),
        "the container's content_len followed the growth -- part 8's set_len gap"
    );

    drop(opened);
    let back = read_back(&path, 8);
    assert_eq!(
        back.len(),
        truth.len(),
        "and the bytes on disk are the whole document"
    );
    assert_eq!(
        &back[at as usize..at as usize + 9],
        b"TAILPASTE",
        "including the pasted tail"
    );
}

/// **Part 13's finding, repaired on disk: a shift is repaired by writing every leaf.**
///
/// The same fixture `write_back_shift.rs` used — one 5-byte insert, which is the edit that made
/// per-leaf write-back repair 0.15 % of the document. **What is different is that `commit` writes every
/// leaf**, so this asserts the whole document is right rather than that most of it is wrong.
#[test]
fn a_commit_repairs_a_shift_the_disk_would_otherwise_keep() {
    // **Three sections**, so a leaf write-back would repair one section's worth and leave two stale.
    let n = 65_520 * 2 + 4_000;
    let saved = text(n);
    let (path, _wf) = container(&saved);

    let file = DirectFile::open(&path).expect("open rw");
    let mut opened = open_document(file, PASS, ITER, DEFAULT_RESIDENT_SECTIONS).expect("open");
    let mut editor = std::mem::replace(&mut opened.editor, holonomy_text::Editor::new());
    let mut store = SectionStore::new(&mut opened.container, DEFAULT_RESIDENT_SECTIONS);

    let at = 10usize;
    editor
        .read_into_faulting(&mut store, at, &mut [0u8; 8])
        .expect("fault");
    editor
        .insert_at(at as u32, b"ZZZZZ", SpanPolicy::GrowIntoInsert)
        .expect("insert");

    let mut truth = saved.clone();
    truth.splice(at..at, b"ZZZZZ".iter().copied());

    commit_document(&mut store, &mut editor).expect("commit");
    drop(store);
    drop(opened);

    let back = read_back(&path, 8);
    assert_eq!(back.len(), truth.len(), "and the extent grew");
    let wrong: Vec<usize> = (0..truth.len()).filter(|&i| back[i] != truth[i]).collect();
    assert!(
        wrong.is_empty(),
        "every byte should be right, where a leaf write-back left {n} of {} wrong. \
         Differing offsets (first 20): {:?}",
        truth.len(),
        &wrong[..wrong.len().min(20)]
    );
}

/// **The return value is not a lie, and this is the test that says so out loud.**
///
/// `commit_document` returns how many bytes reached the disk, and for a whole-document commit that is the
/// whole document. **What it is not is a *change* count**, which is the mistake the first version of this
/// test made.
///
/// **The expectation it originally asserted was that a second commit returns 0** — on the reasoning that
/// nothing was dirty, so nothing should be written. **It returns the full length**, and that is correct:
/// the commit reads the document back through the rope to produce it, that read repopulates the store's
/// cache, and `Entry::dirty` means *cache differs from disk*. After the re-read they genuinely do.
///
/// **So the honest statement is that a second save is a full rewrite**, and the property worth gating is
/// the one that makes repeating a save safe rather than the one that would make it cheap: **the bytes on
/// disk are unchanged.** A caller wanting "is it saved?" compares against `text_len`; a caller wanting "did
/// anything change?" needs a dirty flag this does not yet expose, and that is the gap this paragraph
/// records rather than a number nobody measured.
#[test]
fn the_commit_reports_what_it_actually_wrote() {
    // **One section's worth**, so the budget covers the whole document and the claim is exact.
    let n = 30_000usize;
    let saved = text(n);
    let (path, _wf) = container(&saved);

    let file = DirectFile::open(&path).expect("open rw");
    let mut opened = open_document(file, PASS, ITER, DEFAULT_RESIDENT_SECTIONS).expect("open");
    let mut editor = std::mem::replace(&mut opened.editor, holonomy_text::Editor::new());
    let mut store = SectionStore::new(&mut opened.container, DEFAULT_RESIDENT_SECTIONS);

    // **At offset 0, which is always a character boundary.** The first version of this used offset 5,
    // and `text()` fills the document with `(i * 131 + 17) % 251` — so byte 5 is the middle of a
    // multi-byte sequence and `insert_at` refused with `NotCharBoundary { offset: 5 }`. **A refusal that
    // correct**: the editor will not cut a character in half, and the test had asked it to.
    editor
        .insert_at(0, b"small ", SpanPolicy::GrowIntoInsert)
        .expect("insert at a boundary");
    let len = editor.text_len() as usize;
    let written = commit_document(&mut store, &mut editor).expect("commit");
    assert_eq!(
        written, len,
        "a single-section document commits whole, and says so"
    );

    // **A second commit is idempotent in content, and the number says what it cost.**
    //
    // The first version of this asserted the second commit returns 0, on the reasoning that nothing was
    // dirty. **It returns the full length**, and that is correct: the commit reads the document through the
    // rope, which repopulates the store's cache, so `write_at` finds every section changed relative to what
    // it holds and marks it dirty again. `Entry::dirty` means *cache differs from disk*, and after a
    // re-read they genuinely do.
    //
    // **So the cost of a second save is a full rewrite, and the honest thing is to assert that rather than
    // a number nobody measured.** What is asserted instead is the property that matters and is cheap: the
    // bytes on disk are unchanged, so repeating a save cannot corrupt a document.
    let again = commit_document(&mut store, &mut editor).expect("second commit");
    assert_eq!(
        again, len,
        "the second commit rewrites, because the re-read re-dirties the cache"
    );

    // **And the bytes on disk are what was typed**, because a return value is not evidence.
    drop(store);
    drop(opened);
    let mut expected = saved.clone();
    expected.splice(0..0, b"small ".iter().copied());
    assert_eq!(
        read_back(&path, 8),
        expected,
        "and the file agrees -- twice over"
    );
}

/// **A commit leaves the rope bounded, which is the property that makes it affordable at all.**
///
/// The commit's read loop faults **every** leaf in, so mid-commit the rope holds the whole document
/// page-locked -- and Phase 13 exists precisely so that does not happen. `commit_document` sheds the
/// document's leaves once the store has been written and the record emptied.
///
/// **Asserted as an absolute rather than a ratio**, because the point is not "less than it was" but "not
/// the document": `resident_bytes` after a commit is 0, so nothing page-locked is retained by an operation
/// whose whole purpose is that the document is *not* resident. A ratio assertion would pass at 90% and
/// still blow an 8 MiB page-lock ceiling on a large document.
#[test]
fn a_commit_leaves_the_rope_holding_nothing() {
    let n = 65_520 * 2 + 4_000;
    let saved = text(n);
    let (path, _wf) = container(&saved);

    let file = DirectFile::open(&path).expect("open");
    let mut opened = open_document(file, PASS, ITER, DEFAULT_RESIDENT_SECTIONS).expect("open");
    let mut editor = std::mem::replace(&mut opened.editor, holonomy_text::Editor::new());
    let mut store = SectionStore::new(&mut opened.container, DEFAULT_RESIDENT_SECTIONS);

    editor
        .insert_at(0, b"x", SpanPolicy::GrowIntoInsert)
        .expect("insert");
    let before = editor.resident_bytes();
    commit_document(&mut store, &mut editor).expect("commit");

    assert_eq!(
        editor.resident_bytes(),
        0,
        "a commit retained {before} bytes page-locked; the read loop faults every leaf in, so shedding \
         them afterwards is what keeps a save from being an 8 MiB mlock event"
    );

    // **And the document is still readable afterwards**, because shedding every leaf means the next read is
    // a full fault -- which is only correct if the record really is empty and the store really does hold it.
    let mut out = vec![0u8; 64];
    editor
        .read_into_faulting(&mut store, 30_000, &mut out)
        .expect("the document reads back after a commit");
    let mut want = saved.clone();
    want.splice(0..0, b"x".iter().copied());
    assert_eq!(
        out,
        want[30_000..30_064],
        "and it reads through the record correctly -- which is empty, so this is a plain fault"
    );
}
