//! **The wrap rule, held on its own.** Part 45.
//!
//! # What this file is for
//!
//! **A rule that four subsystems must agree on is worth testing before any of them uses it.** The
//! rule lives in [`holonomy_render::wrap`] as a pure function of `(bytes, measure)`, so every gate
//! here is arithmetic: no session, no atlas, no paint, no frame. **A defect found here is a defect in
//! the coordinate system**; the same defect found after wiring would be a caret three rows off and much
//! harder to attribute.
//!
//! # Every expected row in this file is hand-written, and two of them were wrong
//!
//! The numbers below were **measured and then reasoned about**, not computed from the code. Two of the
//! first drafts were wrong by arithmetic rather than by anything the code did:
//!
//! - `the_wrap_falls_after_the_whitespace` expected **five** rows for a 35-character line at a measure
//!   of 20. It is **two**, because `delta echo foxtrot` is 18.
//! - `line_rows_is_exactly_the_line_s_own_rows` expected one row for `"one line that is short"`. It is
//!   **22 characters**, so at a measure of 20 it is two.
//!
//! **Both failures were in the gates, not in the rule**, and both are recorded at the gate. The
//! alternative — computing the expected value from the implementation — is the thing this file exists
//! to avoid: **a gate that reads its answer out of the code it is testing cannot fail.**
//!
//! # The fixtures, and why they are the shapes they are
//!
//! | fixture | what it is for |
//! |---|---|
//! | words at the measure | the ordinary case, and the row boundaries should be exact |
//! | a line with **no** whitespace | rule 3 — a long token must still be reachable |
//! | multi-byte UTF-8 | rule 4 — no row may split a codepoint |
//! | an empty line | rule 5 — it must produce a row, or the caret has nowhere to sit |
//! | a line starting with a space | the rule 2 `w > row_start` guard |
//! | a line of only spaces | rule 1 against rule 2 pulling in opposite directions |
//!
//! # The offset fixture
//!
//! **`Line::start` is the document's own offset**, and the map adds nothing to it.
//! `an_offset_in_the_second_line_is_exact` proves it by building the same line twice, once inside a
//! three-line map and once alone — **because the alternative design (the map counting lines up from
//! zero) is correct for the first page and wrong for every page after it**, by one byte per line seen,
//! and the symptom is a caret that drifts further off the longer you scroll.

use holonomy_render::wrap::{Line, WrapMap};

/// **A round number of cells, and deliberately not the page's 72.**
///
/// **Twenty is small enough that every expected row in this file can be written out by hand.** A gate
/// that computes `lines.len() * measure` to check itself proves that the map agrees with the map.
const M: u32 = 20;

/// One line at offset 0.
fn line<'a>(index: u32, bytes: &'a [u8]) -> Line<'a> {
    Line {
        line: index,
        start: 0,
        bytes,
    }
}

/// One line at a given document offset.
fn line_at<'a>(index: u32, start: u32, bytes: &'a [u8]) -> Line<'a> {
    Line {
        line: index,
        start,
        bytes,
    }
}

/// The rows' `(start, end, cells)`, the shape every assertion below is written in.
///
/// **Three numbers rather than a `Row`**, because a `Row`'s interesting numbers *are* those three and
/// printing the struct would put `line` and `start_column` in every failure message for nothing.
fn spans(m: &WrapMap) -> Vec<(u32, u32, u32)> {
    m.rows().iter().map(|r| (r.start, r.end, r.cells)).collect()
}

/// **A line that fits is one row, and it is the whole line.**
///
/// **The negative half of every other gate here.** A rule that broke a line that fits would pass all
/// the interesting cases and put a blank row under every short paragraph, so this is the cheapest
/// possible check and it comes first.
#[test]
fn a_line_that_fits_is_one_row() {
    let m = WrapMap::build(M, [line(0, b"short line\n")]);
    assert_eq!(
        spans(&m),
        vec![(0, 10, 10)],
        "**one row of ten cells**, bytes 0..10 -- the content, not the newline"
    );
    assert_eq!(m.len(), 1);
    assert_eq!(m.measure(), M);
}

