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
use holonomy_text::{ResidentSet, SegmentSource, Typing};

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

/// OQ-29: a save records the document order, not just the append log.
///
/// The session reports the real ids of its sealed leaves after each commit; the file-backed sink
/// folds that order into an authenticated witness. Typing `world`, moving to the start and typing
/// `hello ` appends the leaves in the order `world`, `hello `, but the saved order is `hello `,
/// `world` — and a reopen must show the document, not the order the bytes happened to land in.
#[test]
fn a_document_typed_before_its_end_reads_in_document_order_after_a_reopen() {
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
    let shown = text(&mut window);
    assert_eq!(
        shown, "hello world",
        "the authenticated document order survives a reopen (OQ-29)"
    );
    println!(
        "HOLONOMY_PERSIST order live=\"hello world\" reopened={shown:?} segments={segments} \
         witnesses={witnesses}"
    );
}

/// OQ-30: a deletion that reaches sealed text commits an authenticated sub-segment spine manifest
/// on the next save, with zero segment rewrites and a single witness commit.
#[test]
fn a_deleted_tail_is_committed_on_save_and_hidden_after_a_reopen() {
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
    // matters is a cut across bytes the file already holds, which takes the spine manifest path.
    for _ in 0..4 {
        window.press(Key::Left, Modifiers::shift()).expect("moved");
    }
    window
        .press(Key::Backspace, Modifiers::none())
        .expect("deleted");
    assert!(
        window.has_pending(),
        "a structural cut across sealed text leaves the window unsaved until flushed"
    );
    assert_eq!(
        text(&mut window),
        "delet",
        "the live session must show the cut"
    );
    assert!(save(&mut window), "a save after a cut must still succeed");
    assert!(
        !window.has_pending(),
        "saving the spine manifest clears the unsaved state"
    );
    assert_eq!(
        document.segment_count(),
        segments,
        "a cut across sealed text commits a sub-segment slab manifest with zero segment rewrites"
    );
    let witnesses_after_cut = document.witness_count();
    assert_eq!(
        witnesses_after_cut,
        witnesses_before + 1,
        "committing the spine manifest warrants once"
    );
    drop(window);
    drop(document);

    // The segment holding the deleted text is still in the file until compaction, and authentic.
    let reopened = Document::open(&path.0, PASSPHRASE).expect("opens");
    let verified = reopened.verify().expect("verifies");
    assert_eq!(
        verified.segments_verified, segments,
        "only the original segment exists on the world-line"
    );
    let mut window = open_window(&reopened);
    let shown = text(&mut window);
    assert_eq!(
        shown, "delet",
        "the deleted tail must not come back on reopen (OQ-30)"
    );
    println!(
        "HOLONOMY_PERSIST erase live=\"delet\" reopened={shown:?} segments={segments} \
         witnesses_after_cut={witnesses_after_cut} plaintext_bytes_verified={}",
        verified.plaintext_bytes_verified
    );
}

#[test]
fn mid_segment_insertion_empty_deletion_and_post_save_undo_survive_reopen() {
    let path = temp_path("spine-manifest");
    let document = create(&path.0);
    let mut window = open_window(&document);
    type_text(&mut window, "hello world");
    assert!(save(&mut window));
    assert_eq!(document.segment_count(), 1);

    // Move into the middle of the single sealed segment ("hello |world") and insert "brave ".
    for _ in 0..5 {
        window.press(Key::Left, Modifiers::none()).expect("moved");
    }
    type_text(&mut window, "brave ");
    assert_eq!(text(&mut window), "hello brave world");
    assert!(save(&mut window));
    assert_eq!(
        document.segment_count(),
        2,
        "splitting a sealed segment appends only the newly typed middle leaf"
    );
    drop(window);
    drop(document);

    let reopened = Document::open(&path.0, PASSPHRASE).expect("opens");
    let mut window = open_window(&reopened);
    assert_eq!(text(&mut window), "hello brave world");

    // Undo after reopen is inert; type a burst, save it, then undo that burst (`Ctrl+Z`) and save.
    window.press(Key::End, Modifiers::control()).expect("end");
    type_text(&mut window, "!");
    assert!(save(&mut window));
    window
        .press(Key::Character('z'), Modifiers::control())
        .expect("undo");
    assert_eq!(text(&mut window), "hello brave world");
    assert!(
        window.has_pending(),
        "undoing a sealed edit marks the spine dirty"
    );
    assert!(save(&mut window));
    drop(window);
    drop(reopened);

    let after_undo = Document::open(&path.0, PASSPHRASE).expect("opens");
    let mut window = open_window(&after_undo);
    assert_eq!(text(&mut window), "hello brave world");

    // Select all + Backspace + Save commits an explicit empty manifest so reopen yields "".
    window
        .press(Key::Character('a'), Modifiers::control())
        .expect("select all");
    window
        .press(Key::Backspace, Modifiers::none())
        .expect("erase all");
    assert_eq!(text(&mut window), "");
    assert!(window.has_pending());
    assert!(save(&mut window));
    drop(window);
    drop(after_undo);

    let emptied = Document::open(&path.0, PASSPHRASE).expect("opens");
    let mut window = open_window(&emptied);
    assert_eq!(
        text(&mut window),
        "",
        "emptying a sealed document must reopen empty rather than falling back to append order"
    );
}

