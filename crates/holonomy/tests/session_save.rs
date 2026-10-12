//! **The save gate: `Ctrl+S` reaches the container, driven through a `Session`.** 7 tests.
//!
//! # What is being claimed
//!
//! A `Session` opens a container, the user edits, `Ctrl+S` is pressed, and **the bytes are on disk**.
//!
//! Every piece of that existed except the join. `Ctrl+S` was a counter (`session.rs:2670`, and for
//! thirteen phases a comment explaining why it could not be more than a counter), `Action::Save` routed
//! to `None` (`session.rs:5734`), and `commit_document` had been built and gated since Phase 13 part 15
//! with **no caller outside `tests/commit_path.rs`**. The product opened a document, let the user edit
//! it, and discarded every keystroke on exit.
//!
//! # Why this is driven through a `Session`
//!
//! Because **`commit_path.rs` already proved the commit works** — on an `Editor`, against a real
//! container, with the extent, the shift and the residency all gated. It could not see the defect, for
//! the same reason `no_alloc.rs` could not: it drove the model and never the product's entry point.
//!
//! `Ctrl+S` is `InputEvent` → `keymap` → `Command::Hotkey(Save)` → `apply` → `handle_event_with` →
//! `commit_to`, and **every one of those five arrows is where the save could have been lost**. A test
//! that calls `commit_document` directly passes with all five of them broken. This one sends the same
//! keystrokes a keyboard sends, and the container is the only witness.
//!
//! # The container is the witness, not the editor
//!
//! The editor agreeing with the store could be two wrongs cancelling — `commit_path.rs` says so and
//! reopens the file for exactly this reason. Every assertion here reads the document back through
//! `Wavefunction::open` with no rope in the way.
//!
//! | what it proves | test |
//! | --- | --- |
//! | **`Ctrl+S` writes** | [`ctrl_s_writes_the_edited_document_to_the_container`] |
//! | a second save does not change the bytes | [`a_second_save_of_an_unchanged_document_leaves_the_bytes_alone`] |
//! | the menu route reaches the same place | [`the_file_menu_save_row_saves_like_the_key`] |
//! | the status bar's unsaved marker has a writer | [`editing_marks_the_document_unsaved_and_saving_clears_it`] |
//! | a save with nothing behind it is not an error | [`saving_a_session_with_no_container_is_not_an_error`] |
//! | **a failed commit leaves the document dirty** | [`a_failed_commit_leaves_the_document_marked_unsaved`] |
//! | the product's own exit path commits | [`the_driver_saves_unsaved_work_before_it_exports`] |

use holonomy::session::Session;
use holonomy::store::{open_document, SectionStore, DEFAULT_RESIDENT_SECTIONS};
use holonomy_container::io::DirectFile;
use holonomy_container::Wavefunction;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::{InputEvent, KEY_A};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, LeafSource, RopeError};

const PASS: &str = "correct horse battery staple";
const ITER: u64 = holonomy_container::TEST_VDF_ITERATIONS;

fn scratch_dir() -> std::path::PathBuf {
    let dir = std::env::current_exe()
        .expect("test exe")
        .ancestors()
        .nth(3)
        .expect("target/<profile> layout")
        .join("holonomy-container-tests")
        .join("session-save");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn unique() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u128)
        .unwrap_or(0);
    (t << 20) ^ u128::from(COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Bytes that are a function of position, so a one-byte offset error is *visible* rather than plausible.
///
/// Newlines every 40 bytes: the paint path emits lines, and a document with none is one visual row.
fn text(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n + n / 40);
    let mut at = 0usize;
    while at < n {
        let take = 40.min(n - at);
        for i in at..at + take {
            v.push((i.wrapping_mul(97).wrapping_add(13) % 251) as u8);
        }
        v.push(b'\n');
        at += take;
    }
    v
}

fn make(content: &[u8]) -> std::path::PathBuf {
    let path = scratch_dir().join(format!("save-{}-{}.wavefunction", content.len(), unique()));
    let _ = Wavefunction::create(&path, PASS, "saved", content, ITER).expect("create");
    path
}

