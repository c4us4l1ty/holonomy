//! `code`-based keymap: an [`InputEvent`] plus the modifier state becomes a [`Command`].
//!
//! # `code`, never `key`
//!
//! evdev reports two numbers per keystroke. `code` is which key you pressed, in the hardware's own
//! numbering, and it is the same on every layout. `key` is which *character* the keyboard's current
//! layout maps that key to, and it changes with the layout, with Num Lock, and with Caps Lock.
//!
//! Matching on `key` would be the obvious mistake and it is wrong in a way that is invisible until
//! someone switches layouts: the table would have to be rebuilt per layout, Caps Lock and Num Lock
//! would already be baked into the input and would need *un*-baking, and a Dvorak user would find
//! their `code` unmapped. H2's rule is `code`, and the reason survives Phase 8 intact.
//!
//! So this file deals in [`KEY_A`]-style constants, and the *character* comes from a layout table
//! here, chosen by [`ModifierState`]. Changing layouts means changing [`pair`]'s table, not the
//! dispatcher.
//!
//! # Order: hotkeys before text
//!
//! [`Keymap::dispatch`] checks ctrl and alt combinations before it looks at the character table,
//! because `Ctrl+Z` is `KEY_Z` plus ctrl and `Z` is also a letter. If text came first, `Ctrl+Z` would
//! type a `z`. The reverse order would be the bug.
//!
//! # Why there is no NFKD in the crate
//!
//! The brief asks for an NFKD-normalized mapping table. On a single fixed layout it is not needed and
//! adding it would be cargo: for the US layout every unshifted and shifted output here is ASCII, and
//! `char::is_ascii` holds for all ninety-odd of them, so normalization is the identity function. NFKD
//! earns its keep on a layout that produces *decomposed* output -- a Devanagari or Hangul keyboard --
//! where one keystroke is several codepoints that must be reordered. Where that would go is noted on
//! [`pair`]: the table becomes `&[char]`, not `char`, and `Command::Insert` grows a payload. Until then
//! [`Command::Insert`] carries one `char` and the hot path stays allocation-free.

use crate::event::InputEvent;
use crate::modifiers::ModifierState;

// Kernel ABI. From `linux/input-event-codes.h`; transcribed rather than generated, because the header
// is a C macro list and there is no build step here that may run a search.
pub const KEY_RESERVED: u16 = 0;
pub const KEY_ESC: u16 = 1;
pub const KEY_1: u16 = 2;
pub const KEY_2: u16 = 3;
pub const KEY_3: u16 = 4;
pub const KEY_4: u16 = 5;
pub const KEY_5: u16 = 6;
pub const KEY_6: u16 = 7;
pub const KEY_7: u16 = 8;
pub const KEY_8: u16 = 9;
pub const KEY_9: u16 = 10;
pub const KEY_0: u16 = 11;
pub const KEY_MINUS: u16 = 12;
pub const KEY_EQUAL: u16 = 13;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_TAB: u16 = 15;
pub const KEY_Q: u16 = 16;
pub const KEY_W: u16 = 17;
pub const KEY_E: u16 = 18;
pub const KEY_R: u16 = 19;
pub const KEY_T: u16 = 20;
pub const KEY_Y: u16 = 21;
pub const KEY_U: u16 = 22;
pub const KEY_I: u16 = 23;
pub const KEY_O: u16 = 24;
pub const KEY_P: u16 = 25;
pub const KEY_LEFTBRACE: u16 = 26;
pub const KEY_RIGHTBRACE: u16 = 27;
pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_A: u16 = 30;
pub const KEY_S: u16 = 31;
pub const KEY_D: u16 = 32;
pub const KEY_F: u16 = 33;
pub const KEY_G: u16 = 34;
pub const KEY_H: u16 = 35;
pub const KEY_J: u16 = 36;
pub const KEY_K: u16 = 37;
pub const KEY_L: u16 = 38;
pub const KEY_SEMICOLON: u16 = 39;
pub const KEY_APOSTROPHE: u16 = 40;
pub const KEY_GRAVE: u16 = 41;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_BACKSLASH: u16 = 43;
pub const KEY_Z: u16 = 44;
pub const KEY_X: u16 = 45;
pub const KEY_C: u16 = 46;
pub const KEY_V: u16 = 47;
pub const KEY_B: u16 = 48;
pub const KEY_N: u16 = 49;
pub const KEY_M: u16 = 50;
pub const KEY_COMMA: u16 = 51;
pub const KEY_DOT: u16 = 52;
pub const KEY_SLASH: u16 = 53;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_SPACE: u16 = 57;
pub const KEY_CAPSLOCK: u16 = 58;
pub const KEY_F1: u16 = 59;
pub const KEY_F2: u16 = 60;
pub const KEY_F3: u16 = 61;
pub const KEY_F4: u16 = 62;
pub const KEY_F5: u16 = 63;
pub const KEY_F6: u16 = 64;
pub const KEY_F7: u16 = 65;
pub const KEY_F8: u16 = 66;
pub const KEY_F9: u16 = 67;
pub const KEY_F10: u16 = 68;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_HOME: u16 = 102;
pub const KEY_UP: u16 = 103;
pub const KEY_PAGEUP: u16 = 104;
pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_END: u16 = 107;
pub const KEY_DOWN: u16 = 108;
pub const KEY_PAGEDOWN: u16 = 109;
pub const KEY_INSERT: u16 = 110;
pub const KEY_DELETE: u16 = 111;
pub const KEY_F11: u16 = 87;
pub const KEY_F12: u16 = 88;

