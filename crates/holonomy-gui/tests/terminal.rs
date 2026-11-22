//! The terminal front end, end to end and without a terminal — `docs/TO-APP.md` M4b-i.
//!
//! The bytes a tty would send go through the real [`Decoder`], into the real [`App`], onto the real
//! [`CellSurface`], and out through the real ANSI renderer. What the assertions check is what a
//! person would see: the letters in the cells, the caret after them, the status line underneath.
//!
//! Only the tty itself is absent — raw mode, `ioctl`, the read loop — because those are the parts
//! that have no meaning in a test and are the only code left untested in the crate.

use holonomy_eye::Fixed;
use holonomy_gui::term::{screen, CellShaper, Decoder, Input};
use holonomy_gui::{App, CellSurface, Key, Keymap, Modifiers};
use holonomy_shell::Editor;
use holonomy_text::{Appended, ResidentSet, SegmentId, SegmentSink, SegmentSource, Typing};

#[derive(Debug, Clone, Default)]
struct Memory(std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>);

impl SegmentSink for Memory {
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
        let mut segments = self.0.lock().map_err(|_| "poisoned")?;
        let id = segments.len() as u64;
        segments.push(plaintext.to_vec());
        Ok(Appended {
            segment: SegmentId::new(id),
            witness_sequence: id + 1,
            segment_root: [0u8; 32],
            bytes_written: plaintext.len() as u64,
        })
    }
}

impl SegmentSource for Memory {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        let segments = self.0.lock().map_err(|_| "poisoned")?;
        segments
            .get(usize::try_from(segment.get()).unwrap_or(usize::MAX))
            .cloned()
            .ok_or_else(|| format!("no {segment}"))
    }
}

const COLUMNS: usize = 24;
const ROWS: usize = 4;

type Window = App<Memory, Memory, CellShaper, CellSurface>;

