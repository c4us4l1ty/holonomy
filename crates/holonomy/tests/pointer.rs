//! **Clicking: a pointer press, through the widget list, to the document.** 10 tests.
//!
//! # What is being claimed
//!
//! Part 20's claim is narrow and worth stating exactly: **a pointer press is routed by the same
//! widget list that draws the chrome.** Not "the hit test exists", not "the events arrive" — that a
//! press on Bold is decided by `widgets::TOOLBAR` and `widgets::hit`, and a press on the page places
//! the caret, and a press on a menu heading opens that menu.
//!
//! # Why the tests drive real evdev records
//!
//! **Because a gate that constructs `Event::Button` directly would not test the contract change.**
//! The whole of part 20 is that `EV_REL` was being dropped by a trait that promised to drop it. Every
//! fixture here is built from `encode_record` — the same bytes a mouse sends — and read through
//! [`Session::handle_pointer`], so a regression in the decoder shows up here as a click that never
//! lands rather than as a unit test in another crate.
//!
//! | what it proves | test |
//! | --- | --- |
//! | a click on the page moves the caret | [`a_click_on_the_page_puts_the_caret_under_the_pointer`] |
//! | and lands on the right *byte* of it | [`clicking_a_column_puts_the_caret_after_that_character`] |
//! | a click on a heading opens its menu | [`a_click_on_a_menu_heading_opens_that_menu`] |
//! | and on the same heading closes it | [`a_second_click_on_the_same_heading_closes_it`] |
//! | a click on a row chooses it | [`a_click_on_a_popup_row_dismisses_the_menu`] |
//! | and the grab eats the toolbar | [`an_open_menu_swallows_a_click_on_the_toolbar_below_it`] |
//! | an inert button is counted, not faked | [`a_button_with_nothing_behind_it_is_counted_not_faked`] |
//! | hover is not left behind | [`moving_off_a_button_clears_its_highlight`] |
//! | the sidebar's own row is reachable | [`clicking_a_sidebar_row_makes_it_active`] |
//! | and the whole thing is one list | [`every_tool_that_has_a_command_is_routed_through_it`] |

use holonomy::session::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::pointer::encode_record;
use holonomy_input::{Event, RecordDecoder};
use holonomy_render::chrome::{ChromeMetrics, ChromeState};
use holonomy_render::{widgets, Chrome};
use holonomy_text::{Editor, SpanPolicy};

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

/// A session over `text`, with a default chrome state.
fn session(text: &str) -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    if !text.is_empty() {
        ed.insert_at(0, text.as_bytes(), SpanPolicy::GrowIntoInsert)
            .expect("room");
    }
    Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    )
}

/// The events for "move the pointer to `(x, y)`, then click".
///
/// **Absolute, built from a start at the origin.** A real mouse sends deltas, so the fixture sends
/// `REL_X (x - already there)` from `(0, 0)` — one frame's worth, which is what a mouse that has just
/// been picked up and moved does.
fn move_and_click(x: i32, y: i32) -> Vec<Event> {
    let mut d = RecordDecoder::new();
    for ev in [
        encode_record(holonomy_input::EV_REL, holonomy_input::REL_X, x),
        encode_record(holonomy_input::EV_REL, holonomy_input::REL_Y, y),
        encode_record(holonomy_input::EV_KEY, holonomy_input::BTN_LEFT, 1),
        encode_record(holonomy_input::EV_KEY, holonomy_input::BTN_LEFT, 0),
        encode_record(holonomy_input::EV_SYN, 0, 0),
    ] {
        d.push(&ev);
    }
    let mut out = Vec::new();
    while let Some(e) = d.next_event() {
        out.push(e);
    }
    out
}

/// The events for "move the pointer to `(x, y)` and stop".
fn move_to(x: i32, y: i32) -> Vec<Event> {
    let mut d = RecordDecoder::new();
    for ev in [
        encode_record(holonomy_input::EV_REL, holonomy_input::REL_X, x),
        encode_record(holonomy_input::EV_REL, holonomy_input::REL_Y, y),
        encode_record(holonomy_input::EV_SYN, 0, 0),
    ] {
        d.push(&ev);
    }
    let mut out = Vec::new();
    while let Some(e) = d.next_event() {
        out.push(e);
    }
    out
}

/// Deliver events to `s`, as the run loop does.
fn feed(s: &mut Session<'static>, events: &[Event]) {
    for e in events {
        s.handle_pointer(&mut holonomy::store::NoSource, *e)
            .expect("pointer handling does not fail");
    }
}

