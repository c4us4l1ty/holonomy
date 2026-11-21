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
            // The second byte is the introducer — `[` for CSI, `O` for SS3 — and `[` is itself in
            // the final-byte range, so it must not be read as one (D-42). After it, a CSI sequence
            // ends at the first byte in '@'..='~'.
            let complete = if sequence.len() == 2 {
                !matches!(byte, b'[' | b'O')
            } else {
                (0x40..=0x7e).contains(&byte)
            };
            if complete || sequence.len() > 16 {
                let sequence = self.escape.take().unwrap_or_default();
                return Some(interpret(&sequence));
            }
            return None;
        }
        if byte == 0x1b {
            // D-56: an escape can arrive in the middle of a multi-byte character — a tty whose
            // writer was interrupted, a paste that was cut, garbage on a serial line. The UTF-8
            // state was left standing, and the next bytes fed after the escape sequence completed
            // a character out of a lead byte that no longer had one: `0xe4` then `ESC [ A` then
            // `0xb8` typed a character nobody pressed, or swallowed a keystroke that should have
            // been text. An escape starts a new token, so the half-character is abandoned.
            self.utf8.clear();
            self.expect = 0;
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

    /// Whether a sequence has begun and is still incomplete.
    ///
    /// The window asks this before flushing a lone `ESC` as the Escape key: if more bytes are on
    /// their way, `ESC` is the first byte of an arrow, not a key press (D-46).
    #[must_use]
    pub fn pending(&self) -> bool {
        self.escape.is_some()
    }

    /// One read's bytes, as keys, without deciding anything about a trailing `ESC`.
    ///
    /// The subtlety this exists for: a terminal delivers a sequence in as many writes as the kernel
    /// and the network feel like, so `ESC` can arrive in one read and `[A` in the next. Flushing a
    /// lone `ESC` after every read — which the window did until D-46 — therefore *types the rest of
    /// the sequence into the document*: press Up over ssh and get `[A` at the caret. So the window
    /// feeds the read here, asks [`Self::pending`], and calls [`Self::flush_escape`] only when no
    /// more bytes are waiting.
    #[must_use]
    pub fn feed_all(&mut self, bytes: &[u8]) -> Vec<Input> {
        let mut inputs = Vec::new();
        for byte in bytes {
            if let Some(input) = self.feed(*byte) {
                inputs.push(input);
            }
        }
        inputs
    }

    /// Bytes that were only an escape: the Escape key.
    ///
    /// A lone `ESC` is indistinguishable from the start of a sequence until the read blocks, so the
    /// terminal wrapper flushes with this when a read comes back with nothing else.
    pub fn flush_escape(&mut self) -> Option<Input> {
        // D-56: a half-character must not outlive the read it was half of — but only when this
        // call is actually settling an escape. A caller that flushes with nothing pending must not
        // lose a character that is still arriving, which is the ordinary case of a tty splitting a
        // three-byte character across two reads.
        self.escape.as_ref()?;
        self.utf8.clear();
        self.expect = 0;
        let sequence = self.escape.take().unwrap_or_default();
        Some(if sequence.len() == 1 {
            Input::Key(Key::Escape, Modifiers::none())
        } else {
            interpret(&sequence)
        })
    }
}

