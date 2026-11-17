//! Grapheme-cluster boundaries: the smallest unit a caret may occupy.
//!
//! A caret is a position *between* clusters, never inside one. That is what makes `👨‍👩‍👧` or
//! `e` + U+0301 behave like one character on screen and in hit-testing, and it is one half of the
//! RFC's "cluster-honest carets" (the other half — BiDi reordering — is a later slice).
//!
//! # What this is, exactly
//!
//! A **conservative approximation** of UAX #29 extended grapheme clusters, hand-written because
//! the crate has no Unicode segmentation dependency. It merges:
//!
//! * combining marks from the common blocks (Latin, Greek, Cyrillic, Hebrew, Arabic, Devanagari,
//!   Thai, and the general combining-diacritic blocks),
//! * variation selectors (U+FE00–U+FE0F, U+E0100–U+E01EF),
//! * ZWJ sequences: a character following U+200D always joins the cluster,
//! * emoji skin-tone modifiers (U+1F3FB–U+1F3FF),
//! * regional-indicator pairs (a flag is two of them).
//!
//! And it never joins across or onto a **line break**: CR, LF and CR LF end a cluster, so a
//! combining mark or a zero-width joiner on the far side of a newline cannot swallow it. A lone CR
//! is a break of its own, which the layout honours.
//!
//! It is conservative in the direction that matters: **it never splits a mark from its base**. The
//! cost of the approximation is the other direction — a few sequences that UAX #29 would join are
//! left as separate clusters (Hangul jamo composition above all), so a caret may sit where a real
//! renderer would not allow it. That is a correctness gap, not a geometry gap: it cannot produce
//! two different answers for the same point, which is what L-8 forbids.
//!
//! Replacing this table with a real UAX-29 implementation is Block 9 work; the L-8 gate stays
//! `spec` until then, and the API here is the only place that has to change.

/// Whether `character` is a combining mark or another kind of cluster extender.
fn is_extender(character: char) -> bool {
    matches!(character,
        // Combining diacritical marks and their supplements.
        '\u{0300}'..='\u{036F}'
        | '\u{1AB0}'..='\u{1AFF}'
        | '\u{1DC0}'..='\u{1DFF}'
        | '\u{20D0}'..='\u{20F0}'
        | '\u{FE20}'..='\u{FE2F}'
        // Hebrew and Arabic vowel points.
        | '\u{0591}'..='\u{05BD}'
        | '\u{05BF}'..='\u{05C7}'
        | '\u{0610}'..='\u{061A}'
        | '\u{064B}'..='\u{065F}'
        | '\u{0670}'
        | '\u{06D6}'..='\u{06DC}'
        | '\u{06DF}'..='\u{06E8}'
        // Greek and Cyrillic.
        | '\u{0483}'..='\u{0489}'
        | '\u{1FBD}'
        | '\u{1FBF}'..='\u{1FC1}'
        // Devanagari and Thai.
        | '\u{0900}'..='\u{0903}'
        | '\u{093A}'..='\u{094F}'
        | '\u{0951}'..='\u{0957}'
        | '\u{0E31}'
        | '\u{0E34}'..='\u{0E3A}'
        | '\u{0E47}'..='\u{0E4E}'
        // Variation selectors and the zero-width joiner itself.
        | '\u{FE00}'..='\u{FE0F}'
        | '\u{E0100}'..='\u{E01EF}'
        | '\u{200D}'
        // Emoji skin-tone modifiers.
        | '\u{1F3FB}'..='\u{1F3FF}'
    )
}

/// Whether `character` is a regional indicator (two of them make a flag).
fn is_regional_indicator(character: char) -> bool {
    matches!(character, '\u{1F1E6}'..='\u{1F1FF}')
}

/// Whether `character` is a line break, and therefore ends a cluster whatever follows it.
///
/// CR, LF and CR LF are all mandatory breaks (UAX #29 §4.4, GB3/GB4), and this crate treats all
/// three as a hard break in layout. Nothing may join one: a mark glued to an LF would turn the
/// cluster into something that is no longer `"\n"`, and the line break would silently vanish —
/// the caret and the renderer would then disagree about where the line ended, which is the exact
/// class of disagreement this crate exists to make impossible.
fn is_break(character: char) -> bool {
    character == '\n' || character == '\r'
}

/// Whether `current` continues the cluster that ends with `previous`.
///
/// `regionals` is how many regional indicators the cluster already holds, which is what decides
/// whether this one completes a pair or starts a new one.
fn continues(previous: char, current: char, regionals: u32) -> bool {
    if previous == '\r' && current == '\n' {
        return true;
    }
    if is_break(previous) || is_break(current) {
        return false;
    }
    if previous == '\u{200D}' {
        return true;
    }
    if is_regional_indicator(previous) && is_regional_indicator(current) {
        return regionals % 2 == 1;
    }
    is_extender(current)
}