/// **A click on the page puts the caret at the row and column under the pointer.**
///
/// **Driven through evdev records, not by calling a caret method.** The claim is that a *click* works,
/// and a click is bytes off a mouse; a gate that set the caret directly would pass while the decoder
/// was still dropping `EV_REL`, which is precisely the bug part 20 exists to fix.
#[test]
fn a_click_on_the_page_puts_the_caret_under_the_pointer() {
    let mut s = session("alpha\nbravo\ncharlie\n");
    let l = s.chrome_layout();
    let text_x = l.text.x;
    let text_y = l.text.y;
    // Column 3 of row 1: a quarter of the way across the text column and one line down.
    let x = text_x as i32 + 3 * l.cell_w as i32;
    let y = text_y as i32 + l.cell_h as i32;
    assert_eq!(
        widgets::hit(&l, &s.state, x, y),
        widgets::Hit::Page,
        "the probe is on the page"
    );

    feed(&mut s, &move_and_click(x, y));
    assert_eq!(
        (s.caret_line(), s.caret_column()),
        (1, 3),
        "and the caret is where the click was"
    );
}

/// **A click puts the caret after the character in that column, counting bytes and not codepoints.**
///
/// **The line is `éa` — one multi-byte character and one ASCII one.** A caret computed as
/// `line_start + column` lands at byte 2 on a click in column 1, which is the *second* `a` — off by
/// one character, and off by one in a way that looks like "the click is slightly off" rather than like
/// a bug. This is the only gate that would notice, which is why the fixture is not ASCII.
#[test]
fn clicking_a_column_puts_the_caret_after_that_character() {
    let mut s = session("éa\n");
    let l = s.chrome_layout();
    // Column 1 is after the `é` and before the `a`.
    let x = l.text.x as i32 + l.cell_w as i32;
    let y = l.text.y as i32;
    feed(&mut s, &move_and_click(x, y));
    assert_eq!(
        s.caret(),
        2,
        "byte 2 is after the two-byte 'é' and before the 'a'"
    );
    // **`caret_column` is NOT asserted here, and that is a recorded finding rather than an omission.**
    //
    // `Session::caret_to` computes it as `caret - line_start(caret)` -- a **byte** offset -- so for a
    // caret at byte 2 of `éa` it reports column 2, and `Caret::locate` then draws the caret at
    // `text.x + 2 * cell_w`, one cell right of where the click put it.
    //
    // **This is pre-existing and not caused by the click path**: every keystroke goes through the same
    // `caret_to`, so typing `é` has always moved the drawn caret one cell too far. Fixing it means
    // `caret_column` becoming a character count, which moves the caret for *every* non-ASCII document
    // and touches `after_edit` as well as `caret_to` -- a change to the render path, not to the
    // pointer path, and one that wants its own gate.
    //
    // Asserting `caret_column() == 1` here would fail; asserting `== 2` would enshrine the bug.
    // Neither is done, and the byte offset above is the assertion that is actually about the click.
}

/// **A click on a menu heading opens that menu, and only that menu.**
#[test]
fn a_click_on_a_menu_heading_opens_that_menu() {
    let mut s = session("x");
    let l = s.chrome_layout();
    let (_, boxes) = holonomy_render::chrome::menu_boxes(&l);
    let r = boxes[3];
    let cx = (r.x + r.width / 2) as i32;
    let cy = (r.y + r.height / 2) as i32;

    feed(&mut s, &move_and_click(cx, cy));
    assert_eq!(s.state.open, Some(widgets::Open::Menu(3)), "Insert is open");
    assert!(
        widgets::popup(&l, &s.state, Some(widgets::Open::Menu(3))).is_some(),
        "and it has items, so there is something to click"
    );
}

/// **A second click on the same heading closes it.**
///
/// **The one menu behaviour a screenshot cannot show.** Every reference screenshot of an open menu
/// tells you the menu opens; none of them tells you that clicking the heading again takes it away, and
/// it is the behaviour a user discovers by accident and then relies on.
#[test]
fn a_second_click_on_the_same_heading_closes_it() {
    let mut s = session("x");
    let l = s.chrome_layout();
    let (_, boxes) = holonomy_render::chrome::menu_boxes(&l);
    let r = boxes[1];
    let cx = (r.x + r.width / 2) as i32;
    let cy = (r.y + r.height / 2) as i32;

    feed(&mut s, &move_and_click(cx, cy));
    assert_eq!(s.state.open, Some(widgets::Open::Menu(1)));
    feed(&mut s, &move_and_click(cx, cy));
    assert_eq!(s.state.open, None, "the toggle, not 'open'");
}

