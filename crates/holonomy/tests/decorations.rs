//! **Underline, strikethrough, superscript and subscript, armed and drawn.** 12 tests.
//!
//! # The gap this file closes
//!
//! Phase 6 added four decoration bits to `TextIntervalSpan` (`STYLE_UNDERLINE`, `STYLE_STRIKE`,
//! `STYLE_SUPERSCRIPT`, `STYLE_SUBSCRIPT`), and until this pass **nothing drew them**. The bits were
//! representable, they round-tripped through the payload, `format_range` would write them — and a
//! paragraph with every word underlined painted *pixel for pixel identical* to the same paragraph plain.
//! That is the same shape as part 24's bold bug and part 29's colour bug, which is the point: **a bit
//! the model can hold and the renderer ignores is indistinguishable from a bit the model does not
//! have**, and only a gate that compares two frames can tell them apart.
//!
//! # The three mechanisms, and why they are three
//!
//! | decoration | how it is drawn | why not another way |
//! |---|---|---|
//! | underline | a 2 px rule under the run, `Node::Rule` | a face would be new atlas bytes and still no line |
//! | strikethrough | a 2 px rule through the run | same |
//! | superscript | the baseline moved up a third of the line box | a smaller face needs a third atlas size, which `AtlasBuilder` refuses |
//! | subscript | the same, down | same |
//!
//! **`DocRun` carries a `decorations` bitset and a signed `baseline_shift`**, so the *painter* owns where
//! the rule goes and how far the glyphs move — the emitter passes bits, not geometry. An emitter that
//! computed an underline's `y` would be a second answer to "where is the baseline", which is the defect
//! `pen_of` exists to prevent, one axis over.
//!
//! **The size of the rule is shared and its position is not.** `DocRun::RULE_THICKNESS` is the request,
//! because "how thick is an underline" has the same answer in every renderer; `Painter::rule_thickness`
//! is a floor the painter may raise, because only the painter knows the line box the rule has to fit
//! inside. **A position cannot be shared that way**, which is why there is no `RULE_OFFSET` beside it: the
//! offset is the ascent, and the ascent belongs to the atlas.
//!
//! # What is measured, and why not "did anything change"
//!
//! **Ink extents, relative to the baseline.** "The rule is under the glyphs and not through them" cannot
//! be checked by counting differing pixels — a rule drawn *through* the letters differs from plain text by
//! a similar *number* of pixels as one drawn under them. What separates them is the **shape** of the
//! difference: an underline extends the box's bottom and leaves its top alone, a strikethrough stays
//! inside the box, and a shift moves both edges together by the same amount.
//!
//! **So every gate here compares box edges between two frames**, and the two that cannot be answered that
//! way — "was a glyph drawn" and "how many rules were asked for" — read `PaintStats` instead.
//!
//! **The window is one line taller than the text rect in each direction**, because a raised run is *not
//! inside its own line box*: a measurement clipped to `cell_h` would report a superscript's ink as
//! starting at the top of the box, which is exactly the number the superscript gate needs to see move.
//!
//! | what it proves | test |
//! | --- | --- |
//! | **the underline draws, under the glyphs** | [`an_underlined_run_draws_a_rule_below_its_glyphs`] |
//! | **and it costs one `Node::Rule`, not glyphs** | [`the_underline_is_one_rule_and_no_extra_glyph`] |
//! | the strikethrough draws through | [`a_struck_run_draws_a_rule_through_its_glyphs`] |
//! | a superscript moves the run up | [`a_superscript_moves_its_run_up_the_screen`] |
//! | a subscript moves it down | [`a_subscript_moves_its_run_down_the_screen`] |
//! | clear formatting undoes all of them | [`clearing_formatting_removes_every_decoration`] |
//! | and stops at the end of the last selected line | [`clearing_stops_at_the_end_of_the_last_selected_paragraph`] |
//! | **underline arms at a caret and types underlined** | [`arming_underline_styles_the_text_typed_after_it`] |
//! | **a strike arms without lighting a button** | [`the_strikethrough_arms_without_lighting_any_button`] |
//! | **a decoration is not a face** | [`a_decoration_is_not_a_face`] |
//! | a heading draws at the larger size | [`a_heading_draws_at_the_larger_atlas_size`] |
//! | and its underline follows its baseline | [`a_headings_underline_is_where_a_headings_baseline_is`] |

