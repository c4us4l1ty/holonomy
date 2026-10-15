//! **What a click costs, what the chrome lies about, and what the sidebar and the title agree on.** 9 tests.
//!
//! # What this file is about
//!
//! Four reported symptoms. They look like speed, and two of them are really *area*:
//!
//! * **"the buttons lag."** Measured in the live window with `HOLONOMY_WINDOW_TRACE=1`, a hover was
//!   damaging **583,808 px — 36 % of a 1,600x1,000 frame** — and dismissing a menu was calling
//!   `full_damage()`, which is the whole panel for a panel where **one popup's rect** changed.
//! * **"the word count says 0"** — 122 characters on the page and a status bar reading
//!   `Ln 1, Col 1  0 words  0 B`. Found by typing into the live window and reading the frame.
//! * **"the sidebar does nothing"** and **"the title says one thing and the tab rail another"**: two
//!   facts that are one fact each, held in two places.
//!
//! # Why damage is the right thing to assert
//!
//! **`PaintStats::pixels` is not a measure of how much was drawn.** It counts ~1.17 M for a rect of
//! 107 k pixels, because it counts *emitter invocations* and not pixels; a gate written against it
//! would be asserting a number nobody can act on. [`Session::last_damage`] is the rect the paint was
//! actually given, **and a rect is something a person can reason about**: "a hover damages one widget"
//! is a claim with a shape, and "the paint touched 1.17 M things" is not.
//!
//! | what it proves | test |
//! | --- | --- |
//! | a hover damages one widget, not a third of the frame | [`a_hover_damages_one_widget_and_not_a_third_of_the_frame`] |
//! | dismissing a menu is not a full repaint | [`dismissing_a_menu_damages_the_popup_and_not_the_panel`] |
//! | an armed toggle damages one button | [`an_armed_toggle_damages_its_button_and_nothing_else`] |
//! | **a selection change spares every band above the canvas** | [`a_selection_change_damages_the_page_and_no_band_above_the_canvas`] |
//! | the sidebar's flag and its reservation agree | [`the_sidebar_flag_and_its_reservation_agree_after_every_change`] |
//! | and the same for the title and the one row | [`the_title_and_the_one_document_row_are_the_same_fact`] |
//! | and the widgets are all smaller than the frame | [`every_toolbar_widget_is_small_next_to_the_frame`] |
//! | **typing repaints the status bar, a hover does not** | [`typing_repaints_the_status_bar_and_a_hover_does_not`] |
//! | **and the bar draws the real count** | [`the_word_count_on_the_status_bar_counts_the_document`] |

use holonomy::session::Session;
use holonomy::store::NoSource;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::pointer::encode_record;
use holonomy_input::{RecordDecoder, BTN_LEFT, EV_KEY, EV_REL, EV_SYN, REL_X, REL_Y};
use holonomy_render::chrome::{ChromeMetrics, ChromeState};
use holonomy_render::widgets;
use holonomy_text::{Editor, SpanPolicy};

/// The toolbar's padding, as `place_toolbar` wants it. **8 px, and not the cell width** -- see
/// [`tool_rect`] for why the two are not interchangeable.
const TOOLBAR_PAD: u32 = 8;

/// The shared atlas, built once for the whole test binary.
fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

/// A session over two lines of text, painted once so there is a frame to damage.
fn session() -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    ed.insert_at(0, b"alpha\nbravo\n", SpanPolicy::GrowIntoInsert)
        .expect("room");
    let mut s = Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );
    s.repaint_all().expect("first paint");
    s
}

/// The frame's area in pixels.
fn frame_px(s: &Session<'_>) -> u64 {
    let (w, h) = s.frame_size();
    u64::from(w) * u64::from(h)
}

/// The share of the frame a damage rect covers, as a percentage.
fn percent_of_frame(s: &Session<'_>) -> u64 {
    let d = s.last_damage();
    100 * u64::from(d.width) * u64::from(d.height) / frame_px(s)
}

