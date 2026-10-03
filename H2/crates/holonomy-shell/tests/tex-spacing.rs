//! Spacing commands, and one dead arm that was a live bug.
//!
//! # Why this suite exists
//!
//! Because `clippy`'s `unreachable_pattern` fired on an arm in the spacing matcher, and the
//! right response to that is to read it rather than to delete it. Reading it produced:
//!
//! ```text
//! Some(c @ (',' | ';' | ':' | '!')) => { ... _ => "thin_space()" ... }
//! ...
//! Some('!') => {}          // with a comment saying `!` must emit nothing
//! ```
//!
//! Both statements were in the code at once. `'!'` was caught by the *first* arm, whose inner
//! match sent it to `thin_space()`, so the arm that was supposed to emit nothing could never
//! run. A negative thin space had quietly become a positive one, and the comment beside the
//! dead arm said the opposite.
//!
//! The lint is therefore not a style complaint about an unused branch. It is the only thing
//! that noticed that the code and its own documentation disagreed.
//!
//! # Why the correction needs two halves
//!
//! Removing `'!'` from the spacing arm is what makes the emit-nothing arm reachable — and
//! *deleting* that arm instead, which was the first attempt, sends `\!` to the catch-all that
//! preserves a backslash, producing `a \ b`. That compiles as far as this crate is concerned
//! (it is a string) and is rejected by Typst.
//!
//! And the recorded fixture data cannot tell the two apart: `a \! b` is in
//! `tex-to-typst.json` with a `compiles` flag recorded when the correct arm was in place, and
//! the frontend test asserts the *recorded* flags, so it passes either way. Only a real
//! compile distinguishes them — which is why the assertion below is on the emitted string and
//! not on a recorded flag.

use holonomy_shell_lib::export::translate::tex_to_typst_math;

/// The emitted markup must contain no backslash.
///
/// # Why a backslash is the thing to assert
///
/// Because a backslash in Typst markup is not a literal backslash — it opens a code
/// expression, and Typst reports "the character `\` is not valid in code". So a command whose
/// replacement is "nothing" and one whose replacement is "the backslash" differ by the
/// difference between compiling and not.
fn emitted(input: &str) -> String {
    tex_to_typst_math(input)
}

#[test]
fn the_negative_thin_space_emits_nothing() {
    for input in ["a \\! b", "e^{-x} \\! dx", "\\! a"] {
        let out = emitted(input);
        assert!(
            !out.contains('\\'),
            "`{input}` emitted a backslash, which Typst reads as the start of a code \
             expression rather than a literal: {out:?}"
        );
    }
}

#[test]
fn the_positive_spacing_commands_do_emit_their_typst_spelling() {
    // The other half of the distinction: if `\!` and `\,` emitted the same thing, the first
    // test would pass for the wrong reason. Together they say the matcher separates them.
    let cases = [
        ("a \\, b", "thin_space()"),
        ("a \\; b", "med_space()"),
        ("a \\: b", "thick_space()"),
    ];
    for (input, expected) in cases {
        let out = emitted(input);
        assert!(
            out.contains(expected),
            "`{input}` should emit {expected} and emitted {out:?}"
        );
    }

    assert!(
        !emitted("a \\! b").contains("thin_space()"),
        "the negative thin space is being emitted as a positive one — the exact bug the \
         unreachable arm was hiding"
    );
}

#[test]
fn an_escaped_literal_keeps_its_backslash() {
    // `%`, `&` and `#` are TeX escapes for characters Typst reads literally, so the backslash
    // is the *point*. A blanket "no backslashes in the output" rule -- which the test above
    // would be the first to invite -- would delete it and change the character.
    for input in ["100\\% sure", "a \\& b"] {
        assert!(
            emitted(input).contains('\\'),
            "`{input}` lost its backslash, so the escaped character is no longer escaped: {:?}",
            emitted(input)
        );
    }
}

/// Export one equation as a one-section document, for the compile assertions below.
fn export_one(latex: &str) -> Result<usize, String> {
    let dir = tempfile::tempdir().expect("could not make a temp dir");
    let store = holonomy_core::Store::open(&dir.path().join("spacing.holo")).unwrap();
    let doc = store.create_document("spacing").unwrap();
    store
        .add_section(
            &doc.id,
            &serde_json::json!({
                "type": "doc",
                "content": [{"type": "paragraph", "content": [
                    {"type": "mathInline", "attrs": {"latex": latex},
                     "content": [{"type": "text", "text": "x"}]}
                ]}]
            }),
        )
        .unwrap();
    store.flush_all().unwrap();
    holonomy_shell_lib::export::pdf::export_pdf_quiet(&store, &doc.id, "spacing")
        .map(|r| r.pages)
        .map_err(|e| e.to_string())
}

/// Every spacing form compiles — checked by exporting, not by reading a string.
///
/// # Why this needed a real export
///
/// Because a string assertion is a guess about what Typst accepts, and the guess in the first
/// version of this suite was wrong. It asserted that `\\dx` is emitted as `d x`, on the
/// strength of a comment in `translate.rs`. It is not: the translator emits `\\dx` unchanged,
/// and it compiles. The backslash is what stops Typst's lexer joining the letters into one
/// identifier, so the separation the comment describes is performed by the *escape* rather than
/// by an inserted space.
///
/// Which means the comment and the code agree about the outcome and not about the mechanism,
/// and only an export can tell those apart. All five cases were measured: each produces one
/// page.
#[test]
fn every_spacing_form_compiles() {
    let cases = [
        "\\int_0^1 x \\, dx",
        "\\int_0^1 x \\dx",
        "\\int_0^1 x dx",
        "a \\! b",
        "100\\% sure",
    ];
    for latex in cases {
        assert_eq!(
            export_one(latex),
            Ok(1),
            "`{latex}` did not compile. The recorded fixture data cannot catch this, because it \
             was recorded while the arms were correct and asserting on it would pass either way."
        );
    }
}