#[test]
fn nfc_and_nfd_passphrases_open_the_same_document_and_compaction_purges_deleted_history() {
    let source_path = temp_path("compact-src");
    let dest_path = temp_path("compact-dst");
    let nfd = "cafe\u{0301} secret".as_bytes();
    let nfc = "caf\u{00E9} secret".as_bytes();

    let document =
        Document::create_with(&source_path.0, nfd, store::test_params()).expect("creates with NFD");
    let mut window = open_window(&document);
    type_text(&mut window, "discarded prefix -- surviving prose");
    assert!(save(&mut window));
    window.press(Key::Home, Modifiers::control()).expect("home");
    for _ in 0..20 {
        window
            .press(Key::Right, Modifiers::shift())
            .expect("select prefix");
    }
    window
        .press(Key::Backspace, Modifiers::none())
        .expect("delete prefix");
    assert_eq!(text(&mut window), "surviving prose");
    assert!(save(&mut window));
    drop(window);
    drop(document);

    // Reopen with the canonically equivalent NFC spelling (OQ-28).
    let reopened = Document::open(&source_path.0, nfc).expect("NFC opens NFD-created document");
    assert_eq!(
        reopened
            .verify()
            .expect("verifies")
            .plaintext_bytes_verified,
        35,
        "before compaction, the deleted prefix still lives in the append-only segment"
    );

    // Wrong confirmation passphrase refuses compaction without creating the destination file.
    assert!(matches!(
        reopened.compact_to(&dest_path.0, b"wrong confirmation"),
        Err(store::StoreError::Refused)
    ));
    assert!(!dest_path.0.exists());

    // Matching confirmation passphrase compacts only the active spine into a fresh world-line.
    let compacted = reopened
        .compact_to(&dest_path.0, nfd)
        .expect("compacts with confirmed passphrase");
    let report = compacted.verify().expect("compacted verifies");
    assert_eq!(report.segments_verified, 1);
    assert_eq!(
        report.plaintext_bytes_verified,
        "surviving prose".len() as u64,
        "compacted world-line holds only the surviving prose bytes (OQ-14)"
    );
    drop(compacted);

    let reopened_compacted = Document::open(&dest_path.0, nfc).expect("reopens compacted");
    let mut window = open_window(&reopened_compacted);
    assert_eq!(text(&mut window), "surviving prose");
}

#[test]
fn replace_all_commits_one_witness_and_undo_persists() {
    let path = temp_path("replace-all-atomic");
    let document = create(&path.0);
    let mut typing = Typing::new(document.sink(), store::CAPACITY);
    typing.insert(b"one fish, one bird").unwrap();
    typing.flush().unwrap();
    let original = typing.spine().clone();
    let witnesses = document.witness_count();
    let mut source = document.source();
    assert_eq!(typing.replace_all(b"one", b"two", &mut source).unwrap(), 2);
    assert_eq!(document.witness_count(), witnesses + 1);

    let reopened = Document::open(&path.0, PASSPHRASE).expect("reopens replacement");
    assert_eq!(text(&mut open_window(&reopened)), "two fish, two bird");
    reopened.verify().unwrap();
    drop(reopened);

    assert!(typing.undo().unwrap());
    assert!(typing.spine().ptr_eq(&original));
    typing.flush().unwrap();
    drop(typing);
    drop(document);
    let reopened = Document::open(&path.0, PASSPHRASE).expect("reopens undo");
    assert_eq!(text(&mut open_window(&reopened)), "one fish, one bird");
}