/// **A line twice the measure is two rows, at exactly the measure.**
///
/// **Exact boundaries, not "roughly two rows".** `the_wrap_falls_after_the_whitespace` checks the
/// *preferred* break; this checks the *fallback*, and a rule that returned one row of forty cells
/// would fail on `cells`.
#[test]
fn a_long_word_wraps_at_the_measure() {
    let content: Vec<u8> = (0..40).map(|i| b'a' + u8::try_from(i % 26).expect("fits")).collect();
    let mut bytes = content.clone();
    bytes.push(b'\n');
    let m = WrapMap::build(M, [line(0, &bytes)]);
    assert_eq!(
        spans(&m),
        vec![(0, 20, 20), (20, 40, 20)],
        "**two rows of exactly twenty cells**. No row is longer than the measure, which is rule 3 and \
         the reason a 300-character URL is still reachable."
    );
}

/// **Word wrap: the break falls after the last space, and no row begins with whitespace.**
///
/// ## The fixture, and why its first draft was wrong
///
/// **`alpha bravo charlie delta echo foxtrot` at a measure of 20** is 35 characters. The first twenty
/// codepoints are `alpha bravo charlie` plus the space after it, so there is exactly one legal break
/// inside the first row and it is the space at byte 19.
///
/// **So the row *count* is not the assertion; the spans are.** Breaking after the space and breaking at
/// the measure give the same two rows here, because the last space and the measure are adjacent in this
/// fixture. **A rule that left the space on the next row gives `(0,19) (20,38)`**, and that is what
/// this gate rules out.
///
/// The first draft expected **five** rows and was **wrong by arithmetic, not by code**: 35 characters
/// at a measure of 20 is two rows, because `delta echo foxtrot` is 18 and fits on the second.
#[test]
fn the_wrap_falls_after_the_whitespace() {
    let text = b"alpha bravo charlie delta echo foxtrot\n";
    let m = WrapMap::build(M, [line(0, text)]);
    assert_eq!(
        spans(&m),
        vec![(0, 20, 20), (20, 38, 18)],
        "**two rows: `alpha bravo charlie ` and `delta echo foxtrot`.** The break is *after* the space \
         at byte 19, so row 0 keeps it -- which is what makes the rows tile -- and row 1 starts at \
         the space's successor, byte 20."
    );
    for r in m.rows() {
        if r.start < r.end {
            assert_ne!(
                text[r.start as usize],
                b' ',
                "**row at byte {} starts with a space** -- the paragraph would be indented by one \
                 cell on every continuation, which is the signature of a naive wrap",
                r.start
            );
        }
    }
    // **And the continuation starts at column 20, not 19.** A row that broke *after* the space used
    // that space's cell, so the next row begins one column further on — and a `start_column` stuck at
    // 19 would place row 1's glyphs one cell left of where its own cells are.
    assert_eq!(m.rows()[1].start_column, M, "**the continuation starts at column 20**");
}

/// **A row that ends at a space keeps it, and that is what makes the rows tile.**
///
/// ## The gate that found the opposite, and how
///
/// The first version asserted that a row ending at a space reports one cell *fewer* than it has bytes,
/// on the reasoning that the space was "consumed by the break". **`a_lines_rows_tile_it_exactly` failed
/// on the same run**, with *"alpha bravo charlie" ends at 19 and "delta echo foxtrot" starts at 20*:
/// byte 19 was in no row at all.
///
/// **A gap is not cosmetic and it is the whole reason the tiling gate exists.** `row_at_byte` finds a
/// row by `start <= byte`, so a byte in a gap resolves to whichever row is nearest rather than the row
/// it is drawn in — **the caret would sit at the end of one row while the next row's glyphs began a
/// byte further on.** The rows must partition the line.
///
/// ## The space belongs to the row it ends
///
/// **Because that is what the user typed**, it is one cell wide, and the row it ends is exactly the
/// row it belongs to. **The wrap is the invisible thing, not the space.**
#[test]
fn a_row_that_ends_at_a_space_keeps_it_so_the_rows_tile() {
    let text = b"alpha bravo charlie delta echo foxtrot\n";
    let m = WrapMap::build(M, [line(0, text)]);
    let first = m.rows()[0];
    assert_eq!(
        (first.end - first.start, first.cells),
        (20, 20),
        "**twenty bytes and twenty cells**, the last of which is the space at byte 19"
    );
    assert_eq!(
        text[first.end as usize - 1],
        b' ',
        "**and the last byte of the row is that space**"
    );
    assert_eq!(
        m.rows()[1].start,
        first.end,
        "**and the next row starts exactly where this one ended** -- no gap, no overlap"
    );
}