/// An iterator over `(byte offset, cluster text)` pairs.
#[derive(Debug, Clone)]
pub struct Clusters<'a> {
    text: &'a str,
    start: usize,
}

impl<'a> Clusters<'a> {
    /// Segments `text` from the beginning.
    #[must_use]
    pub fn new(text: &'a str) -> Self {
        Self { text, start: 0 }
    }
}

impl<'a> Iterator for Clusters<'a> {
    type Item = (usize, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.start;
        let text = self.text;
        if start >= text.len() {
            return None;
        }
        let mut end = start;
        let mut previous: Option<char> = None;
        let mut regionals = 0u32;
        for (offset, character) in text[start..].char_indices() {
            if previous.is_some_and(|before| !continues(before, character, regionals)) {
                break;
            }
            if is_regional_indicator(character) {
                regionals += 1;
            }
            previous = Some(character);
            end = start + offset + character.len_utf8();
        }
        self.start = end;
        Some((start, &text[start..end]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clusters(text: &str) -> Vec<&str> {
        Clusters::new(text).map(|(_, cluster)| cluster).collect()
    }

    fn offsets(text: &str) -> Vec<usize> {
        Clusters::new(text).map(|(offset, _)| offset).collect()
    }

    #[test]
    fn ascii_is_one_cluster_per_character() {
        assert_eq!(clusters("abc"), vec!["a", "b", "c"]);
        assert_eq!(offsets("abc"), vec![0, 1, 2]);
    }

    #[test]
    fn a_combining_mark_never_leaves_its_base() {
        // e + COMBINING ACUTE ACCENT is one cluster of three bytes.
        assert_eq!(clusters("e\u{301}x"), vec!["e\u{301}", "x"]);
        assert_eq!(offsets("e\u{301}x"), vec![0, 3]);
    }

    #[test]
    fn variation_selectors_join_their_emoji() {
        // U+2764 U+FE0F is a heart with an emoji presentation selector.
        let text = "\u{2764}\u{FE0F}!";
        assert_eq!(clusters(text), vec!["\u{2764}\u{FE0F}", "!"]);
    }

    #[test]
    fn a_zwj_sequence_is_one_cluster() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(clusters(family), vec![family]);
        assert_eq!(clusters(&format!("{family}x")), vec![family, "x"]);
    }

    #[test]
    fn a_flag_is_two_regional_indicators() {
        let flag = "\u{1F1F3}\u{1F1F5}";
        assert_eq!(clusters(flag), vec![flag]);
        // A third indicator starts a new cluster instead of joining the finished flag.
        assert_eq!(
            clusters(&format!("{flag}\u{1F1E6}")),
            vec![flag, "\u{1F1E6}"]
        );
    }

    #[test]
    fn a_skin_tone_modifier_joins_and_crlf_is_one_break() {
        assert_eq!(clusters("\u{1F44D}\u{1F3FD}"), vec!["\u{1F44D}\u{1F3FD}"]);
        assert_eq!(clusters("a\r\nb"), vec!["a", "\r\n", "b"]);
        assert_eq!(clusters("a\nb"), vec!["a", "\n", "b"]);
    }

    #[test]
    fn a_break_is_never_joined_by_what_follows_it() {
        // A combining mark after an LF must not become part of the break's cluster: if it did, the
        // cluster would no longer be "\n" and the line break would disappear.
        assert_eq!(clusters("a\n\u{301}b"), vec!["a", "\n", "\u{301}", "b"]);
        // Nor may a joining emoji sequence reach past a newline.
        assert_eq!(clusters("a\u{200D}\nb"), vec!["a\u{200D}", "\n", "b"]);
        // A mark before a break does not drag the break into the cluster either.
        assert_eq!(clusters("e\u{301}\nx"), vec!["e\u{301}", "\n", "x"]);
        // CR LF is still one break; a lone CR is a break by itself.
        assert_eq!(clusters("a\r\nb"), vec!["a", "\r\n", "b"]);
        assert_eq!(clusters("a\rb"), vec!["a", "\r", "b"]);
        assert_eq!(clusters("a\r\u{301}b"), vec!["a", "\r", "\u{301}", "b"]);
    }

    #[test]
    fn every_offset_is_a_character_boundary() {
        let text = "a\u{301}b\u{1F600}\u{1F3FD}c";
        for (offset, cluster) in Clusters::new(text) {
            assert!(text.is_char_boundary(offset));
            assert!(text[offset..].starts_with(cluster));
        }
    }
}