#[test]
fn document_witness_history_bookmarks_diff_search_and_restore_persist() {
    let path = temp_path("history-restore-persist");
    let document = create(&path.0);
    let mut typing = Typing::new(document.sink(), store::CAPACITY);
    typing.insert(b"chapter one alpha\n").unwrap();
    typing.flush().unwrap();
    document
        .add_bookmark(1, 1_700_000_010, "Chapter One")
        .unwrap();

    typing.insert(b"chapter two beta\n").unwrap();
    typing.flush().unwrap();
    typing.delete(8, 17).unwrap(); // delete "alpha\ncha" across boundary
    typing.flush().unwrap();

    assert_eq!(document.witness_history().len(), 4);
    assert_eq!(document.bookmarks().len(), 1);
    assert_eq!(document.bookmarks()[0].name, "Chapter One");

    let diff = document.diff_witnesses(1, 3).unwrap();
    assert!(!diff.is_empty());

    let hist_hits = document.search_history("alpha").unwrap();
    assert_eq!(hist_hits.len(), 2); // witness 1 and witness 2 had "alpha"
    assert_eq!(hist_hits[0].witness_sequence, 1);
    assert_eq!(hist_hits[1].witness_sequence, 2);

    // Restore back to witness 1 via Typing::restore_spine and verify reopen matches witness 1.
    let w1 = document.spine_at_witness(1).unwrap();
    typing.restore_spine(w1).unwrap();
    drop(typing);
    drop(document);

    let reopened = Document::open(&path.0, PASSPHRASE).expect("reopens restored");
    assert_eq!(text(&mut open_window(&reopened)), "chapter one alpha\n");
    assert_eq!(reopened.bookmarks().len(), 1);
    assert_eq!(reopened.witness_history().len(), 5);
    reopened.verify().unwrap();
}

fn all_bytes(document: &Document) -> Vec<u8> {
    let spine = document.spine().expect("spine");
    let mut source = document.source();
    let mut out = Vec::with_capacity(usize::try_from(spine.len()).expect("fits in usize"));
    for span in spine.spans_in(0, spine.len()) {
        let slab = span.slab();
        let segment = source.load(slab.segment()).expect("segment loads");
        out.extend_from_slice(&segment[slab.start() as usize..slab.end() as usize]);
    }
    out
}

#[test]
fn dense_streaming_replace_all_over_real_document_persists_and_undoes() {
    let path = temp_path("dense-streaming-persist");
    let document = create(&path.0);
    let mut typing = Typing::new(document.sink(), store::CAPACITY);
    let payload = "ab ".repeat(17_000);
    typing.insert(payload.as_bytes()).unwrap();
    typing.flush().unwrap();
    let original = typing.spine().clone();
    let before_witnesses = document.witness_count();

    let mut source = document.source();
    let count = typing.replace_all(b"ab", b"XYZ", &mut source).unwrap();
    assert_eq!(count, 17_000);
    assert_eq!(document.witness_count(), before_witnesses + 1);
    drop(source);

    let reopened = Document::open(&path.0, PASSPHRASE).expect("reopens dense");
    assert_eq!(
        String::from_utf8(all_bytes(&reopened)).unwrap(),
        "XYZ ".repeat(17_000)
    );
    assert_eq!(
        open_window(&reopened).document_len(),
        ("XYZ ".len() * 17_000) as u64
    );
    reopened.verify().unwrap();
    drop(reopened);

    assert!(typing.undo().unwrap());
    assert!(typing.spine().ptr_eq(&original));
    typing.flush().unwrap();
    drop(typing);
    drop(document);

    let reopened = Document::open(&path.0, PASSPHRASE).expect("reopens dense undo");
    assert_eq!(String::from_utf8(all_bytes(&reopened)).unwrap(), payload);
    assert_eq!(open_window(&reopened).document_len(), payload.len() as u64);
}