/// **A line's rows tile its bytes exactly, with no gaps and no overlaps.**
///
/// **This is the invariant everything else rests on.** The caret, the highlight and the emitter each
/// walk the rows and assume they partition the line. **A gap is a byte nothing draws and nothing can
/// put a caret on; an overlap is a byte drawn twice.** Both are silent, and one of them shipped in the
/// first version of this rule.
#[test]
fn a_lines_rows_tile_it_exactly() {
    let cases: [&[u8]; 5] = [
        b"short\n",
        b"alpha bravo charlie delta echo foxtrot\n",
        b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
        b"\n",
        b"  leading spaces then words that wrap around the measure here\n",
    ];
    for text in cases {
        let m = WrapMap::build(M, [line(0, text)]);
        let rows = m.rows();
        let content = text.strip_suffix(b"\n").unwrap_or(text);
        let shown = String::from_utf8_lossy(text);
        assert_eq!(
            rows.first().map(|r| r.start),
            Some(0),
            "**a line's first row starts at the line's first byte** for {shown:?}"
        );
        assert_eq!(
            rows.last().map(|r| r.end as usize),
            Some(content.len()),
            "**and its last row ends at the content's end** for {shown:?}"
        );
        for pair in rows.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert_eq!(
                b.start, a.end,
                "**rows tile with no gap and no overlap** for {shown:?}: row ending at {} is \
                 followed by one starting at {}",
                a.end, b.start
            );
        }
    }
}

/// **Every byte is in exactly one row — the "reachable" half of the rule.**
///
/// **A separate assertion from the spans above**, because a row map that lost or duplicated a byte at
/// a *boundary* can produce spans that look right. `covered` counts membership, so a byte in two rows
/// is a 2 and a byte in none is a 0, and both are visible.
#[test]
fn an_unbreakable_token_wraps_hard_and_every_byte_stays_reachable() {
    let text = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n";
    let content = text.strip_suffix(b"\n").expect("has a newline");
    let m = WrapMap::build(M, [line(0, text)]);
    assert_eq!(spans(&m), vec![(0, 20, 20), (20, 40, 20)], "**two rows at the measure**");
    let mut covered = vec![0u32; content.len()];
    for r in m.rows() {
        for i in r.range() {
            covered[i as usize] += 1;
        }
    }
    assert!(
        covered.iter().all(|&c| c == 1),
        "**every content byte is in exactly one row**: {covered:?}. A byte in two rows would be \
         drawn twice; a byte in none would be unreachable. **The newline is excluded**, because no \
         row covers it -- it is the boundary between lines, not content."
    );
}

/// **A multi-byte codepoint is never split across two rows.**
///
/// ## Why the fixture is an emoji and not an accented letter
///
/// **A 2-byte character straddling a boundary is possible and ugly; a 4-byte one straddling it would
/// be an invalid UTF-8 slice**, which the painter would either reject or decode to a replacement glyph.
/// The 4-byte case is the one that is a *correctness* problem, so it is the one worth the test.
#[test]
fn a_multi_byte_codepoint_is_never_split_across_two_rows() {
    // **Twenty-one emoji, not twenty.** Twenty is eighty bytes and exactly twenty cells, so the
    // boundary lands on a cell boundary and the gate would pass trivially. Twenty-one puts the
    // twenty-first character across it.
    let mut text: Vec<u8> = std::iter::repeat_n("😀".as_bytes(), 21)
        .flat_map(|b| b.iter().copied())
        .collect();
    text.push(b'\n');
    let m = WrapMap::build(M, [line(0, &text)]);
    assert_eq!(
        spans(&m),
        vec![(0, 80, 20), (80, 84, 1)],
        "**twenty emoji then one.** `cells` counts codepoints, not bytes: twenty-one emoji is 21 \
         cells and not 84."
    );
    for r in m.rows() {
        let slice = &text[r.range().start as usize..r.range().end as usize];
        assert!(
            std::str::from_utf8(slice).is_ok(),
            "**every row is valid UTF-8 on its own** -- {:?}",
            String::from_utf8_lossy(slice)
        );
    }
}