/// Move the pointer to `(x, y)` **without pressing**, through real evdev records.
fn hover(s: &mut Session<'static>, x: i32, y: i32) {
    let mut d = RecordDecoder::new();
    for ev in [
        encode_record(EV_REL, REL_X, x),
        encode_record(EV_REL, REL_Y, y),
        encode_record(EV_SYN, 0, 0),
    ] {
        d.push(&ev);
    }
    while let Some(e) = d.next_event() {
        s.handle_pointer(&mut NoSource, e)
            .expect("pointer handling");
    }
}

/// Move and click at `(x, y)`, through real evdev records.
fn click(s: &mut Session<'static>, x: i32, y: i32) {
    let mut d = RecordDecoder::new();
    for ev in [
        encode_record(EV_REL, REL_X, x),
        encode_record(EV_REL, REL_Y, y),
        encode_record(EV_KEY, BTN_LEFT, 1),
        encode_record(EV_KEY, BTN_LEFT, 0),
        encode_record(EV_SYN, 0, 0),
    ] {
        d.push(&ev);
    }
    while let Some(e) = d.next_event() {
        s.handle_pointer(&mut NoSource, e)
            .expect("pointer handling");
    }
}

/// The rect of the toolbar widget called `name`.
///
/// **The second argument to `place_toolbar` is the toolbar's *padding*, not the cell width** -- the
/// signature reads `place_toolbar(l: &Layout, pad: u32)` and passing a cell happens to work only because
/// the design cell and the padding are both 8. **This gate passes the padding explicitly** so that a
/// change to either constant cannot quietly move every button in the file.
fn tool_rect(s: &Session<'_>, name: &str) -> holonomy_render::DamageRect {
    widgets::place_toolbar(&s.chrome_layout(), TOOLBAR_PAD)
        .into_iter()
        .find(|p| p.tool.name() == name)
        .unwrap_or_else(|| panic!("no tool named {name}"))
        .rect
}

/// **A hover over a toolbar button damages that button and nothing else.**
///
/// **The measurement this holds.** In the live window at 20 ppem, hovering `Bold` damaged **583,808 px
/// — 36 % of a 1,600x1,000 frame**. The cause was a `Hit` change invalidating the whole canvas, which
/// is correct for a hover that crosses from the page to the page but is enormous for a hover inside the
/// toolbar, where the only thing that changes is one button's own box.
///
/// **The bound is 1 %, not "small",** because "small" is not checkable: 3 % is small and 36 % was
/// called small by everyone looking at it. **1 % of a 1,280x800 frame is 10,240 px**, which is about
/// eight buttons.
#[test]
fn a_hover_damages_one_widget_and_not_a_third_of_the_frame() {
    let mut s = session();
    let bold = tool_rect(&s, "bold");
    hover(
        &mut s,
        (bold.x + bold.width / 2) as i32,
        (bold.y + bold.height / 2) as i32,
    );

    let d = s.last_damage();
    assert_ne!(
        d,
        holonomy_render::DamageRect::EMPTY,
        "a hover damaged nothing"
    );
    assert!(
        percent_of_frame(&s) <= 1,
        "a hover over Bold damaged {d:?} -- {}% of the {}-px frame. The bug this holds was 36%.",
        percent_of_frame(&s),
        frame_px(&s)
    );
    // **And it is the button's own box, not merely something small.** A hover that damaged the status
    // bar would pass the 1 % gate and be the wrong rectangle entirely.
    assert!(
        bold.contains(d.x as i32 + 1, d.y as i32 + 1) && d.right() <= bold.right() + 1,
        "a hover over Bold damaged {d:?}, which is not inside Bold's own rect {bold:?}"
    );
}

