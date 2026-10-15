//! **One widget list, painted and hit-tested.** 9 tests.
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
//! | **every popup row is inside its panel** | [`a_popup_row_is_inside_its_panel_and_its_panel_is_inside_the_window`] |
//! | **and the panel is as wide as its widest row** | [`a_popup_panel_is_exactly_as_wide_as_its_widest_row_needs`] |

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
    let r = l
        .sidebar_doc(0)
        .expect("the row still exists; only the sidebar is closed");
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
/// ## CORRECTION, part 32: the guard was comparing a constant with itself
///
/// This began as *"`CHROME_CELL_W` equals `ChromeMetrics::DESKTOP.cell_w`, so the menu bar's geometry
/// cannot drift from the font metrics."* **`CHROME_CELL_W` was 8 and `DESKTOP.cell_w` was 8, so it
/// passed -- while the value actually in use was neither**, because `Session::new` overrides `cell_w`
/// from the face's own monospace advance. It went on passing straight through the whole failure: at
/// 24 ppem the menu bar laid eight headings out on an 8 px grid while drawing them at 24, and
/// rendered `Fil eEdit ViewInsertFormatTool sExtensionsHelp`.
///
/// **The constant is gone.** `menu_width` takes the cell width as an argument, so there is nothing
/// left to drift, and the assertion is now on the *property* rather than on the equality of two
/// literals. The old gate compared the *constant* to `DESKTOP`, and `DESKTOP` is a design constant,
/// not the running value -- which is the whole reason it could pass while the product was wrong.
/// **A popup is a lighter plane than anything it can be drawn over.**
///
/// **The first version painted popups in `PILL`, which is also the toolbar's pill.** A menu hangs
/// over the toolbar and over the sidebar, so it was drawn in the colour of both — and the items were
/// legible while the *panel* was not. It read as floating text. This asserts the property rather than
/// the value, so a palette re-tune that broke it would fail here rather than in a screenshot nobody
/// was looking at.
#[test]
fn a_popup_is_a_lighter_plane_than_anything_it_covers() {
    use holonomy_render::chrome::colour;
    let popup = colour::POPUP & 0x00FF_FFFF;
    for (name, under) in [
        ("the toolbar pill", colour::PILL),
        ("a band", colour::BAND),
        ("the panel", colour::CHROME),
        ("the hovered row", colour::PILL_HOVER),
    ] {
        assert_ne!(
            popup,
            under & 0x00FF_FFFF,
            "a popup drawn in the colour of {name} is not a popup; it is the thing it covers"
        );
    }
    // **And brighter than all of them**, because it is a raised surface and everything else is
    // recessed or flat. A darker overlay would work on a light theme and this is a dark one.
    assert!(
        popup > (colour::PILL_HOVER & 0x00FF_FFFF),
        "the popup must be the lightest plane in the chrome, or it reads as a hole"
    );
}

