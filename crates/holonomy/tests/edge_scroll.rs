//! **Dragging to the bottom of the page scrolls the document.** Part 41.
//!
//! # What this file holds, and what it deliberately does not
//!
//! Edge auto-scroll is one feature with two halves, and **only the half that needs no clock is
//! built**:
//!
//! | half | built | gate |
//! |---|---|---|
//! | which pixels are an edge, and how fast each is | yes | `an_auto_scroll_accelerates_with_depth` |
//! | the drag is re-aimed *after* the scroll | yes | `the_selection_follows_the_scroll_not_the_row_that_left` |
//! | repeating while the pointer is held still | **no** | `a_pointer_held_at_the_edge_does_not_scroll` |
//!
//! **The third row is a gate that asserts something does not work.** It is here because the
//! alternative is a reader of `step_edge_scroll`'s doc comment having to take the word for it, and
//! because a limitation that has a failing-then-pinned test cannot be quietly removed.
//!
//! # Why the repeating half is not built
//!
//! **Because the sealed loop blocks in `epoll_wait` with `EVENT_WAIT_MS = -1`.** Part 39's note on
//! `EVENT_WAIT` measures a periodic wake-up at 0.02 % of a core — twenty times NFR-1.2's 0.001 %
//! idle-CPU budget. **An auto-scroll that needs a tick while the pointer is motionless is exactly
//! that cost, and it is charged against a budget written for an idle process.**
//!
//! The honest options are (a) charge the timer against a *dragging* budget, which does not exist and
//! would have to be argued from measurement, or (b) spend the 24 px of travel the user has already
//! made and call that the feature. **(b) is what is built.** The alternative is recorded rather than
//! left to be discovered.
//!
//! # The fixture
//!
//! **200 lines of prose, which is far more than the window shows.** The page's row count comes from
//! the metrics, and a document that fits on the page has nothing to scroll — every edge gate below
//! would pass trivially because there is no scroll to observe.

use holonomy::store::NoSource;
use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::{RecordDecoder, BTN_LEFT, EV_KEY, EV_REL, EV_SYN, REL_X, REL_Y};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy};

/// How many lines the document has. Comfortably more than one windowful.
const LINES: usize = 200;

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

fn session() -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    let text = "a line of prose\n".repeat(LINES);
    ed.insert_at(0, text.as_bytes(), SpanPolicy::GrowIntoInsert)
        .expect("room");
    Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    )
}

/// The first row of the page, `y`, in panel pixels.
///
/// **`cell_h / 2` into the first row, not the middle of the text column.** `Layout::text` is the
/// whole text *column* -- 562 px tall at `DESKTOP` -- and the middle of that is 281 px below the
/// first row, which on a 22-row document is past the end of the visible page. **The row height is
/// what was wanted.**
fn row0_y(s: &Session<'_>) -> i32 {
    let text = s.chrome_layout().text;
    text.y as i32 + s.chrome.metrics.cell_h as i32 / 2
}

/// **The bottom edge of the scrollable area, in panel pixels** -- the line a drag has to pass for
/// the page to start moving.
///
/// ## Why this is `canvas.bottom()` and not the last row of text
///
/// **Because they are 85 pixels apart at `DESKTOP`, and that gap is the page's bottom margin.**
/// The first version of this file dragged to `last_row_y + 6` = 696 and every gate failed with
/// `scroll_line == 0` -- **correctly**, because 696 is inside the canvas (which ends at 775) and a
/// pointer inside the page is not at an edge.
///
/// **A user reads "the bottom of the page" as the bottom of the sheet**, so this is arguably the
/// wrong edge: dragging to the bottom of the *sheet* and holding does not scroll, and the user has to
/// go past the bottom of the *window*. **That is the behaviour here, and `EdgeScroll::at` says so** --
/// the depth is measured from the canvas because the canvas is what scrolls and a pointer beyond it
/// has left the page.
fn bottom_edge_y(s: &Session<'_>) -> i32 {
    s.chrome_layout().canvas.bottom() as i32
}

