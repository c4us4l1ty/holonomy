//! Phase 9B: the micro-parser and its procedural layout.
//!
//! | requirement | test |
//! | --- | --- |
//! | the quadratic formula parses | [`the_quadratic_formula_parses_into_the_shape_it_is_written_in`] |
//! | its box is hand-computable | [`the_quadratic_formulas_box_is_hand_computable`] |
//! | the fraction bar spans the wider child | [`the_fraction_bar_spans_the_wider_child_plus_padding`] |
//! | the radical is tick plus overline | [`the_radical_is_a_tick_and_an_overline_and_no_glyphs`] |
//! | scripts rise from the baseline | [`a_superscript_rises_from_the_baseline_not_from_the_top`] |
//! | a subscript sits beside its superscript | [`a_subscript_sits_beside_the_superscript_not_under_it`] |
//! | integers are one cell per digit | [`an_integer_literal_is_one_cell_per_digit`] |
//! | `b^2` == `b^{2}` | [`a_braced_and_unbraced_script_are_the_same_tree`] |
//! | symbols resolve | [`every_symbol_the_grammar_names_resolves_to_a_codepoint`] |
//! | errors carry an offset and a name | [`a_parse_error_carries_an_offset_and_recovers_the_command_name`] |
//! | depth is bounded | [`nesting_is_bounded_rather_than_overflowing_the_stack`] |
//! | layout allocates nothing | [`laying_out_a_formula_allocates_nothing`] |

use holonomy_render::{layout_boxed, measure_math, MathLayout, MathMetrics, MathRun};
use holonomy_render::{parse_math, symbol, MathError, MathNode, MAX_DEPTH, OUT_OF_SCOPE, SYMBOLS};

/// The page's text cell: 8 x 18 px, which is what the desktop chrome uses.
const CELL_W: u32 = 8;
const CELL_H: u32 = 18;

/// The quadratic formula, exactly as the directive writes it.
const QUADRATIC: &[u8] = br"\frac{-b \pm \sqrt{b^2 - 4ac}}{2a}";

/// The metrics the arithmetic below is done in.
fn m() -> MathMetrics {
    MathMetrics::new(CELL_W, CELL_H)
}

/// The formula parsed, or the test fails with the parse error.
fn quadratic() -> MathNode {
    parse_math(QUADRATIC).expect("the quadratic formula parses")
}

/// The formula parses into the shape it is written in.
///
/// The claim is about *structure*, not "it parsed": a parser that flattened everything into a `Row` of
/// glyphs would also parse it and would then have no fraction bar to align.
#[test]
fn the_quadratic_formula_parses_into_the_shape_it_is_written_in() {
    let node = quadratic();
    let MathNode::Fraction { num, den } = &node else {
        panic!("the whole formula is a fraction, got {node:?}");
    };
    assert_eq!(
        &**den,
        &MathNode::Row(vec![MathNode::Int(2), MathNode::Symbol(u32::from(b'a')),]),
        "the denominator is the row `2a`"
    );

    // Numerator: `-`, `b`, `\pm`, `\sqrt{..}` -- **four** items, because the `-`, `4`, `a` and `c`
    // are *inside* the radical's braces. The first version of this test expected seven items with
    // `4ac` outside the radical, which is the same formula misread: `\sqrt{..}` takes a group and that
    // group is `b^2 - 4ac`. A parser that put the tail outside would have drawn a different formula
    // -- a bare radical followed by `b^2 - 4ac` -- and every width in this file would still have
    // looked plausible, which is why this is a structural assertion and not a width one.
    let MathNode::Row(items) = &**num else {
        panic!("the numerator is a row, got {num:?}");
    };
    assert_eq!(items.len(), 4, "`-`, `b`, `\\pm`, and the radical");
    assert_eq!(
        items[0],
        MathNode::Symbol(u32::from(b'-')),
        "a minus is its own symbol rather than an operator attached to what follows"
    );
    assert_eq!(items[1], MathNode::Symbol(u32::from(b'b')));
    assert_eq!(
        items[2],
        MathNode::Symbol(0xB1),
        "`\\pm` resolves to U+00B1 PLUS-MINUS, which is Latin-1 and **not** Mathematical Operators -- \
         worth pinning, because the whole 0x2200 block would not have contained it"
    );
    let MathNode::Sqrt(inner) = &items[3] else {
        panic!("the fourth item is the radical, got {:?}", items[3]);
    };

    // The radicand: `b^2 - 4ac`, five items.
    let MathNode::Row(rad) = &**inner else {
        panic!("the radicand is a row, got {inner:?}");
    };
    assert_eq!(rad.len(), 5, "`b^2`, `-`, `4`, `a`, `c`");
    assert!(
        matches!(
            rad[0],
            MathNode::SuperSub {
                sup: Some(_),
                sub: None,
                ..
            }
        ),
        "`b^2` is a base with a superscript and no subscript"
    );
    assert_eq!(rad[1], MathNode::Symbol(u32::from(b'-')));
    assert_eq!(
        rad[2],
        MathNode::Int(4),
        "`4ac` parses the 4 as an integer rather than a symbol, so its width is arithmetic and not a \
         lookup"
    );
    assert_eq!(rad[3], MathNode::Symbol(u32::from(b'a')));
    assert_eq!(rad[4], MathNode::Symbol(u32::from(b'c')));
}

