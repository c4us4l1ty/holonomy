use holonomy_render::dialog::{self, DialogView, Field, Hit};
use holonomy_render::{ChromeMetrics, Layout};

fn view() -> DialogView {
    DialogView { title: "Unlock".into(), message: "Enter one passphrase.".into(), fields: vec![Field { label: "Passphrase".into(), value: "•••".into(), secret: true }], focus: 0, error: None, confirm: "Unlock".into(), cancel: Some("Quit".into()), choices: vec![], choice: 0 }
}
#[test]
fn dialog_buttons_and_fields_are_hit_testable_at_minimum_window_size() {
    let l = Layout::new(&ChromeMetrics::for_size(ChromeMetrics::MIN_WIDTH, ChromeMetrics::MIN_HEIGHT));
    let d = view();
    let g = dialog::geometry(&l, &d);
    assert!(g.panel.x + g.panel.width <= l.width);
    assert!(g.panel.y + g.panel.height <= l.height);
    assert_eq!(dialog::hit(&l, &d, (g.confirm.x + 4) as i32, (g.confirm.y + 4) as i32), Hit::Confirm);
    assert_eq!(dialog::hit(&l, &d, (g.cancel.x + 4) as i32, (g.cancel.y + 4) as i32), Hit::Cancel);
    assert_eq!(dialog::hit(&l, &d, (g.panel.x + 24) as i32, (g.panel.y + 100) as i32), Hit::Field(0));
}
#[test]
fn secret_fields_never_put_plaintext_in_the_render_tree() {
    let l = Layout::new(&ChromeMetrics::DESKTOP);
    let mut d = view();
    d.fields[0].value = "hunter2".into();
    let mut tree = holonomy_render::SurfaceTree::default();
    dialog::push_tree(&mut tree, &l, &d);
    let mut labels = Vec::new();
    fn walk(t: &holonomy_render::SurfaceTree, out: &mut Vec<String>) { if let Some(holonomy_render::Node::Label(l)) = t.node { out.push(l.as_str().to_string()); } for c in &t.before { walk(c, out); } for c in &t.after { walk(c, out); } }
    walk(&tree, &mut labels);
    assert!(!labels.iter().any(|s| s.contains("hunter2")));
    assert!(labels.iter().any(|s| s.contains("•••••••")));
}