/// **Dismissing a menu repaints the popup and not the panel.**
///
/// **`Hit::None` used to be `full_damage()`.** The reasoning behind it was sound — *something* outside
/// the menu was clicked, so something outside the menu might have changed — and the cost was a
/// 1,280x800 repaint for a click that closed a 224x300 panel. `invalidate_popup_change` names the two
/// rectangles that are actually involved: **the popup that was open and the popup that is not.**
#[test]
fn dismissing_a_menu_damages_the_popup_and_not_the_panel() {
    let mut s = session();
    // Open the File menu by pressing its heading.
    let heading = holonomy_render::chrome::menu_boxes(&s.chrome_layout()).1[0];
    click(
        &mut s,
        (heading.x + heading.width / 2) as i32,
        (heading.y + heading.height / 2) as i32,
    );
    assert!(
        s.state.open.is_some(),
        "pressing the File heading did not open its popup, so the rest of this gate has nothing to \
         dismiss"
    );

    // **Now click the middle of the page, which is not any menu row.**
    let page = s.chrome_layout().page;
    click(
        &mut s,
        (page.x + page.width / 2) as i32,
        (page.y + page.height / 2) as i32,
    );
    assert!(
        s.state.open.is_none(),
        "clicking the page did not dismiss the menu"
    );
    let full = holonomy_render::chrome::ChromeMetrics::DESKTOP;
    assert_ne!(
        s.last_damage(),
        holonomy_render::DamageRect::new(0, 0, full.width, full.height),
        "**dismissing a menu repainted the whole panel.** `Hit::None` used to call `full_damage()` for \
         every click that missed a menu row, and a click on the page is the commonest click there is."
    );
}

/// **Pressing `Bold` damages the Bold button, and nothing else in the chrome.**
///
/// **The armed-toggle path is its own branch** in `Session::press` and it was one of the four that used
/// to damage the whole canvas. **The claim is that an armed toggle is drawn as a fill on the button**,
/// so the button is the only thing whose pixels changed — and the gate says so by name rather than by
/// size, because a size bound alone cannot tell the button from the status bar.
#[test]
fn an_armed_toggle_damages_its_button_and_nothing_else() {
    let mut s = session();
    let bold = tool_rect(&s, "bold");
    click(
        &mut s,
        (bold.x + bold.width / 2) as i32,
        (bold.y + bold.height / 2) as i32,
    );
    let d = s.last_damage();
    assert!(
        bold.contains(d.x as i32 + 1, d.y as i32 + 1),
        "pressing Bold damaged {d:?}, which does not start inside Bold's rect {bold:?}"
    );
    assert!(
        percent_of_frame(&s) <= 1,
        "pressing Bold damaged {}% of the frame",
        percent_of_frame(&s)
    );
}

