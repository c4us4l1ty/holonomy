//! TEMPORARY: render the window to PPM for review.
use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::{Frame, Scanout};
use holonomy_render::chrome::ChromeMetrics;

struct Keep {
    last: std::rc::Rc<std::cell::RefCell<Option<Frame>>>,
    size: (u32, u32),
}
impl Scanout for Keep {
    fn present(&mut self, f: &Frame) -> Result<u64, holonomy_display::FrameError> {
        *self.last.borrow_mut() = Some(f.clone());
        Ok(f.pixels().len() as u64)
    }
    fn present_damage(&mut self, f: &Frame, _d: Option<holonomy_render::DamageRect>) -> Result<u64, holonomy_display::FrameError> { self.present(f) }
    fn resize(&mut self, w: u32, h: u32) -> Result<bool, holonomy_display::FrameError> { self.size = (w, h); Ok(true) }
    fn width(&self) -> u32 { self.size.0 }
    fn height(&self) -> u32 { self.size.1 }
    fn describe(&self) -> &'static str { "capture" }
}

#[test]
fn capture() {
    std::fs::create_dir_all("/tmp/shot").unwrap();
    for (ppem, w, h) in [(16u16, 1600u32, 900u32), (20, 1280, 800), (16, 1024, 768)] {
        let atlas: &'static holonomy_assets::atlas::Atlas =
            Box::leak(Box::new(holonomy_assets::build_atlas(&[ppem]).unwrap().0));
        let keep = Keep { last: Default::default(), size: (w, h) };
        let m = ChromeMetrics { width: w, height: h, ..ChromeMetrics::DESKTOP }
            .with_ribbon_bands(true, false);
        let editor = holonomy_text::Editor::from_text(
            b"Normal paragraph here.\n\nHeading One\n\nBody text after the heading.\n").unwrap();
        let mut s = Session::new(editor, Painter::new(atlas, 0), Box::new(keep.clone()), m);
        s.state.ribbon = true;
        s.state.ribbon_tab = 1;
        s.paint(None).unwrap();
        let f = keep.last.borrow().clone().unwrap();
        let mut out = std::fs::File::create(format!("/tmp/shot/doc-{ppem}-{w}.ppm")).unwrap();
        f.to_ppm(&mut out).unwrap();
        println!("wrote doc-{ppem}-{w}");
    }
}