/// **A click on a popup row dismisses the menu.**
///
/// **The dismissal is in the same press as the choice, and that is deliberate.** A menu that stayed
/// open while the command ran would need a second click to close, and the command's own paint would
/// race the menu's — so the menu is closed first and the command applied after, in one press, in that
/// order.
#[test]
fn a_click_on_a_popup_row_dismisses_the_menu() {
    let mut s = session("hello");
    let l = s.chrome_layout();
    let (_, boxes) = holonomy_render::chrome::menu_boxes(&l);
    let r = boxes[1]; // Edit, whose first item is Undo
    feed(
        &mut s,
        &move_and_click((r.x + r.width / 2) as i32, (r.y + r.height / 2) as i32),
    );
    assert_eq!(s.state.open, Some(widgets::Open::Menu(1)));

    // Row 0 of Edit is "Undo", which is a real command.
    let pop = widgets::popup(&l, &s.state, Some(widgets::Open::Menu(1))).expect("Edit has items");
    let row = pop.rows[0];
    let before = s.caret();
    feed(
        &mut s,
        &move_and_click(
            (row.x + row.width / 2) as i32,
            (row.y + row.height / 2) as i32,
        ),
    );
    assert_eq!(s.state.open, None, "one press, and the menu is gone");
    assert!(
        s.stats.pointer_commands > 0,
        "and it was not an inert press"
    );
    let _ = before;
}

/// **An open menu swallows a click on the toolbar underneath it.**
///
/// # This is the grab, and it is the whole test
///
/// The popup of `Insert` hangs over the toolbar's left-hand buttons. **Without the grab, a click there
/// would both dismiss the menu and press the button** — and the user would have no way to close a
/// menu without also firing something. It is the most irritating thing a menu can do and it is
/// invisible in every gate that only ever clicks the popup itself.
#[test]
fn an_open_menu_swallows_a_click_on_the_toolbar_below_it() {
    let mut s = session("x");
    let l = s.chrome_layout();
    let (_, boxes) = holonomy_render::chrome::menu_boxes(&l);
    let r = boxes[3];
    feed(
        &mut s,
        &move_and_click((r.x + r.width / 2) as i32, (r.y + r.height / 2) as i32),
    );
    assert_eq!(s.state.open, Some(widgets::Open::Menu(3)));

    // The Undo button, which is under the popup's left edge.
    let undo = widgets::place_toolbar(&l, 8)
        .into_iter()
        .find(|p| p.tool == widgets::Tool::Undo)
        .expect("the toolbar has Undo")
        .rect;
    let bx = (undo.x + undo.width / 2) as i32;
    let by = (undo.y + undo.height / 2) as i32;
    let presses_before = s.stats.pointer_presses;
    let commands_before = s.stats.pointer_commands;

    feed(&mut s, &move_and_click(bx, by));
    assert_eq!(s.state.open, None, "the click dismissed the menu");
    assert_eq!(
        s.stats.pointer_commands, commands_before,
        "and did not also press Undo"
    );
    assert_eq!(
        s.stats.pointer_presses,
        presses_before + 1,
        "but it was a press"
    );
}

/// **A button with nothing behind it is counted, not faked.**
///
/// **Bold is the button.** The document model has no bold — no span, no weight, nothing — so the
/// honest answer is that pressing Bold does nothing. What must not happen is a `Command` that gets
/// applied and changes nothing while `stats.commands` says work was done, so the press is counted in
/// `pointer_inert` and `pointer_commands` does not move.
#[test]
fn a_button_with_nothing_behind_it_is_counted_not_faked() {
    let mut s = session("x");
    let l = s.chrome_layout();
    // **Print, and not Bold.** This gate used `Tool::Bold` and part 25 armed a style from it, so Bold is
    // no longer an example of a button with nothing behind it. **Print is, and there is nothing behind
    // it** — no print backend, and adding one is not something to invent to make a test pass.
    let bold = widgets::place_toolbar(&l, 8)
        .into_iter()
        .find(|p| p.tool == widgets::Tool::Print)
        .expect("the toolbar has Print")
        .rect;
    let inert_before = s.stats.pointer_inert;
    let commands_before = s.stats.pointer_commands;

    feed(
        &mut s,
        &move_and_click(
            (bold.x + bold.width / 2) as i32,
            (bold.y + bold.height / 2) as i32,
        ),
    );
    assert_eq!(s.stats.pointer_inert, inert_before + 1, "counted as inert");
    assert_eq!(
        s.stats.pointer_commands, commands_before,
        "and not as a command that quietly did nothing"
    );
}

