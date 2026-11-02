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
    ATLAS.get_or_init(|| { let (a, _) = holonomy_assets::build_atlas(&[16, 20]).unwrap(); Box::leak(Box::new(a)) })
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
    s.replace_documents(vec![DocumentSnapshot { id: 1, title: "One".into(), payload: Editor::from_text(b"first").unwrap().payload().unwrap(), caret: 5, dirty: false, font_index: 0, style_index: 0, mode: 0 }]).unwrap();
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

#[test]
fn managed_clipboard_publishes_and_system_paste_is_a_request() {
    let mut s = session();
    s.replace_documents(vec![DocumentSnapshot { id: 1, title: "One".into(), payload: Editor::from_text(b"copy me").unwrap().payload().unwrap(), caret: 7, dirty: false, font_index: 0, style_index: 0, mode: 0 }]).unwrap();
    s.apply_action(Action::SelectAll).unwrap();
    let before = s.clipboard_generation();
    s.apply_action(Action::Copy).unwrap();
    assert!(s.clipboard_generation() > before);
    assert_eq!(s.clipboard_bytes(), b"copy me");
    s.apply_action(Action::Paste).unwrap();
    assert!(s.take_paste_request(), "managed paste asks the window driver for the X selection");
    s.clear_clipboard();
    assert_eq!(s.clipboard_len(), 0);
}

#[test]
fn sample_is_a_managed_request_and_gets_a_persistable_identity() {
    let mut s = session();
    s.replace_documents(vec![DocumentSnapshot { id: 1, title: "One".into(), payload: Editor::from_text(b"original").unwrap().payload().unwrap(), caret: 0, dirty: false, font_index: 0, style_index: 0, mode: 0 }]).unwrap();
    s.apply_action(Action::NewSample).unwrap();
    assert_eq!(s.take_document_request(), Some(DocumentRequest::NewSample));
    assert_eq!(s.text().unwrap(), b"original");
    s.create_sample_document(2).unwrap();
    let docs = s.document_snapshots().unwrap();
    assert_eq!(docs.len(), 2);
    let sample = docs.iter().find(|d| d.id == 2).unwrap();
    assert_eq!(sample.title, holonomy::sample::TITLE);
    let restored = Editor::from_payload(&sample.payload).unwrap();
    assert_ne!(restored.style_at(0).style_flags & holonomy_text::STYLE_HEADER, 0);
}

#[test]
fn document_library_does_not_replace_the_editor_and_is_a_managed_request() {
    let mut s = session();
    s.replace_documents(vec![DocumentSnapshot { id: 1, title: "Notes".into(), payload: Editor::from_text(b"draft").unwrap().payload().unwrap(), caret: 5, dirty: false, font_index: 0, style_index: 0, mode: 0 }]).unwrap();
    s.insert_text("!").unwrap();
    s.apply_action(Action::Home).unwrap();
    assert_eq!(s.take_document_request(), Some(DocumentRequest::Home));
    s.set_home(Some(holonomy_render::home::View::new(vec!["Notes".into()])));
    s.repaint_all().unwrap();
    assert_eq!(s.active_document_id(), 1);
    assert_eq!(s.text().unwrap(), b"draft!");
    assert!(s.has_unsaved_documents());
    s.set_home(None);
    s.repaint_all().unwrap();
    assert_eq!(s.text().unwrap(), b"draft!");
}

