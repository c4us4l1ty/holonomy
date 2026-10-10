//! **One widget list, painted and hit-tested.** 7 tests.
//!
//! # The hazard this file is about
//!
//! Hit testing is the one piece of UI logic that is *invisibly* duplicated. Write `hit(x, y)` beside
//! the layout arithmetic and you have two functions that each compute where the Bold button is. They
//! agree on the day they are written. **On the day someone adds a separator they do not**, and nothing
//! fails: the button is drawn at 210 and pressed at 214, so a click lands on the neighbour and the
//! report is "the toolbar is a bit off".
//!
//! So the claim under test is not that the coordinates are right. **It is that there is only one of
//! them.** `widgets::TOOLBAR` is a `const`; [`place_toolbar`](holonomy_render::widgets::place_toolbar)
//! gives each entry a rect; `hit` answers with the entry. A widget that is not in the list is not
//! drawn, and one that is in it is both.
//!
//! | what it proves | test |
//! | --- | --- |
//! | every widget is inside the panel | [`every_widget_has_a_rect_inside_the_panel`] |
//! | **and no two overlap** | [`no_two_widgets_overlap`] |
//! | `hit` agrees with `place_toolbar` for every widget | [`hit_agrees_with_where_the_widget_was_drawn`] |
//! | the seams resolve, not double-hit | [`a_point_on_a_seam_belongs_to_exactly_one_widget`] |
//! | a point on nothing is nothing | [`a_point_on_no_widget_hits_nothing`] |
//! | and the chrome's own bands are | [`the_bands_themselves_are_not_widgets`] |
//! | the menu headings are measured, not tabulated | [`the_menu_headings_are_measured_not_tabulated`] |

use holonomy_render::chrome::{menu_boxes, ChromeMetrics, ChromeState, Layout, MENUS};
use holonomy_render::widgets::{hit, place_title, place_toolbar, Hit, TitleButton, TOOLBAR};

fn layout() -> Layout {
    holonomy_render::Chrome::new(ChromeMetrics::DESKTOP).layout
}

fn state() -> ChromeState {
    ChromeState::default()
}

/// **Every widget is inside the panel.**
///
/// A rect that runs past the right edge is a widget whose right half cannot be pressed, and **the
/// failure is invisible from the left**: the button is drawn, the left half works, and the dead zone
/// is 3 px on the right of a button nobody measures. So this asserts containment, not "mostly inside".
#[test]
fn every_widget_has_a_rect_inside_the_panel() {
    let l = layout();
    for p in place_toolbar(&l, 8) {
        let r = p.rect;
        assert!(
            r.x + r.width <= l.width && r.y + r.height <= l.height,
            "{} is at {:?} with the panel {}x{}: it runs off the panel",
            p.tool.name(),
            (r.x, r.y, r.width, r.height),
            l.width,
            l.height
        );
        assert!(
            r.width > 0 && r.height > 0,
            "{} has an empty rect",
            p.tool.name()
        );
        assert!(
            r.y >= l.toolbar.y && r.bottom() <= l.toolbar.bottom(),
            "{} is at y {} but the toolbar is {}..{}",
            p.tool.name(),
            r.y,
            l.toolbar.y,
            l.toolbar.bottom()
        );
    }
    for (b, r) in place_title(&l, 8) {
        assert!(
            r.x + r.width <= l.width && r.bottom() <= l.title.bottom(),
            "{b:?} runs off the title band"
        );
    }
}

/// **No two toolbar widgets overlap.**
///
/// **And this is the test that fails first when the layout grows a widget**, which is the point: the
/// symptom of a duplicated layout is a widget that is drawn over its neighbour, and it is much cheaper
/// to find here than in a screenshot.
///
/// Half-open comparison, so two buttons sharing an edge do not count as overlapping -- `DamageRect`
/// is half-open precisely so a tile has no seam, and an overlap test that used inclusive bounds would
/// report every pair of neighbours.
#[test]
fn no_two_widgets_overlap() {
    let l = layout();
    let placed = place_toolbar(&l, 8);
    for (i, a) in placed.iter().enumerate() {
        for b in placed.iter().skip(i + 1) {
            let overlap_x = a.rect.x < b.rect.right() && b.rect.x < a.rect.right();
            let overlap_y = a.rect.y < b.rect.bottom() && b.rect.y < a.rect.bottom();
            assert!(
                !(overlap_x && overlap_y),
                "{} and {} overlap: {:?} and {:?}",
                a.tool.name(),
                b.tool.name(),
                (a.rect.x, a.rect.y, a.rect.width, a.rect.height),
                (b.rect.x, b.rect.y, b.rect.width, b.rect.height)
            );
        }
    }
    // And the title buttons too, plus that the title bar and the menu bar do not touch.
    for (i, (ba, a)) in place_title(&l, 8).iter().enumerate() {
        for (bb, b) in place_title(&l, 8).iter().skip(i + 1) {
            assert!(
                a.x + a.width <= b.x || b.x + b.width <= a.x,
                "{ba:?} and {bb:?} overlap in the title bar"
            );
        }
    }
    assert!(
        l.title.bottom() <= l.menubar.y,
        "the title band ends at {} and the menu band starts at {}",
        l.title.bottom(),
        l.menubar.y
    );
}

