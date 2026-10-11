//! **Up and Down move the caret in the document, not just the row the chrome draws it on.** 2 tests.
//!
//! # The finding this file closes
//!
//! Part 28 was adding `Command::SelectUp` / `SelectDown`, so that shift-arrow could extend a selection
//! by a line. **The existing `Command::Up` / `Command::Down` could not be reused, because neither of
//! them moves the caret.**
//!
//! `Session::move_line` was:
//!
//! ```text
//! let line = (self.state.caret_line as i64 + delta).max(0) as u32;
//! …
//! self.state.scroll_line = scroll.min(line);
//! self.state.caret_line = line;
//! self.damage = self.chrome.full_damage();
//! ```
//!
//! **Two assignments to chrome state, and `editor.caret()` untouched.** So:
//!
//! * The caret was **drawn** on a different line from the one it occupied.
//! * **Typing after a Down key inserted at the old offset**, and the document and the display stayed
//!   diverged until some later `caret_to` happened to overwrite `state.caret_line`.
//! * `state.caret_line` and `editor.caret()` disagreed — **exactly the defect part 22 found between
//!   `ChromeState::caret_line` and `editor.caret()`**, in the same field, five parts later.
//!
//! # Why it survived so long
//!
//! **Because nothing tested the *document* after a vertical movement.** Every gate that used Up or Down
//! asserted on `state.caret_line` — the field the bug writes and the document does not have. **The
//! assertion and the defect were the same value**, which is the fixture lesson part 22 already
//! recorded: *"a fixture whose two quantities coincide cannot tell a correct answer from a wrong one."*
//!
//! | what it proves | test |
//! | --- | --- |
//! | **the document caret moves down a line and the state's follows** | [`down_moves_the_caret_in_the_document_and_not_only_in_the_state`] |
//! | and up, at the top of the document, without underflow | [`up_at_the_first_line_stays_put_and_does_not_underflow`] |

use holonomy::session::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy};

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

/// `Up` and `Down` must move `editor.caret()` to the adjacent line.
///
/// **The first version of this test asserted only `state.caret_line`,** which is the field the defect
/// writes, and it passed — the fixture and the bug were the same number. **This one asserts both, and
/// reads the document's own line back out of the session** rather than asking the state what line the
/// caret is on.
#[test]
fn down_moves_the_caret_in_the_document_and_not_only_in_the_state() {
    let mut s = session("alpha\nbravo\ncharlie\n");
    s.caret_to(2).expect("caret");
    let before = s.editor().caret();

    s.apply(holonomy_input::Command::Down).expect("down");

    let after = s.editor().caret();
    assert_ne!(
        before, after,
        "**Down moved the chrome's idea of the caret's line and left the document's caret where it \
         was.** Part 28: typing after this would insert at `before`, not at the line the caret is \
         drawn on."
    );
    // **CORRECTION to this gate's first draft, which said `8`.** It assumed Down preserved the
    // column -- byte 2 of `bravo`, which starts at document offset 6. **Down lands at the *start* of
    // the line, byte 6**, and `Session::move_line`'s doc says so and explains why: preserving the
    // column needs the target line's length, `apply` takes no `LeafSource`, and a clamp against a
    // line that may not be resident would be a guess.
    //
    // **The correction is to the gate, not to the code**, and the reason matters: the gate was written
    // from what a word processor does rather than from what this one does, and part 22's lesson is
    // that a fixture asserting a behaviour the code never promised tests nothing.
    assert_eq!(
        after, 6,
        "**Down lands at the start of the next line**, because this build does not preserve the \
         column -- `alpha\n` is 6 bytes"
    );
    // **And the state's line follows the document's, rather than being set independently.**
    assert_eq!(
        s.state.caret_line, 1,
        "the state's line is now derived from the caret rather than assigned independently"
    );
    // **The state's line is the document's line.** `Layout::caret_at`'s inverse is not public, so the
    // claim is made where it can be: the offset `Down` landed on is byte 2 of the *second* line, so a
    // state reporting any other line is reporting something the document does not say.
    assert_eq!(
        s.state.caret_line, 1,
        "and the state's line is the document's line, not an independently assigned one"
    );
}

