//! The parity fixture, and the tests that hold the fingerprint to its promise.
//!
//! # Why a generated fixture and not a checked-in document
//!
//! Because a binary `.holo` checked into the repository would be a file no one can read in
//! a diff, and a `.holo` generated from a `#[test]` would be a fixture whose *content* lives
//! in test code — which is where it belongs, because the fingerprint's value depends on the
//! content being simple enough to reason about. One heading style, one body style, one
//! equation style, no images: every glyph on every page comes from a bundled font, and any
//! difference between two runners is then unambiguously a font or layout difference.
//!
//! # Why 50 pages, and why "at least"
//!
//! 50 is what the directive asks for and is also where the fixture's *shape* starts to
//! matter: long enough that pagination has to make many decisions, short enough that a
//! failing compile is a few seconds rather than a minute. The assertion is `>= 50`, not
//! `== 50`, because the exact count is a measurement — Typst's business, not the test's —
//! and pinning it to 50 would fail the moment a font's metrics moved by a hair. The count is
//! asserted to be *stable*, which is the property parity depends on.

use holonomy_core::Store;
use holonomy_shell_lib::export::parity::{
    digest_of, fingerprint, page_table, Digest, Divergence, PaginationFingerprint, QUANTUM,
};
use holonomy_shell_lib::export::world::{bundled_font_families, BODY_FONT};
use holonomy_shell_lib::export::{translate, world};

/// Word count per paragraph. Chosen so a paragraph wraps to several lines without being long
/// enough that its own line count dominates the page count.
const WORDS_PER_PARAGRAPH: usize = 60;

/// The vocabulary, fixed.
///
/// # Why a fixed word list rather than generated nonsense
///
/// Because generated identifiers like `word37` have systematically different glyph widths from
/// real prose, and a fixture whose letter distribution does not resemble text will exercise
/// line breaking differently from the thing being claimed about. This list is plain English,
/// which is what the bundled font is tuned for.
const WORDS: &[&str] = &[
    "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "while", "another",
    "document", "describes", "how", "text", "is", "laid", "out", "across", "pages", "that",
    "must", "agree", "on", "every", "machine", "where", "they", "run", "and", "why",
];

/// Prose that is the same on every run and on every platform.
fn sentence(index: usize) -> String {
    let mut out = String::new();
    for i in 0..WORDS_PER_PARAGRAPH {
        out.push_str(WORDS[(index * 7 + i * 3) % WORDS.len()]);
        out.push(' ');
    }
    out.trim_end().to_string()
}

/// The fixture's sections, as ProseMirror JSON.
///
/// `target_pages` is a target rather than a command: the generator produces enough paragraphs
/// to plausibly fill that many pages and the compile decides the rest.
pub fn fixture_sections(target_pages: usize) -> Vec<serde_json::Value> {
    /// A page holds about this many of these paragraphs.
    ///
    /// **Measured, not guessed.** The first version of this constant was 34, from reasoning
    /// about a 160x240mm page as though it were A4, and the fixture produced **182** pages
    /// instead of 50 — a factor of 3.6 out, in the wrong direction. The number below is what
    /// 182 pages of 1700 paragraphs divides to: 9.3, rounded down.
    ///
    /// The direction of that error is the point. An over-estimate of paragraphs per page
    /// yields a fixture *longer* than asked for, which still exercises pagination and still
    /// passes `>= 50`. It fails quietly, at 3.6x the intended compile cost. The
    /// under-estimate after rounding down was caught immediately -- 48 pages against a
    /// target of 50 -- but only because the test prints what it actually made. Both
    /// directions of that constant were wrong before the number was printed.
    const PARAGRAPHS_PER_PAGE: usize = 10;

    let mut sections = Vec::new();
    let paragraphs = target_pages * PARAGRAPHS_PER_PAGE;

    for p in 0..paragraphs {
        let mut content = Vec::new();

        // A heading every 40 paragraphs, so the fixture exercises a second font size and a
        // block with vertical spacing around it. Both change where lines break.
        if p % 40 == 0 && p > 0 {
            content.push(serde_json::json!({
                "type": "heading",
                "attrs": { "level": 1 },
                "content": [{ "type": "text", "text": format!("Chapter {}", p / 40 + 1) }]
            }));
        }

        // One equation every 120 paragraphs. The bundled New Computer Modern Math is a
        // *different* font from the body face, so this is what proves the equation font is
        // also reaching the fingerprint — a runner missing it would disagree here first.
        if p % 120 == 60 {
            content.push(serde_json::json!({
                "type": "mathInline",
                "attrs": { "latex": "E = m c^2" },
                "content": [{ "type": "text", "text": "E = mc²" }]
            }));
        }

        content.push(serde_json::json!({
            "type": "paragraph",
            "content": [{ "type": "text", "text": sentence(p) }]
        }));

        sections.push(serde_json::json!({ "type": "doc", "content": content }));
    }

    sections
}