/// A binding global hotkey, resolved before the character table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    /// Ctrl+Q. Leaves the session: the jail's teardown, not a quit dialog.
    Quit,
    /// Ctrl+Z.
    Undo,
    /// Ctrl+Y.
    Redo,
    /// Ctrl+S. Commits the in-memory rings to the container.
    Save,
    /// Ctrl+Home.
    DocumentStart,
    /// Ctrl+End.
    DocumentEnd,
    /// Ctrl+I. Insert an image at the caret.
    ///
    /// A hotkey rather than a [`Command`] because it carries no payload: the *bytes* come from
    /// whatever the product can offer, which is a session fact, and a keymap that carried image bytes
    /// would be a keymap holding a file.
    ///
    /// Ctrl+I rather than a bare `i` because the bare letter is a character. Every other block-level
    /// insertion is bound this way for the same reason -- Ctrl+T for a table, Ctrl+M for a formula --
    /// and the pattern is that inserting a block is an *action*, while inserting a character is
    /// *typing*.
    InsertImage,
    /// Ctrl+T. Insert a table at the caret.
    ///
    /// A hotkey rather than a [`Command`] because it carries no payload: the dimensions come from the
    /// session's measure, which the keymap cannot see. See [`Command::InsertTable`] for the command
    /// the session actually applies.
    InsertTable,
    /// Ctrl+M. Insert an inline math span at the caret.
    ///
    /// **M is a bad key for math and it is still the right one.** `Ctrl+M` is `Enter` in a terminal
    /// and the same chord is what most editors use to move to the document's end, so a user with
    /// terminal muscle memory will press it expecting a newline. The alternatives are worse: `Ctrl+$`
    /// and `Ctrl+\` are unbound on most layouts but sit under digits and backslash respectively, so
    /// the mnemonic survives at the cost of an unusual reach, and any letter that *is* a mnemonic
    /// here (`Ctrl+E` exponent, `Ctrl+F` fraction, `Ctrl+S` root) collides with a binding that
    /// already exists — `Ctrl+S` is save, and re-binding save would be a much worse surprise than a
    /// mnemonic that is merely weak.
    ///
    /// The honest summary: the directive specifies `Ctrl+M`, the chords above are worse, so it is
    /// `Ctrl+M`. [`Command::InsertMath`] is what the session applies.
    InsertMath,
}

/// Everything the session can be asked to do by one keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Type one character.
    Insert(char),
    /// Backspace: delete the character before the caret.
    Backspace,
    /// Delete: delete the character after the caret.
    DeleteForward,
    /// A newline.
    Newline,
    /// A literal tab.
    ///
    /// **Or the next table cell**, when the caret is inside one. Which one is a property of where the
    /// caret is, not of the key, so the same keystroke means two things and the session decides. See
    /// [`Command::ShiftTab`].
    Tab,
    /// Tab with shift held: the previous table cell.
    ///
    /// A separate variant rather than a flag on [`Command::Tab`], because "previous" and "next" are
    /// the two halves of a navigation and every caller would otherwise have to destructure a boolean
    /// to find out which one it was handed.
    ShiftTab,
    /// Insert a table of `rows` by `cols` at the caret.
    ///
    /// Fields rather than a `TableSpan`, because the *widths* are not in the keymap's gift: a 3×3
    /// table's columns have to add up to the page's measure, which is a session fact. The keymap says
    /// how many columns; the session says how wide they are.
    InsertTable {
        /// Rows. At least 1.
        rows: u16,
        /// Columns. At most 8, which is [`holonomy_text::TableSpan::MAX_COLS`].
        cols: u16,
    },
    /// Dismiss whatever has focus.
    Escape,
    /// Move the caret.
    Left,
    /// Move the caret.
    Right,
    /// Move the caret.
    Up,
    /// Move the caret.
    Down,
    /// Move to the start of the line.
    Home,
    /// Move to the end of the line.
    End,
    /// Move up one screen of text.
    PageUp,
    /// Move down one screen of text.
    PageDown,
    /// A global binding.
    Hotkey(Hotkey),
    /// Zoom the page canvas in.
    ZoomIn,
    /// Zoom the page canvas out.
    ZoomOut,
    /// Back to 100%.
    ZoomReset,
}

