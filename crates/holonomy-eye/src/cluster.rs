//! Grapheme-cluster boundaries: the smallest unit a caret may occupy.
//!
//! A caret is a position *between* clusters, never inside one. That is what makes `👨‍👩‍👧` or
//! `e` + U+0301 behave like one character on screen in hit-testing, and it is one half of the
//! RFC's "cluster-honest carets" (the other half — BiDi reordering — is a later slice).
//!
//! # The rules, not just a table
//!
//! This is a hand-written implementation of UAX #29's extended grapheme clusters, because the crate
//! has no Unicode dependency and must not grow one for this. The structure follows the standard:
//! every code point gets a [`Property`], and [`joins`] applies the rules **in their numbered
//! order**, so each rule's comment can be checked against the specification line by line.
//!
//! | rule | what it says |
//! | ---- | ------------ |
//! | GB3 | CR × LF |
//! | GB4, GB5 | break on both sides of a Control, CR or LF |
//! | GB6–GB8 | the Hangul syllable sequences: L×, V×, T× |
//! | GB9 | × Extend, × ZWJ |
//! | GB9a | × SpacingMark |
//! | GB9b | Prepend × |
//! | GB11 | Pictographic Extend\* ZWJ × Pictographic |
//! | GB12, GB13 | regional indicators pair up, counting from the cluster's start |
//!
//! # The bug this replaced
//!
//! The previous implementation joined **anything** after a ZWJ. That is GB9 (× ZWJ) read as if it
//! were GB11, and it is wrong in the direction that makes a caret *refuse* a position a renderer
//! allows: `a` + ZWJ + `b` is two clusters in UAX #29 (the left context of a joining ZWJ must be
//! Extended_Pictographic), and this said one. The rule is now the standard's, and
//! `a_zwj_joins_only_between_pictographs` pins it.
//!
//! # Coverage, stated exactly
//!
//! The property tables cover the scripts `docs/UI-SPEC.md` names — Latin, Greek, Cyrillic, Hebrew,
//! Arabic, Devanagari, Thai, Hangul, Han, and emoji — plus the general combining-diacritic blocks,
//! variation selectors, tag characters, ZWJ and ZWNJ. They are **not** the whole Unicode database:
//! a combining mark from an unlisted script is classified `Other` and splits from its base, and
//! Extended_Pictographic is approximated by the emoji blocks rather than by the standard's full
//! sparse list, which can only split a pictograph sequence that a renderer would join.
//!
//! Both of those are *coverage* gaps and are stated as such (AUDIT OQ-11), not hidden behind a
//! claim of completeness. What the crate does own is that segmentation is a pure function of the
//! text **and of the state carried into it**: the same point answers the same way however the text
//! was split into windows, which is what L-8 requires.
//!
//! # A window is a piece, and a piece can begin inside a cluster
//!
//! A leaf cut is a byte count, so a window can open on a combining mark whose base is in the window
//! before. [`ClusterContext`] is the two words UAX #29 needs from the text before a piece — the
//! property of its last character, the GB11 run and the regional-indicator count — so a piece
//! handed it segments exactly as the whole document does; [`Clusters::carry`] at the end of one
//! piece is [`Clusters::continuing`] at the start of the next. Without it, a boundary the document
//! does not have became a caret position, and the caret a backspace deleted by stopped at the leaf
//! edge (AUDIT OQ-17).
//!
//! BiDi reordering is **not** here — the other half of the RFC's caret claim — and it changes paint
//! order rather than segmentation, so it belongs beside the run tree.

/// The grapheme-cluster and word properties UAX #29 needs, for the scripts this crate covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Property {
    /// Carriage return: GB3 pairs it with a following LF, GB4 breaks after it.
    Cr,
    /// Line feed.
    Lf,
    /// A C0/C1 control, a bidi control, or a line/paragraph separator: breaks on both sides.
    Control,
    /// A non-spacing mark, an enclosing mark, or anything else GB9 attaches to its base.
    Extend,
    /// Zero-width joiner: an Extend for GB9, and the pivot of GB11.
    Zwj,
    /// A regional indicator: two of them make a flag (GB12/GB13).
    RegionalIndicator,
    /// A prepend character: GB9b attaches it to whatever follows.
    Prepend,
    /// A spacing mark (Indic vowel signs and the like): GB9a attaches it to its base.
    SpacingMark,
    /// Hangul choseong (leading consonant).
    HangulL,
    /// Hangul jungseong (vowel).
    HangulV,
    /// Hangul jongseong (trailing consonant).
    HangulT,
    /// A precomposed Hangul syllable with no trailing consonant.
    HangulLv,
    /// A precomposed Hangul syllable with a trailing consonant.
    HangulLvt,
    /// Extended_Pictographic: the left and right context of GB11.
    Pictographic,
    /// Everything else, which joins nothing by default.
    Other,
}

