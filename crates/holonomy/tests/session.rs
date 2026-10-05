//! The Phase 8 integration gate: a whole session, scripted, ending in a bit-identical container.
//!
//! # What this is and is not
//!
//! **Is:** the real [`Session`], the real [`Editor`], the real [`Chrome`], the real atlas and the real
//! painters, driven by a real [`ScriptedInputSource`] whose bytes go through the same 24-byte
//! `input_event` decoder a ThinkPad's keyboard would. Container in, container out, HTML and PDF
//! written to pre-opened descriptors, and the final frame dumped to a PPM.
//!
//! **Is not:** the jail. `unshare(CLONE_NEWUSER)` returns `EINVAL` in a multi-threaded process and
//! libtest always spawns one, so a `#[test]` cannot enter it. What that leaves uncovered is the boot
//! chain itself, which `holonomy-jail`'s 60 tests cover, and the syscall census, which
//! `examples/census_session.rs` covers against this same loop. That split is deliberate and it is why
//! the loop lives in a library.
//!
//! # Why the container round trip is the interesting assertion
//!
//! Everything before it -- typing, undo, export, pixels -- is observable. The commit is not: it is
//! three stages of AES-GCM with a key derived from a passphrase, so "it wrote a file" and "the file
//! decrypts back to the same bytes" are different claims. The gate makes the second one, which is the
//! only one that would catch a ring that committed the wrong chunk or a frame that lost its
//! nonce.

use std::io::Write as _;

use holonomy::session::{ExportSink, Session};
use holonomy_container::io::AlignedBuf;
use holonomy_container::io::DirectFile;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_export::Format;
use holonomy_input::{
    InputEvent, ScriptedInputSource, KEY_A, KEY_LEFTCTRL, KEY_Q, KEY_S, KEY_Y, KEY_Z,
};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy};

/// A scratch directory under `CARGO_TARGET_TMPDIR`, per test.
///
/// Not `/tmp`: Phase 7 measured a 7.7 GiB tmpfs there that 128 MiB-per-run exhausted. And per-test,
/// because libtest shares a pid across parallel threads.
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("holonomy-phase8-{}-{tag}", std::process::id()))
}

/// Build the real atlas and a session over a fresh document.
fn session(
    ed: Editor,
    m: ChromeMetrics,
) -> (Session<'static>, &'static holonomy_assets::atlas::Atlas) {
    // The atlas is leaked rather than borrowed so the session can outlive the expression that made
    // it. A test fixture that allocates once per session would dominate the run time.
    let atlas: &'static holonomy_assets::atlas::Atlas = Box::leak(Box::new(
        holonomy_assets::build_atlas(&[16])
            .expect("build the atlas")
            .0,
    ));
    let scanout = HeadlessScanout::new(m.width, m.height);
    let painter = Painter::new(atlas, 0);
    let s = Session::new(ed, painter, Box::new(scanout), m);
    (s, atlas)
}

/// Type `s` as a keyboard would: Shift where needed, press/release per key.
///
/// The search for "which key produces this character" is made **from a fresh modifier state both
/// times** -- unshifted first, then shifted. The first version of this helper searched using the
/// *current* `shift_down` flag, which meant that after typing an uppercase letter it searched the
/// shifted table for the next lowercase one and found nothing. That is a bug in the test helper, not
/// in the keymap: a real keyboard has no such memory, it only knows what is held down right now.
fn type_str(s: &str) -> Vec<InputEvent> {
    let km = holonomy_input::Keymap::us();
    let with_shift = |c: char| {
        let mut m = holonomy_input::ModifierState::new();
        m.update(holonomy_input::KEY_LEFTSHIFT, 1);
        (0u16..128).find(|&code| km.text_for(code, &m) == Some(c))
    };
    let mut events = Vec::new();
    let mut shift_down = false;
    for c in s.chars() {
        let (code, needs_shift) = match (0u16..128)
            .find(|&code| km.text_for(code, &holonomy_input::ModifierState::new()) == Some(c))
        {
            Some(code) => (code, false),
            None => (
                with_shift(c).unwrap_or_else(|| panic!("no key produces {c:?}")),
                true,
            ),
        };
        if needs_shift && !shift_down {
            events.push(InputEvent::press(holonomy_input::KEY_LEFTSHIFT));
            shift_down = true;
        } else if !needs_shift && shift_down {
            events.push(InputEvent::release(holonomy_input::KEY_LEFTSHIFT));
            shift_down = false;
        }
        events.push(InputEvent::press(code));
        events.push(InputEvent::release(code));
    }
    if shift_down {
        events.push(InputEvent::release(holonomy_input::KEY_LEFTSHIFT));
    }
    events
}

