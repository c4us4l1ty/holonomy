//! The managed desktop must render the *real* WordCraft-inspired editor, not
//! the compact/sealed chrome. These gates use the session's own paint and hit
//! paths at the reported screenshot size and at laptop/narrow-window widths.
#![cfg(feature = "desktop")]

use holonomy::Session;
use holonomy_display::{paint::Painter, HeadlessScanout};
use holonomy_input::{Button, Event};
use holonomy_render::{chrome::ChromeMetrics, menus::Action, ribbon, widgets};
use holonomy_text::Editor;

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| Box::leak(Box::new(holonomy_assets::build_atlas(&[16, 20]).unwrap().0)))
}

fn desktop(width: u32, height: u32, zoom: u32) -> Session<'static> {
    let m = ChromeMetrics {
        width,
        height,
        ..ChromeMetrics::DESKTOP
    };
    let mut s = Session::new(
        Editor::new(),
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(width, height)),
        m,
    );
    s.set_managed_documents(true);
    s.create_document(1, "Untitled".into());
    s.set_zoom(zoom).unwrap();
    s
}

fn disjoint(a: holonomy_render::DamageRect, b: holonomy_render::DamageRect) -> bool {
    a.is_empty() || b.is_empty() || a.right() <= b.x || b.right() <= a.x
}

#[test]
fn managed_desktop_opens_on_home_with_a_real_paper_workspace() {
    for (w, h, zoom) in [
        (1920, 1167, 125),
        (1600, 900, 125),
        (1024, 768, 100),
        (768, 720, 100),
    ] {
        let mut s = desktop(w, h, zoom);
        assert!(s.state.ribbon && s.state.paged);
        assert!(!s.state.sealed && !s.state.dark);
        assert_eq!(s.state.ribbon_tab, 1); // WordCraft's Home tab
        let l = s.chrome_layout();
        let (groups, buttons) = ribbon::layout(&l, &s.state);
        assert!(groups.iter().any(|g| g.label == "Font"));
        let overflow = ribbon::overflow(&l, &s.state);
        assert!(
            buttons
                .iter()
                .any(|b| b.action == Action::ToggleBold && !b.rect.is_empty())
                || overflow
                    .iter()
                    .any(|item| item.action == Some(Action::ToggleBold))
                || groups.iter().enumerate().any(|(i, g)| {
                    g.label == "Font"
                        && !ribbon::group_trigger(&l, g).is_empty()
                        && ribbon::group_items(&s.state, i)
                            .iter()
                            .any(|item| item.action == Some(Action::ToggleBold))
                }),
            "{w}x{h}: Bold must remain visible or reachable"
        );
        let sheet = l
            .visible_page(&s.state, 0)
            .expect("the first sheet is visible");
        assert!(
            sheet.width >= w / 2,
            "{w}x{h}: paper is only {} px wide",
            sheet.width
        );
        assert!(sheet.x > 0 && sheet.right() < w);
        assert!(l.canvas.y >= l.toolbar.bottom());

        s.repaint_all().unwrap();
        let pixels = s.frame().pixels();
        let at = |x: u32, y: u32| pixels[y as usize * w as usize + x as usize] & 0x00FF_FFFF;
        let palette = ribbon::Palette::get(false);
        assert_eq!(at(1, 1), palette.title & 0x00FF_FFFF, "header palette");
        assert_eq!(
            at(1, l.toolbar.y + l.toolbar.height / 2),
            palette.ribbon & 0x00FF_FFFF
        );
        assert_eq!(at(1, l.ruler.y + 2), palette.ruler_margin & 0x00FF_FFFF);
        assert_eq!(
            at(l.text.x + 30, l.ruler.y + 2),
            palette.ruler & 0x00FF_FFFF
        );
        // With Navigation open the left gutter belongs to the sidebar, so probe
        // the desk between the rail and the sheet rather than inside the rail.
        let desk_x = if l.sidebar.width > 0 && sheet.x > l.sidebar.right() {
            l.sidebar.right() + (sheet.x - l.sidebar.right()) / 2
        } else {
            sheet.x / 2
        };
        assert_eq!(
            at(desk_x, sheet.y + 10),
            palette.canvas & 0x00FF_FFFF,
            "desk, not chrome white"
        );
        assert_eq!(
            at(sheet.x + 4, sheet.y + 4),
            0x00FF_FFFF,
            "the actual paper is white"
        );
    }
}

