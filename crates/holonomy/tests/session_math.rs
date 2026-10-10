//! Phase 9B's session gate: `Ctrl+M`, the compiled/raw switch, and the procedural bars on screen.
//!
//! | requirement | test |
//! | --- | --- |
//! | Ctrl+M inserts a formula with the caret inside | [`ctrl_m_inserts_a_formula_with_the_caret_inside_it`] |
//! | the caret outside compiles, inside shows raw LaTeX | [`a_formula_compiles_only_when_the_caret_is_outside_it`] |
//! | fraction and radical bars are drawn as fills | [`a_compiled_fraction_draws_its_bar_as_a_procedural_fill`] |
//! | a formula that does not parse falls back to raw | [`an_unparseable_formula_falls_back_to_raw_latex_and_says_so`] |
//! | a tall formula displaces the lines below it | [`a_formula_taller_than_a_line_pushes_the_lines_below_it_down`] |
//! | typing in a formula does not reallocate the layout | [`typing_inside_a_formula_does_not_reallocate_the_run_buffer`] |
//! | the raw source is readable back | [`the_session_reports_the_latex_of_the_formula_the_caret_is_in`] |
//! | real advances are live, at the pixel level | [`a_formula_laid_out_on_real_advances_is_wider_than_the_fixed_grid_model`] |
//! | a glyph wider than the cell is blitted at its own width | [`a_glyph_wider_than_the_text_cell_is_blitted_at_its_own_width`] |

use holonomy::session::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::{Command, Hotkey, InputEvent, Keymap, ModifierState, KEY_LEFTCTRL, KEY_M};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy};

/// The one atlas every test in this file shares.
///
/// **One, not one per test.** `Session::new` publishes the atlas into a process-global
/// (`publish_atlas`, and `advance_shim`'s `PUBLISHED_ATLAS`) because `MathMetrics::advance` is a
/// plain `fn` pointer and cannot capture. Building a fresh atlas per test therefore made that global
/// a race between this file's ten tests, running in parallel: whichever session called
/// `Session::new` last owned the global, and every *other* session's `advance_shim` compared its own
/// atlas pointer against a foreign one, found no match, and returned `None` -- so the layout silently
/// fell back to the fixed 8 px grid.
///
/// The visible symptom was `a_formula_laid_out_on_real_advances_is_wider_than_the_fixed_grid_model`
/// failing intermittently with a different ink extent, and passing on a re-run. It is not a flaky
/// test, it is a test that was measuring which of ten racing sessions happened to run last. One
/// shared `OnceLock` makes the global stable and the measurement deterministic.
///
/// `Box::leak` rather than a `static`: the atlas is ~458 KiB of coverage, and a `static` would have
/// to be built at compile time from a `const` expression, which the rasteriser cannot be.
fn shared_atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (atlas, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(atlas))
    })
}

/// A session over a fresh document, with the shared atlas.
fn session(ed: Editor) -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let scanout = HeadlessScanout::new(m.width, m.height);
    let painter = Painter::new(shared_atlas(), 0);
    Session::new(ed, painter, Box::new(scanout), m)
}

/// A session holding `text`, caret at the end.
fn with_text(text: &str) -> Session<'static> {
    let mut ed = Editor::new();
    ed.insert_at(0, text.as_bytes(), SpanPolicy::Strict)
        .expect("seed the document");
    ed.caret_to(text.len()).expect("caret to the end");
    session(ed)
}

/// The `Ctrl+M` keymap dispatch, as the input layer would produce it.
fn ctrl_m() -> Command {
    let mut mods = ModifierState::new();
    mods.update(KEY_LEFTCTRL, 1);
    Keymap::us()
        .dispatch(InputEvent::press(KEY_M), &mods)
        .expect("Ctrl+M is bound")
}

/// Type `s` into `ed` through the real editor, so span bookkeeping happens.
fn type_into(ed: &mut Editor, s: &str) {
    ed.insert_at(ed.caret(), s.as_bytes(), SpanPolicy::GrowIntoInsert)
        .expect("typing succeeds");
}

