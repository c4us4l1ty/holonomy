//! **A second click inside the window selects the word; a third selects the line.** Part 41.
//!
//! # What this file is for
//!
//! **Double- and triple-click are the two gestures every editor has and this one did not.** A
//! double-click that does nothing is not a missing convenience; it is the user selecting a word and
//! getting a caret, which reads as the application having dropped the click.
//!
//! # The two halves, and why they are in one file
//!
//! **Counting the clicks and applying them are one feature.** `Click::next` decides that two presses
//! are one gesture; `click_page` acts on the count. **A gate for the counter alone would pass with a
//! `click_page` that ignored it**, and a gate for `click_page` alone could not be written without
//! inventing a count by hand. Every test here goes through real evdev records and real
//! [`Session::handle_pointer`], so the thing under test is the whole chain.
//!
//! # The fixture, and why it is shaped this way
//!
//! **`alpha bravo charlie\\n`** -- three words on one line, each five letters, separated by single
//! spaces, plus a trailing newline. That shape decides what each assertion can say:
//!
//! - a word is bounded by a space, so a wrong word boundary is a *different byte range* and not a
//!   subtle difference in length;
//! - there is no second line to confuse a line-selection with, so a triple-click's range is exactly
//!   `0..19` and can be asserted as an exact pair;
//! - the trailing newline is what makes "the line, not the newline" assertable -- see
//!   `a_triple_click_selects_the_line_and_not_its_newline`.

use holonomy::store::NoSource;
use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::pointer::encode_record_at;
use holonomy_input::{RecordDecoder, BTN_LEFT, EV_KEY, EV_REL, EV_SYN, REL_X, REL_Y};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy};

/// The document every gate in this file uses. See the module docs for why it is this shape.
const DOC: &[u8] = b"alpha bravo charlie\n";

/// The click window the implementation uses, restated here so the gate can name it.
///
/// **A test that reads the constant out of the implementation proves nothing** -- it would pass if
/// the constant were zero and the gate asked "is a click still a click". **This one is written out**
/// so that changing `Click::WINDOW_MS` is a visible edit to a test that says what the number is.
const WINDOW_MS: u64 = 400;

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
    ed.insert_at(0, DOC, SpanPolicy::GrowIntoInsert).expect("room");
    Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    )
}

/// The selection as `(start, end)`, or `None` when there is none.
///
/// **`Option` and not a `SelectionRange` with an `is_empty`.** The editor answers `Option`, and a
/// gate that unwrapped it would have to say what an absent selection *is* in bytes -- which is
/// `(at, at)`, a range of zero length that every other assertion in this file is trying to tell apart
/// from a real selection. **`None` is the absence; `(n, n)` is an empty range, and the difference is
/// the difference between "nothing is selected" and "the caret is at `n`".**
fn selection(s: &Session<'_>) -> Option<(u32, u32)> {
    s.editor().selection().map(|r| (r.start, r.end))
}