#[test]
fn header_and_status_keep_the_document_name_and_errors_unobscured() {
    for (w, h, zoom) in [
        (1920, 1167, 125),
        (1280, 800, 125),
        (900, 700, 100),
        (768, 720, 125),
    ] {
        let mut s = desktop(w, h, zoom);
        let l = s.chrome_layout();
        let name = ribbon::title_rect(&l);
        let search = ribbon::search_rect(&l);
        let lock = ribbon::lock_rect(&l);
        assert!(
            name.width >= ribbon::text_w(&l, "Untitled"),
            "{w}x{h}: no room for the name"
        );
        assert!(search.width > 0 && lock.width > 0);
        assert!(disjoint(name, search) && disjoint(search, lock));
        for (action, r) in ribbon::qat(&l) {
            assert!(
                disjoint(r, name) && disjoint(r, search) && disjoint(r, lock),
                "{action:?} overlaps the title"
            );
            let x = r.x + r.width / 2;
            let y = r.y + r.height / 2;
            assert_eq!(
                widgets::hit(&l, &s.state, x as i32, y as i32),
                widgets::Hit::Ribbon(action)
            );
        }
        let sr = ribbon::status_regions(&l, &s.state, "Page 1 of 1   0 words", "Editing   125%");
        assert!(disjoint(sr.0, sr.1) && disjoint(sr.1, sr.2));
        s.state.notice = Some("Save failed: local vault is not writable".into());
        let (summary, notice, mode) =
            ribbon::status_regions(&l, &s.state, "Page 1 of 1   0 words", "Editing   125%");
        assert!(disjoint(summary, notice) && disjoint(notice, mode));
        assert!(
            notice.width >= l.cell_w * 8,
            "{w}x{h}: error notice was crowded out"
        );
        assert!(mode.right() <= w && summary.right() <= w && notice.right() <= w);
    }
}

#[test]
fn narrow_ribbon_collapses_groups_and_every_command_stays_reachable() {
    for (w, h, zoom) in [
        (900, 700, 100),
        (768, 720, 125),
        (768, 520, 125),
        (480, 520, 125),
    ] {
        let mut s = desktop(w, h, zoom);
        let l = s.chrome_layout();
        let (groups, buttons) = ribbon::layout(&l, &s.state);
        let overflow = ribbon::overflow(&l, &s.state);
        assert!(
            groups
                .iter()
                .any(|g| g.level == holonomy_render::wordcraft::ribbon_fit::COLLAPSED),
            "{w}x{h}: expected a collapsed group"
        );
        for b in buttons.iter().filter(|b| b.rect.is_empty()) {
            let group = &groups[b.group];
            let via_group = group.level == holonomy_render::wordcraft::ribbon_fit::COLLAPSED
                && !ribbon::group_trigger(&l, group).is_empty()
                && ribbon::group_items(&s.state, b.group)
                    .iter()
                    .any(|it| it.action == Some(b.action));
            let via_overflow = overflow.iter().any(|it| it.action == Some(b.action));
            assert!(
                via_group || via_overflow,
                "{w}x{h}: `{}` became unreachable",
                b.label
            );
        }
        // Open the first collapsed group and exercise its real rows.
        let (index, group) = groups
            .iter()
            .enumerate()
            .find(|(_, g)| {
                g.level == holonomy_render::wordcraft::ribbon_fit::COLLAPSED
                    && !ribbon::group_trigger(&l, g).is_empty()
            })
            .expect("a visible collapsed group");
        let r = ribbon::group_trigger(&l, group);
        let (x, y) = ((r.x + r.width / 2) as i32, (r.y + r.height / 2) as i32);
        assert_eq!(
            widgets::hit(&l, &s.state, x, y),
            widgets::Hit::RibbonGroup(index)
        );
        s.handle_pointer(
            &mut holonomy::store::NoSource,
            Event::Button {
                button: Button::Left,
                pressed: true,
                x,
                y,
                time_ms: 1,
            },
        )
        .unwrap();
        assert_eq!(s.state.open, Some(widgets::Open::RibbonGroup(index)));
        let pop = widgets::popup(&l, &s.state, s.state.open).unwrap();
        assert_eq!(pop.rows.len(), ribbon::group_items(&s.state, index).len());
        for (i, row) in pop.rows.iter().enumerate() {
            assert!(
                row.right() <= w && row.bottom() <= h,
                "{w}x{h}: group row {i} outside window"
            );
            assert_eq!(
                widgets::hit(
                    &l,
                    &s.state,
                    (row.x + row.width / 2) as i32,
                    (row.y + row.height / 2) as i32
                ),
                widgets::Hit::MenuItem {
                    open: widgets::Open::RibbonGroup(index),
                    row: i
                }
            );
        }
    }
}

