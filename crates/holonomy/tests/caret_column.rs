//! **The caret's column counts characters, not bytes.** 8 tests.
//!
//! # The finding this file closes
//!
//! Part 20 fixed half of a bug and recorded the other half in `tests/pointer.rs`:
//!
//! > `Session::caret_to` computes it as `caret - line_start(caret)` -- a **byte** offset -- so for a
//! > caret at byte 2 of `éa` it reports column 2, and `Caret::locate` then draws the caret at
//! > `text.x + 2 * cell_w`, one cell right of where the click put it.
//!
//! and then, correctly, declined to assert `== 1` (which would fail) or `== 2` (which would enshrine
//! the bug).
//!
//! **It was never only a click bug.** Every keystroke goes through the same `caret_to`, so typing `é`
//! has always moved the drawn caret one cell too far.
//!
//! # What "correct" means here, and where it stops
//!
//! `Painter::text` advances `cell_w` **per UTF-8 scalar** — `TextRun`'s `k` indexes scalars — so the
//! caret cell must be the scalar index. **A combining mark is a second scalar at the same place and a
//! CJK ideograph is two cells wide, and this counter gets both wrong.** That is recorded here rather
//! than fixed: the fix is a display-width table, and
//! [`a_combining_sequence_counts_two_scalars_and_documents_the_limit`] says so on purpose.
//!
//! | what it proves | test |
//! | --- | --- |
//! | the column is a character count | [`the_column_counts_characters_not_bytes`] |
//! | **and the caret is drawn where the character ends** | [`the_caret_is_drawn_under_the_character_that_ends_there`] |
//! | a click and the drawn cell agree, both ways | [`a_click_and_the_drawn_cell_agree_in_both_directions`] |
//! | a whole non-ASCII line, not one character | [`a_whole_line_of_multi_byte_characters_counts_right`] |
//! | **column 0 reads nothing at all** | [`a_caret_at_the_start_of_its_line_reads_nothing`] |
//! | the paint path is authoritative for a sparse line | [`a_paint_over_a_sparse_document_puts_the_caret_right`] |
//! | and the width table is what is missing | [`a_combining_sequence_counts_two_scalars_and_documents_the_limit`] |
//! | the constructor reconciles what part 22 found | [`a_session_opened_on_a_document_reconciles_its_caret`] |

use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::chrome::{Caret, ChromeMetrics};
use holonomy_text::{Editor, SpanPolicy};

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

/// A session over `text`, with the caret at the end of it.
fn session(text: &str) -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    if !text.is_empty() {
        ed.insert_at(0, text.as_bytes(), SpanPolicy::GrowIntoInsert)
            .expect("room");
    }
    Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    )
}

/// The x the caret is drawn at, from the state the painter reads.
fn caret_x(s: &Session<'_>) -> u32 {
    let l = s.chrome_layout();
    let m = &s.chrome.metrics;
    Caret::locate(&l, m, &s.state)
        .unwrap_or_else(|| panic!("the caret is not on screen"))
        .cell
        .x
}

/// **The column is a character count and not a byte offset.**
///
/// **`éa` is the fixture and has to be:** the caret at byte 2 is after the two-byte `é`, and a byte
/// count calls that column 2 where a character count calls it 1. An ASCII fixture cannot tell the two
/// apart, which is the same reason part 20 chose this line for `offset_of`.
#[test]
fn the_column_counts_characters_not_bytes() {
    let mut s = session("éa\n");
    s.caret_to(2).expect("after the é");
    assert_eq!(s.caret(), 2, "byte 2 is a character boundary");
    assert_eq!(
        s.caret_column(),
        1,
        "one character has been passed, even though two bytes have"
    );

    // And the end of the line is two characters, though three bytes.
    s.caret_to(3).expect("after the a");
    assert_eq!(s.caret_column(), 2, "two characters before the newline");

    // **The other direction: a caret at the byte offset of the `é` snaps back and is column 0.** If the
    // column were still computed from the *requested* offset rather than the landed one, this would be
    // 1.
    s.caret_to(1).expect("inside the é");
    assert_eq!(s.caret(), 0, "snapped back to the character boundary");
    assert_eq!(s.caret_column(), 0);
}