impl Property {
    /// Whether GB4 and GB5 break on both sides of this property.
    const fn is_break(self) -> bool {
        matches!(self, Self::Cr | Self::Lf | Self::Control)
    }
}

/// The property of `character`, for the scripts this crate covers.
fn property(character: char) -> Property {
    if character == '\r' {
        return Property::Cr;
    }
    if character == '\n' {
        return Property::Lf;
    }
    if character == '\u{200D}' {
        return Property::Zwj;
    }
    if is_control(character) {
        return Property::Control;
    }
    if is_regional_indicator(character) {
        return Property::RegionalIndicator;
    }
    let hangul = is_hangul(character);
    if hangul != Property::Other {
        return hangul;
    }
    if is_prepend(character) {
        return Property::Prepend;
    }
    if is_extend(character) {
        return Property::Extend;
    }
    if is_spacing_mark(character) {
        return Property::SpacingMark;
    }
    if is_pictographic(character) {
        return Property::Pictographic;
    }
    Property::Other
}

/// C0 and C1 controls, the bidi controls that are not Extend, and the line/paragraph separators.
///
/// UAX #29's Control class is `Cc`, `Cf` (minus the ones the standard lists as Extend or Prepend),
/// `Zl` and `Zp`. Classifying an unlisted `Cf` as Control splits it from its neighbours, which is
/// the safe direction, and CR/LF/ZWJ are decided before this is asked.
fn is_control(character: char) -> bool {
    matches!(character,
        '\u{0000}'..='\u{001F}'
        | '\u{007F}'..='\u{009F}'
        | '\u{200E}'..='\u{200F}'
        | '\u{2028}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206F}'
        | '\u{FEFF}'
    )
}

/// A regional indicator: two of them are a flag.
fn is_regional_indicator(character: char) -> bool {
    matches!(character, '\u{1F1E6}'..='\u{1F1FF}')
}

/// The Hangul properties: the conjoining jamo blocks (GB6–GB8) and the syllable arithmetic.
fn is_hangul(character: char) -> Property {
    match character {
        '\u{1100}'..='\u{115F}' | '\u{A960}'..='\u{A97C}' => Property::HangulL,
        '\u{1160}'..='\u{11A7}' | '\u{D7B0}'..='\u{D7C6}' => Property::HangulV,
        '\u{11A8}'..='\u{11FF}' | '\u{D7CB}'..='\u{D7FB}' => Property::HangulT,
        '\u{AC00}'..='\u{D7A3}' => {
            // A precomposed syllable: LV when the trailing-consonant index is zero, LVT otherwise.
            let index = u32::from(character) - 0xAC00;
            if index % 28 == 0 {
                Property::HangulLv
            } else {
                Property::HangulLvt
            }
        }
        _ => Property::Other,
    }
}

/// Prepend characters: Arabic number signs, the Indic and historic signs the standard lists, and
/// the interlinear annotation anchors. GB9b attaches each to whatever follows it.
fn is_prepend(character: char) -> bool {
    matches!(character,
        '\u{0600}'..='\u{0605}'
        | '\u{06DD}'
        | '\u{070F}'
        | '\u{0890}'..='\u{0891}'
        | '\u{08E2}'
        | '\u{0D4E}'
        | '\u{110BD}'
        | '\u{110CD}'
        | '\u{111C2}'..='\u{111C3}'
        | '\u{1193F}'
        | '\u{11941}'
        | '\u{11A3A}'
        | '\u{11D46}'
        | '\u{11F02}'
    )
}