fn window() -> Window {
    let store = Memory::default();
    let residency = ResidentSet::new(store.clone(), 64 * 1024, 0).expect("a budget");
    let editor = Editor::new(
        Typing::new(store, 4_096),
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

/// Feeds a tty's bytes through the decoder and the app, exactly as the binary would.
fn type_bytes(window: &mut Window, bytes: &[u8]) -> u64 {
    let mut decoder = Decoder::new();
    let mut keys = 0;
    let mut press = |input: Option<Input>, keys: &mut u64| {
        if let Some(Input::Key(key, modifiers)) = input {
            window.press(key, modifiers).expect("pressed");
            *keys += 1;
        }
    };
    for byte in bytes {
        press(decoder.feed(*byte), &mut keys);
    }
    // A read that ends mid-sequence is a lone Escape, exactly as the binary flushes it.
    press(decoder.flush_escape(), &mut keys);
    keys
}

#[test]
fn a_session_types_and_the_screen_shows_it() {
    let mut window = window();
    let keys = type_bytes(&mut window, b"hello");
    assert_eq!(window.document_len(), 5, "the document lost bytes");
    window.draw().expect("drew");

    let status = format!(" holonomy · {} bytes ", window.document_len());
    let drawn = screen(window.surface(), COLUMNS, &status);
    assert!(
        drawn.contains("hello"),
        "the screen does not show the text:\n{drawn}"
    );
    assert!(
        drawn.contains("5 bytes"),
        "the status line does not count the bytes"
    );
    // The caret is placed on row 1, column 6: after the five clusters.
    assert!(
        drawn.contains("\x1b[1;6H"),
        "the caret is not after the last cluster:\n{drawn}"
    );
    println!(
        "HOLONOMY_TERMINAL keys={keys} bytes={} clusters={}",
        window.document_len(),
        window.surface().clusters()
    );
    assert!(keys >= 5);
}

#[test]
fn editing_keys_reach_the_document_and_the_screen() {
    let mut window = window();
    type_bytes(&mut window, b"helo");
    // Backspace, then the two letters it was missing: 0x7f is what a tty sends.
    type_bytes(&mut window, b"\x7flo");
    assert_eq!(window.document_len(), 5);
    window.draw().expect("drew");
    let drawn = screen(window.surface(), COLUMNS, "status");
    assert!(drawn.contains("hello"), "backspace did not edit:\n{drawn}");

    // Arrow keys are escape sequences; Ctrl+Z is one byte.
    type_bytes(&mut window, b"\x1b[D\x1b[D");
    assert_eq!(window.caret(), 3, "left did not move the caret");
    // The arrows sealed the open leaf, and before D-43 that seal cleared the undo stack: this
    // press did nothing, sixteen times. It takes back the burst that was being typed now.
    let effects = window
        .press(Key::Character('z'), Modifiers::control())
        .expect("undid");
    assert!(!effects.is_empty(), "Ctrl+Z said nothing changed");
    assert_eq!(window.document_len(), 3, "the burst was not taken back");
    window.draw().expect("drew");
    let after = screen(window.surface(), COLUMNS, "status");
    assert!(
        after.contains("hel") && !after.contains("hello"),
        "the screen still shows the undone text:\n{after}"
    );
    println!(
        "HOLONOMY_TERMINAL_EDIT text={:?}",
        window.frame_text().expect("a frame")
    );
}

#[test]
fn one_burst_is_one_undo() {
    let mut window = window();
    type_bytes(&mut window, b"abc");
    let before = window.document_len();
    let effects = window
        .press(Key::Character('z'), Modifiers::control())
        .expect("undid");
    assert_eq!(
        window.document_len(),
        0,
        "three bytes, one burst, one undo: before={before} effects={}",
        effects.len()
    );
    println!("HOLONOMY_TERMINAL_UNDO before={before} after=0");
}

#[test]
fn a_resized_terminal_redraws_within_its_new_columns() {
    let mut window = window();
    type_bytes(&mut window, b"a fairly long line of text");
    window.surface_mut().resize(10, 2);
    window
        .resize(Fixed::units(10), Fixed::units(2))
        .expect("resized");
    window.draw().expect("drew");
    let drawn = screen(window.surface(), 10, "status");
    for line in drawn.split("\r\n") {
        // The text of a row is what precedes its escape sequence; the sequences themselves are not
        // columns.
        let visible = line.split('\x1b').next().unwrap_or_default();
        assert!(
            visible.chars().count() <= 10,
            "a row is wider than the terminal: {visible:?}"
        );
    }
    assert_eq!(window.surface().columns(), 10);
    println!("HOLONOMY_TERMINAL_RESIZE columns=10 rows=2");
}

#[test]
fn a_page_is_the_screen_the_window_is_showing_now() {
    // D-60: `Keymap::page` was set once, when the window was built, and never followed the
    // viewport. The terminal makes it visible: `Ctrl+G`'s panel takes rows away from the text
    // area, so Page Down stepped a screen that no longer existed — four lines when two were
    // visible, so two lines were never shown. A resized window has the same defect with no panel.
    let mut window = window();
    // Four screens of text: at `CellShaper`'s one unit per cluster, 100 bytes on a 24-column line
    // is four wrapped lines of one unit each, and `CellShaper` is one unit per line too. Two rows
    // are then half the document, and paging by two is a move that can be seen. (Forty bytes was
    // the first version of this: two wrapped lines, exactly one screen, so the clamp left nothing
    // to page to and the assertion passed or failed on arithmetic unrelated to D-60.)
    type_bytes(&mut window, &[b'a'; 100]);
    window
        .resize(Fixed::units(COLUMNS as i64), Fixed::units(2))
        .expect("resized to two rows");
    assert_eq!(window.editor().viewport(), Fixed::units(2));
    // The window is measured before it can be scrolled, which is what the binary does when it
    // opens a document and what this test has to do to be a test of paging rather than of a
    // document nobody has laid out: with no measured height the total is zero and every scroll
    // clamps to it, so Page Down would be a no-op for a reason that has nothing to do with D-60.
    window.editor().reflow().expect("measured the document");
    // Typing reveals the caret (D-53), and this document is taller than the four rows the window
    // was born with, so the scroll is not at the top when the typing stops. Put it there: the
    // question this test asks is how far *one page* moves, not where the caret left the screen.
    window.editor().scroll_to(Fixed::ZERO);
    assert_eq!(
        window.editor().scroll(),
        Fixed::ZERO,
        "the test starts at the top of the document"
    );

    window
        .press(Key::PageDown, Modifiers::none())
        .expect("paged down");
    assert_eq!(
        window.editor().scroll(),
        Fixed::units(2),
        "a page is the screen the window shows, not the one it had when it was built"
    );
    // A page back up is one screen too. Five units of document on a two-unit viewport can scroll
    // to three, so from two a page up must land on zero — a window still paging by the four rows it
    // was built with would clamp to zero as well, but a page size that had *grown* or a page that
    // moved by something other than the viewport would not come back here.
    window
        .press(Key::PageUp, Modifiers::none())
        .expect("paged up");
    assert_eq!(
        window.editor().scroll(),
        Fixed::ZERO,
        "a page up is one screen, not two"
    );
    println!(
        "HOLONOMY_TERMINAL_PAGE scroll={} viewport={}",
        window.editor().scroll().sixtyfourths(),
        window.editor().viewport().sixtyfourths()
    );
}

#[test]
fn wide_characters_and_combining_marks_agree_with_terminal_columns() {
    // OQ-25: `中` occupies two terminal cells, `e\u{0301}` occupies one while retaining its mark,
    // and `a` occupies one — total four cells, placing the ANSI cursor at 1-based column 5.
    let mut window = window();
    type_bytes(&mut window, "中e\u{0301}a".as_bytes());
    window.draw().expect("drew");
    let drawn = screen(window.surface(), COLUMNS, " 笔记.wavefunction ");
    assert!(
        drawn.contains("中e\u{0301}a\x1b[K\r\n"),
        "wide glyph or combining mark was mangled or padded with a stray cell:\n{drawn:?}"
    );
    assert!(
        drawn.contains("\x1b[1;5H"),
        "caret must sit at column 5 after 2 + 1 + 1 cells:\n{drawn:?}"
    );

    // Truncating a wide status line to 5 columns keeps ` 笔记` (1 + 2 + 2 = 5 cells) rather than
    // counting characters and overflowing the row.
    let narrow = screen(window.surface(), 5, " 笔记.wavefunction");
    assert!(
        narrow.contains("\x1b[7m 笔记\x1b[0m"),
        "wide status line was not truncated by display columns:\n{narrow:?}"
    );
}

#[test]
fn tabs_controls_and_escapes_claim_one_safe_cell_without_terminal_injection() {
    let mut window = window();
    // Tab via tty + zero-width space via direct insert + hostile escape in document and filename.
    type_bytes(&mut window, b"a\tb");
    let injected = "\u{200B}\x1b]2;pwn\x07z";
    window.editor().type_text(injected).expect("typed");
    window.draw().expect("drew");

    let drawn = screen(window.surface(), COLUMNS, " evil\x1b]2;bad\x07\nname ");
    assert!(
        !drawn.contains("\x1b]2;"),
        "control cluster injected an OSC sequence into terminal output:\n{drawn:?}"
    );
    assert!(
        !drawn.contains('\t'),
        "raw tab reached terminal output and desynchronized columns:\n{drawn:?}"
    );
    assert!(
        drawn.contains("a b  ]2;pwn z\x1b[K\r\n"),
        "control and zero-width clusters must each occupy one safe space cell:\n{drawn:?}"
    );
    // 13 clusters (`a`, `\t`, `b`, `\u{200B}`, `\x1b`, `]`, `2`, `;`, `p`, `w`, `n`, `\x07`, `z`)
    // -> 1-based column 14.
    assert!(
        drawn.contains("\x1b[1;14H"),
        "caret column disagreed with safe cell advances:\n{drawn:?}"
    );
}

#[test]
fn off_screen_caret_never_jumps_onto_status_and_navigation_follows_caret() {
    let mut window = window();
    type_bytes(&mut window, b"0\n1\n2\n3\n4\n5");
    window.editor().reflow().expect("measured");

    // Scroll to the top of a 6-line single leaf on a 4-row surface: the caret on line 5 is below
    // the visible rows and must not place the cursor onto row 6 (the status/panel area).
    window.editor().scroll_to(Fixed::ZERO);
    // Force a redraw after the direct scroll call.
    window
        .resize(Fixed::units(COLUMNS as i64), Fixed::units(ROWS as i64))
        .expect("resized");
    window.press(Key::PageUp, Modifiers::none()).expect("paged");
    window.draw().expect("drew");
    let scrolled_up = screen(window.surface(), COLUMNS, "status");
    assert!(
        !scrolled_up.contains("\x1b[6;"),
        "off-screen caret jumped past the surface onto the status row:\n{scrolled_up:?}"
    );

    // Ctrl+Home moves the caret to line 0, and Ctrl+End moves it back to line 5 while scrolling
    // the viewport so the caret is visible on row 4 (`\x1b[4;2H`).
    window
        .press(Key::Home, Modifiers::control())
        .expect("ctrl+home");
    assert_eq!(window.editor().scroll(), Fixed::ZERO);
    window
        .press(Key::End, Modifiers::control())
        .expect("ctrl+end");
    assert!(
        window.editor().scroll() > Fixed::ZERO,
        "Ctrl+End did not scroll the viewport to reveal the caret"
    );
    window.draw().expect("drew");
    let at_end = screen(window.surface(), COLUMNS, "status");
    assert!(
        at_end.contains("\x1b[4;2H"),
        "caret at document end was not placed on the bottom visible surface row:\n{at_end:?}"
    );
}