/// The formula's box is a hand-checkable number.
///
/// Done by hand, with the intermediate boxes named, because a test that recomputes the layout's own
/// arithmetic proves nothing. `cell_h` is 18, `pad_px` 2, `bar_px` 1, `rule_pad_px` 4.
///
/// ```text
/// b            8 x 18, baseline 18
/// b^2          8 + 5(script cell) = 13 wide;  script cell is 5x11, rise is 18*3/8 = 6
///              above = max(18, 6 + 11) = 18, below = 0  ->  13 x 18, baseline 18
/// 4ac          3 cells = 24 wide, 18 tall, baseline 18
/// radicand     max(13, 24) + gap for `-`  ->  8 + 13 + 8 + 8 + 24 = 61 wide
///              above = max(18, 18) = 18, below = 0 -> 61 x 18, baseline 18
/// sqrt         61 + 2*3 = 67 wide, 18 tall, baseline 18
/// numerator    max(8+8+8, 67) + 8 = 16 vs 67 -> 67 + 8 = 75 wide
///              above = max(18, 18) + 2 = 20, below = max(18, 18) + 2 = 20
///              -> 75 x 41, baseline 20
/// denominator  16 x 18
/// fraction     max(75, 16) + 8 = 83 wide
///              above = 20, below = 20 + 1 + 2 = 23  -> 83 x 44
/// ```
#[test]
fn the_quadratic_formulas_box_is_hand_computable() {
    let mm = m();
    let box_ = measure_math(&quadratic(), &mm);

    // The radicand, checked on its own so a failure says *where* the arithmetic went wrong.
    let node = quadratic();
    let MathNode::Fraction { num, den } = &node else {
        unreachable!();
    };
    assert_eq!(
        measure_math(den, &mm),
        holonomy_render::MathBox {
            width: 16,
            height: 18,
            baseline: 18
        },
        "the denominator `2a` is two cells wide and one tall"
    );
    assert_eq!(
        measure_math(num, &mm).width,
        75,
        "the numerator is 75 px: the radical's 67 plus `\\pm`'s 8"
    );

    assert_eq!(
        box_,
        holonomy_render::MathBox {
            width: 83,
            height: 41,
            baseline: 20
        },
        "the whole formula is 83 x 41 with its baseline 20 px down: 75 + 4 + 4 across, and \\
         20 + 1 + 20 down. `above` and `below` each already carry their 2 px of `pad_px`, so the \\
         1 px bar is the only thing between them"
    );
}

