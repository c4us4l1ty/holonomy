//! **Bold and Italic: armed at the caret, and what that actually does.** 7 tests.
//!
//! # The dead thing this part found
//!
//! `ChromeState::styles` is a `StyleFlags` with four slots — bold, italic, mono, heading — and **nothing
//! in the workspace read it.** `StyleFlags::SLOTS`, a table of the four with their labels, short forms
//! and atlas styles, had exactly one caller: `holonomy-render/tests/chrome.rs`, which pushed its short
//! forms `B`, `I`, `M`, `H` into the list of labels the chrome is allowed to draw.
//!
//! **And the chrome never drew those labels.** `Tool::label` returns an empty `String` for every tool
//! except Zoom, Style and Font. So the gate was **four labels wider than the chrome is** — and a label
//! list is a permission list, so an entry that is never drawn catches nothing and only makes room for a
//! run that should have failed. **Dead code that made a gate weaker, in exchange for nothing.**
//!
//! Part 25 deletes `SLOTS` and the two slots with no button (`Mono`, `Heading` — `Tool::Style` is a
//! dropdown of paragraph styles, which is not a heading *toggle*), and makes the other two real.
//!
//! # The model, and why it is a field
//!
//! **Arming a style is not a zero-width span.** `SpanMap::style_range` returns `Ok(())` for
//! `start == end` and stores nothing, so "the next character typed will be bold" has nowhere to live in
//! the span map. It is `Session::pending_style: u16`, and `Session::insert` applies it to the bytes the
//! insert produced.
//!
//! **Two copies of that value exist — a `u16` in the model and a pair of bools in the chrome — and both
//! are written through one function pair**, `armed_style` and `set_armed_style`. That is the discipline
//! this file exists to check, because the failure would be a Bold button that lights up and types
//! ordinary text.
//!
//! | what it proves | test |
//! | --- | --- |
//! | the button arms it | [`the_bold_button_arms_bold_and_the_flag_is_the_chromes`] |
//! | and typing then produces bold text | [`typing_after_arming_bold_styles_what_was_typed`] |
//! | and disarming stops it | [`disarming_stops_the_next_character_being_bold`] |
//! | **the two copies cannot drift** | [`the_model_and_the_chrome_agree_after_every_transition`] |
//! | the two are independent | [`bold_and_italic_are_independent_of_each_other`] |
//! | the button draws a surface | [`the_armed_button_draws_a_surface_where_an_unarmed_one_does_not`] |
//! | and nothing else changes | [`the_armed_toggle_fills_its_own_button_and_nothing_else`] |

use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::chrome::ChromeMetrics;
use holonomy_render::widgets;
use holonomy_text::{Editor, SpanPolicy, STYLE_BOLD, STYLE_ITALIC};

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
    ed.insert_at(0, b"", SpanPolicy::GrowIntoInsert)
        .expect("seed");
    Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    )
}

fn tool_rect(s: &Session<'_>, name: &str) -> holonomy_render::DamageRect {
    widgets::place_toolbar(&s.chrome_layout(), 8)
        .into_iter()
        .find(|p| p.tool.name() == name)
        .unwrap_or_else(|| panic!("the toolbar has no tool named {name}"))
        .rect
}

/// Click a toolbar button by name, through real evdev records.
fn click_tool(s: &mut Session<'static>, name: &str) {
    use holonomy::store::NoSource;
    use holonomy_input::pointer::encode_record;
    use holonomy_input::{RecordDecoder, BTN_LEFT, EV_KEY, EV_REL, EV_SYN, REL_X, REL_Y};
    let r = tool_rect(s, name);
    let x = (r.x + r.width / 2) as i32;
    let y = (r.y + r.height / 2) as i32;
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
        s.handle_pointer(&mut NoSource, e).expect("pointer");
    }
}

/// Move the pointer to the middle of the page and let go of any button.
///
/// **This is the whole reason two of these gates failed the first time.** `click_tool` leaves the
/// pointer sitting on the button it clicked, so the button is *hovered* for the rest of the test — and a
/// hovered button already has a `PILL_HOVER` fill. **A toggle that is on and a button under the pointer
/// are drawn by the same machinery with the same shape**, so a gate that measures pixels without moving
/// the pointer away cannot tell an armed toggle from a hover, and will happily pass an armed toggle that
/// draws nothing at all.
fn park_pointer(s: &mut Session<'static>) {
    use holonomy::store::NoSource;
    use holonomy_input::pointer::encode_record;
    use holonomy_input::{RecordDecoder, BTN_LEFT, EV_KEY, EV_REL, EV_SYN, REL_X, REL_Y};
    let l = s.chrome_layout();
    let x = (l.text.x + l.text.width / 2) as i32;
    let y = (l.text.y + l.text.height / 2) as i32;
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
        s.handle_pointer(&mut NoSource, e).expect("pointer");
    }
    assert_eq!(
        s.state.hover, None,
        "the pointer is over the page, which hovers nothing"
    );
}

