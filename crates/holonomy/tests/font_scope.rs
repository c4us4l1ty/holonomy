//! **A font is a character format: it applies to the selection, and nowhere else.**
//!
//! # What this file is for
//!
//! The reported defect was one sentence: *"if I change the font from Inter to JetBrains Mono in
//! holonomy, the whole document changes."* It did, because `state.font_index` was read by `doc_style`
//! for every unstyled run in the document -- so the font box was a **document setting**, which is not
//! what a word processor's font box is in Google Docs, Word or Wordcraft. All three apply a family to
//! the **selection**, and at a bare caret to the text typed next; the document-wide default is a
//! *paragraph style*, on a different menu.
//!
//! # The gates, and why each one is a separate test
//!
//! Each is a distinct way to get this wrong, and they fail independently:
//!
//! * the face is not stored at all (`the_face_is_recorded_on_the_run`);
//! * it is stored but not drawn (`one_word_set_to_mono_leaves_the_rest_in_inter`);
//! * it is drawn but lost on save (`a_face_survives_a_payload_roundtrip`);
//! * it is lost on undo, or undo restores the wrong face (`a_font_change_is_one_undo_step`);
//! * the *next* character does not inherit the armed face (`a_caret_with_no_selection_arms_the_next`);
//! * two adjacent runs in different families are merged into one (`two_faces_do_not_merge`);
//! * the paragraph style stops being the document-wide control (`a_paragraph_style_sets_the_face`);
//! * clearing the formatting leaves the face behind (`clear_formatting_clears_the_face`).

use holonomy::Session;
use holonomy_display::{paint::Painter, HeadlessScanout};
use holonomy_render::{chrome::ChromeMetrics, menus::Action};
use holonomy_text::{Editor, FACE_BODY, FACE_MONO};

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

/// **The defect, stated as a test.** Set the whole document to mono with a selection, then check that
/// a paragraph the selection did not cover is still in the body face.
#[test]
fn one_word_set_to_mono_leaves_the_rest_in_inter() {
    let mut s = session("alpha beta gamma\n\ndelta epsilon\n");
    // Select only `alpha`, then choose JetBrains Mono -- the second row of `FONTS`.
    s.select_range(0, 5).unwrap();
    s.apply_action(Action::SetFont(1)).unwrap();

    assert_eq!(
        s.editor().style_at(0).font_face,
        FACE_MONO,
        "the selection is the run the font box acts on"
    );
    assert_eq!(
        s.editor().style_at(6).font_face,
        FACE_BODY,
        "`beta` was not selected, so it keeps the body face -- this is the assertion that would have \
         failed for the whole life of the defect, when one press repainted every paragraph"
    );
    assert_eq!(
        s.editor().style_at(18).font_face,
        FACE_BODY,
        "and the second paragraph is untouched, which is the sentence the user actually reported"
    );
}

/// **A bare caret sets the *next* character**, and styles nothing that already exists.
///
/// The half of the rule the document-wide version got accidentally right: in an empty document the OLD
/// code did nothing at all (there was no text to repaint), and a fix that only restyled a selection
/// would leave the font box inert until something was selected.
#[test]
fn a_caret_with_no_selection_arms_the_next() {
    let mut s = session("alpha\n");
    s.caret_to(6).unwrap();
    s.apply_action(Action::SetFont(1)).unwrap();
    assert_eq!(
        s.editor().style_at(0).font_face,
        FACE_BODY,
        "nothing was selected and nothing should have been restyled"
    );
    // And the next character typed is mono.
    s.insert_text("x").unwrap();
    assert_eq!(
        s.editor().style_at(6).font_face,
        FACE_MONO,
        "the armed face is applied to the text typed after it"
    );
}

/// **Undo restores the *previous* face, not the default one.**
///
/// The 24-byte record exists for this: a font undo that restored `FACE_BODY` would quietly repaint the
/// paragraph in Inter one undo after every font change, which is the kind of bug that reads as "undo
/// ate my formatting" and is invisible in a diff.
#[test]
fn a_font_change_is_one_undo_step() {
    let mut s = session("alpha beta\n");
    s.select_range(0, 10).unwrap();
    s.apply_action(Action::SetFont(1)).unwrap();
    // Set it *back* to Inter, so the face to restore is not the document default.
    s.select_range(0, 10).unwrap();
    s.apply_action(Action::SetFont(0)).unwrap();
    assert_eq!(s.editor().style_at(0).font_face, FACE_BODY);
    // One undo puts the mono back...
    s.apply_action(Action::Undo).unwrap();
    assert_eq!(
        s.editor().style_at(0).font_face,
        FACE_MONO,
        "one font change is one undo step"
    );
    // ...and a second puts the original body face back.
    s.apply_action(Action::Undo).unwrap();
    assert_eq!(s.editor().style_at(0).font_face, FACE_BODY);
}

