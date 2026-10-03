//! Font determinism: the promise that an exported document paginates the same everywhere.
//!
//! # What the promise is
//!
//! Two documents with the same bytes, exported on Linux, macOS and Windows, produce the same
//! pagination. For that, one thing is enough and one thing is not.
//!
//! Enough: the family the generated preamble names must resolve to the same bytes everywhere.
//!
//! Not enough: the bundle being consulted first, with system fonts appended after it. That was
//! the previous design, and it held the first half while leaving the second open — a document
//! naming a family the bundle *lacks* resolved to whatever the exporting machine had, so the
//! page count followed the machine's software inventory. The parity job cannot diagnose that,
//! because the digests simply differ.
//!
//! # What is guaranteed here
//!
//! Every family in the book comes from a pinned crate whose bytes are `include_bytes!`-ed into
//! the binary. Nothing is read from `/usr/share/fonts`, `/Library/Fonts` or `C:\\Windows\\Fonts`,
//! so two machines with different software installed produce the same pages for the same
//! document. A document naming a family the bundle lacks is typeset in the bundled default and
//! gets a warning that names the substitution.
//!
//! # What it costs
//!
//! A document cannot be set in a face neither machine has. The previous design's argument — "a
//! word processor that cannot use a font the user installed is not a word processor" — was not
//! wrong about word processors; it was answering a different question than the one pagination
//! asks. It is the trade, stated here so it is not discovered later, and
//! `a_document_naming_a_font_the_bundle_lacks_warns_and_still_exports` is what keeps the
//! substitution visible rather than silent.
//!
//! # What no test here reads
//!
//! The system. A test that reads the installed fonts to decide what to expect is a test about
//! the machine it ran on, and this suite is meant to give the same answer on a runner with six
//! fonts and on a developer box with six hundred.

use holonomy_core::Store;
use holonomy_shell_lib::export::pdf::export_pdf_quiet;
use holonomy_shell_lib::export::translate::{self, BODY_FONT};
use holonomy_shell_lib::export::world;
use serde_json::json;

#[test]
fn the_body_font_is_one_of_the_bundled_families() {
    // The single assertion the whole guarantee rests on. If the preamble named a family the
    // bundle does not provide, every document would fall back and every warning would be the
    // only evidence -- which is what happened before the bundle existed, and what the earlier
    // round recorded as "the PDF's appearance depends on which Holonomy build produced it".
    let families = world::bundled_font_families();
    assert!(
        families.iter().any(|f| f == BODY_FONT),
        "the preamble names {BODY_FONT:?}, which the bundle does not provide. Bundled: {families:?}. \
         A document naming a font the bundle lacks typesets in a machine-dependent fallback, \
         which is the property this is supposed to prevent."
    );
}

#[test]
fn the_two_font_constants_are_the_same_string() {
    // `world::BODY_FONT` and `translate::BODY_FONT` are the same value by construction today, and
    // this is what stops them from diverging tomorrow. Two matching literals in two files is a
    // thing that quietly stops being true, and the failure is a warning in a log nobody reads.
    assert_eq!(world::BODY_FONT, translate::BODY_FONT);
}

