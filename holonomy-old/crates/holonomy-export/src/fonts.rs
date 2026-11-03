//! Base-14 font metrics, and the WinAnsi encoding, for the PDF exporter.
//!
//! # Where these numbers come from
//!
//! They are the Adobe AFM widths for the Helvetica family, indexed by WinAnsiEncoding character code,
//! transcribed as a table. There is no build step that downloads or searches for them: PROJECT.md's
//! standing rule is that cryptographic and layout constants are compiled in, and this one earns that
//! treatment twice over -- a lookup that reached out to the network would make the exporter
//! non-deterministic, and a *wrong* width here produces a PDF that still opens and wraps at the wrong
//! places, which no parser complains about.
//!
//! They were cross-checked against two independent sources before being written down:
//!
//! * the Adobe AFM widths as published in `pdfplumber-parse`'s `standard_fonts.rs`, and
//! * `/usr/share/fonts/urw-base35/NimbusSans-{Regular,Bold}.afm`, which are URW's metric-compatible
//!   reimplementations of Helvetica.
//!
//! All 95 printable ASCII codes agree on both. [`widths_provenance_is_checked`] keeps that claim honest
//! for the handful of values worth pinning.
//!
//! # Bold does *not* share the regular widths
//!
//! Worth saying because it is the natural assumption and it is wrong: Helvetica-Bold's `A` is 722
//! where Helvetica's is 667. Laying out bold runs with the regular table overflows the measure by
//! about 8% on capitals, which is invisible in a small sample and obvious across a page. So there are
//! two tables, and [`BaseFont::width`] picks between them.
//!
//! # Oblique *does* share the upright widths
//!
//! Adobe's `Helvetica-Oblique` is a slanted drawing of Helvetica, not a redrawn italic, so it has the
//! same metrics. `Helvetica-BoldOblique` likewise shares `Helvetica-Bold`'s. This is the opposite of
//! the URW `NimbusSans-Italic`, which is a genuine italic design with its own widths -- a good
//! reminder that "the metrics from the local font" and "the metrics a PDF reader will use" are
//! different questions, and the latter is the one that decides where a line breaks.
//!
//! # No font is embedded
//!
//! The 14 standard fonts are in every conforming reader, so a Type1 font dictionary with a `/BaseFont`
//! and nothing else is a complete font. That is what keeps the exporter inside the 2.5 MiB binary
//! budget: embedding a subsetted TTF would mean shipping the subsetter, the glyph tables and the
//! metrics it derives.

/// Widths of one Base-14 font, in 1/1000 em, indexed by WinAnsi code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseFont {
    /// `Helvetica`, and equally `Helvetica-Oblique`.
    Helvetica,
    /// `Helvetica-Bold`, and equally `Helvetica-BoldOblique`.
    HelveticaBold,
    /// `Courier`, `Courier-Oblique`, `Courier-Bold` and `Courier-BoldOblique`: all 600.
    Courier,
}

/// The font's name in a PDF font dictionary, as a `Name` literal.
impl BaseFont {
    /// The `/BaseFont` value, without the slashes.
    pub const fn pdf_name(self) -> &'static [u8] {
        match self {
            // Courier's four variants are named, but the four Courier faces are *metrically identical*
            // -- every glyph 600 -- so code that picks a face for weight or slant would be pretending
            // to a distinction the layout does not make. `Courier` is honest; a `CourierBold` that
            // silently measured the same would not be.
            Self::Courier => b"Courier",
            Self::Helvetica => b"Helvetica",
            Self::HelveticaBold => b"Helvetica-Bold",
        }
    }

    /// Whether this face is fixed-pitch, which changes wrapping from measured to counted.
    #[inline]
    pub const fn is_monospaced(self) -> bool {
        matches!(self, Self::Courier)
    }

    /// The advance for one WinAnsi byte, in 1/1000 em.
    ///
    /// Bytes below 32 and above 126 return [`BaseFont::FALLBACK_WIDTH`] rather than a panic or a zero:
    /// the exporter transcodes to WinAnsi first, so a byte outside this range means a mapping bug, and
    /// a plausible width keeps the line break sane while the transcoder reports it.
    #[inline]
    pub fn width(self, byte: u8) -> u16 {
        match self {
            Self::Courier => COURIER_WIDTH,
            Self::Helvetica => table(&HELVETICA, byte),
            Self::HelveticaBold => table(&HELVETICA_BOLD, byte),
        }
    }

    /// The width used for a byte with no entry, in 1/1000 em.
    ///
    /// The space width, because an unknown character occupying a space's worth is the least
    /// surprising thing: it neither overflows the line nor collapses it.
    pub const FALLBACK_WIDTH: u16 = 278;
}