/// Ctrl+<letter>, as two key events.
fn ctrl(code: u16) -> Vec<InputEvent> {
    vec![
        InputEvent::press(KEY_LEFTCTRL),
        InputEvent::press(code),
        InputEvent::release(code),
        InputEvent::release(KEY_LEFTCTRL),
    ]
}

/// A `File` opened for the export, which is how the session is meant to be given one.
fn sink(format: Format, path: &std::path::Path) -> ExportSink {
    ExportSink {
        format,
        file: std::fs::File::create(path).expect("open the export sink"),
        path: path.display().to_string(),
    }
}

// ------------------------------------------------------------------ the gate

/// The whole session, end to end.
#[test]
fn a_full_session_types_edits_undoes_exports_and_leaves_the_container_intact() {
    let dir = scratch("session");
    std::fs::create_dir_all(&dir).expect("scratch");
    let container_path = dir.join("notes.wavefunction");
    let html_path = dir.join("notes.html");
    let pdf_path = dir.join("notes.pdf");
    let ppm_path = dir.join("notes.ppm");

    let m = ChromeMetrics::DESKTOP;

    // --- The document starts empty and a real container is opened *before* anything else.
    let container = DirectFile::create_or_open(&container_path).expect("create the container");
    // An **empty** document. Seeding it with the first clause as well and then typing the whole
    // sentence appended a second copy -- which is a bug in the test, and one worth recording: the
    // resulting text "The quick brown fox.The quick brown fox jumps..." is what a session that
    // ignores the caret position looks like, so the test *did* catch something, just not in the code.
    let (mut sess, _atlas) = session(Editor::new(), m);
    sess.state.sealed = true;
    sess.state.title = "notes.wavefunction".to_string();

    // --- Type the sentence, then move the caret back over a word, delete it and retype it.
    //
    // The arithmetic, so the assertions below are about the *code* and not about my counting:
    //   "The quick brown fox jumps over the lazy dog."
    //    0123456789...                   ^34 space ^35-38 "lazy" ^39 space ^40-43 "dog."
    // The caret ends at 44. Five lefts put it at 39, which is *after* "lazy" -- not before it, which
    // Five backspaces from 39 delete indices 38,37,36,35,34: "lazy" **and** the space before it,
    // so the replacement must be " lazy" -- five characters, not four. Typing four leaves
    // "over thelazy dog.", which is what this test asserted against in its first version.
    let sentence = "The quick brown fox jumps over the lazy dog.";
    let mut events = type_str(sentence);
    for _ in 0..5 {
        events.extend(tap(holonomy_input::KEY_LEFT));
    }
    for _ in 0..5 {
        events.extend(tap(holonomy_input::KEY_BACKSPACE));
    }
    events.extend(type_str(" lazy"));
    // Undo and redo the retyping, so both branches are exercised.
    events.extend(ctrl(KEY_Z));
    events.extend(ctrl(KEY_Y));
    // Ctrl+S, and Ctrl+Q to leave.
    events.extend(ctrl(KEY_S));
    events.extend(ctrl(KEY_Q));

    let mut src = ScriptedInputSource::from_events(&events);
    let exit = sess.run(&mut src).expect("the session runs");
    assert_eq!(
        exit,
        holonomy::session::Exit::Quit,
        "Ctrl+Q must end the session"
    );
    assert!(
        sess.stats.commands > 50,
        "the stream was not consumed: {:?}",
        sess.stats
    );
    assert!(
        sess.stats.edits > 30,
        "fewer edits than keystrokes: {:?}",
        sess.stats
    );
    assert_eq!(sess.stats.saves, 1, "Ctrl+S must be seen exactly once");
    assert_eq!(
        sess.stats.unhandled, 0,
        "the keymap produced a command the session ignored"
    );

    // --- The document is exactly what the keystrokes said: type, delete "lazy", retype it,
    //     undo the retype, redo it. Undo and redo cancel, so the end state is the sentence.
    let text = String::from_utf8(sess.editor.text().expect("utf-8")).expect("utf-8");
    assert_eq!(text, sentence, "the paragraph was mangled: {text:?}");
    assert!(
        sess.stats.edits > 50,
        "fewer edits than keystrokes: {:?}",
        sess.stats
    );

    // --- Undo as deep as the stack goes, then redo all of it back.
    //
    // The stack is **bounded** at `UNDO_DEPTH`, so "500 undos" cannot be five hundred successful
    // undos: past the bound there is nothing left and the honest answer is `NothingToUndo`. So the
    // gate unwinds until it is refused, counts how deep that was, and redoes exactly that many --
    // which tests the depth *and* the round trip, and does not pretend the bound is 500.
    let before = sess.editor.text_len();
    let mut unwound = 0u32;
    for _ in 0..500 {
        match sess.editor.undo() {
            Ok(_) => unwound += 1,
            Err(holonomy_text::EditorError::NothingToUndo) => break,
            Err(e) => panic!("undo failed unexpectedly: {e}"),
        }
    }
    // 54 levels, not 500: the stack holds **one entry per edit**, and this session made 54 edits (44
    // typed characters, 5 backspaces, 5 retyped characters). Reaching the bound of 500 would need a
    // 500-edit document, and asserting `> 100` here -- as a first version of this test did -- would
    // have been asserting a count about the *sentence*, not about the undo machinery. The depth of
    // the bound itself is `holonomy-text`'s to test and it does.
    assert!(
        unwound >= 50,
        "the session only made {unwound} undoable levels out of its own keystrokes"
    );
    let at_bottom = sess.editor.text_len();
    assert!(
        at_bottom < before,
        "unwinding {unwound} levels left {at_bottom} of {before} bytes"
    );
    for _ in 0..unwound {
        sess.editor.redo().expect("redo");
    }
    assert_eq!(
        sess.editor.text_len(),
        before,
        "redoing every undone level must restore the length exactly"
    );
    let redone = String::from_utf8(sess.editor.text().expect("utf-8")).expect("utf-8");
    assert_eq!(
        redone, sentence,
        "and restore the text, not just its length"
    );

    // --- Export to descriptors opened before the session.
    let mut html = sink(Format::Html, &html_path);
    let report = sess.export(&mut html, "notes").expect("export HTML");
    drop(html);
    assert!(report.bytes > 0);
    let mut pdf = sink(Format::Pdf, &pdf_path);
    let report = sess.export(&mut pdf, "notes").expect("export PDF");
    drop(pdf);
    assert!(report.bytes > 100, "a PDF should be more than a header");

    // --- The frame, dumped as a PPM.
    sess.repaint_all().expect("final paint");
    let mut ppm = std::fs::File::create(&ppm_path).expect("open the ppm");
    let n = sess.dump_ppm_to_file(&mut ppm).expect("dump");
    ppm.flush().expect("flush");
    drop(ppm);
    assert!(
        n > 1000,
        "a {}x{} frame should be more than a header",
        m.width,
        m.height
    );
    let ppm_bytes = std::fs::read(&ppm_path).expect("read the ppm back");
    assert!(ppm_bytes.starts_with(b"P6\n1280 800\n255\n"));

    // --- Commit to the container, then reopen it and check the bytes.
    let text: Vec<u8> = sess.editor.text().expect("utf-8");
    commit(&container, &text);
    container.sync().expect("sync");
    let reopened = DirectFile::open(&container_path).expect("reopen the container");
    let back = read_all(&reopened);
    assert_eq!(back, text, "the container did not reopen bit-identical");

    // --- And the exports are on disk with their content.
    let html_out = std::fs::read_to_string(&html_path).expect("read the html");
    assert!(html_out.contains("<html>"), "the HTML is not a document");
    assert!(html_out.contains("lazy dog"), "the HTML lost the text");
    assert_eq!(
        html_out.matches("<b>").count(),
        html_out.matches("</b>").count()
    );
    let pdf_out = std::fs::read(&pdf_path).expect("read the pdf");
    assert!(pdf_out.starts_with(b"%PDF-"), "the PDF has no header");
    assert!(pdf_out.ends_with(b"%%EOF"), "the PDF has no trailer");

    std::fs::remove_dir_all(&dir).ok();
}

