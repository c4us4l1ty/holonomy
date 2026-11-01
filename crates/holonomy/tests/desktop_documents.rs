#![cfg(feature = "desktop")]
use holonomy::session::{DocumentRequest, DocumentSnapshot};
use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::chrome::ChromeMetrics;
use holonomy_render::menus::Action;
use holonomy_text::Editor;

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> = std::sync::OnceLock::new();
    ATLAS.get_or_init(|| { let (a, _) = holonomy_assets::build_atlas(&[16]).unwrap(); Box::leak(Box::new(a)) })
}
fn session() -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut s = Session::new(Editor::new(), Painter::new(atlas(), 0), Box::new(HeadlessScanout::new(m.width, m.height)), m);
    s.set_managed_documents(true);
    s
}
#[test]
fn managed_lifecycle_waits_for_the_driver_and_preserves_dirty_state_per_document() {
    let mut s = session();
    s.replace_documents(vec![DocumentSnapshot { id: 1, title: "One".into(), payload: Editor::from_text(b"first").unwrap().payload().unwrap(), caret: 0, dirty: false, font_index: 0, style_index: 0, mode: 0 }]).unwrap();
    s.insert_text("!").unwrap();
    assert!(s.has_unsaved_documents());
    s.create_document(2, "Two".into());
    s.insert_text("second").unwrap();
    s.switch_document_public(1);
    assert_eq!(s.state.title, "One");
    assert!(s.state.dirty, "parking a dirty document must not clean it");
    assert_eq!(s.text().unwrap(), b"first!", "active id {} title {:?}", s.active_document_id(), s.state.title);
    s.apply_action(Action::Delete).unwrap();
    assert_eq!(s.take_document_request(), Some(DocumentRequest::Delete));
    assert_eq!(s.document_count(), 2, "delete is not performed before confirmation and durable save");
    s.apply_action(Action::Lock).unwrap();
    assert_eq!(s.take_document_request(), Some(DocumentRequest::Lock));
}
#[test]
fn select_all_selects_instead_of_moving_to_the_end() {
    let mut s = session();
    s.replace_documents(vec![DocumentSnapshot { id: 1, title: "One".into(), payload: Editor::from_text(b"alpha\nbravo").unwrap().payload().unwrap(), caret: 0, dirty: false, font_index: 0, style_index: 0, mode: 0 }]).unwrap();
    s.apply_action(Action::SelectAll).unwrap();
    let sel = s.editor().selection().expect("selection");
    assert_eq!((sel.start, sel.end), (0, 11));
}