#[test]
fn witness_excerpt_trims_split_multibyte_utf8_boundaries() {
    let path = temp_path("excerpt-utf8");
    let document = create(&path.0);
    let mut typing = Typing::new(document.sink(), store::CAPACITY);
    let prefix = "a".repeat(318);
    let body = format!("{prefix}🦀tail");
    typing.insert(body.as_bytes()).unwrap();
    typing.flush().unwrap();

    // End at byte 320 splits the 4-byte emoji at [318..322); excerpt must trim cleanly.
    let head = document.read_witness_excerpt(1, 0, 320).unwrap();
    assert_eq!(head, prefix);

    // Start inside the 4-byte emoji at 319 and end inside it at 324 -> trims leading continuation bytes.
    let mid = document.read_witness_excerpt(1, 319, 326).unwrap();
    assert_eq!(mid, "tail");
}

fn fast_temp_path(label: &str) -> Temp {
    let dir = if std::path::Path::new("/dev/shm").is_dir() {
        std::path::PathBuf::from("/dev/shm")
    } else {
        std::env::temp_dir()
    };
    let id = holonomy_seal::generate_container_id().expect("entropy");
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    Temp(dir.join(format!("holonomy-persist-{label}-{hex}.wavefunction")))
}

#[test]
#[ignore = "60-year (21,900 witness) benchmark; run in release CI `persist` job"]
fn sixty_year_21900_witness_scrub_and_reverify() {
    use std::time::Instant;

    const DAYS: u64 = 60 * 365; // 21,900 daily witnesses
    let path = fast_temp_path("60yr-21900w");
    let document = Document::create(&path.0, PASSPHRASE).expect("creates 60-year world-line");

    let write_start = Instant::now();
    let entries: Vec<String> = (1..DAYS)
        .map(|day| {
            if day % 3650 == 0 {
                format!("decade-{day} milestone entry\n")
            } else {
                format!("d{day}\n")
            }
        })
        .collect();
    document
        .append_witnessed_entries(entries.iter().map(String::as_bytes))
        .expect("seal 21,899 daily witnesses");
    drop(entries);

    // Final day sealed live through `Typing::resume` + `flush()` on top of 21,899 witnesses.
    let spine = document.spine().expect("spine before final year-60 save");
    let mut typing = Typing::resume(document.sink(), spine, store::CAPACITY);
    let end = typing.document_len();
    typing
        .insert_at(end, format!("decade-{DAYS} milestone entry\n").as_bytes())
        .expect("insert final year-60 milestone");
    typing.flush().expect("seal final year-60 witness");
    let write_ms = write_start.elapsed().as_millis();
    drop(typing);

    // Bookmarks across years 1, 10, 30, 60.
    for (seq, name) in [
        (365u64, "year-01"),
        (3_650, "year-10"),
        (10_950, "year-30"),
        (DAYS, "year-60"),
    ] {
        document
            .add_bookmark(seq, 1_700_000_000 + seq as i64 * 86_400, name)
            .expect("bookmark");
    }
    drop(document);

    // Cold reopen + witness scrub across all 60 years.
    let reopen_start = Instant::now();
    let reopened = Document::open(&path.0, PASSPHRASE).expect("reopens 60-year document");
    let reopen_ms = reopen_start.elapsed().as_millis();
    assert_eq!(reopened.witness_history().len() as u64, DAYS + 1);
    assert_eq!(reopened.bookmarks().len(), 4);

    let scrub_start = Instant::now();
    let w_y1 = reopened.spine_at_witness(365).expect("y1 spine");
    let w_y10 = reopened.spine_at_witness(3_650).expect("y10 spine");
    let w_y30 = reopened.spine_at_witness(10_950).expect("y30 spine");
    let w_y60 = reopened.spine_at_witness(DAYS).expect("y60 spine");
    assert!(w_y1.len() < w_y10.len() && w_y10.len() < w_y30.len() && w_y30.len() < w_y60.len());

    // Scrub through years 1, 10, 30, 60 inside a live `Editor` + `ResidentSet`, verifying strict
    // memory-budget enforcement at every history point (`ROADMAP.md` Block 8 Gate 2).
    {
        use holonomy_eye::{Fixed, FontSet, View};
        let fonts = FontSet::embedded(14).expect("fonts");
        let residency =
            ResidentSet::new(reopened.source(), store::BUDGET, store::SLACK).expect("residency");
        let mut editor = Editor::new(
            Typing::resume(reopened.sink(), w_y60.clone(), store::CAPACITY),
            residency,
            fonts,
            Fixed::units(680),
            Fixed::units(800),
        );
        editor.set_view(View::IDENTITY);
        for sp in [w_y1.clone(), w_y10, w_y30.clone(), w_y60.clone()] {
            editor.restore_spine(sp).expect("scrub spine");
            let frame = editor.frame().expect("frame after scrub");
            assert!(!frame.text().is_empty());
            assert!(
                editor.residency().resident_bytes() <= store::BUDGET,
                "60-year witness scrub exceeded ResidentSet budget ({} > {})",
                editor.residency().resident_bytes(),
                store::BUDGET
            );
        }
    }

    let diff = reopened
        .diff_witnesses(10_950, DAYS)
        .expect("diff y30->y60");
    assert!(!diff.is_empty());
    let excerpt = reopened
        .read_witness_excerpt(3_650, 0, w_y30.len().min(64))
        .expect("y10 excerpt");
    assert!(!excerpt.is_empty());
    let scrub_ms = scrub_start.elapsed().as_millis();

    let hits = reopened
        .find_nth_at_witness(DAYS, "milestone entry", 0)
        .expect("bounded witness search at year 60");
    assert_eq!(hits.count, 6);

    let report = reopened
        .verify()
        .expect("full 60-year witness chain verifies");
    assert_eq!(report.segments_verified, DAYS);
    println!(
        "HOLONOMY_60YR_SCRUB witnesses={} write_ms={} reopen_ms={} scrub_ms={} bytes={}",
        DAYS + 1,
        write_ms,
        reopen_ms,
        scrub_ms,
        report.plaintext_bytes_verified
    );
}

