//! A terminal window: keys in from a tty, cells out to a screen (`docs/TO-APP.md` M4b-i).
//!
//! GPUI cannot be compiled where this was written — the lockfile is committed and CI builds
//! `--locked`, so a registry dependency cannot be added (see the crate docs). A terminal needs no
//! dependency at all: `libc` is already in the lockfile for the seal's page locking, and everything
//! else here is bytes in and bytes out. What that buys is the thing the roadmap actually needs
//! next: **an app you can open and type in**, with the same `App`, the same `Keymap`, the same
//! intents and the same geometry that a GPU backend will get.
//!
//! Three pieces, each testable without a terminal:
//!
//! - [`CellShaper`] — a monospace face whose geometry is *exactly* one cell per cluster and one
//!   cell per line. The terminal is a cell grid, so the geometry and the pixels cannot disagree:
//!   the shaper is not an approximation of the screen, it is the screen's own metric.
//! - [`Decoder`] — bytes from a tty to [`Key`] events: printable UTF-8, return, backspace, delete,
//!   the four arrows (with the xterm modifier parameters), Home/End/PgUp/PgDn, and the control
//!   letters the keymap uses (Ctrl+A/S/Z, Ctrl+Shift+Z).
//! - [`screen`] — a [`CellSurface`] and a status line rendered as ANSI: home the cursor, print the
//!   rows, put the cursor back on the caret.
//!
//! The terminal wrapper itself (`src/bin/holonomy.rs`) holds the parts that need a real tty — raw
//! mode, the window size, the read/write loop — and is deliberately thin, because it is the only
//! code in this crate that cannot be tested in CI.

use crate::{CellSurface, Key, Modifiers};
use holonomy_eye::{Fixed, Shaper, Style};

/// A monospace face: one logical unit per cluster, one unit per line, ascent inside it.
///
/// The terminal draws one character per cell and one line per row, so this *is* the screen's
/// geometry. A wide character (which the terminal draws in two cells) is not modelled yet: the
/// cluster claims one cell, and the renderer's own width would disagree — recorded as OQ-25.
#[derive(Debug, Clone, Copy, Default)]
pub struct CellShaper;

impl Shaper for CellShaper {
    fn advance(&self, cluster: &str, _style: Style) -> Fixed {
        if matches!(cluster, "\n" | "\r" | "\r\n") {
            Fixed::ZERO
        } else {
            Fixed::ONE
        }
    }

    fn line_height(&self, _style: Style) -> Fixed {
        Fixed::ONE
    }

    fn ascent(&self, _style: Style) -> Fixed {
        Fixed::ONE
    }
}

/// What the tty sent, once it is a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// A key press with the modifiers that came with it.
    Key(Key, Modifiers),
    /// Bytes that are not a key this editor uses — a mouse report, a focus event. Ignored.
    Ignored,
}

/// Bytes to keys.
///
/// TTYs send escape sequences and UTF-8 as byte streams, so the decoder is a small state machine
/// that holds what it cannot yet decide. It never panics on arbitrary bytes: a sequence it does not
/// recognise is [`Input::Ignored`], which is what a terminal does with keys it does not use.
#[derive(Debug, Default)]
pub struct Decoder {
    /// Bytes of an incomplete UTF-8 character, and how many are still expected.
    utf8: Vec<u8>,
    expect: usize,
    /// Bytes of an incomplete escape sequence.
    escape: Option<Vec<u8>>,
}

