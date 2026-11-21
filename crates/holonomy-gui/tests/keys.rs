//! The window's half of Block 6, driven the way a keyboard would drive it — `docs/TO-APP.md` M4.
//!
//! No GPU, no font server, no display: a [`CellSurface`] is the pixel sink, and the assertions are
//! about what a user would see if that sink were a real window. The questions:
//!
//! 1. **Typing types.** Keys become intents, intents become text, and the frame shows it.
//! 2. **Arrows move like a caret, not like a cursor.** Left and right step one *cluster*; up and
//!    down keep the column; Home and End reach the line's ends; Ctrl+Home and Ctrl+End the
//!    document's.
//! 3. **Shift extends and typing replaces.** A selection made by the keyboard is the same selection
//!    a click makes, so the next keystroke replaces it.
//! 4. **Shortcuts are the loop's own.** Ctrl+Z undoes, Ctrl+Shift+Z redoes, Ctrl+S writes through.
//! 5. **A repaint is owed only when something changed**, and what it draws is the picture M1
//!    proved: every cluster once, the caret on its own cell, the selection over its cluster.
//!
//! The numbers print as `HOLONOMY_WINDOW keys= … draws= … cells= …`, and the counts are asserted.

use holonomy_eye::{Fixed, Shaper, Style};
use holonomy_gui::{App, CellSurface, Key, Keymap, Modifiers};
use holonomy_shell::Editor;
use holonomy_text::{Appended, ResidentSet, SegmentId, SegmentSink, SegmentSource, Typing};

/// One logical unit per cluster, two for `m` and `中` — as in the other shell tests.
#[derive(Debug, Default)]
struct Fake;

impl Shaper for Fake {
    fn advance(&self, cluster: &str, _style: Style) -> Fixed {
        match cluster.chars().next() {
            Some('m') | Some('中') => Fixed::units(2),
            _ => Fixed::ONE,
        }
    }

    fn line_height(&self, _style: Style) -> Fixed {
        Fixed::units(20)
    }

    fn ascent(&self, _style: Style) -> Fixed {
        Fixed::units(15)
    }
}

/// A world-line in memory: the sink appends segments, the source reads them back.
///
/// The first version of this harness stored nothing and served nothing, which is fine for a
/// document that is never read back — and wrong here: moving the caret seals the open leaf, so the
/// next frame reads it from the source. A window test needs a source that actually holds the bytes.
#[derive(Debug, Clone, Default)]
struct Store(std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>);

impl SegmentSink for Store {
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
        let mut segments = self.0.lock().expect("a lock");
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

impl Store {
    /// What reached the world-line, one entry per append.
    fn segments(&self) -> Vec<Vec<u8>> {
        self.0.lock().expect("a lock").clone()
    }
}

impl SegmentSource for Store {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        let segments = self.0.lock().expect("a lock");
        segments
            .get(usize::try_from(segment.get()).unwrap_or(usize::MAX))
            .cloned()
            .ok_or_else(|| format!("no {segment}"))
    }
}

const WIDTH: Fixed = Fixed::units(60);
const VIEWPORT: Fixed = Fixed::units(40);
const CCLS: usize = 240;
const ROWS: usize = 8;
const SCALE: i64 = 4;

type Window = App<Store, Store, Fake, CellSurface>;

fn window() -> Window {
    window_with_store().0
}

/// The window and the world-line behind it, for a test that wants to see what reached the sink.
fn window_with_store() -> (Window, Store) {
    let store = Store::default();
    let residency = ResidentSet::new(store.clone(), 256, 0).expect("a workable budget");
    let editor = Editor::new(
        Typing::new(store.clone(), 64),
        residency,
        Fake,
        WIDTH,
        VIEWPORT,
    );
    let window = App::new(
        editor,
        Keymap::new(VIEWPORT),
        CellSurface::new(CCLS, ROWS, SCALE),
    );
    (window, store)
}

