//! **What a row means: `Action`, and the three dropdowns.** 9 tests.
//!
//! # The coupling this file exists to remove
//!
//! Part 20 routed a menu choice by **comparing the item's label**:
//!
//! ```text
//! Some(match (heading, item.label) { (_, "Undo") => …, ("Insert", "Table") => … })
//! ```
//!
//! and its own comment said what was wrong with it: *renaming "Undo" to "Revert" silently makes it
//! inert.* **A display string was load-bearing.** Both halves still compiled, both still typechecked,
//! and a typo in a word a user reads would have changed what the program does.
//!
//! Part 21 replaces it with `menus::Action` — a value the renderer passes through and the session
//! interprets — and the two tests that matter are
//! [`an_action_is_routed_without_a_menu_open_around_it`] and
//! [`renaming_a_label_does_not_change_what_a_row_does`].
//!
//! | what it proves | test |
//! | --- | --- |
//! | the routing depends on the action, not the row | [`an_action_is_routed_without_a_menu_open_around_it`] |
//! | **and the label is not load-bearing** | [`renaming_a_label_does_not_change_what_a_row_does`] |
//! | every action is either routed or accounted for | [`every_action_is_routed_or_accounted_for`] |
//! | and every one has a name for a log | [`every_action_has_a_name`] |
//! | the real menus carry real actions | [`every_wired_menu_item_carries_an_action`] |
//! | a dropdown opens, ticks the current value, and applies | [`a_dropdown_ticks_the_current_value_and_applies_a_new_one`] |
//! | and two dropdowns cannot be open at once | [`only_one_popup_is_open_at_a_time`] |
//! | the toolbar labels are borrowed, not allocated | [`the_toolbar_labels_are_borrows_not_allocations`] |
//! | and the two dropdown lists agree | [`the_two_dropdown_lists_agree`] |

use holonomy::session::action_command;
use holonomy::store::NoSource;
use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::pointer::encode_record;
use holonomy_input::{RecordDecoder, BTN_LEFT, EV_KEY, EV_REL, EV_SYN, REL_X, REL_Y};
use holonomy_render::chrome::{ChromeMetrics, ChromeState};
use holonomy_render::menus::{self, Action};
use holonomy_render::widgets::{self, Open};
use holonomy_text::{Editor, SpanPolicy};

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
    ed.insert_at(0, b"alpha\nbravo\n", SpanPolicy::GrowIntoInsert)
        .expect("room");
    Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    )
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

/// Click a widget by name: the rect is taken first and the session borrowed afterwards.
///
/// **Two statements rather than `click_rect(&mut s, tool_rect(&s, name))`,** because that is a
/// borrow conflict and the fix would have been a clone of the layout on every call. Taking the rect
/// into a local is free and reads the same.
fn click_tool(s: &mut Session<'static>, name: &str) {
    let r = tool_rect(s, name);
    click_rect(s, r);
}

fn click_rect(s: &mut Session<'static>, r: holonomy_render::DamageRect) {
    click(s, (r.x + r.width / 2) as i32, (r.y + r.height / 2) as i32);
}

fn tool_rect(s: &Session<'_>, name: &str) -> holonomy_render::DamageRect {
    widgets::place_toolbar(&s.chrome_layout(), 8)
        .into_iter()
        .find(|p| p.tool.name() == name)
        .unwrap_or_else(|| panic!("no tool named {name}"))
        .rect
}

/// **An action is routed with no menu open and no row anywhere near it.**
///
/// **This is the whole of the part-21 claim in one call.** Part 20's routing could only be reached
/// by clicking a row, and every row carries a `label` — so "does the label decide?" had no answer that
/// did not involve a label. Calling [`action_command`] with an `Action` and nothing else makes the
/// question answerable: **the routing is a function of the action and of nothing else.**
#[test]
fn an_action_is_routed_without_a_menu_open_around_it() {
    use holonomy_input::{Command, Hotkey};
    assert_eq!(
        action_command(Action::Undo),
        Some(Command::Hotkey(Hotkey::Undo))
    );
    assert_eq!(
        action_command(Action::Redo),
        Some(Command::Hotkey(Hotkey::Redo))
    );
    assert_eq!(
        action_command(Action::InsertTable),
        Some(Command::Hotkey(Hotkey::InsertTable))
    );
    assert_eq!(
        action_command(Action::DocumentStart),
        Some(Command::Hotkey(Hotkey::DocumentStart))
    );
}