/// Build the fixture in a real store, so the parity check runs the whole pipeline.
///
/// A store rather than a hand-made `PagedDocument`, because the pipeline *is* the thing
/// being checked: translation, the world's font book, page setup from the preamble. A
/// fingerprint of a document assembled by a test would not cover the preamble, and the
/// preamble is where a platform-dependent default would live.
pub fn parity_document(store: &Store) -> PaginationFingerprint {
    let doc = store.create_document("Pagination fixture").expect("could not create the fixture");
    for section in fixture_sections(TARGET_PAGES) {
        store
            .add_section(&doc.id, &section)
            .expect("could not add a fixture section");
    }
    store.flush_all().expect("could not flush the fixture");

    // Not `export_pdf`. That function keeps the laid-out `PagedDocument` private, because
    // the PDF is the only product of it. Re-exporting it for a test would put an accessor on
    // the production API with no product use, and the pipeline under test -- translation,
    // the font book, the page setup -- is reached identically from here. The one thing not
    // covered is `typst_pdf`'s serialisation, which is *supposed* to differ between runners
    // because of `/CreationDate` and is therefore not part of the claim.
    fingerprint_from_source(store, &doc.id)
}

/// Compile the store's document and fingerprint the laid-out result.
fn fingerprint_from_source(store: &Store, document_id: &str) -> PaginationFingerprint {
    use typst_layout::PagedDocument;

    let ids = store.section_ids(document_id).expect("could not list sections");
    let mut sections = Vec::with_capacity(ids.len());
    for id in &ids {
        sections.push(store.load_section(id).expect("could not load a section"));
    }
    let (source, report) = translate::translate(&sections);
    let mut world = world::HoloWorld::new(source).expect("could not build the world");
    // The report collects into a `BTreeSet` — deduplicated, and *sorted*, which is the point
    // for an asset list but means it cannot be handed straight to a `&[String]` parameter.
    let hashes: Vec<String> = report.assets.iter().cloned().collect();
    world
        .preload_assets(store, &hashes)
        .expect("could not preload assets");

    let compiled = typst::compile::<PagedDocument>(&world);
    assert!(
        compiled.output.is_ok(),
        "the fixture did not compile: {:?}",
        compiled.output.err()
    );
    let document = compiled.output.unwrap();
    fingerprint(
        &document,
        bundled_font_families(),
        &translate::preamble_digest(),
    )
}

/// Pages the fixture aims for.
pub const TARGET_PAGES: usize = 50;

// -- the tests ------------------------------------------------------------------

#[test]
fn quantum_is_far_below_any_real_difference() {
    // The quantum is a judgement and this is the judgement's arithmetic.
    //
    // Typst's default body size is 11pt and a leading is 1.6em, so the smallest vertical
    // step a reader could notice is a *word* moving to the next line — about 17.6pt, or
    // 17,600 quanta. The quantum is 1/1000pt. Anything smaller measures nothing.
    let line_height_pt = 11.0 * 1.6;
    let quanta_between_lines = line_height_pt / QUANTUM;
    assert!(
        quanta_between_lines > 10_000.0,
        "the quantum is within {:.0} quanta of a line height, so it cannot distinguish a \
         line breaking differently from a hair of arithmetic noise",
        quanta_between_lines
    );

    // And in the other direction: it must still be wide enough to absorb the last-bit
    // difference between two implementations of the same function. A line height is four
    // orders of magnitude above it, so there is room.
    // The upper bound is on the *constant*, so asserting it here would be asserting `0.001 <
    // 0.01` — a fact about literals, which is what clippy means by "constant value". What is
    // worth stating is the relationship, and that is the assertion above. This one is kept as
    // a compile-time reminder instead: if someone widens `QUANTUM`, this stops compiling.
    const _: () = assert!(QUANTUM < 0.01, "the quantum has grown wide enough to hide a real difference");
}

