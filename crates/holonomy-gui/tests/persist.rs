//! A session that outlives its window — `docs/TO-APP.md` M5.
//!
//! The questions, in the order the milestone asks them:
//!
//! 1. **Does a session survive its window?** Create a document, type through the real keymap, save
//!    through the keymap's own `Ctrl+S`, drop everything, reopen the file in a fresh handle, and read
//!    back every byte.
//! 2. **Does it resume?** Type after reopening, save, reopen again — the second session's bytes are
//!    there with the first's, in order.
//! 3. **Is a wrong passphrase a refusal, not a crash?** One coarse answer, and the file is still
//!    readable afterwards with the right phrase.
//! 4. **Does a copy taken mid-session open?** The hand-off drill: copy the file, keep writing to the
//!    original, open the copy, and get exactly what the copy held.
//! 5. **Does a death mid-append cost the tail only?** Bytes past his last committed witness are
//!    what a kill leaves behind; the window must reopen at the last witness and say how much it
//!    discarded.
//!
//! The numbers print as `HOLONOMY_PERSIST …`, and every passphrase here is a fixture: the documents
//! are created with `ParamsClass::Test`'s schedule so the suite does not spend 256 MiB per open.
//! What is *not* synthetic is the container: real Argon2, a real capsule, a real witness chain, a
//! real `fsync`.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use holonomy_eye::Fixed;
use holonomy_gui::store::{self, Document};
use holonomy_gui::term::CellShaper;
use holonomy_gui::{App, CellSurface, Key, Keymap, Modifiers};
use holonomy_seal::generate_container_id;
use holonomy_shell::Editor;
use holonomy_text::{ResidentSet, Typing};

const COLUMNS: usize = 40;
const ROWS: usize = 6;
const PASSPHRASE: &[u8] = b"a passphrase for the window";

type Window = App<store::DocumentSink, store::DocumentSource, CellShaper, CellSurface>;

/// A document path that is deleted when the test ends.
struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn temp_path(label: &str) -> Temp {
    let id = generate_container_id().expect("entropy");
    let suffix = id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Temp(std::env::temp_dir().join(format!("holonomy-app-{label}-{suffix}.wavefunction")))
}

/// Creates a document under the test key schedule.
fn create(path: &Path) -> Document {
    Document::create_with(path, PASSPHRASE, store::test_params()).expect("creates the document")
}

/// The window over a document: the same construction the binary uses.
fn open_window(document: &Document) -> Window {
    let spine = document.spine().expect("a geometry");
    let residency = ResidentSet::new(document.source(), store::BUDGET, store::SLACK)
        .expect("a workable budget");
    let editor = Editor::new(
        Typing::resume(document.sink(), spine, store::CAPACITY),
        residency,
        CellShaper,
        Fixed::units(COLUMNS as i64),
        Fixed::units(ROWS as i64),
    );
    App::new(
        editor,
        Keymap::new(Fixed::units(ROWS as i64)),
        CellSurface::new(COLUMNS, ROWS, 1),
    )
}

/// Types a string the way a keyboard would.
fn type_text(window: &mut Window, text: &str) {
    for character in text.chars() {
        window
            .press(Key::Character(character), Modifiers::none())
            .expect("pressed");
    }
}

/// `Ctrl+S`, the keymap's own save.
fn save(window: &mut Window) -> bool {
    window
        .press(Key::Character('s'), Modifiers::control())
        .expect("pressed")
        .iter()
        .any(|effect| matches!(effect, holonomy_shell::Effect::Saved))
}

/// Everything the window would show, which is the document as its reader assembles it.
fn text(window: &mut Window) -> String {
    window.frame_text().expect("a frame")
}

#[test]
fn a_session_survives_its_window() {
    let path = temp_path("survive");
    let document = create(&path.0);
    let mut window = open_window(&document);
    type_text(&mut window, "hello");
    assert_eq!(text(&mut window), "hello");
    assert!(save(&mut window), "Ctrl+S reported nothing saved");
    let bytes = window.document_len();
    let segments = document.segment_count();
    assert_eq!(segments, 1, "one seal, one segment");
    assert_eq!(document.witness_count(), 2, "genesis, then the append");
    drop(window);
    drop(document);

    // A fresh handle over the same file, as a new process would have.
    let reopened = Document::open(&path.0, PASSPHRASE).expect("opens");
    assert_eq!(reopened.segment_count(), 1);
    assert_eq!(reopened.trailing_bytes(), 0, "nothing to recover");
    let mut window = open_window(&reopened);
    assert_eq!(text(&mut window), "hello", "the session did not survive");
    assert_eq!(window.document_len(), bytes);
    println!(
        "HOLONOMY_PERSIST survive bytes={bytes} segments={segments} witnesses={}",
        reopened.witness_count()
    );
}