use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{
    Editor, SpanPolicy, STYLE_BOLD, STYLE_HEADER, STYLE_ITALIC, STYLE_STRIKE, STYLE_SUBSCRIPT,
    STYLE_SUPERSCRIPT, STYLE_UNDERLINE,
};

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        // **Two sizes**, because one of these gates is about a heading and a heading is the second
        // size. A one-size atlas clamps `run_size_index` to 0 and the heading gate would then be
        // asserting that a heading draws at the body size, which is the bug it exists to catch.
        let (a, _) = holonomy_assets::build_atlas(&[16, 20]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

/// A session over `text` with `range` styled by `flags`, painted once, at the default atlas size.
///
/// **`Painter::new(atlas(), 0)` is what the product does**, so every gate that is not about zoom or
/// headings uses this one and measures what a user sees at 100 %.
fn painted(text: &str, style: Option<(u32, u32, u16)>) -> Session<'static> {
    painted_at(text, style, 0)
}

/// The same, with the painter's size index selectable.
///
/// **The heading gates need size index 1**, and the reason is a real trap: `Painter::size_index` is 0
/// until someone calls `set_size_index`, and the painter's *cell* geometry follows it — so a session
/// that never zoomed measures its lines on the body pitch and a `run_size_index` of 1 draws 20 ppem
/// glyphs on 18 px leading. A gate that painted the heading at index 0 would be asserting that a heading
/// draws at the body size, which is the defect it exists to catch.
fn painted_at(text: &str, style: Option<(u32, u32, u16)>, size: u8) -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    ed.insert_at(0, text.as_bytes(), SpanPolicy::GrowIntoInsert)
        .expect("room");
    if let Some((lo, hi, flags)) = style {
        ed.style_range(lo, hi, flags, 0).expect("style the run");
    }
    let mut s = Session::new(
        ed,
        Painter::new(atlas(), size),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );
    s.caret_to(0).expect("park the caret");
    s.repaint_all().expect("paint");
    s
}

/// **The three armed bits from the model's side**, as a `(bold, italic, underline)` tuple.
///
/// `Session::armed_style` is private, so a gate reads `pending_style()` and does the mask itself — and
/// that is the *same* arithmetic in the same order, which is what makes a disagreement with the chrome's
/// copy (read from `state.styles`) a failure rather than a difference of opinion about which copy to
/// believe.
fn armed(s: &Session<'_>) -> (bool, bool, bool) {
    let f = s.pending_style().unwrap_or(0);
    (
        f & STYLE_BOLD != 0,
        f & STYLE_ITALIC != 0,
        f & STYLE_UNDERLINE != 0,
    )
}

/// **The chrome's three bits**, as the same tuple.
///
/// **Neither reader is the authority**; the claim is that the two *agree*, and a gate that read only one
/// of them could not check that at all.
fn chrome_armed(s: &Session<'_>) -> (bool, bool, bool) {
    (
        s.state.styles.bold,
        s.state.styles.italic,
        s.state.styles.underline,
    )
}

/// **The window a decoration is measured in: one line above the text rect and one below it.**
///
/// `(x0, y0, x1, y1)` inclusive. **A raised run is not inside its own line box**, so a measurement
/// clipped to `cell_h` from the text rect's top would report a superscript's ink as *starting at the top
/// of the box* — which is exactly the number the superscript gate needs to see move. The window is
/// therefore `cell_h` taller at both ends, and it is clipped to the frame.
fn window(s: &Session<'_>) -> (u32, u32, u32, u32) {
    let l = s.text_rect();
    let cell_h = s.chrome.metrics.cell_h;
    let (w, h) = s.frame_size();
    (
        l.x,
        l.y.saturating_sub(cell_h),
        (l.x + l.width).min(w),
        (l.y + 2 * cell_h).min(h),
    )
}

/// The ink extent inside [`window`]: `(top, bottom, left, right)`, or `None` if nothing drew.
///
/// **All four edges, because the four decorations move three different ones.** An underline extends the
/// bottom, a strike stays inside the glyphs, and a shift moves the top *and* the bottom together — so a
/// measurement that only looked at the bottom could not tell an underline from a subscript.
fn ink_box(s: &Session<'_>) -> Option<(u32, u32, u32, u32)> {
    let (x0, y0, x1, y1) = window(s);
    let (mut top, mut left, mut right, mut bottom) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in y0..y1 {
        for x in x0..x1 {
            if s.frame().pixel(x, y) != 0 {
                top = top.min(y);
                bottom = bottom.max(y);
                left = left.min(x);
                right = right.max(x);
            }
        }
    }
    (top != u32::MAX).then_some((top, bottom, left, right))
}

