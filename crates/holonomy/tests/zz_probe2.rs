//! TEMPORARY: dump the painted ribbon labels.
use holonomy_render::chrome::{ChromeMetrics, ChromeState, Layout};
use holonomy_render::ribbon;
use holonomy_render::{Node, SurfaceTree};

fn walk(t: &SurfaceTree, out: &mut Vec<String>) {
    if let Some(n) = &t.node {
        match n {
            Node::Label(l) => out.push(format!("LABEL x={} y={} {:?}", l.x, l.y, String::from_utf8_lossy(&l.bytes[..l.len as usize]))),
            Node::Icon(i) => out.push(format!("ICON {:?}", i)),
            _ => {}
        }
    }
    for c in t.before.iter().chain(t.after.iter()) {
        walk(c, out);
    }
}

#[test]
fn probe2() {
    let m = ChromeMetrics {
        width: 1600,
        height: 900,
        ..ChromeMetrics::DESKTOP
    };
    let m = m.with_ribbon_bands(true, false);
    let l = Layout::new(&m);
    let mut s = ChromeState::default();
    s.ribbon = true;
    s.ribbon_tab = 4;
    let mut v = Vec::new();
    ribbon::paint(&mut v, &l, &s, l.cell_h);
    let mut out = Vec::new();
    for t in &v {
        walk(t, &mut out);
    }
    for o in &out {
        println!("{o}");
    }
}