/// A control byte: the letters a terminal maps `Ctrl+letter` onto, and the two line keys.
///
/// Two of these are not letters at all, and both were missing until D-44 and D-45:
///
/// * `0x08` is `Ctrl+H`, and a terminal whose `stty erase` is `^H` sends it for the backspace key.
///   Every other terminal sends `0x7f`, and the passphrase prompt already accepted both — the
///   editor did not, so on such a terminal backspace did nothing at all.
/// * `0x19` is `Ctrl+Y`, the redo key the window documents. Without it the only reachable redo was
///   `Ctrl+Shift+Z`, which a terminal cannot tell apart from `Ctrl+Z` (both arrive as `0x1a`).
fn control(byte: u8) -> Input {
    match byte {
        0x01 => Input::Key(Key::Character('a'), Modifiers::control()),
        0x03 => Input::Key(Key::Character('c'), Modifiers::control()),
        0x08 => Input::Key(Key::Backspace, Modifiers::none()),
        0x09 => Input::Key(Key::Character('\t'), Modifiers::none()),
        0x0a | 0x0d => Input::Key(Key::Enter, Modifiers::none()),
        0x11 => Input::Key(Key::Character('q'), Modifiers::control()),
        0x13 => Input::Key(Key::Character('s'), Modifiers::control()),
        0x19 => Input::Key(Key::Character('y'), Modifiers::control()),
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
    screen_with(surface, columns, &[], status)
}

/// How many panel lines fit above a surface and a status line in a terminal `rows` high.
///
/// The window used to compute `rows - 1 - panel.len()` and clamp the surface to one row, which is
/// fine until the panel is as tall as the terminal: then the frame is `1 + panel + 1` lines and the
/// terminal **scrolls** on every repaint, so the caret the renderer has just placed is one row off
/// and the screen shivers once a second (D-48). The panel is the thing that gives way — truncating
/// it from the bottom, where the least pressing line is — and the arithmetic is kept here, next to
/// the renderer that must not scroll.
#[must_use]
pub fn panel_fits(panel: &[String], rows: usize) -> Vec<String> {
    // One row for the surface at least, one for the status line: everything else is panel.
    let room = rows.saturating_sub(2);
    panel.iter().take(room).cloned().collect()
}

/// The screen with a panel above the status line — what `Ctrl+G` draws.
///
/// Same renderer, one extra block: the panel's lines are the security panel's own text (see
/// [`crate::panel`]), truncated to the terminal's width, so a number that is too long to show is
/// visibly cut rather than silently wrong.
#[must_use]
pub fn screen_with(
    surface: &CellSurface,
    columns: usize,
    panel: &[String],
    status: &str,
) -> String {
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
    for line in panel {
        out.push_str(&line.chars().take(columns).collect::<String>());
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
    fn backspace_arrives_as_either_byte_a_terminal_sends() {
        // D-44: `^H` is what a terminal with `stty erase ^H` sends; the prompt always accepted it,
        // the editor did not, so backspace deleted nothing on such a terminal.
        let erased = vec![Input::Key(Key::Backspace, Modifiers::none())];
        assert_eq!(keys(b"\x08"), erased);
        assert_eq!(keys(b"\x7f"), erased);
    }

    #[test]
    fn ctrl_y_is_redo_and_ctrl_shift_z_cannot_be_told_apart() {
        // D-45: the window documents Ctrl+Y as redo. It was not decoded at all, and Ctrl+Shift+Z
        // arrives as 0x1a — the same byte as Ctrl+Z — so Ctrl+Y is the only redo a tty can offer.
        assert_eq!(
            keys(b"\x19"),
            vec![Input::Key(Key::Character('y'), Modifiers::control())]
        );
        assert_eq!(
            keys(b"\x1a"),
            vec![Input::Key(Key::Character('z'), Modifiers::control())]
        );
    }

    #[test]
    fn a_sequence_split_across_reads_is_a_sequence_not_text() {
        // D-46: the window used to flush a lone ESC after every read, so an arrow key delivered in
        // two writes became Escape, then `[`, then `A` — three keys, two of them typed into the
        // document. This is the window's own rule, exercised: feed, and flush only if the stream is
        // quiet (the caller has already waited a grace period when it says so).
        let (mut decoder, first) = decoder_receiving(b"\x1b", false);
        assert!(first.is_empty(), "the rest of the sequence is on its way");
        assert!(decoder.pending(), "the sequence must still be open");
        assert_eq!(
            decoder.feed_all(b"[A"),
            vec![Input::Key(Key::Up, Modifiers::none())],
            "the second read completed the arrow"
        );
        assert!(!decoder.pending());

        // A quiet stream still turns a lone ESC into the Escape key.
        let (decoder, inputs) = decoder_receiving(b"\x1b", true);
        assert_eq!(inputs, vec![Input::Key(Key::Escape, Modifiers::none())]);
        assert!(!decoder.pending());
    }

    /// One read, with the window's rule: a started sequence is flushed only when the stream has
    /// gone quiet; `quiet` is the caller's answer to "did no byte arrive during the grace period?".
    fn decoder_receiving(bytes: &[u8], quiet: bool) -> (Decoder, Vec<Input>) {
        let mut decoder = Decoder::new();
        let mut inputs = decoder.feed_all(bytes);
        if quiet && decoder.pending() {
            inputs.extend(decoder.flush_escape());
        }
        (decoder, inputs)
    }

    #[test]
    fn a_panel_never_outgrows_the_terminal() {
        // D-48: `1 + panel + 1` lines must fit `rows`, or the terminal scrolls and the caret lands
        // one row off on every repaint.
        let panel: Vec<String> = (0..6).map(|index| format!("line {index}")).collect();
        for rows in 1..=10usize {
            let fitted = panel_fits(&panel, rows);
            assert!(
                1 + fitted.len() + 1 <= rows.max(2),
                "{rows} rows held {} panel lines",
                fitted.len()
            );
            assert_eq!(
                fitted,
                panel[..fitted.len()],
                "the panel keeps its top lines"
            );
        }
        assert_eq!(
            panel_fits(&panel, 10).len(),
            6,
            "a tall terminal keeps it all"
        );
        assert_eq!(
            panel_fits(&panel, 6).len(),
            4,
            "rows - 1 surface - 1 status"
        );
    }

    #[test]
    fn an_escape_mid_character_does_not_swallow_the_bytes_after_it() {
        // D-56: the lead byte of `中` (`0xe4`), then an arrow key, then the rest of the character.
        // The escape path used to leave `expect = 3` standing, so the two continuation bytes and
        // whatever came after them were glued onto the abandoned lead: the arrow key was `Up` and
        // the next real keystroke was eaten by a character nobody typed.
        // The orphaned continuation bytes are not text on their own, so they are ignored rather
        // than stitched onto the next character. The escape is the only key in the stream.
        let inputs = keys(b"\xe4\x1b[A\xb8\xad");
        assert_eq!(
            inputs,
            vec![
                Input::Key(Key::Up, Modifiers::none()),
                Input::Ignored,
                Input::Ignored,
            ],
            "the escape must abandon the half character, not join it"
        );
        // The whole character arriving without interruption still types.
        assert_eq!(
            keys("中".as_bytes()),
            vec![Input::Key(Key::Character('中'), Modifiers::none())]
        );

        // And a read that ends mid-character leaves nothing behind for the next read.
        let (mut decoder, inputs) = decoder_receiving(b"\xe4\xe4", false);
        assert!(inputs.is_empty());
        let later = decoder.feed_all(b"\x1b[A");
        assert_eq!(later, vec![Input::Key(Key::Up, Modifiers::none())]);
    }

    #[test]
    fn a_character_split_across_reads_survives_a_flush_with_nothing_pending() {
        // D-56's other half, and the mistake the first version of the fix made: `flush_escape`
        // cleared the UTF-8 state unconditionally, so a caller that settled a read with nothing
        // awaiting a decision threw away a character that was still arriving. A tty splitting
        // three bytes across two reads is the ordinary case, not the hostile one.
        let mut decoder = Decoder::new();
        let bytes = "中".as_bytes();
        assert!(decoder
            .feed_all(bytes.get(..2).expect("two bytes"))
            .is_empty());
        assert_eq!(decoder.flush_escape(), None, "nothing was pending");
        assert_eq!(
            decoder.feed_all(bytes.get(2..).expect("one byte")),
            vec![Input::Key(Key::Character('中'), Modifiers::none())],
            "the character still completed"
        );
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