/// Ctrl+M puts four `$$` in the document and the caret between the two middle ones.
#[test]
fn ctrl_m_inserts_a_formula_with_the_caret_inside_it() {
    let mut s = with_text("x = ");
    s.apply(ctrl_m()).expect("Ctrl+M applies");

    let text = s.text().expect("read the document");
    assert_eq!(
        text, b"x = $$$$",
        "Ctrl+M writes four delimiters, not two, so the formula exists before it is typed into"
    );
    assert_eq!(
        s.caret(),
        6,
        "the caret lands between the two middle `$`, which is where the LaTeX goes"
    );
    assert_eq!(s.stats.math_inserts, 1);

    let span = s.active_math().expect("the caret is inside a formula");
    assert_eq!(span.start, 4);
    assert_eq!(span.end, 8);
    assert!(span.is_empty(), "nothing has been typed into it yet");
    assert!(
        span.contains(s.caret()),
        "and the caret position it left is a position `contains` agrees is inside"
    );
}

/// The whole focused-vs-unfocused rule: caret outside compiles, caret inside shows source.
#[test]
fn a_formula_compiles_only_when_the_caret_is_outside_it() {
    // The formula is typed by driving the session's own commands, so what is tested is the wiring
    // rather than a hand-built document that happens to look right.
    let mut s = with_text("");
    s.apply(ctrl_m()).expect("Ctrl+M");
    for c in "\\frac{1}{2}".chars() {
        s.apply(Command::Insert(c)).expect("type LaTeX");
    }
    assert_eq!(
        s.active_math_source().expect("read").as_deref(),
        Some(&b"\\frac{1}{2}"[..]),
        "every keystroke landed inside the span"
    );

    // Caret is inside: raw source, and the session says so.
    s.paint(None).expect("paint");
    assert_eq!(
        s.stats.math_raw, 1,
        "the caret is inside, so this is raw LaTeX"
    );
    assert_eq!(s.stats.math_compiled, 0, "and not compiled");
    assert_eq!(
        s.stats.math_rules, 0,
        "raw LaTeX has no bars: it is the source, and drawing a fraction bar over `\\frac{{1}}{{2}}` \\
         would be a lie about what the document says"
    );

    // One Right puts the caret past the closing `$$`.
    s.apply(Command::Right).expect("Right");
    assert!(
        s.active_math().is_none(),
        "past the closing delimiter the caret is in ordinary text again"
    );
    s.paint(None).expect("paint");
    assert_eq!(s.stats.math_compiled, 1, "now it compiles");
    assert_eq!(s.stats.math_raw, 0);
    assert_eq!(s.stats.math_parse_errors, 0, "`\\frac{{1}}{{2}}` parses");
    assert_eq!(
        s.stats.math_rules, 1,
        "exactly one procedural fill: the fraction bar. The radical is not in this formula"
    );

    // And back inside.
    s.apply(Command::Left).expect("Left");
    s.paint(None).expect("paint");
    assert_eq!(s.stats.math_raw, 1, "and it collapses back to source");
    assert_eq!(s.stats.math_compiled, 0);
}

/// The fraction bar and the radical's overline are fills, not glyphs.
#[test]
fn a_compiled_fraction_draws_its_bar_as_a_procedural_fill() {
    let mut s = with_text("");
    s.apply(ctrl_m()).expect("Ctrl+M");
    type_into(s.editor_mut(), "\\frac{1}{2}");
    // Park the caret past the closing `$$` with two Rights.
    //
    // **Not** a newline, which was the first attempt and was wrong in an instructive way: a newline
    // leaves the closing `$$` at the start of line 1, where it pairs with the next line's opener and
    // forms a *second* span, this one empty. `math_compiled` then read 2 and the gate's `1` was
    // wrong -- the session was counting an empty formula that is genuinely there.
    s.apply(Command::Right).expect("Right");
    s.apply(Command::Right).expect("Right");
    s.paint(None).expect("paint");

    assert_eq!(
        s.stats.math_compiled, 1,
        "`\\frac{{1}}{{2}}` compiles, and only it"
    );
    assert_eq!(
        s.stats.math_rules, 1,
        "one `Rule` run: the fraction bar. It is a `MathRun::Rule`, which becomes a `Node::Rect` -- \\
         an integer fill with no antialiasing, so it meets the glyphs beside it exactly"
    );

    // A radical adds its overline, so the fill count goes to two for one formula with both.
    let mut s2 = with_text("");
    s2.apply(ctrl_m()).expect("Ctrl+M");
    type_into(s2.editor_mut(), "\\sqrt{2}");
    s2.apply(Command::Right).expect("Right");
    s2.apply(Command::Right).expect("Right");
    s2.paint(None).expect("paint");
    assert_eq!(s2.stats.math_compiled, 1);
    assert_eq!(
        s2.stats.math_rules, 2,
        "the overline and the tick. `\\sqrt` is drawn as a shape, not as U+221A: a fixed glyph is \\
         wrong at every radicand height, and PROJECT.md 2.9.2 point 4 says so"
    );
}

