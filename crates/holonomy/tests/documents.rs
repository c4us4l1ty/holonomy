//! **The document list: what `New`, `Close` and the rail's rows do to it.** 7 tests.
//!
//! # What this file is about
//!
//! Part 45 gave `Session` a document list. Before it, `ChromeState` held one `title` and a `docs`
//! vector that was `vec![title]` on every path, and three menu rows were unwired because they had
//! nothing to act on: **`Close` had no list to close from**, and **`Open` and `Import` had nowhere to
//! get a path from.**
//!
//! **The list is a model, and a model is only worth having if it is what the screen shows.** Every
//! claim below is stated against `ChromeState::docs` — the rail's rows — because that is the one place
//! a person sees it. A session holding three documents over a rail that lists one is not a smaller
//! feature; it is a lie, and it is the lie the title bar was already telling in part 39.
//!
//! # What the list is not
//!
//! **`parked` is memory, and that is deliberate.** A parked document's editor is fully resident —
//! [`Editor::text_faulting`] faults every leaf, which is what `adopt_document` does anyway — so
//! parking is a move of a value, not a container being kept open somewhere. **There is no write-back
//! and no `Edit > Revert`**, and a test that asserted the document survives the process would be
//! asserting something no code path provides.
//!
//! | what it proves | test |
//! | --- | --- |
//! | `New` keeps the document that was open | [`new_parks_the_document_that_was_open`] |
//! | and `Close` switches back to it | [`close_switches_to_the_most_recently_parked`] |
//! | and closing the last one leaves a blank | [`closing_the_last_document_leaves_a_blank`] |
//! | the rail lists every document, in order | [`the_rail_lists_every_document_in_order`] |
//! | and clicking a row adopts the document it names | [`clicking_a_rail_row_switches_the_painted_document`] |
//! | `Open` and `Import` are requests, not openings | [`an_open_row_is_a_request_not_an_opening`] |
//! | and both are refused under the seal | [`the_two_path_rows_are_refused_under_the_seal`] |

use holonomy::session::action_command;
use holonomy::store::NoSource;
use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::pointer::encode_record;
use holonomy_input::{RecordDecoder, BTN_LEFT, EV_KEY, EV_REL, EV_SYN, REL_X, REL_Y};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_render::menus::Action;
use holonomy_text::{Editor, SpanPolicy};

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

/// Click at an absolute panel point, through real evdev records.
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

/// **`New` does not throw the document away, and that is the whole of part 45's change to it.**
///
/// **Before, the row called `adopt_document` on a blank and the document on the page was gone.** The
/// row was honest about that — its comment said so — and a user who pressed it to start a new entry
/// lost the one they had. Now the document is parked: still resident, still in the list, and one click
/// on its rail row brings it back.
///
/// **The painted document is row 0 and the parked one follows it**, because `sync_docs` is the one
/// place that writes `docs` and it writes them in that order.
#[test]
fn new_parks_the_document_that_was_open() {
    let mut s = session();
    s.set_document("field notes");
    assert_eq!(s.document_count(), 1, "one document to start");
    assert_eq!(s.state.docs, vec!["field notes".to_string()]);

    s.apply_action(Action::New).expect("new");

    assert_eq!(s.document_count(), 2, "the document that was open is still open");
    assert_eq!(
        s.state.docs,
        vec!["untitled".to_string(), "field notes".to_string()],
        "**the rail lists the new document first and the parked one after it.**"
    );
    assert_eq!(s.state.title, "untitled", "and the title bar is the one on the page");
    assert_eq!(s.state.active_doc, 0, "and it is the active row");
}

/// **`Close` is the row that used to have no meaning, and it is the simplest thing in the list.**
///
/// **It pops the most recently parked document and paints it.** That is a switch, not a destroy: the
/// document being closed leaves the list, the one behind it comes forward, and nothing is written to
/// disk either way.
#[test]
fn close_switches_to_the_most_recently_parked() {
    let mut s = session();
    s.set_document("field notes");
    s.apply_action(Action::New).expect("new");
    assert_eq!(s.document_count(), 2);

    s.apply_action(Action::Close).expect("close");

    assert_eq!(s.document_count(), 1, "the closed document left the list");
    assert_eq!(
        s.state.docs,
        vec!["field notes".to_string()],
        "**the document that was parked is now the painted one.**"
    );
    assert_eq!(s.state.title, "field notes");
    assert_eq!(s.state.active_doc, 0);
}

/// **Closing the last document does not leave a window with nothing to paint.**
///
/// **This is the case the row's own comment used to rule out** — *"with none parked, the session
/// continues with a fresh blank"* — and it is stated separately because it is the one that would look
/// like a hang if it were wrong: a chrome with a title and no document.
#[test]
fn closing_the_last_document_leaves_a_blank() {
    let mut s = session();
    s.set_document("field notes");
    assert_eq!(s.document_count(), 1);

    s.apply_action(Action::Close).expect("close the only document");

    assert_eq!(s.document_count(), 1, "a session always has a document");
    assert_eq!(s.state.title, "untitled");
    assert_eq!(s.state.docs, vec!["untitled".to_string()]);
}

