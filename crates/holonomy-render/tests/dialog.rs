use holonomy_render::dialog::{self, DialogView, Field, Hit};
use holonomy_render::{ChromeMetrics, Layout};

fn view() -> DialogView {
    DialogView {
        title: "Unlock".into(),
        message: "Enter one passphrase.".into(),
        fields: vec![Field {
            label: "Passphrase".into(),
            value: "***".into(),
            secret: true,
        }],
        focus: 0,
        error: None,
        confirm: "Unlock".into(),
        cancel: Some("Quit".into()),
        choices: vec![],
        choice: 0,
    }
}

fn labels(tree: &holonomy_render::SurfaceTree) -> Vec<String> {
    fn walk(t: &holonomy_render::SurfaceTree, out: &mut Vec<String>) {
        if let Some(holonomy_render::Node::Label(l)) = t.node {
            out.push(l.as_str().to_string());
        }
        for c in &t.before {
            walk(c, out);
        }
        for c in &t.after {
            walk(c, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

#[test]
fn dialog_buttons_and_fields_are_hit_testable_at_minimum_window_size() {
    let l = Layout::new(&ChromeMetrics::for_size(
        ChromeMetrics::MIN_WIDTH,
        ChromeMetrics::MIN_HEIGHT,
    ));
    let d = view();
    let g = dialog::geometry(&l, &d);
    assert!(g.panel.x + g.panel.width <= l.width);
    assert!(g.panel.y + g.panel.height <= l.height);
    assert_eq!(
        dialog::hit(&l, &d, (g.confirm.x + 4) as i32, (g.confirm.y + 4) as i32),
        Hit::Confirm
    );
    assert_eq!(
        dialog::hit(&l, &d, (g.cancel.x + 4) as i32, (g.cancel.y + 4) as i32),
        Hit::Cancel
    );
    let f = g.field_rect(0);
    assert_eq!(
        dialog::hit(&l, &d, (f.x + 8) as i32, (f.y + 8) as i32),
        Hit::Field(0)
    );
    // Every target stays inside the panel and keeps a usable height even in the compact layout.
    assert!(f.height >= 24);
    assert!(g.confirm.height >= 24 && g.cancel.height >= 24);
    assert!(g.confirm.right() <= g.panel.right() && g.cancel.x >= g.panel.x);
}

#[test]
fn a_large_window_gets_a_roomier_panel_and_larger_type() {
    let small = Layout::new(&ChromeMetrics::for_size(768, 320));
    let large = Layout::new(&ChromeMetrics::for_size(1600, 1000));
    let d = view();
    let gs = dialog::geometry(&small, &d);
    let gl = dialog::geometry(&large, &d);
    assert!(gl.panel.width > gs.panel.width);
    assert!(gl.panel.height > gs.panel.height);
    assert!(gl.field_h > gs.field_h);
    assert!(gl.pad > gs.pad);
    assert_eq!(gl.text_size, 1, "a normal desktop dialog asks for the larger atlas size");
    assert_eq!(gs.text_size, 0, "a short window stays at the chrome size");
    // The panel is still centred and never leaves the window.
    assert_eq!(gl.panel.x, (large.width - gl.panel.width) / 2);
    assert_eq!(gl.panel.y, (large.height - gl.panel.height) / 2);
    assert!(gl.panel.right() <= large.width && gl.panel.bottom() <= large.height);
}

#[test]
fn secret_fields_never_put_plaintext_in_the_render_tree() {
    let l = Layout::new(&ChromeMetrics::DESKTOP);
    let mut d = view();
    d.fields[0].value = "hunter2".into();
    let mut tree = holonomy_render::SurfaceTree::default();
    dialog::push_tree(&mut tree, &l, &d);
    let labels = labels(&tree);
    assert!(!labels.iter().any(|s| s.contains("hunter2")));
    assert!(labels.iter().any(|s| s.contains("*******")));
    // The mask is ASCII, so it is inside the atlas coverage window (FR-2.3). U+2022 was not, and a
    // missing glyph advanced the pen without drawing — the "typing shows nothing" regression.
    assert!(dialog::MASK.is_ascii());
}

#[test]
fn a_long_secret_keeps_the_caret_inside_the_field() {
    let l = Layout::new(&ChromeMetrics::for_size(1600, 1000));
    let mut d = view();
    d.fields[0].value = "*".repeat(512);
    let g = dialog::geometry(&l, &d);
    let f = g.field_rect(0);
    let mut tree = holonomy_render::SurfaceTree::default();
    dialog::push_tree(&mut tree, &l, &d);
    // The caret is the last 2px-wide accent rect; it must not be drawn past the input box.
    fn carets(t: &holonomy_render::SurfaceTree, out: &mut Vec<holonomy_render::Rect>) {
        if let Some(holonomy_render::Node::Rect(r)) = t.node {
            if r.width == 2 && r.colour == 0xFF7A_A2F7 {
                out.push(r);
            }
        }
        for c in &t.before {
            carets(c, out);
        }
        for c in &t.after {
            carets(c, out);
        }
    }
    let mut cs = Vec::new();
    carets(&tree, &mut cs);
    assert_eq!(cs.len(), 1);
    let c = cs[0];
    assert!(c.x >= f.x as i32 && (c.x as u32) < f.right());
    assert!(c.y >= f.y as i32 && (c.y as u32) < f.bottom());
}

#[test]
fn choices_scroll_to_keep_the_selection_visible() {
    let l = Layout::new(&ChromeMetrics::for_size(800, 600));
    let mut d = view();
    d.fields.clear();
    d.choices = (0..20).map(|i| format!("Document {i}")).collect();
    d.choice = 19;
    let g = dialog::geometry(&l, &d);
    assert!(g.visible_choices > 0);
    assert!(g.choice_start <= d.choice);
    assert!(d.choice < g.choice_start + g.visible_choices);
    let r = g.choice_rect(d.choice - g.choice_start);
    assert_eq!(dialog::hit(&l, &d, (r.x + 8) as i32, (r.y + 8) as i32), Hit::Choice(19));
}