#[test]
fn long_paragraph_wraps_scrolls_edits_and_round_trips_without_hard_breaks() {
    use holonomy_input::Command;
    let mut s = session();
    let text = "many words form an ordinary paragraph that continues beyond the visible sheet ".repeat(55);
    let payload = Editor::from_text(text.as_bytes()).unwrap().payload().unwrap();
    s.replace_documents(vec![DocumentSnapshot { id: 1, title: "Long".into(), payload,
        caret: 0, dirty: false, font_index: 0, style_index: 0, mode: 0 }]).unwrap();
    assert!(s.total_lines() > s.chrome_layout().rows);
    s.apply(Command::PageDown).unwrap();
    assert!(s.caret() > 0);
    assert!(s.scroll_line() > 0);
    let at = s.caret() as usize;
    s.insert_text("Z").unwrap();
    let mut expected = text.clone().into_bytes();
    expected.insert(at, b'Z');
    assert_eq!(s.text().unwrap(), expected);
    assert!(!s.text().unwrap().contains(&b'\n'));
    s.undo().unwrap();
    assert_eq!(s.text().unwrap(), text.as_bytes());
    let snapshots = s.document_snapshots().unwrap();
    let mut reopened = session();
    reopened.replace_documents(snapshots).unwrap();
    assert_eq!(reopened.text().unwrap(), text.as_bytes());
    assert_eq!(reopened.total_lines(), s.total_lines());
}

#[test]
fn wrapping_reflows_on_resize_sidebar_and_zoom_without_moving_text_or_caret() {
    let mut s = session();
    let text = "a long paragraph with several words ".repeat(90);
    let payload = Editor::from_text(text.as_bytes()).unwrap().payload().unwrap();
    s.replace_documents(vec![DocumentSnapshot { id: 1, title: "Reflow".into(), payload,
        caret: 300, dirty: false, font_index: 0, style_index: 0, mode: 0 }]).unwrap();
    let before = s.total_lines();
    assert!(s.resize(700, 700).unwrap());
    assert!(s.total_lines() >= before);
    let caret = s.caret();
    s.set_sidebar(true);
    s.set_zoom(125).unwrap();
    assert_eq!(s.caret(), caret);
    assert_eq!(s.text().unwrap(), text.as_bytes());
    s.repaint_all().unwrap();
}

#[test]
fn click_and_selection_follow_wrapped_rows_after_scrolling() {
    use holonomy_input::{Button, Command, Event};
    use holonomy::store::NoSource;
    let mut s = session();
    let text = "line wrapping needs a consistent byte index for the pointer and selection ".repeat(60);
    let payload = Editor::from_text(text.as_bytes()).unwrap().payload().unwrap();
    s.replace_documents(vec![DocumentSnapshot { id: 1, title: "Click".into(), payload,
        caret: 0, dirty: false, font_index: 0, style_index: 0, mode: 0 }]).unwrap();
    s.scroll_to(10).unwrap();
    let l = s.chrome_layout();
    let x = l.text.x as i32;
    let y = (l.row_y(&s.state, 11).unwrap() + s.chrome.metrics.cell_h / 2) as i32;
    s.handle_pointer(&mut NoSource, Event::Button { button: Button::Left, pressed: true, x, y, time_ms: 100 }).unwrap();
    s.handle_pointer(&mut NoSource, Event::Button { button: Button::Left, pressed: false, x, y, time_ms: 101 }).unwrap();
    assert_eq!(s.caret_line(), 11);
    let at = s.caret();
    s.apply(Command::SelectDown).unwrap();
    let selected = s.editor().selection().unwrap();
    assert_eq!(selected.start, at);
    assert!(selected.end > selected.start);
    s.repaint_all().unwrap();
}