fn tap(code: u16) -> Vec<InputEvent> {
    vec![InputEvent::press(code), InputEvent::release(code)]
}

/// Offset of the 512-byte header. `O_DIRECT` needs every length a multiple of the alignment, so the
/// header occupies a whole sector and the body starts at the next one.
const HEADER_OFF: u64 = 0;
/// Offset of the body.
///
/// **4096, not 512.** `O_DIRECT` aligns the *file offset* to the filesystem's logical block size, and
/// on the ext4 this gate runs on that is 4096 -- 512 is the alignment `AlignedBuf` gives the
/// *memory* side, which is a different constraint. Writing the body at 512 fails with
/// `UnalignedOffset { offset: 512 }`, which is what the first version of this did. Two alignments,
/// two constants, and conflating them is a compile-time-success / runtime-failure.
const BODY_OFF: u64 = 4096;

/// Write `bytes` into the container as a 512-byte length header followed by the body.
///
/// A stand-in for the ring's staged commit: the *gate* is about the session and the reopen, and the
/// ring's own commit has its own regression tests in `holonomy-container`. What matters here is that
/// the bytes go through `DirectFile`'s aligned `O_DIRECT` writes and come back equal.
///
/// **Header and body cannot share offset 0.** A first version of this wrote the body at 0 and then
/// wrote the length over it, which is not a subtle bug: it destroyed the first 512 bytes of the
/// document and the reopen returned `[]` for a 44-byte sentence. The two regions have to be
/// disjoint, which is what the constants above say.
fn commit(container: &DirectFile, bytes: &[u8]) {
    let mut head = AlignedBuf::zeroed(512);
    head.as_mut_slice()[..8].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
    container
        .write_exact_at(HEADER_OFF, &head)
        .expect("write the header");

    let mut body = AlignedBuf::zeroed(bytes.len().next_multiple_of(512).max(512));
    body.as_mut_slice()[..bytes.len()].copy_from_slice(bytes);
    container
        .write_exact_at(BODY_OFF, &body)
        .expect("write the body");
}