/// Every pixel inside [`window`] that differs between two frames, as a count.
///
/// **Culled to the page's text column**, so a difference anywhere in the chrome would be invisible here —
/// and that is deliberate: these gates are about type, and a gate that counted chrome pixels would pass
/// because the clock moved. `style_render.rs`'s `every_styled_byte_was_drawn_in_the_bold_face` is the
/// file that asserts the difference stays on the page at all.
fn differing(s: &Session<'_>, other: &Session<'_>) -> u32 {
    let (x0, y0, x1, y1) = window(s);
    let mut n = 0u32;
    for y in y0..y1 {
        for x in x0..x1 {
            if s.frame().pixel(x, y) != other.frame().pixel(x, y) {
                n += 1;
            }
        }
    }
    n
}

/// **An underlined run draws a rule, and the rule is below the glyphs.**
///
/// **The bottom edge is the measurement**, not the pixel count. A rule drawn *through* the letters
/// differs from plain text by a similar number of pixels as one drawn under them, so "something
/// changed" cannot distinguish an underline from a strikethrough — but "the lowest ink moved down by a
/// couple of pixels while the top stayed put" can.
#[test]
fn an_underlined_run_draws_a_rule_below_its_glyphs() {
    let plain = painted("under\n", None);
    let under = painted("under\n", Some((0, 5, STYLE_UNDERLINE)));
    assert!(
        differing(&plain, &under) > 0,
        "**the frames are identical**: an underlined word paints exactly like a plain one, which is \\
         the state of this build before the decorations were drawn at all"
    );

    let (pt, pb, _, _) = ink_box(&plain).expect("plain text has ink");
    let (ut, ub, _, _) = ink_box(&under).expect("underlined text has ink");
    assert_eq!(
        ut, pt,
        "the underline must not move the glyphs: the top of the ink was {pt} and is {ut}"
    );
    assert!(
        ub > pb,
        "**the rule must be under the glyphs**: the lowest ink was {pb} plain and is {ub} underlined, \\
         so no rule was drawn below the descenders"
    );
}

/// **The underline is one `Node::Rule` and adds no glyph.**
///
/// **The pixel test above cannot see this.** A version that "underlined" the text by drawing a run of
/// `_` characters would pass it — and would corrupt the document's width, its selection and its copy,
/// because the underscores would be *text*. So this counts what the painter was asked for: one rule, and
/// the same number of document glyphs as the plain frame.
#[test]
fn the_underline_is_one_rule_and_no_extra_glyph() {
    let plain = painted("under\n", None);
    let under = painted("under\n", Some((0, 5, STYLE_UNDERLINE)));

    let rules = |s: &Session<'_>| s.paint_stats().rules;
    let glyphs = |s: &Session<'_>| s.paint_stats().doc_glyphs;
    // **`plain` has no rules**, which is what makes the next assertion mean "one more than nothing"
    // rather than "one more than whatever the chrome draws".
    assert_eq!(
        rules(&plain),
        0,
        "a document with no decoration asks the painter for no rule at all -- if the page chrome drew \
         one, the underlined count below would be measuring two things"
    );
    assert_eq!(rules(&under), 1, "five underlined bytes are one run, so one rule");
    assert_eq!(
        glyphs(&under),
        glyphs(&plain),
        "**the underline must not draw characters**: an implementation that emitted `_` would pass \\
         the pixel gate and would be a document whose text changed"
    );
}

