//! Wordcraft-style ribbon: every painted command has a hit target and an action.
use holonomy_render::chrome::{ChromeMetrics, ChromeState, Layout};
use holonomy_render::{menus, ribbon, widgets};

#[test]
fn ribbon_buttons_and_hits_share_geometry() {
    let m = ChromeMetrics {
        title_h: 76,
        menu_h: 32,
        toolbar_h: 112,
        ruler_h: 26,
        status_h: 28,
        ..ChromeMetrics::DESKTOP
    };
    let l = Layout::new(&m);
    let mut s = ChromeState {
        ribbon: true,
        ..ChromeState::default()
    };
    for tab in 1..ribbon::TABS.len() {
        s.ribbon_tab = tab as u8;
        let (_, buttons) = ribbon::layout(&l, &s);
        for b in buttons.iter().filter(|b| !b.rect.is_empty()) {
            let x = b.rect.x + b.rect.width / 2;
            let y = b.rect.y + b.rect.height / 2;
            assert_eq!(
                widgets::hit(&l, &s, x as i32, y as i32),
                widgets::Hit::Ribbon(b.action),
                "tab {tab} {:?}",
                b.label
            );
        }
    }
}

#[test]
fn narrow_ribbon_overflows_instead_of_drawing_dead_buttons() {
    let m = ChromeMetrics {
        width: 768,
        title_h: 76,
        menu_h: 32,
        toolbar_h: 112,
        ..ChromeMetrics::DESKTOP
    };
    let l = Layout::new(&m);
    let s = ChromeState {
        ribbon: true,
        ..ChromeState::default()
    };
    let (_, buttons) = ribbon::layout(&l, &s);
    let hidden = buttons.iter().filter(|b| b.rect.is_empty()).count();
    assert_eq!(ribbon::overflow(&l, &s).len(), hidden);
    for b in buttons.iter().filter(|b| !b.rect.is_empty()) {
        assert!(b.rect.right() <= l.width);
    }
}

// ============================================================================================
// Part 46: the ribbon's geometry has to hold at *every* type size, not just the design one.
//
// The report was a screenshot of the running desktop at 125 % -- which is what
// `initial_desktop_zoom` picks on any screen 1,800 px or wider, so it is the shipping default --
// showing `Int` / `Ital` run together, `Supe` / `Subs` clipped, `HomeInsert` with no gap, and the
// group captions printed over the ruler's number row. Every one of those is one arithmetic defect:
// **the ribbon measured text in a hardcoded 8 px cell while the painter advances 12.**
//
// These gates are written at the type scale the product actually boots into, because that is the
// scale at which the old code was wrong and the one at which no gate in the tree was looking.
// ============================================================================================

/// The metrics a session at `ppem` builds: the atlas's line pitch and the Monospace advance.
///
/// **Measured, not guessed.** 16 ppem is pitch 25 / advance 8 (the design); 20 ppem -- the 125 %
/// the desktop boots into -- is pitch 30 / advance 12.
fn at(ppem: u16) -> ChromeMetrics {
    let (pitch, cell_w) = match ppem {
        16 => (25, 8),
        20 => (30, 12),
        other => panic!("no measured pitch for {other} ppem"),
    };
    ChromeMetrics::DESKTOP
        .with_cell_width(cell_w)
        .with_type_scale(pitch)
        .with_line_pitch(pitch)
        .with_ribbon_bands(true, false)
}

fn ribbon_state(tab: u8) -> ChromeState {
    ChromeState {
        ribbon: true,
        ribbon_tab: tab,
        ..ChromeState::default()
    }
}