/// Non-spacing and enclosing marks, and the other characters GB9 attaches to what precedes them.
///
/// Grouped by script so a reader can check the ranges against the code charts: the general
/// diacritic blocks first, then Latin/Greek/Cyrillic, Hebrew, Arabic, the Indic scripts this UI
/// claims (Devanagari first), Thai and Lao, Tibetan, the historic scripts, the emoji modifiers, and
/// finally the format characters the standard treats as Extend (`ZWNJ`, variation selectors, tags).
fn is_extend(character: char) -> bool {
    matches!(character,
        // General diacritics, and the combining half marks.
        '\u{0300}'..='\u{036F}'
        | '\u{0483}'..='\u{0489}'
        | '\u{1AB0}'..='\u{1AFF}'
        | '\u{1DC0}'..='\u{1DFF}'
        | '\u{20D0}'..='\u{20F0}'
        | '\u{FE20}'..='\u{FE2F}'
        // Greek and Cyrillic.
        | '\u{1FBD}'
        | '\u{1FBF}'..='\u{1FC1}'
        // Hebrew.
        | '\u{0591}'..='\u{05BD}'
        | '\u{05BF}'
        | '\u{05C1}'..='\u{05C2}'
        | '\u{05C4}'..='\u{05C5}'
        | '\u{05C7}'
        // Arabic and Syriac.
        | '\u{0610}'..='\u{061A}'
        | '\u{064B}'..='\u{065F}'
        | '\u{0670}'
        | '\u{06D6}'..='\u{06DC}'
        | '\u{06DF}'..='\u{06E4}'
        | '\u{06E7}'..='\u{06E8}'
        | '\u{06EA}'..='\u{06ED}'
        | '\u{0711}'
        | '\u{0730}'..='\u{074A}'
        | '\u{07A6}'..='\u{07B0}'
        | '\u{07EB}'..='\u{07F3}'
        | '\u{0816}'..='\u{0819}'
        | '\u{081B}'..='\u{0823}'
        | '\u{0825}'..='\u{0827}'
        | '\u{0829}'..='\u{082D}'
        | '\u{0859}'..='\u{085B}'
        | '\u{08D3}'..='\u{08E1}'
        | '\u{08E3}'..='\u{0902}'
        // Devanagari: the marks that take no space, and the virama.
        | '\u{093A}'
        | '\u{093C}'
        | '\u{0941}'..='\u{0948}'
        | '\u{094D}'
        | '\u{0951}'..='\u{0957}'
        | '\u{0962}'..='\u{0963}'
        // Bengali, Gurmukhi, Gujarati, Oriya, Tamil, Telugu, Kannada, Malayalam: the non-spacing
        // marks of the scripts a Bengali or Dravidian user is likely to type.
        | '\u{0981}'
        | '\u{09BC}'
        | '\u{09C1}'..='\u{09C4}'
        | '\u{09CD}'
        | '\u{09E2}'..='\u{09E3}'
        | '\u{0A01}'..='\u{0A02}'
        | '\u{0A3C}'
        | '\u{0A41}'..='\u{0A42}'
        | '\u{0A47}'..='\u{0A48}'
        | '\u{0A4B}'..='\u{0A4D}'
        | '\u{0A51}'
        | '\u{0A70}'..='\u{0A71}'
        | '\u{0A75}'
        | '\u{0A81}'..='\u{0A82}'
        | '\u{0ABC}'
        | '\u{0AC1}'..='\u{0AC5}'
        | '\u{0AC7}'..='\u{0AC8}'
        | '\u{0ACD}'
        | '\u{0AE2}'..='\u{0AE3}'
        | '\u{0B01}'
        | '\u{0B3C}'
        | '\u{0B3F}'
        | '\u{0B41}'..='\u{0B44}'
        | '\u{0B4D}'
        | '\u{0B55}'..='\u{0B56}'
        | '\u{0B62}'..='\u{0B63}'
        | '\u{0B82}'
        | '\u{0BC0}'
        | '\u{0BCD}'
        | '\u{0C00}'
        | '\u{0C04}'
        | '\u{0C3E}'..='\u{0C40}'
        | '\u{0C46}'..='\u{0C48}'
        | '\u{0C4A}'..='\u{0C4D}'
        | '\u{0C55}'..='\u{0C56}'
        | '\u{0C62}'..='\u{0C63}'
        | '\u{0C81}'
        | '\u{0CBC}'
        | '\u{0CBF}'
        | '\u{0CC6}'
        | '\u{0CCC}'..='\u{0CCD}'
        | '\u{0CE2}'..='\u{0CE3}'
        | '\u{0D00}'..='\u{0D01}'
        | '\u{0D3B}'..='\u{0D3C}'
        | '\u{0D41}'..='\u{0D44}'
        | '\u{0D4D}'
        | '\u{0D62}'..='\u{0D63}'
        | '\u{0DCA}'
        | '\u{0DD2}'..='\u{0DD4}'
        | '\u{0DD6}'
        // Thai and Lao.
        | '\u{0E31}'
        | '\u{0E34}'..='\u{0E3A}'
        | '\u{0E47}'..='\u{0E4E}'
        | '\u{0EB1}'
        | '\u{0EB4}'..='\u{0EBC}'
        | '\u{0EC8}'..='\u{0ECD}'
        // Tibetan.
        | '\u{0F18}'..='\u{0F19}'
        | '\u{0F35}'
        | '\u{0F37}'
        | '\u{0F39}'
        | '\u{0F71}'..='\u{0F7E}'
        | '\u{0F80}'..='\u{0F84}'
        | '\u{0F86}'..='\u{0F87}'
        | '\u{0F8D}'..='\u{0F97}'
        | '\u{0F99}'..='\u{0FBC}'
        | '\u{0FC6}'
        // Myanmar, Khmer, Mongolian and the historic scripts' vowel signs.
        | '\u{102D}'..='\u{1030}'
        | '\u{1032}'..='\u{1037}'
        | '\u{1039}'..='\u{103A}'
        | '\u{103D}'..='\u{103E}'
        | '\u{1058}'..='\u{1059}'
        | '\u{105E}'..='\u{1060}'
        | '\u{1071}'..='\u{1074}'
        | '\u{135D}'..='\u{135F}'
        | '\u{1712}'..='\u{1714}'
        | '\u{1732}'..='\u{1734}'
        | '\u{1752}'..='\u{1753}'
        | '\u{1772}'..='\u{1773}'
        | '\u{17B4}'..='\u{17B5}'
        | '\u{17B7}'..='\u{17BD}'
        | '\u{17C6}'
        | '\u{17C9}'..='\u{17D3}'
        | '\u{17DD}'
        | '\u{180B}'..='\u{180D}'
        | '\u{1885}'..='\u{1886}'
        | '\u{18A9}'
        | '\u{1920}'..='\u{1922}'
        | '\u{1927}'..='\u{1928}'
        | '\u{1932}'
        | '\u{1939}'..='\u{193B}'
        | '\u{1A17}'..='\u{1A18}'
        | '\u{1A1B}'
        | '\u{1A56}'
        | '\u{1A58}'..='\u{1A5E}'
        | '\u{1A60}'
        | '\u{1A62}'
        | '\u{1A65}'..='\u{1A6C}'
        | '\u{1A73}'..='\u{1A7C}'
        | '\u{1A7F}'
        | '\u{1B00}'..='\u{1B03}'
        | '\u{1B34}'
        | '\u{1B36}'..='\u{1B3A}'
        | '\u{1B3C}'
        | '\u{1B42}'
        | '\u{1B6B}'..='\u{1B73}'
        | '\u{1B80}'..='\u{1B81}'
        | '\u{1BA2}'..='\u{1BA5}'
        | '\u{1BA8}'..='\u{1BA9}'
        | '\u{1BAB}'..='\u{1BAD}'
        | '\u{1BE6}'
        | '\u{1BE8}'..='\u{1BE9}'
        | '\u{1BED}'
        | '\u{1BEF}'..='\u{1BF1}'
        | '\u{1C2C}'..='\u{1C33}'
        | '\u{1C36}'..='\u{1C37}'
        | '\u{1CD0}'..='\u{1CD2}'
        | '\u{1CD4}'..='\u{1CE0}'
        | '\u{1CE2}'..='\u{1CE8}'
        | '\u{1CED}'
        | '\u{1CF4}'
        | '\u{1CF8}'..='\u{1CF9}'
        | '\u{2D7F}'
        | '\u{2DE0}'..='\u{2DFF}'
        | '\u{302A}'..='\u{302F}'
        | '\u{3099}'..='\u{309A}'
        // Arabic presentation forms and the historic Arabic-script supplements.
        | '\u{A66F}'..='\u{A672}'
        | '\u{A674}'..='\u{A67D}'
        | '\u{A69E}'..='\u{A69F}'
        | '\u{A6F0}'..='\u{A6F1}'
        | '\u{A802}'
        | '\u{A806}'
        | '\u{A80B}'
        | '\u{A825}'..='\u{A826}'
        | '\u{A8C4}'..='\u{A8C5}'
        | '\u{A8E0}'..='\u{A8F1}'
        | '\u{A926}'..='\u{A92D}'
        | '\u{A947}'..='\u{A951}'
        | '\u{A980}'..='\u{A982}'
        | '\u{A9B3}'
        | '\u{A9B6}'..='\u{A9B9}'
        | '\u{A9BC}'..='\u{A9BD}'
        | '\u{A9E5}'
        | '\u{AA29}'..='\u{AA2E}'
        | '\u{AA31}'..='\u{AA32}'
        | '\u{AA35}'..='\u{AA36}'
        | '\u{AA43}'
        | '\u{AA4C}'
        | '\u{AA7C}'
        | '\u{AAB0}'
        | '\u{AAB2}'..='\u{AAB4}'
        | '\u{AAB7}'..='\u{AAB8}'
        | '\u{AABE}'..='\u{AABF}'
        | '\u{AAC1}'
        | '\u{AAEC}'..='\u{AAED}'
        | '\u{AAF6}'
        | '\u{ABE5}'
        | '\u{ABE8}'
        | '\u{ABED}'
        | '\u{FB1E}'
        // Format characters the standard classifies as Extend: ZWNJ, variation selectors and tags.
        | '\u{200C}'
        | '\u{FE00}'..='\u{FE0F}'
        | '\u{E0020}'..='\u{E007F}'
        | '\u{E0100}'..='\u{E01EF}'
        // Emoji skin-tone modifiers.
        | '\u{1F3FB}'..='\u{1F3FF}'
    )
}