#[test]
#[ignore = "30M-word cold-open and bounded residency benchmark; run in release CI `persist` job"]
fn cold_open_30m_word_document_stays_under_residency_budget() {
    use holonomy_eye::{Fixed, FontSet, View};
    use holonomy_text::{SegmentSink, StyleMap};
    use std::time::Instant;

    let path = fast_temp_path("30m-words");
    let document = Document::create(&path.0, PASSPHRASE).expect("creates 30M-word document");
    let chunk = "alpha ".repeat(10_922); // 65,532 B (<= 64 KiB segment quantum, 10,922 words)
    const LEAVES: usize = 2_800; // 2,800 * 10,922 = 30,581,600 words (~183.5 MiB)

    let build_start = Instant::now();
    {
        let mut sink = document.sink();
        let mut writer = sink.begin_stream_publish().expect("stream publish");
        let mut slabs = Vec::with_capacity(LEAVES);
        for _ in 0..LEAVES {
            let seg = writer.write_chunk(chunk.as_bytes()).expect("write leaf");
            slabs.push(holonomy_text::Slab::new(seg, 0, chunk.len() as u32));
        }
        writer
            .finish(&slabs, &StyleMap::empty())
            .expect("commit 30M-word manifest");
    }
    let build_ms = build_start.elapsed().as_millis();
    drop(document);

    // Cold reopen + first frame (bootstrap only, zero full-document scan or decryption).
    let open_start = Instant::now();
    let reopened = Document::open(&path.0, PASSPHRASE).expect("cold open 30M-word document");
    let spine = reopened.spine().expect("spine");
    assert!(
        spine.len() > 180_000_000,
        "must exceed 30M words (~180 MB): got {} B",
        spine.len()
    );
    let residency =
        ResidentSet::new(reopened.source(), store::BUDGET, store::SLACK).expect("residency");
    let fonts = FontSet::embedded(14).expect("embedded fonts");
    let mut editor = Editor::new(
        Typing::resume(reopened.sink(), spine, store::CAPACITY),
        residency,
        fonts,
        Fixed::units(680),
        Fixed::units(800),
    );
    editor.set_view(View::IDENTITY);
    let frame = editor.frame().expect("first frame boots lazily");
    let open_ms = open_start.elapsed().as_millis();
    assert!(
        !frame.text().is_empty() && frame.text().starts_with("alpha "),
        "first viewport must render live 30M-word prose"
    );

    let resident = editor.residency().resident_bytes();
    let segments = editor.residency().resident_segments();
    assert!(
        resident <= 2 * 1024 * 1024,
        "cold open must fault only viewport leaves (<= 2 MiB and {} B budget), not 183 MiB (got {resident} B)",
        store::BUDGET
    );
    assert!(
        open_ms < 10_000,
        "30M-word cold open took {open_ms} ms (> 10s ceiling)"
    );
    println!(
        "HOLONOMY_COLD_OPEN_30M bytes={} build_ms={} open_ms={} resident_bytes={} resident_segments={}",
        editor.document_len(),
        build_ms,
        open_ms,
        resident,
        segments
    );
}