/// **No ribbon button is narrower than the label it draws.** The defect, stated as an inequality.
///
/// At 125 % `Italic` is six cells of 12 px in a box built for six of 8, so the label ended 24 px past
/// its button and the *next* button's ink began inside this one's. The button's label area is what is
/// left after its icon gutter and its drop-down chevron, and the assertion is against that and not
/// against the whole button: an icon and a chevron are real, and a box that fits its label only by
/// spending the icon's pixels is a box that does not fit.
#[test]
fn every_ribbon_label_fits_inside_its_button() {
    for ppem in [16, 20] {
        let m = at(ppem);
        let l = Layout::new(&m);
        for tab in 1..ribbon::TABS.len() {
            let s = ribbon_state(tab as u8);
            let (_, buttons) = ribbon::layout(&l, &s);
            for b in buttons.iter().filter(|b| !b.rect.is_empty() && b.labelled) {
                let label = if b.action == menus::Action::SelectTool(widgets::Tool::Font) {
                    s.font_name()
                } else {
                    b.label
                };
                let ink = ribbon::text_w(&l, label);
                // A small button spends its icon gutter before the label; a large one centres it.
                let room = if b.large {
                    b.rect.width
                } else {
                    b.rect
                        .width
                        .saturating_sub(if b.icon.is_some() {
                            ribbon::ICON_GUTTER
                        } else {
                            0
                        })
                        .saturating_sub(if matches!(b.action, menus::Action::SelectTool(_)) {
                            ribbon::CHEVRON_W
                        } else {
                            0
                        })
                };
                assert!(
                    ink <= room,
                    "at {ppem} ppem, tab {tab}: `{label}` needs {ink} px of ink in {room} px of \
                     button (`{}` is {} px wide). This is the `Italic` / `Ital` overflow: the label \
                     is drawn past its box and reads as part of the button next door.",
                    b.label,
                    b.rect.width
                );
            }
        }
    }
}

/// **No two ribbon buttons overlap**, at either type size, on any tab.
///
/// The earlier Font-group defect was exactly this and it was reported as a *click* -- the middle of
/// the overflow square answered `Ribbon(SelectTool(Font))` -- because an overlap in a hit test is a
/// click that runs the tool next door. A geometric assertion catches it without a pointer.
#[test]
fn no_two_ribbon_buttons_overlap() {
    for ppem in [16, 20] {
        let m = at(ppem);
        let l = Layout::new(&m);
        for tab in 1..ribbon::TABS.len() {
            let s = ribbon_state(tab as u8);
            let (_, buttons) = ribbon::layout(&l, &s);
            let live: Vec<_> = buttons.iter().filter(|b| !b.rect.is_empty()).collect();
            for (i, a) in live.iter().enumerate() {
                for b in live.iter().skip(i + 1) {
                    let hit = a.rect.x < b.rect.right()
                        && b.rect.x < a.rect.right()
                        && a.rect.y < b.rect.bottom()
                        && b.rect.y < a.rect.bottom();
                    assert!(
                        !hit,
                        "at {ppem} ppem, tab {tab}: `{}` {:?} overlaps `{}` {:?}",
                        a.label, a.rect, b.label, b.rect
                    );
                }
            }
        }
    }
}

/// **The group caption is inside the group, and above the ruler.**
///
/// The caption strip used to be a literal 20 px at `rect.bottom() - 20` while the buttons had already
/// been given the same 20 -- fine against a 25 px line, and at 30 px the glyphs' bottom four rows were
/// drawn *below the group*, in the ruler. That is the `Paragraph` printed over `232 234 236`.
#[test]
fn every_group_caption_is_inside_the_ribbon() {
    for ppem in [16, 20] {
        let m = at(ppem);
        let l = Layout::new(&m);
        for tab in 1..ribbon::TABS.len() {
            let s = ribbon_state(tab as u8);
            let (groups, buttons) = ribbon::layout(&l, &s);
            for g in &groups {
                let caption_h = l.cell_h + ribbon::CAPTION_AIR;
                let top = g.rect.bottom().saturating_sub(caption_h);
                assert!(
                    g.rect.bottom() <= l.ruler.y,
                    "at {ppem} ppem, tab {tab}: the `{}` caption ends at {} and the ruler starts at \
                     {} -- the caption is drawn in the next band",
                    g.label,
                    g.rect.bottom(),
                    l.ruler.y
                );
                assert!(
                    caption_h <= g.rect.height,
                    "at {ppem} ppem, tab {tab}: `{}`'s caption strip is {caption_h} px in a {} px \
                     group",
                    g.label,
                    g.rect.height
                );
                // And no button reaches into it.
                for b in buttons.iter().filter(|b| !b.rect.is_empty()) {
                    let inside = b.rect.x >= g.rect.x && b.rect.right() <= g.rect.right();
                    if inside {
                        assert!(
                            b.rect.bottom() <= top + 1,
                            "at {ppem} ppem, tab {tab}: `{}` ends at {} and `{}`'s caption begins \
                             at {top} -- the label is drawn over the buttons",
                            b.label,
                            b.rect.bottom(),
                            g.label
                        );
                    }
                }
            }
        }
    }
}

