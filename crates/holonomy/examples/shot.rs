//! Render the current chrome to a PPM. A development aid, not a product path.
use holonomy::session::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy};

fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("usage: shot <out.ppm> [extra]");
    let extra = std::env::args().nth(2).unwrap_or_default();
    let atlas: &'static holonomy_assets::atlas::Atlas = Box::leak(Box::new(
        holonomy_assets::build_atlas(&[16]).expect("atlas").0,
    ));
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    let mut i = 1;
    while ed.text_len() < 900 {
        let line = format!("The quick brown fox jumps over the lazy dog, line {i}.\n");
        ed.insert_at(
            ed.text_len() as u32,
            line.as_bytes(),
            SpanPolicy::GrowIntoInsert,
        )
        .expect("room");
        i += 1;
    }
    if !extra.is_empty() {
        ed.insert_at(0, extra.as_bytes(), SpanPolicy::GrowIntoInsert)
            .ok();
    }
    let mut s = Session::new(
        ed,
        Painter::new(atlas, 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );
    // Populate the state the new chrome draws: a document list, and an open menu when asked for.
    s.state.docs = vec![
        "Journal".into(),
        "Notes on the container".into(),
        "Field report".into(),
        "Meeting minutes".into(),
    ];
    s.state.active_doc = 3;
    s.state.sidebar_open = true;
    // `--popup=MENU` opens a menu bar popup and `--dropdown=TOOL` opens a toolbar dropdown, so a
    // screenshot can show the two overlay states the reference captures.
    for arg in std::env::args().skip(1) {
        if let Some(v) = arg.strip_prefix("--popup=") {
            if let Ok(i) = v.parse::<usize>() {
                s.state.open = Some(holonomy_render::widgets::Open::Menu(i));
            }
        }
        if let Some(v) = arg.strip_prefix("--dropdown=") {
            for p in holonomy_render::widgets::place_toolbar(&s.chrome_layout(), 8) {
                if p.tool.name() == v {
                    s.state.open = Some(holonomy_render::widgets::Open::Tool(p.tool));
                    break;
                }
            }
        }
    }
    // `--hover=NAME` and `--press=NAME` put the pointer on a toolbar tool, so a screenshot can show
    // the two state surfaces. **The name is resolved through `place_toolbar`** -- the same list the
    // hit test reads -- so the screenshot cannot show a widget the pointer could not reach.
    for arg in std::env::args().skip(1) {
        let Some((flag, want)) = arg.split_once('=') else {
            continue;
        };
        if !matches!(flag, "--hover" | "--press") {
            continue;
        }
        let layout = s.chrome_layout();
        let Some(tool) = holonomy_render::widgets::place_toolbar(&layout, 8)
            .into_iter()
            .find(|p| p.tool.name() == want)
            .map(|p| p.tool)
        else {
            eprintln!("no tool named {want:?}; try one of:");
            for p in holonomy_render::widgets::place_toolbar(&layout, 8) {
                eprintln!("  {}", p.tool.name());
            }
            continue;
        };
        let hit = holonomy_render::widgets::Hit::Tool(tool);
        s.state.hover = Some(hit);
        if flag == "--press" {
            s.state.pressed = Some(hit);
        }
    }
    s.repaint_all().expect("paint");
    let f = s.frame();
    let (w, h) = (f.width(), f.height());
    let mut ppm = Vec::with_capacity((w * h * 3) as usize + 32);
    ppm.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
    for px in f.pixels() {
        let [r, g, b] = [(px >> 16) & 0xFF, (px >> 8) & 0xFF, px & 0xFF];
        ppm.extend_from_slice(&[r as u8, g as u8, b as u8]);
    }
    std::fs::write(&out, &ppm).expect("write");
    println!("{out}: {w}x{h}");
}