#[test]
fn every_toolbar_action_round_trips_byte_identical_style_state() {
    use holonomy_eye::{FontSet, View};
    use holonomy_gui::canvas::{apply_toolbar_action, PixelSurface, ToolbarAction};
    use holonomy_shell::{Intent, QUIET_MD_THEME};

    let path = temp_path("toolbar-roundtrip");
    let doc = create(&path.0);
    let fonts = FontSet::embedded(QUIET_MD_THEME.body_size_px).expect("fonts");
    let surface = PixelSurface::new(fonts.clone(), View::IDENTITY, 1_120, 760);
    let (lw, lh) = surface.logical_editor_extent();
    let residency = ResidentSet::new(doc.source(), store::BUDGET, store::SLACK).expect("residency");
    let mut editor = Editor::new(
        Typing::resume(doc.sink(), doc.spine().expect("spine"), store::CAPACITY),
        residency,
        fonts,
        lw,
        lh,
    );
    editor.set_view(View::IDENTITY);
    let mut app = App::new(editor, Keymap::new(lh), surface);

    let prose = concat!(
        "Title Line\n",
        "Heading Two\n",
        "Heading Three\n",
        "Serif Sized Bold Italic Under Strike Code Linked Span\n",
        "Bullet Item Centered\n",
        "Ordered Item Quoted\n",
        "---",
    );
    app.editor()
        .apply(Intent::Type(prose.to_owned()))
        .expect("type prose");

    let lines: Vec<(u64, u64)> = {
        let mut out = Vec::new();
        let mut start = 0u64;
        for part in prose.split('\n') {
            let end = start + part.len() as u64;
            out.push((start, end));
            start = end + 1;
        }
        out
    };

    // Exercise every formatting ToolbarAction + hyperlink across distinct ranges.
    app.editor()
        .select_range(lines[0].0, lines[0].1)
        .expect("line 0");
    apply_toolbar_action(&mut app, ToolbarAction::Heading1).expect("H1");

    app.editor()
        .select_range(lines[1].0, lines[1].1)
        .expect("line 1");
    apply_toolbar_action(&mut app, ToolbarAction::Heading2).expect("H2");

    app.editor()
        .select_range(lines[2].0, lines[2].1)
        .expect("line 2");
    apply_toolbar_action(&mut app, ToolbarAction::Heading3).expect("H3");

    let l3 = lines[3].0;
    app.editor().select_range(l3, l3 + 5).expect("serif");
    apply_toolbar_action(&mut app, ToolbarAction::CycleFamily).expect("Serif");

    app.editor().select_range(l3 + 6, l3 + 11).expect("size");
    apply_toolbar_action(&mut app, ToolbarAction::CycleSize).expect("17px");

    app.editor().select_range(l3 + 12, l3 + 16).expect("bold");
    apply_toolbar_action(&mut app, ToolbarAction::ToggleBold).expect("Bold");

    app.editor().select_range(l3 + 17, l3 + 23).expect("italic");
    apply_toolbar_action(&mut app, ToolbarAction::ToggleItalic).expect("Italic");

    app.editor().select_range(l3 + 24, l3 + 29).expect("under");
    apply_toolbar_action(&mut app, ToolbarAction::ToggleUnderline).expect("Underline");

    app.editor().select_range(l3 + 30, l3 + 36).expect("strike");
    apply_toolbar_action(&mut app, ToolbarAction::ToggleStrike).expect("Strike");

    app.editor().select_range(l3 + 37, l3 + 41).expect("code");
    apply_toolbar_action(&mut app, ToolbarAction::ToggleCode).expect("Code");

    app.editor()
        .select_range(l3 + 42, lines[3].1)
        .expect("link");
    app.editor()
        .apply(Intent::SetLink(Some(
            "https://holonomy.invalid/l3".to_owned(),
        )))
        .expect("Link");

    app.editor()
        .select_range(lines[4].0, lines[4].1)
        .expect("bullet + align");
    apply_toolbar_action(&mut app, ToolbarAction::ToggleBullet).expect("Bullet");
    apply_toolbar_action(&mut app, ToolbarAction::CycleAlign).expect("Align Center");

    app.editor()
        .select_range(lines[5].0, lines[5].1)
        .expect("ordered + quote");
    apply_toolbar_action(&mut app, ToolbarAction::ToggleNumbered).expect("Numbered");
    apply_toolbar_action(&mut app, ToolbarAction::CycleQuote).expect("Quote");

    app.editor()
        .select_range(lines[6].0, lines[6].1)
        .expect("hr");
    apply_toolbar_action(&mut app, ToolbarAction::ToggleHr).expect("HR");

    assert!(app.draw().expect("paint styled document"));
    app.editor().flush().expect("seal styled manifest");
    let expected_styles = app.editor().typing().spine().styles().clone();
    assert!(!expected_styles.is_empty());
    assert_eq!(
        expected_styles.links(),
        &["https://holonomy.invalid/l3".to_owned()]
    );
    drop(app);
    drop(doc);

    let reopened = Document::open(&path.0, PASSPHRASE).expect("reopen styled doc");
    assert!(reopened.verify().is_ok());
    let reopened_spine = reopened.spine().expect("reopened spine");
    assert_eq!(reopened_spine.styles(), &expected_styles);
    assert_eq!(all_bytes(&reopened), prose.as_bytes());
}

