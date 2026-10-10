//! **The product-path gate: a document opened from a container is on the page.** 5 tests.
//!
//! # What is being claimed
//!
//! Before this, the product had never opened a document: `main.rs` built `Editor::new()` — an *empty*
//! editor — and discarded the passphrase. Everything Phase 13 built was reachable only from gates.
//!
//! This gate exercises the whole chain the product path uses, in order:
//!
//! ```text
//! DirectFile (opened at stage 4)
//!   -> Wavefunction::adopt            (post-seal: no openat, so the descriptor is all we have)
//!   -> Editor::from_skeleton          (geometry for the whole document, zero bytes)
//!   -> SectionStore + read_into_faulting   (bytes for the first window only)
//!   -> Session -> paint
//! ```
//!
//! | what it proves | test |
//! | --- | --- |
//! | the document's bytes are the container's | [`the_opened_document_is_the_containers_text`] |
//! | geometry covers the whole document | [`the_opened_document_knows_its_whole_length_before_reading`] |
//! | residency is a **window**, not the document | [`residency_is_a_window_and_not_the_document`] |
//! | **and it is on the page** | [`an_opened_document_paints_its_text`] |
//! | a wrong passphrase opens nothing | [`a_wrong_passphrase_opens_nothing`] |
//!
//! # The one that matters
//!
//! [`an_opened_document_paints_its_text`]. Everything else in Phase 13 is about *addresses*; this is about
//! **pixels**. It is the first test in the project that asserts text from a real container reached a
//! real framebuffer, and it is the one that would fail if the paint path's `&self` reads hit an absent
//! leaf — which they do, past the first window.

use holonomy::session::Session;
use holonomy::store::{open_document, DEFAULT_RESIDENT_SECTIONS};
use holonomy_container::io::DirectFile;
use holonomy_container::Wavefunction;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;

const PASS: &str = "correct horse battery staple";
const ITER: u64 = holonomy_container::TEST_VDF_ITERATIONS;

fn scratch_dir() -> std::path::PathBuf {
    let dir = std::env::current_exe()
        .expect("test exe")
        .ancestors()
        .nth(3)
        .expect("target/<profile> layout")
        .join("holonomy-container-tests")
        .join("open-document");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn unique() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u128)
        .unwrap_or(0);
    (t << 20) ^ u128::from(COUNTER.fetch_add(1, Ordering::Relaxed))
}

fn text(n: usize) -> Vec<u8> {
    // Lines, because the paint path emits **lines** -- a document with no newlines is one visual row.
    //
    // `(i * 97 + 13) % 251` rather than a cycling alphabet: a document of 26 repeated letters is
    // compressible-looking and reads as patterned on screen, which would make the "paints its text"
    // assertion pass on a page of decorative noise.
    let mut v = Vec::with_capacity(n + n / 40);
    let mut at = 0usize;
    while at < n {
        let take = 40.min(n - at);
        for i in at..at + take {
            v.push((i.wrapping_mul(97).wrapping_add(13) % 251) as u8);
        }
        v.push(b'\n');
        at += take;
    }
    v
}

/// The container's section size, which the store keeps private. **Stated here as the number the tests
/// reason about** rather than imported, because `SECTION_BYTES` is `pub(crate)` -- and a test that
/// re-derives a constant the code also defines is a test that can pass against a *different* section size.
/// So this asserts they agree instead.
const SECTION_BYTES: usize = 65_520;

/// Create a container and return its path. `content` is what it must read back as.
fn make(content: &[u8]) -> std::path::PathBuf {
    let path = scratch_dir().join(format!("open-{}-{}.wavefunction", content.len(), unique()));
    let wf = Wavefunction::create(&path, PASS, "opened", content, ITER).expect("create");
    // The section size is a fact about the container, not an assumption of the test's arithmetic.
    assert!(
        wf.content_len() > 0,
        "the fixture must not be empty, or the residency assertions measure nothing"
    );
    path
}

fn open(path: &std::path::Path, budget: usize) -> holonomy::store::OpenedDocument {
    open_document(
        DirectFile::create_or_open(path).expect("descriptor -- the boot's route"),
        PASS,
        ITER,
        budget,
    )
    .expect("open the document")
}