/// **A multi-byte character in a word-wrapped row costs one cell, and the row's cells say so.**
///
/// **The mixed case, where both rules apply at once.** `mixed 😀 emoji and words that keep going for a
/// while` wraps *after* a space and contains a 4-byte character, so a cell counter that counted bytes
/// would break this line in a different place than a counter that counts codepoints.
#[test]
fn a_row_mixes_word_wrap_and_multi_byte_codepoints() {
    let text = "mixed 😀 emoji and words that keep going for a while\n".as_bytes();
    let m = WrapMap::build(M, [line(0, text)]);
    assert_eq!(
        spans(&m),
        vec![(0, 21, 18), (21, 37, 16), (37, 54, 17)],
        "**three rows of 18, 16 and 17 cells for 21, 16 and 17 bytes.** Row 0 is twenty-one bytes and \
         eighteen cells because of the emoji, which is four bytes and one character."
    );
}

/// **An empty line is one row of zero cells, and it exists.**
///
/// ## Why "it exists" is the assertion
///
/// **A line with no rows has no place to put a caret.** The user pressed Return, there is a blank line
/// on the page, and clicking on it must do something — otherwise the line is invisible as a *target*
/// even though it is visible as a *pixel*.
#[test]
fn an_empty_line_is_one_row_of_zero_cells() {
    let m = WrapMap::build(M, [line(0, b"\n")]);
    assert_eq!(spans(&m), vec![(0, 0, 0)], "**one row, zero bytes, zero cells**");
    assert_eq!(m.len(), 1, "**and the row is there**");
    assert!(
        m.rows()[0].is_empty(),
        "**`is_empty` is true**, which is how a caller sizes a caret box"
    );
}

/// **A line starting with a space does not produce an empty first row.**
///
/// **The `w > row_start` guard, tested directly.** A rule that broke *at* the leading space would make
/// the first row's `end == start`: **zero cells, no glyphs, and a caret box with no width**, on any
/// paragraph someone indented.
#[test]
fn a_leading_space_does_not_produce_an_empty_row() {
    let text = b"  leading spaces then words that wrap around the measure here\n";
    let m = WrapMap::build(M, [line(0, text)]);
    for r in m.rows() {
        assert_ne!(
            r.start, r.end,
            "**no row is empty**: a line beginning with a space must not break *at* it"
        );
    }
    // **And the leading spaces are *drawn*.** Row 0 is `"  leading spaces "` — seventeen bytes and
    // seventeen cells — so the indent is part of the text rather than something the wrap threw away.
    assert_eq!(
        spans(&m),
        vec![(0, 17, 17), (17, 33, 16), (33, 49, 16), (49, 61, 12)],
        "**four rows, the first of which is the indent plus `leading spaces `**"
    );
}

/// **A line of only spaces is one row, and it uses cells.**
///
/// **The case where rule 1 ("every codepoint occupies a cell") and rule 2 ("break at whitespace") pull
/// in opposite directions.** Rule 2 alone would make a line of spaces break at every space and produce
/// `n` rows of zero cells — **blank rows with no glyphs, one per space**, which is a page of empty band
/// and a caret that jumps a row per keystroke.
#[test]
fn a_line_of_only_spaces_is_one_row_that_uses_cells() {
    let m = WrapMap::build(M, [line(0, b"     \n")]);
    assert_eq!(spans(&m), vec![(0, 5, 5)], "**one row of five cells for five spaces**");
}

