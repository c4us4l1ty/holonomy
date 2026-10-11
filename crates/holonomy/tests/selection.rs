//! **A selection: made, drawn, extended, and replaced.** 11 tests.
//!
//! # What this file is
//!
//! Part 28. Before it there was no selection anywhere in the project, and two places said so in their
//! own docs rather than pretending otherwise:
//!
//! * `Session::click_page` — *"A click places the caret and does not clear the selection, because
//!   there is no selection yet. When one lands, the same line becomes the rule; a gate will be written
//!   then and not now, because a gate written for a feature that does not exist tests nothing."*
//! * `Session::emit_body_text`'s style pass — *"Turning existing text bold needs a range, and there is
//!   no range to apply it to."*
//!
//! **Both promises are kept here.** The first is now the rule `click_page` actually implements; the
//! second is [`a_selected_range_can_be_bolded`], which is what the selection was *for*.
//!
//! # The model, in one paragraph
//!
//! **`Editor` holds `Option<u32>` — the anchor — and the caret is the focus.** There is no second copy
//! of the caret anywhere, which is the point: part 22 found `ChromeState::caret_line` and
//! `editor.caret()` disagreeing for twelve phases because there were two fields for one fact.
//! **A selection is therefore `SelectionRange::between(anchor, caret)`, derived on demand, normalised
//! so the direction does not matter, and `None` when the two are equal** — which is what makes a plain
//! click collapse for free rather than by a rule someone has to remember.
//!
//! | what it proves | test |
//! | --- | --- |
//! | the model: anchor, normalisation, and the three states | [`a_selection_is_an_anchor_and_a_caret`] |
//! | a backwards drag is the same range | [`a_backwards_drag_is_the_same_range`] |
//! | a click with no movement selects nothing | [`a_click_that_does_not_move_selects_nothing`] |
//! | shift-click grows the existing one | [`shift_click_grows_the_selection_rather_than_replacing_it`] |
//! | **and it is painted** | [`a_selection_draws_a_highlight_where_its_bytes_are`] |
//! | and only where they are | [`a_selection_highlight_stops_at_the_end_of_its_bytes`] |
//! | and across lines | [`a_selection_across_two_lines_highlights_both`] |
//! | **drag: press, move, release** | [`a_drag_from_press_to_motion_selects_a_range`] |
//! | shift-arrow from the keyboard | [`shift_arrow_grows_the_selection_a_character_at_a_time`] |
//! | **typing replaces it** | [`typing_replaces_the_selection_rather_than_inserting_into_it`] |
//! | **and Bold styles it** — the thing selection was for | [`a_selected_range_can_be_bolded`] |

use holonomy::session::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::pointer::encode_record;
use holonomy_input::{Command, Event, RecordDecoder};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SelectionRange, SpanPolicy, STYLE_BOLD};

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

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

/// **The pointer, as the run loop drives it: evdev records, relative deltas, one real decoder.**
///
/// **Not `Event::Motion { x, y }` constructed directly.** `tests/pointer.rs` set that rule and the
/// reason is the same here: the struct bypasses delta accumulation, the clamping at the panel edge and
/// the `EV_SYN` framing — and **for a drag, delta accumulation is the entire mechanism.** A drag that
/// moves by `+40` from wherever the pointer already is is what the mouse sends; `Event::Motion` says
/// "it is at 40", which is a different claim.
struct Mouse {
    /// **One decoder for the whole gesture, and it is load-bearing.**
    ///
    /// `RecordDecoder` *carries the pointer position* -- `self.pointer.x` is accumulated from `REL_X`
    /// on the record and lives in the decoder, not in the session. **A fresh decoder per frame starts
    /// the pointer at the origin every time**, so "move to column 11" arrives as `Motion { x: 88,
    /// y: 0 }` -- eight pixels along the top of the panel rather than along the text row.
    ///
    /// **That failure is silent and it is the worst kind.** The events decode; the frame just lands in
    /// the wrong place; and a drag gate written that way asserts something about the origin instead of
    /// about the drag. The first draft of this fixture made a decoder per frame and both of its drag
    /// assertions failed with "the motion selected a range" -- **the panic of a fixture that never sent
    /// the gesture at all.**
    d: RecordDecoder,
    x: i32,
    y: i32,
}

