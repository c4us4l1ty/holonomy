//! Formatting through the same actions used by the toolbar.
use holonomy::Session;
use holonomy_display::{paint::Painter, HeadlessScanout};
use holonomy_render::{chrome::ChromeMetrics, menus::Action};
use holonomy_text::{Editor, STYLE_BOLD, STYLE_HEADER, STYLE_ITALIC};

fn session(text: &str) -> Session<'static> {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    let atlas = ATLAS.get_or_init(|| {
        let (atlas, _) = holonomy_assets::build_atlas(&[16]).unwrap();
        Box::leak(Box::new(atlas))
    });
    let metrics = ChromeMetrics::DESKTOP;
    Session::new(
        Editor::from_text(text.as_bytes()).unwrap(),
        Painter::new(atlas, 0),
        Box::new(HeadlessScanout::new(metrics.width, metrics.height)),
        metrics,
    )
}

#[test]
fn heading_applies_to_the_current_paragraph_and_survives_payload_roundtrip() {
    let mut s = session("First paragraph\nSecond paragraph");
    s.caret_to(4).unwrap();
    s.apply_action(Action::SetStyle(1)).unwrap();
    assert_eq!(
        s.editor().style_at(0).style_flags & (STYLE_HEADER | STYLE_BOLD),
        STYLE_HEADER | STYLE_BOLD
    );
    assert_eq!(s.editor().style_at(16).style_flags, 0);
    let payload = s.editor().payload().unwrap();
    let restored = Editor::from_payload(&payload).unwrap();
    assert_eq!(
        restored.style_at(0).style_flags & STYLE_HEADER,
        STYLE_HEADER
    );
    assert_eq!(restored.style_at(16).style_flags, 0);
}

#[test]
fn selection_styles_whole_paragraphs_without_capturing_the_next_one() {
    let mut s = session("first\nsecond\nthird");
    s.select_range(2, 13).unwrap(); // ends at the start of the third paragraph
    s.apply_action(Action::SetStyle(2)).unwrap();
    assert_eq!(
        s.editor().style_at(0).style_flags & STYLE_ITALIC,
        STYLE_ITALIC
    );
    assert_eq!(
        s.editor().style_at(6).style_flags & STYLE_ITALIC,
        STYLE_ITALIC
    );
    assert_eq!(s.editor().style_at(13).style_flags, 0);
}

#[test]
fn empty_paragraph_preset_affects_new_text() {
    let mut s = session("");
    s.apply_action(Action::SetStyle(1)).unwrap();
    s.insert_text("Title").unwrap();
    let style = s.editor().style_at(0);
    assert_eq!(
        style.style_flags & (STYLE_HEADER | STYLE_BOLD),
        STYLE_HEADER | STYLE_BOLD
    );
    assert_eq!(style.color_rgb, 0x001A_5C75);
}

#[test]
fn colour_and_inline_styles_do_not_erase_each_other() {
    let mut s = session("abcdef");
    s.select_range(0, 3).unwrap();
    s.apply_action(Action::ToggleBold).unwrap();
    s.select_range(3, 6).unwrap();
    s.apply_action(Action::ToggleItalic).unwrap();
    s.select_range(0, 6).unwrap();
    s.apply_action(Action::SetColour(0)).unwrap();
    assert_eq!(s.editor().style_at(0).style_flags & STYLE_BOLD, STYLE_BOLD);
    assert_eq!(
        s.editor().style_at(3).style_flags & STYLE_ITALIC,
        STYLE_ITALIC
    );
    assert_eq!(
        s.editor().style_at(0).color_rgb,
        s.editor().style_at(3).color_rgb
    );
    s.apply_action(Action::ToggleBold).unwrap();
    assert_eq!(s.editor().style_at(0).style_flags & STYLE_BOLD, 0);
    assert_eq!(
        s.editor().style_at(3).style_flags & STYLE_ITALIC,
        STYLE_ITALIC
    );
    assert_ne!(s.editor().style_at(0).color_rgb, 0);
}

#[test]
fn armed_bold_and_colour_are_both_applied() {
    let mut s = session("");
    s.apply_action(Action::ToggleBold).unwrap();
    s.apply_action(Action::SetColour(0)).unwrap();
    s.insert_text("x").unwrap();
    let style = s.editor().style_at(0);
    assert_eq!(style.style_flags & STYLE_BOLD, STYLE_BOLD);
    assert_ne!(style.color_rgb, 0);
}