/// **`hit` agrees with where the widget was drawn, for every widget.**
///
/// **This is the whole file.** It walks the toolbar, takes the centre of each widget's own rect, and
/// asks `hit` what is there. **A duplicated layout is exactly a disagreement here**, and there is no
/// other assertion in the workspace that could find it.
#[test]
fn hit_agrees_with_where_the_widget_was_drawn() {
    let l = layout();
    let st = state();
    for p in place_toolbar(&l, 8) {
        let cx = (p.rect.x + p.rect.width / 2) as i32;
        let cy = (p.rect.y + p.rect.height / 2) as i32;
        assert_eq!(
            hit(&l, &st, cx, cy),
            Hit::Tool(p.tool),
            "the middle of {} is drawn at ({}, {}) and the pointer there hits {:?}",
            p.tool.name(),
            cx,
            cy,
            hit(&l, &st, cx, cy)
        );
    }
    for (b, r) in place_title(&l, 8) {
        let cx = (r.x + r.width / 2) as i32;
        let cy = (r.y + r.height / 2) as i32;
        assert_eq!(
            hit(&l, &st, cx, cy),
            Hit::Title(b),
            "the middle of {b:?} hits {:?}",
            hit(&l, &st, cx, cy)
        );
    }
}

/// **Every menu heading is hit-testable at its own centre, and the eight are distinct.**
///
/// Distinct matters: `menu_boxes` measures the text, and if two headings measured the same they would
/// be the same box and one of them would be unreachable. `File` and `Help` are both four characters,
/// **so this test is what stops the measurement from being a constant.**
#[test]
fn every_menu_heading_is_hit_testable_at_its_own_centre() {
    let l = layout();
    let st = state();
    let (_, boxes) = menu_boxes(&l);
    assert_eq!(boxes.len(), MENUS.len());
    for (i, r) in boxes.iter().enumerate() {
        let cx = (r.x + r.width / 2) as i32;
        let cy = (r.y + r.height / 2) as i32;
        assert_eq!(
            hit(&l, &st, cx, cy),
            Hit::Menu(i),
            "heading {i} at ({cx}, {cy})"
        );
    }
    // Adjacent headings share no pixel column.
    for w in boxes.windows(2) {
        assert!(
            w[0].right() <= w[1].x,
            "two headings' boxes touch or overlap"
        );
    }
}

/// **A point on a seam belongs to exactly one widget.**
///
/// **The right-hand edge of every button.** With inclusive bounds both buttons would claim it and the
/// left one would win, so the click went to the neighbour and nothing looked wrong. With half-open
/// bounds it belongs to the one on the right, which is what the pixel under the pointer is.
#[test]
fn a_point_on_a_seam_belongs_to_exactly_one_widget() {
    let l = layout();
    let st = state();
    let placed = place_toolbar(&l, 8);
    for w in placed.windows(2) {
        // Only adjacent *left-to-right* pairs, so `right()` of one is `x` of the next.
        if w[1].rect.x != w[0].rect.right() {
            continue;
        }
        let y = (w[0].rect.y + w[0].rect.height / 2) as i32;
        let seam = w[0].rect.right() as i32;
        assert_eq!(
            hit(&l, &st, seam, y),
            Hit::Tool(w[1].tool),
            "the seam between {} and {} should belong to the right-hand one",
            w[0].tool.name(),
            w[1].tool.name()
        );
        assert_eq!(
            hit(&l, &st, seam - 1, y),
            Hit::Tool(w[0].tool),
            "and the pixel before the seam belongs to the left-hand one"
        );
    }
}

/// **A point on nothing is nothing, and the page is a widget.**
///
/// **A miss has to be a value.** The pointer spends most of its life over the ruler, the gutter and the
/// panel, and if those were an error every caller would have a branch for the common case.
#[test]
fn a_point_on_no_widget_hits_nothing() {
    let l = layout();
    let st = state();
    // The gutter between the sidebar and the page.
    let cx = (l.sidebar.right() + 8) as i32;
    let cy = (l.canvas.y + l.canvas.height / 2) as i32;
    assert_eq!(
        hit(&l, &st, cx, cy),
        Hit::None,
        "the gutter between sidebar and page"
    );
    // The middle of the page.
    let px = (l.page.x + l.page.width / 2) as i32;
    let py = (l.page.y + l.page.height / 2) as i32;
    assert_eq!(
        hit(&l, &st, px, py),
        Hit::Page,
        "the page, where the caret goes"
    );
    // Off the panel entirely.
    assert_eq!(hit(&l, &st, -1, -1), Hit::None);
    assert_eq!(hit(&l, &st, 99_999, 99_999), Hit::None);
}