/// Index a width table, out of range to the fallback.
#[inline]
fn table(widths: &[u16; 95], byte: u8) -> u16 {
    match byte {
        32..=126 => widths[(byte - 32) as usize],
        _ => BaseFont::FALLBACK_WIDTH,
    }
}

/// Courier's advance: every glyph, including the space.
pub const COURIER_WIDTH: u16 = 600;

/// The advance of `text` in 1/1000 em, for a proportional face.
///
/// Sums per byte, so it is O(n) over the run -- and a run is bounded by the span map, not by the
/// document. See [`measure`](crate::pdf::measure) for the wrapping that consumes this.
#[inline]
pub fn advance(font: BaseFont, bytes: &[u8]) -> u32 {
    if font.is_monospaced() {
        // `usize` here rather than `u32`: this is a closed-form `len * 600`, so the multiply is one
        // instruction and there is no per-byte loop for a 40 KB run to walk.
        return (bytes.len() * usize::from(COURIER_WIDTH)) as u32;
    }
    bytes
        .iter()
        .fold(0u32, |acc, b| acc + u32::from(font.width(*b)))
}

/// The WinAnsi byte for a `char`, if it has one.
///
/// # Why WinAnsi and not Standard
///
/// A Type1 font with no `/Encoding` uses StandardEncoding, which is close enough to ASCII and has no
/// code above 127. WinAnsi is the encoding every real-world Latin-1 document uses, and it is what the
/// width tables above are indexed by, so declaring it makes the table and the encoding agree -- which
/// is the pairing that has to be right for wrapping to land where the reader breaks it.
///
/// # The 0x80..=0x9F hole
///
/// WinAnsi puts the C1 controls in 0x80..=0x9F and has *nothing* there. That is a real gap in the
/// encoding and not an oversight to paper over, so `None` is returned and the caller substitutes
/// [`UNMAPPED`]. Characters above U+00FF are outside WinAnsi entirely and also return `None`.
///
/// [`UNMAPPED`]: crate::pdf::UNMAPPED
pub fn winansi(c: char) -> Option<u8> {
    let u = c as u32;
    if (0x20..=0x7E).contains(&u) || (0xA0..=0xFF).contains(&u) {
        return Some(u as u8);
    }
    match u {
        0x20AC => Some(0x80), // euro
        0x201A => Some(0x82),
        0x0192 => Some(0x83), // florin
        0x201E => Some(0x84),
        0x2026 => Some(0x85), // ellipsis
        0x2020 => Some(0x86),
        0x2021 => Some(0x87),
        0x02C6 => Some(0x88), // modifier circumflex
        0x2030 => Some(0x89), // per mille
        0x0160 => Some(0x8A),
        0x2039 => Some(0x8B),
        0x0152 => Some(0x8C), // OE
        0x017D => Some(0x8E), // Z with caron
        0x2018 => Some(0x91),
        0x2019 => Some(0x92),
        0x201C => Some(0x93),
        0x201D => Some(0x94),
        0x2022 => Some(0x95), // bullet
        0x2013 => Some(0x96), // en dash
        0x2014 => Some(0x97), // em dash
        0x02DC => Some(0x98),
        0x2122 => Some(0x99), // trade mark
        0x0161 => Some(0x9A),
        0x203A => Some(0x9B),
        0x0153 => Some(0x9C), // oe
        0x017E => Some(0x9E),
        0x0178 => Some(0x9F), // Y with diaeresis
        _ => None,
    }
}

