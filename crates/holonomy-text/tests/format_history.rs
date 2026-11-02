use holonomy_text::{
    ActionKind, Editor, EditorError, SpanPolicy, STYLE_BOLD, STYLE_CODE, STYLE_DECORATIONS,
    STYLE_HEADER, STYLE_ITALIC, STYLE_KNOWN, STYLE_STRIKE, STYLE_SUBSCRIPT, STYLE_SUPERSCRIPT,
    STYLE_UNDERLINE,
};

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

/// **The four decoration bits are formatting, and they survive undo and redo like every other flag.**
///
/// **The reason this is a gate and not an assumption.** `format_range` records a run per style boundary
/// in a 20-byte undo record and the span map merges adjacent runs whose `same_style` agrees. A bit that
/// the undo record did not carry would come back as plain on undo *and* would let two differently
/// ornamented runs merge into one — a document where undoing a strikethrough silently unbolds a word.
/// The eight bits are one `u16` and the record stores the whole word, so this should hold; the point of
/// the test is that "should" is the word a gate replaces.
#[test]
fn the_decoration_bits_round_trip_through_format_undo() {
    let mut e = Editor::from_text(b"abcdefgh").unwrap();
    let all = STYLE_UNDERLINE | STYLE_STRIKE | STYLE_SUPERSCRIPT | STYLE_SUBSCRIPT;
    e.style_range(0, 4, STYLE_BOLD | all, 0x112233).unwrap();
    assert_eq!(e.style_at(0).style_flags, STYLE_BOLD | all);
    // **`is_header` is the one query that does not see the new bits**, and it must not: a decoration is
    // not a heading, and a heading's line height comes from `STYLE_HEADER` alone.
    assert!(!e.style_at(0).is_header());
    // **And the face is Bold, because a decoration is not a face.** An underline that resolved to an
    // atlas index would draw the wrong glyphs rather than a line.
    assert_eq!(e.style_at(0).atlas_style(), 1);

    // Undo and redo carry all of them back and forth.
    e.undo().unwrap();
    assert_eq!(
        e.style_at(0).style_flags,
        0,
        "the undo took the decorations with it"
    );
    e.redo().unwrap();
    assert_eq!(e.style_at(0).style_flags, STYLE_BOLD | all);

    // **And `STYLE_DECORATIONS` is exactly the four bits above**, which is what a single "clear the
    // ornaments" mask has to be. A mask that missed one would leave a strikethrough on text the user
    // asked to have no formatting at all.
    assert_eq!(all, STYLE_DECORATIONS);
    // **Every bit in `STYLE_DECORATIONS` is in `STYLE_KNOWN`**, so a file carrying an ornament is not a
    // file the importer calls corrupt.
    assert_eq!(STYLE_DECORATIONS & !STYLE_KNOWN, 0);
    // And the three face bits that existed before are still in it, so the mask did not replace them.
    assert_eq!(
        STYLE_BOLD | STYLE_ITALIC | STYLE_CODE | STYLE_HEADER | STYLE_DECORATIONS,
        STYLE_KNOWN
    );
}

/// **Two runs that differ only by a decoration do not merge into one.**
///
/// `TextIntervalSpan::same_style` compares the whole flag word, and a version that compared only the
/// face bits -- which is what an optimisation written against the four original bits would do -- would
/// merge `bold` and `bold+underline` into one run and draw the whole phrase underlined. The failure is
/// invisible in a document where nothing is adjacent, which is most of them.
#[test]
fn a_decoration_boundary_splits_the_run() {
    let mut e = Editor::from_text(b"abcd").unwrap();
    e.style_range(0, 2, STYLE_BOLD, 0).unwrap();
    e.style_range(2, 4, STYLE_BOLD | STYLE_UNDERLINE, 0)
        .unwrap();
    let runs = e.spans().runs_in(0, 4);
    assert_eq!(
        runs.len(),
        2,
        "the underline must start a new run: {runs:?}"
    );
    assert_eq!(runs[0].0.style_flags, STYLE_BOLD);
    assert_eq!(runs[1].0.style_flags, STYLE_BOLD | STYLE_UNDERLINE);
    // Both still draw in the Bold face, which is the pairing the painter relies on.
    assert_eq!(runs[0].0.atlas_style(), runs[1].0.atlas_style());
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
    assert!(!e
        .format_range(0, 3, |s| (s.style_flags, s.color_rgb))
        .unwrap());
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
        e.style_range(
            i,
            i + 1,
            if i % 2 == 0 { STYLE_BOLD } else { STYLE_ITALIC },
            0,
        )
        .unwrap();
    }
    assert!(matches!(
        e.format_range(0, count as u32, |_| (0, 0)),
        Err(EditorError::Undo(_))
    ));
    assert_eq!(e.undo_depth(), 0);
    assert_eq!(e.style_at(0).style_flags, STYLE_BOLD);
    assert_eq!(e.style_at(1).style_flags, STYLE_ITALIC);
}