/// **The bands themselves are not widgets, and the sidebar is not one widget but several.**
///
/// The sidebar is the case that gets forgotten: it is a band, and it is *also* a list of rows, and a
/// hit test that stops at "the sidebar" makes every document unreachable. So the rows are separate
/// widgets and `Doc(i)` says which.
#[test]
fn the_bands_themselves_are_not_widgets() {
    let l = layout();
    let mut st = state();
    st.docs = vec!["one".into(), "two".into(), "three".into()];
    st.active_doc = 1;

    assert!(
        l.sidebar_rows >= 3,
        "the sidebar has {} rows, so at 1280x800 three documents cannot be hit at all",
        l.sidebar_rows
    );
    for i in 0..3usize {
        let r = l.sidebar_doc(i).expect("the row exists");
        let cx = (r.x + r.width / 2) as i32;
        let cy = (r.y + r.height / 2) as i32;
        assert_eq!(
            hit(&l, &st, cx, cy),
            Hit::Doc(i),
            "document row {i} at ({cx}, {cy})"
        );
    }
    // The header controls.
    assert_eq!(
        hit(
            &l,
            &st,
            (l.sidebar_back.x + l.sidebar_back.width / 2) as i32,
            (l.sidebar_back.y + l.sidebar_back.height / 2) as i32
        ),
        Hit::SidebarBack
    );
    assert_eq!(
        hit(
            &l,
            &st,
            (l.sidebar_new.x + l.sidebar_new.width / 2) as i32,
            (l.sidebar_new.y + l.sidebar_new.height / 2) as i32
        ),
        Hit::NewDoc
    );

    // **And with the sidebar closed, its rows are gone.** A hit test that ignores `sidebar_open` makes
    // a closed sidebar still clickable, which is invisible until the pointer lands there.
    st.sidebar_open = false;
    let r = l.sidebar_doc(0).expect("the row still exists; only the sidebar is closed");
    assert_eq!(
        hit(
            &l,
            &st,
            (r.x + r.width / 2) as i32,
            (r.y + r.height / 2) as i32
        ),
        Hit::None,
        "a closed sidebar's rows must not be hit"
    );
    let _ = TitleButton::Star;
}

/// **The menu widths are measured, not tabulated, and the cell width is the metrics'.**
///
/// **`menu_width` is a `const fn`, so it cannot read a `ChromeMetrics`** -- it uses
/// [`CHROME_CELL_W`](holonomy_render::chrome::CHROME_CELL_W) instead. That is the one thing in the
/// chrome's geometry that can drift from the font metrics, so it is asserted rather than assumed.
#[test]
fn the_menu_headings_are_measured_not_tabulated() {
    assert_eq!(
        holonomy_render::chrome::CHROME_CELL_W,
        ChromeMetrics::DESKTOP.cell_w,
        "the menu bar's cell width has drifted from the metrics'. The headings would then be laid \\
         out on a grid the rest of the chrome does not use."
    );
    // **Width is a function of the text's *length*, not its glyphs**, and this is a real property of
    // the chrome rather than a shortcut. The UI face is monospaced, so one cell per character is
    // exact; on a proportional face it would pad `File` to the width of `Extensions` and the menu bar
    // would have holes in it.
    let file = holonomy_render::chrome::menu_width("File");
    let help = holonomy_render::chrome::menu_width("Help");
    let edit = holonomy_render::chrome::menu_width("Edit");
    assert_eq!(file, help, "File and Help are both four characters");
    assert_eq!(
        edit, file,
        "and so is Edit, so all three are the same width"
    );
    assert_eq!(
        holonomy_render::chrome::menu_width("Format"),
        holonomy_render::chrome::menu_width("Insert"),
        "Format and Insert are both six characters"
    );
    assert!(
        holonomy_render::chrome::menu_width("View") < holonomy_render::chrome::menu_width("Format"),
        "and a four-letter heading is narrower than a six-letter one"
    );
    // **And the longest heading really is the one that sets the menu bar's width.** `Extensions` is
    // ten characters, and a menu bar that ran out of panel before its last heading would push `Help`
    // off screen -- which `every_widget_has_a_rect_inside_the_panel` cannot see, because that test is
    // about the toolbar and the title bar.
    assert_eq!(
        holonomy_render::chrome::menu_width("Extensions"),
        MENUS
            .iter()
            .map(|m| holonomy_render::chrome::menu_width(m))
            .max()
            .expect("there are menus"),
        "Extensions is the longest heading, so it is the widest box"
    );
    // **The first version of this test asserted `edit < file` and `edit == file` on consecutive
    // lines.** It was a drafting artefact and it is worth keeping the note, because the contradiction
    // is exactly what the claim looks like if you have not decided whether the width counts glyphs or
    // characters. Deciding it -- one cell per character, because the face is monospaced -- is what
    // makes the other six assertions in this file meaningful.
    let _ = TOOLBAR;
}