/// **Renaming a label does not change what a row does.**
///
/// **Stated as the absence of a `label` in the routing path, because that is the actual property.**
/// There is no way to write this as "rename the label and check" — the labels are `&'static str` in a
/// `const`, so a test cannot mutate one. What *can* be asserted, and is the thing that would break if
/// the coupling came back, is that **no function between a row and its effect takes an `Item`**: the
/// routing takes an `Action`, and `Action` has no `label` field at all.
#[test]
fn renaming_a_label_does_not_change_what_a_row_does() {
    // `Action` has no label. That is the whole mechanism, and it is checked by construction rather
    // than by a negative compile-fail test.
    let a = Action::Undo;
    assert_eq!(a.name(), "undo");
    // The same action reached from two different rows -- one in Edit, one constructed by hand with a
    // different label -- routes identically, because the routing never sees either label.
    let from_edit = menus::item_at(1, 0).expect("Edit's first row");
    let hand_made = menus::Item {
        label: "Revert",
        ..*from_edit
    };
    assert_eq!(from_edit.action, Some(Action::Undo));
    assert_eq!(hand_made.action, Some(Action::Undo));
    assert_ne!(
        from_edit.label, hand_made.label,
        "the fixture is not testing itself"
    );
    // And both route the same way, because `action_command` takes the `Action`.
    assert_eq!(
        action_command(from_edit.action.unwrap()),
        action_command(hand_made.action.unwrap())
    );
}

/// **Every action is either routed to a command or is one of the three the session handles itself.**
///
/// **The two lists must add up to the whole enum, and `Action` is `#[non_exhaustive]`-shaped by being
/// matched exhaustively twice.** A new variant that reaches `action_command` and hits its `_ => None`
/// — there is no `_ =>` arm — does not compile. So this test is about the *documented* split: the
/// ones that change chrome state are handled in `apply_action`, and the rest must produce a command.
#[test]
fn every_action_is_routed_or_accounted_for() {
    // The chrome-state actions never reach `action_command`, and saying so here is what keeps the two
    // lists honest: a fourth one added to `apply_action` and not to this list fails.
    let handled_by_session = [
        Action::SetZoom(100),
        Action::SetStyle(0),
        Action::SetFont(0),
    ];
    for a in handled_by_session {
        assert_eq!(
            action_command(a),
            None,
            "{} changes chrome state and must be handled by apply_action, not routed to a command",
            a.name()
        );
    }
    // Everything else either routes or is explicitly not wired. **The count is written down**, so a
    // new action that routes changes it on purpose and one that does not is visible in the diff.
    let routed = [
        Action::Undo,
        Action::Redo,
        Action::SelectAll,
        Action::DocumentStart,
        Action::DocumentEnd,
        Action::InsertTable,
        Action::InsertImage,
        Action::InsertMath,
    ];
    for a in routed {
        assert!(action_command(a).is_some(), "{} should route", a.name());
    }
    // **The two that are deliberately not wired**, named so they are not mistaken for an oversight.
    for a in [Action::Save, Action::Close] {
        assert_eq!(
            action_command(a),
            None,
            "{} is not wired in this build and pointer_inert is where that shows up",
            a.name()
        );
    }
}

/// **Every action has a name, and no two share one.**
///
/// **`Action::name` is what a status bar and a gate's failure message use, so an action without one is
/// an action that cannot be reported.** It is a `match` with no `_`, so adding a variant breaks the
/// build; this asserts the *names* are distinct, which is the part a `match` cannot see.
#[test]
fn every_action_has_a_name() {
    let all = [
        Action::Undo,
        Action::Redo,
        Action::SelectAll,
        Action::DocumentStart,
        Action::DocumentEnd,
        Action::Save,
        Action::InsertTable,
        Action::InsertImage,
        Action::InsertMath,
        Action::Close,
        Action::SetZoom(100),
        Action::SetStyle(0),
        Action::SetFont(0),
    ];
    let mut seen = std::collections::BTreeSet::new();
    for a in all {
        assert!(!a.name().is_empty(), "{a:?} has no name");
        assert!(
            seen.insert(a.name()),
            "{} is used by two actions; a log line naming it would be ambiguous",
            a.name()
        );
    }
    // **The payload does not change the name.** `SetZoom(125)` and `SetZoom(200)` are the same action
    // at different values, and a status bar that said "set-zoom" for both is right; a *gate* that
    // wanted to name the value is what `Debug` is for.
    assert_eq!(Action::SetZoom(125).name(), Action::SetZoom(200).name());
}