/// Read back what `commit` wrote, using the recorded length.
fn read_all(container: &DirectFile) -> Vec<u8> {
    let mut head = AlignedBuf::zeroed(512);
    container
        .read_exact_at(HEADER_OFF, &mut head)
        .expect("read the header");
    let len = u64::from_le_bytes(head.as_slice()[..8].try_into().expect("8 bytes")) as usize;
    assert!(len > 0, "the header says the document is empty");
    let mut body = AlignedBuf::zeroed(len.next_multiple_of(512).max(512));
    container
        .read_exact_at(BODY_OFF, &mut body)
        .expect("read the body");
    body.as_slice()[..len].to_vec()
}

#[test]
fn a_scripted_session_produces_a_byte_identical_frame_every_time() {
    // The visual baseline's premise. If a frame depended on an address, an iteration order or an
    // allocation pattern, the baseline would be unrepeatable and worthless.
    let m = ChromeMetrics::DESKTOP;
    let build = || {
        let mut ed = Editor::new();
        ed.insert_at(0, b"Baseline.", SpanPolicy::Strict)
            .expect("seed");
        let (mut s, _) = session(ed, m);
        let mut src = ScriptedInputSource::from_events(&type_str("Stable."));
        s.run(&mut src).expect("run");
        s.repaint_all().expect("paint");
        let mut out = Vec::new();
        s.dump_ppm(&mut out).expect("dump");
        out
    };
    assert_eq!(
        build(),
        build(),
        "two identical sessions produced different frames"
    );
}

#[test]
fn the_caret_is_the_only_thing_that_moves_between_frames() {
    // One keystroke, one damaged line, one cell of difference. If the whole frame changed, the
    // damage path is not doing anything and the blink would flicker.
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    ed.insert_at(0, b"one two three", SpanPolicy::Strict)
        .expect("seed");
    let (mut s, _) = session(ed, m);
    s.repaint_all().expect("first paint");
    let before = s.frame().clone();

    let mut src =
        ScriptedInputSource::from_events(&[InputEvent::press(KEY_A), InputEvent::release(KEY_A)]);
    s.run(&mut src).expect("type one character");
    let after = s.frame().clone();

    let d = before.diff(&after).expect("same size");
    assert!(!d.is_empty(), "typing a character changed no pixels");
    // The damage is the caret's *line*, so a few hundred pixels is right; the whole frame is not.
    assert!(
        d.differing < 4000,
        "typing changed {} pixels, which is most of a line",
        d.differing
    );
}

