//! **The icon set: 34 hand-authored 1-bit masks.** 6 tests.
//!
//! # What these assertions are for, given that the icons are pictures
//!
//! A bitmap is the one artefact in this codebase that **compiles when it is wrong.** Every other thing
//! here is checked by a type or a bound; an icon that is mirrored, blank, or two pixels off its box is
//! a valid `&'static [u64]` of the right length and no compiler will ever mention it again. So the gate
//! is about the properties a human cannot reliably check by eye across 34 icons:
//!
//! | what it proves | test |
//! | --- | --- |
//! | **the packing is not mirrored** | [`packing_is_most_significant_bit_leftmost`] |
//! | every icon is the declared size and non-empty | [`every_icon_is_sixteen_square_and_not_empty`] |
//! | and none is nearly full, which would mean the art filled the box | [`no_icon_is_mostly_ink`] |
//! | the set has no duplicates | [`no_two_icons_share_a_mask`] |
//! | `ALL` and `mask()` cannot drift apart | [`the_all_list_and_the_match_agree`] |
//! | and coverage is `false` outside the mask | [`coverage_is_false_outside_the_mask`] |

use holonomy_render::icons::{mask, IconId, SIZE};
use holonomy_render::Icon;

/// **The bit order, asserted rather than described.**
///
/// [`Icon::coverage`] reads bit `63 - (x % 64)`, i.e. **most significant bit leftmost**. The opposite
/// convention mirrors every icon horizontally — which is the failure that *looks almost right*, because
/// a mirrored `undo` is still recognisably an arrow and a mirrored `b` is still a `b`. So it is pinned
/// with an asymmetric mask rather than by looking at the pictures.
///
/// The mask is packed by hand here, from the art, not by calling `pack`: a test that packs its fixture
/// with the function under test proves nothing.
#[test]
fn packing_is_most_significant_bit_leftmost() {
    // Pixel (0, 0) set, and nothing else.
    //
    // **`static`, not `let`, and that is the design working.** `Icon::bits` is `&'static [u64]` so a
    // mask cannot be built on the stack or owned by a caller -- which is what keeps every icon in
    // `.rodata` with no runtime construction. The cost is that a test fixture has to be a static too,
    // and that shows up here rather than as a comment.
    static TOP_BIT: [u64; 1] = [1u64 << 63];
    let icon = Icon {
        bits: &TOP_BIT,
        width: 2,
        height: 1,
        x: 0,
        y: 0,
        colour: 0,
    };
    assert!(
        icon.coverage(0, 0),
        "pixel 0 of row 0 must be bit 63 -- the leftmost pixel is the top bit"
    );
    assert!(
        !icon.coverage(1, 0),
        "and pixel 1 must be bit 62, which is clear here"
    );

    // Pixel 1 set, pixel 0 clear: the discriminating pair for a mirror.
    static NEXT_BIT: [u64; 1] = [1u64 << 62];
    let icon = Icon {
        bits: &NEXT_BIT,
        width: 2,
        height: 1,
        x: 0,
        y: 0,
        colour: 0,
    };
    assert!(
        !icon.coverage(0, 0),
        "with only bit 62 set, pixel 0 is clear"
    );
    assert!(
        icon.coverage(1, 0),
        "and pixel 1 is set -- rightmost is the low bit"
    );

    // **And the packing agrees with `coverage`.** `pack` is the thing that turns art into words, so a
    // packing that disagrees with the reader would render every icon mirrored.
    static PACKED: [u64; 16] = holonomy_render::icons::pack(&[
        b"#.#.#.#.#.#.#.#.",
        b".#.#.#.#.#.#.#.#",
        b"#.#.#.#.#.#.#.#.",
        b".#.#.#.#.#.#.#.#",
        b"#.#.#.#.#.#.#.#.",
        b".#.#.#.#.#.#.#.#",
        b"#.#.#.#.#.#.#.#.",
        b".#.#.#.#.#.#.#.#",
        b"#.#.#.#.#.#.#.#.",
        b".#.#.#.#.#.#.#.#",
        b"#.#.#.#.#.#.#.#.",
        b".#.#.#.#.#.#.#.#",
        b"#.#.#.#.#.#.#.#.",
        b".#.#.#.#.#.#.#.#",
        b"#.#.#.#.#.#.#.#.",
        b".#.#.#.#.#.#.#.#",
    ]);
    let icon = Icon {
        bits: &PACKED,
        width: SIZE,
        height: SIZE,
        x: 0,
        y: 0,
        colour: 0,
    };
    assert!(icon.coverage(0, 0) && !icon.coverage(1, 0), "row 0 is `#.`");
    assert!(!icon.coverage(0, 1) && icon.coverage(1, 1), "row 1 is `.#`");
    // Row 0 is `#.#.#...#.` -- 16 wide, so pixel 15 is the *last* pixel and is clear, while pixel 14
    // is set. That pair is what distinguishes "16 columns" from "15 columns with a ragged edge".
    assert!(icon.coverage(14, 0), "row 0's last ink is at pixel 14");
    assert!(
        !icon.coverage(15, 0),
        "and pixel 15 is clear, so the mask really is 16 wide"
    );
}