/// **Offsets are the document's own, so a window that starts mid-document is exact.**
///
/// ## The design this gate protects
///
/// **`Line::start` is supplied by the caller** because a map that counted lines up from zero would be
/// correct for page 1 and wrong for every page after it, by one byte — the newline — per line already
/// seen. **The second half of the gate is the real proof**: the same line built *alone* produces
/// byte-identical rows to its slice of the three-line map.
#[test]
fn an_offset_in_the_second_line_is_exact() {
    let l0 = line_at(0, 0, b"first\n");
    let l1 = line_at(1, 6, b"alpha bravo charlie delta\n");
    let l2 = line_at(2, 32, b"third\n");
    let m = WrapMap::build(M, [l0, l1, l2]);
    assert_eq!(
        spans(&m),
        vec![
            (0, 5, 5),    // `first`
            (6, 26, 20),  // `alpha bravo charlie ` -- twenty cells, so it wraps
            (26, 31, 5),  // `delta`
            (32, 37, 5),  // `third` -- starts at the caller's 32, NOT at 28
        ],
        "**the fourth row starts at the caller's offset 32.** A map that counted lines up from zero \
         would have put it at 28 -- one byte short for every line before it -- and the glyphs would be \
         drawn four bytes away from where the caret sits."
    );
    // **And the window that covers *only* line 1 gives the same rows.** This is the page-40 case, and
    // it is the half that `Line::start` exists for.
    let only = WrapMap::build(M, [l1]);
    assert_eq!(
        only.rows(),
        &m.rows()[1..3],
        "**a map holding only line 1 produces byte-identical rows to the full map's slice of it.** \
         That is the whole reason the caller supplies the offsets."
    );
}

/// **A byte is in exactly one row, so `row_at_byte` needs no flag — and here is the arithmetic.**
///
/// ## What the flag was for, and what happened to it
///
/// `row_at_byte` used to take `wrapping: bool`, on the argument that a byte at a row's `end` is both
/// the last byte of that row and the first byte of the next, and a click wanted one answer while
/// pressing Right wanted the other.
///
/// **That stopped being true when the rows were made to tile.** **Measured over every byte of the
/// longest fixture, the two flag values differed for 22 bytes and every one of the 22 was past the end
/// of the content**, where `wrapping: true` answered `None`. **`None` is the wrong answer there** — a
/// caret at the end of a document is a real position, the one `End` puts it at — so the flag was
/// complexity with a wrong answer on the side that had one, and it was removed.
///
/// ## What this gate holds instead
///
/// **The boundary behaviour directly**: for a wrapping line, the last byte of row 0 is row 0 and the
/// first byte of row 1 is row 1, with nothing in between.
#[test]
fn a_byte_belongs_to_exactly_one_row_and_needs_no_flag() {
    let text = b"alpha bravo charlie delta echo foxtrot\n";
    let m = WrapMap::build(M, [line(0, text)]);
    assert_eq!(m.len(), 2, "**two rows**");
    // **The boundary is byte 20, and both sides resolve to the row they are drawn in.**
    assert_eq!(m.row_at_byte(19), Some(0), "**byte 19 is row 0's last byte**");
    assert_eq!(m.row_at_byte(20), Some(1), "**byte 20 is row 1's first byte**");
    // **And a byte past the content answers the last row, not `None`** — because that is where the
    // caret goes when you press End on the last line.
    assert_eq!(
        m.row_at_byte(38),
        Some(1),
        "**byte 38 is one past the last byte and still answers the last row**"
    );
    assert_eq!(
        m.row_at_byte(u32::MAX),
        Some(1),
        "**and so does `u32::MAX`** -- a caller clamping a caret to the end of the document gets a \
         row, not `None`"
    );
    // **A byte before the window is the first row**, because the map is a window and the caller
    // routinely asks about a byte above it.
    assert_eq!(m.row_at_byte(0), Some(0), "**byte 0 is row 0**");
    assert_eq!(WrapMap::build(M, []).row_at_byte(0), None, "**an empty map answers `None`**");
}