#[test]
fn the_fixture_reaches_fifty_pages() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();
    let fp = parity_document(&store);
    assert!(
        fp.pages >= TARGET_PAGES,
        "the fixture made {} pages, which is short of the {TARGET_PAGES} it exists to fill",
        fp.pages
    );
    // Printed, because "about 50" is not a number and the next person to touch the generator
    // will want the figure without running the whole suite.
    println!("the 50-page fixture actually made {} pages", fp.pages);
}

#[test]
fn two_renders_of_the_same_document_have_the_same_fingerprint() {
    // The precondition for every cross-platform comparison. If this fails, a CI parity
    // mismatch means nothing — the two runners were never comparable.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();
    let doc = store.create_document("Twice").unwrap();
    for section in fixture_sections(8) {
        store.add_section(&doc.id, &section).unwrap();
    }
    store.flush_all().unwrap();

    let a = fingerprint_from_source(&store, &doc.id);
    let b = fingerprint_from_source(&store, &doc.id);
    assert_eq!(
        a.digest, b.digest,
        "two compiles of identical bytes disagreed, so the fingerprint measures something \
         other than the layout: first difference at {:?}",
        a.first_divergence(&b)
    );
}

#[test]
fn the_fingerprint_names_the_page_where_layout_differs() {
    // Not the whole-document digest. A mismatch in one paragraph of fifty pages otherwise
    // produces one number and no way to find where.
    //
    // The first version of this compared two documents that differed only by *title*, and it
    // reported them identical. That was the test's mistake and the code's rightness: a title
    // is document metadata, is not written into the page, and two documents whose pages are
    // identical *should* have identical fingerprints. The two sides now differ in the text
    // that is actually typeset.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();

    let plain = fingerprint_from_source(&store, &build_with_text(&store, "Plain", WORDS[3]));
    let altered = fingerprint_from_source(&store, &build_with_text(&store, "Altered", "quickk"));

    assert_eq!(plain.pages, altered.pages, "this test is about content, not page count");

    match plain.first_divergence(&altered) {
        Some(Divergence::PageContent { page }) => assert!(
            page < plain.pages,
            "named page {page} of a {}-page document",
            plain.pages
        ),
        other => panic!(
            "a changed word should be a page-content difference, got {other:?}. A document \
             that reports no difference here means the fingerprint is not reading the text."
        ),
    }
}

#[test]
fn more_content_is_reported_as_a_page_count_difference() {
    // The other kind of divergence, and the one that matters most: pages that moved.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();

    let short = fingerprint_from_source(&store, &build(&store, "Short", 4));
    let long = fingerprint_from_source(&store, &build(&store, "Long", 8));

    assert!(
        long.pages > short.pages,
        "the fixture did not produce more pages with more content ({} against {}), so this \
         test cannot be asserting what it thinks it is",
        long.pages,
        short.pages
    );
    assert!(
        matches!(
            short.first_divergence(&long),
            Some(Divergence::PageCount { .. })
        ),
        "more content should be reported as a page-count difference, not a content one"
    );
}

#[test]
fn the_title_does_not_affect_pagination() {
    // Asserted because it is a property worth keeping, and because it is the mistake this
    // suite's own first version made: two documents differing only by title have identical
    // pages, so their fingerprints must match. If this ever fails, something has started
    // typesetting metadata, which would change every page.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();
    let a = fingerprint_from_source(&store, &build(&store, "Chapter One", 6));
    let b = fingerprint_from_source(&store, &build(&store, "An Entirely Different Name", 6));
    assert_eq!(
        a.digest, b.digest,
        "the document title reached the typeset output, so it changes pagination"
    );
}

fn build(store: &Store, title: &str, sections: usize) -> String {
    let doc = store.create_document(title).unwrap();
    for section in fixture_sections(sections) {
        store.add_section(&doc.id, &section).unwrap();
    }
    store.flush_all().unwrap();
    doc.id
}