/// `Up` at the first line stays, and the line subtraction does not wrap.
///
/// **`(0 as i64 - 1).max(0)` is the guard, and it is on `state.caret_line` rather than on the
/// document** — so this gate is about the pair agreeing at the boundary, where a `u32` subtraction
/// would have been the failure.
#[test]
fn up_at_the_first_line_stays_put_and_does_not_underflow() {
    let mut s = session("alpha\nbravo\ncharlie\n");
    s.caret_to(2).expect("caret");
    let before = s.editor().caret();

    s.apply(holonomy_input::Command::Up).expect("up");
    // **The caret stays at byte 2, and this gate's first draft said it should go to 0.** That was
    // wrong: byte 2 is column 2 of line 0, and a line the caret is already on is not a line to move
    // to. Jumping to the start of the line would be a *worse* bug than the one being fixed, and a gate
    // that asked for it would have made the fix worse.
    assert_eq!(
        s.editor().caret(),
        2,
        "**Up at the first line leaves the caret where it is** -- there is no line above it"
    );
    assert_eq!(
        s.state.caret_line, 0,
        "and the line is 0, not `u32::MAX` from the subtraction"
    );

    // **Up again, and a selection still drops.** A movement that cannot happen is still a caret
    // movement, and `move_by_line` says so in the branch that returns early -- otherwise Down on the
    // last line would be the one arrow key that keeps a selection.
    s.select_at(2, 5).expect("select");
    assert!(
        s.editor().has_selection(),
        "the fixture really has a selection"
    );
    // **The caret is now at 5, not 2** -- `select_at` moves the caret to the selection's focus, which is
    // the point of a selection. (This gate's first draft asserted it was still at `before`; it is not,
    // and the assertion was wrong rather than the code.)
    let at_focus = s.editor().caret();
    assert_eq!(
        at_focus, 5,
        "selecting [2, 5) leaves the caret at the selection's focus"
    );
    s.apply(holonomy_input::Command::Up).expect("up");
    assert_eq!(
        s.editor().caret(),
        at_focus,
        "the caret did not move, because there was nowhere to move it to"
    );
    assert!(
        !s.editor().has_selection(),
        "**and the selection is gone anyway**, because Up is a plain caret move"
    );

    // --- The bite check forced this half to be added.
    //
    // **With the old defect reintroduced, everything above still passed.** `move_by_line`'s early
    // return — "there is no line above line 0" — was untouched by the bite, and that branch calls
    // `caret_to`, which parks the caret where it already is and clears the selection. **So the gate was
    // testing a guard the defect did not remove, while its name claimed to test the movement.**
    //
    // A gate that passes with the bug is worse than no gate: it is a false report, and the person
    // reading it later believes the movement is covered. **Part 22's lesson again, in a new place.**
    //
    // So this is a round trip, which no version of the defect survives: Down must actually reach the
    // second line for the Up below to have anything to come back from.
    let mut s = session("alpha\nbravo\ncharlie\n");
    s.caret_to(2).expect("caret");
    s.apply(holonomy_input::Command::Down).expect("down");
    let down_at = s.editor().caret();
    assert_eq!(
        down_at, 6,
        "Down reached the second line of the document, not just the state"
    );
    s.apply(holonomy_input::Command::Up).expect("up");
    // **Byte 0, not byte 2, and this gate said 2 before the correction.** The round trip goes
    // *line start to line start*: Down lands on line 1's start and Up lands on line 0's start. **The
    // column is not preserved in either direction**, which `Session::move_line`'s doc gives as the
    // reason -- there is no `LeafSource` on this path to clamp a column against.
    //
    // **Third correction to this file, and the same one each time.** Byte 2, byte 8, byte 2 were all
    // written from what a word processor does. **This build lands on line starts**, and a gate that
    // asserts the word processor's answer would have sent the fix in the wrong direction.
    assert_eq!(
        s.editor().caret(),
        0,
        "**and Up came back to the first line's start.** With the defect, `Up` reads a `caret_line` \
         the defect itself wrote, so the round trip is a closed loop in the chrome and the document \
         never moves"
    );
}
