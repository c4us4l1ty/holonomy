//! Toolbar behaviour, tested through the same pointer and hit-testing path as the UI.
//!
//! The toolbar exposes real formatting, zoom, insertion, mode and overflow actions. Five
//! placeholders still have no backing model: Search, Spellcheck, PaintFormat, FontSize and
//! Highlight. This file names and counts them rather than pretending they are commands.
//! The inactive Link button was removed: adding it back requires a URL/anchor model.
//!
//! A fresh session per tool prevents an open dropdown from swallowing the next test press.
//! Both wide and narrow layouts are checked so tools do not disappear behind a dead Overflow.

use holonomy::store::NoSource;
use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::pointer::encode_record_at;
use holonomy_input::RecordDecoder;
use holonomy_render::chrome::{ChromeMetrics, ChromeState};
use holonomy_render::widgets::{self, Open, Tool, TOOLBAR};
use holonomy_text::{Editor, SpanPolicy};

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        // **Two sizes**, because `SizeUp` is the zoom control and a one-size atlas refuses 125 %.
        let (a, _) = holonomy_assets::build_atlas(&[16, 20]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

/// A session over a three-word document.
fn session() -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    ed.insert_at(0, b"alpha bravo charlie\n", SpanPolicy::GrowIntoInsert)
        .expect("room");
    Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    )
}

/// The rect of `name` on the toolbar.
fn tool_rect(s: &Session<'_>, name: &str) -> holonomy_render::DamageRect {
    widgets::place_toolbar(&s.chrome_layout(), widgets::TOOLBAR_PAD)
        .into_iter()
        .find(|p| p.tool.name() == name)
        .unwrap_or_else(|| panic!("no tool named {name}"))
        .rect
}

/// Press the centre of `r`.
fn press_rect(s: &mut Session<'static>, r: holonomy_render::DamageRect) {
    press_at(s, (r.x + r.width / 2) as i32, (r.y + r.height / 2) as i32);
}

/// Press at an absolute panel point, through real evdev records.
fn press_at(s: &mut Session<'static>, x: i32, y: i32) {
    let mut d = RecordDecoder::new();
    for ev in [
        encode_record_at(holonomy_input::EV_REL, holonomy_input::REL_X, x, 0, 0),
        encode_record_at(holonomy_input::EV_REL, holonomy_input::REL_Y, y, 0, 0),
        encode_record_at(holonomy_input::EV_KEY, holonomy_input::BTN_LEFT, 1, 0, 0),
        encode_record_at(holonomy_input::EV_KEY, holonomy_input::BTN_LEFT, 0, 0, 0),
        encode_record_at(holonomy_input::EV_SYN, 0, 0, 0, 0),
    ] {
        d.push(&ev);
    }
    while let Some(e) = d.next_event() {
        s.handle_pointer(&mut NoSource, e).expect("pointer");
    }
}

/// Independently named placeholders: removing or wiring one changes this array on purpose.
/// Keep them in toolbar order so the observed inert set must match exactly.
const STILL_INERT: [Tool; 5] = [
    Tool::Search,
    Tool::Spellcheck,
    Tool::PaintFormat,
    Tool::FontSize,
    Tool::Highlight,
];

/// **Pressing every tool and counting the ones with nothing behind them.
///
/// ## Why a fresh session per tool
///
/// **Because a press that opens a popup swallows the next press as a dismissal.** One session for all
/// twenty tools meant that pressing `Zoom` (which opens) was followed by a press on `Style` that
/// landed on the popup's grab instead of on `Style` — **and `pointer_inert` counted four tools that
/// were perfectly well wired.** A measurement that counts the fixture is worse than no measurement,
/// because the number looks like a result.
///
/// ## Why the *press* and not a release
///
/// **Because a click is decided on the press.** `Session::press`'s own note says so, and a release
/// would be measuring a different gesture.
#[test]
fn every_tool_that_is_not_wired_is_one_of_the_five() {
    let mut inert: Vec<&'static str> = Vec::new();
    for &tool in TOOLBAR {
        let mut s = session();
        let before = s.stats.pointer_inert;
        let r = widgets::place_toolbar(&s.chrome_layout(), widgets::TOOLBAR_PAD)
            .into_iter()
            .find(|p| p.tool == tool)
            .expect("every tool is placed")
            .rect;
        press_rect(&mut s, r);
        if s.stats.pointer_inert > before {
            inert.push(tool.name());
        }
    }
    let expected: Vec<&str> = STILL_INERT.iter().map(|t| t.name()).collect();
    assert_eq!(
        inert, expected,
        "the only inert controls are {expected:?}, but {inert:?} were inert; a new control \
         needs an action or must be explicitly listed as a placeholder"
    );
}

