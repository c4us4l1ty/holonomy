//! Regression gates for the MIT-licensed WordCraft UI port.
#![cfg(feature = "desktop")]

use holonomy::Session;
use holonomy_display::{paint::Painter, HeadlessScanout};
use holonomy_input::{Button, Event};
use holonomy_render::{chrome::ChromeMetrics, menus::Action, ribbon, widgets, wordcraft};
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

fn click(s: &mut Session<'_>, x: u32, y: u32, time_ms: u64) {
    s.handle_pointer(
        &mut holonomy::store::NoSource,
        Event::Button {
            button: Button::Left,
            pressed: true,
            x: x as i32,
            y: y as i32,
            time_ms,
        },
    )
    .unwrap();
}

#[test]
fn native_palette_is_the_upstream_token_set() {
    for dark in [false, true] {
        let t = wordcraft::tokens::Tokens::get(dark);
        let p = ribbon::Palette::get(dark);
        assert_eq!(p.title, t.title_bar);
        assert_eq!(p.ribbon, t.ribbon);
        assert_eq!(p.canvas, t.canvas);
        assert_eq!(p.panel, t.panel);
        assert_eq!(p.tab_strip, t.tab_strip);
        assert_eq!(p.status_bar, t.status_bar);
        assert_eq!(p.menu, t.menu);
        assert_eq!(p.icon, t.icon);
        assert_eq!(p.accent, t.accent);
        assert_eq!(p.on_accent, t.on_accent);
    }
}

#[test]
fn every_tab_keeps_hidden_commands_reachable_at_every_width() {
    for (w, h, zoom) in [
        (1920, 1080, 125),
        (1600, 900, 125),
        (1280, 800, 100),
        (900, 700, 100),
        (768, 520, 125),
        (600, 600, 100),
        (480, 520, 125),
    ] {
        let mut s = desktop(w, h, zoom);
        for tab in 1..ribbon::TABS.len() as u8 {
            s.state.ribbon_tab = tab;
            s.refresh_overflow();
            let l = s.chrome_layout();
            let (groups, buttons) = ribbon::layout(&l, &s.state);
            let overflow = ribbon::overflow(&l, &s.state);
            for b in buttons.iter().filter(|b| !b.rect.is_empty()) {
                assert!(
                    b.rect.right() <= l.width && b.rect.bottom() <= l.toolbar.bottom(),
                    "{w}x{h} tab {tab}: `{}` outside ribbon",
                    b.label
                );
            }
            for b in buttons.iter().filter(|b| b.rect.is_empty()) {
                let g = &groups[b.group];
                let via_group = g.level == wordcraft::ribbon_fit::COLLAPSED
                    && !ribbon::group_trigger(&l, g).is_empty()
                    && ribbon::group_items(&s.state, b.group)
                        .iter()
                        .any(|it| it.action == Some(b.action));
                let via_overflow = overflow.iter().any(|it| it.action == Some(b.action));
                assert!(
                    via_group || via_overflow,
                    "{w}x{h} tab {tab}: `{}` unreachable",
                    b.label
                );
            }
        }
    }
}

#[test]
fn collapsed_group_popup_runs_a_command_and_stays_open() {
    let mut s = desktop(768, 720, 125);
    let l = s.chrome_layout();
    let (groups, _) = ribbon::layout(&l, &s.state);
    let (index, group) = groups
        .iter()
        .enumerate()
        .find(|(_, g)| g.label == "Font" && g.level == wordcraft::ribbon_fit::COLLAPSED)
        .expect("Font collapses at 768 px / 125%");
    let r = ribbon::group_trigger(&l, group);
    click(&mut s, r.x + r.width / 2, r.y + r.height / 2, 1);
    assert_eq!(s.state.open, Some(widgets::Open::RibbonGroup(index)));
    let pop = widgets::popup(&l, &s.state, s.state.open).unwrap();
    let row = pop
        .rows
        .iter()
        .enumerate()
        .find(|(i, _)| ribbon::group_items(&s.state, index)[*i].action == Some(Action::ToggleBold))
        .map(|(_, r)| *r)
        .expect("Bold row");
    click(&mut s, row.x + row.width / 2, row.y + row.height / 2, 2);
    assert_eq!(
        s.state.open,
        Some(widgets::Open::RibbonGroup(index)),
        "group palette stays open"
    );
    assert!(s.state.styles.bold, "Bold command ran");
    click(&mut s, r.x + r.width / 2, r.y + r.height / 2, 3);
    assert_eq!(s.state.open, None, "trigger toggles the palette closed");
}

