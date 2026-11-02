use holonomy::{sample, Session};
use holonomy_display::{paint::Painter, HeadlessScanout};
use holonomy_render::{chrome::ChromeMetrics, menus::Action};
use holonomy_text::{Editor, STYLE_BOLD, STYLE_HEADER, STYLE_ITALIC};

#[test]
fn wordcraft_sample_is_editable_and_preserves_styles_in_payload() {
    let e = sample::studio_handbook().unwrap();
    let bytes = e.text().unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    let heading = text.find("Why share a studio?").unwrap() as u32;
    let painters = text.find("Painters").unwrap() as u32;
    let letterpress = text.find("letterpress").unwrap() as u32;
    assert_ne!(e.style_at(heading).style_flags & STYLE_HEADER, 0);
    assert_ne!(e.style_at(painters).style_flags & STYLE_BOLD, 0);
    assert_ne!(e.style_at(letterpress).style_flags & STYLE_ITALIC, 0);
    assert_eq!(e.caret(), 0);
    assert_eq!(e.undo_depth(), 0);
    let reopened = Editor::from_payload(&e.payload().unwrap()).unwrap();
    assert_eq!(reopened.text().unwrap(), bytes);
    assert_eq!(
        reopened.style_at(heading).style_flags,
        e.style_at(heading).style_flags
    );
}

#[test]
fn creating_a_sample_parks_the_previous_document() {
    let (atlas, _) = holonomy_assets::build_atlas(&[16]).unwrap();
    let m = ChromeMetrics::DESKTOP;
    let mut s = Session::new(
        Editor::from_text(b"previous work").unwrap(),
        Painter::new(&atlas, 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );
    s.apply_action(Action::NewSample).unwrap();
    assert_eq!(s.state.title, sample::TITLE);
    assert!(s.state.dirty);
    assert!(s.text().unwrap().starts_with(sample::TITLE.as_bytes()));
    s.switch_document_public(1);
    assert_eq!(s.text().unwrap(), b"previous work");
}