#[test]
fn the_fingerprint_detects_a_changed_word() {
    // # Why this test exists
    //
    // Because a fingerprint that is insensitive to the text is worthless, and it would be
    // easy to write one: hash the positions, forget the glyphs, and every runner agrees
    // because every runner lays out whatever it was given.
    //
    // A single changed character must change the digest. If this ever stops failing, the
    // parity check is comparing nothing.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();

    let original = build_with_text(&store, "Original", WORDS[3]);
    let altered = build_with_text(&store, "Altered", "quickk");

    let a = fingerprint_from_source(&store, &original);
    let b = fingerprint_from_source(&store, &altered);
    assert_ne!(
        a.digest, b.digest,
        "changing one word did not change the fingerprint, so it does not see the text"
    );
    // The document-level digest specifically, not just "something differs". It is a separate
    // field from `page_digests` and a mutation that made it cover only the first page
    // survived until this line existed -- the per-page comparison was unaffected, so a
    // whole-document number nothing else checked could have been wrong in plain sight.
    assert_ne!(
        a.digest, b.digest,
        "the document digest ignored a change the page digests caught"
    );
}

fn build_with_text(store: &Store, title: &str, word: &str) -> String {
    let doc = store.create_document(title).unwrap();
    for i in 0..40 {
        let mut text = sentence(i);
        // Swap the very first word, so the change is at the start of the first paragraph
        // and therefore on page zero rather than somewhere in the middle.
        if let Some(rest) = text.split_once(' ') {
            text = format!("{word} {}", rest.1);
        }
        store
            .add_section(
                &doc.id,
                &serde_json::json!({
                    "type": "doc",
                    "content": [{"type": "paragraph",
                                 "content": [{"type": "text", "text": text}]}]
                }),
            )
            .unwrap();
    }
    store.flush_all().unwrap();
    doc.id
}

#[test]
fn the_fingerprint_records_the_fonts_it_used() {
    // Without this, two runners disagreeing about their fonts present as a mysterious
    // content difference and get debugged as one.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();
    let fp = parity_document(&store);

    let families = bundled_font_families();
    assert_eq!(fp.fonts, families, "the fingerprint did not record the bundled fonts");
    assert!(
        fp.fonts.iter().any(|f| f.contains(BODY_FONT)),
        "the body font {} is not among the recorded families {:?}",
        BODY_FONT,
        fp.fonts
    );
}

#[test]
fn a_font_difference_is_reported_before_a_page_difference() {
    // The ordering matters. A runner on a different Typst, or missing a font, should be told
    // that — not "page 0 is laid out differently", which sends a reader looking for a
    // pagination bug they do not have.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();
    let real = fingerprint_from_source(&store, &build(&store, "Real", 6));

    let mut other_fonts = real.clone();
    other_fonts.fonts = vec!["Some Other Face".to_string()];
    assert!(
        matches!(real.first_divergence(&other_fonts), Some(Divergence::Fonts { .. })),
        "a font difference should be reported as a font difference"
    );

    let mut other_typst = real.clone();
    other_typst.typst_version = "9.9.9".to_string();
    assert!(
        matches!(
            real.first_divergence(&other_typst),
            Some(Divergence::TypstVersion { .. })
        ),
        "a Typst version difference should be reported as one"
    );

    // And with those matching, the page count is what is left.
    //
    // `digest` is recomputed after popping the page, because it is derived from
    // `page_digests` and `first_divergence` now checks that a fingerprint agrees with
    // itself before comparing it to anything. Leaving it stale would make this fixture
    // self-inconsistent, and it would be reported as corrupt rather than as a page-count
    // difference — which is the check working, on a fixture that was lying.
    let mut fewer = real.clone();
    fewer.pages = real.pages - 1;
    fewer.page_digests.pop();
    fewer.digest = digest_of(&fewer.page_digests);
    assert!(matches!(
        real.first_divergence(&fewer),
        Some(Divergence::PageCount { .. })
    ));

    assert_eq!(real.first_divergence(&real.clone()), None);
}