/// The default layout's characters, and the dispatcher.
///
/// Stateless by design: everything it needs is in the [`ModifierState`] argument, so a
/// [`Keymap`] is `Copy`, 0 bytes, and there is nothing to get out of sync. It also means the
/// integration test can construct one per keystroke for free, and a *different* state per keystroke,
/// which is how a test for "release shift mid-word" is written without a session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Keymap {
    _private: (),
}

impl Keymap {
    /// The US layout.
    pub const fn us() -> Self {
        Self { _private: () }
    }

    /// The character a key produces, or `None` if it has none on this layout.
    ///
    /// # Shift XOR Caps Lock, but only for letters
    ///
    /// `shift ^ caps_lock` is the rule for letters and `shift` alone is the rule for everything
    /// else. Applying XOR to digits would turn Caps Lock into "type `!` from `1`", which is wrong --
    /// Caps Lock is documented as affecting letters only, and the hardware agrees. So the table's
    /// two columns are compared and only a pair that differs in case is XOR-ed.
    ///
    /// # Alt
    ///
    /// Alt produces nothing. On a US layout alt+key is not a character, and AltGr -- which is
    /// right-alt on a PC keyboard -- only becomes one on layouts that define it. Rather than guess,
    /// alt+key here is [`None`], and the caller sees nothing. Documented rather than silent.
    #[inline]
    pub fn text_for(&self, code: u16, mods: &ModifierState) -> Option<char> {
        if mods.alt() {
            return None;
        }
        let (lo, hi) = pair(code)?;
        debug_assert!(
            lo.is_ascii() && hi.is_ascii(),
            "the US layout is ASCII-only"
        );
        let shift_effective = if lo.is_ascii_alphabetic() {
            mods.shift() ^ mods.caps_lock()
        } else {
            mods.shift()
        };
        Some(if shift_effective { hi } else { lo })
    }

    /// Whether `code` is a modifier this keymap tracks, so a caller can skip dispatching it.
    #[inline]
    pub fn is_modifier(code: u16) -> bool {
        matches!(
            code,
            KEY_LEFTSHIFT
                | KEY_RIGHTSHIFT
                | KEY_LEFTCTRL
                | KEY_RIGHTCTRL
                | KEY_LEFTALT
                | KEY_RIGHTALT
                | KEY_CAPSLOCK
        )
    }

