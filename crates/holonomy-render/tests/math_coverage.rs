//! Phase 9B: every symbol the grammar can name is a glyph the math face actually carries.
//!
//! This is the gate that makes `math.rs`'s symbol table a *contract* rather than a wish. The parser
//! resolves `\omega` to U+03C9 and hands the renderer a codepoint; nothing between those two points
//! checks that the font has a glyph for it, so a symbol added to the table without subsetting the
//! face renders as .notdef -- a hollow box, which reads as a missing glyph rather than as a missing
//! subsetting step.
//!
//! | requirement | test |
//! | --- | --- |
//! | every symbol is in the font | [`every_symbol_the_parser_can_name_is_in_the_math_face`] |
//! | every symbol is in a declared range | [`every_symbol_is_inside_a_declared_math_range`] |
//! | the subsetting actually happened | [`the_math_face_really_carries_greek_and_operators`] |

use holonomy_assets::payload;
use holonomy_render::SYMBOLS;

/// Every symbol the parser can name, decompressed once.
fn math_face() -> ttf_parser::Face<'static> {
    // The payload is brotli'd and lives in a `const`, so a test that wants glyph indices has to
    // decompress it. Decompressed once into a leaked buffer: `ttf_parser::Face` borrows its bytes, and
    // a local `Vec` would not outlive the face.
    let mut input = payload::PACKED_FONTS;
    let mut out = Vec::new();
    {
        use std::io::Read;
        brotli_decompressor::Decompressor::new(&mut input, 4096)
            .read_to_end(&mut out)
            .expect("the payload decompresses");
    }
    let out: &'static [u8] = Box::leak(out.into_boxed_slice());
    let entry = payload::FACES
        .iter()
        .find(|f| f.style == payload::Style::Math)
        .expect("a math face");
    ttf_parser::Face::parse(
        &out[entry.offset as usize..(entry.offset + entry.length) as usize],
        0,
    )
    .expect("the math face parses")
}

/// Every symbol the parser can name resolves to a glyph in the math face.
///
/// The one that matters. A `\pm` that parses to a codepoint the face does not carry would not error --
/// it would draw a hollow box, and the formula would look *nearly* right.
#[test]
fn every_symbol_the_parser_can_name_is_in_the_math_face() {
    let face = math_face();
    let absent: Vec<(&str, u32)> = SYMBOLS
        .iter()
        .filter(|&&(_, cp)| {
            face.glyph_index(char::from_u32(cp).expect("a scalar"))
                .is_none()
        })
        .map(|&(name, cp)| (name, cp))
        .collect();
    assert!(
        absent.is_empty(),
        "{} of {} symbols are not in the math face and would render as .notdef: {:?}",
        absent.len(),
        SYMBOLS.len(),
        &absent[..absent.len().min(6)]
    );
    assert_eq!(
        SYMBOLS.len(),
        58,
        "58 symbols, and the count is here so that removing one is a deliberate act"
    );
}

/// Every symbol is inside a range the payload declares as math coverage.
///
/// Separate from the glyph check on purpose. A glyph can be in the font and still be outside
/// `MATH_RANGES`, which means the subsetter was told to keep it by something other than the declared
/// coverage -- and then a future regeneration with a corrected range would silently drop it.
#[test]
fn every_symbol_is_inside_a_declared_math_range() {
    let outside: Vec<(&str, u32)> = SYMBOLS
        .iter()
        .filter(|&&(_, cp)| {
            !payload::MATH_RANGES
                .iter()
                .any(|&(lo, hi)| cp >= lo && cp <= hi)
        })
        .map(|&(name, cp)| (name, cp))
        .collect();
    assert!(
        outside.is_empty(),
        "{} symbols are outside MATH_RANGES {:?}: {:?}",
        outside.len(),
        payload::MATH_RANGES,
        &outside[..outside.len().min(6)]
    );
    // And none of them is Box Drawing, which is procedural: a symbol resolving into that range would
    // mean two procedural sources were drawing the same codepoint.
    assert!(
        SYMBOLS
            .iter()
            .all(|&(_, cp)| !(payload::BOX_RANGE.0..=payload::BOX_RANGE.1).contains(&cp)),
        "no symbol resolves into the procedural Box Drawing range"
    );
}

/// The math face really carries Greek and the operators, at both sizes the atlas builds.
///
/// The ranges are declared in `payload.rs` and generated from `build_font_payload.py`; a range that says
/// 0x2200..0x22FF and a face with nothing in it would pass every count-based gate and render nothing.
#[test]
fn the_math_face_really_carries_greek_and_operators() {
    let face = math_face();
    for (label, cp) in [
        ("alpha", 0x3B1u32),
        ("omega", 0x3C9),
        ("Omega", 0x3A9),
        ("plus-minus", 0xB1),
        ("times", 0xD7),
        ("divide", 0xF7),
        ("sum", 0x2211),
        ("integral", 0x222B),
        ("not-equal", 0x2260),
        ("less-or-equal", 0x2264),
        ("greater-or-equal", 0x2265),
        ("infinity", 0x221E),
    ] {
        let c = char::from_u32(cp).expect("a scalar");
        assert!(
            face.glyph_index(c).is_some(),
            "{} (U+{cp:04X}) is in the math face -- it is the symbol set the directive names, and a \
             missing one would render as a hollow box rather than as an error",
            label
        );
    }
}