/// Helvetica widths, WinAnsi codes 32..=126, 1/1000 em.
pub static HELVETICA: [u16; 95] = [
    278,  //  32 ' '
    278,  //  33 '!'
    355,  //  34 '"'
    556,  //  35 '#'
    556,  //  36 '$'
    889,  //  37 '%'
    667,  //  38 '&'
    191,  //  39 "'"
    333,  //  40 '('
    333,  //  41 ')'
    389,  //  42 '*'
    584,  //  43 '+'
    278,  //  44 ','
    333,  //  45 '-'
    278,  //  46 '.'
    278,  //  47 '/'
    556,  //  48 '0'
    556,  //  49 '1'
    556,  //  50 '2'
    556,  //  51 '3'
    556,  //  52 '4'
    556,  //  53 '5'
    556,  //  54 '6'
    556,  //  55 '7'
    556,  //  56 '8'
    556,  //  57 '9'
    278,  //  58 ':'
    278,  //  59 ';'
    584,  //  60 '<'
    584,  //  61 '='
    584,  //  62 '>'
    556,  //  63 '?'
    1015, //  64 '@'
    667,  //  65 'A'
    667,  //  66 'B'
    722,  //  67 'C'
    722,  //  68 'D'
    667,  //  69 'E'
    611,  //  70 'F'
    778,  //  71 'G'
    722,  //  72 'H'
    278,  //  73 'I'
    500,  //  74 'J'
    667,  //  75 'K'
    556,  //  76 'L'
    833,  //  77 'M'
    722,  //  78 'N'
    778,  //  79 'O'
    667,  //  80 'P'
    778,  //  81 'Q'
    722,  //  82 'R'
    667,  //  83 'S'
    611,  //  84 'T'
    722,  //  85 'U'
    667,  //  86 'V'
    944,  //  87 'W'
    667,  //  88 'X'
    667,  //  89 'Y'
    611,  //  90 'Z'
    278,  //  91 '['
    278,  //  92 '\\'
    278,  //  93 ']'
    469,  //  94 '^'
    556,  //  95 '_'
    333,  //  96 '`'
    556,  //  97 'a'
    556,  //  98 'b'
    500,  //  99 'c'
    556,  // 100 'd'
    556,  // 101 'e'
    278,  // 102 'f'
    556,  // 103 'g'
    556,  // 104 'h'
    222,  // 105 'i'
    222,  // 106 'j'
    500,  // 107 'k'
    222,  // 108 'l'
    833,  // 109 'm'
    556,  // 110 'n'
    556,  // 111 'o'
    556,  // 112 'p'
    556,  // 113 'q'
    333,  // 114 'r'
    500,  // 115 's'
    278,  // 116 't'
    556,  // 117 'u'
    500,  // 118 'v'
    722,  // 119 'w'
    500,  // 120 'x'
    500,  // 121 'y'
    500,  // 122 'z'
    334,  // 123 '{'
    260,  // 124 '|'
    334,  // 125 '}'
    584,  // 126 '~'
];

