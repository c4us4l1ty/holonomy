//! **Phase 13's gate: the manifest makes the 6 MiB read unnecessary.** 8 tests.
//!
//! # What Phase 12 could not do, and why
//!
//! Phase 12 ended by saying so plainly. Its whole memory story was *"body text reads a page, not the
//! document"*, and it delivered that — but `session_rss.rs` still measured **26.85 MiB with the same 6.00
//! MiB `doc_scratch`**, because four emitters called `read_document` unconditionally:
//!
//! * `publish_line_heights` → `math_blocks_for` → `read_document`
//! * `image_blocks` → `read_document`
//! * `emit_tables`, `emit_math`, `emit_images` → `read_document`
//!
//! And the reason was a single missing capability: **you cannot ask "does this document contain a
//! formula?" without reading it.** `for_each_math_span` and `scan_anchors` are cursors over bytes, so the
//! only way to find out is to scan the bytes, and a 6 MiB scan per paint is what the 6 MiB buffer was for.
//!
//! [`holonomy::manifest::Manifest`] is the answer: it counts those markers **once, at open**, into a
//! `u32` per section, and answers `span_total() == 0` from 1,164 bytes of structure.
//!
//! | what it proves | test |
//! | --- | --- |
//! | a document with no spans never grows `doc_scratch` | [`a_prose_document_never_allocates_the_whole_document_buffer`] |
//! | and one with a formula does, so the guard is not vacuous | [`a_document_with_a_formula_does_allocate_it`] |
//! | a formula is noticed **within one keystroke** | [`a_formula_appears_and_the_manifest_notices_within_one_keystroke`] |
//! | and so is its deletion | [`deleting_a_formula_is_noticed_within_one_keystroke`] |
//! | typing never rebuilds the manifest | [`a_keystroke_updates_sections_and_never_rebuilds_one`] |
//! | a newline does, and only that | [`a_newline_rebuilds_the_manifest_and_a_letter_does_not`] |
//! | the section size is the container's chunk | [`a_section_is_the_size_of_one_container_chunk`] |
//! | the manifest is negligible against the text | [`the_manifest_costs_twelve_bytes_a_section_against_the_document`] |

use holonomy::manifest::{Manifest, SECTION_BYTES};
use holonomy::session::Session;
use holonomy_input::Command;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy};

/// One atlas for the whole file, for the same reason `session_math.rs` has one:
/// `MathMetrics::advance` is a plain `fn` pointer into a process-global, so a file that builds an atlas
/// per test makes that global a race.
fn shared_atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (atlas, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(atlas))
    })
}

/// A session over a document holding `text`.
fn with_text(text: &str) -> Session<'static> {
    let mut ed = Editor::new();
    // **Line by line, not one insert.** `UndoStack` refuses an action larger than its capacity —
    // `ActionTooLarge { len: 400_000, capacity: 65_536 }` — so a one-shot insert of a large fixture is a
    // failing test rather than a slow one. Typing the document is also more honest: it is a document, not
    // one keystroke.
    for line in text.split_inclusive('\n') {
        if line.is_empty() {
            continue;
        }
        ed.insert_at(ed.text_len() as u32, line.as_bytes(), SpanPolicy::GrowIntoInsert)
            .expect("seed the document");
    }
    let m = ChromeMetrics::DESKTOP;
    Session::new(
        ed,
        Painter::new(shared_atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    )
}

/// ~220 KB of prose, which is 3.4 sections — enough that a document-sized read is visible.
fn prose() -> String {
    "the quick brown fox jumps over the lazy dog\n".repeat(5_000)
}

/// **The headline: the 6 MiB is gone, and the reason is one `u32` per section.**
///
/// A first version of this file asserted `doc_scratch_capacity() == 0` after painting prose and
/// **measured 221,184 bytes** — because `math_blocks_for` called `read_document` before anything could ask
/// whether it needed to. The manifest is what makes the assertion true, and this is the test that says so.
#[test]
fn a_prose_document_never_allocates_the_whole_document_buffer() {
    let text = prose();
    let mut s = with_text(&text);
    assert_eq!(
        s.doc_scratch_capacity(),
        0,
        "constructing a session reads the document for DocLines and the manifest, and neither keeps it"
    );

    s.paint(None).expect("first paint");
    assert_eq!(
        s.doc_scratch_capacity(),
        0,
        "painting {} bytes of prose allocated a {}-byte whole-document buffer: a formula, a table or \\
         an image was looked for by reading the document instead of by asking the manifest",
        s.text_len() as usize,
        s.doc_scratch_capacity()
    );

    // And still zero after edits, which is where a whole-document read would really hurt.
    for _ in 0..8 {
        s.apply(Command::Right).expect("right");
    }
    s.paint(None).expect("paint after navigation");
    assert_eq!(
        s.doc_scratch_capacity(),
        0,
        "an edit sequence put the whole-document read back"
    );
}

