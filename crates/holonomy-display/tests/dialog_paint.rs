//! The dialog actually paints: the mask has ink, and the larger label size is larger.
//!
//! The regression this holds was a secret field that accepted keystrokes and drew nothing. The mask
//! was U+2022, outside the atlas coverage, and `Painter::label` advances over a missing glyph without
//! blitting. These tests paint through the real atlas and count pixels, not tree nodes.

use holonomy_display::paint::Painter;
use holonomy_display::Frame;
use holonomy_render::dialog::{self, DialogView, Field};
use holonomy_render::{ChromeMetrics, Label, Layout, Node, Style, SurfaceTree};

fn atlas() -> holonomy_assets::atlas::Atlas {
    holonomy_assets::build_atlas(&[16, 20]).expect("build the two-size atlas").0
}

fn unlock() -> DialogView {
    DialogView {
        title: "Unlock".into(),
        message: "Enter one passphrase.".into(),
        fields: vec![Field { label: "Passphrase".into(), value: "*******".into(), secret: true }],
        focus: 0,
        error: None,
        confirm: "Unlock".into(),
        cancel: Some("Quit".into()),
        choices: vec![],
        choice: 0,
    }
}

#[test]
fn the_secret_mask_paints_ink_and_missing_no_glyphs() {
    let a = atlas();
    let l = Layout::new(&ChromeMetrics::for_size(1600, 1000));
    let d = unlock();
    let g = dialog::geometry(&l, &d);
    let f = g.field_rect(0);
    let mut tree = SurfaceTree::default();
    dialog::push_tree(&mut tree, &l, &d);
    let mut frame = Frame::black(l.width, l.height);
    let stats = Painter::new(&a, 0).paint(&mut frame, &tree, None).expect("paint");
    assert_eq!(stats.missing, 0, "a dialog label asked for a glyph the atlas does not carry: {stats:?}");

    // Count light ink inside the input box. The field background is near-black, the mask is near-white.
    let mut ink = 0u64;
    for y in f.y..f.bottom() {
        for x in f.x..f.right() {
            let px = frame.pixels()[(y * l.width + x) as usize];
            let r = (px >> 16) & 0xFF;
            let gg = (px >> 8) & 0xFF;
            let b = px & 0xFF;
            if r > 180 && gg > 180 && b > 180 {
                ink += 1;
            }
        }
    }
    assert!(ink > 20, "the passphrase mask drew no visible dots: {ink} light pixels in {f:?}");
}

#[test]
fn an_explicit_label_size_draws_larger_than_the_chrome_size() {
    let a = atlas();
    let paint_ink = |size: u8| -> u64 {
        let mut tree = SurfaceTree::default();
        tree.push(Node::Label(Label::new(8, 8, "Passphrase", Style::REGULAR, size, 0xFFFF_FFFF)));
        let mut frame = Frame::black(320, 80);
        Painter::new(&a, 0).paint(&mut frame, &tree, None).expect("paint");
        frame.pixels().iter().filter(|&&px| px & 0x00FF_FFFF != 0).count() as u64
    };
    let small = paint_ink(0);
    let large = paint_ink(1);
    assert!(small > 0);
    assert!(large > small, "size index 1 drew {large} ink pixels, size 0 drew {small}; Label::size was ignored");
}