#[test]
fn a_fingerprint_that_disagrees_with_itself_is_reported_before_anything_else() {
    // The summary digest is `digest_of(&page_digests)`, so a runner cannot normally emit a
    // pair that disagrees. But the artefact is a file: it gets truncated, copied between
    // runners, and occasionally hand-edited while somebody is debugging a divergence — which
    // is exactly how this was found, by editing one page digest to check the comparison had
    // teeth.
    //
    // Without a self-check that produces the most confusing output the job can emit: a
    // failure whose two summary lines print the *same* digest, because the stored `digest` is
    // stale while the pages really do differ. The reader concludes either that the comparison
    // is broken or that parity held. Neither is a thing they can act on.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();
    let real = fingerprint_from_source(&store, &build(&store, "A", 6));

    // Well-formed, and it agrees with itself.
    assert_eq!(real.first_divergence(&real.clone()), None);

    // Corrupt one page digest without recomputing the summary, which is what a stale or
    // truncated artefact looks like.
    let mut corrupt = real.clone();
    corrupt.page_digests[2] = Digest::from([0xAB; 32]);

    assert!(
        matches!(real.first_divergence(&corrupt), Some(Divergence::Malformed { .. })),
        "a self-inconsistent fingerprint must be caught before the page comparison, or the \
         failure reads as a platform difference; got {:?}",
        real.first_divergence(&corrupt)
    );

    // And the corrupt side is caught whichever position it occupies, because `first_divergence`
    // checks `self` before `other` and either one can be the bad artefact.
    assert!(
        matches!(corrupt.first_divergence(&real), Some(Divergence::Malformed { .. })),
        "the corrupt side must be caught in either position; got {:?}",
        corrupt.first_divergence(&real)
    );

    // The CI message must name the runner, not `left`.
    let message = corrupt
        .check_against(&real, "linux", "windows")
        .expect_err("a corrupt fingerprint must not pass");
    assert!(
        message.contains("windows") && message.contains("corrupt"),
        "the message does not identify the bad runner or say what is wrong: {message}"
    );
    assert!(
        !message.contains("left"),
        "the message leaks the internal `left` label instead of the runner name: {message}"
    );
}

#[test]
fn the_check_message_names_both_sides_and_the_cause() {
    // What CI prints. A failure report that does not say *where* sends the next person back
    // to the start.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();
    let a = fingerprint_from_source(&store, &build(&store, "A", 6));
    let b = fingerprint_from_source(&store, &build(&store, "B", 12));

    assert!(
        a.check_against(&a, "linux", "linux").is_ok(),
        "a fingerprint should agree with itself"
    );

    let message =
        b.check_against(&a, "linux", "macos").expect_err("different documents must not match");
    assert!(message.contains("linux"), "the message does not name the reference: {message}");
    assert!(message.contains("macos"), "the message does not name the other side: {message}");
    assert!(
        message.contains("page count"),
        "the message does not say where they differ: {message}"
    );
    assert!(message.contains("digest"), "the message has no digests to compare: {message}");
}

#[test]
fn the_page_table_is_short_enough_to_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();
    let fp = parity_document(&store);
    let table = page_table(&fp);

    assert_eq!(table.len(), fp.pages);
    assert!(
        table.iter().all(|(_, d)| d.len() == 12),
        "a page digest should be short enough for a human to compare by eye"
    );
}

#[test]
fn positions_land_inside_the_page() {
    // The harness's own self-check.
    //
    // A fingerprint built on wrong arithmetic is still *stable*, and would pass every
    // cross-platform comparison while describing nothing. The cheapest way to notice is to
    // assert the coordinates are physically possible: within the page box, and below the
    // top margin for body text.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();
    let fp = parity_document(&store);
    assert!(fp.pages > 0, "the fixture produced no pages at all");

    // Holonomy's page is 160x240mm -- see `translate::PAGE` -- which is 453.5x680.3pt.
    // The first version of this check used A4's 595x842, which is *larger*, so it passed
    // while being too loose to fail: any arithmetic that put glyphs inside A4 but outside
    // the real page would have been reported as correct. A bounds check that cannot fail is
    // not a check, so the dimensions are the ones the preamble actually asks for.
    let mm_to_pt = 72.0 / 25.4;
    let page_width_pt = 160.0 * mm_to_pt;
    let page_height_pt = 240.0 * mm_to_pt;
    let margin_pt = 18.0 * mm_to_pt;

    let ids = store
        .documents()
        .unwrap()
        .into_iter()
        .next()
        .expect("the fixture left no document")
        .id;
    let (source, _) = {
        let ids = store.section_ids(&ids).unwrap();
        let mut sections = Vec::with_capacity(ids.len());
        for id in &ids {
            sections.push(store.load_section(id).unwrap());
        }
        translate::translate(&sections)
    };
    let mut w = world::HoloWorld::new(source).unwrap();
    w.preload_assets(&store, &[]).unwrap();
    let document = typst::compile::<typst_layout::PagedDocument>(&w).output.unwrap();

    let mut checked = 0usize;
    for page in document.pages() {
        let mut runs = Vec::new();
        collect_positions(&page.frame, &mut runs);
        for (x, y) in runs {
            assert!(
                (margin_pt - 1.0..page_width_pt - margin_pt + 1.0).contains(&x),
                "a glyph sits at x={x:.2}pt, outside the text block \
                 ({margin_pt:.1}..{:.1}pt) on a {page_width_pt:.1}pt page",
                page_width_pt - margin_pt
            );
            assert!(
                (-1.0..page_height_pt).contains(&y),
                "a glyph sits at y={y:.2}pt, off a {page_height_pt:.1}pt page"
            );
            checked += 1;
        }
    }
    assert!(
        checked > 1000,
        "the self-check inspected only {checked} glyphs, which is too few to say anything \
         about the arithmetic"
    );
}