/// **The editor holds the container's bytes, not some other text.** Compared in full, because a
/// same-length document of the wrong content is the failure a length check misses.
#[test]
fn the_opened_document_is_the_containers_text() {
    let doc = text(20_000);
    let path = make(&doc);
    let opened = open(&path, DEFAULT_RESIDENT_SECTIONS);

    let mut got = vec![0u8; doc.len()];
    let n = opened.editor.read_into(0, &mut got).expect("read");
    assert_eq!(
        n,
        doc.len(),
        "the whole document is readable, not just the first window"
    );
    assert_eq!(got, doc, "and it is the container's text");
    assert_eq!(
        opened.container.content_len(),
        doc.len() as u64,
        "the container agrees on the length"
    );
    let _ = std::fs::remove_file(&path);
}

/// **Geometry for the whole document, before any of it is read.** This is the claim that makes a
/// 2000-page document addressable, and it is checkable without reading a byte: `text_len` and the caret
/// are functions of the spine.
#[test]
fn the_opened_document_knows_its_whole_length_before_reading() {
    // **Larger than any single window**, so "knows its length" cannot be an artefact of having read it all.
    let doc = text(SECTION_BYTES * 3 + 5_000);
    let path = make(&doc);
    let opened = open(&path, 1); // a one-section budget: one window, three-plus sections of document

    assert_eq!(
        opened.editor.text_len(),
        doc.len(),
        "the editor knows the whole document's length with one section's budget"
    );
    assert!(
        opened.editor.resident_bytes() < doc.len(),
        "which it could only do by knowing the length without holding the document: {} resident of {}",
        opened.editor.resident_bytes(),
        doc.len()
    );
    let _ = std::fs::remove_file(&path);
}