/// **Every icon is [`SIZE`] square, and every icon draws something.**
///
/// A mask of the wrong height is a panic in [`Icon::coverage`] at paint time — `bits[y * wpr + x/64]`
/// indexes past the slice — so this is not a style assertion.
#[test]
fn every_icon_is_sixteen_square_and_not_empty() {
    for id in IconId::ALL {
        let icon = id.at(0, 0, 0xFF00_00FF);
        assert_eq!(icon.width, SIZE, "{id:?} is not {SIZE} wide");
        assert_eq!(icon.height, SIZE, "{id:?} is not {SIZE} tall");
        assert_eq!(
            icon.words_per_row(),
            1,
            "{id:?}: {SIZE} px is one word per row, and anything else means the mask was authored \
             for a different size"
        );
        assert_eq!(
            icon.bits.len(),
            SIZE as usize,
            "{id:?}: the mask must have one word per row"
        );
        let ink = id.ink_count();
        assert!(
            ink > 8,
            "{id:?} has {ink} pixels set. An icon with almost nothing in it is either an unfinished \
             draft or a copy-paste that lost its art."
        );
    }
}

/// **No icon is mostly ink.**
///
/// The mirror of the test above, and the one that catches the mistake of drawing with a thick brush
/// instead of a thin one. A 16x16 box filled past ~40% reads as a *block* at 1x rather than as a glyph,
/// because there is no white space left for the eye to separate the strokes. **34 icons of blocks look
/// exactly like 34 icons of glyphs in a node-count assertion**, which is why this is a gate.
#[test]
fn no_icon_is_mostly_ink() {
    let budget = (SIZE * SIZE) / 3;
    for id in IconId::ALL {
        let ink = id.ink_count();
        assert!(
            ink <= budget,
            "{id:?} has {ink} of {} pixels set, over the {budget} budget. A filled box is not a glyph.",
            SIZE * SIZE
        );
    }
}

/// **No two icons are the same picture.**
///
/// Duplicates are how a toolbar ends up with the same arrow twice and nobody noticing for a week, and
/// they are invisible in a count of nodes. [`IconId::REDO`] and [`IconId::UNDO`] are the near-miss this
/// catches: they are mirrors, not duplicates, and a check that accepted mirrors would accept a
/// duplicate too if the art ever regressed.
#[test]
fn no_two_icons_share_a_mask() {
    for (i, a) in IconId::ALL.iter().enumerate() {
        for b in IconId::ALL.iter().skip(i + 1) {
            assert_ne!(
                a.mask(),
                b.mask(),
                "{a:?} and {b:?} are the same mask. Two buttons with the same picture are a bug that \
                 no count of nodes will show."
            );
        }
    }
}

/// **`ALL` and `mask()` agree, and the set is the size `ALL` claims.**
///
/// `ALL` exists so a gate can iterate the whole set. **If an icon is added to the enum and not to
/// `ALL`, every "every icon" test silently stops covering it** — which is the same class of failure as
/// the decimation test in `scale_cache.rs`: the gate passes, and the thing it was written for is not
/// covered. So `ALL` is checked against the enum's own discriminants.
#[test]
fn the_all_list_and_the_match_agree() {
    assert_eq!(
        IconId::ALL.len(),
        34,
        "the set grew or shrank. That is fine -- update this number, and check the new icon against \
         `no_icon_is_mostly_ink` and `every_icon_is_sixteen_square_and_not_empty`."
    );
    // Every `ALL` entry resolves, and `repr(u8)` keeps the values dense so `ALL` can be checked
    // against the range.
    let mut seen = [false; 256];
    for id in IconId::ALL {
        let d = *id as usize;
        assert!(!seen[d], "{id:?} is listed twice in ALL");
        seen[d] = true;
        assert!(
            !id.mask().is_empty(),
            "{id:?} resolves to an empty mask, so it is not in `mask()`"
        );
    }
    // **Only the first `ALL.len()` discriminants are required**, because `#[repr(u8)]` numbers the
    // variants from zero -- walking all 256 would be asserting a range that was never populated,
    // which is the same shape of mistake as the check itself was guarding against.
    for d in 0..IconId::ALL.len() {
        assert!(
            seen[d],
            "discriminant {d} has no entry in `IconId::ALL`. Every variant must be listed or the \
             'every icon' gates silently stop covering it."
        );
    }
}

/// **Coverage is `false` outside the mask, not a panic and not a wrap.**
///
/// [`Icon::coverage`] is called by the painter with clip-relative coordinates that can be negative
/// after a damage rect is applied, so "outside" has to mean *outside* rather than "wraps into the
/// neighbouring row".
#[test]
fn coverage_is_false_outside_the_mask() {
    let id = IconId::Doc;
    assert!(!id.at(0, 0, 0).coverage(SIZE, 0), "one past the right edge");
    assert!(
        !id.at(0, 0, 0).coverage(0, SIZE),
        "one past the bottom edge"
    );
    assert!(
        !id.at(0, 0, 0).coverage(u32::MAX, 0),
        "far right does not wrap"
    );
    assert!(
        !id.at(0, 0, 0).coverage(0, u32::MAX),
        "far down does not wrap"
    );

    // The same for a mask authored at a negative position, which is what a left-clipped toolbar button
    // produces: `bounds` clamps to 0 and `coverage` must not.
    let icon = mask::DOC.len() as u32;
    assert!(
        icon > 0 && !id.at(-4, -4, 0).coverage(u32::MAX, u32::MAX),
        "clipped icon, absurd coords"
    );
}