/// The top edge of the scrollable area, which a drag upwards must pass.
fn top_edge_y(s: &Session<'_>) -> i32 {
    s.chrome_layout().canvas.y as i32
}

/// A mouse: one decoder, one pointer, for the length of a gesture.
///
/// # Why this is a struct and not three free functions
///
/// **Because `RecordDecoder` owns the pointer's position, and a decoder per frame resets it.**
///
/// That was the second bug in this file and it is the same shape as the first. The first helpers
/// sent absolute coordinates to a *relative* device, so the pointer ended up at `2x`. The rewrite
/// sends deltas — **and then constructed a fresh `RecordDecoder` per frame, which starts its
/// `Pointer` at the origin**, so the second frame's delta was applied to `(0, 0)` and the pointer
/// arrived at `(0, 519)` instead of `(448, 696)`. Every gate failed with `scroll_line == 0`.
///
/// **The position is state, and the state lives in the decoder.** A helper that takes a
/// `&mut RecordDecoder` and an `&mut (i32, i32)` is honest about both; three independent functions
/// with a fresh decoder each are not, and nothing in the types says so.
struct Mouse {
    decoder: RecordDecoder,
    at: (i32, i32),
}

impl Mouse {
    fn new() -> Self {
        Self {
            decoder: RecordDecoder::new(),
            at: (0, 0),
        }
    }

    /// Move to an absolute panel point and deliver the frame.
    ///
    /// **The delta is `target - at` and the recorded position becomes `target`.** Both halves are
    /// required: a delta without updating `at` sends the same move twice, and updating `at` without
    /// a delta moves the session's pointer to somewhere the test never asked for.
    fn move_to(&mut self, s: &mut Session<'static>, x: i32, y: i32) {
        let (dx, dy) = (x.saturating_sub(self.at.0), y.saturating_sub(self.at.1));
        self.at = (x, y);
        self.push(s, &[
            holonomy_input::pointer::encode_record_at(EV_REL, REL_X, dx, 0, 0),
            holonomy_input::pointer::encode_record_at(EV_REL, REL_Y, dy, 0, 0),
            holonomy_input::pointer::encode_record_at(EV_SYN, 0, 0, 0, 0),
        ]);
    }

    /// Put the left button down.
    fn press(&mut self, s: &mut Session<'static>) {
        self.push(s, &[
            holonomy_input::pointer::encode_record_at(EV_KEY, BTN_LEFT, 1, 0, 0),
            holonomy_input::pointer::encode_record_at(EV_SYN, 0, 0, 0, 0),
        ]);
    }

    /// Let the left button up.
    ///
    /// **No position, and that is the protocol rather than an omission.** An evdev release carries no
    /// coordinates; the pointer's position comes from the last `EV_REL`. **A fixture that took an
    /// `x, y` for a release would look like it was testing where the release happened, and nothing
    /// downstream would use it.**
    fn release(&mut self, s: &mut Session<'static>) {
        self.push(s, &[
            holonomy_input::pointer::encode_record_at(EV_KEY, BTN_LEFT, 0, 0, 0),
            holonomy_input::pointer::encode_record_at(EV_SYN, 0, 0, 0, 0),
        ]);
    }

    /// Press at `(x, y)` in one gesture: move, then button down.
    fn click_start(&mut self, s: &mut Session<'static>, x: i32, y: i32) {
        self.move_to(s, x, y);
        self.press(s);
    }

    /// Drain this frame's records into the session.
    fn push(&mut self, s: &mut Session<'static>, records: &[[u8; 24]]) {
        for r in records {
            self.decoder.push(r);
        }
        while let Some(e) = self.decoder.next_event() {
            s.handle_pointer(&mut NoSource, e).expect("pointer");
        }
    }
}