/// **The caret is drawn at the cell the character ends at.**
///
/// **This is the half that was visible.** `Caret::locate` places the cell at
/// `text.x + caret_column * cell_w`, so a byte column drew the caret one cell past the glyph it belongs
/// to — and the symptom is "the caret is slightly too far right", which reads as a metrics problem and
/// is an arithmetic one.
#[test]
fn the_caret_is_drawn_under_the_character_that_ends_there() {
    let mut s = session("éa\n");
    let l = s.chrome_layout();
    let cell_w = s.chrome.metrics.cell_w;

    s.caret_to(2).expect("after the é");
    assert_eq!(
        caret_x(&s),
        l.text.x + cell_w,
        "one cell in, under the a — the column and the pixels agree"
    );

    s.caret_to(3).expect("after the a");
    assert_eq!(
        caret_x(&s),
        l.text.x + 2 * cell_w,
        "and two cells in, which is byte 3 and character 2"
    );
}

/// **A click and the drawn cell agree, in both directions.**
///
/// **`offset_of` (part 20) and `refresh_caret_column` (part 22) are inverses, and this is the gate that
/// says so.** Part 20 fixed the forward direction — a click in column 1 of `éa` puts the caret at byte
/// 2 — and this adds the backward direction, which is the one that was missing: **having arrived at a
/// byte offset, the column computed from it must be the column that got you there.** A pair that is
/// each correct alone and disagree with each other puts the caret back where it started.
#[test]
fn a_click_and_the_drawn_cell_agree_in_both_directions() {
    use holonomy::store::NoSource;
    use holonomy_input::pointer::encode_record;
    use holonomy_input::{RecordDecoder, BTN_LEFT, EV_KEY, EV_REL, EV_SYN, REL_X, REL_Y};

    let mut s = session("éa\n");
    let l = s.chrome_layout();
    let cell_w = s.chrome.metrics.cell_w;

    let mut click = |s: &mut Session<'static>, column: u32| {
        let x = (l.text.x + column * cell_w) as i32;
        let y = l.text.y as i32;
        let mut d = RecordDecoder::new();
        for ev in [
            encode_record(EV_REL, REL_X, x),
            encode_record(EV_REL, REL_Y, y),
            encode_record(EV_KEY, BTN_LEFT, 1),
            encode_record(EV_KEY, BTN_LEFT, 0),
            encode_record(EV_SYN, 0, 0),
        ] {
            d.push(&ev);
        }
        while let Some(e) = d.next_event() {
            s.handle_pointer(&mut NoSource, e).expect("pointer");
        }
    };

    // Column 1 is one cell in. The click lands at byte 2 and the column reads back as 1.
    click(&mut s, 1);
    assert_eq!(s.caret(), 2, "the click's half, from part 20");
    assert_eq!(s.caret_column(), 1, "and the column reads back the same");
    assert_eq!(
        caret_x(&s),
        l.text.x + cell_w,
        "so the caret is drawn where the click put it"
    );

    // And the round trip the other way: every column on the line round-trips.
    for column in 0..=2u32 {
        click(&mut s, column);
        assert_eq!(
            s.caret_column(),
            column,
            "column {column} round-trips through a click"
        );
    }
}