/// **The tab strip has a gap between every pair of tabs, and every label fits its tab.**
///
/// `Home` and `Insert` were laid 54 and 70 px apart while their ink was 120 px wide, so the strip
/// read `HomeInsert`. There was no padding bug to find: there was a measurement bug, and the padding
/// that a real cell needs is what this asserts.
#[test]
fn ribbon_tabs_do_not_touch_and_never_clip_their_labels() {
    for ppem in [16, 20] {
        let m = at(ppem);
        let l = Layout::new(&m);
        let rects = ribbon::tab_rects(&l);
        assert_eq!(rects.len(), ribbon::TABS.len());
        for (i, (r, name)) in rects.iter().zip(ribbon::TABS).enumerate() {
            let ink = ribbon::text_w(&l, name);
            assert!(
                ink <= r.width,
                "at {ppem} ppem: the tab `{name}` needs {ink} px of ink in {} px",
                r.width
            );
            if let Some(prev) = i.checked_sub(1).and_then(|j| rects.get(j)) {
                assert!(
                    r.x >= prev.right() + ribbon::TAB_GAP,
                    "at {ppem} ppem: `{}` starts at {} and `{name}` ends at {} -- the two read as \
                     one word",
                    ribbon::TABS[i - 1],
                    prev.right(),
                    r.x
                );
            }
        }
    }
}

/// **The quick-access row fits its own labels**, `AutoSave` included.
///
/// Its box was a fixed 112 px with the label drawn into the first 76, and the switch at `x + 78` --
/// so at 12 px a cell `AutoSave` is 96 px of ink and the switch was drawn on top of its last two
/// letters. The screenshot shows `AutoSa` with the toggle over the `v`.
#[test]
fn the_quick_access_row_fits_its_labels() {
    for ppem in [16, 20] {
        let m = at(ppem);
        let l = Layout::new(&m);
        let row = ribbon::qat(&l);
        let autosave = row
            .iter()
            .find(|(a, _)| *a == menus::Action::ToggleAutosave)
            .expect("AutoSave is the first quick-access item");
        let ink = ribbon::text_w(&l, "AutoSave");
        let (label_box, switch) = ribbon::autosave_switch(&l, autosave.1);
        assert!(
            ink <= label_box.width,
            "at {ppem} ppem: `AutoSave` needs {ink} px and its label box is {}",
            label_box.width
        );
        assert!(
            switch.x >= label_box.right(),
            "at {ppem} ppem: the switch starts at {} and the label ends at {}",
            switch.x,
            label_box.right()
        );
        // And every item is inside the title band, left of the search field.
        for (a, r) in &row {
            assert!(
                r.right() <= l.width,
                "at {ppem} ppem: {a:?} ends at {} past a {} px panel",
                r.right(),
                l.width
            );
        }
    }
}

/// **The Home tab fits a 1600 px window** -- the width `windowed::DESIGN_W` opens at.
///
/// It did not, and that is the defect underneath the blue tile in the Editing group: with honest
/// widths Home's five groups need 2,088 px at 16 ppem and 2,664 at 125 % against a 1,552 px limit,
/// so the Styles tiles and the Editing buttons were drawn past the overflow square or on top of each
/// other. The ribbon now gives the small buttons' labels up from the right, which is Word's
/// *Collapse the Group* and Wordcraft's `COMPACT`, and the tab fits.
#[test]
fn the_home_tab_fits_the_window_the_product_opens() {
    for ppem in [16, 20] {
        let m = ChromeMetrics {
            width: 1600,
            height: 1000,
            ..at(ppem)
        };
        let l = Layout::new(&m);
        let s = ribbon_state(1);
        let (groups, buttons) = ribbon::layout(&l, &s);
        let end = groups
            .iter()
            .map(|g| g.rect.right())
            .max()
            .expect("Home has groups");
        assert!(
            end <= l.width - 48,
            "at {ppem} ppem Home ends at {end} and the overflow square's column begins at {}",
            l.width - 48
        );
        // **And nothing was hidden to get there.** The compact level drops labels; it must not drop
        // commands. A tab that fits by hiding half its buttons is the defect wearing a different hat.
        let hidden = buttons.iter().filter(|b| b.rect.is_empty()).count();
        assert_eq!(
            hidden, 0,
            "at {ppem} ppem Home hides {hidden} buttons in the overflow menu to fit"
        );
        // A label is dropped only where an icon stands in for it.
        for b in buttons.iter().filter(|b| !b.rect.is_empty() && !b.labelled) {
            assert!(
                b.icon.is_some(),
                "at {ppem} ppem `{}` dropped its label and has no icon: an empty square",
                b.label
            );
        }
    }
}
