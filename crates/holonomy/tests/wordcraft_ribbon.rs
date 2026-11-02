//! Wordcraft-style ribbon: every painted command has a hit target and an action.
use holonomy_render::chrome::{ChromeMetrics, ChromeState, Layout};
use holonomy_render::{ribbon, widgets};

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
    let l = Layout::new(m);
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
    let l = Layout::new(m);
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
