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
//! | wide glyphs are clipped by the renderer | [`the_renderer_clips_glyphs_wider_than_its_cell_and_this_is_recorded`] |

use holonomy::session::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::{Command, Hotkey, InputEvent, Keymap, ModifierState, KEY_LEFTCTRL, KEY_M};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy};

/// A session over a fresh document, with the real atlas.
fn session(ed: Editor) -> Session<'static> {
    let atlas: &'static holonomy_assets::atlas::Atlas = Box::leak(Box::new(
        holonomy_assets::build_atlas(&[16])
            .expect("build the atlas")
            .0,
    ));
    let m = ChromeMetrics::DESKTOP;
    let scanout = HeadlessScanout::new(m.width, m.height);
    let painter = Painter::new(atlas, 0);
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

    let text = s.editor.text().expect("read the document");
    assert_eq!(
        text, b"x = $$$$",
        "Ctrl+M writes four delimiters, not two, so the formula exists before it is typed into"
    );
    assert_eq!(
        s.editor.caret(),
        6,
        "the caret lands between the two middle `$`, which is where the LaTeX goes"
    );
    assert_eq!(s.stats.math_inserts, 1);

    let span = s.active_math().expect("the caret is inside a formula");
    assert_eq!(span.start, 4);
    assert_eq!(span.end, 8);
    assert!(span.is_empty(), "nothing has been typed into it yet");
    assert!(
        span.contains(s.editor.caret()),
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
    type_into(&mut s.editor, "\\frac{1}{2}");
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
    type_into(&mut s2.editor, "\\sqrt{2}");
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
    type_into(&mut s.editor, "\\fra");
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

/// A fraction is 41 px tall where a line is 18, so the model has to know.
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
        formula_h, 41,
        "18 + 2 pad + 1 bar + 2 pad + 18, which is the number `tests/math.rs` also derives"
    );
    assert!(
        formula_h > m.cell_h,
        "the formula is {formula_h} px and a line is {} px, so this test is about something",
        m.cell_h
    );

    s.apply(ctrl_m()).expect("Ctrl+M");
    type_into(&mut s.editor, "\\frac{1}{2}");
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
    type_into(&mut s.editor, "\\frac{1}{2}");
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
    type_into(&mut s.editor, "\\frac{-b \\pm \\sqrt{b^2 - 4ac}}{2a}");
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
    type_into(&mut s.editor, "\\alpha");
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
    type_into(&mut real.editor, src);
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
    // ink: three `x`-width runs at 10 px spacing leave the last one's right edge 6 px beyond the
    // fixed grid's, which the frame shows as ink further right.
    let m = real.chrome().metrics;
    let text_x = real.chrome().layout.text.x;
    let top = real.chrome().layout.text.y;
    let frame = real.frame();
    let ink_right = (text_x..text_x + 120)
        .rev()
        .find(|&x| {
            (top..top + 40).any(|y| {
                let p = frame.pixel(x, y);
                p != 0
            })
        })
        .expect("the formula drew something");

    // Exactly 28, and the number is derived rather than observed twice.
    //
    // Three glyphs at a 10 px advance sit at x = 0, 10, 20. The painter blits `cell_width()` = 8
    // columns per glyph, not the glyph's own 10, so the last one covers 20..28 and the ink stops
    // there. On the fixed grid it would be three 8 px cells: 0..8, 8..16, 16..24.
    //
    // Asserting the exact value rather than `>=` is what makes this a gate on *both* features at
    // once: 24 says the advance shim is inert, 30 would say the blit uses the real width, and 28 is
    // the answer only if the advance is real *and* the painter's cell truncation is still in place.
    assert_eq!(
        ink_right,
        text_x + 28,
        "ink reaches {ink_right}, {} px into a text column at {text_x}. Expected 28 = two 10 px \
         advances plus the painter's 8-column blit; the fixed grid would give 24",
        ink_right - text_x
    );
    let _ = m;
}

/// The renderer blits every glyph into a fixed-width cell, so a glyph wider than the cell is clipped.
///
/// Recorded here because it is the thing that most limits how a formula looks, and it is **not** a
/// math bug: it is how every glyph on the page is drawn.
///
/// `Painter::text` calls `blit_coverage` with `w = cell_width()` -- `ppem / 2`, so 8 px at 16 ppem --
/// rather than `GlyphMetric::width`. Measured (`holonomy-assets/examples/math_advances.rs`), the ink widths at
/// 16 ppem are 9 px for most Latin letters, 10 px for Greek and operators, and **14 px for `\sum`**.
/// So every glyph loses its right-hand columns to the cell, `\sum` losing 6 of them.
///
/// For body text this is masked: runs advance by the same 8 px, so the next glyph's cell covers the
/// clipped part and the page reads as tight-but-fine. For a formula it is visible, because
/// `math_layout` advances by real advances while the painter still blits a cell.
///
/// **The fix is one line** -- pass `m.width` instead of `cell_w` -- and it is not taken here because
/// it changes the ink of every glyph on the page, which is a visual-baseline change for the whole
/// product rather than a Phase 9B one. It is the top of the 9C list.
#[test]
fn the_renderer_clips_glyphs_wider_than_its_cell_and_this_is_recorded() {
    let atlas = holonomy_assets::build_atlas(&[16]).expect("atlas").0;
    let sum = atlas.metric(0x2211, holonomy_assets::payload::Style::Math, 16);
    assert_eq!(sum.width, 14, "`\\sum`'s ink is 14 px at 16 ppem");
    let cell = 16 / 2;
    assert_eq!(cell, 8, "the painter's cell is ppem / 2");
    assert!(
        sum.width > cell,
        "so {} of its 14 columns are outside the cell the painter blits. See this test's docs: \
         `Painter::text` passes `cell_width()`, not `m.width`",
        sum.width - cell
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