/// Spacing marks (Unicode `Mc`): they take space, and GB9a still attaches them to their base.
///
/// Only the scripts this UI claims are listed; an unlisted spacing mark splits from its base, which
/// is a coverage gap of OQ-11 and not a rule error.
fn is_spacing_mark(character: char) -> bool {
    matches!(character,
        // Devanagari, the script this UI's own text is most likely to mix in.
        '\u{0903}'
        | '\u{093B}'
        | '\u{093E}'..='\u{0940}'
        | '\u{0949}'..='\u{094C}'
        | '\u{094E}'..='\u{094F}'
        // Bengali, Tamil, Telugu, Kannada, Malayalam: the vowel signs that take space.
        | '\u{0982}'..='\u{0983}'
        | '\u{09BE}'..='\u{09C0}'
        | '\u{09C7}'..='\u{09C8}'
        | '\u{09CB}'..='\u{09CC}'
        | '\u{09D7}'
        | '\u{0BBE}'..='\u{0BBF}'
        | '\u{0BC1}'..='\u{0BC2}'
        | '\u{0BC6}'..='\u{0BC8}'
        | '\u{0BCA}'..='\u{0BCC}'
        | '\u{0BD7}'
        | '\u{0C01}'..='\u{0C03}'
        | '\u{0C41}'..='\u{0C44}'
        | '\u{0C82}'..='\u{0C83}'
        | '\u{0CBE}'
        | '\u{0CC0}'..='\u{0CC4}'
        | '\u{0CC7}'..='\u{0CC8}'
        | '\u{0CCA}'..='\u{0CCB}'
        | '\u{0CD5}'..='\u{0CD6}'
        | '\u{0D02}'..='\u{0D03}'
        | '\u{0D3E}'..='\u{0D40}'
        | '\u{0D46}'..='\u{0D48}'
        | '\u{0D4A}'..='\u{0D4C}'
        | '\u{0D57}'
        | '\u{0D82}'..='\u{0D83}'
        | '\u{0DCF}'..='\u{0DD1}'
        | '\u{0DD8}'..='\u{0DDF}'
        | '\u{0DF2}'..='\u{0DF3}'
    )
}