/// **A struck run draws its rule through the glyphs, not under them.**
///
/// **The measurement that separates the two rules.** A strikethrough sits just above the baseline and an
/// underline just below it, so on the same word the strike's ink box is *taller at the top* and *not
/// taller at the bottom*, which is the exact opposite of the underline's. A gate that only counted
/// differing pixels would pass either for both.
#[test]
fn a_struck_run_draws_a_rule_through_its_glyphs() {
    let plain = painted("struck\n", None);
    let strike = painted("struck\n", Some((0, 6, STYLE_STRIKE)));
    assert!(
        differing(&plain, &strike) > 0,
        "the strikethrough drew nothing"
    );

    // **The difference is inside the glyphs' own rows.** Everything that changed must sit between the
    // plain frame's top and bottom of ink -- a strike is *within* the letters, so it cannot extend the
    // box the way an underline does.
    let (pt, pb, pl, pr) = ink_box(&plain).expect("plain text has ink");
    // **The whole window, not just the line box**, because that is the assertion: a rule that leaked into
    // the row above or below is exactly as wrong as one that leaked past the glyphs horizontally, and a
    // scan clipped to `cell_h` could not see it.
    let (x0, y0, x1, y1) = window(&plain);
    let mut outside = 0u32;
    for y in y0..y1 {
        for x in x0..x1 {
            if plain.frame().pixel(x, y) != strike.frame().pixel(x, y)
                && (y < pt || y > pb || x < pl || x > pr)
            {
                outside += 1;
            }
        }
    }
    assert_eq!(
        outside, 0,
        "**the strikethrough must be inside the glyphs' own box** (x {pl}..={pr}, y {pt}..={pb}) \\
         and {outside} pixel(s) of it are not. A rule that extended the box would be an underline, \\
         which is drawn deliberately below the descenders"
    );
}

/// **A superscript moves its run up the screen, and moves nothing else.**
///
/// **The fixture is one word, styled whole**, so the shift is the only difference. A mixed line would let
/// a bug that moved the *line* look like a bug that moved the *run*.
#[test]
fn a_superscript_moves_its_run_up_the_screen() {
    let plain = painted("raised\n", None);
    let sup = painted("raised\n", Some((0, 6, STYLE_SUPERSCRIPT)));
    assert!(differing(&plain, &sup) > 0, "the superscript moved nothing");

    let (pt, pb, _, _) = ink_box(&plain).expect("plain text has ink");
    let (st, sb, _, _) = ink_box(&sup).expect("raised text has ink");
    assert!(
        st < pt,
        "**a superscript rises**: the top of the ink was {pt} and is {st}, so the glyphs did not move up"
    );
    // **And it stays a shift rather than a different face.** The box keeps its height to within a pixel
    // or two of antialiasing -- a version that dropped to a smaller face would shrink it.
    let plain_h = pb - pt;
    let sup_h = sb - st;
    assert!(
        sup_h.abs_diff(plain_h) <= 2,
        "the run changed size, not position: ink height {plain_h} plain and {sup_h} raised"
    );
}

/// **A subscript moves its run down the screen.**
///
/// The counterpart, and it is a separate gate rather than a signed parameter: the sign is the whole of
/// the difference between the two bits, and a version that used `abs()` would pass the superscript gate
/// and fail this one. **`0 - third` and not `-third.abs()`** is the line in `Session::baseline_shift`
/// this holds.
#[test]
fn a_subscript_moves_its_run_down_the_screen() {
    let plain = painted("lowered\n", None);
    let sub = painted("lowered\n", Some((0, 7, STYLE_SUBSCRIPT)));

    let (pt, pb, _, _) = ink_box(&plain).expect("plain text has ink");
    let (st, sb, _, _) = ink_box(&sub).expect("lowered text has ink");
    assert!(
        sb > pb,
        "**a subscript sinks**: the lowest ink was {pb} and is {sb}"
    );
    assert!(
        st >= pt,
        "and the top sinks with it rather than the glyphs growing: {pt} -> {st}"
    );
}

/// **Clear formatting removes every decoration at once, and it is `Action::ClearFormatting` that does it.**
///
/// **The bitset's own gate.** Each of the four decorations is a different mechanism inside the painter,
/// so removing them one at a time is four chances to leave one behind — and a leftover strikethrough on
/// text the user asked to have no formatting at all is the kind of bug nobody reports as a bug. This
/// styles every bit onto a paragraph, clears it through the *menu action*, and asserts the span is plain
/// and that the pixels are the plain frame's.
#[test]
fn clearing_formatting_removes_every_decoration() {
    use holonomy_render::menus::Action;
    // **Paragraph-wide, so the caret's line is the thing cleared.** An empty selection means "this
    // paragraph", which is what the action does and what every word processor does.
    let mut s = painted(
        "cleared\n",
        Some((
            0,
            7,
            STYLE_UNDERLINE | STYLE_STRIKE | STYLE_SUPERSCRIPT | STYLE_SUBSCRIPT,
        )),
    );
    assert!(
        differing(&s, &painted("cleared\n", None)) > 0,
        "the fixture is wrong: the decorated document paints like the plain one, so clearing it proves \\
         nothing"
    );

    s.caret_to(0).expect("caret");
    s.apply_action(Action::ClearFormatting).expect("clear");
    s.repaint_all().expect("paint");

    assert_eq!(
        s.editor().style_at(0).style_flags,
        0,
        "every bit must be gone, and `style_at` is the model's own answer"
    );
    assert_eq!(
        differing(&s, &painted("cleared\n", None)),
        0,
        "**the cleared document must paint exactly like one that was never styled**"
    );
    // **And nothing is armed for the next character.** Leaving `pending_style` set would make the next
    // word underlined in a paragraph the user just asked to have no formatting at all.
    assert_eq!(
        s.pending_style(),
        None,
        "clearing must leave the model saying nothing, not saying plain"
    );
}