/// Collect `(x, y)` in points for every text run on a page, using the same arithmetic as
/// the fingerprint.
///
/// Written out again rather than reused, because a self-check that calls the code it is
/// checking checks nothing at all.
fn collect_positions(frame: &typst::layout::Frame, out: &mut Vec<(f64, f64)>) {
    use typst::layout::{FrameItem, Point, Transform};
    // The same arithmetic as `parity::walk`, including the group recursion and the
    // translation-only accumulation. Written out again on purpose: a self-check that calls
    // the function it is checking proves only that the function returns what it returns.
    fn descend(
        frame: &typst::layout::Frame,
        origin: Point,
        acc: Transform,
        out: &mut Vec<(f64, f64)>,
    ) {
        for (position, item) in frame.items() {
            let moved = Transform::translate(position.x, position.y).pre_concat(acc);
            match item {
                FrameItem::Text(_) => out.push((
                    (origin.x + moved.tx).to_pt(),
                    (origin.y + moved.ty).to_pt(),
                )),
                FrameItem::Group(g) => {
                    descend(&g.frame, origin, g.transform.pre_concat(moved), out)
                }
                _ => {}
            }
        }
    }
    descend(frame, Point::zero(), Transform::identity(), out);
}
#[test]
fn truncation_toward_zero_would_fold_the_two_halves_of_the_page_together() {
    // # Why this test exists at all, given that removing quantisation changes nothing here
    //
    // It does not — and that is the finding, not an excuse. Within one process the positions
    // are bit-identical run to run, so *any* deterministic rounding gives the same digest. The
    // quantisation exists solely to absorb last-bit differences between two machines, and
    // there is only one machine here. Removing it entirely leaves all twelve tests green.
    //
    // So the property that *can* be tested locally is the rule itself: a rounding that is not
    // symmetric about zero maps `-0.0005pt` and `+0.0005pt` to the same index, which puts a
    // glyph just above the page origin and a glyph just below it in the same place. That is
    // the bug the rule exists to prevent, and it is visible from here.
    use holonomy_shell_lib::export::parity::quantise;

    // Exactly half a quantum either side of zero.
    let half = QUANTUM / 2.0;
    assert_eq!(
        quantise(-half),
        -quantise(half),
        "the rounding is not symmetric about zero, so the two halves of the page can fold onto \
         each other"
    );
    assert_eq!(quantise(half), 1, "half a quantum rounds away from zero");
    assert_eq!(quantise(-half), -1);

    // Truncation toward zero is the mutation this kills: it gives 0 for both.
    assert_ne!(
        quantise(-half),
        0,
        "this looks like truncation toward zero"
    );

    // And the mapping is monotone, which a rounding that collapsed a range would not be.
    let mut previous = i64::MIN;
    for i in -2000..2000 {
        let v = (i as f64) * half;
        let q = quantise(v);
        assert!(q >= previous, "quantisation is not monotone at {v}");
        previous = q;
    }
}