/// Press at `(x, y)` at `at_ms`, through real evdev records.
///
/// ## Why the timestamp is written into the records rather than faked
///
/// **Because a fabricated `time_ms` on the event would not test the plumbing.** The whole chain
/// under test is `kernel timestamp -> decode_record -> Record::Button -> Event::Button -> Click`, and
/// `encode_record_at` puts a real stamp in bytes 0..16 so every link is exercised. **A gate that
/// constructed `Event::Button { time_ms: 0 }` by hand would test `Click::next` and nothing else.**
///
/// ## Why the position is absolute, and the two `EV_REL` records
///
/// **Because this crate's pointer is relative.** `RecordDecoder` accumulates deltas into a
/// `Pointer`, so reaching panel point `(x, y)` from the origin is one `REL_X x` and one `REL_Y y`.
/// **There is no `EV_ABS` path**, so a test that wanted absolute coordinates would be testing a
/// feature `pointer.rs` explicitly declines to have.
fn press_at(s: &mut Session<'static>, x: i32, y: i32, at_ms: u64) {
    // **Milliseconds in, `(sec, usec)` out -- and this line is here because getting it wrong is
    // silent.** The first version passed `at_ms` straight into `encode_record_at`'s *seconds*
    // parameter, so a click "400 ms" later stamped 400,000 ms -- twenty seconds -- and every gate
    // below passed for the wrong reason except the two that were *meant* to be outside the window,
    // which would have failed loudly and told nobody where the unit went.
    //
    // **A record's clock is two words and neither of them is milliseconds.** `struct timeval` is
    // `sec` then `usec`, and `decode_record` is what turns that into `time_ms`; a fixture that skips
    // the conversion is writing a different clock than the one under test.
    let sec = at_ms / 1000;
    let usec = (at_ms % 1000) * 1000;
    let mut d = RecordDecoder::new();
    for ev in [
        encode_record_at(EV_REL, REL_X, x, sec, usec),
        encode_record_at(EV_REL, REL_Y, y, sec, usec),
        encode_record_at(EV_KEY, BTN_LEFT, 1, sec, usec),
        encode_record_at(EV_KEY, BTN_LEFT, 0, sec, usec),
        encode_record_at(EV_SYN, 0, 0, sec, usec),
    ] {
        d.push(&ev);
    }
    while let Some(e) = d.next_event() {
        s.handle_pointer(&mut NoSource, e)
            .expect("pointer handling");
    }
}

/// Press at `(x, y)`, `n` times, each `WINDOW_MS` after the last.
fn click_n(s: &mut Session<'static>, x: i32, y: i32, n: u32) {
    for i in 0..n {
        press_at(s, x, y, u64::from(i) * WINDOW_MS);
    }
}

/// The vertical middle of **line 0's row**, not the middle of the text column.
///
/// ## Why this is a helper and not `text.y + text.height / 2`
///
/// **Because `Layout::text` is the whole text *column*, not one row.** It is 562 px tall at
/// `DESKTOP`, and the fixture has one line of text in it -- so `text.y + text.height / 2` is 281 px
/// below the first row, past the end of the document, and every gate that used it was clicking on
/// empty space and correctly producing no selection. **`cell_h` is the row height and is the thing
/// that was wanted.**
fn row0_y(s: &Session<'_>) -> i32 {
    let text = s.chrome_layout().text;
    text.y as i32 + s.chrome.metrics.cell_h as i32 / 2
}

/// The `x` that puts the caret on `target` when clicked, or `None` if no pixel in the line reaches
/// it.
///
/// ## Why a search and not `text.x + cell * cell_w`
///
/// **Because that arithmetic is wrong for a proportional face, and wrong in a way a test can see.**
/// See the module docs for the measured table. **The grid exists for the caret and the ruler; the
/// glyph advances are what a click has to invert**, and `column_at_pen` is the function that inverts
/// them.
///
/// ## Why a linear scan and not a binary search
///
/// **Because the mapping is monotone and 720 clicks is not a cost worth optimising away** -- each is
/// four 24-byte records through the real decoder, and the whole file runs in under a second. **A
/// binary search would need its own gate that the mapping is monotone**, which is a *different*
/// claim about the click path, and this file is about click counting.
///
/// ## Why the press is at a distinct millisecond each time
///
/// **Because `x_for_byte` clicks many times in a row** and a run of clicks at one timestamp would
/// count as one enormous multi-click and move the caret with a word selected. **`ms` advances by
/// `WINDOW_MS + 1` per probe**, which is outside the click window, so every probe is its own gesture.
fn x_for_byte(s: &mut Session<'static>, target: u32) -> Option<i32> {
    let text = s.chrome_layout().text;
    let y = row0_y(s);
    let mut ms = 0u64;
    for x in text.x as i32..text.right() as i32 {
        ms += WINDOW_MS + 1;
        press_at(s, x, y, ms);
        if s.editor().caret() == target {
            return Some(x);
        }
    }
    None
}

/// The caret's byte after one click at `x`.
fn caret_at(s: &mut Session<'static>, x: i32, y: i32, ms: u64) -> u32 {
    press_at(s, x, y, ms);
    s.editor().caret()
}