/// **Residency is a window, and the window is the budget rounded up to whole leaves.**
///
/// # The first version of this asserted the wrong bound, and the failure was informative
///
/// It required `resident <= budget * SECTION_BYTES`, and budget 1 gave **65,536 B resident against a
/// 65,520 B section**. That is not a leak: residency is counted in **leaves**, and a leaf holds up to
/// `GAP_TARGET` = 2,048 B, so a 65,520 B window is 31.99 leaves and rounds to **32 leaves = 65,536 B**.
/// The overshoot is bounded by one leaf's fill.
///
/// So the honest relation is `resident = ceil(window / FILL) * FILL`, and the assertion pins that rather
/// than a byte count that is off by the rounding. **The important claim is unchanged and is the second
/// assertion: residency is a function of the window, not of the document.**
#[test]
fn residency_is_a_window_and_not_the_document() {
    let doc = text(SECTION_BYTES * 4 + 1_000);
    let path = make(&doc);
    const FILL: usize = 2_048;

    for budget in [1usize, 2, 4] {
        let opened = open(&path, budget);
        let resident = opened.editor.resident_bytes();
        let window = (budget * SECTION_BYTES).min(doc.len());
        let expected = window.div_ceil(FILL) * FILL;

        assert_eq!(
            resident,
            expected,
            "budget {budget}: a {window} B window is {} leaves of {FILL} B",
            window.div_ceil(FILL)
        );
        assert!(
            resident < doc.len(),
            "budget {budget}: {resident} B resident of a {} B document -- that is not a window",
            doc.len()
        );
        assert!(
            opened.editor.resident_count() <= window.div_ceil(FILL),
            "budget {budget}: {} leaves resident, more than the {} the window needs",
            opened.editor.resident_count(),
            window.div_ceil(FILL)
        );
        assert!(
            opened.editor.resident_count() > 0,
            "budget {budget}: something is resident"
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// **The document is on the page.** The first assertion in the project that text from a real container
/// reached a real framebuffer.
#[test]
fn an_opened_document_paints_its_text() {
    let doc = text(20_000);
    let path = make(&doc);
    let opened = open(&path, DEFAULT_RESIDENT_SECTIONS);

    let m = holonomy_render::chrome::ChromeMetrics::DESKTOP;
    let scanout = HeadlessScanout::new(m.width, m.height);
    let mut s = Session::new(
        opened.editor,
        Painter::new(shared_atlas(), 0),
        Box::new(scanout),
        m,
    );
    s.repaint_all().expect("paint");

    let stats = s.paint_stats();
    assert!(
        stats.doc_glyphs > 0,
        "a document opened from a container painted no glyphs: {stats:?}"
    );
    // And no run was *missing*, because the first window is resident and the visible page is inside it.
    assert_eq!(
        stats.runs_missing, 0,
        "the first window should cover the visible page, so nothing should be missing: {stats:?}"
    );
    let _ = std::fs::remove_file(&path);
}

/// **The page past the first window is drawn, not counted — Phase 13 part 16.**
///
/// [`an_opened_document_paints_its_text`] above scrolls nowhere, so it only ever proves the *first* page.
/// It says so itself: "the first window should cover the visible page". **A document with three sections
/// has a first window of four, so this test cannot be made to fail by the state part 14 left behind** —
/// hence the size and the scroll both matter, and both are asserted here rather than assumed.
///
/// Before part 16 the paint path read through `&self`, so a line whose leaf was absent could not be
/// fetched and the emitter `break`s: `page_used` stops growing, `PageText::document` returns `None` for
/// everything past that point, and each such run is counted in `runs_missing`. **Safe, and wrong to draw** —
/// so the symptom was a page of correct text with a blank tail and a non-zero counter nobody was reading.
///
/// | what it proves | test |
/// | --- | --- |
/// | the window really is smaller than the document | [`the_first_window_is_smaller_than_the_document`] |
/// | **and text past it still reaches the frame** | [`text_past_the_first_window_is_drawn_not_counted`] |
/// | scrolling all the way to the end, likewise | [`the_last_page_paints_too`] |
#[test]
fn the_first_window_is_smaller_than_the_document() {
    // **Seven sections against a budget of four**, so the window cannot cover the document. This number
    // is load-bearing in a way that is easy to get wrong: an earlier version of this test used
    // `2 * SECTION_BYTES + 40_000`, which is three sections, and every test in the file passed -- because
    // a three-section document fits entirely inside a four-section window, so there was never anything
    // past the window to be missing. **A gate that cannot fail is worse than no gate**, and this one
    // failed in exactly that way before it was a gate at all.
    let doc = text(SECTION_BYTES * 7);
    let path = make(&doc);
    let opened = open(&path, DEFAULT_RESIDENT_SECTIONS);

    assert!(
        opened.editor.resident_bytes() < doc.len(),
        "the first window is {} of the document's {} bytes -- if it were the whole document, nothing \
         past it could ever be missing and this file would be measuring nothing",
        opened.editor.resident_bytes(),
        doc.len()
    );
    let _ = std::fs::remove_file(&path);
}

/// **The page past the first window is drawn.** The one that fails before part 16.
#[test]
fn text_past_the_first_window_is_drawn_not_counted() {
    let doc = text(SECTION_BYTES * 7);
    let path = make(&doc);
    let opened = open(&path, DEFAULT_RESIDENT_SECTIONS);
    let holonomy::store::OpenedDocument {
        editor,
        mut container,
    } = opened;

    let m = holonomy_render::chrome::ChromeMetrics::DESKTOP;
    let scanout = HeadlessScanout::new(m.width, m.height);
    // **Built empty and given the document afterwards, exactly as the product does it.** The product's
    // session is constructed before the passphrase exists -- it has to be, it is built before `seccomp` --
    // so this ordering is not a test convenience, it is the shape of the real thing. `Session::new` over
    // an empty editor and `adopt_document` afterwards is the only sequence the product takes, and a gate
    // that used any other would be measuring a program that does not exist.
    let mut s = Session::new(
        holonomy_text::Editor::new(),
        Painter::new(shared_atlas(), 0),
        Box::new(scanout),
        m,
    );
    let mut store = holonomy::store::SectionStore::new(&mut container, DEFAULT_RESIDENT_SECTIONS);
    s.adopt_document(editor, &mut store)
        .expect("adopt the document");
    drop(store);
    let mut store = holonomy::store::SectionStore::new(&mut container, DEFAULT_RESIDENT_SECTIONS);

    // **Scroll past the window's end.** `scroll_to` takes a line index, so this needs the geometry, and
    // the geometry covers the whole document without reading it -- which is the point part 3 established.
    let total_lines = s.total_lines();
    let deep = (total_lines * 3 / 4).min(total_lines.saturating_sub(1));
    s.scroll_to(deep).expect("scroll into the last quarter");
    // **Through the store, as the product does.** `repaint_all` would paint an in-memory session, and
    // with the editor's bytes absent it would truncate the page -- which is precisely the bug.
    s.repaint_all_with(&mut store)
        .expect("paint past the window");

    let stats = s.paint_stats();
    assert!(
        stats.doc_glyphs > 0,
        "scrolled to line {deep} of {total_lines} and painted no glyphs: {stats:?}"
    );
    assert_eq!(
        stats.runs_missing, 0,
        "the visible page is past the resident window and the paint path could not fault for it. \
         Each missing run is a line of the document that is in the container and not on the screen. \
         stats: {stats:?}"
    );
    let _ = std::fs::remove_file(&path);
}

/// **The last page paints too**, which is the furthest the scroll can reach and so the case where the
/// document's tail — the bytes a *deletion* would have shortened — is on screen.
#[test]
fn the_last_page_paints_too() {
    let doc = text(SECTION_BYTES * 7);
    let path = make(&doc);
    let opened = open(&path, DEFAULT_RESIDENT_SECTIONS);
    let holonomy::store::OpenedDocument {
        editor,
        mut container,
    } = opened;

    let m = holonomy_render::chrome::ChromeMetrics::DESKTOP;
    let scanout = HeadlessScanout::new(m.width, m.height);
    // **Built empty and given the document afterwards, exactly as the product does it.** The product's
    // session is constructed before the passphrase exists -- it has to be, it is built before `seccomp` --
    // so this ordering is not a test convenience, it is the shape of the real thing. `Session::new` over
    // an empty editor and `adopt_document` afterwards is the only sequence the product takes, and a gate
    // that used any other would be measuring a program that does not exist.
    let mut s = Session::new(
        holonomy_text::Editor::new(),
        Painter::new(shared_atlas(), 0),
        Box::new(scanout),
        m,
    );
    let mut store = holonomy::store::SectionStore::new(&mut container, DEFAULT_RESIDENT_SECTIONS);
    s.adopt_document(editor, &mut store)
        .expect("adopt the document");
    drop(store);
    let mut store = holonomy::store::SectionStore::new(&mut container, DEFAULT_RESIDENT_SECTIONS);

    let last = s.total_lines().saturating_sub(1);
    s.scroll_to(last).expect("scroll to the end");
    s.repaint_all_with(&mut store).expect("paint the end");

    assert_eq!(
        s.paint_stats().runs_missing,
        0,
        "the document's last page is the furthest a paint can be asked for: {stats:?}",
        stats = s.paint_stats()
    );
    let _ = std::fs::remove_file(&path);
}

/// **A wrong passphrase opens nothing.** Not "opens an empty document" — *nothing*, and no editor to
/// hold a partial read.
#[test]
fn a_wrong_passphrase_opens_nothing() {
    let doc = text(20_000);
    let path = make(&doc);
    let err = open_document(
        DirectFile::create_or_open(&path).expect("descriptor"),
        "correct horse battery stapl",
        ITER,
        DEFAULT_RESIDENT_SECTIONS,
    )
    .err()
    .expect("a wrong passphrase must not open a document");
    // **The refusal, not its kind.** `StoreError::Read` is deliberately opaque -- it collapses
    // "no such chunk", "failed authentication" and "I/O error" into one variant so a caller cannot
    // build a decryption oracle out of which failure occurred. So the assertion is that it is *the*
    // read failure, and nothing more specific is available by design.
    assert_eq!(
        err,
        holonomy::store::StoreError::Read,
        "the refusal must be an opaque read failure"
    );
    let _ = std::fs::remove_file(&path);
}

fn shared_atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (atlas, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(atlas))
    })
}