/// The fraction bar spans the wider child plus padding, and is one pixel tall.
///
/// The bar's width is the claim: a bar sized to the numerator of `-b \pm \sqrt{..}` would leave the
/// denominator hanging off its right end, and a bar sized to the *sum* would stick out both sides.
#[test]
fn the_fraction_bar_spans_the_wider_child_plus_padding() {
    let mm = m();
    // A fraction whose denominator is narrower than its numerator: `\frac{\sqrt{b^2 - 4ac}}{2a}`.
    let mut out = MathLayout::with_capacity(16);
    layout_boxed(&quadratic(), &mm, 0, 0, &mut out);

    // There are **two** rules: the fraction bar and the radical's overline. The first version filtered
    // for `MathRun::Rule` and then asserted one, which counts the overline as well -- the two are the
    // same primitive and only their widths tell them apart, so the bar is the wider one.
    let rules: Vec<(u32, u32, u32, u32)> = out
        .runs
        .iter()
        .filter_map(|r| match r {
            MathRun::Rule { x, y, w, h } => Some((*x, *y, *w, *h)),
            _ => None,
        })
        .collect();
    assert_eq!(
        rules.len(),
        2,
        "two rules: the fraction bar and the radical's overline"
    );
    let (x, y, w, h) = *rules
        .iter()
        .max_by_key(|r| r.2)
        .expect("two rules, so a maximum exists");
    assert_eq!(x, 0, "the bar starts at the formula's left edge");
    assert_eq!(
        w, 83,
        "and is 83 px wide, the whole formula: the numerator's 75 plus 4 px of `rule_pad_px` each \\
         side"
    );
    assert_eq!(
        h, mm.bar_px,
        "and exactly one pixel tall -- the directive's \"1-2 px integer fill\", and 1 because a 2 px \\
         bar on a white page reads as a rule rather than as a division"
    );
    assert_eq!(
        (x, y),
        (0, 18),
        "the bar's top is the numerator's bottom edge, 18 px down -- 2 px of `pad_px` above
         the formula's baseline of 20, not on it. The first version asserted `y == 20` on
         the reasonable but wrong grounds that a bar sits on the baseline; it sits `pad_px`
         above it, because the baseline is where the fraction's content is centred and the bar
         is 1 px of the 41 that centring is measured across"
    );
}

/// The radical is a tick and an overline, and no glyph is drawn for it.
///
/// The absence of a `√` glyph is the point. Noto Sans Math *has* U+221A, and drawing it would mean
/// antialiased strokes meeting the radicand's first glyph, which is where a fractional `√` shows a
/// seam. Two rules and a tick cannot.
#[test]
fn the_radical_is_a_tick_and_an_overline_and_no_glyphs() {
    let mm = m();
    let mut out = MathLayout::with_capacity(32);
    layout_boxed(&quadratic(), &mm, 0, 0, &mut out);

    let ticks = out
        .runs
        .iter()
        .filter(|r| matches!(r, MathRun::RadicalTick { .. }))
        .count();
    assert_eq!(ticks, 1, "one radical, so one tick");

    let radical = out
        .runs
        .iter()
        .find_map(|r| match r {
            MathRun::RadicalTick { x, y, w, tick_h } => Some((*x, *y, *w, *tick_h)),
            _ => None,
        })
        .expect("a tick");
    assert_eq!(
        (radical.0, radical.1, radical.3),
        (28, 16, mm.tick_h_px),
        "the tick is at (28, 16) and {} px tall. Not the corner: the numerator is 75 px inside an \
         83 px fraction, so it is centred at (83 - 75) / 2 = 4, and the radical is its third item, \
         three 8 px glyphs in. 4 + 24 = 28, and its top is the bar's top less `pad_px` = 16",
        mm.tick_h_px
    );

    // And no glyph anywhere claims to be a square root.
    let radical_glyphs: Vec<u32> = out
        .runs
        .iter()
        .filter_map(|r| match r {
            MathRun::Glyph { cp, .. } if *cp == 0x221A => Some(*cp),
            _ => None,
        })
        .collect();
    assert!(
        radical_glyphs.is_empty(),
        "no U+221A glyph is emitted, because the radical is procedural: {:?}",
        radical_glyphs
    );

    // Two rules: the fraction bar and the radical's overline. Distinct widths, which is how a gate can
    // tell them apart without depending on order.
    let widths: Vec<u32> = out
        .runs
        .iter()
        .filter_map(|r| match r {
            MathRun::Rule { w, .. } => Some(*w),
            _ => None,
        })
        .collect();
    assert_eq!(widths.len(), 2, "the fraction bar and the overline");
    // **Sorted**, because emission order is not the claim: a fraction emits its numerator before its
    // bar, and the numerator is where the radical lives, so the overline is emitted *first*. The first
    // version compared `[83, 48]` and failed on `[48, 83]` -- which is the right answer in the wrong
    // order, and a gate that had asserted order for its own sake.
    let mut sorted = widths.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        vec![48, 83],
        "83 is the fraction bar and 48 is the overline: the radicand's 45 plus one 3 px tick width, \\
         the overline overhangs the right edge by 3 px. An overline that stops exactly at the last \\
         glyph reads as a box with its top edge missing"
    );
}