/// **A whole line of multi-byte characters counts right, not one character of one.**
///
/// **The fixture is deliberately all non-ASCII**, because a gate that proves `é` works can be satisfied
/// by an off-by-one that happens to be zero. Four two-byte characters and one ASCII one: every prefix
/// is asserted, so an error anywhere in the walk is caught and not just at the end.
#[test]
fn a_whole_line_of_multi_byte_characters_counts_right() {
    // Four characters (`é`, `é`, `é`, `a`) in 7 bytes, then a newline: **byte 8 is the start of the
    // next line and not column 5**, which is the boundary this table walks off on purpose.
    let mut s = session("éééa\n");
    for (column, byte) in [(0usize, 0usize), (1, 2), (2, 4), (3, 6), (4, 7)] {
        s.caret_to(byte).expect("a boundary");
        assert_eq!(s.caret(), byte as u32, "byte {byte} is a boundary");
        assert_eq!(
            s.caret_column(),
            column as u32,
            "{column} characters in is byte {byte}"
        );
    }

    // **The line boundary resets the count, and that is a separate assertion.** A byte offset walk would
    // report 8 here — the whole line plus the newline — and a gate that only asserted the prefixes
    // would never see it.
    s.caret_to(8).expect("after the newline");
    assert_eq!(s.caret_column(), 0, "a new line starts at column 0");
    assert_eq!(s.caret_line(), 1, "and it is the next line");
}

/// **A caret at the start of its line reads nothing, and that is measurable.**
///
/// **`SessionStats::caret_column_scans` exists for this assertion.** The scan is O(line length) and runs
/// on every caret move, so the question worth gating is not "is it correct" — the other seven are that —
/// but "does it run when there is nothing to count". **Column 0 is the case that matters**, because
/// `DocumentStart`, `Home`, and typing at the beginning of a line all land there, and a scan that ran
/// anyway would be a rope walk on the most common keystroke in an editor.
#[test]
fn a_caret_at_the_start_of_its_line_reads_nothing() {
    let mut s = session("alpha\nbravo\n");

    // Park the caret at the start of line 1 and count what it cost.
    s.caret_to(6).expect("start of bravo");
    assert_eq!(s.caret_column(), 0);
    let before = s.stats.caret_column_scans;
    for _ in 0..8 {
        s.caret_to(6).expect("same place");
    }
    assert_eq!(
        s.stats.caret_column_scans, before,
        "eight moves to column 0 and not one scan"
    );

    // And a caret one character in does scan, so the counter is not simply stuck.
    let before = s.stats.caret_column_scans;
    s.caret_to(7).expect("after the b");
    assert_eq!(s.caret_column(), 1);
    assert_eq!(
        s.stats.caret_column_scans,
        before + 1,
        "and one move off the boundary is one scan"
    );

    // **`DocumentStart` too**, which is the same path with a different entry.
    let before = s.stats.caret_column_scans;
    s.apply(holonomy_input::Command::Hotkey(
        holonomy_input::Hotkey::DocumentStart,
    ))
    .expect("ctrl+Home");
    assert_eq!(s.caret_column(), 0);
    assert_eq!(
        s.stats.caret_column_scans, before,
        "and it does not scan either"
    );
}