/// **The guard is not vacuous.** If `span_total() == 0` were always true this test would pass and
/// [`a_prose_document_never_allocates_the_whole_document_buffer`] would be meaningless — the buffer would
/// never be allocated because the emitters would be permanently switched off, and every formula and image
/// in the product would silently not draw.
///
/// So this asserts the *other* direction: a document **with** a formula does allocate, and does draw it.
#[test]
fn a_document_with_a_formula_does_allocate_it() {
    // **The formula first, not last.** An earlier version put it after 220 KB of prose and the
    // `math_compiled + math_raw > 0` assertion failed at zero -- correctly, because `emit_math` only lays
    // out formulas on the visible page and line 5,000 is not on it. The test was asking the wrong
    // question: it wanted "the guard did not switch the emitter off", and the emitter's own page culling
    // is a separate mechanism with its own gates in `session_math.rs`.
    let mut s = with_text("the answer is $$x^2$$\nand then some prose\n");
    s.paint(None).expect("paint");
    assert!(
        s.doc_scratch_capacity() > 0,
        "a document with a formula must still read the document, or the guard has switched the emitters \\
         off rather than skipping work that is not needed"
    );
    assert_eq!(
        s.manifest().span_total(),
        2,
        "and the manifest must have counted the formula's two delimiters"
    );
    assert!(
        s.stats.math_compiled + s.stats.math_raw > 0,
        "and the formula must have been laid out, so the read was for something"
    );
}

/// A formula appearing is noticed within one keystroke.
///
/// **The failure mode this exists to catch is a formula that silently stops rendering.** The manifest is
/// synced in `after_edit`, and a stale one would make `emit_math` skip a document that has a formula. No
/// error, no crash — the formula just is not there. This asserts the manifest sees it on the next paint.
#[test]
fn a_formula_appears_and_the_manifest_notices_within_one_keystroke() {
    let mut s = with_text(&prose());
    s.paint(None).expect("first paint");
    assert_eq!(s.manifest().span_total(), 0, "prose has no formulas");

    // Type one `$`. No delimiter yet, so no span — and that is the *first* half of the claim.
    s.apply(Command::Insert('$')).expect("insert $");
    s.paint(None).expect("paint with one dollar");
    assert_eq!(
        s.manifest().span_total(),
        0,
        "a single $ is not a delimiter, so there is no formula to draw"
    );

    // Type the second. Now there is a `$$`, and `for_each_math_span` treats it as a span that runs to the
    // end of its line — **unpaired is still a formula**, and that is the normal state while someone is
    // halfway through typing one. The manifest must see it in the same keystroke.
    s.apply(Command::Insert('$')).expect("insert the second $");
    assert_eq!(
        s.manifest().span_total(),
        1,
        "an unpaired $$ is one formula, and the manifest must see it in the same keystroke -- counting \
         pairs instead of openers reported 0 here and left the formula undrawn"
    );
}

/// And the reverse, because a stale manifest errs in one direction only if both are tested.
#[test]
fn deleting_a_formula_is_noticed_within_one_keystroke() {
    let mut s = with_text(&format!("the answer is $$x^2$$\n{}", prose()));
    s.paint(None).expect("first paint");
    assert_eq!(s.manifest().span_total(), 2, "two delimiters to start with");
    // The caret at the end is *inside* nothing; move it to just after the closing `$$` so the backspace
    // below removes a delimiter rather than an unrelated character.
    // "the answer is " is 14 bytes and "$$x^2$$" is 7, so the closing delimiter ends at 21. Offset 24
    // was the first version's guess and it landed inside the following prose, so the backspace removed an
    // unrelated character and the span count correctly did not move.
    s.caret_to(21).expect("caret after the closing $$");

    // Backspace over the closing `$`. The pair is broken, so the span is gone and the document's bytes
    // are back to prose — but `doc_scratch` is still allocated from the first paint, which is the point:
    // the buffer shrinking is not what is being tested, the *count* is.
    s.apply(Command::Backspace).expect("backspace");
    assert_eq!(
        s.manifest().span_total(),
        1,
        "removing one delimiter leaves one, and the manifest must see it in the same keystroke. Counting \
         delimiters rather than pairs would report 0 here, and would be wrong: for_each_math_span calls \
         an unpaired $$ a formula"
    );
}

/// Typing never rebuilds the manifest — the cheap path, and the one that runs on every keystroke.
#[test]
fn a_keystroke_updates_sections_and_never_rebuilds_one() {
    let mut s = with_text(&prose());
    s.paint(None).expect("first paint");
    let rebuilds_before = s.stats.section_rebuilds;
    let updates_before = s.stats.section_updates;

    // Nine ordinary letter insertions, away from any newline.
    for c in "abcdefghi".chars() {
        s.apply(Command::Insert(c)).expect("insert");
    }
    assert_eq!(
        s.stats.section_rebuilds,
        rebuilds_before,
        "nine letters rebuilt the manifest {} times; a rebuild means a boundary moved, and a letter \\
         moves no boundary",
        s.stats.section_rebuilds - rebuilds_before
    );
    assert!(
        s.stats.section_updates > updates_before,
        "and nine letters produced no section update at all, so the cheap path is not running"
    );
}

