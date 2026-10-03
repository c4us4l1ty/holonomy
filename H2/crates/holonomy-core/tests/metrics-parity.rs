//! The frontend's section metrics agree with `analyze` on a shared fixture.
//!
//! # Why a parity test at all, given DOCTRINE.md §8
//!
//! Because §8 forbids a *duplicated source of truth kept in step by a parity test*.
//! This is the other case: `analyze` is the definition and runs on every write, but it
//! cannot answer the two questions the renderer asks — "has this section passed 1500
//! words" on every keystroke, and "what are the counts for a section that does not exist
//! yet" after a split. Both would put the store on the typing path, which is the one
//! thing the WAL exists to avoid.
//!
//! So the renderer recomputes them locally, and the arrangement that keeps the two
//! honest is a shared fixture with one expected triple, asserted from both languages.
//! Neither side can change without one of the two failing.
//!
//! What this does *not* license: the renderer may not grow its own geometry model, and
//! `localMetrics`'s comment says so. It counts what the split trigger reads and what a
//! freshly split section needs to be laid out, and nothing else.
//!
//! # The fixture
//!
//! `app/test/fixtures/metrics-parity.json` — deliberately awkward, because a fixture of
//! uniform paragraphs would agree with almost any counting rule:
//!
//! - adjacent inline runs with no space between them (`three` + `four` is one word),
//!   which catches a concatenation that loses word boundaries;
//! - a nested list, which separates lines without being a top-level block;
//! - three inline mark instances, two on one run, which catches mark counting that only
//!   looks at the first run of a block.

use std::path::Path;

#[test]
fn analyze_on_the_parity_fixture() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../app/test/fixtures/metrics-parity.json");
    let raw = std::fs::read(&path).expect("metrics-parity.json; it is committed beside this test");
    let json: serde_json::Value = serde_json::from_slice(&raw).expect("the fixture parses");

    let analyzed = holonomy_core::store::analyze(&json);

    // `one two threefourfive / six / seven` is five words: the first and second runs of
    // the opening paragraph meet with no space, so they are one word. This is the case a
    // naive per-run word count gets wrong by doubling.
    assert_eq!(analyzed.word_count, 5, "word count");
    // Non-whitespace characters: the two newlines Rust inserts do not count.
    assert_eq!(analyzed.char_count, 27, "character count");
    // Three top-level blocks. The list is one block, and the paragraph inside it is not
    // counted separately.
    assert_eq!(analyzed.block_count, 3, "block count");

    // The extracted text, because the word count above is only explicable from it, and a
    // future change to where the separators go should have to change this line too.
    assert_eq!(analyzed.text, "one two threefourfive\nsix\nseven");
}

#[test]
fn the_parity_fixture_is_shared_with_the_frontend() {
    // A fixture nothing reads is a fixture that will drift. The frontend asserts the
    // same three numbers from `app/test/lifecycle.ts`; if this file is ever moved or
    // renamed, this test is the one that notices.
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../app/test/lifecycle.ts");
    let src = std::fs::read_to_string(&path).expect("lifecycle.ts");
    assert!(
        src.contains("metrics-parity.json"),
        "app/test/lifecycle.ts no longer reads the parity fixture; one of the two \
         language-side assertions is now checking nothing"
    );
    // The exact form the assertions take in that file. Matching on the literals rather
    // than on a shape like "words: 5" keeps this a check that the numbers are the same
    // numbers, which is the entire claim.
    for needle in [
        "m.words === 5",
        "m.chars === 27",
        "m.blocks === 3",
    ] {
        assert!(
            src.contains(needle),
            "app/test/lifecycle.ts no longer asserts `{needle}`; one side of the parity is \
             nominal"
        );
    }
}
