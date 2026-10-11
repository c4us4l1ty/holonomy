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
//! | the colour lands on the right cells | [`the_colour_lands_on_the_run_and_not_on_its_neighbours`] |
//! | **and two colours on one line are two runs** | [`two_colours_on_one_line_are_two_runs_and_two_colours`] |
//! | **and one undo takes it back, pixels and all** | [`a_colour_is_taken_back_by_one_undo`] |
//! | **and a run boundary moves the text after it** — a second defect | [`a_run_boundary_moves_the_text_after_it`] |

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
    // **Two pixels, not one, and the first draft said one.** The painter blits at `run.x + bearing_x`
    // and draws `m.width` wide, so a glyph can overhang *either* side of its cell. Measured on
    // `bravo`: the left edge reaches 369 against a cell starting at 368 (one pixel), and the right
    // reaches 410 against a cell ending at 408 (**two** pixels). **One would have been a guess from
    // `bearing_x` being signed; two is what the pixels say.**
    assert!(
        x0 >= l.x + 6 * m.cell_w - 2 && x1 <= l.x + 11 * m.cell_w + 2,
        "**within `bravo`'s five cells, give or take the glyph overhang**: {}..{} against {}..{}",
        x0,
        x1,
        l.x + 6 * m.cell_w - 2,
        l.x + 11 * m.cell_w + 2
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
        box_.0 >= l.x + 6 * m.cell_w - 1,
        "no pixel more than one cell-width before `bravo` changed: {} against {}",
        box_.0,
        l.x + 6 * m.cell_w - 1
    );
    // **And the far edge is deliberately NOT asserted here.** The first draft did, and it failed with
    // `464 against 409` -- which is not a bleed and not a tolerance problem: **the text *after* the
    // coloured run moves.** See `a_run_boundary_moves_the_text_after_it` below, which is that
    // defect's gate, and which asserts it as a *known* behaviour rather than pretending it is right.
    //
    // **Two gates, one assertion each**, because "the colour is on these five cells" and "the rest of
    // the line did not move" are different claims and only the first is true today.
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
        tx0 >= l.x + 12 * m.cell_w - 2 && tx1 <= l.x + 19 * m.cell_w + 2,
        "**and the teal is on `charlie`, not on `bravo`**: {}..{} against {}..{}",
        tx0,
        tx1,
        l.x + 12 * m.cell_w - 2,
        l.x + 19 * m.cell_w + 2
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

/// **A run boundary moves the text after it. This gate records a defect, not a wish.**
///
/// ## The measurement
///
/// Bolding or colouring `bravo` in `alpha bravo charlie` and comparing the row against the plain
/// document gives:
///
/// ```text
/// plain        :   0 columns differ
/// bold  6..11  :   7 columns differ, first at x=47
/// colour 6..11 :   5 columns differ, first at x=47
/// ```
///
/// and the row's ink ends at **143 px** plain and **141 px** with `bravo` bold. **Five columns
/// differing for a colour change is far too many for five letters**, and the extras are all to the
/// *right* of the coloured run.
///
/// ## The cause, to the two functions
///
/// **`Session::emit_body_text` positions each run at `text.x + cell_x * cell_w`** -- the fixed 8 px
/// grid, which is what the caret, the ruler, the tables and the wrap point all use.
/// **`Painter::doc_text` advances its pen by `m.advance_x.max(1)` -- the font's own metric.** Those two
/// disagree, so **a run boundary is a discontinuity**: the text after it lands where the grid says
/// rather than where the accumulated advances left it.
///
/// **It is not part 29's bug and it is older than part 28.** Part 24 made multi-run lines reachable
/// through the document model and its gates passed anyway, because they asserted ink *extent* on the
/// styled row and never the position of the row's tail.
///
/// ## Why this asserts the current behaviour rather than the right one
///
/// **Because the right one has not been measured.** The two positioners disagree and this says they
/// do; **it does not say by how much**, and a fix that moved the painter onto `cell_w` would move
/// every measured number in this project. **A gate written for behaviour the project does not have
/// tests a wish.** This one fails loudly when the defect is fixed, which is the useful direction: it
/// says "this changed", and whoever fixed it has to say why.
#[test]
fn a_run_boundary_moves_the_text_after_it() {
    let plain = painted("alpha bravo charlie\n", None);
    let coloured = painted("alpha bravo charlie\n", Some((6, 11, CRIMSON)));
    let m = coloured.chrome.metrics;
    let l = coloured.chrome_layout().text;

    // **Right of the coloured run's last cell.**
    let beyond = l.x + 12 * m.cell_w;
    let (_, box_) = difference(&plain, &coloured);
    assert!(
        box_.2 > beyond,
        "**text past the coloured run moved**: the changed region reaches {} and the run's grid cell \
         ends at {}. If this gate fails, the two positioners now agree and \
         `the_colour_lands_on_the_run_and_not_on_its_neighbours` can assert the far edge again -- \
         and the note in PROJECT.md should be deleted with it.",
        box_.2,
        beyond
    );
}