impl Mouse {
    fn new() -> Self {
        // **At the origin**, like the decoder, because the first `REL_X` is a delta from it.
        Self {
            d: RecordDecoder::new(),
            x: 0,
            y: 0,
        }
    }

    /// Push records and drain the events, **on the decoder that has been accumulating all along**.
    fn send(&mut self, records: &[(u16, u16, i32)]) -> Vec<Event> {
        for (kind, code, value) in records {
            self.d.push(&encode_record(*kind, *code, *value));
        }
        self.d.push(&encode_record(holonomy_input::EV_SYN, 0, 0));
        let mut out = Vec::new();
        while let Some(e) = self.d.next_event() {
            out.push(e);
        }
        out
    }

    /// Move to `(x, y)`, as a delta from wherever the pointer already is.
    ///
    /// **Both axes in one frame**, which is what a mouse sends: `REL_X` and `REL_Y` in the same report,
    /// then one `EV_SYN`. Two frames would deliver the x first and the y second, and the pointer would
    /// pass through an intermediate position -- for a drag, that means the selection grows through the
    /// corner rather than straight to it.
    fn move_to(&mut self, s: &mut Session<'_>, x: i32, y: i32) {
        let (dx, dy) = (x - self.x, y - self.y);
        self.x = x;
        self.y = y;
        feed(
            s,
            &self.send(&[
                (holonomy_input::EV_REL, holonomy_input::REL_X, dx),
                (holonomy_input::EV_REL, holonomy_input::REL_Y, dy),
            ]),
        );
    }

    fn press(&mut self, s: &mut Session<'_>, x: i32, y: i32) {
        self.move_to(s, x, y);
        feed(
            s,
            &self.send(&[(holonomy_input::EV_KEY, holonomy_input::BTN_LEFT, 1)]),
        );
    }

    fn release(&mut self, s: &mut Session<'_>, x: i32, y: i32) {
        self.move_to(s, x, y);
        feed(
            s,
            &self.send(&[(holonomy_input::EV_KEY, holonomy_input::BTN_LEFT, 0)]),
        );
    }
}

/// Deliver events to `s`, as the run loop does.
///
/// **`NoSource`, not the editor's own leaves.** There is no container behind this session, so the
/// resident rope is the whole document and the source has nothing to fetch — which is the same
/// arrangement `tests/pointer.rs` uses, and the reason a selection over a container-backed document is
/// a separate question this file does not claim to answer.
fn feed(s: &mut Session<'_>, events: &[Event]) {
    for e in events {
        s.handle_pointer(&mut holonomy::store::NoSource, *e)
            .expect("pointer handling does not fail");
    }
}

// ---------------------------------------------------------------- the model

/// The three states: no anchor, an anchor equal to the caret, and an anchor away from it.
#[test]
fn a_selection_is_an_anchor_and_a_caret() {
    let mut ed = Editor::new();
    ed.insert_at(0, b"abcdef", SpanPolicy::GrowIntoInsert)
        .expect("room");

    assert_eq!(ed.selection(), None, "a new editor has no selection");
    assert!(!ed.has_selection());

    ed.select(1, 4).expect("select");
    assert_eq!(
        ed.selection(),
        Some(SelectionRange { start: 1, end: 4 }),
        "**the anchor is 1 and the focus is 4** -- two numbers and no stored focus"
    );
    assert_eq!(
        ed.caret(),
        4,
        "and the caret is the focus, from the rope's own cursor"
    );

    // **A caret move collapses it**, which is the rule `Editor::park` exists to enforce once.
    ed.caret_to(2).expect("caret");
    assert_eq!(ed.selection(), None, "an arrow key drops the selection");

    // **A degenerate range is `None`, not `Some(1, 1)`.** This is what makes a plain click collapse
    // for free: `select(at, at)` plants an anchor and then the model reports no selection.
    assert_eq!(
        SelectionRange::between(3, 3),
        None,
        "**a zero-length selection is not representable**"
    );
}