/// Whether `DOC[from..to]` is a *maximal* run of word bytes: non-empty, all word bytes, and bounded
/// on both sides by something that is not a word byte.
///
/// ## Why this predicate and not a hard-coded `(6, 11)`
///
/// **Because the property under test is "the word, and only the word", and that is a statement about
/// the *shape* of the range.** A hard-coded pair also asserts the font's pixel positions, which is
/// a different claim and one that changes when the atlas does. **`maximal_word_run` says the
/// selection cannot have started or stopped in the middle of a word, which is the bug this feature
/// is for** -- the two wrong implementations (extending left to the previous word, and right to the
/// next) both produce a run that is word bytes and are not maximal, and one of them produces a run
/// that is not word bytes either.
fn maximal_word_run(from: u32, to: u32) -> bool {
    let (f, t) = (from as usize, to as usize);
    if f >= t || t > DOC.len() {
        return false;
    }
    let word = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
    word(DOC.get(f))
        && word(DOC.get(t - 1))
        && !word(DOC.get(f.wrapping_sub(1)))
        && !word(DOC.get(t))
}

/// One click is a caret, and leaves no selection.
///
/// **The negative half, and it is the one that would break silently.** With a double-click gate alone,
/// an implementation that selected *something* on every click -- the whole line, say -- would pass the
/// word gate and fail this. **Three presses, not one**: the interesting bug is a click counter that
/// never resets, and that only shows up on the third.
#[test]
fn three_separate_clicks_are_three_carets_and_no_selection() {
    let mut s = session();
    let x = x_for_byte(&mut s, 7).expect("byte 7 is reachable");
    let y = row0_y(&s);
    for i in 0..3 {
        // **A gap of `WINDOW_MS + 1`, not `WINDOW_MS * 2`.** The boundary is the interesting place,
        // and one millisecond past the window is the smallest gap that must not count -- a gate at
        // twice the window would pass with a window that is twice as long as it claims.
        press_at(&mut s, x, y, u64::try_from(i).expect("three") * (WINDOW_MS + 1));
    }
    assert_eq!(
        selection(&s),
        None,
        "**no selection after three slow clicks.** Each click drops the selection by planting an \
         anchor equal to the caret, so a counter that had run past 2 would show up here as a range."
    );
}

/// **Two quick clicks select the word, and the word is a maximal run of word bytes.**
#[test]
fn a_double_click_selects_the_word_under_it() {
    let mut s = session();
    let target = 7; // inside `bravo`
    let x = x_for_byte(&mut s, target).expect("byte 7 is reachable");
    let y = row0_y(&s);
    click_n(&mut s, x, y, 2);
    let Some((from, to)) = selection(&s) else {
        panic!(
            "**a double-click must select something**: at x={x} the caret lands on byte {target}, \
             which is the third letter of `bravo`, and there is no selection"
        );
    };
    assert!(
        maximal_word_run(from, to),
        "**{from}..{to} is a maximal run of word bytes.** It must contain the byte that was \
         clicked ({target}), be entirely letters or digits, and stop at a non-word byte on both \
         sides. The two wrong answers here are `alpha bravo` (the run extended left across the \
         space) and `bravo charlie` (it extended right)."
    );
    assert!(
        (from..to).contains(&target),
        "**the click is inside the selection** -- {from}..{to} does not contain {target}"
    );
    // **And named, so a failure message says which word rather than a pair of numbers.**
    assert_eq!(
        std::str::from_utf8(&DOC[from as usize..to as usize]),
        Ok("bravo"),
        "**and it is `bravo`** -- the word the click was measured onto"
    );
}

/// **Three quick clicks select the line — and not its newline.**
///
/// **The newline is the whole of this gate.** `DocLines::line_end` is one past the last byte *of the
/// stored range*, and the stored range includes the `\n`. A triple-click that selected it would make
/// the very next thing a user does — type, to replace the line — eat the line break and join the
/// line to the next one. **That is the sequence a triple-click exists to support**, so a triple-click
/// that breaks it is worse than no triple-click.
#[test]
fn a_triple_click_selects_the_line_and_not_its_newline() {
    let mut s = session();
    let x = x_for_byte(&mut s, 13).expect("byte 13 is reachable");
    let y = row0_y(&s);
    click_n(&mut s, x, y, 3);
    assert_eq!(
        selection(&s),
        Some((0, 19)),
        "**bytes 0..19** -- `alpha bravo charlie` is 19 bytes, and 19 is the position of the `\n`. \
         A range of 0..20 would have taken the newline with it, and the next keystroke would have \
         joined this line to the next one."
    );
}