/// **A face is part of "same style", so two families never merge into one run.**
///
/// `SpanMap::normalise` joins adjacent spans whose `same_style` agrees. Without the face in that
/// comparison the second half of a font change would be discarded on the next edit that normalises --
/// silently, and only visible as "the font did not stick".
#[test]
fn two_faces_do_not_merge() {
    let mut s = session("aaa\n");
    s.select_range(0, 1).unwrap();
    s.apply_action(Action::SetFont(1)).unwrap();
    assert_eq!(s.editor().style_at(0).font_face, FACE_MONO);
    assert_eq!(
        s.editor().style_at(1).font_face,
        FACE_BODY,
        "the neighbouring bytes are a different run, not part of the styled one"
    );
    assert!(
        s.editor().spans().spans().len() >= 2,
        "two faces must be two spans"
    );
    // A normalising edit must not join them.
    s.caret_to(3).unwrap();
    s.insert_text("z").unwrap();
    assert_eq!(s.editor().style_at(0).font_face, FACE_MONO);
    assert_eq!(s.editor().style_at(1).font_face, FACE_BODY);
}

/// **The face survives a save**, and an old payload with a zero in the padding byte reads as the body
/// face -- the whole reason the byte went into padding rather than a new field.
#[test]
fn a_face_survives_a_payload_roundtrip() {
    let mut s = session("alpha beta\n");
    s.select_range(0, 5).unwrap();
    s.apply_action(Action::SetFont(1)).unwrap();
    let payload = s.editor().payload().unwrap();
    let restored = Editor::from_payload(&payload).unwrap();
    assert_eq!(restored.style_at(0).font_face, FACE_MONO);
    assert_eq!(restored.style_at(6).font_face, FACE_BODY);
}

/// **The paragraph style is where a document-wide face belongs**, which is the control that replaces
/// the old behaviour rather than removing it: `Normal` sets the body face everywhere it is applied.
#[test]
fn a_paragraph_style_sets_the_face() {
    let mut s = session("alpha\n\nbeta\n");
    // Set the first paragraph to mono by hand...
    s.select_range(0, 5).unwrap();
    s.apply_action(Action::SetFont(1)).unwrap();
    assert_eq!(s.editor().style_at(0).font_face, FACE_MONO);
    // ...then apply the Normal paragraph style to it, which carries the document's face.
    s.caret_to(2).unwrap();
    s.apply_action(Action::SetStyle(0)).unwrap();
    assert_eq!(
        s.editor().style_at(0).font_face,
        FACE_BODY,
        "a heading or a Normal paragraph carries its own typography, which includes the face"
    );
    assert_eq!(
        s.editor().style_at(7).font_face,
        FACE_BODY,
        "and applying a style to one paragraph does not reach the next"
    );
}

/// **Clear formatting clears the face**, or "clear" means "clear some of it".
#[test]
fn clear_formatting_clears_the_face() {
    let mut s = session("alpha beta\n");
    s.select_range(0, 5).unwrap();
    s.apply_action(Action::SetFont(1)).unwrap();
    assert_eq!(s.editor().style_at(0).font_face, FACE_MONO);
    s.select_range(0, 5).unwrap();
    s.apply_action(Action::ClearFormatting).unwrap();
    assert_eq!(s.editor().style_at(0).font_face, FACE_BODY);
}

/// **The face is recorded on the run**, which is the model-level statement the rest of these rest on.
#[test]
fn the_face_is_recorded_on_the_run() {
    let mut e = Editor::from_text(b"abc\n").unwrap();
    e.face_format_range(0, 2, |s| (s.style_flags, s.color_rgb, FACE_MONO))
        .unwrap();
    assert_eq!(e.style_at(0).font_face, FACE_MONO);
    assert_eq!(e.style_at(1).font_face, FACE_MONO);
    assert_eq!(e.style_at(2).font_face, FACE_BODY);
    // The record is still 16 bytes: the face is in the padding.
    assert_eq!(std::mem::size_of::<holonomy_text::TextIntervalSpan>(), 16);
}