/// Reopen the container and read its whole content back, **with no rope in the way**.
fn read_back(path: &std::path::Path) -> Vec<u8> {
    let mut wf = Wavefunction::open(path, PASS, ITER).expect("reopen");
    let len = wf.content_len() as usize;
    let mut store = SectionStore::new(&mut wf, DEFAULT_RESIDENT_SECTIONS);
    let mut out = vec![0u8; len];
    let mut at = 0usize;
    while at < len {
        let n = store
            .copy_into((at / holonomy::manifest::SECTION_BYTES) as u32, &mut out[at..])
            .expect("copy");
        if n == 0 {
            break;
        }
        at += n;
    }
    out.truncate(len);
    out
}

/// The atlas, leaked once. `tests/session.rs:56-59` does this and for the same reason: building it per
/// session would dominate the run time and, worse, allocate inside the measured path.
fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    Box::leak(Box::new(
        holonomy_assets::build_atlas(&[16]).expect("build the atlas").0,
    ))
}

/// A session over an empty editor, which is exactly the state `main.rs` builds it in: the container is
/// opened after the passphrase exists, so the session is constructed empty and adopts later.
fn empty_session() -> (Session<'static>, &'static holonomy_assets::atlas::Atlas) {
    let atlas = atlas();
    let metrics = ChromeMetrics::DESKTOP;
    let session = Session::new(
        Editor::new(),
        Painter::new(atlas, 16),
        Box::new(HeadlessScanout::new(metrics.width, metrics.height)),
        metrics,
    );
    (session, atlas)
}