/// Extended_Pictographic, approximated by the emoji blocks plus the BMP symbols that carry emoji
/// presentation. GB11 needs this on **both** sides of a joining ZWJ.
///
/// The standard's list also contains sparse non-emoji code points (U+1F10D…, U+1FC00…), which are
/// left out on purpose: leaving them out splits a sequence a renderer would join, while adding
/// wrong ones would join sequences it would not.
fn is_pictographic(character: char) -> bool {
    matches!(character,
        '\u{00A9}'
        | '\u{00AE}'
        | '\u{203C}'
        | '\u{2049}'
        | '\u{2122}'
        | '\u{2139}'
        | '\u{2194}'..='\u{2199}'
        | '\u{21A9}'..='\u{21AA}'
        | '\u{231A}'..='\u{231B}'
        | '\u{2328}'
        | '\u{23CF}'
        | '\u{23E9}'..='\u{23F3}'
        | '\u{23F8}'..='\u{23FA}'
        | '\u{24C2}'
        | '\u{25AA}'..='\u{25AB}'
        | '\u{25B6}'
        | '\u{25C0}'
        | '\u{25FB}'..='\u{25FE}'
        | '\u{2600}'..='\u{2604}'
        | '\u{260E}'
        | '\u{2611}'
        | '\u{2614}'..='\u{2615}'
        | '\u{2618}'
        | '\u{261D}'
        | '\u{2620}'
        | '\u{2622}'..='\u{2623}'
        | '\u{2626}'
        | '\u{262A}'
        | '\u{262E}'..='\u{262F}'
        | '\u{2638}'..='\u{263A}'
        | '\u{2640}'
        | '\u{2642}'
        | '\u{2648}'..='\u{2653}'
        | '\u{265F}'..='\u{2660}'
        | '\u{2663}'
        | '\u{2665}'..='\u{2666}'
        | '\u{2668}'
        | '\u{267B}'
        | '\u{267E}'..='\u{267F}'
        | '\u{2692}'..='\u{2697}'
        | '\u{2699}'
        | '\u{269B}'..='\u{269C}'
        | '\u{26A0}'..='\u{26A1}'
        | '\u{26A7}'
        | '\u{26AA}'..='\u{26AB}'
        | '\u{26B0}'..='\u{26B1}'
        | '\u{26BD}'..='\u{26BE}'
        | '\u{26C4}'..='\u{26C5}'
        | '\u{26C8}'
        | '\u{26CE}'..='\u{26CF}'
        | '\u{26D1}'
        | '\u{26D3}'..='\u{26D4}'
        | '\u{26E9}'..='\u{26EA}'
        | '\u{26F0}'..='\u{26F5}'
        | '\u{26F7}'..='\u{26FA}'
        | '\u{26FD}'
        | '\u{2702}'
        | '\u{2705}'
        | '\u{2708}'..='\u{270D}'
        | '\u{270F}'
        | '\u{2712}'
        | '\u{2714}'
        | '\u{2716}'
        | '\u{271D}'
        | '\u{2721}'
        | '\u{2728}'
        | '\u{2733}'..='\u{2734}'
        | '\u{2744}'
        | '\u{2747}'
        | '\u{274C}'
        | '\u{274E}'
        | '\u{2753}'..='\u{2755}'
        | '\u{2757}'
        | '\u{2763}'..='\u{2764}'
        | '\u{2795}'..='\u{2797}'
        | '\u{27A1}'
        | '\u{27B0}'
        | '\u{27BF}'
        | '\u{2934}'..='\u{2935}'
        | '\u{2B05}'..='\u{2B07}'
        | '\u{2B1B}'..='\u{2B1C}'
        | '\u{2B50}'
        | '\u{2B55}'
        | '\u{3030}'
        | '\u{303D}'
        | '\u{3297}'
        | '\u{3299}'
        // The emoji blocks themselves.
        | '\u{1F004}'
        | '\u{1F0CF}'
        | '\u{1F170}'..='\u{1F171}'
        | '\u{1F17E}'..='\u{1F17F}'
        | '\u{1F18E}'
        | '\u{1F191}'..='\u{1F19A}'
        | '\u{1F1E6}'..='\u{1F1FF}'
        | '\u{1F201}'..='\u{1F202}'
        | '\u{1F21A}'
        | '\u{1F22F}'
        | '\u{1F232}'..='\u{1F23A}'
        | '\u{1F250}'..='\u{1F251}'
        | '\u{1F300}'..='\u{1F321}'
        | '\u{1F324}'..='\u{1F393}'
        | '\u{1F396}'..='\u{1F397}'
        | '\u{1F399}'..='\u{1F39B}'
        | '\u{1F39E}'..='\u{1F3F0}'
        | '\u{1F3F3}'..='\u{1F3F5}'
        | '\u{1F3F7}'..='\u{1F3FA}'
        | '\u{1F400}'..='\u{1F4FD}'
        | '\u{1F4FF}'..='\u{1F53D}'
        | '\u{1F546}'..='\u{1F64F}'
        | '\u{1F680}'..='\u{1F6FF}'
        | '\u{1F7E0}'..='\u{1F7EB}'
        | '\u{1F90C}'..='\u{1F93A}'
        | '\u{1F93C}'..='\u{1F945}'
        | '\u{1F947}'..='\u{1F9FF}'
        | '\u{1FA70}'..='\u{1FAFF}'
    )
}