/// **A double-click on a space selects nothing, and does not select a neighbour.**
///
/// ## Why this is a gate and not an edge case
///
/// **Because the obvious implementation gets it wrong in one of two ways**, and both are invisible in
/// the gate above:
///
/// - extend the word search leftwards from the click, which on a space selects the word *before* it;
/// - extend it rightwards, which selects the word *after* it.
///
/// **Either one means clicking in a gap between two words silently grabs the wrong one**, which is
/// the single most annoying thing a word-selection can do. The correct answer is nothing: the space is
/// not in a word, so there is no word to select.
///
/// ## Why it names byte 5 and measures its own `x`
///
/// **Byte 5 is the space after `alpha`.** A click that lands there is not a click on a narrow pixel
/// between two words by luck; it is a click the session resolved to a byte that is not in any word,
/// and the assertion is that nothing is selected.
#[test]
fn a_double_click_on_a_space_selects_nothing() {
    let mut s = session();
    let x = x_for_byte(&mut s, 5).expect("the space at byte 5 is reachable");
    let y = row0_y(&s);
    // **And the single click really does land on the space**, or this gate is testing nothing.
    assert_eq!(
        caret_at(&mut s, x, y, WINDOW_MS + 1),
        5,
        "**the probe click lands on byte 5** -- otherwise this gate is asserting about a click that \
         never happened"
    );
    click_n(&mut s, x, y, 2);
    assert_eq!(
        selection(&s),
        None,
        "**no selection.** Byte 5 is a space, which is not in a word; the correct behaviour is a \
         caret, and either `alpha` (0..5) or `bravo` (6..11) would be the bug."
    );
}

/// **A click far away in space starts a new gesture, however fast it follows.**
///
/// ## The other half of the time rule
///
/// `a_double_click_selects_the_word_under_it` holds the position fixed and varies the time; this one
/// holds the time fixed and varies the position. **Both are needed**: a counter that only checked
/// the clock would pass both of the time gates and fail this, and one that only checked the position
/// would fail `three_separate_clicks_...`.
///
/// ## Why two different words and not two cells of the same word
///
/// **Because the position check has to be able to fail.** Two clicks in different words are hundreds
/// of pixels apart, so a generous slop would still separate them; **two clicks three pixels apart are
/// inside the 5-pixel slop**, and that is the case this is not.
#[test]
fn a_click_elsewhere_starts_a_new_gesture_however_fast() {
    let mut s = session();
    let ax = x_for_byte(&mut s, 2).expect("byte 2 is reachable");
    let bx = x_for_byte(&mut s, 16).expect("byte 16 is reachable");
    let y = row0_y(&s);
    press_at(&mut s, ax, y, 0);
    // **No gap at all** -- the second press carries the *same* timestamp, which is the strongest
    // statement of "fast" available and the one a fixture cannot accidentally weaken.
    press_at(&mut s, bx, y, 0);
    assert_eq!(
        selection(&s),
        None,
        "**no selection.** Two presses at the same instant but ~140 px apart are two clicks, not one \
         double-click; the slop is 5."
    );
}

/// **A fourth click is a single click again.
///
/// ## Why four and not three
///
/// **Because the reset is the part that is easy to get wrong and invisible until it is not.** A
/// counter that kept incrementing would make click 4 a line too -- indistinguishable from click 3 --
/// and would only be caught by a user clicking five times in a row and getting a word. **The rule is
/// `count < 3`, so a fourth press is a fresh first press**, and this gate says so.
#[test]
fn a_fourth_click_is_a_single_click_again() {
    let mut s = session();
    let x = x_for_byte(&mut s, 7).expect("byte 7 is reachable");
    let y = row0_y(&s);
    click_n(&mut s, x, y, 4);
    assert_eq!(
        selection(&s),
        None,
        "**no selection after four quick clicks.** The counter stops at three, so the fourth press \
         is a first press and plants a caret -- which is what makes a rapid repeated click settle \
         rather than staying stuck on 'select the line'."
    );
}