/// **A paint puts the caret right even when the keystroke path could not.**
///
/// **This is the reason there are two callers of one function.** `Session::apply` is public *so that* a
/// driver with its own event source can drive it — a stated reason, in the gate for it — so `caret_to`
/// cannot take a `&mut dyn LeafSource` and was not given one. On a container-backed document whose
/// caret line is not resident, the resident-only read in `caret_to` cannot complete, and the column
/// stays a byte count.
///
/// **The paint path is the frame that draws**, and `paint_with` recomputes with a faulting source
/// immediately before `chrome.tree`. So the value `Caret::locate` reads is the authoritative one, and
/// the fallback window is one in which nothing is drawn.
#[test]
fn a_paint_over_a_sparse_document_puts_the_caret_right() {
    use holonomy::store::{open_document, SectionStore, DEFAULT_RESIDENT_SECTIONS};
    use holonomy_container::io::DirectFile;
    use holonomy_container::Wavefunction;
    use holonomy_input::Command;

    const PASS: &str = "correct horse battery staple";
    const ITER: u64 = holonomy_container::TEST_VDF_ITERATIONS;

    // **A container-backed document, so its leaves can be evicted.** The gate-fixture rule this project
    // has learned four times: a gate built on a wrong premise tells you which of the two halves is
    // wrong, so **this fixture makes the failure happen on purpose** rather than hoping the residency
    // schedule lands right. The eviction below is the mechanism.
    let dir = std::env::current_exe()
        .expect("test exe")
        .ancestors()
        .nth(3)
        .expect("target/<profile> layout")
        .join("holonomy-tests")
        .join("caret-column");
    std::fs::create_dir_all(&dir).expect("scratch dir");

    // **One line, long enough to cross a section boundary, made of two-byte characters.**
    //
    // **The boundary is what makes the fallback reachable, and this is how that was worked out.**
    // Evicting a range releases every leaf overlapping it, so the obvious fixture — evict the caret's
    // own line and move onto it — does not work: `caret_to` reads around the offset it is moving to,
    // finds the leaf absent, and returns `LeafAbsent` before the column scan ever runs. That is part
    // 16's rule doing its job, and it is why this fixture is shaped the way it is.
    //
    // **A container-backed rope's leaves are the container's sections**, 65,520 bytes each (the figure
    // `tests/session_open_document.rs` states rather than imports, for the same reason it states it
    // there: a test that re-derives a constant the code also defines is a test that can pass against a
    // different value). So a caret four bytes into section 1, with section 0 evicted, is a caret whose
    // *line prefix* is absent and whose own byte is present — and that is the only state in which the
    // resident scan can fail after a `caret_to` that succeeded.
    const SECTION: usize = 65_520;
    let mut doc: Vec<u8> = Vec::new();
    // **Padding made of two-byte characters, not ASCII — and that is the premise, so it is asserted.**
    //
    // The first version of this fixture padded with `b'a' + (n % 26)`, which is one byte per character:
    // **the byte count and the character count were then equal and every assertion in the test was
    // vacuous.** It failed at `65524 != 2` with a byte count where a character count was expected, and
    // the byte count being *correct as a byte count* is what said so. The rule this project keeps
    // re-learning, once more: **a fixture whose two quantities coincide cannot tell a correct answer
    // from a wrong one.**
    while doc.len() < SECTION + 8 {
        doc.extend_from_slice("é".as_bytes());
    }
    doc.push(b'\n');
    assert_eq!(
        doc.len(),
        SECTION + 9,
        "65520 + 8 bytes of two-byte characters, then a newline"
    );
    // Four bytes in is **two characters**, and there is no reading of this offset under which that is
    // the byte count.
    let caret_at = SECTION + 4;
    assert_eq!(caret_at / 2, 32762, "and 32762 characters precede it");
    let path = dir.join(format!("cc-{}.wavefunction", doc.len()));
    let _ = std::fs::remove_file(&path);
    Wavefunction::create(&path, PASS, "p22", &doc, ITER).expect("create the container");

    let holonomy::store::OpenedDocument {
        editor,
        mut container,
    } = open_document(
        DirectFile::create_or_open(&path).expect("descriptor"),
        PASS,
        ITER,
        DEFAULT_RESIDENT_SECTIONS,
    )
    .expect("open the document");

    let m = ChromeMetrics::DESKTOP;
    // **Built empty and given the document afterwards, exactly as the product does it.**
    let mut s = Session::new(
        Editor::new(),
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );
    let mut store = SectionStore::new(&mut container, DEFAULT_RESIDENT_SECTIONS);
    s.adopt_document(editor, &mut store)
        .expect("adopt the document");
    drop(store);
    let mut store = SectionStore::new(&mut container, DEFAULT_RESIDENT_SECTIONS);

    // **The fixture's own premise, asserted first and with both numbers in it: the caret is 65,524
    // bytes in and 32,762 characters in, and those are not the same number.** If the scan ever
    // returned the byte count the rest of this test would still pass its own assertions, so the
    // premise is a separate assertion rather than a comment.
    const BYTE_COLUMN: u32 = 65_524;
    const CHAR_COLUMN: u32 = 32_762;
    assert_ne!(
        BYTE_COLUMN, CHAR_COLUMN,
        "the two counts must differ for this test to mean anything"
    );
    s.caret_to(0).expect("park at the start");
    s.caret_to(caret_at).expect("caret into section 1");
    assert_eq!(
        s.caret_column(),
        CHAR_COLUMN,
        "**32,762 characters, not 65,524 bytes** -- the whole premise of this test in one assertion"
    );

    // **Now break it on purpose.** Park the caret at the line's start so the stale value is a *specific*
    // wrong number — the fast path sets it to 0 without reading anything — and evict section 0.
    s.caret_to(0).expect("park at the start again");
    assert_eq!(
        s.caret_column(),
        0,
        "column 0, and the fast path read nothing"
    );
    let released = s.editor_mut().evict_range(0, 1).expect("evict section 0");
    assert!(
        released > 0,
        "**the fixture's premise**: evicting one byte at the start released {released} leaves. If this \
         is zero the rope is not holding this document as evictable leaves and the rest of this test \
         is measuring a document that is entirely resident -- which would make every assertion below \
         pass for the wrong reason."
    );

    // **Move into section 1 with section 0 gone.** `caret_to` reads around the destination, which is
    // resident, so it succeeds — and then the column scan walks the prefix from byte 0 and cannot
    // finish.
    s.caret_to(caret_at)
        .expect("the destination leaf is resident, so this moves");
    assert_eq!(s.caret(), caret_at as u32, "the cursor moved");
    assert_eq!(
        s.caret_column(),
        0,
        "**the fallback, deliberately reached**: the line's prefix is not resident, so the scan cannot \
         complete and the column is what the last completed scan said -- which is 0, because parking \
         the caret at the start took the no-read fast path. This is the only window in which it is \
         not a character count, and nothing has been drawn in it."
    );

    // **The paint is authoritative**, and this is the assertion that matters: it has a source, so it
    // faults section 0 back in and counts properly.
    s.repaint_all_with(&mut store)
        .expect("paint through the store");
    assert_eq!(
        s.caret_column(),
        CHAR_COLUMN,
        "after the paint the column is the character count again -- the faulting read brought section \
         0 back and counted it"
    );

    // **No drawn-cell assertion here, and that is deliberate.** This caret is 32,762 characters into a
    // line and the page is 80 columns wide, so `Caret::locate` answers `None` — the caret is scrolled
    // off the right-hand end of a document that has no horizontal scroll in this build. Asserting a
    // cell would mean asserting one the renderer does not draw. **The cell is gated instead on an
    // in-memory document**, in `the_caret_is_drawn_under_the_character_that_ends_there` and
    // `a_click_and_the_drawn_cell_agree_in_both_directions`, where the caret is on screen.

    let _ = std::fs::remove_file(&path);
}

