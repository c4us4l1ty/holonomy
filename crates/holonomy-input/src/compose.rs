//! Desktop-only dead-key and Compose support.
//!
//! The sealed evdev path is unchanged. This module is a small X11 text-entry state machine: it
//! consumes keysyms (not physical shortcuts) and returns committed UTF-8. It deliberately does not
//! implement XIM/IME preedit; native IME commit still needs a protocol layer this client does not have.

use unicode_normalization::UnicodeNormalization;

/// What a key did to the composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fed {
    /// Not a text/compose key; let the normal command path handle it.
    Pass,
    /// A dead key or Compose prefix is waiting for the next key.
    Pending,
    /// Commit this text.
    Text(String),
}

#[derive(Debug, Default)]
pub struct Composer {
    dead: Option<char>,
    compose_active: bool,
    compose: Vec<char>,
}

impl Composer {
    pub fn new() -> Self { Self::default() }

    pub fn reset(&mut self) {
        self.dead = None;
        self.compose_active = false;
        self.compose.clear();
    }

    /// Feed one key. `keysym` is the X keysym selected by the server layout; `ch` is the ordinary
    /// character it would type, if any.
    pub fn feed(&mut self, keysym: u32, ch: Option<char>) -> Fed {
        // Compose / Multi_key.
        if keysym == 0xff20 {
            self.dead = None;
            self.compose_active = true;
            self.compose.clear();
            return Fed::Pending;
        }
        if let Some((mark, alone)) = dead_mark(keysym) {
            self.compose_active = false;
            self.compose.clear();
            if self.dead == Some(mark) {
                self.dead = None;
                return Fed::Text(alone.to_string());
            }
            self.dead = Some(mark);
            return Fed::Pending;
        }
        let Some(c) = ch else {
            // Commands cancel a pending accent rather than swallowing the next command.
            if matches!(keysym, 0xff08 | 0xff09 | 0xff0d | 0xff1b | 0xff50..=0xff57 | 0xff60..=0xff67) {
                self.reset();
            }
            return Fed::Pass;
        };
        if self.compose_active {
            self.compose.push(c);
            if let Some(text) = compose_try(&self.compose) {
                self.reset();
                return Fed::Text(text);
            }
            if self.compose.len() >= 3 {
                let text: String = self.compose.iter().collect();
                self.reset();
                return Fed::Text(text);
            }
            return Fed::Pending;
        }
        if let Some(mark) = self.dead.take() {
            if c == ' ' {
                return Fed::Text(standalone_mark(mark).to_string());
            }
            return Fed::Text(combined(c, mark));
        }
        Fed::Text(c.to_string())
    }
}

fn combined(base: char, mark: char) -> String {
    let mut s = String::with_capacity(base.len_utf8() + mark.len_utf8());
    s.push(base);
    s.push(mark);
    s.nfc().collect()
}

fn standalone_mark(mark: char) -> char {
    match mark {
        '\u{0300}' => '`',
        '\u{0301}' => '´',
        '\u{0302}' => '^',
        '\u{0303}' => '~',
        '\u{0304}' => '¯',
        '\u{0306}' => '˘',
        '\u{0307}' => '˙',
        '\u{0308}' => '¨',
        '\u{030a}' => '°',
        '\u{030b}' => '˝',
        '\u{030c}' => 'ˇ',
        '\u{0327}' => '¸',
        '\u{0328}' => '˛',
        _ => mark,
    }
}

fn dead_mark(keysym: u32) -> Option<(char, char)> {
    Some(match keysym {
        0xfe50 => ('\u{0300}', '`'),
        0xfe51 => ('\u{0301}', '´'),
        0xfe52 => ('\u{0302}', '^'),
        0xfe53 => ('\u{0303}', '~'),
        0xfe54 => ('\u{0304}', '¯'),
        0xfe55 => ('\u{0306}', '˘'),
        0xfe56 => ('\u{0307}', '˙'),
        0xfe57 => ('\u{0308}', '¨'),
        0xfe58 => ('\u{030a}', '°'),
        0xfe59 => ('\u{030b}', '˝'),
        0xfe5a => ('\u{030c}', 'ˇ'),
        0xfe5b => ('\u{0327}', '¸'),
        0xfe5c => ('\u{0328}', '˛'),
        _ => return None,
    })
}

fn compose_try(seq: &[char]) -> Option<String> {
    if seq.len() == 2 {
        let (a, b) = (seq[0], seq[1]);
        if let Some(text) = match (a, b) {
            ('<', '<') => Some('«'.into()),
            ('>', '>') => Some('»'.into()),
            ('!', '!') => Some('¡'.into()),
            ('?', '?') => Some('¿'.into()),
            ('-', '-') => Some('–'.into()),
            ('.', '.') => Some('…'.into()),
            ('a', 'e') => Some('æ'.into()),
            ('A', 'E') => Some('Æ'.into()),
            ('o', 'e') => Some('œ'.into()),
            ('O', 'E') => Some('Œ'.into()),
            ('s', 's') => Some('ß'.into()),
            _ => None,
        } {
            return Some(text);
        }
        if let Some(mark) = match a {
            '\'' => Some('\u{0301}'),
            '`' => Some('\u{0300}'),
            '^' => Some('\u{0302}'),
            '~' => Some('\u{0303}'),
            '"' => Some('\u{0308}'),
            ',' => Some('\u{0327}'),
            '-' => Some('\u{0304}'),
            '.' => Some('\u{0307}'),
            'u' => Some('\u{0306}'),
            'v' => Some('\u{030c}'),
            'o' => Some('\u{030a}'),
            _ => None,
        } {
            return Some(combined(b, mark));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_acute_composes_e() {
        let mut c = Composer::new();
        assert_eq!(c.feed(0xfe51, None), Fed::Pending);
        assert_eq!(c.feed(0x65, Some('e')), Fed::Text("é".into()));
    }

    #[test]
    fn compose_multi_key_apostrophe_a() {
        let mut c = Composer::new();
        assert_eq!(c.feed(0xff20, None), Fed::Pending);
        assert_eq!(c.feed(0x27, Some('\'')), Fed::Pending);
        assert_eq!(c.feed(0x61, Some('a')), Fed::Text("á".into()));
    }

    #[test]
    fn dead_key_then_space_yields_standalone_accent() {
        let mut c = Composer::new();
        assert_eq!(c.feed(0xfe51, None), Fed::Pending);
        assert_eq!(c.feed(0x20, Some(' ')), Fed::Text("´".into()));
    }

    #[test]
    fn command_cancels_pending_dead_key() {
        let mut c = Composer::new();
        assert_eq!(c.feed(0xfe51, None), Fed::Pending);
        assert_eq!(c.feed(0xff08, None), Fed::Pass);
        assert_eq!(c.feed(0x61, Some('a')), Fed::Text("a".into()));
    }
}