/// **Underline arms at a bare caret like bold does, and it types underlined text.**
///
/// **The half of the underline that is not a drawing problem.** `toggle_inline_style` has two branches —
/// a selection is restyled, a caret is *armed* — and the arming one is where a decoration with no toolbar
/// button would have been lost in silence. This asserts the arming, the chrome's copy agreeing with the
/// model, the inserted text carrying the bit, and the pixels changing — **the rest of this file styles
/// text that is already there**, so none of it would notice an arming path that armed nothing.
///
/// **The tuple is asserted whole and not just its third element**, because arming underline through
/// `set_armed_style` rewrites all three bits at once: a version that wrote `bold` where it meant
/// `underline` lights the wrong button and types the wrong face, and `== (false, false, true)` is the only
/// spelling of the assertion that sees it.
#[test]
fn arming_underline_styles_the_text_typed_after_it() {
    use holonomy_render::menus::Action;
    let mut s = painted("ab\n", None);
    s.caret_to(2).expect("caret after the two letters");
    s.apply_action(Action::ToggleUnderline).expect("arm underline");

    assert_eq!(
        armed(&s),
        (false, false, true),
        "the model must say underline and nothing else"
    );
    assert_eq!(
        armed(&s),
        chrome_armed(&s),
        "**the two copies of the armed flags disagree**: the model says {:?} and the chrome says {:?}, \
         which is a button that lights up and types plain text",
        armed(&s),
        chrome_armed(&s)
    );

    s.insert_text("c").expect("type");
    assert_eq!(
        s.editor().style_at(2).style_flags & STYLE_UNDERLINE,
        STYLE_UNDERLINE,
        "the typed character must carry the armed bit: a session that armed the flag and then inserted \
         plain text is the defect this gate exists for"
    );
    s.repaint_all().expect("paint");
    assert!(
        differing(&s, &painted("abc\n", None)) > 0,
        "**the arming cost nothing on screen**: the document with an underlined `c` paints exactly like \
         the same document with a plain one"
    );
}

/// **Strikethrough has no button, and it still arms and still draws.**
///
/// **The case that separates the bitset from the three bools.** `StyleFlags` has `bold`, `italic` and
/// `underline` and nothing else, so `Ctrl+Shift+5` / FORMAT ▸ Strikethrough has no slot to light — which
/// is why `toggle_inline_style` takes `Option<Tool>` and passes `None` for it. **A version that took a
/// `Tool` would have had to name a button for a flag it does not own**, and the first thing that button
/// would do is highlight for formatting it does not represent.
///
/// The assertion is therefore two-sided: the three slots stay exactly as they were, *and* the typed
/// character is struck.
#[test]
fn the_strikethrough_arms_without_lighting_any_button() {
    use holonomy_render::menus::Action;
    let mut s = painted("ab\n", None);
    s.caret_to(2).expect("caret");
    let before = chrome_armed(&s);
    s.apply_action(Action::ToggleStrike)
        .expect("arm the strike");

    assert_eq!(
        armed(&s),
        before,
        "**no slot may change**: the strike bit is not bold, italic or underline, and a version that \
         mapped it onto one of the three would light a button for formatting it does not own"
    );
    assert_eq!(chrome_armed(&s), before, "and the chrome agrees");

    s.insert_text("c").expect("type");
    assert_eq!(
        s.editor().style_at(2).style_flags & STYLE_STRIKE,
        STYLE_STRIKE,
        "the typed character must be struck"
    );
    s.repaint_all().expect("paint");
    // **And the strike is drawn, which is the half the armed-state assertions cannot reach.**
    let (_, plain_bottom, _, _) = ink_box(&painted("abc\n", None)).expect("the plain line has ink");
    let (_, struck_bottom, _, _) = ink_box(&s).expect("the struck line has ink");
    assert_eq!(
        struck_bottom, plain_bottom,
        "**a strikethrough must not extend the ink box downward** -- that is an underline's signature, \
         and getting it means the two bits are wired to the same rule"
    );
}