impl Decoder {
    /// A decoder at the start of a stream.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one byte, and answers with a key once one is complete.
    pub fn feed(&mut self, byte: u8) -> Option<Input> {
        if let Some(sequence) = &mut self.escape {
            sequence.push(byte);
            if sequence.len() > 16 {
                self.escape = None;
                return Some(Input::Ignored);
            }
            // A CSI sequence ends at the first byte in '@'..='~'.
            if (0x40..=0x7e).contains(&byte) && sequence.len() > 1 {
                let sequence = self.escape.take().unwrap_or_default();
                return Some(interpret(&sequence));
            }
            return None;
        }
        if byte == 0x1b {
            self.escape = Some(vec![byte]);
            return None;
        }
        if self.expect > 0 {
            self.utf8.push(byte);
            if self.utf8.len() == self.expect {
                let bytes = std::mem::take(&mut self.utf8);
                self.expect = 0;
                let character = std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|text| text.chars().next());
                return match character {
                    Some(character) => {
                        Some(Input::Key(Key::Character(character), Modifiers::none()))
                    }
                    None => Some(Input::Ignored),
                };
            }
            return None;
        }
        match byte {
            0x00..=0x1f => Some(control(byte)),
            0x20..=0x7e => Some(Input::Key(
                Key::Character(char::from(byte)),
                Modifiers::none(),
            )),
            0x7f => Some(Input::Key(Key::Backspace, Modifiers::none())),
            _ => {
                // A UTF-8 lead byte: how many bytes follow.
                self.expect = match byte {
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf7 => 4,
                    _ => return Some(Input::Ignored),
                };
                self.utf8.push(byte);
                None
            }
        }
    }

    /// Bytes that were only an escape: the Escape key.
    ///
    /// A lone `ESC` is indistinguishable from the start of a sequence until the read blocks, so the
    /// terminal wrapper flushes with this when a read comes back with nothing else.
    pub fn flush_escape(&mut self) -> Option<Input> {
        self.escape.take().map(|sequence| {
            if sequence.len() == 1 {
                Input::Key(Key::Escape, Modifiers::none())
            } else {
                interpret(&sequence)
            }
        })
    }
}

/// A control byte: the letters a terminal maps `Ctrl+letter` onto, and the two line keys.
fn control(byte: u8) -> Input {
    match byte {
        0x01 => Input::Key(Key::Character('a'), Modifiers::control()),
        0x03 => Input::Key(Key::Character('c'), Modifiers::control()),
        0x09 => Input::Key(Key::Character('\t'), Modifiers::none()),
        0x0a | 0x0d => Input::Key(Key::Enter, Modifiers::none()),
        0x11 => Input::Key(Key::Character('q'), Modifiers::control()),
        0x13 => Input::Key(Key::Character('s'), Modifiers::control()),
        0x1a => Input::Key(Key::Character('z'), Modifiers::control()),
        0x1b => Input::Key(Key::Escape, Modifiers::none()),
        _ => Input::Ignored,
    }
}

/// An escape sequence, as a key.
fn interpret(sequence: &[u8]) -> Input {
    let body = sequence.get(1..).unwrap_or_default();
    // `ESC O x` is the application-mode arrow set; `ESC [ x` the normal one.
    let params = match body.first() {
        Some(b'[') | Some(b'O') => body.get(1..).unwrap_or_default(),
        _ => return Input::Ignored,
    };
    let Some(&final_byte) = params.last() else {
        return Input::Ignored;
    };
    let numbers: Vec<u8> = params[..params.len() - 1]
        .split(|byte| *byte == b';')
        .filter(|part| !part.is_empty())
        .filter_map(|part| std::str::from_utf8(part).ok()?.parse::<u8>().ok())
        .collect();
    // xterm's modifier parameter: 1 is none, then shift(2)/alt(3)/shift+alt(4)/ctrl(5)/...
    let modifiers = match numbers.get(1).copied() {
        None | Some(1) => Modifiers::none(),
        Some(code) => {
            let bits = code.saturating_sub(1);
            Modifiers {
                shift: bits & 1 != 0,
                alt: bits & 2 != 0,
                control: bits & 4 != 0,
            }
        }
    };
    let key = match (final_byte, numbers.first().copied()) {
        (b'A', _) => Key::Up,
        (b'B', _) => Key::Down,
        (b'C', _) => Key::Right,
        (b'D', _) => Key::Left,
        (b'H', _) => Key::Home,
        (b'F', _) => Key::End,
        (b'~', Some(1) | Some(7)) => Key::Home,
        (b'~', Some(4) | Some(8)) => Key::End,
        (b'~', Some(3)) => Key::Delete,
        (b'~', Some(5)) => Key::PageUp,
        (b'~', Some(6)) => Key::PageDown,
        _ => return Input::Ignored,
    };
    Input::Key(key, modifiers)
}

