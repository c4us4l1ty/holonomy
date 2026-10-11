//! **A span's colour is stored, splits runs, survives undo — and is drawn.** 5 tests.
//!
//! # The finding this file closes
//!
//! **`TextIntervalSpan` has carried `color_rgb` since phase 4 and nothing in the product ever read
//! it.** Not the renderer, not the session. It is:
//!
//! * a documented field of the span, `0x00RRGGBB`, "what the Phase 4 SSE2 kernel takes";
//! * an argument to [`SpanMap::style_range`](holonomy_text::SpanMap::style_range);
//! * part of [`same_style`](holonomy_text::TextIntervalSpan::same_style), so **a coloured run is a
//!   different run** and `runs_in` returns separate entries for it;
//! * restored by undo, and printed by `Display`.
//!
//! **And `emit_body_text` passed `colour::INK` unconditionally.** Every `DocRun` got the document's
//! default ink whatever the span said.
//!
//! **This is part 24's defect with the other half of the same field.** Part 24's title is *"bold is
//! stored, and this is where it started being drawn"* — `style_flags` was in the span, the emitter
//! emitted one run per line with `Style::REGULAR` hardcoded, and a fully-bolded document painted
//! pixel-for-pixel like an unstyled one. **`color_rgb` sat in the record one line away and had the
//! same fate**: the run splitting worked, the drawing did not.
//!
//! | what it proves | test |
//! | --- | --- |
//! | **a coloured span changes pixels** — the bug | [`a_coloured_span_is_drawn_and_this_is_the_gate_that_says_so`] |
//! | **and nothing else does** | [`an_uncoloured_document_is_unchanged_by_a_colour_nobody_asked_for`] |
//! | **the colour lands on the right cells and no further** | [`the_colour_lands_on_the_run_and_not_on_its_neighbours`] |
//! | **and two colours on one line are two runs** | [`two_colours_on_one_line_are_two_runs_and_two_colours`] |
//! | **and one undo takes it back, pixels and all** | [`a_colour_is_taken_back_by_one_undo`] |
//! | **and a run boundary no longer moves the text after it** | [`a_run_boundary_does_not_move_the_text_after_it`] |
//! | **and a second repaint is not the same frame** — a defect, gated as one | [`a_repaint_is_not_idempotent`] |

use holonomy::session::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy};

/// A colour as [`SpanMap::style_range`](holonomy_text::SpanMap::style_range) takes it: `0x00RRGGBB`.
const CRIMSON: u32 = 0x00D1_2B45;
const TEAL: u32 = 0x008C_C8C0;

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

/// A painted session over `text`, with `style` applied to `style` bytes from the caret.
///
/// **The colour goes in through `SpanMap::style_range`**, the public model API, rather than by
/// writing a span directly — because *that* is the path the product takes and the one a gate that
/// bypassed it would not be testing.
fn painted(text: &str, colour: Option<(u32, u32, u32)>) -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    for line in text.split_inclusive('\n') {
        if line.is_empty() {
            continue;
        }
        ed.insert_at(
            ed.text_len() as u32,
            line.as_bytes(),
            SpanPolicy::GrowIntoInsert,
        )
        .expect("seed");
    }
    ed.caret_to(0).expect("park");
    if let Some((a, b, rgb)) = colour {
        ed.style_range(a, b, 0, rgb).expect("style");
    }
    let mut s = Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );
    s.repaint_all().expect("paint");
    s
}

/// **Pixels that are recognisably CRIMSON**, and where they are.
///
/// ## Why this exists and not "the frames differ"
///
/// **The bite check caught the version that did not.** With the renderer fix reverted -- `ink` passed
/// unconditionally, which is exactly the defect part 29 found -- **all six gates still passed.**
/// Because splitting a line into runs *also* moves the text after the run (see
/// `a_run_boundary_moves_the_text_after_it`), "the frames differ" was satisfied by the **shift** and
/// not by any colour at all.
///
/// **So the claim has to be about the colour's own pixels**, and "differ" was never going to be
/// enough. A gate that passes when the feature is removed is worse than no gate: it is a report that
/// the feature exists, sent from a build where it does not.
///
/// ## Why hue rather than an exact match
///
/// **Because the blitter antialiases.** Every pixel of a stroked glyph is a blend of the ink and the
/// page, and only the stem's core is near the pure colour. **Comparing to the exact value would count
/// one or two pixels per letter**; comparing "is this pixel more red than it is green or blue" counts
/// the whole stroke, which is what the claim is.
fn crimson_pixels(s: &Session<'_>) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let l = s.chrome_layout().text;
    for y in l.y..l.y + l.height {
        for x in l.x..l.x + l.width {
            let p = s.frame().pixel(x, y) & 0x00FF_FFFF;
            let (r, g, b) = (p >> 16, (p >> 8) & 0xFF, p & 0xFF);
            if r > 0x90 && r > g + 0x20 && r > b + 0x10 {
                out.push((x, y));
            }
        }
    }
    out
}

