//! TEMPORARY: dump ribbon geometry.
use holonomy_render::chrome::{ChromeMetrics, ChromeState, Layout};
use holonomy_render::ribbon;

#[test]
fn probe() {
    for (w, h) in [(1600u32, 900u32), (1280, 800), (1024, 768)] {
        let m = ChromeMetrics {
            width: w,
            height: h,
            ..ChromeMetrics::DESKTOP
        };
        let m = m.with_ribbon_bands(true, false);
        let l = Layout::new(&m);
        for tab in 1u8..8 {
            let mut s = ChromeState::default();
            s.ribbon = true;
            s.ribbon = true;
            s.ribbon_tab = tab;
            let (groups, buttons) = ribbon::layout(&l, &s);
            println!("=== {w}x{h} tab {} cell_w {} cell_h {} toolbar {:?}", ribbon::TABS[tab as usize], ribbon::advance(&l), l.cell_h, l.toolbar);
            for g in &groups {
                println!("  group {:20} rect {:?}", g.label, g.rect);
            }
            for b in &buttons {
                println!(
                    "    btn {:22} w {:3} x {:4} y {:3} h {:3} large {} labelled {} tw {}",
                    b.label, b.rect.width, b.rect.x, b.rect.y, b.rect.height,
                    b.large, b.labelled, ribbon::text_w(&l, b.label)
                );
            }
        }
    }
}