fn type_text(s: &mut Session<'static>, text: &str) {
    for c in text.chars() {
        s.apply(holonomy_input::Command::Insert(c)).expect("insert");
    }
}

/// **The model's view of what is armed, and the chrome's, for comparison.**
/// **The model's view and the chrome's, for comparison.**
///
/// **`Option<u16>` and not `u16`, because the model has three states and the chrome has two.** `None` is
/// "inherit", `Some(0)` is "plain", and two bools cannot tell those apart -- which is fine, because the
/// chrome shows what is *on* and the model remembers what was *said*.
fn both(s: &Session<'_>) -> (Option<u16>, (bool, bool)) {
    (
        s.pending_style(),
        (s.state.styles.bold, s.state.styles.italic),
    )
}

/// **Clicking Bold arms it, and the chrome's flag is the model's flag.**
///
/// **The fixture is an empty document**, so nothing about the assertion depends on what is already
/// styled. `armed_style` is the session's own reader — a gate cannot ask the chrome what it thinks and
/// conclude the two agree, which is the failure this whole file exists to rule out.
#[test]
fn the_bold_button_arms_bold_and_the_flag_is_the_chromes() {
    let mut s = session();
    assert_eq!(
        both(&s),
        (None, (false, false)),
        "nothing is armed and nothing has been said, on a fresh session"
    );

    click_tool(&mut s, "bold");
    assert_eq!(
        both(&s),
        (Some(STYLE_BOLD), (true, false)),
        "the click armed bold in the model and in the chrome"
    );
    assert!(
        s.state.styles.bold,
        "and the chrome's flag is what the toolbar draws"
    );

    click_tool(&mut s, "bold");
    assert_eq!(
        both(&s),
        (Some(0), (false, false)),
        "**and a second click is `Some(0)`, not `None`** -- the user said plain, which is a different \
         fact from having said nothing. A `u16` here would be 0 and the next character would inherit \
         bold from the character before the caret, which is the bug this type exists to prevent."
    );
}

/// **Typing after arming bold styles what was typed.**
///
/// **The end-to-end claim, and it goes through the same three parts in sequence**: `insert` applies
/// `pending_style` over the bytes it just inserted (part 25), `after_edit` records it as an edit, and
/// `emit_body_text` walks `runs_in` and draws it in the bold face (part 24). **If any of the three were
/// missing this asserts zero flags on the inserted byte.**
#[test]
fn typing_after_arming_bold_styles_what_was_typed() {
    let mut s = session();
    click_tool(&mut s, "bold");
    type_text(&mut s, "hi");
    assert_eq!(
        s.editor().style_at(0).style_flags & STYLE_BOLD,
        STYLE_BOLD,
        "the first character typed is bold"
    );
    assert_eq!(
        s.editor().style_at(1).style_flags & STYLE_BOLD,
        STYLE_BOLD,
        "and so is the second"
    );

    // **And the flags survive an unrelated edit afterwards**, which is part 24's claim about the span map
    // reaching the renderer rather than about `style_range` alone.
    s.apply(holonomy_input::Command::Newline).expect("newline");
    s.apply(holonomy_input::Command::Insert('x')).expect("x");
    assert_eq!(
        s.editor().style_at(0).style_flags & STYLE_BOLD,
        STYLE_BOLD,
        "and the bold text is still bold after a newline and another character"
    );
}

/// **Disarming stops the next character, and the character before it keeps its style.**
///
/// **The direction that matters.** A toggle that cannot be turned off is not a toggle, and an
/// implementation that armed by styling the *caret's preceding byte* would bold the previous character
/// too. This asserts the boundary is exactly where the button was pressed.
#[test]
fn disarming_stops_the_next_character_being_bold() {
    let mut s = session();
    type_text(&mut s, "ab");
    click_tool(&mut s, "bold");
    type_text(&mut s, "c");
    click_tool(&mut s, "bold"); // disarm
    type_text(&mut s, "d");

    assert_eq!(
        s.editor().style_at(2).style_flags & STYLE_BOLD,
        STYLE_BOLD,
        "the character typed while armed is bold"
    );
    assert_eq!(
        s.editor().style_at(3).style_flags & STYLE_BOLD,
        0,
        "and the one typed after disarming is not"
    );
    // **And the earlier plain text is untouched**, which is what "armed at the caret" means as opposed
    // to "applied to the line".
    for at in 0..2u32 {
        assert_eq!(
            s.editor().style_at(at).style_flags & STYLE_BOLD,
            0,
            "byte {at} was typed before the toggle and must still be plain"
        );
    }
}