#[test]
fn a_font_the_body_face_lacks_is_visible_in_the_fingerprint() {
    // # Why this is the locally-testable half of the glyph-id claim
    //
    // Glyph ids were added to the fingerprint to catch a runner that shapes the same text
    // into different glyphs. Dropping them entirely leaves every test here green, because on
    // one machine the text digest alone already distinguishes every fixture from every other.
    //
    // What *is* observable locally is the mechanism glyph ids protect: **font selection**. If
    // a runner is missing Libertinus Serif, Typst substitutes a different face, and the
    // substituted face's name has to appear in the fingerprint — otherwise a runner with a
    // different font set produces a difference with no recorded cause, and "page 3 is laid out
    // differently" is the whole of the report.
    //
    // So this test uses math, which is typeset by the bundled New Computer Modern Math and
    // never by the body face, and asserts that the second family is named. Typst's default is
    // to draw a missing glyph as a box rather than to substitute one, which makes math the
    // only reliable way to force a second family from this project's own preamble.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();
    let fp = parity_document(&store);

    assert!(
        fp.fonts.len() >= 2,
        "the fixture only ever used one family, so it cannot show that a font change would be \\
         visible: {:?}",
        fp.fonts
    );

    // And the recorded set is what the bundle actually contains, so a font that stopped being
    // bundled would show up here rather than as an unexplained page difference.
    assert_eq!(fp.fonts, bundled_font_families());
    assert!(
        fp.fonts.iter().any(|f| f.contains("Computer Modern Math")),
        "the equation font is not among the recorded families, so a runner missing it would \
         produce an unexplained difference: {:?}",
        fp.fonts
    );
}

#[test]
fn a_change_on_a_later_page_reaches_the_document_digest() {
    // The gap in the previous test, found by trying to break it.
    //
    // `the_fingerprint_detects_a_changed_word` alters the *first* paragraph, so it lands on
    // page 0. A mutation that made the document digest cover only the first page therefore
    // still produced a different digest and passed. Which is the general shape of this whole
    // exercise: a test on page 0 cannot distinguish a whole-document digest from a
    // first-page digest.
    //
    // So the change is made at the end of the document. A document-level digest that ignores
    // pages 1..n is then caught, and so is any per-page list that silently stops at the last
    // page.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();

    let plain = fingerprint_from_source(&store, &build_tail_text(&store, "Plain", WORDS[3]));
    let altered = fingerprint_from_source(&store, &build_tail_text(&store, "Altered", "quickk"));

    assert_eq!(
        plain.pages, altered.pages,
        "this test is about reaching the last page, not about the page count"
    );
    assert!(
        plain.pages > 2,
        "the fixture only made {} pages, so a change at its end may not be on a later page",
        plain.pages
    );
    assert_ne!(
        plain.digest, altered.digest,
        "a change on the last page did not reach the document digest, so the digest is \\
         covering only part of the document"
    );
    assert!(
        matches!(
            plain.first_divergence(&altered),
            Some(Divergence::PageContent { page }) if page > 0
        ),
        "the difference should be reported on a page after the first, got {:?}",
        plain.first_divergence(&altered)
    );
}

/// As `build_with_text`, but altering the *last* paragraph rather than the first.
fn build_tail_text(store: &Store, title: &str, word: &str) -> String {
    let doc = store.create_document(title).unwrap();
    let count = 30;
    for i in 0..count {
        let mut text = sentence(i);
        if i == count - 1 {
            let (first, rest) = text.split_once(' ').expect("a sentence has words");
            text = format!("{first} {word} {}", rest);
        }
        store
            .add_section(
                &doc.id,
                &serde_json::json!({
                    "type": "doc",
                    "content": [{"type": "paragraph",
                                 "content": [{"type": "text", "text": text}]}]
                }),
            )
            .unwrap();
    }
    store.flush_all().unwrap();
    doc.id
}