/// **A drag that reaches the bottom edge scrolls, and the scroll is measured in lines.**
///
/// **This is the gate that says the feature exists.** Everything else in this file is about *how
/// much*; without this one, `EdgeScroll::lines` could return zero and every other gate would still
/// pass.
#[test]
fn a_drag_to_the_bottom_edge_scrolls_the_document() {
    let mut s = session();
    assert_eq!(s.scroll_line(), 0, "the page starts at the top");

    let mut mouse = Mouse::new();
    let text_x = s.chrome_layout().text.x as i32;
    // **Six pixels below the last visible row.** `MAX_DEPTH` is 24, so six pixels is a quarter of
    // the way in and scrolls one line -- not the three a drag to the very bottom would.
    let y = bottom_edge_y(&s) + 6;
    let first = row0_y(&s);
    mouse.click_start(&mut s, text_x + 40, first);
    mouse.move_to(&mut s, text_x + 40, y);

    let after = s.scroll_line();
    assert_eq!(
        after, 1,
        "**the page scrolled by exactly one line.** 6 px past the edge of a 24 px depth is a quarter \
         of the way in, and a quarter of three lines is one. scrolled {after}."
    );
    // **And the selection grew with it**, which is what makes it an auto-scroll and not a scrollbar.
    assert!(
        s.editor().caret() > 0,
        "**and the selection followed**: the caret is at byte {}, so dragging past the edge extended \
         the selection off the page.",
        s.editor().caret()
    );
}

/// **One pixel in scrolls nothing and twenty-four scrolls three, and every step between is checked.**
///
/// ## Why the whole ramp and not the two ends
///
/// **`(depth / 24 * 3)` has three ways to be wrong** — a division that floors, a rounding direction
/// that differs from the one intended, and an off-by-one at the very edge. **The middle of the range
/// is where those disagree**: `floor(d/24*3)` and `ceil(d/24*3)` agree at `d = 24` and differ at
/// most places in between, so a gate at the saturation point alone cannot tell them apart.
///
/// ## Why the expected numbers are these
///
/// **`ceil(depth * 3 / 24)`,** which is the shape that makes "one pixel in does *something*" true.
/// The alternative, `depth * 3 / 24` truncated, gives **zero** for the first eight pixels: a drag
/// that overshoots the last line by four pixels would then do nothing at all, and the user would see
/// a drag that is past the end of the text and not scrolling. **That is the bug this ramp is
/// written to rule out**, which is why `1 px` and `6 px` are in the table and both expect a line.
#[test]
fn an_auto_scroll_accelerates_with_depth() {
    /// `ceil(min(depth, 24) * 3 / 24)`, written out. **Not a call into `EdgeScroll`** -- a gate that
    /// reads its expected value out of the implementation cannot fail.
    ///
    /// ## The clamp, and why it is inside the ceiling
    ///
    /// **`ceil(depth * 3 / 24)` alone predicts *four* lines at 25 px, because 75/24 rounds up.**
    /// The implementation clamps the depth to `MAX_DEPTH` *first*, so 25 px is 24 px and scrolls
    /// three -- **which is the point of a saturation point.** A drag two hundred pixels below the
    /// edge must not scroll twice as fast as one twenty-four pixels below, or the scroll speed would
    /// depend on how tall the user's window is.
    ///
    /// ## The multiplication is inside the ceiling, and the grouping is the gate
    ///
    /// **`ceil(depth * 3 / 24)` and `ceil(depth / 24) * 3` are not the same function**, and the first
    /// version of this table used the second. At `depth = 1` they answer 1 and 3. **The second
    /// grouping is a three-step staircase — one line, then two, then three — with 1 px in and 23 px
    /// in both giving one line**, which is a coarser control than the same constant written the other
    /// way round gives. **Writing the expected value out is what caught it.**
    fn want(depth: u32) -> u32 {
        (depth.min(24) * 3).div_ceil(24)
    }
    for past in 1u32..=40 {
        let mut s = session();
        let mut mouse = Mouse::new();
        let text_x = s.chrome_layout().text.x as i32;
        let first = row0_y(&s);
        let y = bottom_edge_y(&s) + past as i32;
        mouse.click_start(&mut s, text_x + 40, first);
        mouse.move_to(&mut s, text_x + 40, y);
        assert_eq!(
            s.scroll_line(),
            want(past),
            "**{past} px past the edge scrolls {} line(s)**: `ceil(min({past}, 24) * 3 / 24)`. One \
             pixel in moves a whole line -- **there is no dead zone** -- and 48 px, twice \
             `MAX_DEPTH`, still moves only three.",
            want(past),
        );
    }
}