#[test]
fn the_menu_headings_are_measured_not_tabulated() {
    use holonomy_render::chrome::menu_width;
    const CELL: u32 = ChromeMetrics::DESKTOP.cell_w;
    assert_eq!(
        menu_width("File", 8) * 2,
        menu_width("File", 16),
        "a heading's width scales with the cell, not with a constant: a 16 px cell is twice an \
         8 px one. Part 32 removed `CHROME_CELL_W` because this identity could not be stated \
         about it -- the constant is 8 whatever the face says, and the face stopped saying 8 in \
         part 31."
    );
    // **Width is a function of the text's *length*, not its glyphs**, and this is a real property of
    // the chrome rather than a shortcut. The UI face is monospaced, so one cell per character is
    // exact; on a proportional face it would pad `File` to the width of `Extensions` and the menu bar
    // would have holes in it.
    let file = menu_width("File", CELL);
    let help = menu_width("Help", CELL);
    let edit = menu_width("Edit", CELL);
    assert_eq!(file, help, "File and Help are both four characters");
    assert_eq!(
        edit, file,
        "and so is Edit, so all three are the same width"
    );
    assert_eq!(
        menu_width("Format", CELL),
        menu_width("Insert", CELL),
        "Format and Insert are both six characters"
    );
    assert!(
        menu_width("View", CELL) < menu_width("Format", CELL),
        "and a four-letter heading is narrower than a six-letter one"
    );
    // **And the longest heading really is the one that sets the menu bar's width.** `Extensions` is
    // ten characters, and a menu bar that ran out of panel before its last heading would push `Help`
    // off screen -- which `every_widget_has_a_rect_inside_the_panel` cannot see, because that test is
    // about the toolbar and the title bar.
    assert_eq!(
        menu_width("Extensions", CELL),
        MENUS
            .iter()
            .map(|m| menu_width(m, CELL))
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

/// **Every popup's rows are inside its own panel, and its panel is inside the window.**
///
/// **This is the invariant `Popup` exists to keep**, and `widgets.rs`'s header says why: hit testing
/// and layout are written once here so that a row that is *drawn* is a row that can be *clicked*. A
/// row that hangs off the bottom of the panel is drawn (the frame clips it) and cannot be clicked —
/// so the pair diverges silently, and the symptom is a menu item that does nothing.
///
/// # The two defects it holds, both real, both invisible to a bounds check
///
/// **The height clamp.** `y` was `min(height - h).max(0)`, so a popup taller than the remaining panel
/// had its *top* pushed to 0 and its rows — laid out from the clamped top — ran off the bottom. The
/// window is 800 px tall and the Style menu is 19 items, so this happened on the first open.
///
/// **The right clamp.** `Extensions` is the longest heading and its popup is 336 px; anchored to the
/// heading with no clamp, its last two items were drawn into the scrollbar and off the panel.
///
/// **Every menu and every dropdown, at both a 1,280x800 panel and a 900x700 one**, because a clamp
/// that holds at one size and not another is the failure mode this whole file is about.
#[test]
fn a_popup_row_is_inside_its_panel_and_its_panel_is_inside_the_window() {
    use holonomy_render::widgets::{popup, Open, Tool, TOOLBAR};

    for (w, h) in [(1280u32, 800u32), (1600, 1000), (900, 700), (1024, 600)] {
        let m = ChromeMetrics::for_size(w, h);
        let l = Layout::new(&m);
        let st = state();
        let opens: Vec<Open> = (0..MENUS.len())
            .map(Open::Menu)
            .chain(TOOLBAR.iter().map(|t| Open::Tool(*t)))
            .collect();
        for open in opens {
            let Some(p) = popup(&l, &st, Some(open)) else {
                // **A popup with no items is `None` on purpose** -- a caller could pass a bad index and
                // the answer is "draw nothing" rather than a panic inside a paint.
                assert_eq!(
                    open.items(&st).len(),
                    0,
                    "{open:?} at {w}x{h} has items but no popup"
                );
                continue;
            };
            assert!(
                p.rect.right() <= w && p.rect.bottom() <= h,
                "{w}x{h} {open:?}: the panel at {:?} runs off the window, which is a clamp that is \
                 not clamping",
                p.rect
            );
            assert!(
                p.rows.len() <= open.items(&st).len(),
                "{w}x{h} {open:?}: {} rows for {} items",
                p.rows.len(),
                open.items(&st).len()
            );
            for (i, r) in p.rows.iter().enumerate() {
                assert!(
                    r.x >= p.rect.x && r.right() <= p.rect.right()
                        && r.y >= p.rect.y && r.bottom() <= p.rect.bottom(),
                    "{w}x{h} {open:?}: row {i} at {r:?} is not inside its panel at {:?}. A row that \
                     hangs off the panel is drawn and cannot be clicked, which is the divergence this \
                     file exists to prevent.",
                    p.rect
                );
                // **And it is hit-testable at its own centre**, which is the same claim from the other
                // side: not just "inside the panel" but "the pointer finds it there".
                let cx = (r.x + r.width / 2) as i32;
                let cy = (r.y + r.height / 2) as i32;
                assert_eq!(
                    p.row_at(cx, cy),
                    Some(i),
                    "{w}x{h} {open:?}: row {i}'s own centre ({cx}, {cy}) does not hit row {i}"
                );
            }
        }
    }
    let _ = Tool::Zoom;
}

/// **A popup's panel is as wide as its widest row needs and no wider — measured, at every cell size.**
///
/// **CORRECTION, part 39, and this is the gate for it.** `widgets::popup` sized the panel over
/// `leading + label` and left the accelerator to be drawn *on top of the label*: at 20 ppem `Save` ended
/// at x = 144 and `Ctrl+S` started at x = 143. **Nothing overflowed and nothing was clipped**, so every
/// "is this text inside the popup" gate was green while the two texts were inside each other.
///
/// **The property is a band, not a width.** A panel *narrower* than its widest row clips the row's
/// accelerator — the defect. A panel *wider* is merely a wide menu. So the gate asserts the panel is at
/// least as wide as every row needs, and that it is **no wider than the widest row plus the padding on
/// both sides**, which is what stops `popup` growing a slack panel the first time a label gets long.
#[test]
fn a_popup_panel_is_exactly_as_wide_as_its_widest_row_needs() {
    use holonomy_render::widgets::{popup, row_cells, Open, TOOLBAR};

    for cell_w in [8u32, 10, 12, 16, 20] {
        let m = ChromeMetrics {
            cell_w,
            ..ChromeMetrics::DESKTOP
        };
        let l = Layout::new(&m);
        let st = state();
        for open in (0..MENUS.len())
            .map(Open::Menu)
            .chain(TOOLBAR.iter().map(|t| Open::Tool(*t)))
        {
            let items = open.items(&st);
            let Some(p) = popup(&l, &st, Some(open)) else {
                continue;
            };
            let pad = 2 * cell_w;
            let widest = items
                .iter()
                .map(|it| row_cells(it, cell_w))
                .max()
                .expect("a popup with rows has items")
                .max(14 * cell_w);
            assert_eq!(
                p.rect.width,
                widest + pad * 2,
                "cell {cell_w}, {open:?}: the panel is {} px for a widest row of {widest} px plus \
                 {pad} of padding on each side",
                p.rect.width
            );
        }
    }
}