#[test]
fn the_generated_preamble_names_the_body_font() {
    let (source, _) = translate::translate(&[json!({"type":"doc","content":[]})]);
    assert!(
        source.contains(&format!("font: \"{BODY_FONT}\"")),
        "the preamble should set the body font; got:\n{}",
        source.lines().take(6).collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn the_bundle_is_not_empty_and_covers_the_math_glyphs() {
    // An empty book is the silent failure this exists to catch: every glyph becomes a fallback
    // box and the export still succeeds, so nothing but this check would ever notice.
    let families = world::bundled_font_families();
    assert!(
        families.len() >= 2,
        "the bundle should carry at least a text family and a math family; got {families:?}"
    );
    // A document with equations is ordinary, not specialist, and `integral_0^1` needs a glyph
    // the text face does not have.
    assert!(
        families.iter().any(|f| f.contains("Math") || f.contains("CM")),
        "the bundle should carry a math face, or every equation falls back: {families:?}"
    );
}

#[test]
fn a_document_exports_without_a_font_warning() {
    // The end-to-end property, and the one that would have caught the original problem. Before
    // the bundle, every export on this machine produced `unknown font family: libertinus serif`
    // -- 290 fonts installed, none of them that one -- and the warning was the only evidence of
    // a silent substitution.
    let store = Store::open_in_memory().expect("store");
    let document = store.create_document("Fonts").expect("document");
    store
        .add_section(
            &document.id,
            &json!({"type":"doc","content":[
                {"type":"paragraph","content":[
                    {"type":"text","text":"Body text in the bundled face, with an equation: "},
                    {"type":"inlineMath","attrs":{"latex":"\\int_0^1 e^{-x}\\,dx"}}
                ]},
                {"type":"heading","attrs":{"level":1},"content":[
                    {"type":"text","text":"A heading in the same face"}
                ]}
            ]}),
        )
        .expect("section");

    let result = export_pdf_quiet(&store, &document.id, "Fonts").expect("export");

    assert!(result.pdf.starts_with(b"%PDF"));
    assert!(
        result.warnings.is_empty(),
        "a bundled font should not warn; got {:?}",
        result.warnings
    );
}

#[test]
fn the_font_book_is_the_bundle_and_nothing_else() {
    // The determinism guarantee, stated as the strongest form of it that is now true.
    //
    // The previous version of this test asserted that the bundled families occupied the
    // *lowest* indices -- a real claim, and the right one while a system-font pass existed
    // behind the bundle. It was also unobservable in two directions at once: on a machine with
    // no system fonts it had nothing to compare against and skipped, which is why it failed on
    // `macos-14`, and on a machine with 290 of them it could only say "not *first*", never
    // "not present".
    //
    // The book is now the bundle, so both halves hold everywhere and neither needs a skip:
    //
    //   * every family in the book is a bundled one, and
    //   * every bundled family is in the book.
    //
    // A system font appearing at all is the failure — it means `/usr/share/fonts` (or
    // `/Library/Fonts`, or `C:\\Windows\\Fonts`) is being read again, and a document naming a
    // family the bundle lacks will paginate differently here than on a machine with different
    // software installed. That is the whole of the guarantee, and it is now one assertion
    // rather than a comparison with a precondition.
    let book = world::font_book_families();
    let bundled_families = world::bundled_font_families();
    let bundle_end = world::bundled_face_count();
    assert!(bundle_end > 0, "the bundle should contribute at least one face");

    let expected: std::collections::BTreeSet<String> = bundled_families.iter().cloned().collect();

    let in_book: std::collections::BTreeSet<String> = book.iter().map(|(f, _)| f.clone()).collect();
    assert_eq!(
        in_book, expected,
        "the font book holds {in_book:?} and the bundle provides {expected:?}. Anything extra in \
         the book was read from this machine, so a document naming it would render from whatever \
         this OS happens to have installed."
    );

    // And the faces: the book's length is the bundle's face count, so nothing was appended
    // after the bundle either. Compared against the *face* count rather than the family count,
    // because those are different shapes and a test that confuses them fails in a way that
    // reads like an implementation bug.
    assert_eq!(
        book.len(),
        bundle_end,
        "the book has {} entries and the bundle contributes {bundle_end} faces. A longer book is a \
         font that was appended after the bundle.",
        book.len()
    );
}

#[test]
fn a_document_naming_a_font_the_bundle_lacks_warns_and_still_exports() {
    // What bundle-only actually does to a document that asks for a face it does not have.
    //
    // The obvious guess is that it refuses, and that guess is wrong: Typst reports
    // `unknown font family: <name>` as a **warning** and lays the document out in the default
    // face. Measured, not assumed — the first version of this test asserted an error and the
    // export succeeded, which is how the difference was found.
    //
    // So the contract is not "refused". It is: **the same bytes everywhere, and a warning that
    // names the substitution**. Which is better than what the system-font pass produced, because
    // before, a document naming Arial resolved to whatever Arial the exporting machine had —
    // different glyph widths, a different page count, and silence, because the name *did*
    // resolve. That is the failure the pagination parity job cannot diagnose: the digests just
    // differ.
    //
    // The fallback face is a bundled one by construction, and that is the other half of the
    // guarantee: `the_font_book_is_the_bundle_and_nothing_else` asserts the book holds nothing
    // else, so there is nowhere else for a fallback to come from.
    let source = "#set text(font: \"Holonomy Missing Face\")\nBody text.\n";
    let world = world::HoloWorld::new(source.to_string()).expect("world");
    let warned = typst::compile::<typst_layout::PagedDocument>(&world);

    let messages: Vec<String> = warned
        .warnings
        .iter()
        .map(|w| format!("{w:?}"))
        .collect();
    let named = messages
        .iter()
        .any(|m| m.contains("unknown font family") && m.to_lowercase().contains("holonomy missing face"));

    assert!(
        named,
        "the substitution must be reported and must name the family. Without it the document is \
         typeset in a different face and the user is not told. Warnings: {messages:?}"
    );

    assert!(
        warned.output.is_ok(),
        "the document must still export. Refusing would turn every document naming a font the \
         bundle lacks into an error, which is a larger loss than the substitution it prevents."
    );
    let document = warned.output.expect("exported, asserted above");
    assert!(
        !document.pages().is_empty(),
        "the substituted document produced no pages, so the export is empty rather than merely \
         approximate"
    );
}

#[test]
fn the_bundled_families_are_the_same_on_every_call() {
    // The book is a `LazyLock`, so this is nearly free -- but it is worth holding because the
    // thing that would break it is a mutable cache or a per-call rescan, and either would make
    // pagination depend on how many exports had run before this one.
    let first = world::bundled_font_families();
    let second = world::bundled_font_families();
    assert_eq!(first, second);
    assert!(!first.is_empty());
}

#[test]
fn two_exports_of_the_same_document_paginate_identically() {
    // The property, stated in the only terms that matter to a reader. Two exports in one process
    // are cheap to compare; what makes it meaningful is that it is the *page count* and not a
    // hash, because a page count is what a reader would notice.
    //
    // It cannot prove cross-platform determinism -- that needs a second platform, and it is
    // recorded in `STATUS.md` as the outstanding half. What it does prove is that nothing in
    // the pipeline is stateful across exports, which is the failure mode that would make even a
    // perfect bundle produce different documents on the same machine.
    let store = Store::open_in_memory().expect("store");
    let document = store.create_document("Repeat").expect("document");
    let paragraph = |i: usize| {
        json!({"type":"paragraph","content":[{
            "type":"text",
            "text": format!("Paragraph {i}. The quick brown fox jumps over the lazy dog. ")
                + &"filler words to give the line a length worth justifying. ".repeat(12)
        }]})
    };
    store
        .add_section(
            &document.id,
            &json!({"type":"doc","content":(0..40).map(paragraph).collect::<Vec<_>>()}),
        )
        .expect("section");

    let first = export_pdf_quiet(&store, &document.id, "Repeat").expect("first export");
    let second = export_pdf_quiet(&store, &document.id, "Repeat").expect("second export");

    assert_eq!(first.pages, second.pages, "the page count changed between two exports");
    assert_eq!(
        first.pdf.len(),
        second.pdf.len(),
        "the PDF length changed between two exports"
    );
    assert!(
        first.pages > 0,
        "a document with 40 paragraphs should not be zero pages"
    );
}