/// What GB11 needs to know about the characters already in the cluster.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Context {
    /// The cluster so far — up to but not including a ZWJ just absorbed — is
    /// `Pictographic Extend*`, which is GB11's left context.
    pictographic: bool,
    /// The character last absorbed was a ZWJ, and the run before it was `Pictographic Extend*`.
    ///
    /// GB11's text is `Pictographic Extend* ZWJ`, so the ZWJ has to remember the run behind it —
    /// and it has to *end* that run. A second ZWJ's left context is a ZWJ, which is not Extend, so
    /// `👨‍‍👩` is two clusters; keeping `pictographic` set across the ZWJ made it one.
    joins_after_zwj: bool,
    /// Regional indicators in the cluster so far, for GB12/GB13's odd/even rule.
    regionals: u32,
}

impl Context {
    /// Folds one more character into the context.
    fn absorb(&mut self, property: Property) {
        // Decided before the run is updated: a ZWJ's left context is the run *behind* it.
        self.joins_after_zwj = property == Property::Zwj && self.pictographic;
        self.pictographic = match property {
            Property::Pictographic => true,
            Property::Extend => self.pictographic,
            _ => false,
        };
        self.regionals = if property == Property::RegionalIndicator {
            self.regionals + 1
        } else {
            0
        };
    }
}

/// Whether `current` continues the cluster that ends with `previous`, by UAX #29's rules in order.
fn joins(previous: Property, current: Property, context: Context) -> bool {
    use Property::{Cr, Extend, HangulL, HangulLv, HangulLvt, HangulT, HangulV};
    use Property::{Lf, Pictographic, Prepend, RegionalIndicator, SpacingMark, Zwj};

    // GB3: CR × LF.
    if previous == Cr && current == Lf {
        return true;
    }
    // GB4 and GB5: a Control, CR or LF breaks on both sides. This is also what forbids a mark or a
    // joiner from reaching across a line break, which the layout depends on.
    if previous.is_break() || current.is_break() {
        return false;
    }
    // GB6: L × (L | V | LV | LVT).
    // GB7: (LV | V) × (V | T).
    // GB8: (LVT | T) × T.
    match (previous, current) {
        (HangulL, HangulL | HangulV | HangulLv | HangulLvt)
        | (HangulLv | HangulV, HangulV | HangulT)
        | (HangulLvt | HangulT, HangulT) => return true,
        _ => {}
    }
    // GB9: × (Extend | ZWJ).
    if matches!(current, Extend | Zwj) {
        return true;
    }
    // GB9a: × SpacingMark.
    if current == SpacingMark {
        return true;
    }
    // GB9b: Prepend ×.
    if previous == Prepend {
        return true;
    }
    // GB11: Pictographic Extend* ZWJ × Pictographic.
    if previous == Zwj && context.joins_after_zwj && current == Pictographic {
        return true;
    }
    // GB12 and GB13: an even number of regional indicators in the cluster means the next one starts
    // a new flag.
    if previous == RegionalIndicator && current == RegionalIndicator {
        return context.regionals % 2 == 1;
    }
    false
}

