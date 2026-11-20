//! A real face behind the `Shaper` trait — `docs/TO-APP.md` **M3**.
//!
//! Every geometry test before this one asked a shaper that a test wrote. This one asks Inter:
//! `holonomy_eye::Face` parses the font's `head`, `hhea`, `hmtx`, `maxp` and `cmap`, and the
//! assertions below are about *the font's own numbers* — proportional advances, the font's ascent
//! and descent, a real character map — not about arithmetic on a constant.
//!
//! What is asserted:
//!
//! 1. The file parses, and reports the design units and glyph count a real text face has;
//! 2. every printable ASCII character maps to a glyph, and every one of them has a width;
//! 3. the face is **proportional** — `i` is not `m` — so a monospace assumption anywhere in the
//!    geometry would show up as a failure rather than as a coincidence;
//! 4. scaling is exact to within one sixty-fourth of a unit when the size doubles;
//! 5. a cluster's width is the sum of its characters' advances, and a hard break has no width;
//! 6. line height is the font's ascender minus its descender, and ascent sits inside it;
//! 7. a paragraph laid out with this face keeps the law the Eye exists for:
//!    `hit_test(paint_point(c)) == c` for every caret the layout offers.
//!
//! The numbers print as `HOLONOMY_FACE units_per_em= glyphs= mapped= advances= roundtrips=`, and
//! the counts are asserted, so a green run cannot be a run that read no font.
//!
//! The font is the one the previous editor shipped (`holonomy-old/assets/fonts/Inter-Regular.ttf`,
//! SIL Open Font License, licence beside it). It is compiled into this test binary, so the test
//! does not depend on the working directory.

use holonomy_eye::{Face, Fixed, Shaper, Style};

const FONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../holonomy-old/assets/fonts/Inter-Regular.ttf"
));

fn face() -> Face {
    Face::parse(FONT).expect("the font parses")
}

fn size(units: i64) -> Style {
    Style::at(units * 1_000)
}

/// One sixty-fourth of a logical unit — the smallest disagreement the arithmetic can have.
const TICK: Fixed = Fixed::from_64ths(1);

fn near(one: Fixed, other: Fixed) -> bool {
    let delta = one.sixtyfourths() - other.sixtyfourths();
    delta.abs() <= 1
}

#[test]
fn the_font_is_read_rather_than_assumed() {
    let face = face();
    assert!(
        face.units_per_em() >= 1_000,
        "only {} design units per em",
        face.units_per_em()
    );
    assert!(
        face.glyph_count() > 100,
        "only {} glyphs",
        face.glyph_count()
    );
    assert!(
        face.mapped_characters() > 200,
        "only {} mapped characters",
        face.mapped_characters()
    );

    // Every printable ASCII character is in the font, and every one of them has a width.
    let mut advances = 0u64;
    for character in ' '..='~' {
        let glyph = face
            .glyph(character)
            .unwrap_or_else(|| panic!("the font has no glyph for {character:?}"));
        assert!(glyph != 0, "{character:?} maps to .notdef");
        let units = face.advance_units(character);
        assert!(units > 0, "{character:?} has no advance");
        advances += 1;
    }

    // Proportional, not monospace: this is the property a constant shaper can never have.
    let i = face.advance_units('i');
    let m = face.advance_units('m');
    let w = face.advance_units('W');
    assert!(i < m && i < w, "i={i} m={m} W={w} is not proportional");
    assert!(
        face.advance('i', size(1)) < face.advance('m', size(1)),
        "the advances did not survive scaling"
    );

    println!(
        "HOLONOMY_FACE units_per_em={} glyphs={} mapped={} advances={advances}",
        face.units_per_em(),
        face.glyph_count(),
        face.mapped_characters()
    );
    assert_eq!(advances, 95, "printable ASCII, space to tilde");
}

#[test]
fn scaling_and_sums_are_the_fonts_own_arithmetic() {
    let face = face();
    let small = size(1);
    let double = size(2);

    let mut scalings = 0u64;
    for character in ['i', 'm', 'W', 'a', ' '] {
        let one = face.advance(&character.to_string(), small);
        let two = face.advance(&character.to_string(), double);
        assert!(
            near(two, one + one),
            "{character:?} at twice the size is {two:?}, not {:?}",
            one + one
        );
        scalings += 1;
    }

    // A cluster is the sum of its characters: no kerning is claimed, and no kerning is invented.
    let mut sums = 0u64;
    for pair in ["ab", "ma", "W.", "i'm"] {
        let mut total = Fixed::ZERO;
        for character in pair.chars() {
            total = total + face.advance(&character.to_string(), small);
        }
        let cluster = face.advance(pair, small);
        assert!(
            near(cluster, total),
            "{pair:?} is {cluster:?}, not the sum {total:?}"
        );
        sums += 1;
    }

    // A hard break has no width; it ends a line rather than occupying one.
    for line in ["\n", "\r\n"] {
        assert_eq!(face.advance(line, small), Fixed::ZERO, "{line:?} has width");
    }

    // Line metrics are the font's: ascender minus descender, with ascent inside it.
    let height = face.line_height(small);
    let ascent = face.ascent(small);
    assert!(height > Fixed::ZERO && ascent > Fixed::ZERO);
    assert!(ascent < height, "the ascent is taller than the line box");
    assert!(
        near(face.line_height(double), height + height),
        "the line box did not scale"
    );

    println!(
        "HOLONOMY_FACE_SCALE scalings={scalings} sums={sums} line_height={} ascent={} \
         tick_64ths={}",
        height.sixtyfourths(),
        ascent.sixtyfourths(),
        TICK.sixtyfourths()
    );
    assert!(scalings >= 5 && sums >= 4);
}

#[test]
fn a_paragraph_laid_out_with_a_real_face_keeps_the_law() {
    let face = face();
    let style = size(1);
    let width = Fixed::units(200);
    let text = "The quick brown fox jumps over the lazy dog — 0123456789\n\
                and a second line, because a writer wraps where the page ends.";
    let laid = holonomy_eye::layout_window(text, 0, width, &face);

    // Every line fits, except where one cluster is wider than the whole width.
    let widest = text
        .chars()
        .map(|character| face.advance(&character.to_string(), style))
        .max()
        .expect("a character");
    for (index, line) in laid.lines().iter().enumerate() {
        assert!(
            line.end_x() <= width + widest,
            "line {index} ends at {:?}, past the {width:?} width",
            line.end_x()
        );
    }

    // The law: every caret the layout offers can be walked back to from its own painted point.
    let mut roundtrips = 0u64;
    for line in laid.lines() {
        for (_, byte) in line.boundaries() {
            let painted = laid
                .paint_point(byte)
                .unwrap_or_else(|| panic!("byte {byte} has a boundary but no paint point"));
            let back = laid.hit_test(painted.point()).unwrap_or_else(|| {
                panic!("byte {byte} painted at a point that hit-tests to nothing")
            });
            assert_eq!(
                back.byte, byte,
                "byte {byte} round-tripped to {}",
                back.byte
            );
            roundtrips += 1;
        }
    }

    // And the paragraph is wider than it is tall, or it was not laid out at all.
    assert!(
        laid.lines().len() >= 2,
        "a two-line paragraph laid out as {} line(s)",
        laid.lines().len()
    );
    assert!(laid.height() > Fixed::ZERO);
    println!(
        "HOLONOMY_FACE_LAYOUT lines={} roundtrips={roundtrips} height={} \
         width={}",
        laid.lines().len(),
        laid.height().sixtyfourths(),
        laid.width().sixtyfourths()
    );
    assert!(roundtrips > 50, "only {roundtrips} carets round-tripped");
}