/// **`row_at_byte` is monotonic and in range, which is what makes it safe to scroll by.**
///
/// **Every row it answers is inside `0..len` and it never goes backwards for an increasing byte.** This
/// is not an implementation detail so much as a property of the coordinates, and it is what a caret's
/// scroll-into-view depends on, since that walks backwards through rows.
#[test]
fn row_at_byte_is_monotonic_and_in_range() {
    let text = b"a line that wraps over the twenty cell measure here\n";
    // **Forty copies of a wrapping line**, each with the offset the caller would really have, so the
    // map spans the document rather than one line.
    let lines: Vec<Line<'_>> = (0..40u32)
        .map(|i| Line {
            line: i,
            start: i * u32::try_from(text.len()).unwrap_or(u32::MAX),
            bytes: text,
        })
        .collect();
    let map = WrapMap::build(M, lines);
    assert!(map.len() > 40, "**the map has more rows than lines**, or it is not wrapping");
    let mut last = 0usize;
    for byte in 0..u32::try_from(text.len()).unwrap_or(0) * 40 {
        let i = map
            .row_at_byte(byte)
            .unwrap_or_else(|| panic!("**byte {byte} has a row**"));
        assert!(i < map.len(), "**row {i} is in range** for byte {byte}");
        assert!(
            i >= last,
            "**rows do not go backwards**: byte {byte} gave {i}, the previous byte gave {last}"
        );
        last = i;
    }
}

/// **`line_rows` returns exactly the rows of that line, and nothing of its neighbour's.**
///
/// ## The boundary this is about
///
/// **The two `partition_point`s have different predicates** — `< line` to find the start and `== line` to
/// count them — and the second is the one that is easy to write as `> line`, which would return a
/// **non-empty** range for a line the map does not contain.
///
/// The first draft of this gate expected one row for `"one line that is short"`. **It is 22
/// characters**, so at a measure of 20 it is two, and the gate was wrong by arithmetic rather than by
/// anything the code did.
#[test]
fn line_rows_is_exactly_the_line_s_own_rows() {
    let l0 = line_at(0, 0, b"one line that is short\n");
    let l1 = line_at(1, 23, b"a longer line that wraps over the measure here\n");
    let l2 = line_at(2, 69, b"three\n");
    let m = WrapMap::build(M, [l0, l1, l2]);

    let r0 = m.line_rows(0);
    assert_eq!(r0, 0..2, "**line 0 is 22 characters, so it is two rows at a measure of 20**");
    let r1 = m.line_rows(1);
    assert!(
        r1.len() > 1,
        "**line 1 has more than one row**, or this gate is not testing a boundary: {r1:?}"
    );
    assert!(
        m.rows()[r1.clone()].iter().all(|r| r.line == 1),
        "**and every row in it is line 1's**"
    );
    assert_eq!(m.rows()[r1.start - 1].line, 0, "**the row before it is line 0's**");
    assert_eq!(m.rows()[r1.end].line, 2, "**and the row after it is line 2's**");
    assert_eq!(m.line_rows(2), r1.end..m.len(), "**line 2 is what follows**");
    assert_eq!(
        m.line_rows(99),
        m.len()..m.len(),
        "**and a line the map does not hold is empty** -- which is what `> line` would have broken"
    );
}

/// **A zero measure degrades to one cell rather than dividing or panicking.**
///
/// **The degenerate-layout gate.** A page narrower than one cell cannot be produced by
/// `ChromeMetrics::for_size`, so this is unreachable in the product — **and that is exactly why it
/// needs a gate**: an unreachable panic is one that a layout change turns reachable without anybody
/// looking at this file.
#[test]
fn a_zero_measure_degrades_to_one_cell() {
    let m = WrapMap::build(0, [line(0, b"abc\n")]);
    assert_eq!(m.measure(), 1, "**the measure is one cell**");
    assert_eq!(spans(&m), vec![(0, 1, 1), (1, 2, 1), (2, 3, 1)], "**one cell per row**");
    assert_eq!(
        WrapMap::build(0, []).measure(),
        1,
        "**and an empty map has a usable measure** too, so a caller can divide by it"
    );
}