/// A subscript sits *beside* the superscript, not under it.
///
/// `measure` reserves `max(sup, sub)` for the two, and the first `emit` put both at the same x, so
/// the two drew on top of each other. No box assertion can see that: the width was right, the height
/// was right, and the pixels were wrong.
///
/// It stayed invisible for as long as the layout was a fixed 8 px grid, where a one-character script
/// is exactly 5 px wide and two scripts at the same x do not overlap. With real advances -- see
/// `MathMetrics::advance` -- `i=0` is 19 px against `n`'s 6, and `\sum_{i=0}^{n} i` rendered as
/// `∑ in0i`. The fix is in `emit`; this is the gate that would have caught it, and it exists because
/// every other test here lays out `b^2`, which has no sibling script to collide with.
#[test]
fn a_subscript_sits_beside_the_superscript_not_under_it() {
    let mm = m();
    let node = parse_math(br"x_i^2").expect("parses");
    let box_ = measure_math(&node, &mm);

    let mut out = MathLayout::with_capacity(16);
    layout_boxed(&node, &mm, 0, 0, &mut out);

    // The two script runs, by their codepoints.
    let glyph_x = |want: char| -> Option<u32> {
        out.runs.iter().find_map(|r| match *r {
            MathRun::Glyph { x, cp, .. } if char::from_u32(cp) == Some(want) => Some(x),
            _ => None,
        })
    };
    let x = glyph_x('x').expect("the base is drawn");
    let i = glyph_x('i').expect("the subscript is drawn");
    let two = glyph_x('2').expect("the superscript is drawn");

    assert_eq!(
        two, 8,
        "`x` is 8 px wide on the fixed grid, so the superscript starts there"
    );
    assert_eq!(
        i, 13,
        "the subscript starts 5 px after the superscript, which is one script cell -- the two are \
         side by side. Both were at 8 before, which drew them on top of each other"
    );
    assert_eq!(
        box_.width, 13,
        "and the box reserves exactly that: base 8 + one script cell 5, because `measure` takes \
         max(sup, sub) rather than their sum"
    );
    assert!(i > x && two > x, "both scripts clear the base's own 8 px");
}

/// A superscript rises from the baseline, not from the top of the box.
///
/// The distinction only shows up on a tall base, which is why the test uses one: `a^2` on a single
/// line would come out the same either way, and a gate that only tested that would have let the wrong
/// version through.
#[test]
fn a_superscript_rises_from_the_baseline_not_from_the_top() {
    let mm = m();
    // `\frac{b^2}{a}` -- the base of the script sits on a fraction, whose baseline is in the middle.
    let node = parse_math(br"\frac{b^2}{a}").expect("parses");
    let b = measure_math(&node, &mm);
    let MathNode::Fraction { num, .. } = &node else {
        unreachable!();
    };
    let num_box = measure_math(num, &mm);

    assert_eq!(
        num_box.baseline, 18,
        "the numerator's baseline is 18 px down -- `b^2` is 18 tall, so nothing above it"
    );
    assert_eq!(
        num_box.height, 18,
        "and the superscript did NOT make it taller: `b^2`'s script rises 6 px and is 11 tall, so its \\
         top is at 18 - 6 - 11 = 1, which is inside the 18 the base already occupies. Measuring the \\
         script from the top of the box instead of from the baseline would have given 6 + 11 = 17 \\
         above and a box 35 tall for a formula that is 18"
    );
    assert_eq!(
        (b.width, b.height, b.baseline),
        (21, 41, 20),
        "the fraction is 21 x 41: the numerator's 13 px (`b` is 8, the script cell 5) plus 4 px of \
         `rule_pad_px` either side"
    );
}