/// **The rail's rows and the session's documents are one list, not two.**
///
/// **Three `New`s from one document give four rows, in the order they were opened.** The order matters
/// and is not implied by the count: `parked` is a `Vec` with a defined push order, and a rail that
/// listed the same documents in a different order would be listing them in an order no person chose.
#[test]
fn the_rail_lists_every_document_in_order() {
    let mut s = session();
    s.set_document("one");
    s.apply_action(Action::New).expect("new");
    s.apply_action(Action::New).expect("new");
    s.apply_action(Action::New).expect("new");

    assert_eq!(s.document_count(), 4);
    assert_eq!(
        s.state.docs,
        vec![
            "untitled".to_string(),
            "one".to_string(),
            "untitled".to_string(),
            "untitled".to_string(),
        ],
        "**four documents, and the rail has four rows in the order they were opened.** The painted one \
         is row 0 and the parked ones follow it."
    );
}

/// **A rail row is a switch, and this is the test that says the row is wired to the document it names.**
///
/// **The click is a real evdev record against the row's own rect**, not a call into the session, so
/// the thing being exercised is the hit test, the dispatch and the adoption together. **The row is
/// found through `Layout::sidebar_doc`, which is the same function the hit test uses** — so this cannot
/// pass by clicking a point the hit test would read as something else.
#[test]
fn clicking_a_rail_row_switches_the_painted_document() {
    let mut s = session();
    s.set_document("field notes");
    s.apply_action(Action::New).expect("new");
    s.apply_action(Action::New).expect("new again");
    assert_eq!(
        s.state.docs,
        vec![
            "untitled".to_string(),
            "field notes".to_string(),
            "untitled".to_string()
        ],
        "the rail has the row this clicks"
    );

    let row = s
        .chrome_layout()
        .sidebar_doc(1)
        .expect("row 1 is on the rail");
    click(
        &mut s,
        (row.x + row.width / 2) as i32,
        (row.y + row.height / 2) as i32,
    );

    assert_eq!(
        s.state.title, "field notes",
        "**the painted document is the one row 1 names.** A row that changed the highlight and not the \
         document would pass every count and still be a lie."
    );
    assert_eq!(s.state.docs[0], "field notes");
    assert_eq!(s.document_count(), 3, "switching does not close anything");
}

/// **`Open` and `Import` do not open anything, and saying so is the point of the row.**
///
/// **The session can ask for a path to be opened and cannot supply one** — the sealed jail has no
/// `openat` on a user path and the developer window has no file dialog — so the row stores a request
/// and the *driver* answers it. `Session::take_open_request` is the hand-off, and this test is what
/// keeps the two halves from being written as if one of them did the work.
///
/// **`Import` is the same request with a different action**, and the driver tells them apart by it.
#[test]
fn an_open_row_is_a_request_not_an_opening() {
    let mut s = session();
    assert_eq!(s.take_open_request(), None, "nothing asked before a row is chosen");

    s.apply_action(Action::Open).expect("open");

    assert_eq!(
        s.take_open_request(),
        Some(Action::Open),
        "**the row is the request, and it carries which of the two it was.**"
    );
    assert_eq!(s.take_open_request(), None, "and taking it once clears it");
    assert_eq!(s.document_count(), 1, "asking for a path opened no document");

    s.apply_action(Action::Import).expect("import");
    assert_eq!(s.take_open_request(), Some(Action::Import));
    assert_eq!(s.document_count(), 1, "and neither does importing");

    // **Neither row is a keystroke.** The two that need a path are handled by the session rather than
    // routed to a command, which is why they are in `handled_by_session` in `tests/actions.rs`.
    assert_eq!(action_command(Action::Open), None);
    assert_eq!(action_command(Action::Import), None);
}

/// **Under the seal both path rows are refused by name, and the refusal is counted.**
///
/// **This is not a UI nicety.** The jail's seccomp filter has no `openat` on a user path, so the call
/// would be `SIGSYS` and the process would exit 137 — a menu row that kills the app. The refusal is in
/// `apply_action` because that is where the row is interpreted, and `pointer_inert` is where a gate
/// can see that it happened.
#[test]
fn the_two_path_rows_are_refused_under_the_seal() {
    let mut s = session();
    s.state.sealed = true;
    let before = s.stats.pointer_inert;

    s.apply_action(Action::Open).expect("open");
    s.apply_action(Action::Import).expect("import");

    assert_eq!(
        s.take_open_request(),
        None,
        "**nothing is asked for under the seal** — the request never leaves the row."
    );
    assert_eq!(
        s.stats.pointer_inert, before + 2,
        "**and both are counted as inert.** `pointer_inert` is 'a widget was pressed and nothing is \
         behind it', which is exactly what a sealed row is."
    );
    assert_eq!(s.document_count(), 1, "and no document was opened");
}