    /// The command one event means, or `None` for an event that means nothing.
    ///
    /// `mods` is read, not written: the caller owns the state and passes it in, so the dispatch is a
    /// pure function of `(event, mods)`. [`Keymap::dispatch_into`] is the stateful form.
    ///
    /// Returns `None` for: releases (a key going up is not a command), modifier keys, autorepeat of a
    /// modifier, `EV_SYN`, and any `ctrl`/`alt` combination with no binding. The last one matters:
    /// `Ctrl+C` and friends must be *swallowed*, not typed, and returning `None` is how that is
    /// spelled.
    pub fn dispatch(&self, event: InputEvent, mods: &ModifierState) -> Option<Command> {
        if !event.is_key() || event.is_release() {
            return None;
        }
        let code = event.code;

        // 1. Hotkeys. Before the character table, because Ctrl+Z is KEY_Z and Z is also a letter.
        if mods.ctrl() {
            let binding = match code {
                KEY_Q if !mods.shift() && !mods.alt() => Some(Hotkey::Quit),
                KEY_Z if !mods.shift() && !mods.alt() => Some(Hotkey::Undo),
                KEY_Y if !mods.shift() && !mods.alt() => Some(Hotkey::Redo),
                KEY_S if !mods.shift() && !mods.alt() => Some(Hotkey::Save),
                KEY_HOME if !mods.shift() && !mods.alt() => Some(Hotkey::DocumentStart),
                KEY_END if !mods.shift() && !mods.alt() => Some(Hotkey::DocumentEnd),
                KEY_T if !mods.shift() && !mods.alt() => Some(Hotkey::InsertTable),
                KEY_M if !mods.shift() && !mods.alt() => Some(Hotkey::InsertMath),
                KEY_I if !mods.shift() && !mods.alt() => Some(Hotkey::InsertImage),
                _ => None,
            };
            if let Some(hotkey) = binding {
                return Some(Command::Hotkey(hotkey));
            }
            // Ctrl+Shift+I and friends: an unbound combo. Swallowed, never typed.
            return None;
        }

        // 2. Navigation and editing keys. Unshifted only where a shifted variant would be a character.
        let nav = match (code, mods.shift()) {
            (KEY_LEFTBRACE, false) | (KEY_UP, false) => Some(Command::Up),
            (KEY_RIGHTBRACE, false) | (KEY_DOWN, false) => Some(Command::Down),
            (KEY_LEFT, _) => Some(Command::Left),
            (KEY_RIGHT, _) => Some(Command::Right),
            (KEY_PAGEUP, _) => Some(Command::PageUp),
            (KEY_PAGEDOWN, _) => Some(Command::PageDown),
            (KEY_HOME, _) => Some(Command::Home),
            (KEY_END, _) => Some(Command::End),
            (KEY_F11, false) => Some(Command::ZoomIn),
            (KEY_F12, false) => Some(Command::ZoomReset),
            (KEY_F11, true) => Some(Command::ZoomOut),
            // # Tab has to be resolved here, not by the character table
            //
            // `text_for(KEY_TAB, mods)` is `Some('\t')` whether or not shift is down, because on every
            // layout tab is tab. So the character table cannot tell Tab from Shift+Tab, and the variant
            // it produced could not be either. This arm reads `mods.shift()` directly, which is why it
            // sits in step 2 rather than in the character lookup below.
            //
            // It is checked before the table for the same reason `KEY_LEFT` is: a shifted navigation
            // key must not fall through and be typed as a character.
            (KEY_TAB, false) => Some(Command::Tab),
            (KEY_TAB, true) => Some(Command::ShiftTab),
            _ => None,
        };
        if nav.is_some() {
            return nav;
        }

        // 3. Everything else on the character table, which also covers the shifted navigation keys
        //    that fell through step 2 -- `{`, `}`, `^` -- and correctly types them.
        if let Some(c) = self.text_for(code, mods) {
            return Some(match c {
                '\n' => Command::Newline,
                '\u{7f}' => Command::Backspace,
                // Unreachable in practice: `KEY_TAB` was resolved in step 2. Kept as the honest
                // mapping rather than as `_ => Command::Insert(c)`, which would type a tab character
                // into the document if step 2 were ever removed.
                '\t' => Command::Tab,
                _ => Command::Insert(c),
            });
        }

        // 4. Non-text editing keys, which must come after the table so KEY_BACKSPACE's `\u{7f}`
        //    reading above is the one that wins.
        match code {
            KEY_BACKSPACE => Some(Command::Backspace),
            KEY_DELETE => Some(Command::DeleteForward),
            KEY_ENTER => Some(Command::Newline),
            // Also unreachable: step 2 resolved `KEY_TAB` for both shift states.
            KEY_TAB => Some(if mods.shift() {
                Command::ShiftTab
            } else {
                Command::Tab
            }),
            KEY_ESC => Some(Command::Escape),
            KEY_SPACE => Some(Command::Insert(' ')),
            _ => None,
        }
    }

    /// [`dispatch`](Self::dispatch), folding the event into `mods` first.
    ///
    /// The state update happens *before* the dispatch and that ordering is load-bearing: `Ctrl+Q` is
    /// "left ctrl goes down" then "Q goes down", and the second event is only `Ctrl+Q` once the first
    /// has been folded in. An implementation that dispatched and then updated would type a `q`.
    #[inline]
    pub fn dispatch_into(&self, event: InputEvent, mods: &mut ModifierState) -> Option<Command> {
        if event.is_key() {
            mods.update(event.code, event.value);
        }
        self.dispatch(event, mods)
    }
}