/// **A combining sequence is two scalars at one place, and this counter says so out loud.**
///
/// **`é` as `e` + U+0301 is 1 + 2 bytes and 2 scalars, and the glyph is one.** So the caret after it is
/// reported at column 2 where a reader would call it column 1 — **and this is not fixed**, because the
/// fix is a display-width table and the caret's x would then need the same table. Asserting the wrong
/// number here would enshrine it; asserting nothing would let someone find it by accident instead.
///
/// **So the gate documents the limitation and asserts that it is exactly the limitation claimed**: two
/// scalars, not one, and no more.
#[test]
fn a_combining_sequence_counts_two_scalars_and_documents_the_limit() {
    let mut s = session("e\u{301}\n");
    s.caret_to(3).expect("after both scalars");
    assert_eq!(s.caret(), 3, "three bytes");
    assert_eq!(
        s.caret_column(),
        2,
        "**two** scalars — one glyph, two cells in the current model, which is the recorded limit. \
         A width table is what makes this 1, and nothing here pretends otherwise."
    );
    // A precomposed `é` is one scalar, and the same visual position reads 1. **The two spellings of the
    // same character get different columns**, which is the sharpest statement of what is missing.
    let mut pre = session("\u{e9}\n");
    pre.caret_to(2).expect("after the precomposed form");
    assert_eq!(pre.caret_column(), 1, "one scalar for the same glyph");
    assert_ne!(
        s.caret_column(),
        pre.caret_column(),
        "the same character, spelled two ways, lands in two different columns -- this is the \
         limitation stated as a failing-shaped fact rather than left to be discovered"
    );
}