/// A drag that starts at the end and finishes at the start is the same selection.
#[test]
fn a_backwards_drag_is_the_same_range() {
    let mut ed = Editor::new();
    ed.insert_at(0, b"abcdef", SpanPolicy::GrowIntoInsert)
        .expect("room");

    ed.select(4, 1).expect("select backwards");
    assert_eq!(
        ed.selection(),
        Some(SelectionRange { start: 1, end: 4 }),
        "**normalised in the constructor**, so no consumer ever has a backwards range"
    );
    assert_eq!(
        ed.caret(),
        1,
        "and the caret is still at the focus, at the low end"
    );

    // **`clip_to` is symmetric**, which is what the highlighter relies on.
    let r = SelectionRange { start: 1, end: 4 };
    assert_eq!(
        r.clip_to(0, 2),
        Some((1, 2)),
        "clipped to a line holding the low half"
    );
    assert_eq!(
        r.clip_to(3, 6),
        Some((3, 4)),
        "and to a line holding the high half"
    );
    assert_eq!(r.clip_to(5, 6), None, "a line outside it gets nothing");
}

// ---------------------------------------------------------------- the pointer

/// Press and release without moving: nothing is selected.
#[test]
fn a_click_that_does_not_move_selects_nothing() {
    let mut s = session("alpha bravo charlie\n");
    let l = s.chrome_layout();
    // **On the page's first text row, inside the text column.**
    let (x, y) = ((l.text.x + 40) as i32, (l.text.y + 4) as i32);
    let mut m = Mouse::new();
    m.press(&mut s, x, y);
    assert!(
        !s.editor().has_selection(),
        "a click with no movement selects nothing"
    );
    assert_eq!(s.state.selection, None, "and the chrome state agrees");
    m.release(&mut s, x, y);
    assert!(
        !s.editor().has_selection(),
        "**and the release does not select anything either**"
    );
}

/// Shift-click grows the selection instead of replacing it.
#[test]
fn shift_click_grows_the_selection_rather_than_replacing_it() {
    let mut s = session("alpha bravo charlie delta\n");
    let l = s.chrome_layout();
    let y = (l.text.y + 4) as i32;
    let mut m = Mouse::new();
    m.press(&mut s, (l.text.x + 8) as i32, y);
    let anchor = s.editor().caret();

    // **Shift held.** The session already had a `ModifierState` and never read it for a pointer --
    // that is part 28's second finding, and this is the gate for it.
    set_shift(&mut s, true);
    m.press(&mut s, (l.text.x + 56) as i32, y);

    let sel = s
        .editor()
        .selection()
        .expect("shift-click selected something");
    assert_eq!(
        sel.start, anchor,
        "**the anchor stayed where the first click put it** -- shift-click grew the selection"
    );
    assert!(
        sel.end > sel.start + 1,
        "and the range covers the distance between the two clicks: {sel:?}"
    );
    assert_eq!(
        s.state.selection,
        Some(sel),
        "and the state the chrome paints agrees"
    );

    // **And a third plain click collapses it**, which is the half of the rule that is easy to get
    // wrong: if a plain click *extended* instead of replacing, there would be no way to clear a
    // selection without pressing something else first.
    set_shift(&mut s, false);
    m.press(&mut s, (l.text.x + 8) as i32, y);
    m.release(&mut s, (l.text.x + 8) as i32, y);
    assert_eq!(
        s.editor().selection(),
        None,
        "**a plain click collapses the selection**"
    );
}

// ---------------------------------------------------------------- the paint