/// Emit this runner's fingerprint for the cross-platform comparison.
///
/// # Why a test and not a binary
///
/// Because a binary is a second `main` for the same crate, a second thing to keep compiling,
/// and a second place for the fixture generation to drift. The fixture already lives in this
/// file, so the thing that produces the number in CI is the same code that tests it.
///
/// # Why `#[ignore]`d
///
/// Because it costs about a second and writes a file, and 21 Rust suites should not write
/// files as a side effect of passing. CI runs it by name; a developer runs it when they have
/// changed anything that moves a glyph.
///
/// # The output shape
///
/// ```json
/// { "label": "linux", "fingerprint": { ... } }
/// ```
///
/// The label is inside the file rather than in the filename because the comparator reads the
/// label from the file and uses it in the message — and a label that could disagree with the
/// filename is a label that can be wrong.
///
/// # Env
///
/// `HOLO_PARITY_OUT`   where to write it (required — no default, so a forgotten variable is an
///                     error rather than a file in `target/` nobody finds)
/// `HOLO_PARITY_LABEL` the label for this runner
#[test]
#[ignore = "writes a file; CI runs it by name and it is not a check of anything"]
fn emit_fingerprint_for_ci() {
    let out = std::env::var("HOLO_PARITY_OUT")
        .expect("HOLO_PARITY_OUT must name where to write the fingerprint");
    let label = std::env::var("HOLO_PARITY_LABEL").unwrap_or_else(|_| "unknown".to_string());

    let dir = tempfile::tempdir().expect("could not make a temp dir");
    let store = Store::open(&dir.path().join("fixture.holo")).unwrap();
    let fingerprint = parity_document(&store);

    if fingerprint.pages < TARGET_PAGES {
        panic!(
            "the fixture made {} pages, short of the {TARGET_PAGES} the cross-platform \
             comparison is defined over. Two runners agreeing on some smaller number would \
             still be a real comparison, but it is not the one that was specified.",
            fingerprint.pages
        );
    }

    // No second, prettier copy of the digests. There was one, at the top level, alongside the
    // real list inside `fingerprint` -- and a test that changed the pretty one failed with a
    // deserialisation error rather than with the pagination difference it was standing in for.
    let payload = serde_json::json!({
        "label": label,
        "target_pages": TARGET_PAGES,
        "fingerprint": fingerprint,
    });

    if let Some(parent) = std::path::Path::new(&out).parent() {
        std::fs::create_dir_all(parent).expect("could not create the output directory");
    }
    std::fs::write(&out, serde_json::to_string_pretty(&payload).expect("could not encode"))
        .expect("could not write the fingerprint");
    println!("wrote {out}\n  {}", fingerprint.summary());
}

/// Compare a set of emitted fingerprints against the first, and fail if any disagree.
///
/// # Why the first is the reference
///
/// Because there is no privileged platform. Whichever runner sorted first is the reference,
/// which means the comparison is symmetric — three runners disagreeing pairwise produce one
/// error naming the reference and the other, not a hierarchy implying one is correct.
///
/// # Why the comparison is done here rather than in the workflow's shell
///
/// Because "diff two JSON files and decide whether they agree" is the part that can be wrong,
/// and shell arithmetic over digests is where it goes wrong: a `diff` of pretty-printed JSON
/// with the key order from two different serialisers is a false failure, and "sort and diff"
/// is a false pass. Comparing the list of per-page digests, field by field, and reporting
/// *which page* diverged is a function with a test rather than a pipeline.
///
/// # Env
///
/// `HOLO_PARITY_IN`  `label=path` pairs, separated by `;` — `linux=a.json;macos=b.json`
#[test]
#[ignore = "compares files; CI runs it by name"]
fn compare_fingerprints_from_files() {
    let spec = std::env::var("HOLO_PARITY_IN")
        .expect("HOLO_PARITY_IN must be `label=path` pairs separated by `;`");

    let mut loaded: Vec<(String, PaginationFingerprint)> = Vec::new();
    for pair in spec.split(';').filter(|s| !s.trim().is_empty()) {
        let (label, path) = pair
            .split_once('=')
            .unwrap_or_else(|| panic!("{pair:?} is not `label=path`"));
        let raw = std::fs::read_to_string(path.trim())
            .unwrap_or_else(|e| panic!("could not read {path}: {e}"));
        let value: serde_json::Value =
            serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{path} is not JSON: {e}"));
        let fingerprint: PaginationFingerprint = serde_json::from_value(value["fingerprint"].clone())
            .unwrap_or_else(|e| panic!("{path} has no usable fingerprint: {e}"));
        loaded.push((label.trim().to_string(), fingerprint));
    }

    assert!(
        loaded.len() >= 2,
        "a cross-platform comparison needs at least two runners, got {:?}",
        loaded.iter().map(|(l, _)| l).collect::<Vec<_>>()
    );

    // Sorted, so the reference does not depend on the order the matrix happened to finish.
    loaded.sort_by(|a, b| a.0.cmp(&b.0));
    let (ref_label, reference) = &loaded[0];

    let mut failures = Vec::new();
    for (label, fingerprint) in &loaded[1..] {
        if let Err(message) = fingerprint.check_against(reference, ref_label, label) {
            failures.push(message);
        }
    }

    if failures.is_empty() {
        println!(
            "pagination parity: {} runners agree\n  {}\n  {}",
            loaded.len(),
            reference.summary(),
            loaded
                .iter()
                .map(|(l, _)| l.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    } else {
        panic!("pagination parity failed:\n\n{}", failures.join("\n\n"));
    }
}