#[test]
fn viewing_mode_refuses_paragraph_formatting() {
    let mut s = session("text");
    s.apply_action(Action::SetMode(1)).unwrap();
    assert!(s.apply_action(Action::SetStyle(1)).is_err());
    assert_eq!(s.editor().style_at(0).style_flags, 0);
}

#[test]
fn enter_at_the_end_of_a_heading_starts_normal_text() {
    let mut s = session("");
    s.apply_action(Action::SetStyle(1)).unwrap();
    s.insert_text("Heading\nBody").unwrap();
    assert_ne!(s.editor().style_at(0).style_flags & STYLE_HEADER, 0);
    assert_eq!(s.editor().style_at(8).style_flags & STYLE_HEADER, 0);
    assert_eq!(s.editor().style_at(8).color_rgb, 0);
}

#[test]
fn bold_toolbar_button_formats_the_selection() {
    use holonomy::store::NoSource;
    use holonomy_input::{
        pointer::encode_record_at, RecordDecoder, BTN_LEFT, EV_KEY, EV_REL, EV_SYN, REL_X, REL_Y,
    };
    use holonomy_render::widgets::{self, Tool};
    let mut s = session("selected text");
    s.select_range(0, 8).unwrap();
    let rect = widgets::place_toolbar(&s.chrome_layout(), widgets::TOOLBAR_PAD)
        .into_iter()
        .find(|p| p.tool == Tool::Bold)
        .unwrap()
        .rect;
    let x = (rect.x + rect.width / 2) as i32;
    let y = (rect.y + rect.height / 2) as i32;
    let mut decoder = RecordDecoder::new();
    for event in [
        encode_record_at(EV_REL, REL_X, x, 0, 0),
        encode_record_at(EV_REL, REL_Y, y, 0, 0),
        encode_record_at(EV_KEY, BTN_LEFT, 1, 0, 0),
        encode_record_at(EV_KEY, BTN_LEFT, 0, 0, 0),
        encode_record_at(EV_SYN, 0, 0, 0, 0),
    ] {
        decoder.push(&event);
    }
    while let Some(event) = decoder.next_event() {
        s.handle_pointer(&mut NoSource, event).unwrap();
    }
    assert_ne!(s.editor().style_at(0).style_flags & STYLE_BOLD, 0);
    assert_eq!(s.editor().style_at(9).style_flags & STYLE_BOLD, 0);
}

#[test]
fn undo_and_redo_formatting_leave_text_intact() {
    use holonomy_input::{Command, Hotkey};
    let mut s = session("first\nsecond");
    s.select_range(0, 12).unwrap();
    s.apply_action(Action::SetStyle(1)).unwrap();
    assert_ne!(s.editor().style_at(0).style_flags & STYLE_HEADER, 0);
    s.apply(Command::Hotkey(Hotkey::Undo)).unwrap();
    assert_eq!(s.text().unwrap(), b"first\nsecond");
    assert_eq!(s.editor().style_at(0).style_flags, 0);
    s.apply(Command::Hotkey(Hotkey::Redo)).unwrap();
    assert_ne!(s.editor().style_at(0).style_flags & STYLE_HEADER, 0);
    assert_ne!(s.editor().style_at(6).style_flags & STYLE_HEADER, 0);
}

#[test]
fn keyboard_bold_uses_the_same_selection_and_undo_path() {
    use holonomy_input::{
        keymap::{KEY_B, KEY_LEFTCTRL},
        InputEvent,
    };
    let mut s = session("example");
    s.select_range(0, 7).unwrap();
    s.handle_event(InputEvent::press(KEY_LEFTCTRL)).unwrap();
    s.handle_event(InputEvent::press(KEY_B)).unwrap();
    s.handle_event(InputEvent::release(KEY_B)).unwrap();
    s.handle_event(InputEvent::release(KEY_LEFTCTRL)).unwrap();
    assert_ne!(s.editor().style_at(0).style_flags & STYLE_BOLD, 0);
    s.undo().unwrap();
    assert_eq!(s.editor().style_at(0).style_flags & STYLE_BOLD, 0);
    assert_eq!(s.text().unwrap(), b"example");
}

#[test]
fn inline_toggle_does_not_discard_an_armed_heading() {
    let mut s = session("");
    s.apply_action(Action::SetStyle(1)).unwrap();
    s.apply_action(Action::ToggleItalic).unwrap();
    s.insert_text("Heading").unwrap();
    let flags = s.editor().style_at(0).style_flags;
    assert_ne!(flags & STYLE_HEADER, 0);
    assert_ne!(flags & STYLE_BOLD, 0);
    assert_ne!(flags & STYLE_ITALIC, 0);
}