#[test]
fn styles_gallery_applies_a_real_persisted_style() {
    let mut s = desktop(1600, 900, 100);
    s.insert_text("A heading").unwrap();
    s.select_range(0, s.text_len()).unwrap();
    let l = s.chrome_layout();
    let (_, buttons) = ribbon::layout(&l, &s.state);
    let tile = buttons
        .iter()
        .find(|b| b.action == Action::SetStyle(1) && !b.rect.is_empty())
        .expect("Heading 1 tile visible in the compact gallery");
    assert!(tile.style_preview);
    click(
        &mut s,
        tile.rect.x + tile.rect.width / 2,
        tile.rect.y + tile.rect.height / 2,
        1,
    );
    assert_eq!(s.state.style_index, 1);
    let flags = s.editor().style_at(0).style_flags;
    assert_ne!(flags & holonomy_text::STYLE_HEADER, 0);
    assert_ne!(flags & holonomy_text::STYLE_BOLD, 0);
    let payload = s.editor().payload().unwrap();
    let restored = Editor::from_payload(&payload).unwrap();
    assert_eq!(restored.style_at(0).style_flags, flags);
}

#[test]
fn navigation_pane_switches_between_headings_and_pages_and_closes() {
    let mut s = desktop(1600, 900, 100);
    s.create_sample_document(2).unwrap();
    assert!(s.state.sidebar_open, "wide desktop opens Navigation");
    assert!(!s.state.navigation.is_empty());
    let l = s.chrome_layout();
    let tabs = ribbon::nav_tabs(&l);
    click(
        &mut s,
        tabs[1].x + tabs[1].width / 2,
        tabs[1].y + tabs[1].height / 2,
        1,
    );
    assert_eq!(s.state.nav_tab, 1);
    assert_eq!(
        ribbon::nav_entry_count(&l, &s.state),
        l.page_count(s.state.total_lines) as usize
    );
    let rows = ribbon::nav_rows(&l, &s.state);
    if rows.len() > 1 {
        let (_, r) = rows[1];
        click(&mut s, r.x + r.width / 2, r.y + r.height / 2, 2);
        assert!(s.state.scroll_line >= l.page_rows());
    }
    let close = ribbon::nav_close(&l);
    click(
        &mut s,
        close.x + close.width / 2,
        close.y + close.height / 2,
        3,
    );
    assert!(!s.state.sidebar_open);
    s.apply_action(Action::ToggleNavigation).unwrap();
    let l = s.chrome_layout();
    let search = ribbon::nav_search(&l);
    click(
        &mut s,
        search.x + search.width / 2,
        search.y + search.height / 2,
        4,
    );
    assert!(matches!(
        s.take_document_request(),
        Some(holonomy::session::DocumentRequest::Find)
    ));
}

#[test]
fn mit_license_and_upstream_revision_are_preserved() {
    let license = include_str!("../../../third_party/wordcraft/LICENSE-MIT");
    assert!(license.contains("Copyright (c) 2026 ArtCraft Team and the WordCraft contributors"));
    assert!(license.contains("Permission is hereby granted, free of charge"));
    let notice = include_str!("../../../third_party/wordcraft/NOTICE.md");
    assert!(notice.contains("ae5c65b9dd11faf359ce039a2a487e57e3d824a2"));
    assert!(notice.contains("MIT"));
}