#[test]
fn formatting_torture_corpus_across_slab_boundaries_single_undo() {
    use holonomy_eye::{FontSet, View};
    use holonomy_shell::{Intent, QUIET_MD_THEME};
    use holonomy_text::{Alignment, FontFamily, Heading, StylePatch};

    let path = temp_path("format-torture");
    let doc = create(&path.0);

    // Build 6 explicit sealed slabs (`>= 6` Slab nodes) with mixed ASCII, CJK, RTL Arabic, and emoji
    // so formatting spans cross real encrypted slab boundaries.
    let mut typing = Typing::new(doc.sink(), store::CAPACITY);
    let mut chunk = String::new();
    for i in 0..6 {
        chunk.clear();
        chunk.push_str(&format!("Slab-{i}: Alpha βeta العربية 日本語 🦀 "));
        while chunk.len() < store::CAPACITY / 2 {
            chunk.push_str("prose-boundary-segment ");
        }
        chunk.push('\n');
        typing.insert(chunk.as_bytes()).expect("insert slab chunk");
        typing.flush().expect("seal distinct slab leaf");
    }
    drop(typing);
    let spine = doc.spine().expect("spine");
    assert!(
        spine.slab_count() >= 6,
        "need >= 6 slabs, got {}",
        spine.slab_count()
    );
    let baseline_styles = spine.styles().clone();
    assert!(baseline_styles.is_empty());

    let fonts = FontSet::embedded(QUIET_MD_THEME.body_size_px).expect("fonts");
    let residency = ResidentSet::new(doc.source(), store::BUDGET, store::SLACK).expect("residency");
    let mut editor = Editor::new(
        Typing::resume(doc.sink(), spine, store::CAPACITY),
        residency,
        fonts,
        Fixed::units(680),
        Fixed::units(800),
    );
    editor.set_view(View::IDENTITY);

    let total_len = editor.document_len();
    let s1 = editor.typing().spine().slab_span(1).expect("slab 1");
    let s3 = editor.typing().spine().slab_span(3).expect("slab 3");
    let s4 = editor.typing().spine().slab_span(4).expect("slab 4");
    let cross_lo = s1.offset() + (s1.len() / 2);
    let cross_hi = (s4.offset() + (s4.len() / 2)).min(total_len);

    // Apply multiple overlapping cross-slab formats as ONE single-undo root transaction.
    let changed = editor
        .apply_styles_batch(&[
            (
                cross_lo,
                cross_hi,
                StylePatch::new()
                    .with_bold(true)
                    .with_family(FontFamily::Serif),
            ),
            (
                s1.offset(),
                s3.offset() + 64,
                StylePatch::new()
                    .with_italic(true)
                    .with_heading(Heading::H2)
                    .with_alignment(Alignment::Center),
            ),
            (
                s3.offset().saturating_sub(32),
                cross_hi,
                StylePatch::new().with_underline(true),
            ),
        ])
        .expect("cross-slab batch format");
    assert!(changed);
    let formatted_styles = editor.typing().spine().styles().clone();
    assert!(formatted_styles.runs().len() >= 3);
    editor.flush().expect("persist cross-slab formatted spine");

    // Reopen independently to confirm multi-slab formatting persisted cleanly.
    let reopened_fmt = Document::open(&path.0, PASSPHRASE).expect("reopen formatted");
    assert_eq!(
        reopened_fmt.spine().expect("spine").styles(),
        &formatted_styles
    );
    drop(reopened_fmt);

    // Single Undo must revert EVERY cross-slab style span in O(1) without leaving stale runs.
    assert!(editor.apply(Intent::Undo).is_ok());
    assert_eq!(editor.typing().spine().styles(), &baseline_styles);

    // Single Redo restores the exact same cross-slab StyleMap.
    assert!(editor.apply(Intent::Redo).is_ok());
    assert_eq!(editor.typing().spine().styles(), &formatted_styles);
}

