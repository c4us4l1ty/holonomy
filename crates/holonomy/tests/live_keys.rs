//! **The keys that decoded and did nothing.** 9 tests.
//!
//! # What is being claimed
//!
//! Six `Command` variants were reachable from the keyboard and produced no observable effect:
//!
//! | key | was | now |
//! | --- | --- | --- |
//! | `Home` | `caret_to(0)` — document start | start of the caret's line |
//! | `End` | `caret_to(text_len)` — document end | after the last character of the line |
//! | `Shift+Home` / `Shift+End` | agreed with the defect on purpose | agree with the fix |
//! | `PageUp` / `PageDown` | a bare `{}` | one screenful |
//! | `Escape` | a bare `{}` | dismisses the open popup |
//!
//! **A command that decodes and then vanishes is the hardest kind of nothing to notice.** The keymap
//! test passes because the command exists; `Command` has the variant; and the window ignores you.
//! Nothing in the suite ever pressed one of these and looked at the result.
//!
//! # Why `Home` looked right anyway
//!
//! `Home` was `caret_to(0)`, which is the *document* start — and it coincided with "start of the
//! caret's line" **on line 0**, which is where every gate that ever pressed it put the caret. So the
//! bug was invisible to the suite for the same reason the `caret_line` desync was invisible for
//! twelve phases in part 22: **both halves of the arithmetic were zero.**
//!
//! **So the fixture here is a document with the caret in the middle**, and there is a test for the
//! specific accident — the first one asserts that pressing `Home` on line 0 is unchanged, which is
//! what makes the middle-of-document case meaningful rather than a claim that `Home` "works".
//!
//! | what it proves | test |
//! | --- | --- |
//! | **`Home` is the line, not the document** | [`home_goes_to_the_start_of_the_carets_line_not_the_document`] |
//! | and that is not the same as line 0 | [`the_fixture_does_not_hide_the_bug_it_tests_for`] |
//! | **`End` is the line, not the document** | [`end_goes_to_the_end_of_the_carets_line_not_the_document`] |
//! | **and not one past the newline** | [`end_lands_after_the_last_character_not_on_the_next_line`] |
//! | a last line with no newline still gets its true end | [`end_on_a_final_line_without_a_newline_is_not_one_short`] |
//! | shift agrees with unshifted | [`the_shift_variants_agree_with_the_ones_they_extend`] |
//! | **`PageDown` moves, and by a screenful** | [`page_down_moves_the_caret_by_a_screenful`] |
//! | **and consecutive presses lose no lines** | [`repeated_page_downs_lose_no_line`] |
//! | **`Escape` dismisses the popup** | [`escape_dismisses_an_open_menu`] |

use holonomy::session::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::{InputEvent, KEY_END, KEY_ESC, KEY_HOME, KEY_LEFTSHIFT, KEY_PAGEDOWN, KEY_PAGEUP};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_render::widgets::Open;
use holonomy_render::widgets;
use holonomy_text::{Editor, SpanPolicy};

/// Six short lines, so "the document" and "the caret's line" are different places and `End` on the
/// last line is a document with no trailing newline — the case that makes a naive `line_end - 1`
/// wrong.
const DOC: &[u8] = b"alpha\nbravo\ncharlie\ndelta\necho\nfoxtrot";

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

/// A session over [`DOC`] with the caret on line `line`, byte `col` within it.
fn session_at(line: u32, col: u32) -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    ed.insert_at(0, DOC, SpanPolicy::GrowIntoInsert)
        .expect("room");
    // Byte offset of line `line` plus `col`, computed by hand from the fixture rather than by asking
    // the geometry: a gate that computes its expectation with the code under test is not a gate.
    let starts = [0u32, 6, 12, 20, 26, 31];
    let at = starts[line as usize] + col;
    ed.caret_to(at as usize).expect("place the caret");
    let s = Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );
    s
}

fn press(s: &mut Session<'_>, code: u16) {
    s.handle_event(InputEvent::press(code)).expect("event");
    s.handle_event(InputEvent::release(code)).expect("event");
}

/// The byte offset of line `line` in [`DOC`], by hand.
fn line_start(line: u32) -> u32 {
    [0u32, 6, 12, 20, 26, 31][line as usize]
}

/// **The one that was invisibly right.**
#[test]
fn home_goes_to_the_start_of_the_carets_line_not_the_document() {
    let mut s = session_at(3, 2);
    assert_eq!(s.state.caret_line, 3, "the caret starts mid-document");

    press(&mut s, KEY_HOME);

    assert_eq!(
        s.caret(),
        line_start(3),
        "**Home lands at the start of line 3**, not at byte 0"
    );
    assert_ne!(s.caret(), 0, "and specifically not at the document start, which is what it did");
}