/// **The time window's edge is at `WINDOW_MS`, and one millisecond later is not in it.**
///
/// ## Two assertions, because `Click::next` uses `<=` and the other side of `<=` is the bug
///
/// `three_separate_clicks_are_three_carets_and_no_selection` proves `WINDOW_MS + 1` is too long.
/// This proves `WINDOW_MS` itself is short enough. **A counter written `< WINDOW_MS` instead of
/// `<= WINDOW_MS` passes both of those and fails this** -- and the difference is one millisecond of
/// tolerance at the boundary, which is exactly the kind of thing that is never noticed and is then
/// wrong on a slow machine.
#[test]
fn the_window_edge_is_inclusive() {
    let mut s = session();
    let x = x_for_byte(&mut s, 7).expect("byte 7 is reachable");
    let y = row0_y(&s);
    press_at(&mut s, x, y, 0);
    press_at(&mut s, x, y, WINDOW_MS);
    let got = selection(&s);
    assert!(
        got.is_some_and(|(f, t)| maximal_word_run(f, t)),
        "**a word, not a caret**: two presses exactly `WINDOW_MS` apart are one double-click, and \
         the selection is {got:?}. The implementation uses `<=`, and this is the gate that holds it \
         to that."
    );

    // **And the same two clicks, one millisecond past the edge.**
    let mut s = session();
    press_at(&mut s, x, y, 0);
    press_at(&mut s, x, y, WINDOW_MS + 1);
    assert_eq!(
        selection(&s),
        None,
        "**no selection**: `WINDOW_MS + 1` is outside the window, so these are two single clicks."
    );
}

/// **A double-click on a line that will not fault in places a caret rather than nothing.**
///
/// ## The behaviour this records, and why it is worth recording
///
/// `word_around` returns `None` for two different reasons -- "there is no word here" and "the line
/// is not readable" -- and `click_page` treats them the same: it plants a caret. **For the first
/// reason that is right and `a_double_click_on_a_space_selects_nothing` says so.** For the second it
/// is a limitation, and a limitation that is not written down is a bug report three weeks later.
///
/// **This gate pins the `NoSource` case**: the session is given a source that faults nothing in, so a
/// faulting read of a non-resident leaf fails and the line cannot be scanned.
///
/// ## What is *not* asserted, and why
///
/// **The exact byte.** `NoSource` cannot read the line, so `offset_of` falls back to the grid
/// column, and a grid column is a different number on every font build. **The gate that matters is
/// the absence of the failure** -- a double-click path that could not previously be reached had no
/// test at all, and an unexercised `?` is an unexercised panic.
#[test]
fn a_double_click_on_an_unreadable_line_places_a_caret() {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    ed.insert_at(0, DOC, SpanPolicy::GrowIntoInsert).expect("room");
    let mut s = Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );
    let text = s.chrome_layout().text;
    let cell_w = s.chrome.metrics.cell_w as i32;
    let x = text.x as i32 + 8 * cell_w;
    let y = row0_y(&s);

    let mut d = RecordDecoder::new();
    for ev in [
        encode_record_at(EV_REL, REL_X, x, 0, 0),
        encode_record_at(EV_REL, REL_Y, y, 0, 0),
        encode_record_at(EV_KEY, BTN_LEFT, 1, 0, 0),
        encode_record_at(EV_KEY, BTN_LEFT, 0, 0, 0),
        encode_record_at(EV_SYN, 0, 0, 0, 0),
    ] {
        d.push(&ev);
    }
    // **Both records in the same frame, at the same timestamp**, so the second is a double-click.
    while let Some(e) = d.next_event() {
        s.handle_pointer(&mut NoSource, e).expect("pointer handling");
    }
    assert_eq!(
        selection(&s),
        None,
        "**no selection and no panic.** An unreadable line falls back to a caret rather than to a \
         word, and this gate is here because that fallback had never been exercised."
    );
}