/// **`SizeUp` and `SizeDown` are the zoom pair, and they change the type.**
#[test]
fn the_size_buttons_are_the_zoom_buttons() {
    let mut s = session();
    s.repaint_all().expect("paint");
    let cell_100 = s.chrome.metrics.cell_w;

    press_tool(&mut s, "size-up");
    assert_eq!(s.state.zoom_percent, 125, "**SizeUp is 125 %**");
    s.repaint_all().expect("paint");
    let cell_125 = s.chrome.metrics.cell_w;

    assert!(
        cell_125 > cell_100,
        "**and the measure grew**: cell_w {cell_100} -> {cell_125}. A size button that changed a \
         label and nothing else is the defect part 43 fixed for the zoom control."
    );
    press_tool(&mut s, "size-down");
    assert_eq!(
        s.state.zoom_percent, 100,
        "**and SizeDown is back to 100 %**"
    );
}

/// **Both ends of the pair clamp rather than wrapping, and neither is an error.**
///
/// ## Why this is a gate
///
/// **`zoom_step` walks the rank in `ZOOMS` and clamps it.** The alternative — wrapping — would make
/// `SizeUp` at 125 % jump to 100 %, **which is a zoom control that moves the wrong way**, and it is
/// the kind of thing a `clamp` in the wrong place produces.
#[test]
fn the_size_buttons_clamp_rather_than_wrapping() {
    let mut s = session();
    press_tool(&mut s, "size-down");
    assert_eq!(
        s.state.zoom_percent, 100,
        "**SizeDown at the bottom stays at the bottom**"
    );
    assert_eq!(
        s.stats.pointer_inert, 0,
        "**and is not counted as inert**: it pressed, and what it \
         did was nothing further"
    );

    press_tool(&mut s, "size-up");
    press_tool(&mut s, "size-up");
    assert_eq!(
        s.state.zoom_percent, 125,
        "**and SizeUp at the top stays at the top** rather than wrapping to 100 %"
    );
    assert_eq!(
        s.stats.pointer_inert, 0,
        "**and is not counted as inert either**"
    );
}

/// **`Overflow` lists the tools that did not fit, and nothing else.**
///
/// ## How the toolbar is made narrow
///
/// **`resize` to a width below the sum of the buttons.** At `DESKTOP` (1,280 px) all twenty fit; the
/// panel is driven to 480 px, which is `bugs/1.png`'s width — **the width at which part 36 found the
/// toolbar running off the right edge.**
#[test]
fn the_overflow_menu_lists_the_tools_that_did_not_fit() {
    let mut s = session();
    s.resize(480, 800).expect("shrink the panel");
    let hidden = s.state.overflow_tools.clone();
    assert!(
        !hidden.is_empty(),
        "**480 px hides something.** If it hides nothing, this gate is measuring nothing."
    );

    press_tool(&mut s, "overflow");
    eprintln!(
        "DBG after press open={:?} inert={} chrome={}",
        s.state.open, s.stats.pointer_inert, s.stats.pointer_chrome
    );
    assert_eq!(
        s.state.open,
        Some(Open::Tool(Tool::Overflow)),
        "**the overflow button opens its own dropdown**"
    );
    let items = Open::Tool(Tool::Overflow).items(&s.state);
    assert_eq!(
        items.len(),
        hidden.len(),
        "**one row per hidden tool**: {} hidden, {} rows",
        hidden.len(),
        items.len()
    );
    // **And the rows carry the right tools**, found through `Action` rather than through position --
    // which is the whole reason `Action::SelectTool` exists instead of a parallel `Action` per tool.
    for (row, &tool) in items.iter().zip(&hidden) {
        assert_eq!(
            row.action,
            Some(holonomy_render::menus::Action::SelectTool(tool)),
            "**row `{}` runs the tool it names**",
            row.label
        );
    }
}

/// **On a wide panel the overflow button opens nothing, and that is not inert.**
///
/// ## Why emptiness is not inertness
///
/// **Because the button pressed and there was nothing to show.** `pointer_inert` means *a widget with
/// no action behind it* — and there is one; it just has nothing to do at this width. **`the button
/// found nothing to do` and `the button does nothing` are different sentences**, and part 39's
/// `pointer_inert` count would have lumped them together.
#[test]
fn an_overflow_button_with_nothing_hidden_opens_nothing() {
    let mut s = session();
    s.repaint_all().expect("paint");
    assert!(
        s.state.overflow_tools.is_empty(),
        "**a 1,280 px panel hides nothing**, or this gate is measuring nothing"
    );
    press_tool(&mut s, "overflow");
    assert_eq!(s.state.open, None, "**so the button opens nothing**");
    assert_eq!(s.stats.pointer_inert, 0, "**and is not counted as inert**");
}