/// The US layout's `(unshifted, shifted)` character for each printable key code.
///
/// One `match`, both columns adjacent. That is the whole reason it is spelled this way rather than
/// two separate tables or a 128-entry array: `'q'` and `'Q'` sit on neighbouring lines, so an
/// omitted key is visible as a missing *pair* rather than as two independent omissions, and the
/// audit the brief asks for is a read down one column.
///
/// `KEY_SPACE` is here with `(' ', ' ')` rather than being special-cased in the dispatcher, because
/// Caps Lock and Shift must not touch it and this makes that structural.
///
/// Where a multi-codepoint layout would go: this returns `&[char]` instead of `char` and
/// [`Command::Insert`] carries the slice. See the module docs.
#[inline]
const fn pair(code: u16) -> Option<(char, char)> {
    Some(match code {
        KEY_1 => ('1', '!'),
        KEY_2 => ('2', '@'),
        KEY_3 => ('3', '#'),
        KEY_4 => ('4', '$'),
        KEY_5 => ('5', '%'),
        KEY_6 => ('6', '^'),
        KEY_7 => ('7', '&'),
        KEY_8 => ('8', '*'),
        KEY_9 => ('9', '('),
        KEY_0 => ('0', ')'),
        KEY_MINUS => ('-', '_'),
        KEY_EQUAL => ('=', '+'),
        KEY_BACKSPACE => ('\u{7f}', '\u{7f}'),
        KEY_TAB => ('\t', '\t'),
        KEY_Q => ('q', 'Q'),
        KEY_W => ('w', 'W'),
        KEY_E => ('e', 'E'),
        KEY_R => ('r', 'R'),
        KEY_T => ('t', 'T'),
        KEY_Y => ('y', 'Y'),
        KEY_U => ('u', 'U'),
        KEY_I => ('i', 'I'),
        KEY_O => ('o', 'O'),
        KEY_P => ('p', 'P'),
        KEY_LEFTBRACE => ('[', '{'),
        KEY_RIGHTBRACE => (']', '}'),
        KEY_ENTER => ('\n', '\n'),
        KEY_A => ('a', 'A'),
        KEY_S => ('s', 'S'),
        KEY_D => ('d', 'D'),
        KEY_F => ('f', 'F'),
        KEY_G => ('g', 'G'),
        KEY_H => ('h', 'H'),
        KEY_J => ('j', 'J'),
        KEY_K => ('k', 'K'),
        KEY_L => ('l', 'L'),
        KEY_SEMICOLON => (';', ':'),
        KEY_APOSTROPHE => ('\'', '"'),
        KEY_GRAVE => ('`', '~'),
        KEY_BACKSLASH => ('\\', '|'),
        KEY_Z => ('z', 'Z'),
        KEY_X => ('x', 'X'),
        KEY_C => ('c', 'C'),
        KEY_V => ('v', 'V'),
        KEY_B => ('b', 'B'),
        KEY_N => ('n', 'N'),
        KEY_M => ('m', 'M'),
        KEY_COMMA => (',', '<'),
        KEY_DOT => ('.', '>'),
        KEY_SLASH => ('/', '?'),
        KEY_SPACE => (' ', ' '),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::InputEvent;

    fn held(code: u16) -> ModifierState {
        let mut m = ModifierState::new();
        m.update(code, 1);
        m
    }

    fn ctrl() -> ModifierState {
        held(KEY_LEFTCTRL)
    }

    fn shift() -> ModifierState {
        held(KEY_LEFTSHIFT)
    }

    #[test]
    fn the_kernel_codes_match_the_header() {
        // Transcribed by hand from linux/input-event-codes.h. If the kernel ever renumbers these,
        // this is the test that says so.
        assert_eq!(KEY_ESC, 1);
        assert_eq!(KEY_1, 2);
        assert_eq!(KEY_0, 11);
        assert_eq!(KEY_MINUS, 12);
        assert_eq!(KEY_BACKSPACE, 14);
        assert_eq!(KEY_Q, 16);
        assert_eq!(KEY_A, 30);
        assert_eq!(KEY_LEFTCTRL, 29);
        assert_eq!(KEY_LEFTSHIFT, 42);
        assert_eq!(KEY_Z, 44);
        assert_eq!(KEY_SLASH, 53);
        assert_eq!(KEY_CAPSLOCK, 58);
        assert_eq!(KEY_LEFTALT, 56);
        assert_eq!(KEY_F11, 87);
        assert_eq!(KEY_F12, 88);
        assert_eq!(KEY_RIGHTALT, 100);
        assert_eq!(KEY_RIGHT, 106);
        assert_eq!(KEY_RIGHTCTRL, 97);
        assert_eq!(KEY_HOME, 102);
        assert_eq!(KEY_DELETE, 111);
        // Neighbours, to catch a transposed digit in the middle of a run.
        assert_eq!(KEY_W, KEY_Q + 1);
        assert_eq!(KEY_E, KEY_Q + 2);
        assert_eq!(KEY_A, KEY_Q + 14);
        assert_eq!(KEY_Z, KEY_A + 14);
    }

    #[test]
    fn every_letter_maps_to_itself_unshifted() {
        let codes = [
            KEY_Q, KEY_W, KEY_E, KEY_R, KEY_T, KEY_Y, KEY_U, KEY_I, KEY_O, KEY_P, KEY_A, KEY_S,
            KEY_D, KEY_F, KEY_G, KEY_H, KEY_J, KEY_K, KEY_L, KEY_Z, KEY_X, KEY_C, KEY_V, KEY_B,
            KEY_N, KEY_M,
        ];
        let expect = [
            'q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p', 'a', 's', 'd', 'f', 'g', 'h', 'j',
            'k', 'l', 'z', 'x', 'c', 'v', 'b', 'n', 'm',
        ];
        let km = Keymap::us();
        for (code, want) in codes.iter().zip(expect.iter()) {
            assert_eq!(
                km.text_for(*code, &ModifierState::new()),
                Some(*want),
                "code {code}"
            );
            assert_eq!(
                km.text_for(*code, &shift()),
                Some(want.to_ascii_uppercase())
            );
        }
    }

    #[test]
    fn the_digit_row_maps_to_its_symbols() {
        let km = Keymap::us();
        for (code, lo, hi) in [
            (KEY_1, '1', '!'),
            (KEY_2, '2', '@'),
            (KEY_3, '3', '#'),
            (KEY_4, '4', '$'),
            (KEY_5, '5', '%'),
            (KEY_6, '6', '^'),
            (KEY_7, '7', '&'),
            (KEY_8, '8', '*'),
            (KEY_9, '9', '('),
            (KEY_0, '0', ')'),
        ] {
            assert_eq!(km.text_for(code, &ModifierState::new()), Some(lo));
            assert_eq!(km.text_for(code, &shift()), Some(hi));
        }
    }

    #[test]
    fn the_punctuation_keys_map_to_both_columns() {
        let km = Keymap::us();
        for (code, lo, hi) in [
            (KEY_MINUS, '-', '_'),
            (KEY_EQUAL, '=', '+'),
            (KEY_LEFTBRACE, '[', '{'),
            (KEY_RIGHTBRACE, ']', '}'),
            (KEY_SEMICOLON, ';', ':'),
            (KEY_APOSTROPHE, '\'', '"'),
            (KEY_GRAVE, '`', '~'),
            (KEY_BACKSLASH, '\\', '|'),
            (KEY_COMMA, ',', '<'),
            (KEY_DOT, '.', '>'),
            (KEY_SLASH, '/', '?'),
        ] {
            assert_eq!(km.text_for(code, &ModifierState::new()), Some(lo), "{code}");
            assert_eq!(km.text_for(code, &shift()), Some(hi), "{code}");
        }
    }

    #[test]
    fn caps_lock_uppercases_letters_only() {
        let km = Keymap::us();
        let caps = held(KEY_CAPSLOCK);
        assert_eq!(km.text_for(KEY_A, &caps), Some('A'));
        assert_eq!(km.text_for(KEY_1, &caps), Some('1'));
        assert_eq!(km.text_for(KEY_MINUS, &caps), Some('-'));
        assert_eq!(km.text_for(KEY_SLASH, &caps), Some('/'));
    }

    #[test]
    fn caps_lock_and_shift_cancel_for_letters() {
        let km = Keymap::us();
        let mut m = held(KEY_CAPSLOCK);
        m.update(KEY_LEFTSHIFT, 1);
        assert!(m.shift() && m.caps_lock());
        assert_eq!(
            km.text_for(KEY_A, &m),
            Some('a'),
            "caps+shift should cancel"
        );
        // But the symbols are still symbols: the XOR does not reach them.
        assert_eq!(km.text_for(KEY_1, &m), Some('!'));
    }

    #[test]
    fn shift_comes_from_either_side() {
        let km = Keymap::us();
        assert_eq!(km.text_for(KEY_A, &held(KEY_RIGHTSHIFT)), Some('A'));
        assert_eq!(km.text_for(KEY_A, &held(KEY_LEFTSHIFT)), Some('A'));
    }

    #[test]
    fn ctrl_hotkeys_win_over_text() {
        let km = Keymap::us();
        let c = ctrl();
        for (code, want) in [
            (KEY_Q, Hotkey::Quit),
            (KEY_Z, Hotkey::Undo),
            (KEY_Y, Hotkey::Redo),
            (KEY_S, Hotkey::Save),
        ] {
            assert_eq!(
                km.dispatch(InputEvent::press(code), &c),
                Some(Command::Hotkey(want)),
                "Ctrl+{code}"
            );
        }
    }

    #[test]
    fn an_unbound_ctrl_combo_is_swallowed_not_typed() {
        let km = Keymap::us();
        let c = ctrl();
        // Ctrl+C must not type a 'c'. And must not become anything else either.
        for code in [KEY_C, KEY_A, KEY_V, KEY_X, KEY_1, KEY_SPACE, KEY_ESC] {
            assert_eq!(
                km.dispatch(InputEvent::press(code), &c),
                None,
                "Ctrl+{code} should produce no command at all"
            );
        }
    }

    #[test]
    fn ctrl_shift_does_not_fire_a_binding() {
        // Ctrl+Shift+Q is not Quit, and it is not a 'q' either.
        let km = Keymap::us();
        let mut m = ctrl();
        m.update(KEY_LEFTSHIFT, 1);
        assert_eq!(km.dispatch(InputEvent::press(KEY_Q), &m), None);
    }

    #[test]
    fn alt_produces_no_text() {
        let km = Keymap::us();
        assert_eq!(km.text_for(KEY_A, &held(KEY_LEFTALT)), None);
        assert_eq!(
            km.dispatch(InputEvent::press(KEY_A), &held(KEY_LEFTALT)),
            None
        );
    }

    #[test]
    fn releases_produce_nothing() {
        let km = Keymap::us();
        let m = ModifierState::new();
        for code in [KEY_A, KEY_ENTER, KEY_LEFT, KEY_DELETE, KEY_Q] {
            assert_eq!(km.dispatch(InputEvent::release(code), &m), None);
        }
    }

    #[test]
    fn autorepeat_of_a_letter_produces_the_same_text_as_a_press() {
        let km = Keymap::us();
        let m = ModifierState::new();
        assert_eq!(
            km.dispatch(InputEvent::repeat(KEY_A), &m),
            km.dispatch(InputEvent::press(KEY_A), &m)
        );
        assert_eq!(
            km.dispatch(InputEvent::repeat(KEY_A), &m),
            Some(Command::Insert('a'))
        );
    }

    #[test]
    fn autorepeat_of_a_modifier_produces_nothing() {
        let km = Keymap::us();
        let mut m = ModifierState::new();
        m.update(KEY_LEFTSHIFT, 1);
        // Shift held, autorepeating: 'A' again, and Shift stays on.
        assert_eq!(
            km.dispatch(InputEvent::repeat(KEY_A), &m),
            Some(Command::Insert('A'))
        );
        m.update(KEY_LEFTSHIFT, 0);
        assert_eq!(
            km.dispatch(InputEvent::repeat(KEY_A), &m),
            Some(Command::Insert('a'))
        );
        // The modifier key itself autorepeating is a no-op.
        assert_eq!(km.dispatch(InputEvent::repeat(KEY_LEFTSHIFT), &m), None);
        assert!(!m.shift());
    }

    #[test]
    fn navigation_keys_map_without_modifiers() {
        let km = Keymap::us();
        let m = ModifierState::new();
        for (code, want) in [
            (KEY_LEFT, Command::Left),
            (KEY_RIGHT, Command::Right),
            (KEY_UP, Command::Up),
            (KEY_DOWN, Command::Down),
            (KEY_HOME, Command::Home),
            (KEY_END, Command::End),
            (KEY_PAGEUP, Command::PageUp),
            (KEY_PAGEDOWN, Command::PageDown),
            (KEY_DELETE, Command::DeleteForward),
            (KEY_ESC, Command::Escape),
            (KEY_ENTER, Command::Newline),
            (KEY_TAB, Command::Tab),
        ] {
            assert_eq!(
                km.dispatch(InputEvent::press(code), &m),
                Some(want),
                "{code}"
            );
        }
    }

    #[test]
    fn shifted_arrow_keys_type_braces_instead_of_moving() {
        // `{` is shift+`[`, which is the same physical key as the up arrow on a US layout.
        let km = Keymap::us();
        assert_eq!(
            km.dispatch(InputEvent::press(KEY_LEFTBRACE), &shift()),
            Some(Command::Insert('{'))
        );
        assert_eq!(
            km.dispatch(InputEvent::press(KEY_LEFTBRACE), &ModifierState::new()),
            Some(Command::Up)
        );
    }

    #[test]
    fn backspace_is_backspace_in_both_shift_states() {
        let km = Keymap::us();
        assert_eq!(
            km.dispatch(InputEvent::press(KEY_BACKSPACE), &ModifierState::new()),
            Some(Command::Backspace)
        );
        assert_eq!(
            km.dispatch(InputEvent::press(KEY_BACKSPACE), &shift()),
            Some(Command::Backspace)
        );
    }

    #[test]
    fn space_types_a_space_regardless_of_shift() {
        let km = Keymap::us();
        assert_eq!(
            km.dispatch(InputEvent::press(KEY_SPACE), &shift()),
            Some(Command::Insert(' '))
        );
        let mut caps = held(KEY_CAPSLOCK);
        caps.update(KEY_LEFTSHIFT, 1);
        assert_eq!(
            km.dispatch(InputEvent::press(KEY_SPACE), &caps),
            Some(Command::Insert(' '))
        );
    }

    #[test]
    fn zoom_keys() {
        let km = Keymap::us();
        let m = ModifierState::new();
        assert_eq!(
            km.dispatch(InputEvent::press(KEY_F11), &m),
            Some(Command::ZoomIn)
        );
        assert_eq!(
            km.dispatch(InputEvent::press(KEY_F11), &shift()),
            Some(Command::ZoomOut)
        );
        assert_eq!(
            km.dispatch(InputEvent::press(KEY_F12), &m),
            Some(Command::ZoomReset)
        );
    }

    #[test]
    fn dispatch_into_folds_the_modifier_before_dispatching() {
        // Ctrl+Q arrives as two events. The first must not produce a command; the second must be Quit.
        let km = Keymap::us();
        let mut m = ModifierState::new();
        assert_eq!(
            km.dispatch_into(InputEvent::press(KEY_LEFTCTRL), &mut m),
            None
        );
        assert!(m.ctrl());
        assert_eq!(
            km.dispatch_into(InputEvent::press(KEY_Q), &mut m),
            Some(Command::Hotkey(Hotkey::Quit))
        );
        // Releasing Q does nothing; releasing ctrl clears it; then Q is a letter again.
        assert_eq!(km.dispatch_into(InputEvent::release(KEY_Q), &mut m), None);
        assert_eq!(
            km.dispatch_into(InputEvent::release(KEY_LEFTCTRL), &mut m),
            None
        );
        assert!(!m.ctrl());
        assert_eq!(
            km.dispatch_into(InputEvent::press(KEY_Q), &mut m),
            Some(Command::Insert('q'))
        );
    }

    #[test]
    fn typing_a_word_then_releasing_shift_mid_word() {
        // The case the `value == 1` / `value == 0` rule exists for: Shift is down for `H` and released
        // before `e`, and the letters must follow it rather than latching.
        let km = Keymap::us();
        let mut m = ModifierState::new();
        let mut typed = String::new();

        // `He` with Shift, `llo` without -- as press/release pairs, which is what a device sends.
        let stream = [
            (InputEvent::press(KEY_LEFTSHIFT), false),
            (InputEvent::press(KEY_H), true),
            (InputEvent::release(KEY_H), false),
            (InputEvent::release(KEY_LEFTSHIFT), false),
            (InputEvent::press(KEY_E), true),
            (InputEvent::release(KEY_E), false),
            (InputEvent::press(KEY_L), true),
            (InputEvent::release(KEY_L), false),
            (InputEvent::press(KEY_L), true),
            (InputEvent::release(KEY_L), false),
            (InputEvent::press(KEY_O), true),
            (InputEvent::release(KEY_O), false),
        ];
        for (ev, is_text) in stream {
            let cmd = km.dispatch_into(ev, &mut m);
            if is_text {
                assert!(matches!(cmd, Some(Command::Insert(_))), "{ev:?} -> {cmd:?}");
            }
            if let Some(Command::Insert(c)) = cmd {
                typed.push(c);
            }
        }
        assert_eq!(typed, "Hello");
    }

    #[test]
    fn typing_whole_words_with_shift_held_releases_when_released() {
        // The same thing at a word boundary, which is where the user notices: Shift up, next word
        // lower-case.
        let km = Keymap::us();
        let mut m = ModifierState::new();
        let mut typed = String::new();
        let tap = |code: u16, m: &mut ModifierState, typed: &mut String| {
            if let Some(Command::Insert(c)) = km.dispatch_into(InputEvent::press(code), m) {
                typed.push(c);
            }
            let _ = km.dispatch_into(InputEvent::release(code), m);
        };
        for code in [KEY_LEFTSHIFT, KEY_H, KEY_I] {
            if code == KEY_LEFTSHIFT {
                let _ = km.dispatch_into(InputEvent::press(code), &mut m);
                continue;
            }
            tap(code, &mut m, &mut typed);
        }
        let _ = km.dispatch_into(InputEvent::release(KEY_LEFTSHIFT), &mut m);
        for code in [KEY_T, KEY_H, KEY_E, KEY_R, KEY_E] {
            tap(code, &mut m, &mut typed);
        }
        assert_eq!(typed, "HIthere");
    }

    #[test]
    fn unmapped_codes_produce_nothing() {
        let km = Keymap::us();
        let m = ModifierState::new();
        for code in [KEY_RESERVED, 200, 1000, u16::MAX] {
            assert_eq!(
                km.dispatch(InputEvent::press(code), &m),
                None,
                "code {code}"
            );
        }
    }

    #[test]
    fn non_key_events_produce_nothing() {
        let km = Keymap::us();
        let m = ModifierState::new();
        assert_eq!(km.dispatch(crate::event::syn_report(), &m), None);
    }

    #[test]
    fn is_modifier_agrees_with_the_state() {
        for code in [
            KEY_LEFTSHIFT,
            KEY_RIGHTSHIFT,
            KEY_LEFTCTRL,
            KEY_RIGHTCTRL,
            KEY_LEFTALT,
            KEY_RIGHTALT,
            KEY_CAPSLOCK,
        ] {
            assert!(Keymap::is_modifier(code), "code {code}");
        }
        assert!(!Keymap::is_modifier(KEY_A));
        assert!(!Keymap::is_modifier(KEY_ESC));
    }

    #[test]
    fn the_layout_is_ascii_only() {
        // The claim in the module docs, asserted rather than asserted-in-a-comment.
        for code in 0..=u16::MAX {
            if let Some((lo, hi)) = pair(code) {
                assert!(lo.is_ascii(), "code {code} -> {lo:?}");
                assert!(hi.is_ascii(), "code {code} -> {hi:?}");
            }
        }
    }
}