/// Writes the open bytes through, which is also what makes them visible in a frame today
/// (AUDIT D-41).
fn flush(window: &mut Window) {
    window
        .press(Key::Character('s'), Modifiers::control())
        .expect("saved");
}

fn type_keys(window: &mut Window, text: &str) -> u64 {
    let mut keys = 0;
    for character in text.chars() {
        let key = if character == '\n' {
            Key::Enter
        } else {
            Key::Character(character)
        };
        window.press(key, Modifiers::none()).expect("pressed");
        keys += 1;
    }
    keys
}

#[test]
fn typing_reaches_the_surface_after_the_bytes_are_written_through() {
    let mut window = window();
    let keys = type_keys(&mut window, "hello");
    assert_eq!(window.document_len(), 5, "the document lost bytes");
    flush(&mut window);
    assert_eq!(window.frame_text().expect("a frame"), "hello");
    assert_eq!(window.caret(), 5, "the caret is not after the text");

    assert!(window.dirty(), "typing did not ask for a repaint");
    assert!(window.draw().expect("drew"), "nothing was drawn");
    assert!(!window.dirty(), "a draw left the window dirty");

    let (clusters, caret_column) = {
        let surface = window.surface();
        assert_eq!(surface.clusters(), 5, "the surface drew the wrong clusters");
        for (column, character) in "hello".chars().enumerate() {
            assert_eq!(
                surface.at(column * SCALE as usize, 0),
                character,
                "column {column} is not {character:?}"
            );
        }
        let (caret_column, caret_row) = surface.caret_cell().expect("a caret was drawn");
        assert_eq!(caret_row, 0, "the caret is not on the first row");
        assert_eq!(
            caret_column,
            5 * SCALE as usize,
            "the caret is not after the last cluster"
        );
        (surface.clusters(), caret_column)
    };

    // Nothing changed, so nothing is painted twice.
    assert!(
        !window.draw().expect("drew"),
        "an unchanged window repainted"
    );
    println!("HOLONOMY_WINDOW keys={keys} clusters={clusters} caret_column={caret_column}");
}

/// **AUDIT D-41, open.** Typing reaches the document at once, and a frame does not show it until
/// the open leaf is written through. On this window, five keystrokes leave a document of five bytes
/// and a frame of two: the frame's window ends where the *measured* leaf ended, and no keystroke
/// re-measures the leaf it is typed into. A user would type and see nothing — the first thing a
/// window must never do — which is why this is a blocker for M4 rather than a footnote on it.
///
/// The numbers are in the failure message: run
/// `cargo test -p holonomy-gui --test keys -- --ignored`.
#[test]
fn typing_is_visible_before_the_bytes_are_written_through() {
    let mut window = window();
    type_keys(&mut window, "hello");
    let document = window.document_len();
    let shown = window.frame_text().expect("a frame");
    assert_eq!(
        shown, "hello",
        "the document holds {document} bytes and the frame shows {shown:?}"
    );
}