/// A formula that does not parse draws as its own source, and says so.
#[test]
fn an_unparseable_formula_falls_back_to_raw_latex_and_says_so() {
    let mut s = with_text("");
    s.apply(ctrl_m()).expect("Ctrl+M");
    // `\fra` is a real LaTeX command that this grammar does not implement, so it is
    // `UnsupportedCommand` rather than `UnknownCommand` -- see `math.rs`.
    type_into(s.editor_mut(), "\\fra");
    s.apply(Command::Right).expect("Right");
    s.apply(Command::Right).expect("Right");
    s.paint(None).expect("paint");

    assert_eq!(
        s.stats.math_parse_errors, 1,
        "the counter is the only way to tell a deliberate fallback from a formula that quietly \\
         stopped compiling, because both look like source on screen"
    );
    assert_eq!(s.stats.math_compiled, 0, "it does not compile");
    assert_eq!(s.stats.math_raw, 1, "so it is drawn as source");
    assert_eq!(
        s.stats.math_rules, 0,
        "and a half-typed command draws no bars"
    );
}

/// A fraction is taller than a line, so the model has to know.
///
/// **Derived, not literal.** This asserted `formula_h == 41`, which encodes `18` -- and `18` was
/// `ppem + 2`, a number that had nothing to do with the faces. The line pitch is now
/// `Atlas::line_pitch()` (25 px: the tallest packed face's ascent plus descent at 16 ppem), so the
/// same fraction is 55. Pinning either literal would leave the test asserting whatever the pitch
/// happened to be when it was written; pinning `2 * cell_h + 5` is the property, and it survives
/// the pitch changing again.
#[test]
fn a_formula_taller_than_a_line_pushes_the_lines_below_it_down() {
    let mut s = with_text("");
    let m = s.chrome().metrics;
    let mm = holonomy_render::MathMetrics::new(m.cell_w, m.cell_h);
    let formula_h = holonomy_render::measure_math(
        &holonomy_render::parse_math(b"\\frac{1}{2}").expect("parses"),
        &mm,
    )
    .height;
    assert_eq!(
        formula_h,
        m.cell_h * 2 + 5,
        "{cell_h} px line + 2 pad + 1 bar + 2 pad + {cell_h} px line, with cell_h={cell_h}          coming from the atlas's line pitch rather than from `ppem + 2`",
        cell_h = m.cell_h
    );
    assert!(
        formula_h > m.cell_h,
        "the formula is {formula_h} px and a line is {} px, so this test is about something",
        m.cell_h
    );

    s.apply(ctrl_m()).expect("Ctrl+M");
    type_into(s.editor_mut(), "\\frac{1}{2}");
    s.paint(None).expect("paint");

    // The model displaces every line from the anchor **onward, the anchor included** -- that is
    // Phase 9A's paid debt and `LineHeights::from`'s documented behaviour. The first version of
    // this test asserted the opposite (`y(1) == 0`), on the assumption that the anchor line stayed
    // put. It cannot: the formula is drawn *on* that line, so if the line did not move the
    // formula would be drawn over the first line of text after it.
    assert_eq!(
        s.state.line_heights.y(0),
        formula_h,
        "the formula's own line moves down by its height"
    );
    assert_eq!(
        s.state.line_heights.y(1),
        m.cell_h + formula_h,
        "line 1 is one pitch plus the running extras: `y` is pitch * line + extras at or below it"
    );

    // A second formula on line 1 adds to the running total rather than replacing it.
    s.apply(Command::Newline).expect("Newline");
    s.apply(ctrl_m()).expect("Ctrl+M on line 1");
    type_into(s.editor_mut(), "\\frac{1}{2}");
    s.paint(None).expect("paint");
    assert_eq!(
        s.state.line_heights.y(2),
        2 * m.cell_h + 2 * formula_h,
        "two formulas on consecutive lines displace by the sum"
    );
}