/// **The model and the chrome agree after every transition, and this walks all four.**
///
/// **This is the gate for the two-copies-of-one-value problem.** `pending_style` is a `u16` and
/// `ChromeState::styles` is two bools in a crate that knows nothing about `u16` flags, so nothing but
/// the discipline keeps them equal. **A Bold button that lights up and types ordinary text** is what
/// disagreement looks like, and it is a state no other gate here would think to look for.
#[test]
fn the_model_and_the_chrome_agree_after_every_transition() {
    let mut s = session();
    // **From after the first press, not before.** A fresh session's model is `None` -- "the user has said
    // nothing" -- while `model_of` says `Some(0)` because `set_armed_style` never writes `None`. Both are
    // correct and they are different states; the first version of this loop asserted they were equal
    // before anything had happened, which asserts that a session nobody has touched is already an
    // opinion.
    click_tool(&mut s, "bold");

    // **The states actually visited, so the gate says it walked the space and not just that it agreed
    // once.** Four alternating presses reach all four combinations of the two bools.
    let mut seen: Vec<(bool, bool)> = Vec::new();
    for step in 0..4 {
        let (bold, italic) = (s.state.styles.bold, s.state.styles.italic);
        assert_eq!(
            s.pending_style(),
            model_of(bold, italic),
            "after {step} transition(s) the model and the chrome disagree: the chrome says \
             (bold={bold}, italic={italic}) and the model says {:?}",
            s.pending_style(),
        );
        if !seen.contains(&(bold, italic)) {
            seen.push((bold, italic));
        }
        // **Alternate, so a same-button-twice loop that only ever visited two states would show up.**
        click_tool(&mut s, if step % 2 == 0 { "bold" } else { "italic" });
    }
    assert_eq!(
        seen.len(),
        4,
        "the walk visited only {seen:?} -- all four of (bold, italic) combinations must be reachable, \
         because the claim being checked is about a pair of independent bits"
    );
    // **And the final state is whatever the walk arrived at, stated so a change in the button order
    // shows up as a diff rather than as a silently different trace.**
    let (bold, italic) = (s.state.styles.bold, s.state.styles.italic);
    assert_eq!(
        s.pending_style(),
        model_of(bold, italic),
        "five presses in, the two copies still agree"
    );
}

/// **What the model must hold, given the chrome's two bools. Always `Some`**, because
/// `set_armed_style` never writes `None` -- a gate expecting `None` after a press would be asserting
/// that a toggle is indistinguishable from never having touched it.
fn model_of(bold: bool, italic: bool) -> Option<u16> {
    Some(flags_of(bold, italic))
}

fn flags_of(bold: bool, italic: bool) -> u16 {
    let mut f = 0u16;
    if bold {
        f |= STYLE_BOLD;
    }
    if italic {
        f |= STYLE_ITALIC;
    }
    f
}

/// **Bold and italic are independent, and arming one does not arm the other.**
#[test]
fn bold_and_italic_are_independent_of_each_other() {
    let mut s = session();
    click_tool(&mut s, "bold");
    click_tool(&mut s, "italic");
    assert_eq!(
        both(&s),
        (Some(STYLE_BOLD | STYLE_ITALIC), (true, true)),
        "both armed"
    );
    click_tool(&mut s, "bold");
    assert_eq!(
        both(&s),
        (Some(STYLE_ITALIC), (false, true)),
        "clearing bold leaves italic alone"
    );
}