/// **`Mode` opens Editing, Suggesting and Viewing, and switching changes the state.**
#[test]
fn the_mode_button_opens_a_three_row_menu() {
    let mut s = session();
    press_tool(&mut s, "mode");
    assert_eq!(s.state.open, Some(Open::Tool(Tool::Mode)), "**Mode opens**");

    let items = Open::Tool(Tool::Mode).items(&s.state);
    assert_eq!(items.len(), 3, "Editing, Suggesting and Viewing all have rows");
    assert_eq!(items[0].label, "Editing");
    assert_eq!(items[1].label, "Suggesting");
    assert_eq!(items[2].label, "Viewing");
    assert!(items[0].checked, "Editing starts active");
    assert!(!items[1].checked && !items[2].checked);

    // Choose Viewing through the real popup hit test, not by writing the mode directly.
    let l = s.chrome_layout();
    let pop = widgets::popup(&l, &s.state, s.state.open).expect("the mode popup has rows");
    press_rect(&mut s, pop.rows[2]);
    assert_eq!(s.state.mode_index, 2, "**the state moved**");
    assert_eq!(s.mode_name(), "Viewing", "**and the mode name follows**");
    assert_eq!(s.state.open, None, "**and the menu closed**");
}

/// **Viewing refuses every edit, and counts the refusals separately from inertness.**
///
/// ## Why a separate counter
///
/// **`pointer_inert` is the count this project is trying to drive to zero.** A viewing mode where
/// typing is silently dropped is *correct*, and folding it into that counter would either make the
/// count permanently non-zero or make the mode lie. **`SessionStats::edits_readonly_refused` is its
/// own number.**
#[test]
fn viewing_mode_refuses_every_edit_and_counts_it() {
    let mut s = session();
    s.repaint_all().expect("paint");
    let before_text = s.text().expect("text").to_vec();

    // **Into viewing mode through the real UI**, not by setting the field.
    press_tool(&mut s, "mode");
    let l = s.chrome_layout();
    let pop = widgets::popup(&l, &s.state, s.state.open).expect("rows");
    press_rect(&mut s, pop.rows[2]);
    assert_eq!(s.mode_name(), "Viewing");

    // **Typing, backspace and delete-forward.**
    s.handle_event(holonomy_input::InputEvent::press(holonomy_input::KEY_X))
        .expect("the key is handled");
    s.apply(holonomy_input::Command::Backspace)
        .expect_err("**backspace must be refused**");
    s.apply(holonomy_input::Command::DeleteForward)
        .expect_err("**delete-forward must be refused**");

    assert_eq!(
        s.text().expect("text"),
        before_text.as_slice(),
        "**the document is byte-for-byte unchanged**"
    );
    assert_eq!(
        s.stats.pointer_inert, 0,
        "**and nothing was counted as inert**"
    );
    assert_eq!(
        s.stats.edits_readonly_refused, 3,
        "**and three refusals were counted** — one per refused edit. This is a policy, not a missing \
         action, and the number is its own."
    );
}

/// **Viewing still moves the caret, because a read-only view is still a view.**
#[test]
fn viewing_mode_still_moves_the_caret() {
    let mut s = session();
    s.repaint_all().expect("paint");
    s.caret_to(6).expect("caret");
    press_tool(&mut s, "mode");
    let l = s.chrome_layout();
    let pop = widgets::popup(&l, &s.state, s.state.open).expect("rows");
    press_rect(&mut s, pop.rows[2]);
    assert_eq!(s.mode_name(), "Viewing");

    // **Two moves, so "the caret still moves" is a measurement rather than a tautology.** One move
    // from 6 that lands back on 6 would pass a gate that only compared to the start.
    s.apply(holonomy_input::Command::Right).expect("right");
    assert_eq!(
        s.editor().caret(),
        7,
        "**the caret moved** -- viewing mode is read-only, not frozen"
    );
    s.apply(holonomy_input::Command::Home).expect("home");
    assert_eq!(
        s.editor().caret(),
        0,
        "**and again.** The refusal is on edits; movement is not an edit."
    );
    // **And back to editing, and it works.**
    press_tool(&mut s, "mode");
    let l = s.chrome_layout();
    let pop = widgets::popup(&l, &s.state, s.state.open).expect("rows");
    press_rect(&mut s, pop.rows[0]);
    assert_eq!(s.mode_name(), "Editing");
    s.apply(holonomy_input::Command::Insert('z')).expect("type");
    assert!(
        s.text().expect("text").contains(&b'z'),
        "**and editing works again** — a mode that could not be left would be a stuck session"
    );
}

/// **A mode index past the end falls back rather than panicking.**
///
/// **Stated as `ChromeState`, not `Session`,** because that is where the index lives and where a
/// state file's number arrives. The same rule already covers `style_index` and `font_index`.
#[test]
fn an_out_of_range_mode_index_falls_back() {
    let state = ChromeState {
        mode_index: 200,
        ..ChromeState::default()
    };
    assert_eq!(
        state.mode_name(),
        "Editing",
        "**a bad mode index reads as `Editing`**, not as a panic and not as a label from nowhere"
    );
}

/// Press the tool named `name`.
fn press_tool(s: &mut Session<'static>, name: &str) {
    let r = tool_rect(s, name);
    press_rect(s, r);
}