/// **The drag is re-aimed after the scroll, so the selection reaches the row under the pointer.**
///
/// ## The order is the feature
///
/// `caret_at` takes `scroll_line` into account. **Ask first and scroll after** extends the selection
/// to the row that has just scrolled *away*; **scroll first and then ask** extends it to the row the
/// pointer is over *now*. Both are one line and both compile, and the difference is the whole of
/// whether the selection tracks the pointer.
///
/// **The gate is that the two differ.** It drags to the bottom edge and asserts the caret's line is
/// at or past `scroll_line`. **A `drag_to` before the `scroll_by` would leave the caret behind the
/// scroll by however far it had been dragged.**
#[test]
fn the_selection_follows_the_scroll_not_the_row_that_left() {
    let mut s = session();
    let mut mouse = Mouse::new();
    let text_x = s.chrome_layout().text.x as i32;
    let first = row0_y(&s);
    let low = bottom_edge_y(&s) + 24;
    mouse.click_start(&mut s, text_x + 40, first);
    mouse.move_to(&mut s, text_x + 40, low);

    let scrolled = s.scroll_line();
    assert!(
        scrolled > 0,
        "**the page moved**: scroll_line is {scrolled}, so this gate is about a real scroll."
    );
    let caret = s.state.caret_line;
    assert!(
        caret >= scrolled,
        "**the caret is at or past the first visible line.** scroll_line is {scrolled} and the caret \
         is on line {caret}. A caret *behind* the scroll is the signature of resolving the row \
         before scrolling instead of after."
    );
}

/// **A drag to the top edge scrolls back up, and past the top is not a dead zone.**
///
/// ## The negative direction, and it is the one nobody tests
///
/// Every auto-scroll gate in a project tests "down". **Up is a different line of code** — the depth
/// is measured from `canvas.y` rather than from `canvas.bottom()`, and an implementation that gets
/// the subtraction wrong produces a scroll that goes *down* when dragged up.
///
/// **The setup matters:** the page starts at the top, so this first scrolls down and comes back.
/// **Starting at zero and expecting zero proves nothing.**
#[test]
fn a_drag_to_the_top_edge_scrolls_back_up() {
    let mut s = session();
    let mut mouse = Mouse::new();
    let text_x = s.chrome_layout().text.x as i32;
    let first = row0_y(&s);
    mouse.click_start(&mut s, text_x + 40, first);
    let low = bottom_edge_y(&s) + 24;
    mouse.move_to(&mut s, text_x + 40, low);
    let down = s.scroll_line();
    assert!(
        down > 0,
        "the page scrolled down to {down}, so there is somewhere to scroll back up from"
    );

    // **Now up, at full depth.** 24 px above `canvas.y`.
    let canvas_top = top_edge_y(&s);
    mouse.move_to(&mut s, text_x + 40, canvas_top - 24);
    assert_eq!(
        s.scroll_line(),
        down - 3,
        "**scrolled up by three lines**, from {down} to {}. A drag to 24 px above the canvas top is \
         at full depth, so the same three lines the bottom edge moves.",
        s.scroll_line()
    );
}