/// **Clearing with a selection that stops at the next paragraph does not clear the next paragraph.**
///
/// # The off-by-one this exists for
///
/// `paragraph_range` answers with `line_end_offset(end - 1)` and the doc says why: a selection whose
/// `end` is the *first byte of the next line* did not select that line, so asking where the line
/// containing `end` finishes answers one paragraph too low. **The symptom is a paragraph going plain that
/// the user never selected**, one line below the one they were looking at, and it is invisible in a test
/// that only ever selects within a single line.
///
/// # Why the fixture is two decorated paragraphs
///
/// **So "the wrong one was cleared" is observable.** With one decorated paragraph and one plain one, both
/// a correct range and a range one line long leave *some* decoration on screen. Here both are decorated
/// and only the first should lose its formatting.
#[test]
fn clearing_stops_at_the_end_of_the_last_selected_paragraph() {
    use holonomy_render::menus::Action;
    // `one` is bytes 0..=3, the newline is 3, and `two` is 4..=7.
    let mut s = painted("one\ntwo\n", None);
    s.editor_mut()
        .style_range(0, 8, STYLE_UNDERLINE, 0)
        .expect("underline both paragraphs");
    s.repaint_all().expect("paint");

    // **A selection from the middle of the first paragraph to the first byte of the second**, which is
    // what a drag that stops at the start of the next line produces.
    s.select_range(1, 4).expect("select into the second line's first byte");
    s.apply_action(Action::ClearFormatting).expect("clear");

    assert_eq!(
        s.editor().style_at(0).style_flags & STYLE_UNDERLINE,
        0,
        "the paragraph the selection started in must be cleared"
    );
    assert_eq!(
        s.editor().style_at(5).style_flags & STYLE_UNDERLINE,
        STYLE_UNDERLINE,
        "**the second paragraph must keep its formatting**: byte 5 is inside `two`, which the selection \
         did not reach. A range that ran to `line_end_offset(end)` would have cleared it."
    );
}

/// **A decoration is not a face: it costs no atlas size and changes no advance.**
///
/// # The decision this records
///
/// The PRD's 512 KiB ceiling makes the atlas a budget rather than a cache, and **a face is the expensive
/// way to draw formatting**. `AtlasBuilder::new` refuses a third size outright; one extra size costs
/// 14,080 bytes of metric table on top of the first size's 236,690. A design that wanted an underlined
/// face would be asking for a seventh and eighth set of glyphs whose ink is the same glyphs with a line
/// drawn near them — and the line would still have to be drawn, because a face cannot draw *under* a
/// descender without a glyph that does.
///
/// **So the four decorations are one atlas.** Underline and strike are `Node::Rule` — a filled rectangle,
/// no glyph; super- and subscript are a `baseline_shift`, which is a `y` offset and nothing else.
///
/// # What is measured, and why each half is separate
///
/// * **The size list**, which is what the ceiling is actually about: two sizes, the ones the fixture
///   built, and a decoration did not add a third.
/// * **The advance**, which is what a face would change. A face whose glyphs are drawn differently has
///   different metrics, so the decorated line's ink would be wider or narrower than the plain one's. A
///   rule and a shift change neither.
#[test]
fn a_decoration_is_not_a_face() {
    let decorated = painted(
        "under struck supersub\n",
        Some((
            0,
            19,
            STYLE_UNDERLINE | STYLE_STRIKE | STYLE_SUPERSCRIPT | STYLE_SUBSCRIPT,
        )),
    );
    let plain = painted("under struck supersub\n", None);
    // **The fixture has to be decorated for the gate to mean anything.** If the flags above were a typo
    // in a bit the session does not read, both frames would be plain and every assertion below would
    // pass trivially -- which is the failure mode of every "nothing changed" gate.
    assert!(
        differing(&plain, &decorated) > 0,
        "the fixture is wrong: the decorated document paints identically to the plain one, so this \
         gate is comparing a frame with itself"
    );

    // **Two sizes and no more**, from the atlas's own list. The fixture builds `[16, 20]`; a decoration
    // that needed a face would need a size for it, and this is where that would show.
    assert_eq!(
        atlas().sizes(),
        &[16, 20],
        "the atlas is built at exactly the two sizes the fixture asked for"
    );

    // **The advance is unchanged.** Both frames begin at the text rect's left edge, so the right edges of
    // their ink are comparable directly, and a decoration that changed a glyph's advance would move one.
    let (_, _, _, plain_right) = ink_box(&plain).expect("the plain line has ink");
    let (_, _, _, deco_right) = ink_box(&decorated).expect("the decorated line has ink");
    assert_eq!(
        deco_right, plain_right,
        "the decorated line's ink ends at x = {deco_right} and the plain one's at x = {plain_right}: \
         **a decoration must not move a glyph**, and a face is exactly the way it would"
    );
}