/// Editing inside a formula must not reallocate the run buffer.
#[test]
fn typing_inside_a_formula_does_not_reallocate_the_run_buffer() {
    let mut s = with_text("");
    s.apply(ctrl_m()).expect("Ctrl+M");
    type_into(s.editor_mut(), "\\frac{-b \\pm \\sqrt{b^2 - 4ac}}{2a}");
    s.paint(None).expect("paint");
    let after_paint = s.math_run_capacity();
    assert!(
        after_paint >= 64,
        "the quadratic formula lays out to 14 runs and the buffer is sized 64"
    );

    // Compile again after a keystroke, which is the path that would grow it.
    s.apply(Command::Newline).expect("Newline");
    s.paint(None).expect("paint");
    assert_eq!(
        s.math_run_capacity(),
        after_paint,
        "a second compile of the same formula must reuse the allocation"
    );
}

/// The LaTeX the caret is in, for a gate to read.
#[test]
fn the_session_reports_the_latex_of_the_formula_the_caret_is_in() {
    let mut s = with_text("before ");
    s.apply(ctrl_m()).expect("Ctrl+M");
    type_into(s.editor_mut(), "\\alpha");
    assert_eq!(
        s.active_math_source().expect("read").as_deref(),
        Some(&b"\\alpha"[..])
    );

    // Out of the formula: nothing to report, which is a different answer from an empty formula.
    s.apply(Command::Newline).expect("Newline");
    assert_eq!(
        s.active_math_source().expect("read"),
        None,
        "no formula under the caret is `None`, and an empty formula would be `Some(b\"\")`"
    );
}