/// The screen a terminal shows: the editor's cells, then a status line, as ANSI.
///
/// The renderer is a pure function of what was drawn, so a test can assert exactly what a terminal
/// would print. The caret is placed by moving the cursor after the rows, never by writing a
/// character into the grid — the text is the document's, not the cursor's.
#[must_use]
pub fn screen(surface: &CellSurface, columns: usize, status: &str) -> String {
    let mut out = String::with_capacity(columns * 24 + 64);
    out.push_str("\x1b[H");
    for row in 0..surface.rows() {
        let mut line = String::with_capacity(columns);
        for column in 0..columns {
            line.push(surface.at(column, row));
        }
        out.push_str(line.trim_end());
        out.push_str("\x1b[K\r\n");
    }
    out.push_str("\x1b[7m");
    out.push_str(&status.chars().take(columns).collect::<String>());
    out.push_str("\x1b[0m\x1b[K");
    if let Some((column, row)) = surface.caret_cell() {
        out.push_str(&format!("\x1b[{};{}H", row + 1, column + 1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(bytes: &[u8]) -> Vec<Input> {
        let mut decoder = Decoder::new();
        let mut out = Vec::new();
        for byte in bytes {
            if let Some(input) = decoder.feed(*byte) {
                out.push(input);
            }
        }
        if let Some(input) = decoder.flush_escape() {
            out.push(input);
        }
        out
    }

    #[test]
    fn plain_and_control_bytes_are_keys() {
        assert_eq!(
            keys(b"a"),
            vec![Input::Key(Key::Character('a'), Modifiers::none())]
        );
        assert_eq!(keys(b"\r"), vec![Input::Key(Key::Enter, Modifiers::none())]);
        assert_eq!(
            keys(b"\x7f"),
            vec![Input::Key(Key::Backspace, Modifiers::none())]
        );
        assert_eq!(
            keys(b"\x1a"),
            vec![Input::Key(Key::Character('z'), Modifiers::control())]
        );
        assert_eq!(
            keys(b"\x13"),
            vec![Input::Key(Key::Character('s'), Modifiers::control())]
        );
    }

    #[test]
    fn escape_sequences_are_arrows_and_their_modifiers() {
        assert_eq!(
            keys(b"\x1b[A"),
            vec![Input::Key(Key::Up, Modifiers::none())]
        );
        assert_eq!(
            keys(b"\x1b[C"),
            vec![Input::Key(Key::Right, Modifiers::none())]
        );
        assert_eq!(
            keys(b"\x1b[1;2D"),
            vec![Input::Key(Key::Left, Modifiers::shift())]
        );
        assert_eq!(
            keys(b"\x1b[3~"),
            vec![Input::Key(Key::Delete, Modifiers::none())]
        );
        assert_eq!(
            keys(b"\x1b[5~"),
            vec![Input::Key(Key::PageUp, Modifiers::none())]
        );
        assert_eq!(
            keys(b"\x1b[H"),
            vec![Input::Key(Key::Home, Modifiers::none())]
        );
        // A lone escape is the Escape key once the stream says nothing follows it.
        assert_eq!(
            keys(b"\x1b"),
            vec![Input::Key(Key::Escape, Modifiers::none())]
        );
        // A sequence nobody uses is ignored, not misread.
        assert_eq!(keys(b"\x1b[99~"), vec![Input::Ignored]);
    }

    #[test]
    fn multi_byte_characters_arrive_whole() {
        assert_eq!(
            keys("é".as_bytes()),
            vec![Input::Key(Key::Character('é'), Modifiers::none())]
        );
        assert_eq!(
            keys("中".as_bytes()),
            vec![Input::Key(Key::Character('中'), Modifiers::none())]
        );
        assert_eq!(keys("中 a".as_bytes()).len(), 3);
    }
}