#[test]
fn entering_past_a_sheet_creates_a_second_visible_page_without_losing_the_first() {
    use holonomy_input::Command;
    let mut s = session();
    s.create_document(1, "Pages".into());
    s.insert_text("first page").unwrap();
    let rows = s.chrome_layout().page_rows();
    for _ in 0..rows { s.apply(Command::Newline).unwrap(); }
    s.insert_text("second page").unwrap();
    assert_eq!(s.chrome_layout().page_count(s.total_lines()), 2);
    assert!(s.scroll_line() > 0);
    assert!(s.text().unwrap().starts_with(b"first page\n"));
    assert!(s.text().unwrap().ends_with(b"second page"));
    s.repaint_all().unwrap();
    s.scroll_to(0).unwrap();
    let canvas = s.chrome_layout().canvas;
    let wheel_x = canvas.right().saturating_sub(2) as i32;
    let wheel_y = (canvas.y + canvas.height / 2) as i32;
    let x_wheel = |button| holonomy_input::x11key::pointer_event(&holonomy_x11::Event::ButtonPress {
        button, event_x: wheel_x as i16, event_y: wheel_y as i16, time: 1,
    }).unwrap();
    s.handle_pointer(&mut holonomy::store::NoSource, x_wheel(5)).unwrap();
    assert!(s.scroll_line() > 0, "wheel down works over the desk, not just a sheet");
    s.handle_pointer(&mut holonomy::store::NoSource, x_wheel(4)).unwrap();
    assert_eq!(s.scroll_line(), 0);
    let rail = s.chrome_layout().scrollbar;
    let x = (rail.x + rail.width / 2) as i32;
    let bottom = rail.bottom().saturating_sub(1) as i32;
    s.handle_pointer(&mut holonomy::store::NoSource, holonomy_input::Event::Button {
        button: holonomy_input::Button::Left, pressed: true, x, y: bottom, time_ms: 200,
    }).unwrap();
    assert!(s.scroll_line() > 0, "clicking the scrollbar moves to later pages");
    s.handle_pointer(&mut holonomy::store::NoSource, holonomy_input::Event::Motion {
        x, y: rail.y as i32,
    }).unwrap();
    s.handle_pointer(&mut holonomy::store::NoSource, holonomy_input::Event::Button {
        button: holonomy_input::Button::Left, pressed: false, x, y: rail.y as i32, time_ms: 201,
    }).unwrap();
    assert_eq!(s.scroll_line(), 0, "dragging the thumb back returns to the first page");
    assert!(s.chrome_layout().visible_page(&s.state, 0).is_some());
    s.repaint_all().unwrap();
    s.scroll_to(rows).unwrap();
    assert!(s.chrome_layout().visible_page(&s.state, 1).is_some());
    s.repaint_all().unwrap();
}

#[test]
fn desktop_sheet_margins_shrink_when_the_window_is_narrowed() {
    let mut s = session();
    let large = s.chrome.metrics.page_pad;
    assert!(large > holonomy_render::ChromeMetrics::DESKTOP.page_pad);
    s.resize(600, 700).unwrap();
    assert!(s.chrome.metrics.page_pad < large);
    assert_eq!(s.chrome.metrics.page_pad, 50);
    assert!(s.chrome_layout().text.width > 0);
    s.resize(1280, 800).unwrap();
    assert_eq!(s.chrome.metrics.page_pad, large);
}

#[test]
fn managed_insert_actions_request_input_instead_of_a_test_chart() {
    let mut s = session();
    s.insert_text("Before").unwrap();
    s.apply_action(Action::InsertImage).unwrap();
    assert_eq!(s.take_document_request(), Some(DocumentRequest::InsertImage));
    assert_eq!(s.text().unwrap(), b"Before");
    s.apply_action(Action::InsertTable).unwrap();
    assert_eq!(s.take_document_request(), Some(DocumentRequest::InsertTable));
    assert_eq!(s.text().unwrap(), b"Before");
}

#[test]
fn managed_equation_and_image_are_separate_blocks_and_persist() {
    let mut s = session();
    s.insert_text("Before").unwrap();
    s.insert_image_bytes(holonomy::TEST_CHART_PNG).unwrap();
    s.insert_text("After").unwrap();
    let text = s.text().unwrap();
    assert_eq!(&text, b"Before\n\xef\xbf\xbc\nAfter");
    s.insert_math().unwrap();
    assert_eq!(s.text().unwrap(), b"Before\n\xef\xbf\xbc\nAfter\n$$$$\n");
    let payload = s.editor().payload().unwrap();
    let restored = Editor::from_payload(&payload).unwrap();
    assert_eq!(restored.text().unwrap(), s.text().unwrap());
    assert_eq!(restored.assets().len(), 1);
}

#[test]
fn managed_invalid_image_does_not_change_the_document() {
    let mut s = session();
    s.insert_text("unchanged").unwrap();
    assert!(s.insert_image_bytes(b"not a PNG").is_err());
    assert_eq!(s.text().unwrap(), b"unchanged");
    assert_eq!(s.editor().assets().len(), 0);
}