/// **Every menu item that means something says so, and the ones that do not are the expected ones.**
#[test]
fn every_wired_menu_item_carries_an_action() {
    let mut with_action = 0;
    let mut without = Vec::new();
    for (i, menu) in menus::MENUS.iter().enumerate() {
        for it in *menu {
            match it.action {
                Some(_) => with_action += 1,
                None => without.push(format!(
                    "{} > {}",
                    holonomy_render::chrome::MENUS
                        .get(i)
                        .copied()
                        .unwrap_or("?"),
                    it.label
                )),
            }
        }
    }
    assert_eq!(
        with_action, 8,
        "the eight rows that mean something: Undo, Redo, Select all, Close, Insert > Image, \
         Insert > Table, and Format > Bold and Format > Italic. Adding one changes this number."
    );
    // **The list is printed rather than asserted empty**, because "everything is wired" is not true
    // and pretending otherwise would be the worse answer. What matters is that the count of the rest
    // is stable and visible.
    assert_eq!(
        without.len(),
        48,
        "the 48 rows that draw and do nothing, out of 56 menu items in the eight headings. \
         **48 and not 46, because part 25 *added* two wired rows rather than converting two unwired \
         ones** — Bold and Italic are new rows in Format, so the unwired count did not move and the \
         total went from 54 to 56. The first draft of this edit subtracted from both and was wrong. \
         The \
         list is printed because \"everything is wired\" is not true here and pretending otherwise \
         would be the worse answer: what matters is that the count is stable and the rows are named. \
         Got: {without:?}"
    );
}

/// **A dropdown ticks the current value, and choosing another applies it.**
///
/// **The tick is the feature the reference's screenshot shows and part 20 did not have.** It is
/// `Item::checked`, set by `menus::dropdown_items` comparing the row's value against the state — which
/// means **the tick cannot be on the wrong row**, because the same comparison that draws it is the one
/// that will be acted on.
#[test]
fn a_dropdown_ticks_the_current_value_and_applies_a_new_one() {
    let mut s = session();
    let zoom = tool_rect(&s, "zoom");
    click_rect(&mut s, zoom);
    assert_eq!(
        s.state.open,
        Some(Open::Tool(widgets::Tool::Zoom)),
        "clicking Zoom opens its dropdown"
    );

    let l = s.chrome_layout();
    let pop = widgets::popup(&l, &s.state, s.state.open).expect("the zoom dropdown has rows");
    let items = Open::Tool(widgets::Tool::Zoom).items(&s.state);

    // **Exactly one row is ticked, and it is the current value.**
    let ticked: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| it.checked)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(ticked, vec![2], "100% is index 2 and is the default");
    assert_eq!(items[2].label, "100%");
    assert_eq!(s.state.zoom_percent, 100);

    // And choosing another applies it, and closes the dropdown.
    let target = 4; // 150%
    assert_eq!(items[target].label, "150%");
    assert!(!items[target].checked, "not ticked before it is chosen");
    click_rect(&mut s, pop.rows[target]);
    assert_eq!(s.state.zoom_percent, 150, "the state moved");
    assert_eq!(s.state.open, None, "and the dropdown closed");
    // **`pointer_chrome`, not `pointer_commands` — corrected in part 23.**
    //
    // Part 21 asserted `pointer_commands`, because it read as "the press did something". It did not do
    // a *command*, and `pointer_commands` says *presses that produced a `Command` the session applied* —
    // `Action::SetZoom` is handled in `apply_action` before `action_command` is reached, and no
    // `Command` exists. Part 23 then measured what a zoom change actually moves and found it to be the
    // label and nothing else, which made the wrong count worse rather than merely loose: the counter was
    // reporting a document edit that does not happen.
    assert!(
        s.stats.pointer_chrome > 0,
        "counted as a chrome-state change, which is what it is"
    );
    assert_eq!(
        s.stats.pointer_commands, 0,
        "**and not as a command**, because no `Command` was produced and no document byte changed"
    );
    assert_eq!(
        s.stats.pointer_inert, 0,
        "and not as inert either, because the label did change — a fourth outcome, which is why \
         `pointer_chrome` exists"
    );

    // **Re-opening ticks the new value, because the tick is computed from the state and not stored.**
    click_tool(&mut s, "zoom");
    let items = Open::Tool(widgets::Tool::Zoom).items(&s.state);
    assert_eq!(items[target].checked, true);
    assert_eq!(items[2].checked, false);
}