/// Real advances are actually live, at the pixel level.
///
/// **Why this test is not a counter.** The advance function is the one feature whose absence changes
/// no statistic: `math_compiled`, `math_rules` and the damage rect are all identical whether the
/// layout sits on the page's 8 px grid or on the fonts' real advances. It was inert for a while and
/// every gate still passed, because `advance_shim` was comparing the painter's *size index* against a
/// published *pixel* size, never matched, and returned `None` -- so the layout quietly used the
/// fixed grid while the comments and the module docs described real metrics.
///
/// So this asserts the observable consequence: two glyphs that the fixed grid would overlap are, in
/// fact, drawn with a gap between their ink. If the shim ever stops matching, this fails and no counter
/// moves.
#[test]
fn a_formula_laid_out_on_real_advances_is_wider_than_the_fixed_grid_model() {
    // Three ASCII letters in a row. On the page's 8 px grid they occupy 24 px; JetBrains Mono advances
    // 10 px at 16 ppem, so the real layout reserves 30.
    let src = "abc";
    let mut real = with_text("");
    real.apply(ctrl_m()).expect("Ctrl+M");
    type_into(real.editor_mut(), src);
    real.apply(Command::Right).expect("Right");
    real.apply(Command::Right).expect("Right");
    real.paint(None).expect("paint");

    // The fixed-grid model, straight from the layout module, for comparison.
    let mm = holonomy_render::MathMetrics::new(
        real.chrome().metrics.cell_w,
        real.chrome().metrics.cell_h,
    );
    assert!(
        mm.advance.is_none(),
        "the default is the fixed grid, which is what the hand-computed gate asserts"
    );
    let fixed = holonomy_render::measure_math(
        &holonomy_render::parse_math(src.as_bytes()).expect("parses"),
        &mm,
    );
    assert_eq!(fixed.width, 24, "3 letters x 8 px on the page's grid");

    // Now with the shim. The session does this internally, so the comparison is against the painted
    // ink: three glyphs at a 10 px advance leave the last one's right edge beyond the fixed grid's.
    //
    // **This measures glyph ink, not the chrome's fills.** The previous version searched for any
    // non-zero pixel in rows `top..top + 40`, which is the page background (`0x00FAFAF8`) -- a solid
    // fill -- so it was measuring the right edge of that fill, and its expected value of 28 encoded
    // the painter's old 8-column cell truncation rather than anything about glyphs. That is why the
    // number moved when the width fix landed even though no glyph had changed.
    //
    // Ink is then exactly "neither transparent black nor the page fill", which is a two-colour test
    // rather than a luminance threshold: a threshold of `< 0x808080` matches the canvas, which is
    // black, and reports ink 119 px into the column. The frame's only other colour here is black, so
    // this is exact rather than approximate.
    //
    // The chrome's caret is a third thing in the same rectangle -- `Caret::locate` reads the same
    // `LineHeights`, so a caret just past a compiled formula sits in the formula's own rows -- and it
    // is ink by that two-colour test. It is dropped by exact value, and no glyph pixel can be
    // mistaken for it: glyph ink is a coverage blend of `INK` (0x18181C) into `PAGE` (0xFAFAF8), and
    // matching the caret's red 0x30 pins coverage at (0xFA - 0x30) / (0xFA - 0x18) = 202/226, which
    // puts blue at 0xF8 - 220 * 202/226 = 0x33 rather than the caret's 0x38. A flat fill and a blend
    // of those two colours meet nowhere.
    const CANVAS: u32 = 0x0000_0000;
    const PAGE: u32 = 0x00FA_FAF8;
    let caret_fill = holonomy_render::chrome::colour::CARET & 0x00FF_FFFF;
    let m = real.chrome().metrics;
    let text_x = real.chrome().layout.text.x;
    let top = real.chrome().layout.text.y;
    let frame = real.frame();

    // # The window is the formula's line box, and the caret is dropped by exact value
    //
    // `LineHeights::from` adds a block's height to `y(line)` for every `l <= line`, so a line
    // holding a formula-tall block starts *after* its own block: the caret and the formula both live
    // in `[text.y + cell_h, text.y + 2 * cell_h)`. `Caret::locate` and `emit_math` read that same
    // model, which is what makes this the box rather than a guess, and it is the box
    // `Painter::text`'s vertical placement promises to fill.
    //
    // The previous window was `text.y - cell_h .. text.y + cell_h`, written when `bearing_y` put ink
    // 17-21 px *above* its box. The fix put the ink inside the box, so that window now misses it
    // entirely -- the fix working, not a test that cannot fail: ink escaping the box in either
    // direction still leaves this scan empty and fails at the `.expect` below.
    let box_top = top + m.cell_h;
    let rows = box_top..box_top + m.cell_h;
    let inked = |x: u32, y: u32| {
        let p = frame.pixel(x, y);
        p != CANVAS && p != PAGE && p != caret_fill
    };
    // Scanned from the text column's left edge, not from 0: the ruler draws vertical rules at
    // `text.x` and `text.right()`, and a search that started off-column would find the rule rather
    // than the formula.
    let ink_right = (text_x..text_x + 60)
        .rev()
        .find(|&x| rows.clone().any(|y| inked(x, y)))
        .expect("the formula drew something");

    // Exactly 29 px of inked extent, and the number is derived rather than observed twice.
    //
    // Three glyphs at a 10 px advance sit at x = 0, 10, 20, and the painter blits each glyph's own
    // ink width -- `m.width`, measured 9-11 px for Inter Italic at 16 ppem, not the 8 px cell. The
    // last glyph, `c`, therefore covers columns 20..29 relative to the text column.
    //
    // The rightmost of those is the rasteriser's **1 px antialiasing pad** (`raster.rs` takes
    // `max_x.ceil() + 1` so edge coverage is not clipped), and a pad column has zero coverage. So
    // the rightmost *inked* column is 28, and that is what this asserts. The pad is why the number
    // is 29 and not 30; it is a real property of the atlas, not slack in the assertion.
    //
    // Asserting the exact value rather than `>=` is what makes this a gate on *both* features at
    // once, and all three failure modes stay distinguishable:
    //
    // | rightmost ink | meaning |
    // |---|---|
    // | text_x + 23 | fixed grid **and** the painter truncating to its cell -- today's two bugs together |
    // | text_x + 24 | fixed grid, real-width blit: the advance shim is inert |
    // | text_x + 28 | real advances and real-width blit: correct |
    //
    // So a regression in either the advance shim or the blit fails here, and neither can hide behind
    // the other.
    assert_eq!(
        ink_right,
        text_x + 28,
        "ink reaches {ink_right}, {} px into a text column at {text_x}. Expected 29 px of inked \
         extent = three 10 px advances with the painter blitting each glyph's own width, less the \
         rasteriser's 1 px pad column. A fixed grid gives 24 and a truncating blit gives 23",
        ink_right - text_x
    );
}

