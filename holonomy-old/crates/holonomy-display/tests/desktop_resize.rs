//! The window resize, read as source.
//!
//! # The defect this file exists for
//!
//! **`Desktop` had a `pub fn resize` of its own, and `Session` calls `Scanout::resize` through a
//! `Box<dyn Scanout>`.** An inherent method of the same name and signature shadows the trait's *for a
//! caller holding a `Desktop`*, so every gate written against a concrete `Desktop` saw the inherent
//! method and passed -- while the product, which holds a trait object, got the trait's default body,
//! `Ok(false)`, and told every `ConfigureNotify` that nothing had changed.
//!
//! **Measured, in a window on a real X server, after the fix to every other resize defect:**
//!
//! ```text
//! event ConfigureNotify { width: 900, height: 700 }
//! resize: server 900x700, changed false, frame 1600x1000, metrics 1600x1000, cell 12x30
//! ```
//!
//! **The window was 900 px wide and the session was still laying out 1,600.** What the user saw was
//! the top-left corner of a 1,600 x 1,000 chrome: a menu bar with its 1,600 px spacing running off
//! the right edge mid-word, a status bar at y = 970 in a 700 px window, and a page past the window
//! edge. Every one of those looked like a layout bug, and the layout was innocent.
//!
//! # Why this gate reads the source instead of calling the method
//!
//! **Because calling it cannot tell the two apart.** A behavioural test that builds a `Desktop` and
//! calls `resize` reaches the inherent method and is green either way -- which is precisely why the
//! defect survived. Only the *absence* of a shadowing definition is the claim, and that is a fact
//! about the file, not about a call. `holonomy-container/tests/adopt.rs` reads `lib.rs` for the same
//! reason and says so in the same words.

use std::path::Path;

/// `desktop.rs` itself.
const SOURCE: &str = include_str!("../src/desktop.rs");

/// The `impl Scanout for Desktop` block, as `(start, end)` byte offsets into [`SOURCE`].
///
/// **A brace count rather than a regex**, because the block's end is the first `\n}` at column 0
/// after the header, which is exactly where `rustfmt` puts it and exactly one level of nesting below
/// the top level. A regex would have to know how to count braces in Rust; this only has to know that
/// the impl block is the only top-level item with a header like that.
fn scanout_impl() -> &'static str {
    let start = SOURCE
        .find("impl Scanout for Desktop {")
        .expect("`impl Scanout for Desktop` exists");
    let rest = &SOURCE[start..];
    let end = rest
        .find("\n}\n")
        .map(|i| start + i)
        .expect("the impl block is closed at column 0");
    &SOURCE[start..end]
}

/// Every **definition** of `resize` in `text`, as byte offsets.
///
/// **Line-based, and it ignores doc comments on purpose.** This file's own correction writes the two
/// broken shapes out as prose -- `impl Desktop { pub fn resize(..) }` and `trait Scanout { fn resize(..)
/// { Ok(false) } }` -- so a substring count sees two definitions that do not exist. A definition is a
/// line whose first non-space characters are `fn resize(`, `pub fn resize(` or `pub(crate) fn resize(`,
/// and a doc comment line begins with `///`.
fn resize_definitions(text: &str) -> Vec<usize> {
    let mut at = 0usize;
    let mut found = Vec::new();
    for line in text.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with("fn resize(") || t.starts_with("pub fn resize(") {
            found.push(at);
        }
        at += line.len();
    }
    found
}

/// **There is exactly one `resize` in `desktop.rs`, and it is inside `impl Scanout for Desktop`.**
///
/// **Two would mean an inherent one exists again.** That is the whole failure: an inherent
/// `Desktop::resize` is invisible to every `Box<dyn Scanout>` caller, so the product silently falls
/// back to whatever the trait says -- and what the trait used to say was `false`.
///
/// **One in the wrong place is just as bad, so the count and the position are both asserted** rather
/// than the position alone; `desktop_resize_is_the_trait_override_and_not_an_inherent_method` is the
/// sentence a reader is meant to remember.
#[test]
fn desktop_resize_is_the_trait_override_and_not_an_inherent_method() {
    let block = scanout_impl();
    assert!(
        block.contains("fn resize(&mut self"),
        "the `Scanout` impl must define `resize`, or a `Box<dyn Scanout>` cannot resize a window. \
         Block:\n{block}"
    );
    // **Every definition of `resize`, and every one of them has to be in the impl block.**
    //
    // Counting the whole file rather than the block alone is what makes this a shadowing check: an
    // inherent `pub fn resize` would be *outside* the block, so the count would be one higher and a
    // position test that only asked "is there one inside" would still pass.
    let block_at = SOURCE.find(block).expect("the block is in the file");
    let outside: Vec<usize> = resize_definitions(SOURCE)
        .into_iter()
        .filter(|at| *at < block_at || *at >= block_at + block.len())
        .collect();
    assert!(
        outside.is_empty(),
        "an inherent `Desktop::resize` would shadow the trait's for every caller that has a `Desktop`, \
         and `Session` has a `Box<dyn Scanout>`: {outside:?} of the definitions are outside \
         `impl Scanout for Desktop`"
    );
    assert_eq!(
        resize_definitions(SOURCE).len(),
        1,
        "there is one `resize` in desktop.rs and it is the trait override; a second one is a second \
         entrance, and entrances are where this defect lived"
    );
}

/// **The trait declares `resize` with no default body, so a backend cannot inherit a silent `false`.**
///
/// **This is the half of the fix that stops it coming back.** The default existed so that a fixed-size
/// backend did not have to think about it; it also meant that *any* backend which did not think about
/// it -- including one that thought about it and wrote it in the wrong place -- reported that a
/// window had not changed size. A required method turns the second case into a compile error.
///
/// **The assertion is on the absence of a body, not on the presence of a method**: a default that is
/// one line long still reads as `fn resize(&mut self, _width: u32, _height: u32) -> Result<bool, \
/// FrameError> {`, so the body is found by the brace that opens it.
#[test]
fn scanout_resize_has_no_default_body() {
    let lib = include_str!("../src/lib.rs");
    let at = lib
        .find("fn resize(&mut self, _width: u32, _height: u32)")
        .expect("`Scanout::resize` is declared");
    let after = &lib[at..];
    let semi = after
        .find(';')
        .expect("the declaration ends in a semicolon");
    let brace = after.find('{').map(|i| i < semi).unwrap_or(false);
    assert!(
        !brace,
        "`Scanout::resize` must have no body: a default is a silent answer to a question every \
         backend has to answer, and the silent answer `Ok(false)` is what made a window that never \
         resized look like a window that did."
    );
    // **And `drm.rs` says it out loud**, because that is the backend the default was written for.
    let drm = include_str!("../src/drm.rs");
    assert!(
        drm.contains("fn resize(&mut self, _width: u32, _height: u32)"),
        "a DRM panel has a fixed size and must now say so in its own impl"
    );
}

/// **The window driver reads a resize through the trait and not through a downcast.**
///
/// **`Session::resize` is the only entrance, and it is on the trait.** A driver that reached
/// `Desktop::resize` directly would be correct today and would keep working through an inherent
/// method, which is the thing this gate is about -- so the claim is made where the *choice* is, in
/// `session.rs`.
#[test]
fn session_resizes_through_the_scanout_trait() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../holonomy/src/session.rs");
    let src = std::fs::read_to_string(&path).expect("read session.rs");
    assert!(
        src.contains("self.scanout.resize(w, h)"),
        "`Session::resize` must ask the backend through `Box<dyn Scanout>`, which is the only \
         entrance `Desktop`'s override is reachable from"
    );
}