/// Helvetica-Bold widths.
pub static HELVETICA_BOLD: [u16; 95] = [
    278, //  32 ' '
    333, //  33 '!'
    474, //  34 '"'
    556, //  35 '#'
    556, //  36 '$'
    889, //  37 '%'
    722, //  38 '&'
    238, //  39 "'"
    333, //  40 '('
    333, //  41 ')'
    389, //  42 '*'
    584, //  43 '+'
    278, //  44 ','
    333, //  45 '-'
    278, //  46 '.'
    278, //  47 '/'
    556, //  48 '0'
    556, //  49 '1'
    556, //  50 '2'
    556, //  51 '3'
    556, //  52 '4'
    556, //  53 '5'
    556, //  54 '6'
    556, //  55 '7'
    556, //  56 '8'
    556, //  57 '9'
    333, //  58 ':'
    333, //  59 ';'
    584, //  60 '<'
    584, //  61 '='
    584, //  62 '>'
    611, //  63 '?'
    975, //  64 '@'
    722, //  65 'A'
    722, //  66 'B'
    722, //  67 'C'
    722, //  68 'D'
    667, //  69 'E'
    611, //  70 'F'
    778, //  71 'G'
    722, //  72 'H'
    278, //  73 'I'
    556, //  74 'J'
    722, //  75 'K'
    611, //  76 'L'
    833, //  77 'M'
    722, //  78 'N'
    778, //  79 'O'
    667, //  80 'P'
    778, //  81 'Q'
    722, //  82 'R'
    667, //  83 'S'
    611, //  84 'T'
    722, //  85 'U'
    667, //  86 'V'
    944, //  87 'W'
    667, //  88 'X'
    667, //  89 'Y'
    611, //  90 'Z'
    333, //  91 '['
    278, //  92 '\\'
    333, //  93 ']'
    584, //  94 '^'
    556, //  95 '_'
    333, //  96 '`'
    556, //  97 'a'
    611, //  98 'b'
    556, //  99 'c'
    611, // 100 'd'
    556, // 101 'e'
    333, // 102 'f'
    611, // 103 'g'
    611, // 104 'h'
    278, // 105 'i'
    278, // 106 'j'
    556, // 107 'k'
    278, // 108 'l'
    889, // 109 'm'
    611, // 110 'n'
    611, // 111 'o'
    611, // 112 'p'
    611, // 113 'q'
    389, // 114 'r'
    556, // 115 's'
    333, // 116 't'
    611, // 117 'u'
    556, // 118 'v'
    778, // 119 'w'
    556, // 120 'x'
    556, // 121 'y'
    500, // 122 'z'
    389, // 123 '{'
    280, // 124 '|'
    389, // 125 '}'
    584, // 126 '~'
];
/// Keeps the provenance claim above honest for the values that carry the most weight.
///
/// Chosen because each one is either a value this exporter's own layout depends on, or a value a
/// plausible transcription error would produce. `A` is the one that proves the bold table is a *separate*
/// table and not a copy of the regular one; `space`, `i` and `W` are the extremes that make a
/// proportional font proportional; `m` is the widest lowercase letter and the usual wrapping offender.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_provenance_is_checked() {
        // Helvetica.
        assert_eq!(HELVETICA[0], 278, "space");
        assert_eq!(
            HELVETICA[(b'i' - 32) as usize],
            222,
            "i, the narrowest letter"
        );
        assert_eq!(HELVETICA[(b'A' - 32) as usize], 667, "A");
        assert_eq!(
            HELVETICA[(b'W' - 32) as usize],
            944,
            "W, the widest capital"
        );
        assert_eq!(
            HELVETICA[(b'm' - 32) as usize],
            833,
            "m, the widest lowercase"
        );
        assert_eq!(
            HELVETICA[(b'@' - 32) as usize],
            1015,
            "@ is the widest glyph in the range"
        );

        // Helvetica-Bold, and the difference that matters.
        assert_eq!(
            HELVETICA_BOLD[(b'A' - 32) as usize],
            722,
            "A is wider in bold"
        );
        assert_eq!(
            HELVETICA_BOLD[(b'W' - 32) as usize],
            944,
            "W is the same width in bold"
        );
        assert_eq!(
            HELVETICA_BOLD[(b'i' - 32) as usize],
            278,
            "i is wider in bold"
        );
        assert_eq!(
            HELVETICA_BOLD[(b'@' - 32) as usize],
            975,
            "@ is *narrower* in bold"
        );

        assert_ne!(
            HELVETICA, HELVETICA_BOLD,
            "if these were equal, bold would be laid out with the regular table"
        );
    }

    #[test]
    fn the_tables_are_exactly_95_wide() {
        assert_eq!(HELVETICA.len(), 95);
        assert_eq!(HELVETICA_BOLD.len(), 95);
    }

    #[test]
    fn lookup_matches_the_table() {
        assert_eq!(BaseFont::Helvetica.width(b'A'), 667);
        assert_eq!(BaseFont::HelveticaBold.width(b'A'), 722);
        assert_eq!(BaseFont::Courier.width(b'A'), 600);
        assert_eq!(BaseFont::Courier.width(b' '), 600);
        // Out of range falls back rather than panicking.
        assert_eq!(BaseFont::Helvetica.width(0), BaseFont::FALLBACK_WIDTH);
        assert_eq!(BaseFont::Helvetica.width(255), BaseFont::FALLBACK_WIDTH);
    }

    #[test]
    fn courier_is_exactly_600_everywhere() {
        for b in 0..=255u8 {
            assert_eq!(BaseFont::Courier.width(b), 600, "byte {b}");
        }
    }

    #[test]
    fn advance_sums_the_table() {
        // "iii" vs "WWW": 222*3 = 666 against 944*3 = 2832.
        assert_eq!(advance(BaseFont::Helvetica, b"iii"), 666);
        assert_eq!(advance(BaseFont::Helvetica, b"WWW"), 2832);
        // Courier counts.
        assert_eq!(advance(BaseFont::Courier, b"iiiiiiiiii"), 6000);
        assert_eq!(advance(BaseFont::Helvetica, b""), 0);
    }

    #[test]
    fn winansi_covers_ascii_and_latin1() {
        for b in 0x20u8..=0x7E {
            assert_eq!(winansi(b as char), Some(b), "byte {b:#x}");
        }
        assert_eq!(winansi('\u{a0}'), Some(0xA0));
        assert_eq!(winansi('\u{ff}'), Some(0xFF));
        assert_eq!(winansi('\u{e9}'), Some(0xE9), "e-acute");
    }

    #[test]
    fn winansi_maps_the_punctuation_range() {
        assert_eq!(winansi('\u{2014}'), Some(0x97), "em dash");
        assert_eq!(winansi('\u{2019}'), Some(0x92), "right single quote");
        assert_eq!(winansi('\u{20ac}'), Some(0x80), "euro");
        assert_eq!(winansi('\u{2026}'), Some(0x85), "ellipsis");
        assert_eq!(winansi('\u{2122}'), Some(0x99), "trade mark");
    }

    #[test]
    fn winansi_reports_the_gaps_honestly() {
        // The C1 hole.
        assert_eq!(winansi('\u{0080}'), None);
        assert_eq!(winansi('\u{009f}'), None);
        // Control characters.
        assert_eq!(winansi('\n'), None);
        assert_eq!(winansi('\t'), None);
        // Outside WinAnsi entirely.
        assert_eq!(winansi('\u{4e2d}'), None, "CJK has no WinAnsi code");
        assert_eq!(winansi('\u{1f600}'), None, "and neither has an emoji");
    }

    #[test]
    fn winansi_is_a_bijection_over_its_range() {
        // Every byte WinAnsi defines must come back from exactly one char.
        let mut seen = std::collections::BTreeSet::new();
        // Swept to U+2FFF, not U+00FF: the punctuation bytes 0x80..=0x9F are reached from characters
        // *above* Latin-1 (U+20AC, U+2014, ...), so a sweep that stopped at U+00FF would never see
        // them. An earlier version of this test did exactly that and missed the whole range.
        for u in 0x20u32..=0x2FFF {
            let c = char::from_u32(u).expect("in range");
            if let Some(b) = winansi(c) {
                assert!(seen.insert(b), "byte {b:#x} claimed twice, by U+{u:04X}");
            }
        }
        // 218 defined codes: 0x20..=0x7E (95), 0xA0..=0xFF (96), and 27 of the 32 slots in the
        // 0x80..=0x9F C1 range. The other five -- 0x81, 0x8D, 0x8F, 0x90, 0x9D -- WinAnsi leaves
        // undefined, which is why `winansi` returns `None` for that whole block rather than
        // pretending it is encodable. 95 + 96 + 27 = 218.
        assert_eq!(seen.len(), 218, "WinAnsi's defined code count");
        for b in [0x81u8, 0x8D, 0x8F, 0x90, 0x9D] {
            assert!(
                !seen.contains(&b),
                "byte {b:#x} is one WinAnsi leaves undefined"
            );
        }
        for b in [0x80u8, 0x85, 0x91, 0x97, 0x99, 0xA0, 0xFF] {
            assert!(seen.contains(&b), "byte {b:#x} is never produced");
        }
    }
}