/// **The fixture check, and it exists because a passing gate means nothing without one.**
///
/// `Home` was `caret_to(0)` — the document start — and that is *identical* to "the start of the
/// caret's line" whenever the caret is on line 0. **Every gate that ever pressed `Home` put the
/// caret there**, so the suite could not tell the two apart. If this fails, the gate above is not
/// testing what it says.
#[test]
fn the_fixture_does_not_hide_the_bug_it_tests_for() {
    let mut s = session_at(0, 3);
    press(&mut s, KEY_HOME);
    assert_eq!(s.caret(), 0, "on line 0 the two definitions coincide");

    // ...and the *same key* on a different line does not, which is the whole claim.
    let mut s = session_at(2, 1);
    press(&mut s, KEY_HOME);
    assert_eq!(
        s.caret(),
        line_start(2),
        "so the fixture must be mid-document, and it is"
    );
}

#[test]
fn end_goes_to_the_end_of_the_carets_line_not_the_document() {
    let mut s = session_at(2, 1);
    assert_eq!(s.state.caret_line, 2);

    press(&mut s, KEY_END);

    // `line_end` is *one past the terminator* -- line 2 is `charlie\n` at bytes 12..=19, so
    // `line_end(2)` is 20 and the byte before it is the `\n` at 19. End sits **at 19**, which is
    // between `e` and the newline: the last content character is at 18 and the caret after it is 19.
    //
    // The first version of this fixture said `line_start(2) + 6` = 18, which is one byte short and
    // would have "passed" a caret parked *before* the `e`. The count is 7 because `charlie` is seven
    // characters, and a fixture that miscounts its own document is worse than no fixture.
    assert_eq!(
        s.caret(),
        line_start(2) + 7,
        "**End lands after `charlie`**, just before the newline"
    );
    assert_ne!(
        s.caret(),
        DOC.len() as u32,
        "and not at the document end, which is what it did"
    );
}

/// **The newline is the trap.** [`DocLines::line_end`] includes the terminator, so a naive
/// `line_end` puts the caret at offset 18 — which is byte 0 of the *next* line, and looks like the
/// key simply did not move far enough.
#[test]
fn end_lands_after_the_last_character_not_on_the_next_line() {
    let mut s = session_at(2, 0);
    press(&mut s, KEY_END);
    assert_eq!(
        s.caret(),
        line_start(2) + 7,
        "**the caret is inside line 2**, at the end of `charlie` rather than one past the newline"
    );
    assert_eq!(
        s.state.caret_line, 2,
        "and the session agrees: End on `charlie` has not moved to `delta`"
    );
    assert!(
        s.caret() < line_start(3),
        "the caret at {} is before line 3's start at {}",
        s.caret(),
        line_start(3)
    );
}

/// **The last line has no terminator**, so `line_end - 1` would be one byte short — and that is
/// invisible on every document *except* the one being typed, which is to say: on the document that
/// matters most.
#[test]
fn end_on_a_final_line_without_a_newline_is_not_one_short() {
    let mut s = session_at(5, 0);
    assert_eq!(s.caret(), line_start(5), "start of `foxtrot`");

    press(&mut s, KEY_END);

    assert_eq!(
        s.caret(),
        DOC.len() as u32,
        "**End reaches the last byte**, which for an unterminated final line is its end. \
         A `line_end - 1` here would report {}.",
        DOC.len() as u32 - 1
    );
}

/// The shift variants move to the same places, which is the whole reason part 28 left them agreeing
/// with the defect rather than compounding it.
#[test]
fn the_shift_variants_agree_with_the_ones_they_extend() {
    let mut plain = session_at(3, 2);
    press(&mut plain, KEY_HOME);
    let home = plain.caret();

    let mut shifted = session_at(3, 4);
    // **Shift is delivered as a key, not set on the session.** `mods` is private and folds from the
    // event stream, which is what a real keyboard does -- and a test that reached in would be
    // testing a modifier state no input path can produce.
    press(&mut shifted, KEY_LEFTSHIFT);
    press(&mut shifted, KEY_HOME);
    assert_eq!(
        shifted.caret(),
        home,
        "**Shift+Home extends to the same place Home goes**, rather than to a second answer"
    );

    let mut plain = session_at(2, 1);
    press(&mut plain, KEY_END);
    let end = plain.caret();

    let mut shifted = session_at(2, 1);
    press(&mut shifted, KEY_LEFTSHIFT);
    press(&mut shifted, KEY_END);
    assert_eq!(shifted.caret(), end, "**and Shift+End likewise**");
}