/// An iterator over `(byte offset, cluster text)` pairs.
/// The segmentation state at a cluster boundary: what UAX #29 needs from the text before it.
///
/// A leaf cut is a byte count, so a piece of a document can begin **inside** a cluster — a mark or
/// a joiner at the first byte of a window, with its base at the last byte of the one before.
/// Segmenting that piece from its own first byte would make a boundary a caret position the
/// document does not have (`a` + ZWJ + `b`'s tail, or half a flag), which is AUDIT OQ-17. So the
/// state travels: [`Clusters::carry`] at the end of one piece is [`Clusters::continuing`] at the
/// start of the next, and it is two small words rather than text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClusterContext {
    /// The property of the last character consumed, which is what GB3–GB13's left context reads.
    previous: Option<Property>,
    /// The GB11 run and the GB12/GB13 regional-indicator count *within* the current cluster.
    context: Context,
}

impl ClusterContext {
    /// The state before the document's first byte: nothing consumed, no cluster open.
    pub const START: Self = Self {
        previous: None,
        context: Context {
            pictographic: false,
            joins_after_zwj: false,
            regionals: 0,
        },
    };

    /// Whether `character` continues the cluster that ends at this boundary.
    ///
    /// A caller that wants to know if a piece begins inside a cluster asks this about its first
    /// character, which is exactly what the segmenter asks itself.
    #[must_use]
    pub fn joins(self, character: char) -> bool {
        self.previous
            .is_some_and(|before| joins(before, property(character), self.context))
    }
}

#[derive(Debug, Clone)]
pub struct Clusters<'a> {
    text: &'a str,
    start: usize,
    /// The state at `start`: what the text before this piece said, or the end of the last cluster
    /// this iterator emitted.
    carried: ClusterContext,
}

impl<'a> Clusters<'a> {
    /// Segments `text` from the beginning.
    #[must_use]
    pub fn new(text: &'a str) -> Self {
        Self::continuing(text, ClusterContext::START)
    }

    /// Segments `text` as the continuation of a document that ends at `context`.
    ///
    /// The first cluster of the piece may then begin *before* the piece does, in which case its
    /// boundary inside the piece is not a caret position — the segmenter simply emits it starting
    /// at byte zero, and [`crate::Layout`] keeps it out of the line's boundaries.
    #[must_use]
    pub fn continuing(text: &'a str, context: ClusterContext) -> Self {
        Self {
            text,
            start: 0,
            carried: context,
        }
    }