/// A selection is actually painted.
///
/// **Asserted on pixels, not on state.** The model tests below assert numbers the session computed;
/// this one asks whether anything reached the frame, because **"the model has a selection" and "the
/// user can see which text they selected" are two claims and only the second is the feature.**
///
/// The comparison is against *the same document with nothing selected*, so the difference is the
/// highlight and nothing else.
#[test]
fn a_selection_draws_a_highlight_where_its_bytes_are() {
    let (with, without) = painted_pair("alpha bravo charlie\n", Some((6, 11)));
    let (n, box_) = difference(&without, &with);
    let m = with.chrome.metrics;
    let l = with.chrome_layout().text;

    assert!(
        n > 0,
        "**nothing was drawn for a selection** -- the model had one and the page did not. This is the \
         failure part 24 found for styles, and it would look exactly the same here."
    );
    assert_eq!(
        box_.0,
        l.x + 6 * m.cell_w,
        "**the changed pixels start at column 6** -- the `b` of `bravo`"
    );
    // **Inclusive, so the far edge is `width - 1`.** `difference` returns the largest x that differed,
    // and a five-cell highlight at x 368 ends at 407. **The first draft of this assertion said 408**,
    // which is one past the end of the highlight -- and would have passed for a highlight that was one
    // cell too wide if the fixture had been a cell narrower. Subtracting the `cell_w` back out is the
    // only way to say "and no further", and the off-by-one is why.
    assert_eq!(
        box_.2,
        l.x + 11 * m.cell_w - 1,
        "**and stop at the last cell of column 10** -- `bravo` and nothing more"
    );
    assert_eq!(
        box_.1, l.y,
        "and they start on the first text row, which is the line the selection is on"
    );
    assert_eq!(
        (box_.1, box_.3),
        (l.y, l.y + m.cell_h - 1),
        "**and the changed pixels fill exactly one line box**, origin to origin. A highlight is a \\
         rectangle the height of the row and not of the glyphs, so it starts at `text.y`."
    );
}

/// The highlight covers exactly the selected bytes and no more.
///
/// **`bravo` is bytes 6..11.** A highlight that ran to the end of the line would be the shape of bug
/// this file is here to catch: the emitter clipping at the *line's* end rather than the selection's.
#[test]
fn a_selection_highlight_stops_at_the_end_of_its_bytes() {
    let (with, without) = painted_pair("alpha bravo charlie delta\n", Some((6, 11)));
    let (_, box_) = difference(&without, &with);
    let m = with.chrome.metrics;
    let l = with.chrome_layout().text;

    // **`+ 1` because `difference`'s box is inclusive at both ends.** See the note in
    // `a_selection_draws_a_highlight_where_its_bytes_are`: a highlight five cells wide at x 368 ends
    // at 407, and `407 - 368` is 39.
    assert_eq!(
        box_.2 + 1 - box_.0,
        5 * m.cell_w,
        "**five cells for five letters** -- the highlight does not run on to `charlie` or `delta`, \
         which are on the same line and would look identical if it did"
    );
    assert_eq!(box_.0, l.x + 6 * m.cell_w, "starting where byte 6 lands");
}

/// A selection crossing a line boundary highlights both lines.
///
/// **Bytes 6..20 crosses the `\n` at 11**, so this is the case where `clip_to` has to do real work:
/// line 0 gets 6..11 and line 1 gets 0..8.
#[test]
fn a_selection_across_two_lines_highlights_both() {
    let (with, without) = painted_pair("alpha bravo\ncharlie delta\n", Some((6, 20)));
    let m = with.chrome.metrics;
    let l = with.chrome_layout().text;

    // **Per row, not one bounding box.** A bounding box says "somewhere in rows 0 and 1"; it cannot
    // tell a clip that stops at each line's own end from one that stops at the selection's end, because
    // both put ink in the same two rows. **The widths are the claim**: `bravo` is five cells and
    // `charlie ` is eight, so the two rows must measure differently.
    //
    // **And that is precisely the bug this gate exists for** -- a clip that ran to the end of each
    // *line* rather than the end of each *line's share of the selection* would put eight cells on both
    // rows and look, on a two-line selection, almost right.
    let widths = row_widths(&without, &with, l.y, l.y + 2 * m.cell_h, m.cell_h, m.cell_w);
    assert_eq!(
        widths[0],
        6 * m.cell_w,
        "**row 0 is six cells** -- `bravo` *and the newline's cell*. This is deliberate and the next \\
         paragraph is why; see `the_newline_cell_is_highlighted_on_purpose` below."
    );
    assert_eq!(
        widths[1],
        8 * m.cell_w,
        "**row 1 is eight cells** -- `charlie `, from the start of the line to byte 20"
    );
    assert_eq!(
        widths.iter().sum::<u32>(),
        14 * m.cell_w,
        "and together they are every one of the selection's 14 bytes: {widths:?}"
    );
}