#[test]
#[ignore = "24-year (8,760 witness) fresh-user lifecycle drill; run in CI and qualify script"]
fn twenty_four_year_fresh_user_lifecycle_drill() {
    const DAYS: u64 = 24 * 365; // 8,760 daily witnesses
    let dir = fast_temp_path("24yr-dir");
    let _ = fs::remove_file(&dir.0);
    let root = dir.0.with_extension("drill");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("create temp drill root");

    let original = root.join("alice.wavefunction");
    let backup = root.join("backup.wavefunction");
    let compacted = root.join("alice-compacted.wavefunction");

    // 1. Fresh creation + 24 years (8,760 daily witnesses).
    let doc = Document::create(&original, PASSPHRASE).expect("create 24-year doc");
    let entries: Vec<String> = (1..DAYS).map(|d| format!("year24-day-{d}\n")).collect();
    doc.append_witnessed_entries(entries.iter().map(String::as_bytes))
        .expect("append 8,759 witnesses");
    let spine = doc.spine().expect("spine");
    let mut typing = Typing::resume(doc.sink(), spine, store::CAPACITY);
    let len = typing.document_len();
    typing
        .insert_at(len, format!("year24-day-{DAYS}\n").as_bytes())
        .expect("final day");
    typing.flush().expect("seal final day");
    doc.add_bookmark(DAYS, 1_764_000_000, "anniversary-24")
        .expect("bookmark year 24");
    drop(typing);
    assert_eq!(
        doc.verify().expect("verify original").segments_verified,
        DAYS
    );

    // 2. Back up container, wipe original file completely, reopen backup in clean state.
    fs::copy(&original, &backup).expect("backup copy");
    drop(doc);
    fs::remove_file(&original).expect("wipe original");

    let restored =
        Document::open(&backup, PASSPHRASE).expect("reopen backup with passphrase alone");
    assert_eq!(restored.witness_history().len() as u64, DAYS + 1);
    assert_eq!(restored.bookmarks().len(), 1);
    assert_eq!(
        restored
            .verify()
            .expect("verify restored")
            .segments_verified,
        DAYS
    );

    // 3. Continue writing + compact + re-verify.
    let mut t2 = Typing::resume(
        restored.sink(),
        restored.spine().expect("spine"),
        store::CAPACITY,
    );
    let end2 = t2.document_len();
    t2.insert_at(end2, b"year25-day-1 continued cleanly\n")
        .expect("continue typing");
    t2.flush().expect("seal year 25");
    drop(t2);
    assert_eq!(
        restored
            .verify()
            .expect("verify continued")
            .segments_verified,
        DAYS + 1
    );

    let final_doc = restored
        .compact_to(&compacted, PASSPHRASE)
        .expect("compact after 24yr lifecycle");
    drop(restored);
    assert_eq!(final_doc.witness_count(), 1);
    assert_eq!(
        final_doc
            .verify()
            .expect("verify compacted")
            .segments_verified,
        DAYS + 1
    );
    drop(final_doc);
    let _ = fs::remove_dir_all(&root);
    println!("HOLONOMY_24YR_LIFECYCLE witnesses={} status=ok", DAYS + 2);
}