/// **A heading draws at the larger atlas size, and it is the only style that does.**
///
/// **A heading has had a style bit since Phase 6, and whether it was ever drawn bigger is what this gate
/// asks.** The bit is load-bearing for the *line metrics* — a header line is taller — and `doc_style`
/// collapses it onto a face, so the only thing that can make a heading's *glyphs* larger is
/// `run_size_index` returning 1 and the painter resolving it against a second atlas size. **If this gate
/// fails, the screen is drawing a heading in a bold face at body size while the HTML export emits `<h1>`
/// and the PDF a 1.35x size** — the screen and the file disagreeing about the same paragraph.
///
/// **The painter's size index is 1 here as well as the run's**, which is what a zoomed session looks like:
/// the cell geometry follows the size index, so a heading drawn at index 1 on a painter at index 0 would
/// lay 20 ppem glyphs on 18 px leading and the ink box would be clipped by its own line.
#[test]
fn a_heading_draws_at_the_larger_atlas_size() {
    let plain = painted("Heading\n", None);
    let head = painted_at("Heading\n", Some((0, 7, STYLE_HEADER)), 1);
    assert!(differing(&plain, &head) > 0, "the heading changed nothing");

    // **The ink is taller, not merely wider** -- a face change is a different shape and a size change is
    // the same shape at 125 %. The two-size atlas makes the heading's glyphs a fifth bigger.
    let (pt, pb, _, pr) = ink_box(&plain).expect("plain text has ink");
    let (ht, hb, _, hr) = ink_box(&head).expect("heading has ink");
    assert!(
        (hb - ht) > (pb - pt),
        "**the heading's ink must be taller**: {} px plain and {} px as a heading",
        pb - pt,
        hb - ht
    );
    // **And wider**, which is the half that matters for the caret: `pen_of` measures a run at the run's
    // own size, so a heading's advance is 125 % of the body's -- see `Painter::advance_at`. A version
    // that drew bigger glyphs and measured them at the body size would overlap them. Both boxes start at
    // the same x, so the right edges are comparable directly.
    assert!(
        hr > pr,
        "**the heading's ink must be wider as well as taller**: the plain run ends at x = {pr} and the \
         heading at x = {hr}. A heading drawn large and advanced at the body size would overlap its own \
         letters, and the caret inside it would sit left of every glyph."
    );
}

/// **A heading's underline follows the heading's baseline.**
///
/// **`RuleRun` carries the size index for exactly this reason.** The two rule positions are fractions of
/// the *ascent*, and the ascent belongs to the size — so a rule resolved against the painter's current
/// size instead of the run's would sit **inside** a heading's glyphs. That is the failure this gate
/// exists for: it is a two-size bug, so it is invisible on the one-size atlas every other test uses.
#[test]
fn a_headings_underline_is_where_a_headings_baseline_is() {
    let head = painted_at("Heading\n", Some((0, 7, STYLE_HEADER)), 1);
    let both = painted_at("Heading\n", Some((0, 7, STYLE_HEADER | STYLE_UNDERLINE)), 1);

    let (_, hb, _, _) = ink_box(&head).expect("the heading has ink");
    let (_, ub, _, _) = ink_box(&both).expect("the underlined heading has ink");
    assert!(
        ub > hb,
        "**the rule must be below the heading's glyphs**: the heading's lowest ink is {hb} and the \\
         underlined one's is {ub}. A rule that came out at or above it was resolved against the body \\
         size instead of the heading's, which puts it through the letters"
    );
}