/// The renderer blits every glyph into a fixed-width cell, so a glyph wider than the cell is clipped.
///
/// Recorded here because it is the thing that most limits how a formula looks, and it is **not** a
/// math bug: it is how every glyph on the page is drawn.
///
/// `Painter::text` calls `blit_coverage` with `w = cell_width()` -- `ppem / 2`, so 8 px at 16 ppem --
/// The painter blits a glyph at its own ink width, so a glyph wider than the text cell is not clipped.
///
/// **This replaces a test that asserted the opposite.** Phase 9B recorded the limitation instead of
/// fixing it: `the_renderer_clips_glyphs_wider_than_its_cell_and_this_is_recorded` asserted that
/// `\sum`'s 14 px of ink exceeded the 8 px cell the painter blitted, and documented "the fix is one
/// line -- pass `m.width` instead of `cell_w` -- and it is not taken here". Phase 9C took it, so that
/// test's claim became false and asserting it would have pinned the bug in place.
///
/// `\sum` is the right glyph to gate on because it is the worst case in the atlas: 14 px of ink in an
/// 8 px cell, so a truncating blit loses 6 columns. Inter's letters are 9-11 px and lose only 1-3,
/// which is a weak signal -- see `math_advances.rs`, which prints the whole table.
///
/// The expected extent is derived, not observed: the metric is 14 px wide including `raster.rs`'s
/// 1 px pad on each side, so the inked columns are 1..=12 relative to the pen and the rightmost is
/// `text_x + 12`. A cell-truncating blit would stop at `text_x + 6`.
#[test]
fn a_glyph_wider_than_the_text_cell_is_blitted_at_its_own_width() {
    let sum = shared_atlas().metric(0x2211, holonomy_assets::payload::Style::Math, 16);
    assert_eq!(sum.width, 14, "`\\sum`'s ink is 14 px at 16 ppem");
    let cell = 16 / 2;
    assert_eq!(cell, 8, "the painter's cell is ppem / 2");
    assert!(
        sum.width > cell,
        "so this glyph is {} px wider than the cell, which is what makes it a usable gate",
        sum.width - cell
    );

    let mut s = with_text("");
    s.apply(ctrl_m()).expect("Ctrl+M");
    type_into(s.editor_mut(), "\\sum");
    s.apply(Command::Right).expect("Right");
    s.paint(None).expect("paint");
    assert_eq!(s.stats.math_compiled, 1, "`\\sum` should have compiled");

    const CANVAS: u32 = 0x0000_0000;
    const PAGE: u32 = 0x00FA_FAF8;
    let caret_fill = holonomy_render::chrome::colour::CARET & 0x00FF_FFFF;
    let cell_h = s.chrome().metrics.cell_h;
    let text_x = s.chrome().layout.text.x;
    let top = s.chrome().layout.text.y;
    let frame = s.frame();
    // The formula's line box and the caret's exclusion: see the same construction in
    // `a_formula_laid_out_on_real_advances_is_wider_than_the_fixed_grid_model`, which spells out why
    // the window is `[text.y + cell_h, text.y + 2 * cell_h)` and not `text.y +/- cell_h`.
    let rows = (top + cell_h)..(top + cell_h + cell_h);
    let inked = |x: u32, y: u32| {
        let p = frame.pixel(x, y);
        p != CANVAS && p != PAGE && p != caret_fill
    };
    let rightmost = (text_x..text_x + 40)
        .rev()
        .find(|&x| rows.clone().any(|y| inked(x, y)))
        .expect("`\\sum` drew something");
    assert_eq!(
        rightmost,
        text_x + 12,
        "`\\sum`'s ink reached {rightmost}, {} px past the pen at {text_x}. Expected 12 = its 14 px \
         metric width less the 1 px pad on each side; a blit truncated to the 8 px cell would stop \
         at {}. See `Painter::text`.",
        rightmost - text_x,
        text_x + 6
    );
}

/// The keymap's own binding, asserted where it is defined.
#[test]
fn the_keymap_binds_ctrl_m_to_insert_math() {
    let mut mods = ModifierState::new();
    mods.update(KEY_LEFTCTRL, 1);
    assert_eq!(
        Keymap::us().dispatch(InputEvent::press(KEY_M), &mods),
        Some(Command::Hotkey(Hotkey::InsertMath))
    );
}