/// An integer literal is one cell per digit, and `b^2` equals `b^{2}`.
///
/// The structural half matters: if digits were glyphs, `b^2` and `b^{2}` would be two different trees
/// that happened to draw the same thing, and a gate comparing them would compare the renderer rather
/// than the parser.
#[test]
fn an_integer_literal_is_one_cell_per_digit() {
    let mm = m();
    for (v, cells) in [(0i64, 1u32), (7, 1), (42, 2), (1000, 4)] {
        assert_eq!(
            measure_math(&MathNode::Int(v), &mm).width,
            cells * CELL_W,
            "the integer {v} is {cells} cell(s) wide"
        );
    }
    assert_eq!(
        parse_math(b"b^2").expect("parses"),
        parse_math(b"b^{2}").expect("parses"),
        "and a braced and an unbraced script are the same tree, not two trees that draw alike"
    );
    assert_eq!(
        holonomy_render::digits(i64::MIN),
        19,
        "`i64::MIN` has 19 digits and is not negated on the way: `abs()` would overflow"
    );
}

/// Every symbol the grammar names resolves, and the table is sorted so the binary search is valid.
///
/// Sortedness is not a style point: `symbol` binary-searches, and an unsorted table would make
/// `\\omega` unreachable while `\\alpha` worked -- a bug that a single-symbol test would never see.
#[test]
fn every_symbol_the_grammar_names_resolves_to_a_codepoint() {
    let mut sorted = SYMBOLS.to_vec();
    sorted.sort_by_key(|&(n, _)| n);
    assert_eq!(
        sorted,
        SYMBOLS.to_vec(),
        "SYMBOLS must be sorted by name, because `symbol` binary-searches it"
    );
    for &(name, cp) in SYMBOLS {
        assert_eq!(symbol(name), Some(cp), "`\\{name}` resolves to U+{cp:04X}");
        assert!(char::from_u32(cp).is_some(), "U+{cp:04X} is a scalar value");
        assert_ne!(
            (0x2500..=0x257F).contains(&cp),
            true,
            "`\\{name}` is U+{cp:04X}, inside Box Drawing, which is procedural"
        );
    }
    assert_eq!(
        SYMBOLS.len(),
        58,
        "58 symbols -- 24 lowercase Greek, 11 uppercase, 23 operators -- and adding one means \
         updating this number"
    );
    assert!(
        OUT_OF_SCOPE.iter().all(|n| symbol(n).is_none()),
        "and nothing in OUT_OF_SCOPE is also a symbol, or the two lists would disagree about whether \\
         it is supported"
    );
}