/// **A session opened on a document reconciles the chrome's caret with the editor's.**
///
/// **This is a real desync that part 22 found by accident, and it had been there since the session
/// existed.** `Session::new` built `ChromeState` with `..ChromeState::default()`, so `caret_line` and
/// `caret_column` were 0 — while `editor.caret()` is wherever the document builder left it, which for
/// every gate that seeds with `insert_at` is the end of the document.
///
/// **It was invisible for twelve phases because both halves of the arithmetic were zero.** A byte
/// count of 0 times `cell_w` is 0, which is a perfectly sensible column for column 0, and the caret was
/// drawn at the top-left of a document whose model said the end of the last line. It surfaced the
/// moment the column started being computed from the actual caret: `a_space_advances_the_pen_rather_
/// _than_stacking_glyphs` failed by exactly one cell, because `"a a"` drew a caret one cell further
/// right than `"aa"`. **A gate measuring ink extent found a caret it had never been able to see.**
#[test]
fn a_session_opened_on_a_document_reconciles_its_caret() {
    let s = session("alpha\nbravo\ncharlie\n");
    // **Line 3, column 0 — and not line 2, column 7.** The builder's `insert_at` leaves the caret at the
    // end of the *document*, and this document ends with a newline, so the caret is on the line after
    // `charlie`. Asserting line 2 is the mistake of reading the fixture as "the caret is after the last
    // word" rather than "the caret is at `text_len`", which is what `insert_at` does.
    assert_eq!(
        (s.caret_line(), s.caret_column()),
        (3, 0),
        "the chrome agrees with the editor, and it is line 3 -- not line 0"
    );
    // **20 bytes, not 23.** `alpha\n` + `bravo\n` + `charlie\n` is 6 + 6 + 8, and the caret is at
    // `text_len` because that is what `insert_at` leaves behind. Getting this wrong is what made an
    // earlier version of this test fail on a byte count rather than on the thing it is about.
    assert_eq!(s.caret(), 20, "and that is byte 20 of a 20-byte document");

    // **The drawn cell is on row 3, which is the part that was wrong.** Before part 22 this was row 0
    // column 0: the chrome drew the caret at the top-left of a document whose model said the very end.
    //
    // **After a paint, and the "after a paint" is load-bearing.** `ChromeState::line_heights` is a
    // model the chrome owns, and `publish_line_heights` fills it — so an *unpainted* session has every
    // row at y = 0 and `Caret::locate` places the caret on the first row whatever `caret_line` says.
    // That is a consequence of where the model lives, not a bug in this part, and asserting the row on
    // an unpainted session would assert a lie.
    let mut s = s;
    s.repaint_all().expect("paint");
    let l = s.chrome_layout();
    let cell_h = s.chrome.metrics.cell_h;
    let cell = Caret::locate(&l, &s.chrome.metrics, &s.state).expect("the caret is on screen");
    assert_eq!(cell.cell.x, l.text.x, "column 0, so the left edge");
    assert_eq!(
        cell.cell.y,
        l.text.y + 3 * cell_h,
        "**row 3, not row 0** -- this is the assertion the whole test exists for"
    );

    // **A document with no trailing newline puts the caret after the last character**, which is the
    // other shape, and there the column is non-zero for the first time.
    let t = session("alpha\nbravo\ncharlie");
    assert_eq!(
        (t.caret_line(), t.caret_column()),
        (2, 7),
        "seven characters in, and seven cells"
    );
    assert_eq!(
        caret_x(&t),
        l.text.x + 7 * s.chrome.metrics.cell_w,
        "which is what `charlie` is"
    );

    // **An empty document still agrees with itself**, which is the case `Default` was right for.
    let empty = session("");
    assert_eq!((empty.caret_line(), empty.caret_column()), (0, 0));
}
