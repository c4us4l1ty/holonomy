//! TEMPORARY: render the real ribbon to PPM so the usability pass can be looked at.
use holonomy::{Session, TEST_CHART_PNG};
use holonomy_display::paint::{PaintStats, Painter};
use holonomy_display::{Frame, Scanout};
use holonomy_render::chrome::ChromeMetrics;

#[derive(Debug, Clone)]
struct Keep {
    last: std::rc::Rc<std::cell::RefCell<Option<Frame>>>,
    size: (u32, u32),
}

impl Scanout for Keep {
    fn present(&mut self, frame: &Frame) -> Result<u64, holonomy_display::FrameError> {
        *self.last.borrow_mut() = Some(frame.clone());
        Ok(frame.pixels().len() as u64)
    }
    fn present_damage(
        &mut self,
        frame: &Frame,
        _d: Option<holonomy_render::DamageRect>,
    ) -> Result<u64, holonomy_display::FrameError> {
        self.present(frame)
    }
    fn resize(&mut self, w: u32, h: u32) -> Result<bool, holonomy_display::FrameError> {
        self.size = (w, h);
        Ok(true)
    }
    fn width(&self) -> u32 {
        self.size.0
    }
    fn height(&self) -> u32 {
        self.size.1
    }
    fn describe(&self) -> &'static str {
        "capture"
    }
}

fn shot(name: &str, w: u32, h: u32, ppem: u16, tab: u8) {
    let atlas: &'static holonomy_assets::atlas::Atlas =
        Box::leak(Box::new(holonomy_assets::build_atlas(&[ppem]).expect("atlas").0));
    let keep = Keep {
        last: Default::default(),
        size: (w, h),
    };
    let m = ChromeMetrics {
        width: w,
        height: h,
        ..ChromeMetrics::DESKTOP
    };
    let editor = holonomy_text::Editor::from_text(b"Hello\n").expect("editor");
    let mut s = Session::new(editor, Painter::new(atlas, 0), Box::new(keep.clone()), m);
    s.state.ribbon = true;
    s.state.ribbon_tab = tab;
    s.paint(None).expect("paint");
    let f = keep.last.borrow().clone().expect("frame");
    let mut out = std::fs::File::create(format!("/tmp/shot/{name}.ppm")).expect("file");
    f.to_ppm(&mut out).expect("ppm");
    println!("wrote {name} {}x{}", f.width(), f.height());
}

#[test]
fn capture() {
    let _ = std::fs::create_dir_all("/tmp/shot");
    for ppem in [16u16, 20] {
        for (w, h) in [(1600u32, 900u32), (1280, 800), (1024, 768)] {
            shot(&format!("ribbon-{ppem}-{w}"), w, h, ppem, 1);
        }
    }
    let _ = TEST_CHART_PNG;
    let _ = PaintStats::default();
}