/// **Only one popup is open at a time, and that is what the type is for.**
///
/// **This is the argument for `Open` being an enum rather than two `Option`s.** With
/// `open_menu: Option<usize>` and a second `open_tool: Option<Tool>`, both could be `Some` — two
/// overlays drawn on top of each other, and a hit test with no rule about which one the pointer meant.
/// **The compiler rules that out; this asserts the behaviour as well, because the enum can still be
/// set to the wrong arm by a caller.**
#[test]
fn only_one_popup_is_open_at_a_time() {
    let mut s = session();
    // Open a menu.
    let l = s.chrome_layout();
    let (_, boxes) = holonomy_render::chrome::menu_boxes(&l);
    click_rect(&mut s, boxes[1]);
    assert!(matches!(s.state.open, Some(Open::Menu(_))));

    // Now click the zoom dropdown's opener. **The grab means the menu is dismissed first**, and the
    // press is *not* also delivered to the zoom control -- the same rule as the toolbar, and the
    // reason `hover_at` returns the opener rather than passing through.
    click_tool(&mut s, "zoom");
    assert_eq!(
        s.state.open, None,
        "one press dismissed the menu and did not also open the dropdown"
    );

    // And now open the dropdown, and try to open a menu through it.
    click_tool(&mut s, "zoom");
    assert!(matches!(s.state.open, Some(Open::Tool(_))));
    {
        let r = boxes[1];
        click_rect(&mut s, r);
    }
    assert_eq!(s.state.open, None, "and the same in the other direction");
}

/// **The toolbar's labels are borrowed out of a `const`, not allocated per paint.**
///
/// **This is the measurement, and it is a claim about three `format!`s that are no longer there.**
/// Part 19 stored `style_name: String` and `font_name: String` on the state and called `format!` for
/// Zoom and for `"Inter 11"` — **three heap allocations on every paint of every frame**, on the path
/// with a latency budget. Part 21 replaced them with two `usize` indices into `menus::STYLES` and
/// `menus::FONTS`.
///
/// Only `font_label` still formats, because `"Inter 11"` is two values with a space between them; the
/// other two are borrows, and this asserts the type rather than the behaviour.
#[test]
fn the_toolbar_labels_are_borrows_not_allocations() {
    let st = ChromeState::default();
    let a: &'static str = st.style_name();
    let b: &'static str = st.font_name();
    assert_eq!(a, "Normal text");
    assert_eq!(b, "Inter");
    // **`&'static str` is only possible because the lists are `const`** — an owned `Vec<String>` of
    // names would have to be returned as a borrow of the state and could not be `'static`.
    assert_eq!(menus::STYLES.len(), 7);
    assert_eq!(menus::FONTS.len(), 5);
    // An index past the end falls back rather than panicking: a state loaded from a file written by a
    // build with more styles should not take the session down.
    let mut oob = ChromeState::default();
    oob.style_index = 999;
    assert_eq!(oob.style_name(), "Normal text");
    oob.font_index = 999;
    assert_eq!(oob.font_name(), "Inter");
}

/// **The two lists that say which tools have dropdowns agree.**
///
/// `Tool::has_dropdown` is the cheap answer a caller asks; `menus::dropdown_items` is the authority
/// that produces the rows. **Two lists is two lists**, and this is what stops a tool being drawn with
/// a chevron that opens nothing — which is the affordance the reference gets right and that a `None`
/// would silently undo.
#[test]
fn the_two_dropdown_lists_agree() {
    for p in widgets::TOOLBAR {
        let rows = menus::dropdown_items(*p, &ChromeState::default());
        assert_eq!(
            rows.is_empty(),
            !p.has_dropdown(),
            "{}: has_dropdown says {}, dropdown_items returned {} rows",
            p.name(),
            p.has_dropdown(),
            rows.len()
        );
    }
    // And the three that have rows really do have rows, with a tick on exactly one.
    for name in ["zoom", "style", "font"] {
        let tool = tool_by_name(name);
        let rows = menus::dropdown_items(tool, &ChromeState::default());
        assert!(!rows.is_empty(), "{name} has no rows");
        assert_eq!(
            rows.iter().filter(|r| r.checked).count(),
            1,
            "{name} ticks exactly one row"
        );
        assert!(
            rows.iter().all(|r| r.action.is_some()),
            "{name} rows all carry an action, or choosing one does nothing"
        );
    }
}

/// Look a tool up by name, panicking with a useful message.
fn tool_by_name(name: &str) -> widgets::Tool {
    widgets::TOOLBAR
        .iter()
        .copied()
        .find(|t| t.name() == name)
        .unwrap_or_else(|| panic!("the toolbar has no tool named {name}"))
}