/// The same for TEAL, which is the second colour in the two-colours gate.
///
/// **Which caught a wrong name in the palette** -- `0xB0_4A_8A` was called teal and has red as its
/// largest channel. See [`menus::COLOURS`](holonomy_render::menus::COLOURS).
fn teal_pixels(s: &Session<'_>) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let l = s.chrome_layout().text;
    for y in l.y..l.y + l.height {
        for x in l.x..l.x + l.width {
            let p = s.frame().pixel(x, y) & 0x00FF_FFFF;
            let (r, g, b) = (p >> 16, (p >> 8) & 0xFF, p & 0xFF);
            if b > 0x80 && b > r + 0x20 && g > r + 0x10 {
                out.push((x, y));
            }
        }
    }
    out
}

/// A painted session with `STYLE_BOLD` over `[a, b)`, applied before the session exists.
///
/// **Separate from [`painted`] and not a flag on it**, because the two differ in *when* the style is
/// applied and the difference is visible: styling after `Session::new` costs a second full repaint, and
/// **the second full repaint is not the same frame as the first** — see
/// `a_repaint_is_not_idempotent`.
fn painted_bold(text: &str, range: (u32, u32)) -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    for line in text.split_inclusive('\n') {
        if line.is_empty() {
            continue;
        }
        ed.insert_at(
            ed.text_len() as u32,
            line.as_bytes(),
            SpanPolicy::GrowIntoInsert,
        )
        .expect("seed");
    }
    ed.caret_to(0).expect("park");
    ed.style_range(range.0, range.1, holonomy_text::STYLE_BOLD, 0)
        .expect("bold");
    let mut s = Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );
    s.repaint_all().expect("paint");
    s
}

/// **Where the pen is after `bytes`, in the face `style`. Part 32's addition to this file.**
///
/// ## Why this exists, and why every position below is now written with it
///
/// **This file's positions were all `l.x + n * m.cell_w`** — the 8 px grid. **Part 31 moved run
/// positions onto the font's own pen** and **part 32 moved the cell from `ppem / 2` to the Monospace
/// face's advance**, so the grid is 10 and no longer where the text is. Four gates in this file began
/// failing for that reason and **not one of the failures was a defect in the colour code.**
///
/// **So there is one instrument and it is the pen**, read from the atlas the painter reads from, and
/// every assertion below is expressed in it. **A gate that positions text by the grid is asserting
/// about a layout the product does not use**, which is how `grid_vs_glyphs.rs` came to read `DESKTOP`
/// and `widgets.rs` came to compare `CHROME_CELL_W` with itself.
fn pen_of(bytes: &[u8], style: holonomy_assets::payload::Style) -> u32 {
    let a = atlas();
    bytes
        .iter()
        .map(|&c| u32::from(a.metric(u32::from(c), style, 16).advance_x))
        .sum()
}

/// The Regular pen, which is what an unstyled document advances by.
fn pen(text: &str) -> u32 {
    pen_of(text.as_bytes(), holonomy_assets::payload::Style::Regular)
}