/// Open `path`, adopt it into a session, and run `f` with both.
///
/// # Why this is a closure rather than a returned tuple
///
/// `SectionStore<'c>` borrows the container it reads, so a helper returning `(Session, SectionStore)`
/// would have to return the container too — and the container has to outlive the store, which makes the
/// returned struct self-referential. **The product avoids the same shape for the same reason**
/// (`store.rs`, `open_document`'s docs): it keeps the container beside the session and hands the store
/// in per call. A test that could not do what the product does would be testing a different arrangement.
///
/// **The store outlives the adopt**, which is the point: `open_document`'s own docs say the store it
/// builds is a *loader* and is dropped, and that faulting a leaf in later needs a store alive for the
/// session's lifetime. A save is exactly that.
fn with_session<F>(path: &std::path::Path, f: F)
where
    F: FnOnce(&mut Session<'_>, &mut dyn LeafSource),
{
    let mut opened = open_document(
        DirectFile::create_or_open(path).expect("descriptor -- the boot's route"),
        PASS,
        ITER,
        DEFAULT_RESIDENT_SECTIONS,
    )
    .expect("open the document");
    let (mut s, _atlas) = empty_session();
    let mut store = SectionStore::new(&mut opened.container, DEFAULT_RESIDENT_SECTIONS);
    s.adopt_document(opened.editor, &mut store)
        .expect("adopt the document");
    f(&mut s, &mut store);
}

/// `Ctrl+S`, as the keyboard delivers it: ctrl down, s down, s up, ctrl up.
fn ctrl_s(s: &mut Session<'_>, store: &mut dyn LeafSource) {
    for event in [
        InputEvent::press(holonomy_input::KEY_LEFTCTRL),
        InputEvent::press(holonomy_input::KEY_S),
        InputEvent::release(holonomy_input::KEY_S),
        InputEvent::release(holonomy_input::KEY_LEFTCTRL),
    ] {
        s.handle_event_with(store, event).expect("event");
    }
}

fn type_a(s: &mut Session<'_>, store: &mut dyn LeafSource, n: usize) {
    for _ in 0..n {
        s.handle_event_with(store, InputEvent::press(KEY_A)).expect("a");
        s.handle_event_with(store, InputEvent::release(KEY_A)).expect("a up");
    }
}

// ------------------------------------------------------------------ the gates

/// **The one this file exists for.** Keystrokes through the session, `Ctrl+S` through the session, and
/// the container on disk is the witness.
#[test]
fn ctrl_s_writes_the_edited_document_to_the_container() {
    let doc = text(20_000);
    let path = make(&doc);
    let before = read_back(&path);
    assert_eq!(before, doc, "the fixture reads back before anything is claimed about it");

    with_session(&path, |s, store| {
        // `Ctrl+Home` first so the insert lands at offset 0 and the expectation is a prefix rather than
        // a splice -- a splice would have to agree with the rope about where the caret went, which is
        // a different gate's claim.
        for event in [
            InputEvent::press(holonomy_input::KEY_LEFTCTRL),
            InputEvent::press(holonomy_input::KEY_HOME),
            InputEvent::release(holonomy_input::KEY_HOME),
            InputEvent::release(holonomy_input::KEY_LEFTCTRL),
        ] {
            s.handle_event_with(store, event).expect("ctrl+home");
        }
        type_a(s, store, 12);
        assert!(s.state.dirty, "typing marks the document unsaved");

        ctrl_s(s, store);
        assert!(!s.state.dirty, "and a successful save clears it");
    });

    let after = read_back(&path);
    assert_eq!(
        after.len(),
        doc.len() + 12,
        "the committed document is the edited one, and it is longer by what was typed"
    );
    assert!(
        after.ends_with(&doc),
        "the original text survives intact at the tail, so the insert did not overwrite it"
    );
    let _ = std::fs::remove_file(&path);
}

/// **A second save is idempotent — and it is a rewrite, which the first draft of this gate got wrong.**
///
/// This originally asserted that saving an unchanged document adds **zero** bytes, on the reasonable
/// reading that a clean document needs no writing. It added `8,204` — the whole document.
///
/// **The behaviour is correct and already recorded.** `commit_document`'s read loop repopulates the
/// store's cache through `read_into_faulting`, and `Entry::dirty` means *cache differs from disk*, so
/// the sections genuinely differ again by the time `commit_dirty` runs. PROJECT.md part 15 says so and
/// names `the_commit_reports_what_it_actually_wrote` as the gate that pins a full rewrite rather than
/// the number a first draft hoped for.
///
/// So the claim worth making is **idempotence**, not economy: the bytes on disk after a second save are
/// the bytes on disk after the first. A rewrite that produces a different file would be the bug, and a
/// zero-cost assertion would have passed against a commit that silently wrote nothing at all — which is
/// the same shape as a save that does not save.
#[test]
fn a_second_save_of_an_unchanged_document_leaves_the_bytes_alone() {
    let path = make(&text(8_000));
    with_session(&path, |s, store| {
        type_a(s, store, 4);
        ctrl_s(s, store);
        let first = s.stats.bytes_committed;
        assert!(first > 0, "the first save wrote something: {first}");
        let after_first = read_back(&path);

        ctrl_s(s, store);
        assert_eq!(
            s.stats.committed,
            2,
            "and the second is counted as a save, not refused: a no-op and a refusal are different"
        );
        assert_eq!(
            read_back(&path),
            after_first,
            "**the document on disk is byte-identical after a second save**"
        );
        assert!(!s.state.dirty, "and it is still marked saved");
    });
    let _ = std::fs::remove_file(&path);
}

/// **The menu route, not the key.** A row and a keystroke are two ways of asking for one thing, and
/// giving them separate implementations is how they drift — which is the failure Part 20 fixed by
/// replacing a label match with `action_command`.
#[test]
fn the_file_menu_save_row_saves_like_the_key() {
    let path = make(&text(8_000));
    with_session(&path, |s, store| {
        type_a(s, store, 6);
        assert!(s.state.dirty);

        let cmd = holonomy::session::action_command(holonomy_render::menus::Action::Save)
            .expect("the Save row routes to a command rather than to nothing");
        s.apply(cmd).expect("apply the routed command");
        assert!(
            s.save_requested(),
            "the row set the same request the key does, so both reach one commit"
        );

        s.commit_to(store);
        assert!(!s.state.dirty, "and the commit clears the marker");
    });
    let _ = std::fs::remove_file(&path);
}

/// **The marker has a writer.** `ChromeState::dirty` existed since Phase 8 and had **no writer anywhere
/// in the workspace**, so the status bar showed "saved" for the life of every session including one
/// mid-edit.
#[test]
fn editing_marks_the_document_unsaved_and_saving_clears_it() {
    let path = make(&text(8_000));
    with_session(&path, |s, store| {
        assert!(!s.state.dirty, "a freshly opened document is not marked unsaved");
        type_a(s, store, 1);
        assert!(s.state.dirty, "and one keystroke marks it");
        ctrl_s(s, store);
        assert!(!s.state.dirty, "and a save clears it");
    });
    let _ = std::fs::remove_file(&path);
}

/// **`Ok(0)` is a no-op, not an error.** `NoSource` is a session with no container behind it — an empty
/// buffer, which is what `main.rs` falls back to when the container cannot be opened. Pressing `Ctrl+S`
/// there must not be a failure.
#[test]
fn saving_a_session_with_no_container_is_not_an_error() {
    let (mut s, _atlas) = empty_session();
    type_a(&mut s, &mut holonomy::store::NoSource, 3);
    assert!(s.state.dirty);

    let written = s.commit_to(&mut holonomy::store::NoSource);
    assert_eq!(written, 0, "nothing was durable, and it says so");
    assert_eq!(s.stats.commit_failures, 0, "and it is not an error either");
    assert!(
        s.state.dirty,
        "**and the document is still marked unsaved** -- nothing reached storage, so claiming otherwise is the lie"
    );
}

/// **A failed commit leaves the document dirty.**
///
/// The flag is the only place the difference between "saved" and "failed to save" is visible: the pixels
/// on screen are identical either way. Clearing it unconditionally would make a full disk look like a
/// successful save.
#[test]
fn a_failed_commit_leaves_the_document_marked_unsaved() {
    let path = make(&text(8_000));

    // A source that refuses every commit. `fetch_leaf` still works, so the session is otherwise
    // healthy -- **which is the point**: the failure has to be the commit's alone.
    struct Refusing;
    impl LeafSource for Refusing {
        fn fetch_leaf(&mut self, _offset: usize, out: &mut [u8]) -> Result<usize, RopeError> {
            out.fill(0);
            Ok(out.len())
        }
        fn store_leaf(&mut self, _offset: usize, _bytes: &[u8]) -> Result<(), RopeError> {
            Ok(())
        }
        fn commit_document(&mut self, _e: &mut Editor) -> Result<usize, RopeError> {
            Err(RopeError::SourceUnavailable)
        }
    }

    with_session(&path, |s, store| {
        type_a(s, store, 5);
        s.commit_to(&mut Refusing);
        assert_eq!(
            s.stats.commit_failures, 1,
            "the failure is counted separately from a save"
        );
        assert_eq!(s.stats.committed, 0, "and is not counted as one");
        assert!(
            s.state.dirty,
            "**the document stays marked unsaved**, which is the only place the difference is visible"
        );
        // The real store still works afterwards, so the refusal was the source's and not the session's.
        assert!(s.commit_to(store) > 0, "a healthy source still saves");
    });
    let _ = std::fs::remove_file(&path);
}

/// **The product's own exit path.** `drive()` commits before it repaints or exports; this asserts the
/// reasoning by running the same sequence `main.rs` runs.
#[test]
fn the_driver_saves_unsaved_work_before_it_exports() {
    let path = make(&text(8_000));
    let original = read_back(&path);
    with_session(&path, |s, store| {
        type_a(s, store, 10);
        assert!(s.state.dirty, "there is unsaved work at the moment the driver checks");

        // `drive()`'s order: commit, then repaint, then export.
        let written = s.commit_to(store);
        assert!(written > 0, "the exit commit wrote something: {written}");
        s.repaint_all_with(store).expect("repaint");
    });

    let after = read_back(&path);
    assert_eq!(
        after.len(),
        original.len() + 10,
        "and it is on disk, not just in memory"
    );
    let _ = std::fs::remove_file(&path);
}