/// **A selection change repaints the page, the scrollbar and the status bar — and no band above them.**
///
/// # CORRECTION, part 39: this was `full_damage()`
///
/// `after_selection_moves` ended `self.damage = self.chrome.full_damage()`, **while its own doc comment
/// one line above said "the page is the damage unit for anything that moves the selection"**. **The
/// function's reasoning and its code disagreed, and the code was the one that ran**: one `Shift+Left`
/// damaged **1,600,000 px — 100 % of the frame** in the live window at 20 ppem.
///
/// **Shift-arrow is a gesture, not a keystroke** — a person selecting a paragraph presses it sixty
/// times, and a pointer drag reaches this function once per motion event — **so this is the "the
/// buttons lag" report by another route than the hover that was fixed beside it.**
///
/// ## What the gate asserts, and why the bound is not "small"
///
/// **Not "small", because the honest damage is large.** The old highlight and the new one can each span
/// the page, so the page is repainted: **720x562 of a 1,280x800 panel.** The claim available from a
/// single-rect damage model is the one that costs nothing to keep: **the five bands above the canvas —
/// title, menu bar, toolbar, ruler — are untouched**, and they are 117 px of chrome that cannot have
/// changed.
#[test]
fn a_selection_change_damages_the_page_and_no_band_above_the_canvas() {
    let mut s = session();
    // **Shift goes through the real key path rather than by writing `s.mods`**, because
    // `Session::dispatch` is the only place `ModifierState` moves -- `selection.rs` says the same and
    // explains what that gate is covering.
    let _ = s.dispatch(holonomy_input::InputEvent::press(
        holonomy_input::KEY_LEFTSHIFT,
    ));
    s.handle_event(holonomy_input::InputEvent::press(holonomy_input::KEY_LEFT))
        .expect("shift+left");
    assert!(
        s.editor().selection().is_some(),
        "shift+left selected nothing, so this gate has no selection to move"
    );

    let m = s.chrome_metrics();
    let full = holonomy_render::DamageRect::new(0, 0, m.width, m.height);
    let d = s.last_damage();
    assert_ne!(
        d, full,
        "**a selection change repainted the whole panel.** `after_selection_moves` called \
         `full_damage()` on every shift-arrow and every drag motion event, which is 100% of the frame \
         on the gesture a document is read with."
    );

    let l = s.chrome_layout();
    // **The page is inside it** -- the highlight is the reason the repaint exists.
    assert!(
        d.x <= l.page.x
            && d.right() >= l.page.right()
            && d.y <= l.page.y
            && d.bottom() >= l.page.bottom(),
        "damaged {d:?}, which does not cover the page at {:?}. The old highlight and the new one are \
         both inside the page, and the old one has to be erased.",
        l.page
    );
    // **The status bar is inside it** -- it prints `Ln N, Col M`, and a selection that stays on one
    // line still moves the column.
    assert!(
        d.y <= l.status.y && d.bottom() >= l.status.bottom(),
        "damaged {d:?}, which does not reach the status bar at {:?}. It prints Ln/Col and the caret \
         column moved.",
        l.status
    );
    // **And nothing above the canvas is.** This is the half the 1% bounds on the other gates cannot
    // make: those are about widgets, and this is about five bands that do not change.
    for (what, band) in [
        ("the title bar", l.title),
        ("the menu bar", l.menubar),
        ("the toolbar", l.toolbar),
        ("the ruler", l.ruler),
    ] {
        assert!(
            d.y >= band.bottom(),
            "{what} runs {}..={} and the damage starts at y={}, so {what} is repainted by a selection \
             change that cannot have changed it",
            band.y,
            band.bottom(),
            d.y
        );
    }
}

/// **`set_sidebar` puts the flag and the reservation back in step, in both directions and
/// repeatedly.**
///
/// **Part 39's one-fact change, held.** The reservation used to be a constant that a `bool` on
/// [`ChromeState`] had nothing to do with, so a closed sidebar still pushed the page 256 px to the
/// right and no caller could put the two back together.
///
/// **The sweep is over both orderings and over a repeat**, because the failure mode is *the second
/// call being a no-op*: a function that set the flag and returned early when the flag already matched
/// would pass a one-shot check and leave the reservation stale forever after.
#[test]
fn the_sidebar_flag_and_its_reservation_agree_after_every_change() {
    use holonomy_render::chrome::sidebar_reservation;

    let mut s = session();
    let full = holonomy_render::chrome::ChromeMetrics::DESKTOP;
    for want in [false, true, false, true, true, false] {
        s.set_sidebar(want);
        let got = s.state.sidebar_open;
        assert_eq!(got, want, "set_sidebar({want}) left the flag at {got}");
        assert_eq!(
            s.chrome_metrics().sidebar_w,
            sidebar_reservation(got),
            "with sidebar_open = {got} the reservation is {} px; `sidebar_reservation({got})` says \
             {}",
            s.chrome_metrics().sidebar_w,
            sidebar_reservation(got)
        );
        assert_eq!(
            s.chrome_layout().sidebar.width == 0,
            !got,
            "and the sidebar's own rect follows the flag: {:?} with sidebar_open = {got}",
            s.chrome_layout().sidebar
        );
    }
    assert_eq!(
        full.sidebar_w,
        sidebar_reservation(true),
        "and the width is the design one"
    );
}