/// **A row never reports more cells than it has codepoints.**
///
/// **The bound that catches a cell counter that drifted.** Every other gate checks *which* rows exist;
/// this one checks that the number each row reports is a number it could legitimately have — stated as
/// an inequality rather than an equality because a row that breaks *after* a space reports exactly its
/// own codepoint count, and one that breaks *hard* does too, but a rule that counted the break twice
/// would report one more.
#[test]
fn a_row_reports_no_more_cells_than_it_has_codepoints() {
    let cases: [&[u8]; 4] = [
        b"alpha bravo charlie delta echo foxtrot\n",
        b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
        b"  leading spaces then words that wrap around the measure here\n",
        "mixed 😀 emoji and words that keep going for a while\n".as_bytes(),
    ];
    for text in cases {
        let m = WrapMap::build(M, [line(0, text)]);
        for r in m.rows() {
            let bytes = &text[r.range().start as usize..r.range().end as usize];
            let codepoints = std::str::from_utf8(bytes)
                .map(str::chars)
                .map(Iterator::count)
                .unwrap_or(r.len() as usize);
            assert!(
                r.cells as usize <= codepoints,
                "**a row reports {} cells for {} codepoints** ({:?})",
                r.cells,
                codepoints,
                String::from_utf8_lossy(bytes)
            );
        }
    }
}

/// **A continuation is a row that is not its line's first, and that is the whole test.**
///
/// ## What the first version of this gate asked, and why the question was wrong
///
/// It asked whether `Row::is_continuation` distinguished "a wrapped tail" from "a short last row",
/// and the implementation it was testing answered `start_column > 0 && start_column < measure` — which
/// is **false** for the most ordinary continuation there is.
///
/// **`alpha bravo charlie delta echo foxtrot` at a measure of 20** gives a second row starting at
/// column **20**: the first row used every cell, so the continuation begins exactly at the measure and
/// the `< measure` excluded it. The gate failed on its first run for that reason.
///
/// **The distinction it was reaching for does not exist.** A "short last row" and a "continuation" are
/// the same thing: **a row that starts at column 3 has a row above it in its line, so it is a
/// continuation.** There is no non-first row that is not one, which makes `start_column > 0` both
/// sufficient and necessary — **and it needs no `measure` argument at all.**
#[test]
fn a_continuation_is_anything_but_its_lines_first_row() {
    let text = b"alpha bravo charlie delta echo foxtrot\n";
    let m = WrapMap::build(M, [line(0, text)]);
    assert_eq!(m.len(), 2, "**two rows**");
    assert_eq!(
        m.rows()[1].start_column,
        M,
        "**the continuation starts exactly *at* the measure** -- the first row used every cell"
    );
    assert!(
        m.rows()[1].is_continuation(),
        "**and it is a continuation**, which the `< measure` version of this test said it was not"
    );
    assert!(
        !m.rows()[0].is_continuation(),
        "**and a line's first row is not**, whatever its length"
    );

    // **The short-final-row case, which the old reasoning was trying to catch.** A continuation that
    // broke *after a space* rather than at the measure starts well below it — and it is still a
    // continuation, because there is a row above it. `a longer line that wraps over the measure here`
    // gives rows of 19, 15 and 12 cells, so its second row starts at column 19.
    let short_tail = b"a longer line that wraps over the measure here\n";
    let t = WrapMap::build(M, [line(0, short_tail)]);
    assert_eq!(t.len(), 3, "**three rows**");
    assert!(
        t.rows()[1].start_column < M,
        "**its second row starts at column {}**, below the measure of {M}",
        t.rows()[1].start_column
    );
    assert!(
        t.rows()[1].is_continuation(),
        "**and is a continuation** -- being short does not make a row a first row"
    );

    // **And a line that fits is a single row at column 0**, so a full line above a wrapping line cannot
    // make its first row look like a continuation.
    let one = WrapMap::build(M, [line(0, b"short\n")]);
    assert!(
        !one.rows()[0].is_continuation(),
        "**a line that fits is one row at column 0**"
    );
}