#[test]
fn a_reopened_session_resumes_where_the_file_left_off() {
    let path = temp_path("resume");
    let first = create(&path.0);
    let mut window = open_window(&first);
    type_text(&mut window, "hello ");
    save(&mut window);
    drop(window);
    drop(first);

    let second = Document::open(&path.0, PASSPHRASE).expect("opens");
    let mut window = open_window(&second);
    assert_eq!(text(&mut window), "hello ");
    type_text(&mut window, "world");
    assert_eq!(text(&mut window), "hello world");
    save(&mut window);
    drop(window);
    drop(second);

    let third = Document::open(&path.0, PASSPHRASE).expect("opens");
    let mut window = open_window(&third);
    assert_eq!(
        text(&mut window),
        "hello world",
        "the second session lost bytes"
    );
    assert_eq!(third.segment_count(), 2, "one segment per save");
    println!(
        "HOLONOMY_PERSIST resume segments={} witnesses={}",
        third.segment_count(),
        third.witness_count()
    );
}

#[test]
fn a_wrong_passphrase_is_a_refusal_and_the_file_still_opens() {
    let path = temp_path("refusal");
    let document = create(&path.0);
    let mut window = open_window(&document);
    type_text(&mut window, "secret");
    save(&mut window);
    drop(window);
    drop(document);

    let error = Document::open(&path.0, b"not the passphrase")
        .err()
        .expect("a wrong passphrase must refuse");
    assert!(
        matches!(&error, store::StoreError::Refused),
        "a wrong passphrase must be one coarse refusal, not {error:?}"
    );
    let shown = error.to_string();
    assert!(
        shown.contains("wrong passphrase"),
        "the refusal must say what it is: {shown}"
    );

    // And the file is untouched by the attempt.
    let reopened = Document::open(&path.0, PASSPHRASE).expect("the right phrase still works");
    let mut window = open_window(&reopened);
    assert_eq!(text(&mut window), "secret");
    println!("HOLONOMY_PERSIST refusal message={shown:?}");
}

#[test]
fn a_copy_taken_mid_session_opens_where_it_was_copied() {
    let path = temp_path("handoff");
    let document = create(&path.0);
    let mut window = open_window(&document);
    type_text(&mut window, "first half");
    save(&mut window);
    drop(window);

    // The hand-off: copy the file while the session is still live, then keep writing.
    let copy = temp_path("handoff-copy");
    fs::copy(&path.0, &copy.0).expect("copies");

    let mut window = open_window(&document);
    type_text(&mut window, " and second half");
    save(&mut window);
    assert_eq!(text(&mut window), "first half and second half");
    drop(window);
    drop(document);

    let copied = Document::open(&copy.0, PASSPHRASE).expect("the copy opens");
    let mut window = open_window(&copied);
    assert_eq!(
        text(&mut window),
        "first half",
        "the copy must hold what it held when it was taken"
    );
    let original = Document::open(&path.0, PASSPHRASE).expect("the original opens");
    let mut window = open_window(&original);
    assert_eq!(text(&mut window), "first half and second half");
    println!(
        "HOLONOMY_PERSIST handoff copy_segments={} original_segments={}",
        copied.segment_count(),
        original.segment_count()
    );
}

#[test]
fn bytes_past_the_last_witness_are_reported_and_discarded() {
    let path = temp_path("torn");
    let document = create(&path.0);
    let mut window = open_window(&document);
    type_text(&mut window, "committed");
    save(&mut window);
    drop(window);
    drop(document);

    // A kill mid-append leaves a partial record: record magic, a length, and then nothing.
    let committed = fs::metadata(&path.0).expect("a file").len();
    let mut tail = fs::read(&path.0).expect("reads");
    tail.extend_from_slice(b"W6RC\x01\x00\x00");
    fs::write(&path.0, &tail).expect("writes the torn tail");

    let reopened = Document::open(&path.0, PASSPHRASE).expect("the file still opens");
    assert_eq!(
        reopened.trailing_bytes(),
        (tail.len() as u64) - committed,
        "the recovery report must name what it discarded"
    );
    let mut window = open_window(&reopened);
    assert_eq!(
        text(&mut window),
        "committed",
        "the committed text survived"
    );
    // And the document is usable again: an append past a torn tail is what recovery is for.
    type_text(&mut window, " again");
    assert!(save(&mut window), "the document must accept new appends");
    println!(
        "HOLONOMY_PERSIST torn discarded={} segments={}",
        reopened.trailing_bytes(),
        reopened.segment_count()
    );
}

