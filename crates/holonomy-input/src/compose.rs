//! Desktop-only dead-key and Compose support.
//!
//! The sealed evdev path is unchanged. This module is a small X11 text-entry state machine: it
//! consumes keysyms (not physical shortcuts) and returns committed UTF-8. It deliberately does not
//! implement XIM/IME preedit; native IME commit still needs a protocol layer this client does not have.

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
    if let Some(c) = precomposed(base, mark) { return c.to_string(); }
    let mut s = String::with_capacity(base.len_utf8() + mark.len_utf8());
    s.push(base);
    s.push(mark);
    s
}

/// A compact Latin table instead of linking full Unicode normalization tables into the desktop
/// binary. Unsupported pairs fall back to base + combining mark rather than losing the accent.
fn precomposed(base: char, mark: char) -> Option<char> {
    Some(match (base, mark) {
        ('a','\u{0301}')=>'á', ('e','\u{0301}')=>'é', ('i','\u{0301}')=>'í', ('o','\u{0301}')=>'ó', ('u','\u{0301}')=>'ú', ('y','\u{0301}')=>'ý',
        ('A','\u{0301}')=>'Á', ('E','\u{0301}')=>'É', ('I','\u{0301}')=>'Í', ('O','\u{0301}')=>'Ó', ('U','\u{0301}')=>'Ú', ('Y','\u{0301}')=>'Ý',
        ('c','\u{0301}')=>'ć', ('n','\u{0301}')=>'ń', ('r','\u{0301}')=>'ŕ', ('s','\u{0301}')=>'ś', ('z','\u{0301}')=>'ź',
        ('C','\u{0301}')=>'Ć', ('N','\u{0301}')=>'Ń', ('R','\u{0301}')=>'Ŕ', ('S','\u{0301}')=>'Ś', ('Z','\u{0301}')=>'Ź',
        ('a','\u{0300}')=>'à', ('e','\u{0300}')=>'è', ('i','\u{0300}')=>'ì', ('o','\u{0300}')=>'ò', ('u','\u{0300}')=>'ù',
        ('A','\u{0300}')=>'À', ('E','\u{0300}')=>'È', ('I','\u{0300}')=>'Ì', ('O','\u{0300}')=>'Ò', ('U','\u{0300}')=>'Ù',
        ('a','\u{0302}')=>'â', ('e','\u{0302}')=>'ê', ('i','\u{0302}')=>'î', ('o','\u{0302}')=>'ô', ('u','\u{0302}')=>'û', ('y','\u{0302}')=>'ŷ',
        ('A','\u{0302}')=>'Â', ('E','\u{0302}')=>'Ê', ('I','\u{0302}')=>'Î', ('O','\u{0302}')=>'Ô', ('U','\u{0302}')=>'Û', ('Y','\u{0302}')=>'Ŷ',
        ('a','\u{0303}')=>'ã', ('n','\u{0303}')=>'ñ', ('o','\u{0303}')=>'õ',
        ('A','\u{0303}')=>'Ã', ('N','\u{0303}')=>'Ñ', ('O','\u{0303}')=>'Õ',
        ('a','\u{0308}')=>'ä', ('e','\u{0308}')=>'ë', ('i','\u{0308}')=>'ï', ('o','\u{0308}')=>'ö', ('u','\u{0308}')=>'ü', ('y','\u{0308}')=>'ÿ',
        ('A','\u{0308}')=>'Ä', ('E','\u{0308}')=>'Ë', ('I','\u{0308}')=>'Ï', ('O','\u{0308}')=>'Ö', ('U','\u{0308}')=>'Ü', ('Y','\u{0308}')=>'Ÿ',
        ('c','\u{0327}')=>'ç', ('C','\u{0327}')=>'Ç',
        ('a','\u{030a}')=>'å', ('A','\u{030a}')=>'Å', ('u','\u{030a}')=>'ů', ('U','\u{030a}')=>'Ů',
        ('a','\u{0304}')=>'ā', ('e','\u{0304}')=>'ē', ('i','\u{0304}')=>'ī', ('o','\u{0304}')=>'ō', ('u','\u{0304}')=>'ū', ('y','\u{0304}')=>'ȳ',
        ('A','\u{0304}')=>'Ā', ('E','\u{0304}')=>'Ē', ('I','\u{0304}')=>'Ī', ('O','\u{0304}')=>'Ō', ('U','\u{0304}')=>'Ū',
        ('a','\u{0306}')=>'ă', ('e','\u{0306}')=>'ĕ', ('i','\u{0306}')=>'ĭ', ('o','\u{0306}')=>'ŏ', ('u','\u{0306}')=>'ŭ',
        ('A','\u{0306}')=>'Ă', ('E','\u{0306}')=>'Ĕ', ('I','\u{0306}')=>'Ĭ', ('O','\u{0306}')=>'Ŏ', ('U','\u{0306}')=>'Ŭ',
        ('c','\u{030c}')=>'č', ('d','\u{030c}')=>'ď', ('e','\u{030c}')=>'ě', ('n','\u{030c}')=>'ň', ('r','\u{030c}')=>'ř', ('s','\u{030c}')=>'š', ('t','\u{030c}')=>'ť', ('z','\u{030c}')=>'ž',
        ('C','\u{030c}')=>'Č', ('D','\u{030c}')=>'Ď', ('E','\u{030c}')=>'Ě', ('N','\u{030c}')=>'Ň', ('R','\u{030c}')=>'Ř', ('S','\u{030c}')=>'Š', ('T','\u{030c}')=>'Ť', ('Z','\u{030c}')=>'Ž',
        ('o','\u{030b}')=>'ő', ('u','\u{030b}')=>'ű', ('O','\u{030b}')=>'Ő', ('U','\u{030b}')=>'Ű',
        ('a','\u{0328}')=>'ą', ('e','\u{0328}')=>'ę', ('i','\u{0328}')=>'į', ('u','\u{0328}')=>'ų',
        ('A','\u{0328}')=>'Ą', ('E','\u{0328}')=>'Ę', ('I','\u{0328}')=>'Į', ('U','\u{0328}')=>'Ų',
        ('e','\u{0307}')=>'ė', ('z','\u{0307}')=>'ż', ('E','\u{0307}')=>'Ė', ('Z','\u{0307}')=>'Ż',
        _ => return None,
    })
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