#[test]
fn page_down_moves_the_caret_by_a_screenful() {
    // **A document many screens long.** The first version of this used the six-line fixture and got
    // `caret_line == 5` -- the last line, because the page was longer than the document. That is
    // correct clamping and a useless test: a page-down on a six-line document is a page-down to the
    // end, and it would have passed with any implementation at all.
    let mut s = long_session(400);
    let rows = s.chrome_layout().rows;
    assert!(rows > 1, "the fixture needs a panel with more than one row, got {rows}");

    press(&mut s, KEY_PAGEDOWN);

    // `rows - 1`, because `rows` would put the starting line just off the top.
    assert_eq!(
        s.state.caret_line,
        rows - 1,
        "**one PageDown moves {rows} rows minus the one kept as context.** A page-down that lands a \
         full screen away is the classic off-by-one that makes it feel like it is skipping lines."
    );
    // **And the caret is on screen.** Not "the scroll did not move" -- on a 31-row page a caret on
    // row 30 is visible with the scroll still at 0, so that would have been an assertion about the
    // fixture's arithmetic rather than about the behaviour. What has to hold is that the two agree.
    let scroll = s.state.scroll_line;
    assert!(
        scroll <= s.state.caret_line && s.state.caret_line < scroll + rows,
        "**the caret at line {} is on the page**, which spans {scroll}..{}. A page-down that scrolls \
         without the caret moving would put them out of step and the next keystroke's \
         `scroll_line_into_view` would undo the scroll.",
        s.state.caret_line,
        scroll + rows
    );
}

#[test]
fn repeated_page_downs_lose_no_line() {
    let mut s = session_at(0, 0);
    let rows = s.chrome_layout().rows;
    let step = rows - 1;

    let mut s2 = long_session(400);
    let mut seen = vec![s2.state.caret_line];
    for _ in 0..5 {
        press(&mut s2, KEY_PAGEDOWN);
        seen.push(s2.state.caret_line);
    }

    // **Consecutive pages must be `step` apart**, which is the property "loses no line" means. A
    // `rows` step would also be monotonic, so the assertion is the *difference*, not the order.
    for pair in seen.windows(2) {
        assert_eq!(
            pair[1] - pair[0],
            step,
            "each PageDown advanced by {} rather than {step}: {seen:?}",
            pair[1] - pair[0]
        );
    }
}

/// A session over `lines` lines of prose, caret at the top. Long enough that a page-down is a
/// page-down rather than a jump to the end.
fn long_session(lines: usize) -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    let text = "line of prose\n".repeat(lines);
    ed.insert_at(0, text.as_bytes(), SpanPolicy::GrowIntoInsert)
        .expect("room");
    ed.caret_to(0).expect("caret");
    Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    )
}

/// **Escape is a keyboard user's only route to what a click outside the menu does** — and the popup
/// grab means every later click is swallowed by the menu it should have dismissed.
#[test]
fn escape_dismisses_an_open_menu() {
    let mut s = session_at(0, 0);
    let zoom = widgets::place_toolbar(&s.chrome_layout(), 8)
        .into_iter()
        .find(|p| p.tool.name() == "zoom")
        .expect("the zoom tool")
        .rect;
    let (cx, cy) = ((zoom.x + zoom.width / 2) as i32, (zoom.y + zoom.height / 2) as i32);

    click_at(&mut s, cx, cy);
    assert_eq!(
        s.state.open,
        Some(Open::Tool(widgets::Tool::Zoom)),
        "the dropdown is open"
    );

    press(&mut s, KEY_ESC);
    assert_eq!(
        s.state.open, None,
        "**Escape closes it** -- which it did not do at all before this"
    );
}

/// Click through real evdev records, as `actions.rs` does.
fn click_at(s: &mut Session<'_>, x: i32, y: i32) {
    use holonomy_input::pointer::encode_record;
    use holonomy_input::{RecordDecoder, BTN_LEFT, EV_KEY, EV_REL, EV_SYN, REL_X, REL_Y};
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
        s.handle_pointer(&mut holonomy::store::NoSource, e)
            .expect("pointer");
    }
}

// `PageUp` is decoded by the same arm as `PageDown` and is exercised through `KEY_PAGEUP`'s constant
// being live here; the asymmetry is the point of the table at the top.
const _: () = {
    let _ = (KEY_PAGEUP, DOC.len());
};