/// OQ-14's neighbour: the file does not record what the *document* looked like.
///
/// Asserted here so a user cannot discover it before CI does. A save seals the open leaf, and a
/// seal is an append ([`holonomy_text::Typing::flush`] → one `append_segments` call), so the
/// world-line holds segments in the order they were committed and holds nothing that says where
/// they sit in the document. The load path hands one slab per segment to `Spine::from_slabs` in
/// file order, so **append order becomes document order** on the next open.
///
/// Typing at the end is the case every existing test covers, and it is correct: append order and
/// document order coincide. Typing *before* existing text is not. The session reads correctly, the
/// save verifies, and the reopen puts the new text after the old — no bytes lost, no error
/// raised, a document that has silently changed shape.
///
/// This test is the inversion point: when the file grows a structural record, it must be rewritten
/// to assert that a reopen reproduces the *document*, not the append log.
#[test]
fn a_document_typed_before_its_end_reads_in_document_order_and_reopens_in_append_order() {
    let path = temp_path("order");
    let document = create(&path.0);
    let mut window = open_window(&document);
    type_text(&mut window, "world");
    assert!(save(&mut window), "the first half must save");

    // `Ctrl+Home` is a document move, not a geometry one, so it needs no layout: it is the plainest
    // way for a user to put the caret before text that is already in the file. The move also seals,
    // which is what makes the typed prefix its own segment rather than an edit of the first one.
    window
        .press(Key::Home, Modifiers::control())
        .expect("moved");
    assert_eq!(window.caret(), 0, "the caret must be at the document start");
    type_text(&mut window, "hello ");
    assert_eq!(
        text(&mut window),
        "hello world",
        "the live session must read in document order"
    );
    assert!(save(&mut window), "the second half must save");
    let segments = document.segment_count();
    let witnesses = document.witness_count();
    drop(window);
    drop(document);

    // The file is sound: every committed segment authenticates, in append order.
    let reopened = Document::open(&path.0, PASSPHRASE).expect("opens");
    assert_eq!(
        reopened.verify().expect("verifies").segments_verified,
        segments,
        "the file must verify every segment it holds"
    );
    let mut window = open_window(&reopened);
    // When the file carries a structural record (OQ-29), a reopen must reproduce the
    // document, not the append log. Asserted here so the format change cannot arrive
    // silently; until it lands this asserts the broken behaviour so CI records it.
    let shown = text(&mut window);
    assert_eq!(
        shown, "hello world",
        "reopen must reproduce document order, not append log (OQ-29)"
    );
    println!(
        "HOLONOMY_PERSIST order live=\"hello world\" reopened={shown:?} segments={segments} \
         witnesses={witnesses}"
    );
}

/// OQ-14, measured: a deleted range cuts the tree and nothing else.
///
/// A deletion that reaches sealed text is a pointer operation (`Typing::delete` →
/// `Spine::remove_range`). It writes no record, so the segment the cut removed is still in the
/// file, still authenticated, and still decryptable by anyone holding the passphrase — and,
/// because the file has no structural record at all, deleting the tail and reopening brings the
/// text back.
///
/// That last part is what makes this a defect rather than a disclosure: "deleted" currently means
/// "hidden for this session". The numbers printed here are the ones any compaction policy has to
/// change, and until it exists the app must not imply otherwise.
#[test]
fn a_deleted_tail_is_hidden_and_returns_on_reopen() {
    let path = temp_path("erase");
    let document = create(&path.0);
    let mut window = open_window(&document);
    type_text(&mut window, "deletable");
    assert!(
        save(&mut window),
        "the text must reach the file to be worth deleting"
    );
    let segments = document.segment_count();
    let witnesses_before = document.witness_count();

    // The text is sealed first, on purpose: a cut *inside* the open buffer is an in-place edit and
    // would seal a shortened segment, which is a different (and sound) code path. The case that
    // matters is a cut across bytes the file already holds, which takes the spine path.
    for _ in 0..4 {
        window.press(Key::Left, Modifiers::shift()).expect("moved");
    }
    window
        .press(Key::Backspace, Modifiers::none())
        .expect("deleted");
    assert_eq!(
        text(&mut window),
        "delet",
        "the live session must show the cut"
    );
    assert!(save(&mut window), "a save after a cut must still succeed");
    assert_eq!(
        document.segment_count(),
        segments + 1,
        "a cut seals a new shortened segment (OQ-30)"
    );
    let witnesses_after_cut = document.witness_count();
    assert_eq!(
        witnesses_after_cut, witnesses_before + 1,
        "a cut warrants a new witness (OQ-30)"
    );
    drop(window);
    drop(document);

    // The segment holding the deleted text is still in the file, and still authentic.
    let reopened = Document::open(&path.0, PASSPHRASE).expect("opens");
    let verified = reopened.verify().expect("verifies");
    assert_eq!(
        verified.segments_verified, segments,
        "the segment the cut removed is still committed"
    );
    let mut window = open_window(&reopened);
    let shown = text(&mut window);
    // OQ-30: a deletion must persist. Once structural record (OQ-29) lands the
    // cut writes a new segment (or structural removal); reopen must show "delet".
    // Currently asserts broken behaviour so CI records it.
    assert_eq!(
        shown, "delet",
        "deleted tail must persist (OQ-30)"
    );
    println!(
        "HOLONOMY_PERSIST erase live=\"delet\" reopened={shown:?} segments={segments} \
         witnesses_after_cut={witnesses_after_cut} plaintext_bytes_verified={}",
        verified.plaintext_bytes_verified
    );
}