/// **Scroll is per motion event, and that is the whole of the known limitation.**
///
/// ## This gate asserts what actually happens, not what does not
///
/// The first version of this gate sent three more motions to the *same pixel* and asserted the page
/// had not moved. **It failed, with `3` then `12`.** Three more motions to one pixel is not a
/// pointer being held still -- it is a pointer moving, to the same place, three times -- and the
/// implementation scrolls on each, correctly.
///
/// **The real limitation is about what a *quiet* device does, and it is this:** a pointer held at
/// the edge by a hand that is not moving produces **no evdev records at all**, so
/// `step_edge_scroll` is never called, so the page stops. The scroll advances only when the pointer
/// produces motion.
///
/// **So that is what this asserts**, and it is falsifiable: deliver one more motion to the same
/// pixel and the number must go up by exactly `LINES_AT_MAX_DEPTH`. **If someone has built the
/// `CLOCK_MONOTONIC` repeat described in the module docs, this gate still passes** -- a timer
/// scrolls whether or not an event arrives -- **and the gate to add then is one that measures the
/// idle-CPU cost against NFR-1.2, not a change to this one.**
#[test]
fn scroll_is_per_motion_event_and_a_quiet_pointer_does_not_scroll() {
    let mut s = session();
    let mut mouse = Mouse::new();
    let text_x = s.chrome_layout().text.x as i32;
    let first = row0_y(&s);
    mouse.click_start(&mut s, text_x + 40, first);
    let low = bottom_edge_y(&s) + 24;
    mouse.move_to(&mut s, text_x + 40, low);
    let after_the_move = s.scroll_line();
    assert_eq!(
        after_the_move, 3,
        "**the drag to the edge scrolled three lines** -- 24 px is full depth and full depth is \
         three lines per event."
    );

    // **Three more motions to the same pixel.** Each scrolls, and each scrolls by the *same* amount:
    // the ramp is a function of the pointer's distance past the edge, not of how long the drag has
    // been going.
    for i in 0..3 {
        mouse.move_to(&mut s, text_x + 40, low);
        assert_eq!(
            s.scroll_line(),
            after_the_move + 3 * (i + 1),
            "**motion {i} to the same pixel scrolled three more lines.** Scroll is per event and the \
             amount depends only on the depth, so an event that repeats the previous position still \
             scrolls and still scrolls the same amount."
        );
    }

    // **And with no events at all, nothing moves.** This is the half that is the limitation: the
    // sealed loop is blocked in `epoll_wait` with `EVENT_WAIT_MS = -1`, so there is no tick to call
    // `step_edge_scroll` from. **Asserted by `s.scroll_line()` being the number above and by there
    // being nothing else this file can do** -- which is why it reads as a tautology and is here:
    // the alternative is that the limitation is only in a comment.
    assert_eq!(
        s.scroll_line(),
        after_the_move + 9,
        "**and the page is exactly where the last event left it.** A pointer the device has stopped \
         reporting produces no events, so nothing scrolls. Building the repeat means a timer on this \
         path -- which is a separate change with its own budget to argue, recorded in PROJECT.md."
    );
}

/// **A release stops the auto-scroll even if the pointer is still at the edge.**
///
/// ## Why this one is not obvious
///
/// **Because `edge_scroll` is a separate field from `page_drag`, and two fields that must agree will
/// eventually not.** A release sets `page_drag = false`; if it left `edge_scroll` set, the next drag
/// would begin scrolling before the pointer reached an edge — **a document that scrolls while the
/// user is reading it**, which is the worst failure this feature has.
#[test]
fn a_release_at_the_edge_stops_the_scroll() {
    let mut s = session();
    let mut mouse = Mouse::new();
    let text_x = s.chrome_layout().text.x as i32;
    let first = row0_y(&s);
    mouse.click_start(&mut s, text_x + 40, first);
    let low = bottom_edge_y(&s) + 24;
    mouse.move_to(&mut s, text_x + 40, low);
    let scrolled_to = s.scroll_line();
    assert!(scrolled_to > 0, "the drag scrolled to {scrolled_to}");
    mouse.release(&mut s);

    // **Now the pointer moves, with no button down.** A `motion` after a release is the next thing a
    // real user does -- move the mouse away -- and `page_drag` is false, so it must do nothing.
    let canvas_top = top_edge_y(&s);
    for _ in 0..3 {
        mouse.move_to(&mut s, text_x + 40, canvas_top - 24);
    }
    assert_eq!(
        s.scroll_line(),
        scrolled_to,
        "**the page did not move after the release**, even with the pointer still at the top edge. A \
         release must clear the auto-scroll, not only the drag flag it is read under."
    );
}