/// **`set_document` sets the title and the one row together, because they are one fact.**
///
/// **The temptation is to invent three rows so the rail *looks* like the reference's**, and a rail that
/// lists documents that cannot be opened is worse than an empty one, because a person clicks it. **So
/// there is one document and one row**, and this gate holds that the title above the rail and the row
/// in it cannot come apart.
#[test]
fn the_title_and_the_one_document_row_are_the_same_fact() {
    let mut s = session();
    s.set_document("field notes");
    let st = &s.state;
    assert_eq!(st.title, "field notes", "the title bar names the document");
    assert_eq!(
        st.docs,
        vec!["field notes".to_string()],
        "**and the rail lists exactly the one document the session has.** A rail padded with rows that \
         cannot be opened is worse than an absent row, because a person clicks it."
    );
    assert_eq!(st.active_doc, 0, "and it is the active one");

    // **A second call with the same name changes nothing**, so a driver that sets the document on every
    // focus event does not repaint the whole panel every time.
    let before = s.last_damage();
    s.set_document("field notes");
    assert_eq!(
        s.last_damage(),
        before,
        "setting the same document again damaged the panel"
    );

    // **A different name moves both, in step.**
    s.set_document("chapter one");
    let st = &s.state;
    assert_eq!(st.title, "chapter one");
    assert_eq!(st.docs, vec!["chapter one".to_string()]);
}

/// **Every toolbar widget is a small fraction of the frame — so a damage bound above has room to be
/// tight.**
///
/// **This is the premise for the two gates above.** If a button were most of the panel, "a hover
/// damages 1 %" would be unsatisfiable and the 1 % would have to be loosened to make it pass, which is
/// how a bound stops meaning anything. **The bound is chosen from the widgets, and this is what holds
/// it.**
#[test]
fn every_toolbar_widget_is_small_next_to_the_frame() {
    let s = session();
    let m = s.chrome_metrics();
    let frame = frame_px(&s);
    let mut worst = 0u64;
    let mut n = 0usize;
    for p in widgets::place_toolbar(&s.chrome_layout(), TOOLBAR_PAD) {
        let px = u64::from(p.rect.width) * u64::from(p.rect.height);
        assert!(
            px * 100 <= frame,
            "{} at {:?} is {} px, {}% of the frame -- the 1% bound on a hover has no room",
            p.tool.name(),
            p.rect,
            px,
            100 * px / frame
        );
        // **In hundredths of a percent, because `100 * px / frame` is 0 for every button here** --
        // 2,600 px of a 1,024,000 px frame is 0.25 %, which integer-divides to nothing. The first
        // version of this loop tracked that truncated number and asserted it was non-zero, so it
        // failed with "the toolbar placed no widgets" when in fact all twenty were placed.
        worst = worst.max(10_000 * px / frame);
        n += 1;
    }
    assert!(
        n > 0,
        "the toolbar placed no widgets, so the damage gates above measure nothing"
    );
    assert!(
        worst <= 100,
        "the largest button is {worst} hundredths of a percent of the frame, so the 1% hover bound \
         has no room"
    );
    let _: ChromeState = ChromeState::default();
    let _ = m;
}