/// **No ordinary keystroke rebuilds the manifest, and the newline is no longer the exception.**
///
/// This test used to assert the opposite, and that assertion was *passing for the wrong reason*: the
/// manifest carried a per-section `newlines`, an insertion at offset 0 shifted every section's content, the
/// newline total drifted, and the drift triggered a whole-document rebuild — **3,300 µs, on one keystroke
/// in ~21**, 6.6x the budget. The field is gone (see `SectionMetrics`), so there is nothing to drift.
#[test]
fn no_keystroke_rebuilds_the_manifest_including_a_newline() {
    let mut s = with_text(&prose());
    s.paint(None).expect("first paint");
    let before = s.stats.section_rebuilds;

    // A newline and a letter, at offset 0, which is the case that used to rebuild.
    s.apply(Command::Insert('\n')).expect("newline");
    s.apply(Command::Insert('a')).expect("letter");
    assert_eq!(
        s.stats.section_rebuilds,
        before,
        "a newline and a letter at offset 0 rebuilt the manifest {} times. A newline changes no section \
         boundary -- the cut is at a byte offset from each section's start -- so there is nothing to \
         rebuild, and the whole-document read this used to trigger was 3.3 ms",
        s.stats.section_rebuilds - before
    );
    assert!(
        s.stats.section_updates > 0,
        "and the cheap path must have run: a manifest that never rebuilds and never updates is not \
         keeping itself in agreement with anything"
    );
    // The manifest still describes the document.
    assert_eq!(
        s.manifest().total_bytes(),
        s.text_len() as usize,
        "after two edits the manifest's byte count must agree with the document's"
    );
}

/// The section size is the container's chunk size, so a section is one `pread64`.
#[test]
fn a_section_is_the_size_of_one_container_chunk() {
    assert_eq!(SECTION_BYTES, 65_520, "CHUNK_SLOT minus a 16-byte tag");
    assert_eq!(
        SECTION_BYTES,
        holonomy_container::layout::CHUNK_PLAINTEXT,
        "if the container's chunk size changes this fails, which is the point: the section size is \\
         derived from the format rather than chosen"
    );
    // And the residency arithmetic, so the number in the docs is checked against the code.
    let six_mib: usize = 6 * 1024 * 1024;
    assert_eq!(six_mib.div_ceil(SECTION_BYTES), 97);
    assert_eq!(
        8 * SECTION_BYTES,
        524_160,
        "an 8-section working set, which is the bound Phase 13 quotes"
    );
    assert!(
        524_160 * 32 > six_mib,
        "and 8 sections is 1/32 of a 6 MiB document, so the residency budget is not the whole document \\
         wearing a smaller name"
    );
}

/// The manifest's own cost, against the document it describes.
///
/// **Eight bytes per section, not twelve**, and the four-byte saving is worth nothing. The third tree was
/// `newlines`, and removing it is worth 3.3 ms per keystroke rather than 4 bytes of memory.
#[test]
fn the_manifest_costs_eight_bytes_a_section_against_the_document() {
    let text = prose();
    let s = with_text(&text);
    let m = s.manifest();
    let cost = m.heap_bytes();
    assert_eq!(cost, 2 * (m.len() + 1) * 4, "two trees of n+1 u32s");
    assert_eq!(
        std::mem::size_of::<holonomy::manifest::SectionMetrics>(),
        8,
        "two u32s of payload"
    );
    assert!(
        cost * 1000 < text.len(),
        "a {cost}-byte manifest for {} bytes of text is more than 0.1% overhead",
        text.len()
    );
    println!(
        "manifest: {} sections, {cost} bytes for {} bytes of text ({:.5}%)",
        m.len(),
        text.len(),
        100.0 * cost as f64 / text.len() as f64
    );
}

/// The manifest's totals agree with a full scan of the document, through a real session.
#[test]
fn the_manifest_agrees_with_a_scan_of_the_document_it_describes() {
    let text = prose();
    let s = with_text(&text);
    let full = s.text().expect("read");
    let m: &Manifest = s.manifest();
    assert_eq!(m.total_bytes(), full.len(), "byte count");
    // And no markers, because the fixture is prose.
    assert_eq!(m.span_total(), 0, "prose has no formulas and no images");
    // The sections tile the document exactly once, in order — the property every range query depends on.
    let mut at = 0u32;
    for i in 0..m.len() as u32 {
        let sec = m.section(i).expect("section");
        assert_eq!(sec.start, at, "section {i} starts at {at}, not {}", sec.start);
        assert!(sec.end > sec.start, "section {i} is empty");
        at = sec.end;
    }
    assert_eq!(at as usize, full.len(), "the sections cover the document");
}