/// **The newline's cell is highlighted, and that is the decision, not an accident.**
///
/// **This gate's first draft asserted five cells on row 0 and failed with `48` against `40`.** The
/// question it was really asking is whether a selection crossing a line break should light the last
/// cell of the row above, and the answer here is **yes**, for three reasons:
///
/// * **A selection is a byte range and `\\n` is a byte in it.** The highlight is computed by clipping
///   that range to each line, and the newline belongs to the row above it. Clipping to `line_end`
///   *excluding* the terminator would mean deciding that one byte in the range gets no cell, which is
///   a special case in the emitter for no reason.
/// * **It is what every other editor draws.** A browser selection running through a line break paints
///   to the end of that row. Stopping one cell short is a visible difference and not an improvement.
/// * **The glyphs stop but the highlight does not**, and that is not an inconsistency: `seg` is
///   `strip_terminator`'d because a `\\n` *has a glyph* in most faces and drawing it would put a mark at
///   the end of every line. The highlight has no glyph to suppress.
///
/// **Recorded as a gate rather than a comment because it is exactly the kind of decision a later
/// reader would "fix"**, and the fix would look like a correction.
#[test]
fn the_newline_cell_is_highlighted_on_purpose() {
    // **A selection of one character *before* the newline** is the contrast case: it must stop short,
    // or this gate cannot tell "the terminator is included" from "the row is always full".
    let (with, without) = painted_pair("alpha bravo\\ncharlie delta\\n", Some((0, 5)));
    let m = with.chrome.metrics;
    let l = with.chrome_layout().text;
    let widths = row_widths(&without, &with, l.y, l.y + 2 * m.cell_h, m.cell_h, m.cell_w);
    assert_eq!(
        widths[0],
        5 * m.cell_w,
        "`alpha` alone is five cells and does not reach the terminator: {widths:?}"
    );
    assert_eq!(
        widths[1], 0,
        "and row 1 is untouched, because the selection stopped on row 0"
    );
}

/// The width of the differing run in each of `rows` rows, starting at `top`.
///
/// **A row is a glyph line, not a damage rect.** `row_widths` walks pixel rows inside each line box
/// and reports the widest contiguous run of differing pixels in it, which is what a highlight is; a
/// highlight is one rectangle per row and its width is constant across the row, so the widest run is
/// its width and not an artefact of where the glyphs sit.
fn row_widths(
    a: &Session<'_>,
    b: &Session<'_>,
    top: u32,
    bottom: u32,
    row_h: u32,
    cell_w: u32,
) -> Vec<u32> {
    let mut out = Vec::new();
    let mut row = 0u32;
    let mut y = top;
    while y < bottom {
        let y_end = (y + row_h).min(bottom);
        let mut widest = 0u32;
        let mut run = 0u32;
        for x in 0..a.chrome.metrics.width {
            let differs = (y..y_end).any(|yy| a.frame().pixel(x, yy) != b.frame().pixel(x, yy));
            if differs {
                run += 1;
                widest = widest.max(run);
            } else {
                run = 0;
            }
        }
        out.push(widest);
        y = y_end;
        row += 1;
    }
    assert_eq!(
        row, 2,
        "this helper is written for the two-row case it is used for"
    );
    out
}