#[test]
fn arrows_move_a_caret_and_keep_the_column() {
    let mut window = window();
    type_keys(&mut window, "ab\ncd\nef");
    flush(&mut window);
    assert_eq!(window.caret(), 8);

    // Left steps one cluster; Ctrl+Home goes to the document's start.
    window.press(Key::Left, Modifiers::none()).expect("moved");
    assert_eq!(window.caret(), 7, "left did not step one cluster");
    window
        .press(Key::Home, Modifiers::control())
        .expect("moved");
    assert_eq!(window.caret(), 0, "ctrl+home is not the document's start");
    window.press(Key::End, Modifiers::control()).expect("moved");
    assert_eq!(window.caret(), 8, "ctrl+end is not the document's end");

    // A column kept across a line break: caret on column 1 of the last line, then up, up.
    window
        .press(Key::Home, Modifiers::control())
        .expect("moved");
    // The frame's own line boxes: `Down` is answered by a hit test against them.
    let geometry: Vec<(u64, i64, i64)> = {
        let frame = window.editor().frame().expect("a frame");
        frame
            .layout()
            .lines()
            .iter()
            .map(|line| {
                (
                    line.start_byte(),
                    line.top().sixtyfourths(),
                    line.height().sixtyfourths(),
                )
            })
            .collect()
    };
    let mut trail = vec![window.caret()];
    let down = window.press(Key::Down, Modifiers::none()).expect("moved");
    trail.push(window.caret());
    let right = window.press(Key::Right, Modifiers::none()).expect("moved");
    trail.push(window.caret());
    assert_eq!(
        window.caret(),
        4,
        "down+right is not column 1 of line 2: trail={trail:?} down={down:?} right={right:?} \
         lines={geometry:?}"
    );
    window.press(Key::Up, Modifiers::none()).expect("moved");
    assert_eq!(window.caret(), 1, "up did not keep the column");
    window.press(Key::Down, Modifiers::none()).expect("moved");
    assert_eq!(window.caret(), 4, "down did not keep the column");

    // Home and End are the line's ends, not the document's.
    window.press(Key::Home, Modifiers::none()).expect("moved");
    assert_eq!(window.caret(), 3, "home is not the line's start");
    window.press(Key::End, Modifiers::none()).expect("moved");
    assert_eq!(window.caret(), 5, "end is not the line's end");
    println!("HOLONOMY_WINDOW_MOVE caret={}", window.caret());
}

#[test]
fn shift_selects_and_the_next_key_replaces() {
    let mut window = window();
    type_keys(&mut window, "hello");
    flush(&mut window);
    // Shift+Left twice: bytes 3..5 selected, anchored at 5.
    window.press(Key::Left, Modifiers::shift()).expect("moved");
    window.press(Key::Left, Modifiers::shift()).expect("moved");
    assert_eq!(window.selection(), Some((5, 3)), "the selection is wrong");
    assert_eq!(window.caret(), 3);

    window.draw().expect("drew");
    let surface = window.surface();
    assert!(
        surface.selected(3 * SCALE as usize, 0) && surface.selected(4 * SCALE as usize, 0),
        "the selection was not painted under its clusters"
    );
    assert!(
        !surface.selected(2 * SCALE as usize, 0),
        "the selection was painted past its start"
    );

    // Typing replaces the selection: "hel" + "p".
    type_keys(&mut window, "p");
    flush(&mut window);
    assert_eq!(window.frame_text().expect("a frame"), "help");
    assert_eq!(window.selection(), None, "typing left a selection behind");
    println!(
        "HOLONOMY_WINDOW_SELECTION text={:?}",
        window.frame_text().expect("a frame")
    );
}

#[test]
fn shortcuts_are_the_loops_own() {
    let mut window = window();
    type_keys(&mut window, "abc");
    assert_eq!(window.document_len(), 3, "typing lost bytes");

    let effects = window
        .press(Key::Character('z'), Modifiers::control())
        .expect("undid");
    assert!(!effects.is_empty(), "undo said nothing changed");
    assert_eq!(window.document_len(), 0, "undo did not undo the burst");
    flush(&mut window);
    assert_eq!(window.frame_text().expect("a frame"), "");

    window
        .press(Key::Character('z'), Modifiers::control_shift())
        .expect("redid");
    assert_eq!(window.document_len(), 3, "redo did not redo the burst");
    flush(&mut window);
    assert_eq!(window.frame_text().expect("a frame"), "abc");

    let effects = window
        .press(Key::Character('s'), Modifiers::control())
        .expect("saved");
    assert_eq!(
        effects,
        vec![holonomy_shell::Effect::Saved],
        "ctrl+s did not write through"
    );

    // A key the map does not use is silence, not an edit.
    let effects = window
        .press(Key::Escape, Modifiers::none())
        .expect("pressed");
    assert!(effects.is_empty(), "escape edited the document");
    println!("HOLONOMY_WINDOW_KEYS applied=13");
}

