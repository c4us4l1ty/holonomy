//! Desktop editing workflows added with the Wordcraft ribbon.
use holonomy::Session;
use holonomy_display::{paint::Painter, HeadlessScanout};
use holonomy_render::{chrome::ChromeMetrics, menus::Action};
use holonomy_text::Editor;

fn session(text: &str) -> Session<'static> {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> = std::sync::OnceLock::new();
    let atlas = ATLAS.get_or_init(|| Box::leak(Box::new(holonomy_assets::build_atlas(&[16]).unwrap().0)));
    let m = ChromeMetrics::DESKTOP;
    let mut s = Session::new(Editor::from_text(text.as_bytes()).unwrap(), Painter::new(atlas, 0), Box::new(HeadlessScanout::new(m.width, m.height)), m);
    s.set_managed_documents(true);
    s
}

#[test]
fn lists_toggle_and_continue() {
    let mut s = session("one\ntwo");
    s.select_range(0, 7).unwrap();
    s.apply_action(Action::ToggleBullets).unwrap();
    assert_eq!(s.text().unwrap(), b"- one\n- two");
    s.caret_to(s.text().unwrap().len()).unwrap();
    s.insert_text("\n").unwrap();
    assert!(s.text().unwrap().ends_with(b"\n- "));
    s.apply_action(Action::ToggleNumbering).unwrap();
    assert!(s.text().unwrap().starts_with(b"1. one"));
}

#[test]
fn find_and_replace_all_are_bounded_and_undoable() {
    let mut s = session("alpha beta alpha");
    let found = s.find_text("alpha", true, true).unwrap();
    assert_eq!(found, Some((0, 5)));
    assert_eq!(s.replace_all("alpha", "gamma", true).unwrap(), 2);
    assert_eq!(s.text().unwrap(), b"gamma beta gamma");
    s.undo().unwrap();
    assert_eq!(s.text().unwrap(), b"gamma beta alpha");
}

#[test]
fn alignment_roundtrips_and_does_not_erase_inline_style() {
    let mut s = session("centered");
    s.select_range(0, 8).unwrap();
    s.apply_action(Action::ToggleBold).unwrap();
    s.apply_action(Action::AlignCenter).unwrap();
    let flags = s.editor().style_at(0).style_flags;
    assert_ne!(flags & holonomy_text::STYLE_BOLD, 0);
    assert_ne!(flags & holonomy_text::STYLE_ALIGN_CENTER, 0);
    let payload = s.editor().payload().unwrap();
    let restored = Editor::from_payload(&payload).unwrap();
    assert_eq!(restored.style_at(0).style_flags, flags);
}