#[test]
fn hidden_tabs_have_a_more_menu_and_select_their_real_ribbon() {
    for (w, h, zoom) in [(768, 720, 125), (600, 600, 100), (480, 520, 125)] {
        let mut s = desktop(w, h, zoom);
        let l = s.chrome_layout();
        let tabs = ribbon::tab_rects(&l);
        let hidden = tabs
            .iter()
            .rposition(|r| r.is_empty())
            .expect("some tabs must be hidden");
        assert!(hidden > 0 && hidden < ribbon::TABS.len());
        let more = ribbon::more_rect(&l);
        assert!(more.width >= ribbon::text_w(&l, "More"));
        assert!(more.right() < ribbon::collapse_rect(&l).x);
        for (i, r) in tabs.iter().enumerate().filter(|(_, r)| !r.is_empty()) {
            assert!(r.right() < more.x, "tab {i} overlaps More at {w}x{h}");
        }
        let (x, y) = (
            (more.x + more.width / 2) as i32,
            (more.y + more.height / 2) as i32,
        );
        assert_eq!(widgets::hit(&l, &s.state, x, y), widgets::Hit::RibbonMore);
        s.handle_pointer(
            &mut holonomy::store::NoSource,
            Event::Button {
                button: Button::Left,
                pressed: true,
                x,
                y,
                time_ms: 1,
            },
        )
        .unwrap();
        assert_eq!(s.state.open, Some(widgets::Open::RibbonTabs));
        let pop = widgets::popup(&l, &s.state, s.state.open).unwrap();
        assert_eq!(pop.rows.len(), ribbon::TABS.len());
        let row = pop.rows[hidden];
        assert!(row.right() <= w && row.bottom() <= h);
        let (rx, ry) = (
            (row.x + row.width / 2) as i32,
            (row.y + row.height / 2) as i32,
        );
        assert_eq!(
            widgets::hit(&l, &s.state, rx, ry),
            widgets::Hit::MenuItem {
                open: widgets::Open::RibbonTabs,
                row: hidden,
            }
        );
        s.handle_pointer(
            &mut holonomy::store::NoSource,
            Event::Button {
                button: Button::Left,
                pressed: true,
                x: rx,
                y: ry,
                time_ms: 2,
            },
        )
        .unwrap();
        assert_eq!(s.state.open, None);
        assert_eq!(usize::from(s.state.ribbon_tab), hidden);
        assert!(ribbon::tab_rects(&s.chrome_layout())[hidden].is_empty());
    }
}