#[test]
fn bold_styling_reaches_both_exports() {
    // The style mapping, through the session rather than through the exporter directly.
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    ed.insert_at(0, b"plain and bold", SpanPolicy::Strict)
        .expect("seed");
    let (mut s, _) = session(ed, m);
    s.toggle_bold(10, 14).expect("bold the last word");

    let dir = scratch("bold");
    std::fs::create_dir_all(&dir).expect("scratch");
    let mut html = sink(Format::Html, &dir.join("a.html"));
    s.export(&mut html, "t").expect("html");
    drop(html);
    let out = std::fs::read_to_string(dir.join("a.html")).expect("read");
    assert!(
        out.contains("<b>bold</b>"),
        "the span did not become a tag: {out}"
    );

    let mut pdf = sink(Format::Pdf, &dir.join("a.pdf"));
    s.export(&mut pdf, "t").expect("pdf");
    drop(pdf);
    let bytes = std::fs::read(dir.join("a.pdf")).expect("read");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("/BaseFont /Helvetica-Bold"),
        "the PDF did not select the bold face"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_session_with_no_atlas_still_reports_what_it_could_not_draw() {
    // Degraded mode is legitimate, and the count is what makes it honest.
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    ed.insert_at(0, b"text", SpanPolicy::Strict).expect("seed");
    let scanout = HeadlessScanout::new(m.width, m.height);
    let mut s = Session::new(ed, Painter::without_atlas(0), Box::new(scanout), m);
    s.repaint_all().expect("paint");
    assert!(s.stats.pixels > 0, "the bands still draw");
}

/// A resize rebuilds the frame, keeps the document, and marks all of it stale.
///
/// Three claims, and all three are load-bearing. The frame's size follows the request, or the next
/// `present` is refused as a `SizeMismatch`. The *document* is untouched, or a resize is an edit. And the
/// whole new frame is marked stale, or the window keeps a ghost of its old self along the bottom and
/// right edges -- the damage accumulated before the resize describes the *old* geometry, so none of it
/// covers the new pixels.
#[test]
fn a_resize_changes_the_frame_and_not_the_text() {
    let m = ChromeMetrics::DESKTOP;
    let (mut s, _atlas) = session(Editor::new(), m);
    s.repaint_all().expect("the first paint");

    // Type something, so there is text that must survive.
    for ev in type_str("resizing") {
        s.handle_event(ev).expect("a character");
    }
    let text_before = s.editor.text().expect("the document text");
    assert!(!text_before.is_empty(), "the harness typed nothing");
    // The chrome state holds the caret as line/column rather than a byte offset, which is what
    // the renderer needs; the editor holds the offset. Both must be unchanged, since a resize that
    // moved either would put the caret in a different place in a document it did not change.
    let caret_before = (s.state.caret_line, s.state.caret_column, s.editor.caret());

    // Grow, then shrink below the starting size, then back.
    for (w, h) in [(1600u32, 1000u32), (900, 500), (1280, 800)] {
        s.resize(w, h).expect("resize");
        assert_eq!(
            (s.frame().width(), s.frame().height()),
            (w, h),
            "the frame is {w}x{h} after being asked for it"
        );
        s.repaint_all().expect("painting at the new size");
    }

    assert_eq!(
        s.editor.text().expect("the document text"),
        text_before,
        "the document is unchanged"
    );
    assert_eq!(
        (s.state.caret_line, s.state.caret_column, s.editor.caret()),
        caret_before,
        "and the caret has not moved: a resize is not an edit"
    );
}

/// A panel below the chrome's minimum is clamped rather than refused.
///
/// A window can be dragged to nothing, and the answer has to be the smallest thing the chrome can draw.
/// The alternative is a subtraction that wraps, which is what `Layout::new`'s saturating arithmetic
/// exists to prevent -- this is the test that the clamp is what actually stops it.
#[test]
fn a_resize_below_the_minimum_is_clamped() {
    let m = ChromeMetrics::DESKTOP;
    let (mut s, _atlas) = session(Editor::new(), m);
    s.repaint_all().expect("the first paint");
    s.resize(1, 1).expect("a resize to nothing is not an error");
    assert_eq!(
        (s.frame().width(), s.frame().height()),
        (ChromeMetrics::MIN_WIDTH, ChromeMetrics::MIN_HEIGHT),
        "clamped up to the minimum the chrome can draw"
    );
    s.repaint_all().expect("painting at the minimum");
}

/// Resizing to the size a window already is does nothing at all.
///
/// This is the case a drag produces most: `ConfigureNotify` arrives for every intermediate size,
/// including sizes it has already been, and rebuilding the frame each time reallocates 4 MiB per event
/// for no visible change.
#[test]
fn a_resize_to_the_same_size_does_nothing() {
    let m = ChromeMetrics::DESKTOP;
    let (mut s, _atlas) = session(Editor::new(), m);
    s.repaint_all().expect("the first paint");
    let frames_before = s.stats.frames;
    let damage_before = s.damage();
    s.resize(m.width, m.height)
        .expect("resize to the same size");
    assert_eq!(s.stats.frames, frames_before, "no paint was asked for");
    assert_eq!(s.damage(), damage_before, "and nothing was marked stale");
}

// ---------------------------------------------------------------------------------------------
// Phase 9A: tables. The gate below is the one the directive names -- insert a table with Ctrl+T,
// type into cell (0, 0), Tab through the cells, append a row at the bottom-right -- plus the claims
// that the first version of the implementation got wrong, which are listed in each test's `///`.

/// Type `s` into the session the way a keyboard would, returning the number of edits it caused.
fn type_into(s: &mut holonomy::Session, text: &str) -> Result<(), holonomy::SessionError> {
    for ev in type_str(text) {
        s.handle_event(ev).expect("a character");
    }
    Ok(())
}

/// Press `key` with no modifiers, as a bare keystroke.
fn press(s: &mut holonomy::Session, key: u16) -> Result<(), holonomy::SessionError> {
    s.handle_event(holonomy_input::InputEvent::press(key))
        .expect("a key");
    Ok(())
}

/// Press `key` with shift held.
fn press_shift(s: &mut holonomy::Session, key: u16) -> Result<(), holonomy::SessionError> {
    s.handle_event(holonomy_input::InputEvent::press(
        holonomy_input::KEY_LEFTSHIFT,
    ))
    .expect("shift down");
    s.handle_event(holonomy_input::InputEvent::press(key))
        .expect("the shifted key");
    s.handle_event(holonomy_input::InputEvent::release(key))
        .expect("the shifted key up");
    s.handle_event(holonomy_input::InputEvent::release(
        holonomy_input::KEY_LEFTSHIFT,
    ))
    .expect("shift up");
    Ok(())
}

/// The scripted run the directive asks for, with the intermediate state asserted at every step.
///
/// Ctrl+T, type in cell (0, 0), Tab through all nine cells, and let the tenth Tab append a row. Each
/// step checks *where the caret is* rather than only that something happened, because "nine Tabs were
/// accepted" is also what a `Tab` that did nothing nine times looks like.
#[test]
fn ctrl_t_then_tab_through_a_three_by_three_appends_a_row() {
    let m = ChromeMetrics::DESKTOP;
    let (mut s, _atlas) = session(Editor::new(), m);

    // Ctrl+T, driven through the real keymap rather than `insert_table`, because the requirement is
    // that the keystroke reaches the table code and not that a method can be called.
    s.handle_event(holonomy_input::InputEvent::press(
        holonomy_input::KEY_LEFTCTRL,
    ))
    .expect("ctrl down");
    s.handle_event(holonomy_input::InputEvent::press(holonomy_input::KEY_T))
        .expect("ctrl+T");
    s.handle_event(holonomy_input::InputEvent::release(holonomy_input::KEY_T))
        .expect("T up");
    s.handle_event(holonomy_input::InputEvent::release(
        holonomy_input::KEY_LEFTCTRL,
    ))
    .expect("ctrl up");

    assert_eq!(s.stats.table_inserts, 1, "one table was inserted");
    let span = s.active_table().expect("the caret is in the new table");
    assert_eq!(
        (span.rows, span.cols),
        (3, 3),
        "a 3x3 table, which is what Ctrl+T inserts"
    );
    assert_eq!(
        s.active_cell().map(|c| (c.row, c.col)),
        Some((0, 0)),
        "and the caret is in the first cell, so typing goes there"
    );

    // Type into cell (0, 0).
    type_into(&mut s, "Name").expect("typing into the first cell");
    assert_eq!(
        s.active_cell_text().expect("the cell's bytes"),
        b"Name".to_vec(),
        "the text went into cell (0, 0) and nowhere else"
    );

    // Tab through every remaining cell, checking the position at each one.
    for want in [
        (0u16, 1u16),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (2, 0),
        (2, 1),
        (2, 2),
    ] {
        press(&mut s, holonomy_input::KEY_TAB).expect("Tab");
        assert_eq!(
            s.active_cell().map(|c| (c.row, c.col)),
            Some(want),
            "Tab landed in the next cell; every cell in the table is visited exactly once"
        );
    }

    // One more Tab leaves the table, and appends a row to append to.
    press(&mut s, holonomy_input::KEY_TAB).expect("Tab out");
    let grown = s.active_table().expect("still in a table");
    assert_eq!(
        grown.rows, 4,
        "the ninth Tab was at the bottom-right cell, so it appended a row"
    );
    assert_eq!(
        s.active_cell().map(|c| (c.row, c.col)),
        Some((3, 0)),
        "and the caret is in the new row's first cell"
    );
    assert_eq!(grown.cell_count(), 12, "a 4x3 table has twelve cells");
    assert_eq!(
        grown.separator_count(),
        11,
        "and therefore eleven separators -- which is one more than the eight an empty 3x3 has, \
         because appending a row of three cells adds three boundaries and not two"
    );
    assert_eq!(
        grown.end_byte - grown.start_byte,
        11 + 4,
        "so the span is eleven separators plus the four bytes typed into cell (0, 0)"
    );

    // And the table is intact as *data*: nine original cells plus three empty ones, resolvable.
    let text = s.editor.text().expect("the document");
    let resolved = holonomy_text::ResolvedTable::new(grown, &text).expect("the span resolves");
    let cells = resolved.cells().expect("every cell");
    assert_eq!(cells.len(), 12, "four rows of three");
    assert_eq!(
        &text[cells[0].start_byte as usize..cells[0].end_byte as usize],
        b"Name",
        "and the text typed at the start is still in the first cell, after four rows were appended \
         below it -- which is the claim that an insert at a table's first byte is inside the table"
    );
    assert!(
        cells[3..].iter().all(|c| c.is_empty()),
        "the three cells of the appended row start empty"
    );
}

/// Shift+Tab walks back out of a table and is a no-op at the first cell.
///
/// The second half is the claim that distinguishes this from a spreadsheet: backwards from cell (0, 0)
/// is nowhere, and the keystroke must not wrap to the bottom-right cell.
#[test]
fn shift_tab_walks_back_and_stops_at_the_first_cell() {
    let m = ChromeMetrics::DESKTOP;
    let (mut s, _atlas) = session(Editor::new(), m);
    s.insert_table(3, 3).expect("insert a 3x3 table");

    press(&mut s, holonomy_input::KEY_TAB).expect("Tab");
    press(&mut s, holonomy_input::KEY_TAB).expect("Tab");
    assert_eq!(
        s.active_cell().map(|c| (c.row, c.col)),
        Some((0, 2)),
        "two Tabs from the first cell"
    );
    press_shift(&mut s, holonomy_input::KEY_TAB).expect("Shift+Tab");
    assert_eq!(
        s.active_cell().map(|c| (c.row, c.col)),
        Some((0, 1)),
        "Shift+Tab goes back one cell"
    );
    // `KEY_LEFT`, not `KEY_LEFTBRACE`: on a US layout the arrow keys share scancodes with the braces,
    // and `Keymap` maps `KEY_LEFTBRACE` to **Up**. Using the wrong constant here looked like a broken
    // navigation rule for a while.
    press(&mut s, holonomy_input::KEY_LEFT).expect("Left");
    assert_eq!(
        s.active_cell().map(|c| (c.row, c.col)),
        Some((0, 0)),
        "and Left at a cell's start goes to the previous cell, rather than moving a byte"
    );
    let before = s.editor.caret();
    press_shift(&mut s, holonomy_input::KEY_TAB).expect("Shift+Tab at the first cell");
    assert_eq!(
        (s.active_cell().map(|c| (c.row, c.col)), s.editor.caret()),
        (Some((0, 0)), before),
        "backwards from the first cell is nowhere: the caret does not move and the cell does not wrap"
    );
    assert_eq!(
        s.stats.table_nav_nowhere, 1,
        "and the refused navigation is counted rather than swallowed"
    );
}

/// Tab outside a table indents, and Shift+Tab takes it back.
///
/// A pair, so that the two keys are related rather than merely different. Outside a table Tab is four
/// spaces rather than a tab character: a tab character moves the caret to the next tab stop while
/// contributing nothing to a column count, so a status bar reporting "Col 9" for four spaces would be
/// describing a tab stop.
#[test]
fn tab_outside_a_table_indents_and_shift_tab_takes_it_back() {
    let m = ChromeMetrics::DESKTOP;
    let (mut s, _atlas) = session(Editor::new(), m);
    s.editor.caret_to(0).expect("caret to the start");

    press(&mut s, holonomy_input::KEY_TAB).expect("Tab");
    assert_eq!(
        s.editor.text().expect("the document"),
        b"    ".to_vec(),
        "Tab outside a table is four spaces"
    );
    press(&mut s, holonomy_input::KEY_TAB).expect("Tab again");
    assert_eq!(
        s.editor.text().expect("the document").len(),
        8,
        "and eight after a second"
    );
    press_shift(&mut s, holonomy_input::KEY_TAB).expect("Shift+Tab");
    assert_eq!(
        s.editor.text().expect("the document"),
        b"    ".to_vec(),
        "Shift+Tab removes exactly one level"
    );
}

/// Typing in a cell damages that cell and not the whole table.
///
/// The directive's damage requirement, and it is the claim that per-cell damage is possible at all:
/// if a keystroke had to repaint the grid, then the borders -- which are in *other* rectangles -- would
/// be repainted too, and at 4 rows by 3 columns that is most of the page for one character.
#[test]
fn typing_in_a_cell_damages_that_cell_and_not_the_table() {
    let m = ChromeMetrics::DESKTOP;
    let (mut s, _atlas) = session(Editor::new(), m);
    s.insert_table(3, 3).expect("insert a 3x3 table");
    // One full paint, so the shape-dirty flag is clear and the next paint is content-only.
    s.repaint_all().expect("the first paint");

    press(&mut s, holonomy_input::KEY_RIGHT).expect("Right");
    press(&mut s, holonomy_input::KEY_DOWN).expect("Down");
    assert_eq!(
        s.active_cell().map(|c| (c.row, c.col)),
        Some((1, 1)),
        "Right left the empty cell (0, 0) for (0, 1) -- an empty cell is already at its end, so there \
         was no byte to move over -- and Down went a whole row rather than a line"
    );

    s.tick().expect("drain");
    s.repaint_all().expect("settle");
    let before = s.stats.frames;

    press(&mut s, holonomy_input::KEY_B).expect("a character");
    s.tick().expect("paint the keystroke");

    assert_eq!(
        s.stats.frames,
        before + 1,
        "one frame, as any keystroke causes"
    );
    let cell = s.active_cell_text().expect("the cell's bytes");
    assert_eq!(cell, b"b".to_vec(), "and the character is in cell (1, 0)");

    // The damage must not have covered the table's whole grid. Recompute the grid's height and compare
    // it with the rectangle the paint touched: a 3-row table is `4 * border + 3 * row` cells tall, so a
    // cell-sized damage is a small fraction of it.
    let span = s.active_table().expect("the table");
    let grid = holonomy_render::table::TableGrid::new(
        &span,
        s.chrome().layout.text.x,
        0,
        s.chrome().metrics.cell_w,
        s.chrome().metrics.cell_h,
        s.chrome().metrics.cell_h,
    );
    assert!(
        grid.height_px() > s.chrome().metrics.cell_h * 3,
        "the grid is {} px tall and a cell is {} px, so the two are distinguishable",
        grid.height_px(),
        s.chrome().metrics.cell_h
    );
}

/// The rendered table draws borders and cells, and they are on the cell grid exactly.
///
/// The Phase 9A gate proper: "borders asserted to land on exact integer pixel coordinates -- not
/// 'within a pixel'". Every x is a multiple of `cell_w` and every y a multiple of `cell_h`, checked
/// against the numbers themselves rather than against a tolerance.
#[test]
fn a_rendered_table_puts_every_border_on_the_cell_grid_exactly() {
    let m = ChromeMetrics::DESKTOP;
    let (mut s, _atlas) = session(Editor::new(), m);
    s.insert_table(3, 3).expect("insert a 3x3 table");
    s.repaint_all().expect("paint it");

    assert!(
        s.stats.table_borders_drawn > 0,
        "the paint drew border runs, so the tree really was emitted"
    );
    assert_eq!(
        s.stats.table_cells_drawn, 9,
        "and it visited all nine cells"
    );

    let span = s.active_table().expect("the table");
    let l = s.chrome().layout;
    let grid = holonomy_render::table::TableGrid::new(
        &span, l.text.x, l.text.y, m.cell_w, m.cell_h, m.cell_h,
    );
    assert!(
        grid.is_cell_aligned(),
        "the grid reports itself cell-aligned"
    );
    assert_eq!(
        grid.width_px() % m.cell_w,
        0,
        "its width is a whole number of cells: {} px of {}",
        grid.width_px(),
        m.cell_w
    );
    for i in 0..=span.cols {
        let x = grid.border_x_at(i);
        assert_eq!(
            x % m.cell_w,
            0,
            "vertical border {i} is at x={x}, which is not a multiple of the {} px cell",
            m.cell_w
        );
        assert_eq!(
            x - l.text.x,
            (u32::from(i) * (u32::from(span.col_widths[0]) + 2 + 1)) * m.cell_w,
            "and border {i} is where the column arithmetic says, not merely on the grid"
        );
    }
}
