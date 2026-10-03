//! The zstd interop fixture is what the JavaScript decoder expects.
//!
//! # Why this test exists
//!
//! The boot payload carries `content_zstd: Vec<u8>`, decoded in the renderer by
//! `fzstd`. Two independent implementations have to agree on the format, and nothing
//! in either language enforces it: a frame Rust writes is decompressed by a
//! completely separate JavaScript decoder, and a disagreement would surface as
//! corrupt section content at boot — in a packaged app, on the first open, with the
//! user's document involved.
//!
//! So the fixture is pinned from both sides:
//!
//! - this test: Rust decodes the committed frame and requires the committed JSON back.
//! - `app/test/boot.ts`: `fzstd` decodes the same frame and requires the same JSON.
//!
//! Together those say "a frame from the store is readable by the renderer". Either
//! alone would leave a gap: this test would pass if the JavaScript decoder were wrong,
//! and that test would pass if the encoder were wrong.
//!
//! # Regenerating
//!
//! ```sh
//! # writes app/test/fixtures/rust-zstd-frame.{bin,json}
//! cargo run -p holonomy-core --example gen_interop_fixture
//! ```
//!
//! Only when the encoding or the zstd level changes. A regeneration that changes the
//! frame but not the JSON is a compressor change; one that changes both is a content
//! change, and both should be reviewed.

use std::path::Path;

#[test]
fn rust_can_read_the_frame_the_renderer_will_decode() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/test/fixtures");
    let frame = std::fs::read(dir.join("rust-zstd-frame.bin"))
        .expect("rust-zstd-frame.bin; regenerate with the gen_interop_fixture example");
    let expected: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.join("rust-zstd-frame.json")).expect("rust-zstd-frame.json"),
    )
    .expect("the fixture JSON parses");

    let got = holonomy_core::store::decode("fixture", &frame).expect("Rust decodes its own frame");
    assert_eq!(
        got, expected,
        "the committed frame does not hold the committed JSON. If the encoder or the \
         zstd level changed, regenerate both fixtures together."
    );
}

/// The frame carries content a decoder could plausibly mangle.
///
/// A fixture of plain ASCII would pass against a decoder that drops multi-byte
/// characters, truncates at a null, or loses trailing whitespace. This one has an em
/// dash, Greek, Han, an emoji (a surrogate pair on the wire), a non-breaking space,
/// nested marks, a list, and a heading with attributes — each of which is something a
/// subtly wrong implementation gets wrong in a different way.
#[test]
fn the_fixture_contains_what_a_wrong_decoder_would_break() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/test/fixtures");
    let expected: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.join("rust-zstd-frame.json")).expect("rust-zstd-frame.json"),
    )
    .unwrap();
    let text = serde_json::to_string(&expected).expect("serialise");

    for (what, needle) in [
        ("an em dash", "\u{2014}"),
        ("Greek", "\u{03b1}"),
        ("Han", "\u{4e2d}"),
        ("an emoji", "\u{1f389}"),
        ("a non-breaking space", "\u{00a0}"),
        ("two marks on one run", "highlight"),
        ("a list node", "bulletList"),
        ("heading attributes", "\"level\""),
    ] {
        assert!(
            text.contains(needle),
            "the fixture has no {what}; a decoder that breaks on one would pass against it"
        );
    }
}
