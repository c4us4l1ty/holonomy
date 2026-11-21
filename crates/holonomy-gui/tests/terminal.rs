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
use holonomy_gui::{App, CellSurface, Keymap};
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
    // Backspace, then the missing letter: 0x7f is what a tty sends.
    type_bytes(&mut window, b"\x7fo");
    assert_eq!(window.document_len(), 5);
    window.draw().expect("drew");
    let drawn = screen(window.surface(), COLUMNS, "status");
    assert!(drawn.contains("hello"), "backspace did not edit:\n{drawn}");

    // Arrow keys are escape sequences; Ctrl+Z is one byte.
    type_bytes(&mut window, b"\x1b[D\x1b[D");
    assert_eq!(window.caret(), 3, "left did not move the caret");
    type_bytes(&mut window, b"\x1a");
    assert_eq!(window.document_len(), 0, "Ctrl+Z did not undo");
    window.draw().expect("drew");
    let after = screen(window.surface(), COLUMNS, "status");
    assert!(
        !after.contains("hello"),
        "the screen still shows the undone text:\n{after}"
    );
    println!(
        "HOLONOMY_TERMINAL_EDIT text={:?}",
        window.frame_text().expect("a frame")
    );
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
        let visible: String = line.chars().filter(|c| !c.is_control()).collect();
        assert!(
            visible.chars().count() <= 12,
            "a row is wider than the terminal: {visible:?}"
        );
    }
    assert_eq!(window.surface().columns(), 10);
    println!("HOLONOMY_TERMINAL_RESIZE columns=10 rows=2");
}