// ---------------------------------------------------------------- drag and keys

/// Press, move, release: the range between them is selected.
///
/// **The three events are the contract**, and this is the gate for the whole of `page_drag`.
#[test]
fn a_drag_from_press_to_motion_selects_a_range() {
    let mut s = session("alpha bravo charlie delta echo foxtrot\n");
    let l = s.chrome_layout();
    let y = (l.text.y + 4) as i32;
    let mut m = Mouse::new();

    m.press(&mut s, (l.text.x + 8) as i32, y);
    assert!(
        !s.editor().has_selection(),
        "the press alone selects nothing -- it plants the anchor"
    );

    m.move_to(&mut s, (l.text.x + 88) as i32, y);
    let sel = s.editor().selection().expect("the motion selected a range");
    assert!(
        sel.len() >= 8,
        "the range covers the pointer's travel: {sel:?}"
    );

    // **Growing while still down.** One motion is a thin gate for a drag: the bug this file's model
    // rules out is an anchor that never moves *because* a later motion planted a new one.
    m.move_to(&mut s, (l.text.x + 104) as i32, y);
    let grown = s.editor().selection().expect("still a range");
    assert_eq!(
        grown.start, sel.start,
        "**the second motion grew the far end and left the anchor alone**: {sel:?} then {grown:?}"
    );

    m.release(&mut s, (l.text.x + 104) as i32, y);
    assert!(
        s.editor().has_selection(),
        "**the release keeps the selection** -- it outlives the gesture, because the next thing the \\
         user does is type over it"
    );

    // **And after the release, motion no longer extends it.** The drag is over; a stray mouse move
    // with no button down must not keep growing the range. This is the assertion that catches a
    // `page_drag` that is set and never cleared.
    let before = s.editor().selection();
    m.move_to(&mut s, (l.text.x + 200) as i32, y);
    assert_eq!(
        s.editor().selection(),
        before,
        "**motion after the release is not a drag**, so the selection is unchanged"
    );
}

/// Shift-arrow extends a selection one character at a time.
#[test]
fn shift_arrow_grows_the_selection_a_character_at_a_time() {
    let mut s = session("abcdefghij\n");
    s.caret_to(4).expect("caret");
    assert!(!s.editor().has_selection());

    s.apply(Command::SelectRight).expect("shift-right");
    let one = s.editor().selection().expect("one character is selected");
    assert_eq!(one, SelectionRange { start: 4, end: 5 });

    s.apply(Command::SelectRight).expect("shift-right again");
    let two = s.editor().selection().expect("two characters are selected");
    assert_eq!(
        two,
        SelectionRange { start: 4, end: 6 },
        "**the anchor stayed at 4** -- which is the whole of what an extend is"
    );
    assert_eq!(s.editor().caret(), 6, "and the caret is the focus");

    // **A plain arrow collapses it**, which is `Editor::park`.
    s.apply(Command::Left).expect("left");
    assert_eq!(
        s.editor().selection(),
        None,
        "**a plain Left drops the selection**, because it is a caret move"
    );
}

// ---------------------------------------------------------------- what it is for

/// Typing over a selection replaces it.
#[test]
fn typing_replaces_the_selection_rather_than_inserting_into_it() {
    let mut s = session("alpha bravo charlie\n");
    s.select_at(6, 11).expect("select bravo");
    s.apply(Command::Insert('X')).expect("type");

    assert_eq!(
        String::from_utf8(s.text().expect("read")).unwrap(),
        "alpha X charlie\n",
        "**the five selected letters are gone and one is in their place**"
    );
    assert_eq!(
        s.editor().selection(),
        None,
        "and the selection is gone -- the insert cleared it"
    );
}