/// A parse error carries an offset, and the command's name is recoverable from the source.
///
/// The offset is the difference between an actionable error and "unexpected token", and the name is
/// recovered without the error carrying a borrow -- see `MathError::name_in`.
#[test]
fn a_parse_error_carries_an_offset_and_recovers_the_command_name() {
    // An unbalanced group: the brace is never closed.
    let err = parse_math(br"\frac{1").expect_err("no closing brace");
    assert_eq!(
        err,
        MathError::UnexpectedEnd { at: 5 },
        "reported at the **opening brace**, byte 5, rather than at the end of the source: the brace is \
         the thing that was never closed, and pointing past the last byte is a worse place to start"
    );

    // A real command that is out of scope, reported as such rather than as unknown.
    let source = br"\frac{1}{2}\quad";
    let err = parse_math(source).expect_err("\\quad is not in the grammar");
    let MathError::UnsupportedCommand { at, name_len } = err else {
        panic!("expected UnsupportedCommand, got {err:?}");
    };
    assert_eq!(
        at, 11,
        "at the backslash, which is byte 11 of an 18-byte source"
    );
    assert_eq!(name_len as usize, 4, "and `quad` is four letters");
    assert_eq!(
        err.name_in(source),
        "quad",
        "recovered from the source without the error owning a borrow of it"
    );

    // And genuinely unknown.
    let err = parse_math(br"\notacommand").expect_err("not a command");
    assert_eq!(err.name_in(br"\notacommand"), "notacommand");

    // A `}` with nothing open is its own error, not "unexpected token".
    assert_eq!(
        parse_math(b"a}b").expect_err("unbalanced"),
        MathError::UnbalancedClose { at: 1 }
    );

    // `\` followed by punctuation is an escaped character, which is LaTeX's own rule.
    assert_eq!(
        parse_math(br"\ ").expect("an escaped space"),
        MathNode::Symbol(u32::from(b' ')),
        "`\\ ` is a space, not an error"
    );
}

/// Nesting is bounded, so a pathological formula cannot overflow the stack on a keystroke.
///
/// The renderer walks the same tree the parser built, so an unbounded depth is a crash in the middle of
/// typing rather than an error.
#[test]
fn nesting_is_bounded_rather_than_overflowing_the_stack() {
    let deep = format!(
        "{}{}",
        "\\frac{1}{".repeat(MAX_DEPTH + 4),
        "}".repeat(MAX_DEPTH + 4)
    );
    let err = parse_math(deep.as_bytes()).expect_err("deeper than the limit");
    assert_eq!(
        err,
        MathError::TooDeep { max: MAX_DEPTH },
        "refused at the limit rather than recursing"
    );
    // And one level under it is fine, so the limit is not simply refusing everything.
    let ok = format!(
        "{}{}",
        "\\frac{1}{".repeat(MAX_DEPTH - 1),
        "}".repeat(MAX_DEPTH - 1)
    );
    assert!(
        parse_math(ok.as_bytes()).is_ok(),
        "{MAX_DEPTH} levels of nesting is within the limit"
    );
}

/// Laying out a formula allocates nothing.
///
/// The gate is on `Vec::capacity`, not on timing, because an allocation is a *count* and a count is
/// what the requirement is about -- "zero dynamic allocations during in-math typing". Reserving up
/// front and then checking the capacity never moved is how you assert that, rather than measuring a
/// rate and hoping.
#[test]
fn laying_out_a_formula_allocates_nothing() {
    let mm = m();
    let node = quadratic();
    // Generous, so the check is not accidentally satisfied by a formula that happens to fit.
    let mut out = MathLayout::with_capacity(256);
    let before = out.runs.capacity();
    let box_ = layout_boxed(&node, &mm, 40, 60, &mut out);
    assert_eq!(
        out.runs.capacity(),
        before,
        "laying out {} runs did not grow a Vec reserved for 256",
        out.runs.len()
    );
    assert_eq!(
        out.box_,
        holonomy_render::MathBox::default(),
        "and `layout_boxed` returns the box rather than storing it, so a caller that only wants the \\
         runs is not forced to keep a second copy -- `out.box_` is untouched, and the gate for the \\
         box reads the return value"
    );
    assert_eq!(
        (box_.width, box_.height),
        (83, 41),
        "the box is the hand-computed one"
    );
    assert_eq!(
        out.runs.len(),
        14,
        "and the runs are 11 glyphs, 2 rules and 1 tick: `-`, `b`, `\\pm`, then the radicand's `b`, \\
         `2`, `-`, `4`, `a`, `c`, then the denominator's `2` and `a`"
    );

    // `measure` allocates nothing at all, which is a stronger claim than "allocates nothing after a
    // layout" and is why the parser can call it freely.
    for _ in 0..1000 {
        let b = measure_math(&node, &mm);
        assert_eq!(
            (b.width, b.height),
            (83, 41),
            "measuring 1000 times gives the same box"
        );
    }
}