    /// The state at the end of the text consumed so far — what the next piece needs.
    #[must_use]
    pub fn carry(&self) -> ClusterContext {
        self.carried
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
        let rest = &text[start..];
        let mut end = start;
        let mut previous = self.carried.previous;
        let mut context = self.carried.context;
        // The piece's first character decides whether this cluster is a continuation. If it does
        // not join, the cluster ended before the piece began and this one opens a new cluster —
        // decided once, here, so the loop below can start from a clean state.
        let breaks = rest.chars().next().is_some_and(|first| {
            previous.is_some_and(|before| !joins(before, property(first), context))
        });
        if breaks {
            previous = None;
            context = Context::default();
        }
        for (offset, character) in rest.char_indices() {
            let class = property(character);
            if previous.is_some_and(|before| !joins(before, class, context)) {
                break;
            }
            context.absorb(class);
            previous = Some(class);
            end = start + offset + character.len_utf8();
        }
        self.start = end;
        self.carried = ClusterContext { previous, context };
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
    fn a_zwj_joins_only_between_pictographs() {
        // GB11 needs Extended_Pictographic on both sides of the joiner. The old implementation
        // joined anything after a ZWJ, which refused a caret position a renderer allows.
        assert_eq!(clusters("a\u{200D}b"), vec!["a\u{200D}", "b"]);
        // A pictograph on the left is enough for the rule, with Extend* in between.
        let with_mark = "\u{1F469}\u{1F3FD}\u{200D}\u{1F4BB}";
        assert_eq!(clusters(with_mark), vec![with_mark]);
        // And a pictograph on the right alone is not: `a` is not the rule's left context.
        assert_eq!(
            clusters("\u{1F600}\u{200D}a"),
            vec!["\u{1F600}\u{200D}", "a"]
        );
    }

    #[test]
    fn a_second_zwj_ends_the_pictographic_run() {
        // GB11's text is `Pictographic Extend* ZWJ` — one joiner at the end of the run. Two
        // joiners put a ZWJ where the Extend* has to be, so the sequence cannot reach across the
        // second one: `👨‍‍👩` is the man-and-joiner cluster and then the woman. The first
        // implementation kept the pictographic flag alive across every ZWJ, which made it one.
        assert_eq!(
            clusters("\u{1F468}\u{200D}\u{200D}\u{1F469}"),
            vec!["\u{1F468}\u{200D}\u{200D}", "\u{1F469}"]
        );
        // One joiner, with Extend* behind it, still joins — the toned sequence is untouched.
        let toned = "\u{1F468}\u{1F3FD}\u{200D}\u{1F469}";
        assert_eq!(clusters(toned), vec![toned]);
        // And a joined pictograph restarts the run, so a chain of them is one cluster: the
        // standard's own family sequence, which the first implementation also got wrong.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(clusters(family), vec![family]);
    }

    #[test]
    fn a_piece_that_continues_a_cluster_segments_as_the_whole_text_does() {
        // A leaf cut is a byte count, so a piece can begin inside a cluster. Handed the state the
        // text before it ended in, the piece's first cluster ends where the document's does — at
        // every character boundary of a text that has a mark, a joiner chain and a flag in it.
        let text = "e\u{301}x\u{1F468}\u{200D}\u{1F469}\u{1F1E6}\u{1F1E7}\u{1F1E8}";
        let whole: Vec<(usize, usize)> = Clusters::new(text)
            .map(|(at, cluster)| (at, at + cluster.len()))
            .collect();
        for cut in 1..text.len() {
            if !text.is_char_boundary(cut) {
                continue;
            }
            let mut prefix = Clusters::new(&text[..cut]);
            while prefix.next().is_some() {}
            let context = prefix.carry();
            let (_, next) = Clusters::continuing(&text[cut..], context)
                .next()
                .expect("a cluster after the cut");
            let expected = whole
                .iter()
                .find(|(at, end)| *at <= cut && cut < *end)
                .map(|(_, end)| *end);
            assert_eq!(
                Some(cut + next.len()),
                expected,
                "a cut at {cut} ends where the document's cluster does"
            );
        }
    }

    #[test]
    fn hangul_jamo_compose_by_the_syllable_rules() {
        // L + V + T is one cluster (GB6, GB7, GB8), and so is a precomposed LV plus a trailing T.
        let conjoining = "\u{1100}\u{1161}\u{11A8}";
        assert_eq!(clusters(conjoining), vec![conjoining]);
        let precomposed = "\u{AC00}\u{11A8}";
        assert_eq!(clusters(precomposed), vec![precomposed]);
        // L + L joins (GB6), and a bare V after an LV joins (GB7), but T + V does not.
        assert_eq!(clusters("\u{1100}\u{1100}"), vec!["\u{1100}\u{1100}"]);
        assert_eq!(clusters("\u{11A8}\u{1161}"), vec!["\u{11A8}", "\u{1161}"]);
    }

    #[test]
    fn a_prepend_character_attaches_to_what_follows_it() {
        // U+0600 ARABIC NUMBER SIGN leads the number it applies to.
        assert_eq!(clusters("\u{0600}\u{0661}a"), vec!["\u{0600}\u{0661}", "a"]);
    }

    #[test]
    fn a_spacing_mark_stays_with_its_base() {
        // Devanagari KA + the spacing vowel sign AA is one cluster.
        assert_eq!(clusters("\u{0915}\u{093E}"), vec!["\u{0915}\u{093E}"]);
    }

    #[test]
    fn a_control_breaks_on_both_sides() {
        // GB4/GB5: a tab is a Control, so nothing joins it from either side.
        assert_eq!(clusters("a\tb"), vec!["a", "\t", "b"]);
        assert_eq!(
            clusters("e\u{301}\u{0007}\u{301}"),
            vec!["e\u{301}", "\u{0007}", "\u{301}"]
        );
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
    fn an_unlisted_code_point_is_other_and_joins_nothing() {
        // Coverage, not rules: a mark from a script the tables do not list starts its own cluster.
        assert_eq!(property('\u{10FFFF}'), Property::Other);
        // And a leading mark is a cluster of its own rather than a continuation.
        assert_eq!(clusters("\u{301}\u{301}"), vec!["\u{301}\u{301}"]);
    }

    #[test]
    fn every_offset_is_a_character_boundary() {
        let text = "a\u{301}b\u{1F600}\u{1F3FD}c\u{1100}\u{1161}\u{11A8}\u{0600}\u{0661}";
        for (offset, cluster) in Clusters::new(text) {
            assert!(text.is_char_boundary(offset));
            assert!(text[offset..].starts_with(cluster));
        }
        // The clusters tile the text exactly: no byte is skipped and none is counted twice.
        let mut covered = 0;
        for (_, cluster) in Clusters::new(text) {
            covered += cluster.len();
        }
        assert_eq!(covered, text.len());
    }
}