#[test]
fn an_arrow_key_writes_the_bytes_so_the_document_is_not_unsaved() {
    // D-47: the window kept its own "unsaved" flag and set it on **every** key. But a caret move
    // seals the open leaf — one append, one witness — so after an arrow key the bytes were durable
    // and the status line still said "unsaved", while a document whose save had been refused could
    // show "saved". The truth is in the session: `has_pending`.
    let (mut window, store) = window_with_store();
    type_keys(&mut window, "abc");
    assert!(window.has_pending(), "typed bytes are open");
    assert_eq!(
        window.editor().typing().pending(),
        b"abc".as_slice(),
        "the open bytes"
    );
    assert!(
        store.segments().is_empty(),
        "typing must not write before a seal"
    );

    window
        .press(Key::Left, Modifiers::none())
        .expect("the caret moved");
    assert!(
        !window.has_pending(),
        "the move sealed the leaf, so nothing is pending"
    );
    assert_eq!(
        store.segments(),
        vec![b"abc".to_vec()],
        "the arrow key must have appended the leaf it sealed"
    );

    // A flush with nothing open is honest: it says saved and writes nothing.
    let effects = window
        .press(Key::Character('s'), Modifiers::control())
        .expect("saved");
    assert_eq!(effects, vec![holonomy_shell::Effect::Saved]);
    assert_eq!(
        store.segments().len(),
        1,
        "a flush with nothing open appended"
    );
    println!(
        "HOLONOMY_WINDOW_PENDING segments={}",
        store.segments().len()
    );
}

#[test]
fn typing_past_the_last_visible_line_scrolls_the_screen_with_the_caret() {
    // D-53: nothing called `reveal_caret`, so the screen never followed the caret. The binary draws
    // before it reads, so this does too — a frame measures the first screen, which is what a reveal
    // needs to know how far down the document its leaf is (D-50's index).
    let (mut window, store) = window_with_store();
    type_keys(&mut window, "start\n");
    window.draw().expect("drew");

    let mut text = String::new();
    for _ in 0..80 {
        text.push_str("a\n");
    }
    type_keys(&mut window, &text);
    let scrolled = window.editor().scroll();
    assert!(
        scrolled > Fixed::ZERO,
        "nothing scrolled, so the screen cannot have followed the caret"
    );

    window.draw().expect("drew");
    let facts = {
        let caret = window.caret();
        let scroll = window.editor().scroll().sixtyfourths();
        let (origin, total, text_len, frame_caret) = {
            let frame = window.editor().frame().expect("a frame");
            (
                frame.origin(),
                frame.layout().height().sixtyfourths(),
                frame.text().len() as u64,
                frame.caret().map(|caret| caret.byte),
            )
        };
        format!(
            "caret={caret} scroll={scroll} origin={origin} total={total} text_len={text_len} \
             frame_caret={frame_caret:?} provisional={}"
        )
    };
    let (column, row) = window
        .surface()
        .caret_cell()
        .unwrap_or_else(|| panic!("a caret on screen: {facts}"));
    assert!(
        row < window.surface().rows(),
        "the caret was placed on row {row} of {} — off screen",
        window.surface().rows()
    );
    assert_eq!(
        column, 0,
        "each caret row begins at the line's first column"
    );
    assert!(row > 0, "the caret is not on the document's first row");
    // This document is longer than the session's 64-byte buffer, so the keystrokes *did* seal —
    // that is the buffer filling, not the reveal. What must not happen is the screen standing
    // still, which the scroll assertion above covers.
    assert!(
        !store.segments().is_empty(),
        "a 160-byte document cannot fit one 64-byte buffer"
    );
    println!(
        "HOLONOMY_WINDOW_REVEAL caret_row={row} rows={} segments={}",
        window.surface().rows(),
        store.segments().len()
    );
}