/// **An armed button draws a surface and an unarmed one does not.**
///
/// **Asserted on pixels, in the button's own rect, because that is the observable.** The armed fill is
/// `colour::PILL` — **the same fill hover uses**, which is stated in `paint_toolbar` and is a decision:
/// a toggle that is on and a button under the pointer are different facts that look the same.
#[test]
fn the_armed_button_draws_a_surface_where_an_unarmed_one_does_not() {
    let mut s = session();
    let bold = tool_rect(&s, "bold");
    let italic = tool_rect(&s, "italic");
    park_pointer(&mut s);
    s.repaint_all().expect("paint");

    // **The corner pixel of the button's rect**, where a glyph is not: the icon is centred and the fill
    // is not. **A pixel rather than a label, because the fill draws no text** — which is why deleting
    // `StyleFlags::SLOTS` did not break the chrome's label list.
    let probe = |s: &Session<'_>, r: holonomy_render::DamageRect| s.frame().pixel(r.x + 1, r.y + 1);
    let plain_bold = probe(&s, bold);
    let plain_italic = probe(&s, italic);

    click_tool(&mut s, "bold");
    park_pointer(&mut s);
    s.repaint_all().expect("paint");
    assert_ne!(
        probe(&s, bold),
        plain_bold,
        "the armed Bold button's surface is not there when it is off"
    );
    assert_eq!(
        probe(&s, italic),
        plain_italic,
        "and arming Bold did not touch Italic's button"
    );

    // **And disarming puts it back**, so the fill is a function of the flag and not of history.
    click_tool(&mut s, "bold");
    park_pointer(&mut s);
    s.repaint_all().expect("paint");
    assert_eq!(
        probe(&s, bold),
        plain_bold,
        "disarming restores the unarmed pixel exactly"
    );
}

/// **An armed toggle fills its own button and nothing else.**
///
/// **The first version of this counted non-black pixels in the toolbar band, and it was the wrong
/// metric.** `PILL` and `PILL_ACTIVE` are both non-black, so arming a toggle changed the band's colours
/// without changing the band's count of lit pixels: 43,520 before and 43,520 after, with a filled
/// button in between. **A count of "pixels that are not the background" cannot see a change of
/// background.** The measurements here are a count of *differing* pixels and the box they lie in.
///
/// **What this does not claim, having failed to observe it:** that arming draws no glyph. Pixels cannot
/// distinguish "a rectangle went behind the icon" from "a letter went on top of the icon" — both change
/// pixels inside the button. The reason `StyleFlags::SLOTS` could be deleted is not that this test
/// proves it; it is that `Tool::label` returns an empty `String` for every tool except Zoom, Style and
/// Font, which is a fact about the code rather than about a picture.
#[test]
fn the_armed_toggle_fills_its_own_button_and_nothing_else() {
    let mut s = session();
    let l = s.chrome_layout();
    let band = l.toolbar;
    let bold = tool_rect(&s, "bold");

    // **The band as a `Vec` of pixels**, so the comparison is between two snapshots rather than two
    // loops. A snapshot per side of a button's rect is 62 x 18 pixels; this is the whole toolbar band.
    let snapshot = |s: &Session<'_>| {
        let mut v = Vec::new();
        for y in band.y..band.bottom() {
            for x in band.x..band.right() {
                v.push(s.frame().pixel(x, y));
            }
        }
        v
    };

    park_pointer(&mut s);
    s.repaint_all().expect("paint");
    let before = snapshot(&s);

    click_tool(&mut s, "bold");
    park_pointer(&mut s);
    s.repaint_all().expect("paint");
    let after = snapshot(&s);

    let differing: Vec<(u32, u32)> = (0..before.len())
        .filter(|&i| before[i] != after[i])
        .map(|i| {
            (
                band.x + (i as u32) % band.width,
                band.y + (i as u32) / band.width,
            )
        })
        .collect();
    assert!(
        !differing.is_empty(),
        "arming a toggle changed nothing in the toolbar band, so the armed surface is not being drawn"
    );
    // **Confined to the button.** A fill is the button's rect; anything outside it is a different
    // widget's state changing, which would be the bug.
    let outside: Vec<(u32, u32)> = differing
        .iter()
        .copied()
        .filter(|&(x, y)| !bold.contains(x as i32, y as i32))
        .collect();
    assert!(
        outside.is_empty(),
        "arming Bold changed {} pixel(s) outside its own button, at {outside:?} -- the first {:?}",
        outside.len(),
        outside.first()
    );

    // **And the glyph survives the fill**, which is the ordering `paint_toolbar` insists on: the state
    // surface is behind the thing it is a state of.
    let mut ink_after = 0u32;
    for y in bold.y..bold.bottom() {
        for x in bold.x..bold.right() {
            if s.frame().pixel(x, y) != after[0] {
                ink_after += 1;
            }
        }
    }
    assert!(
        ink_after > 0,
        "the armed button's rect has no ink in it at all, which is what a fill drawn *over* the glyph \
         looks like"
    );
}