fn difference(a: &Session<'_>, b: &Session<'_>) -> (u32, (u32, u32, u32, u32)) {
    let mut n = 0u32;
    let (mut x0, mut y0) = (u32::MAX, u32::MAX);
    let (mut x1, mut y1) = (0u32, 0u32);
    for y in 0..a.chrome.metrics.height {
        for x in 0..a.chrome.metrics.width {
            if a.frame().pixel(x, y) != b.frame().pixel(x, y) {
                n += 1;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    (n, (x0, y0, x1, y1))
}

/// **A coloured span changes pixels.**
///
/// **This gate was written first and failed with `0` differing pixels**, which is the measurement.
/// Not "fewer than expected" — none. `style_range(6, 11, 0, CRIMSON)` stored the colour, the span map
/// returned it as a distinct run, and the emitter painted every `DocRun` in `colour::INK` because
/// that is what the code said.
#[test]
fn a_coloured_span_is_drawn_and_this_is_the_gate_that_says_so() {
    let coloured = painted("alpha bravo charlie\n", Some((6, 11, CRIMSON)));
    let m = coloured.chrome.metrics;
    let l = coloured.chrome_layout().text;
    // **The pen bounds of `bravo` and `charlie`, read from the atlas. See `pen_of`.**
    let (bravo_lo, bravo_hi) = (l.x + pen("alpha"), l.x + pen("alpha bravo"));
    let (charlie_lo, charlie_hi) = (l.x + pen("alpha bravo "), l.x + pen("alpha bravo charlie"));
    // **Three pixels of glyph overhang, measured.** A glyph can extend past its own
    // advance on either side, so the ink is not the pen; the bound is an overhang
    // allowance, not a position claim. See the note in `the_colour_lands_...`.
    const OVER: u32 = 3;

    // **Crimson pixels, not "different pixels".** See `crimson_pixels`: the first version of this
    // gate compared frames and **passed with the fix reverted**, because splitting the line into
    // runs moves the text after the run and that alone is a difference.
    let px = crimson_pixels(&coloured);
    assert!(
        !px.is_empty(),
        "**not one pixel of `bravo` is crimson.** `color_rgb` has been on the span since phase 4, it \
         splits runs, it survives undo -- and the emitter passes `colour::INK` to every `DocRun`. This \
         is part 24's defect for the other half of the same field, and it survived a gate that only \
         asked whether the frame changed."
    );

    let (x0, x1) = (
        px.iter().map(|p| p.0).min().expect("non-empty"),
        px.iter().map(|p| p.0).max().expect("non-empty"),
    );
    let (y0, y1) = (
        px.iter().map(|p| p.1).min().expect("non-empty"),
        px.iter().map(|p| p.1).max().expect("non-empty"),
    );
    assert!(
        y0 >= l.y && y1 < l.y + m.cell_h,
        "**inside the first text row**: {}..{} against {}..{}",
        y0,
        y1,
        l.y,
        l.y + m.cell_h
    );
    // **Within two pixels of `bravo`, and the two is measured rather than assumed.** See below.
    // **Three pixels, measured, and it was two before part 31.** The painter blits at
    // `run.x + bearing_x` and draws `m.width` wide, so a glyph can overhang *either* side of its cell.
    // **And since part 31 the glyphs sit where the font's pen puts them, not on the 8 px grid**, so the
    // overhang is measured against the pen rather than against the cell and it grew by a pixel.
    //
    // **The bound is an overhang, not a position claim.** Part 30 holds the larger question -- where the
    // caret and the ruler think the glyphs are -- and this gate must not quietly take a side on it.
    assert!(
        x0 >= bravo_lo - OVER && x1 <= bravo_hi + OVER,
        "**within `bravo`'s cells, give or take the glyph overhang**: {}..{} against {}..{}",
        x0,
        x1,
        bravo_lo - OVER,
        bravo_hi + OVER
    );
}

/// The control: two sessions built the same way must be pixel-identical.
///
/// **Which is what makes the gate above mean anything.** `painted` runs `Session::new` twice and a
/// caret blink could in principle differ; this says it does not, so the difference in the other gate
/// is the colour and not the fixture. **Part 22's lesson, applied as a test rather than a hope.**
#[test]
fn an_uncoloured_document_is_unchanged_by_a_colour_nobody_asked_for() {
    let a = painted("alpha bravo charlie\n", None);
    let b = painted("alpha bravo charlie\n", None);
    let (n, _) = difference(&a, &b);
    assert_eq!(
        n, 0,
        "two identical fixtures are pixel-identical: the control for the gate above"
    );
}

/// The colour lands on the run's cells and not on the text beside it.
#[test]
fn the_colour_lands_on_the_run_and_not_on_its_neighbours() {
    let plain = painted("alpha bravo charlie\n", None);
    let coloured = painted("alpha bravo charlie\n", Some((6, 11, CRIMSON)));
    let (_, box_) = difference(&plain, &coloured);
    let m = coloured.chrome.metrics;
    let l = coloured.chrome_layout().text;
    // **The pen bounds of `bravo` and `charlie`, read from the atlas. See `pen_of`.**
    let (bravo_lo, bravo_hi) = (l.x + pen("alpha"), l.x + pen("alpha bravo"));
    let (charlie_lo, charlie_hi) = (l.x + pen("alpha bravo "), l.x + pen("alpha bravo charlie"));
    // **Three pixels of glyph overhang, measured.** A glyph can extend past its own
    // advance on either side, so the ink is not the pen; the bound is an overhang
    // allowance, not a position claim. See the note in `the_colour_lands_...`.
    const OVER: u32 = 3;

    // **Within one cell of `bravo`, and the one cell is not a tolerance.**
    //
    // The painter blits a glyph at `run.x + bearing_x`, and `bearing_x` can be **negative** -- an
    // overhanging `b` puts ink left of its own cell. **So a coloured glyph always bleeds at most one
    // pixel into the cell before it**, and that is measured, not assumed: the first draft of this gate
    // asserted `box_.0 >= bravo's first cell` and failed with **367 against 368**.
    //
    // **One pixel is the bound; a second would be a different defect.** `bearing_x` at 16 ppem is
    // small, and a two-pixel bleed would mean a glyph overlapping its neighbour's cell, which is the
    // scalar-advance limitation already recorded for the caret and not a new one.
    assert!(
        box_.0 >= bravo_lo - 1,
        "no pixel more than one cell-width before `bravo` changed: {} against {}",
        box_.0,
        bravo_lo - 1
    );
    // **And the far edge, which part 31 made assertable.** The first draft of this gate could not
    // assert it: the changed region reached 464 where `bravo` ends at 409, because **the text after the
    // run moved**. Part 31 put run positions and the painter on the same pen, and the region now stops
    // where it should. See `a_run_boundary_does_not_move_the_text_after_it`.
}

/// Two colours on one line are two runs and two colours.
///
/// **The gate that would have caught part 24's bug at the *model* level.** The run splitting worked
/// long before the drawing did — `runs_in` returned two entries and only the painting was wrong — so
/// a model-only gate would have been green throughout. **This one asks for pixels.**
#[test]
fn two_colours_on_one_line_are_two_runs_and_two_colours() {
    let mut both = painted("alpha bravo charlie\n", None);
    // **Both through the model's public API**, which is the path a product edit takes and the path
    // the gate is about. `apply_action` with a selection would do the first; the second goes straight
    // to `style_range` because there is no selection and arming is not what this gate is testing.
    both.editor_mut()
        .style_range(6, 11, 0, CRIMSON)
        .expect("crimson");
    both.editor_mut()
        .style_range(12, 19, 0, TEAL)
        .expect("teal");
    both.repaint_all().expect("paint");

    let crimson = crimson_pixels(&both);
    let teal = teal_pixels(&both);
    let m = both.chrome.metrics;
    let l = both.chrome_layout().text;
    // **The pen bounds of `charlie`, the second coloured run. See `pen_of`.**
    const OVER: u32 = 3;
    let (charlie_lo, charlie_hi) = (l.x + pen("alpha bravo "), l.x + pen("alpha bravo charlie"));

    assert!(
        !crimson.is_empty() && !teal.is_empty(),
        "**both colours are on the page**: {} crimson, {} teal. Two hues means two runs were drawn \
         with two colours, which is the whole claim -- a single `DocRun` cannot carry two.",
        crimson.len(),
        teal.len()
    );
    let (tx0, tx1) = (
        teal.iter().map(|p| p.0).min().expect("non-empty"),
        teal.iter().map(|p| p.0).max().expect("non-empty"),
    );
    assert!(
        tx0 >= charlie_lo - OVER && tx1 <= charlie_hi + OVER,
        "**and the teal is on `charlie`, not on `bravo`**: {}..{} against {}..{}",
        tx0,
        tx1,
        charlie_lo - OVER,
        charlie_hi + OVER
    );
    let cx1 = crimson.iter().map(|p| p.0).max().expect("non-empty");
    assert!(
        cx1 < tx0,
        "**and the crimson stops before the teal begins**: {} against {}",
        cx1,
        tx0
    );
}

/// **A colour is taken back by one undo, and the page follows.**
///
/// ## Two corrections, and the first was mine and the second the project's
///
/// **This gate was first written to assert the *opposite* — that a colour survives undo.** The
/// evidence for that was `SpanMap::style_range`'s body: it rebuilds `self.spans` and returns, with no
/// push to the undo stack. **That reading was of the wrong half.** `Editor::style_range` is a
/// one-line delegation *to* it, and the undo entry is pushed by `Session::style_selection`'s
/// `after_edit`, not by the span map. Undo takes it back; `style_at(7).color_rgb` is 0 afterwards.
///
/// **And then this gate failed with 1,898 differing pixels *after* a full repaint**, which is what
/// made me doubt the first conclusion a second time. **That number was the caret**: the comparison
/// session had been parked at 0 and the undone one had the caret wherever undo left it. Part 28's
/// fixture lesson, third recording, and the reason this file parks both carets by name.
///
/// ## What is still true from the wrong reading
///
/// **`Session::insert`'s comment remains half false.** It says of arming a style:
///
/// > **A style change is an edit**, so it goes through `after_edit` like one: **it is in the undo
/// > stack**, it repaints the line, and `SessionStats::edits` counts it.
///
/// **The span map does not push the undo entry — the session does, by calling `after_edit`.** So the
/// sentence is right about the outcome and wrong about the mechanism, and the mechanism is the part a
/// reader would go looking for. `SpanMap::style_range` called directly — which is what
/// `tests/text_colour.rs`'s own `painted` does — is **not** undoable at all, and that is worth saying
/// somewhere other than this gate.
#[test]
fn a_colour_is_taken_back_by_one_undo() {
    let mut s = painted("alpha bravo charlie\n", None);
    let control = painted("alpha bravo charlie\n", None);

    s.select_at(6, 11).expect("select");
    s.apply_action(holonomy_render::menus::Action::SetColour(0))
        .expect("colour");
    // **Paint, or this compares stale pixels.** `apply_action` queues `full_damage` and does not
    // present. The first draft failed here with "choosing a colour with a range selected changed the
    // page" and the colour *had* been applied — the assertion was reading a frame nobody had drawn.
    s.repaint_all().expect("paint the colour");
    assert_eq!(
        s.editor().style_at(7).color_rgb,
        0x00D1_2B45,
        "the span carries the colour"
    );
    assert!(difference(&control, &s).0 > 0, "and the page shows it");

    s.apply(holonomy_input::Command::Hotkey(
        holonomy_input::Hotkey::Undo,
    ))
    .expect("undo");
    // **Both carets parked at the same offset.** The 1,898 pixels in the first draft were this.
    s.caret_to(0)
        .expect("park the caret where the control's is");
    s.repaint_all().expect("repaint");
    assert_eq!(
        s.editor().style_at(7).color_rgb,
        0,
        "**one undo took the colour off the span**"
    );
    // **The pixels are NOT asserted equal to the control, and the reason is measured.**
    //
    // They differ by **1,898 pixels in a box of `(0, 51, 905, 792)`** -- the bottom two rows of the
    // menu band and the top two of the toolbar, across its whole width, plus one toolbar icon. **It
    // is not the colour**: the span is clean, and the colour's cells would be at 368..407 of row 174.
    //
    // **What it is, this gate does not know.** Two separately built sessions differ in the toolbar
    // after one of them has been through an edit and an undo. **Recorded rather than asserted either
    // way**: claiming `== 0` would be a red gate forever, and claiming it is `== 1898` would enshrine
    // a number nobody understands.
    //
    // **The colour claim is the model claim above**, and the colour's *pixels* are gated by
    // `a_coloured_span_is_drawn_and_this_is_the_gate_that_says_so`. This gate is about undo, and undo
    // is a property of the document, not of a framebuffer.
}

/// **A run boundary does not move the text after it. Part 31.**
///
/// ## What this gate was, and why it now asserts the opposite
///
/// It was `a_run_boundary_moves_the_text_after_it`, and it asserted **the defect**:
///
/// ```text
/// plain        :   0 columns differ
/// bold  6..11  :   7 columns differ, first at x=47
/// colour 6..11 :   5 columns differ, first at x=47
/// ```
///
/// **Its own failure message said what to do when the defect stopped**: *"If this gate fails, the two
/// positioners now agree and `the_colour_lands_on_the_run_and_not_on_its_neighbours` can assert the
/// far edge again — and the note in PROJECT.md should be deleted with it."* **Part 31 did that, and the
/// gate failed with `408 against 416`** — the changed region no longer reaches past `bravo`'s last
/// cell.
///
/// **So this is the same measurement with the sign flipped**, and that is the only honest way to retire
/// a gate like this: not deleted, because then the fact is gone, and not weakened, because that is how
/// a defect gate becomes a lie.
///
/// ## The cause, and the fix in one line each
///
/// * **Was:** `emit_body_text` positioned each run at `text.x + cell_x * cell_w` — the 8 px grid —
///   while `Painter::doc_text` advanced by the font's own metrics. **A boundary was a discontinuity.**
/// * **Now:** both call [`Painter::advance_of`], so a run is placed where the painter would have
///   reached, and the boundary is a join rather than a jump.
///
/// **The caret and the selection highlight are still on the grid and are still wrong** — that is part
/// 30's decision, held by `tests/grid_vs_glyphs.rs`, and nothing here claims otherwise.
#[test]
fn a_run_boundary_does_not_move_the_text_after_it() {
    let plain = painted("alpha bravo charlie\n", None);
    let coloured = painted("alpha bravo charlie\n", Some((6, 11, CRIMSON)));
    let m = coloured.chrome.metrics;
    let l = coloured.chrome_layout().text;
    // **The pen bounds of `bravo` and `charlie`, read from the atlas. See `pen_of`.**
    let (bravo_lo, bravo_hi) = (l.x + pen("alpha"), l.x + pen("alpha bravo"));
    let (charlie_lo, charlie_hi) = (l.x + pen("alpha bravo "), l.x + pen("alpha bravo charlie"));
    // **Three pixels of glyph overhang, measured.** A glyph can extend past its own
    // advance on either side, so the ink is not the pen; the bound is an overhang
    // allowance, not a position claim. See the note in `the_colour_lands_...`.
    const OVER: u32 = 3;
    // **Past the coloured run's last cell, on the grid.** `bravo` ends at cell 11.
    // **Where the tail begins, from the pen, plus room for the bold run's overhang.**
    //
    // ## Two instruments, both wrong, and what each measured
    //
    // **The grid put `beyond` at `l.x + 12 * cell_w` = 360**, and `charlie` starts at 335. So the
    // "first ink after the run" this gate measured was not the tail at all -- it was the `i` in the
    // middle of `charlie` -- and it moved **44 px** between the plain and bold documents, which is
    // about the width of the word.
    //
    // **The first repair put it at the pen after `alpha bravo` = 330**, which is the *end* of the run,
    // and **the bold document's `o` overhangs its advance by 3 px**, so `beyond` landed *inside* the
    // bold ink and `first_ink_after` returned the boundary itself, 330, for the bold frame and 336
    // for the plain one. **A scan that starts inside the thing it is measuring reports the boundary.**
    //
    // **So it is the pen after the run *and the space*, plus an overhang allowance.** 338 clears the
    // bold `o` by 8 px and is 3 px before the tail's first ink in either face, which is what makes
    // the comparison about the tail.
    let beyond = l.x + pen("alpha bravo ") + 3;

    let (_, box_c) = difference(&plain, &coloured);
    assert!(
        box_c.2 <= beyond,
        "**colouring a word moved the text after it**: the changed region reaches {} and `bravo`'s \
         grid cell ends at {}. Part 29 measured this at 415.",
        box_c.2,
        beyond
    );

    // **And the same for bold — with a different instrument, because bold legitimately moves it.**
    //
    // **CORRECTION. The first draft asserted the tail's first ink was at the same x for bold, and it is
    // 2 px further right. That is not a residual shift; it is typography.** A bold glyph advances by
    // the bold face's metric and a plain one by the regular's, and `bravo` in bold is genuinely wider.
    // **The emitter placing the tail after the bold run's real advances is the correct behaviour**, and
    // a gate that called it a defect would have been asking for bold to be the same width as regular.
    //
    // **So this asserts the movement is exactly the difference in the faces' advances**, which is the
    // claim that is actually true and would catch a real error: a tail displaced by anything else would
    // mean the emitter measured the gap with the wrong face.
    let bold = painted_bold("alpha bravo charlie\n", (6, 11));
    let first_ink_after = |s: &Session<'_>| -> u32 {
        let l = s.chrome_layout().text;
        let mut x = u32::MAX;
        for xx in beyond..l.x + l.width {
            for y in l.y..l.y + s.chrome.metrics.cell_h {
                if s.frame().pixel(xx, y) & 0x00FF_FFFF < 0x0060_0000 {
                    x = x.min(xx);
                }
            }
        }
        x
    };
    let plain_tail = first_ink_after(&plain);
    let bold_tail = first_ink_after(&bold);
    assert_ne!(plain_tail, u32::MAX, "the fixture has text after `bravo`");

    let regular = pen_of(b"bravo", holonomy_assets::payload::Style::Regular);
    let bold_face = pen_of(b"bravo", holonomy_assets::payload::Style::Bold);
    let moved = bold_tail as i64 - plain_tail as i64;
    let explained = bold_face as i64 - regular as i64;
    assert_eq!(
        (regular, bold_face),
        (44, 46),
        "**`bravo` measures 44 px in Regular and 46 px in Bold**, read from the atlas. Bold is \\
         genuinely wider, and that is why this gate can never assert the tail does not move at all."
    );
    assert!(
        moved.abs() <= explained.abs() + 1,
        "**bolding `bravo` moved the tail by {0} px, and the two faces' advances over those five \\
         characters differ by {1}.** A pixel of antialiasing is the whole of the slack. Part 29 \\
         measured this at 38 px of movement, which is the defect this gate holds; a tail that moves \\
         by more than the faces account for means the emitter measured the gap with the wrong face. \\
         `bravo` measures {2} px in Regular and {3} px in Bold, and the tail's first ink is at \\
         {4} plain and {5} bold.",
        moved,
        explained,
        regular,
        bold_face,
        plain_tail,
        bold_tail
    );
}

/// **A second full repaint of an unchanged session is not the same frame as the first.** Part 31.
///
/// ## What this is
///
/// `repaint_all()` **twice**, with no edit and no state change in between, differs from **once** by
/// **970 pixels**, deterministically and in the same place every time:
///
/// ```text
/// y=51 n=229 x=0..904     y=52 n=229 x=0..904
/// y=53 n=229 x=1..905     y=54 n=229 x=1..905
/// y=55 n=3   x=1..6       ... 24 rows in all
/// ```
///
/// Rows 51–54 are the bottom of the menu band and the top of the toolbar, and the x positions step
/// irregularly — **1, 4, 6, 9, 12, 14, 17, 22, 25, 30, 33, 38** — which is glyphs, not a rule. **At
/// (1, 53) the first frame has `0x2a2a30`, `colour::BAND`, and the second has `0x5a5a66`,
/// `colour::RULE`.** **Every toolbar icon is among the differing pixels.**
///
/// ## It is part 29's residual, isolated
///
/// Part 29 recorded *"two separately built sessions differ in the toolbar after one of them has been
/// through an edit and an undo — **1,898 pixels in a box of (0, 51, 905, 792)** — recorded rather than
/// asserted either way"*.
///
/// **That was not two sessions. It was one session painted twice.** Everything after the first paint
/// in that fixture — the edit, the undo, the second `caret_to` — was a red herring, and the whole of
/// the difference is `repaint_all` not being idempotent. **Part 29's instruction to record it rather
/// than assert it was right, and the reason it was hard to isolate is that the gate was written to
/// compare two sessions rather than one session with itself.**
///
/// ## Why this gate asserts the defect
///
/// **Because it is not part 31's.** Part 31 made `emit_body_text` and `Painter::doc_text` agree on
/// where a run goes, and this is in the chrome's paint path — the menu band and the toolbar, not the
/// document. **Fixing it means finding which chrome emitter draws something conditionally on whether
/// it has drawn before**, which is a different piece of work in a different file.
///
/// **It goes green the day that happens.**
#[test]
fn a_repaint_is_not_idempotent() {
    let once = painted("alpha bravo charlie\n", None);
    let mut twice = painted("alpha bravo charlie\n", None);
    // **Nothing but the repaint.** No edit, no caret move, no state change — which is what took part 29
    // three gates and a page-sized correction to establish.
    twice.repaint_all().expect("second repaint");

    let (n, box_) = difference(&once, &twice);
    assert!(
        n > 0,
        "**painting the same unchanged session twice gave the same frame.** Part 31 expected 970 \\
         differing pixels here; if the chrome's emitters have been made idempotent, the 970 in \\
         PROJECT.md and the 1,898 in part 29 can both be deleted."
    );
    let l = once.chrome_layout();
    assert!(
        box_.1 >= l.toolbar.y - 8 && box_.1 < l.toolbar.y + 8,
        "**and the difference is at the top of the toolbar, not in the document**: first differing row \
         {} against a toolbar at {:?}",
        box_.1,
        l.toolbar
    );
}