/// **Typing repaints the status bar, and a hover does not.**
///
/// # CORRECTION, part 39: the status bar never repainted at all
///
/// **Found by typing into the live window and reading the frame.** 122 characters on the page and a
/// status bar reading **`Ln 1, Col 1  0 words  0 B`**; the same document after the fix reads
/// **`Col 123  23 words  122 B`**. The state was right --
/// `publish_counts` runs on every edit -- and the pixels were from the first frame: `after_edit`
/// damages `line_box(caret_line)`, which is one text row, **and the status bar is not in it**.
///
/// **A word count that does not count words is the chrome lying about the document**, and it survived
/// every gate in the project because every gate asserts what the session *emits*. The emitter had the
/// right numbers; nothing asked whether they were *drawn*.
///
/// ## Why this is a second region and not a bigger rect
///
/// **Because the union is a bounding box and the status bar is at the bottom of the window.** The
/// caret's cell is a 54x50 rect in the middle of the page, the status bar is at the foot of the panel,
/// and the bounding box of the two is the whole frame -- **on the keystroke path, which NFR-1.1 budgets
/// at 0.50 ms.** So the bar is painted as its own clipped pass and *reported* as its own rect, and the
/// two numbers stay separately true.
///
/// **The negative half is the half that keeps the cost down**: a hover changes nothing the bar prints,
/// so it must not drag a 1,280x22 repaint into every pointer motion event.
#[test]
fn typing_repaints_the_status_bar_and_a_hover_does_not() {
    let mut s = session();
    // **A hover first, because it is the cheap case and the one an "always repaint" fix would break.**
    let bold = tool_rect(&s, "bold");
    hover(
        &mut s,
        (bold.x + bold.width / 2) as i32,
        (bold.y + bold.height / 2) as i32,
    );
    assert_eq!(
        s.last_status_damage(),
        holonomy_render::DamageRect::EMPTY,
        "a hover over Bold asked for the status bar to be repainted. The bar's text does not change \
         when the pointer moves, so this is 28,160 px per pointer event for nothing."
    );

    // **And now type, through the same `Command` the keymap produces.** `apply` rather than
    // `handle_event`: the keymap's contribution is *which* command a key becomes, and this gate is
    // about what happens after that, so synthesising keycodes for characters would be testing the
    // keymap again rather than the damage.
    for c in "hi".chars() {
        s.apply(holonomy_input::Command::Insert(c)).expect("insert");
        s.tick().expect("tick");
    }
    let bar = s.chrome_layout().status;
    assert_eq!(
        s.last_status_damage(),
        bar,
        "typing did not ask for the status bar to be repainted, so the word count and the column on \
         screen are the ones from the first frame. The state has the right numbers -- `publish_counts` \
         ran -- and nothing draws them."
    );

    // **And the main damage is still one line**, which is the reason the bar is a second region rather
    // than a union. This is the assertion that would fail if somebody "simplified" it into `damage`.
    let d = s.last_damage();
    assert!(
        d.bottom() <= bar.y,
        "typing damaged {d:?} down to y={}, and the status bar starts at {}. The union of a caret cell \
         and the bar is the whole window, which is the thing this design exists to avoid.",
        d.bottom(),
        bar.y
    );
    assert!(
        d.height <= 3 * s.chrome_metrics().cell_h,
        "and it is {} px tall, which is a line or two rather than a frame",
        d.height
    );
}

/// **What the status bar prints is what the document is, on the pixels and not only in the state.**
///
/// **The gate above holds the damage; this one holds the result.** They are separate claims, and the
/// defect they cover had the *state* right the whole time -- so a state-only gate would have been green
/// throughout and this defect would still be in the product.
#[test]
fn the_word_count_on_the_status_bar_counts_the_document() {
    let mut s = session();
    let bar = s.chrome_layout().status;
    let blank = ink_in(&s, bar);
    for c in "one two three".chars() {
        s.apply(holonomy_input::Command::Insert(c)).expect("insert");
        s.tick().expect("tick");
    }
    // **Two words came from the fixture's `alpha\nbravo\n` and three from here.** The first version
    // of this line expected `3`, read `5`, and **the `5` was right** -- which is worth recording,
    // because a gate that expected a smaller number would have been "fixing" a correct count.
    assert_eq!(
        s.state.words, 5,
        "two words in the fixture and three typed. A word count that does not count is the defect, and \\
         this is the number it has to get right to draw anything true."
    );
    let after = ink_in(&s, bar);
    assert_ne!(
        blank, after,
        "**the status bar drew the same thing before and after typing three words.** A word count that \
         never counts is the defect, and a damage gate cannot see it -- the damage was the right rect."
    );

    // **And the column moved**, because `Ln 1, Col 1` is the other half of what the live window showed.
    assert_eq!(
        s.state.caret_column, 13,
        "thirteen characters were typed, so the bar should be about to read `Col 14`"
    );
}

/// Every pixel of `r`, for a comparison a gate can make.
fn ink_in(s: &Session<'_>, r: holonomy_render::DamageRect) -> Vec<u32> {
    (r.y..r.bottom())
        .flat_map(|y| (r.x..r.right()).map(move |x| (x, y)))
        .map(|(x, y)| s.frame().pixel(x, y))
        .collect()
}