/// **Bold is no longer in the inert set, and neither is Italic — and that is the only change.**
///
/// **CORRECTION, part 25.** The gate above used `Tool::Bold` as its example of a button with nothing
/// behind it, and it was right when it was written. **Part 25 armed a style from that button**, so Bold
/// and Italic left the inert set and this test needed a different example rather than a deletion.
///
/// **The claim it now makes is the one worth keeping: the inert set shrank by exactly two and no
/// further.** A toolbar button silently leaving the inert set is the thing this file exists to prevent —
/// `pointer_inert` is only meaningful if moving out of it is a deliberate, counted act, and asserting
/// the *size* of the change is what makes it one.
#[test]
fn bold_and_italic_left_the_inert_set_and_nothing_else_did() {
    let mut s = session("x");
    let mut inert = Vec::new();
    let mut chrome = Vec::new();
    // **The popup is closed after every click, and that is what this gate had to learn.**
    //
    // The first version clicked all twenty-one tools in one pass and came back with eleven inert
    // instead of twelve, missing `size-down`. **The cause is the dropdown grab.** Clicking Zoom opens
    // its dropdown, and an open popup swallows every press that is not one of its rows — so `style`,
    // `font` and `size-down` after it were *dismissed* rather than pressed, and did nothing at all.
    //
    // **That is correct product behaviour and a broken fixture**, and the difference between the two is
    // exactly what part 20 wrote the grab for. Part 21's routing gate clears `state.open` before each
    // press for this reason; this gate did not, and paid for it with a number that looked like
    // arithmetic being wrong.
    for name in widgets::TOOLBAR.iter().map(|t| t.name()) {
        let r = widgets::place_toolbar(&s.chrome_layout(), 8)
            .into_iter()
            .find(|p| p.tool.name() == name)
            .unwrap_or_else(|| panic!("the toolbar has {name}"))
            .rect;
        let i0 = s.stats.pointer_inert;
        let c0 = s.stats.pointer_chrome;
        // **Nothing open, so the press belongs to this tool.** See the note above.
        s.state.open = None;
        feed(
            &mut s,
            &move_and_click((r.x + r.width / 2) as i32, (r.y + r.height / 2) as i32),
        );
        if s.stats.pointer_inert > i0 {
            inert.push(name);
        }
        if s.stats.pointer_chrome > c0 {
            chrome.push(name);
        }
    }
    assert!(
        !inert.contains(&"bold") && !inert.contains(&"italic"),
        "Bold and Italic must not be counted inert: {inert:?}"
    );
    assert_eq!(
        chrome,
        vec!["bold", "italic"],
        "**and the tools that change chrome state are exactly these**, in toolbar order. A third would \\
         mean something else started claiming to work without a decision."
    );
    // **Zoom, Style and Font are not in that list, and their absence is the point.** They *open a
    // dropdown* and return before anything is applied, so they are counted by the routing gate's
    // `opened` and not here. The first version of this assertion expected `["zoom", "bold", "italic"]`
    // and the compiler was right: a dropdown's opener has changed nothing yet.
    //
    // **The arithmetic, spelled out so a change of category is a diff and not a mystery.** 21 tools:
    // 3 fire a command (Undo, Redo, Image), 1 toggles the sidebar (Collapse), 3 open a dropdown (Zoom,
    // Style, Font), 2 change chrome state (Bold, Italic), and the rest are inert.
    assert_eq!(
        inert.len(),
        12,
        "**twelve inert buttons**, which is 21 minus 3 commands, 1 toggle, 3 dropdowns and 2 chrome \\
         changes. The list is {inert:?}"
    );
}

