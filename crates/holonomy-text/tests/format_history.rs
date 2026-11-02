use holonomy_text::{ActionKind, Editor, EditorError, SpanPolicy, STYLE_BOLD, STYLE_ITALIC};

#[test]
fn mixed_formatting_is_one_undoable_and_redoable_action() {
    let mut e = Editor::from_text(b"abcdef").unwrap();
    e.style_range(0, 3, STYLE_BOLD, 0x123456).unwrap();
    e.style_range(3, 6, STYLE_ITALIC, 0).unwrap();
    assert!(e.format_range(1, 5, |s| (s.style_flags, 0xff0000)).unwrap());
    assert_eq!(e.undo_depth(), 1);
    assert_eq!(e.undo().unwrap().kind, ActionKind::Format);
    assert_eq!(e.text().unwrap(), b"abcdef");
    assert_eq!(e.style_at(1).color_rgb, 0x123456);
    assert_eq!(e.style_at(3).color_rgb, 0);
    assert_eq!(e.style_at(3).style_flags, STYLE_ITALIC);
    assert_eq!(e.redo().unwrap().kind, ActionKind::Format);
    assert_eq!(e.style_at(1).color_rgb, 0xff0000);
    assert_eq!(e.style_at(3).color_rgb, 0xff0000);
    assert_eq!(e.style_at(0).color_rgb, 0x123456);
    assert_eq!(e.style_at(5).color_rgb, 0);
}

#[test]
fn undo_redo_interleaves_formatting_typing_and_deletion() {
    let mut e = Editor::from_text(b"abcd").unwrap();
    e.format_range(0, 4, |_| (STYLE_BOLD, 7)).unwrap();
    e.insert_at(4, b"x", SpanPolicy::GrowIntoInsert).unwrap();
    e.style_range(4, 5, STYLE_ITALIC, 9).unwrap(); // inserted-text typing attributes
    e.delete_at(0, 1).unwrap();
    e.undo().unwrap(); // delete
    e.undo().unwrap(); // insert
    e.undo().unwrap(); // formatting
    assert_eq!(e.text().unwrap(), b"abcd");
    assert_eq!(e.style_at(0).style_flags, 0);
    e.redo().unwrap();
    e.redo().unwrap();
    assert_eq!(e.text().unwrap(), b"abcdx");
    assert_eq!(e.style_at(4).style_flags, STYLE_ITALIC);
    assert_eq!(e.style_at(4).color_rgb, 9);
    e.redo().unwrap();
    assert_eq!(e.text().unwrap(), b"bcdx");
    assert_eq!(e.style_at(0).style_flags, STYLE_BOLD);
    assert_eq!(e.style_at(3).style_flags, STYLE_ITALIC);
}

#[test]
fn no_op_and_refused_format_leave_history_and_redo_intact() {
    let mut e = Editor::from_text(b"abc").unwrap();
    e.format_range(0, 3, |_| (STYLE_BOLD, 0)).unwrap();
    e.undo().unwrap();
    assert!(!e.format_range(0, 3, |s| (s.style_flags, s.color_rgb)).unwrap());
    assert!(e.format_range(0, 4, |_| (STYLE_ITALIC, 0)).is_err());
    e.redo().unwrap();
    assert_eq!(e.style_at(0).style_flags, STYLE_BOLD);
    e.undo().unwrap();
    e.format_range(0, 3, |_| (STYLE_ITALIC, 0)).unwrap();
    assert!(matches!(e.redo(), Err(EditorError::NothingToRedo)));
}

#[test]
fn oversize_format_record_is_refused_before_mutation() {
    let count = holonomy_text::ARENA_BYTES / 20 + 1;
    let mut e = Editor::from_text(&vec![b'a'; count]).unwrap();
    for i in 0..count as u32 {
        e.style_range(i, i + 1, if i % 2 == 0 { STYLE_BOLD } else { STYLE_ITALIC }, 0).unwrap();
    }
    assert!(matches!(e.format_range(0, count as u32, |_| (0, 0)), Err(EditorError::Undo(_))));
    assert_eq!(e.undo_depth(), 0);
    assert_eq!(e.style_at(0).style_flags, STYLE_BOLD);
    assert_eq!(e.style_at(1).style_flags, STYLE_ITALIC);
}