/// The reason selection exists: a selected range can be styled.
///
/// **This is the test `emit_body_text` asked for and could not have.** Its comment reads:
///
/// > Turning existing text bold needs a range, and there is no range to apply it to.
///
/// **`SpanMap::style_range` has taken `(start, end, flags, colour)` since phase 6.** The function was
/// there; the model had nothing to hand it.
#[test]
fn a_selected_range_can_be_bolded() {
    let mut s = session("alpha bravo charlie\n");
    s.select_at(6, 11).expect("select bravo");

    s.apply_action(holonomy_render::menus::Action::ToggleBold)
        .expect("bold");

    // **The bold is on `bravo` and only on `bravo`.** Part 24 measured that styles in the map were
    // never drawn; part 25 wired the toggle but only at the caret. This is the range case.
    let flags = |at: u32| s.editor().style_at(at).style_flags;
    assert_eq!(
        flags(6) & STYLE_BOLD,
        STYLE_BOLD,
        "**byte 6 -- the `b` of `bravo` -- is now bold**"
    );
    assert_eq!(
        flags(5) & STYLE_BOLD,
        0,
        "and byte 5 -- the space before it -- is not"
    );
    assert_eq!(
        flags(11) & STYLE_BOLD,
        0,
        "and byte 11 -- the `c` after the selection -- is not"
    );

    // **And the selection survives**, because restyling does not consume a selection the way typing
    // does. A second Bold press has to have a range to un-bold.
    assert!(
        s.editor().has_selection(),
        "**the selection is still there**, so Bold can be pressed again to undo it"
    );
}

// ---------------------------------------------------------------- helpers

/// Hold or release shift **through the real key path**.
///
/// **Not by writing `s.mods` directly**, even though it is reachable. `Session::dispatch` is the only
/// place `ModifierState` is updated, so going through it means this gate also covers the thing that
/// makes shift-click work at all: **a modifier key produces no `Command`, and if the update lived
/// behind the command lookup, pressing Shift would have updated nothing.** It does not -- `dispatch_into`
/// calls `mods.update` before `dispatch` -- and this is the gate that says so.
fn set_shift(s: &mut Session<'_>, down: bool) {
    let ev = if down {
        holonomy_input::InputEvent::press(holonomy_input::KEY_LEFTSHIFT)
    } else {
        holonomy_input::InputEvent::release(holonomy_input::KEY_LEFTSHIFT)
    };
    let _ = s.dispatch(ev);
}

/// How many pixels differ between two frames, and the box they differ in.
///
/// **Copied from `tests/style_render.rs` rather than shared.** Two test files having the same eight
/// lines is cheaper than a shared module in `tests/`, which would be a new crate-level arrangement for
/// one function.
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

/// A session with `text`, painted once, and a copy of it painted with nothing selected.
///
/// **Two frames rather than one tree walk**, because the claim is about *pixels*: a highlight the
/// emitter computed but the painter never drew is the failure this file exists to catch, and only a
/// frame comparison can tell the two apart.
fn painted_pair(text: &str, sel: Option<(u32, u32)>) -> (Session<'static>, Session<'static>) {
    let mut with = session(text);
    with.caret_to(0).expect("park");
    if let Some((a, b)) = sel {
        with.select_at(a, b).expect("select");
    }
    with.repaint_all().expect("paint with");

    // **The control's caret is at the selection's focus, not at 0.** Measured, not guessed.
    //
    // The first version of this fixture put both carets at byte 0 and measured a difference box of
    // `(97, 162, 415, 792)` where the highlight is only `(368, 162, 407, 186)`. **The extra pixels were
    // two unrelated things**: the caret itself had moved from column 0 to column 11, and the status bar
    // reads `Col 1` against `Col 12`.
    //
    // **That is part 22's fixture lesson for a third time** -- a comparison in which two things differ
    // at once measures their sum. Holding the caret still leaves exactly the highlight, which is the
    // thing the gate is about.
    let mut without = session(text);
    without
        .caret_to(sel.map_or(0, |(_, focus)| focus as usize))
        .expect("park the control's caret where the selected frame's is");
    without.repaint_all().expect("paint without");
    (with, without)
}