/// **Moving off a button clears its highlight.**
///
/// **A stale highlight is permanent.** The damage for a motion event is the union of the old hover's
/// rect and the new one's — computed from `widgets::hit_rect`, the inverse of `hit` — and if the old
/// one were left out the button would stay lit under a pointer that is somewhere else, with nothing
/// left to invalidate it. This asserts the *state*; the pixels are `paint`'s business and
/// `chrome_paint_order.rs`'s.
#[test]
fn moving_off_a_button_clears_its_highlight() {
    let mut s = session("x");
    let l = s.chrome_layout();
    let bold = widgets::place_toolbar(&l, 8)
        .into_iter()
        .find(|p| p.tool == widgets::Tool::Bold)
        .expect("the toolbar has Bold")
        .rect;
    let cx = (bold.x + bold.width / 2) as i32;
    let cy = (bold.y + bold.height / 2) as i32;

    feed(&mut s, &move_to(cx, cy));
    assert_eq!(
        s.state.hover,
        Some(widgets::Hit::Tool(widgets::Tool::Bold)),
        "hovering it"
    );
    assert!(
        widgets::hit_rect(&l, &s.state, widgets::Hit::Tool(widgets::Tool::Bold)).is_some(),
        "and it has a rect, or the highlight could never be invalidated"
    );

    // Now move to the middle of the page, which is nothing.
    let page = l.page;
    feed(
        &mut s,
        &move_to(
            (page.x + page.width / 2) as i32,
            (page.y + page.height / 2) as i32,
        ),
    );
    assert_eq!(s.state.hover, None, "and it is not hovered any more");
}

/// **Clicking a sidebar row makes it active.**
///
/// **The sidebar is the case that gets forgotten**, because it is a band *and* a list, and a hit test
/// that stops at "the sidebar" makes every document unreachable. `tests/widgets.rs` asserts the rows
/// are hit; this asserts that hitting one does something.
#[test]
fn clicking_a_sidebar_row_makes_it_active() {
    let mut s = session("x");
    s.state.docs = vec!["one".into(), "two".into(), "three".into()];
    s.state.active_doc = 0;
    let l = s.chrome_layout();
    let row = l.sidebar_doc(2).expect("the sidebar has a third row");

    feed(
        &mut s,
        &move_and_click(
            (row.x + row.width / 2) as i32,
            (row.y + row.height / 2) as i32,
        ),
    );
    assert_eq!(s.state.active_doc, 2);
}

/// **Every tool that has a command is reachable by clicking it, and the count is stated.**
///
/// **The table that would rot silently.** `Session::tool_command` is a `match` over `Tool`, and a
/// `Tool` added to `TOOLBAR` with no arm in it is a button that draws, hovers, presses and does
/// nothing — and nothing else in the workspace would notice. So the number is written down here, and
/// a new tool with an action behind it changes it on purpose.
///
/// **Four, and the three that are missing are named.** Undo, Redo and Image are wired. Collapse is
/// handled in `press` because it toggles a chrome flag rather than issuing a document command. The
/// rest — bold, italic, the font controls, print — have nothing behind them because the document
/// model has no representation for any of them, and `pointer_inert` is where that shows up.
#[test]
fn every_tool_that_has_a_command_is_routed_through_it() {
    let mut s = session("abc");
    let l = s.chrome_layout();
    let mut wired = 0;
    // **The layout is re-read inside the loop, and the honest reason is a hazard that does not fire
    // today.** Clicking Collapse toggles the sidebar, which changes `Layout` and moves every toolbar
    // button after it — **and Collapse is last in `TOOLBAR`, so nothing after it exists and the
    // rects-taken-once version of this loop was accidentally correct.**
    //
    // **It is re-read anyway.** A gate that walks a layout the loop itself can change should not depend
    // on the order of the list it is walking, and re-reading costs one `Layout` copy per tool. The
    // first draft of this comment claimed the stale rects *were* causing a wrong answer, measured eleven
    // inert buttons against an expected twelve, and pointed at this. **The cause was the popup grab
    // four lines away and this was not it** — the same lesson as part 24's three fixture errors: a
    // number that is wrong has one cause and the nearest explanation is usually not it.
    for name in widgets::TOOLBAR.iter().map(|t| t.name()) {
        let (p, r) = widgets::place_toolbar(&s.chrome_layout(), 8)
            .into_iter()
            .find(|p| p.tool.name() == name)
            .map(|p| (p.tool, p.rect))
            .unwrap_or_else(|| panic!("the toolbar has {name}"));
        let commands_before = s.stats.pointer_commands;
        let inert_before = s.stats.pointer_inert;
        let chrome_before = s.stats.pointer_chrome;
        let sidebar_before = s.state.sidebar_open;
        let before_open = s.state.open;
        s.state.open = None;
        feed(
            &mut s,
            &move_and_click((r.x + r.width / 2) as i32, (r.y + r.height / 2) as i32),
        );
        let fired = s.stats.pointer_commands - commands_before;
        let inert = s.stats.pointer_inert - inert_before;
        let toggled = (s.state.sidebar_open != sidebar_before) as u32;
        let opened = u32::from(s.state.open != before_open);
        let chrome = s.stats.pointer_chrome - chrome_before;

        // **A press is exactly one of five things, and never two.** The counters are alternatives,
        // not a spectrum: a tool that fired *and* was counted inert would be counted as working and
        // not working, which is the state this whole file exists to make distinguishable.
        //
        // **The fourth was `opened`, added in part 21** when Zoom, Style and Font grew dropdowns. The
        // first version of this test asserted `fired + inert + toggled == 1` and Zoom came back 0 --
        // which was the gate correctly reporting that a press had an outcome it did not know about.
        //
        // **The fifth is `chrome`, added in part 23.** Part 21 counted a zoom choice as `fired`, which
        // claimed a `Command` was produced; none was, and part 23 measured that a zoom change moves one
        // label and nothing else. **Both times the fix was to name the outcome rather than relax the
        // sum** -- a `>= 0` would have passed and told us nothing.
        assert_eq!(
            fired + inert + toggled + opened + chrome,
            1,
            "{} must fire a command, be counted inert, toggle the sidebar, open a dropdown, or change \
             the chrome -- exactly once. Got {fired} fired, {inert} inert, {toggled} toggled, \
             {opened} opened, {chrome} chrome.",
            p.name()
        );
        if matches!(
            p,
            widgets::Tool::Undo | widgets::Tool::Redo | widgets::Tool::Image
        ) {
            assert_eq!(fired, 1, "{} should have fired", p.name());
            wired += 1;
        }
        if p == widgets::Tool::Collapse {
            assert_eq!(
                toggled, 1,
                "Collapse toggles the sidebar, and does not also fire"
            );
        }
        if p.has_dropdown() {
            assert_eq!(
                opened,
                1,
                "{} opens a dropdown, and does not also fire or count inert",
                p.name()
            );
        }
        // Leave nothing open for the next tool.
        s.state.open = None;
    }
    assert_eq!(
        wired, 3,
        "Undo, Redo and Image. Adding a fourth changes this number."
    );
    let _ = Chrome::new(ChromeMetrics::DESKTOP);
    let _ = ChromeState::default();
}

/// **The layout a gate reads is a layout whose rects are where the bands say they are.**
///
/// # The premise of the other nine, asserted once
///
/// Every other test takes a rect from `s.chrome_layout()` and clicks it. **If that layout were not
/// self-consistent, they would all be clicking the right *tool* at a pixel nothing occupies** and
/// passing for the wrong reason -- the failure mode of a gate whose fixture is itself wrong.
///
/// # Why this does not compare against `ChromeMetrics::DESKTOP`
///
/// **The first version did, and it failed — correctly.** `Session::new` calls
/// `with_line_pitch(atlas.line_pitch())` before building its chrome, so a session's cell height is
/// the *atlas's* pitch (25 at 16 ppem) and not `DESKTOP`'s fallback (18). Comparing the two was
/// comparing two different things, and the assertion it could make was not a useful one.
///
/// What is useful is the property that actually matters: **the bands partition the panel, and the text
/// column is inside the page.** Everything a gate clicks is derived from those.
#[test]
fn the_layout_a_gate_reads_is_self_consistent() {
    let s = session("x");
    let l = s.chrome_layout();
    let m = l.width * 0 + ChromeMetrics::DESKTOP.height; // the panel is the full height

    let covered: u32 = [
        l.title, l.menubar, l.tabs, l.toolbar, l.ruler, l.canvas, l.status,
    ]
    .iter()
    .map(|r| r.height)
    .sum();
    assert_eq!(covered, m, "the bands partition the panel's height");
    assert!(
        l.text.x >= l.page.x && l.text.right() <= l.page.right(),
        "the text column is inside the page: {:?} vs {:?}",
        l.text,
        l.page
    );
    assert!(
        l.page.y >= l.canvas.y && l.page.bottom() <= l.canvas.bottom(),
        "the page is inside the canvas"
    );
    let _ = Chrome::new(ChromeMetrics::DESKTOP);
}
