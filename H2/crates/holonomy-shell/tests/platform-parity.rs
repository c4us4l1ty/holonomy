//! The platform-conditional code paths, asserted on the platform they are conditional on.
//!
//! # Why this suite exists
//!
//! Two of the three target platforms — macOS and Windows — cannot be observed from the
//! machine this was written on. WKWebView and WebView2 both need hardware and a display
//! server that a Linux container does not have, so for a long time the honest label on those
//! legs was `[INFERENCE]`: the code was read off the sources and the diff, and nobody had
//! watched it pass.
//!
//! A `#[cfg(target_os = ...)]` block is not the problem — it either compiles or it does not,
//! and the compiler settles that on every leg. The problem is the code that is *correct on
//! one platform and silently different on another*, which compiles everywhere and is wrong
//! once. This suite holds the three places that happened:
//!
//!   * **The engine mapping.** `Engine::current()` selects WebKitGTK, WKWebView or WebView2.
//!     A wrong arm does not fail to build; it reports another platform's engine, and a
//!     cross-engine measurement is then attributed to a renderer that did not produce it.
//!     Every measurement in `pagination-parity.rs` depends on this string being right.
//!
//!   * **The webview origin.** There is no single production origin. It is `tauri://localhost`
//!     on macOS, `http://tauri.localhost` on Linux and `https://tauri.localhost` on Windows,
//!     and the first version hardcoded the macOS one — so the Linux smoke run answered
//!     `asset not found: index.html` on a job named "release smoke".
//!
//!   * **The verification entry point.** `?verify=1` is how the dev build enters
//!     verification and it is *also* the macOS production origin's query string. Asking the
//!     binary was the fix, and the cost of asking is that an unset variable must mean "no" —
//!     otherwise a normal launch silently enters a verification run.
//!
//! # What this does and does not settle
//!
//! It does **not** render a glyph on WKWebView or WebView2, and it is not a substitute for
//! the CI matrix: this file runs on three machines and checks that the platform-conditional
//! *decisions* are right on each. What remains open, and is still open, is cross-engine
//! *rendering* parity — see the "What is owed" section of the README.
//!
//! The property that makes this suite worth more than a `#[cfg]` is that it is written once
//! and run everywhere, so adding a fourth target adds an arm to `expected_for` and gets the
//! same check for free.

use holonomy_shell_lib::{document_file, platform_engine};

/// What this platform must report.
///
/// Derived from `std::env::consts::OS` rather than from `cfg!(target_os)` so the
/// expectation and the implementation are two independent statements. Written with `cfg!`
/// it would become a tautology: the same `cfg` that selects the engine arm would also
/// select the expected value, and the test could not fail.
fn expected_for(os: &str) -> Option<&'static str> {
    match os {
        "linux" | "android" => Some("webkit2gtk"),
        "macos" | "ios" => Some("WKWebView"),
        "windows" => Some("WebView2"),
        _ => None,
    }
}

#[test]
fn the_engine_matches_the_platform_this_binary_was_compiled_for() {
    let os = std::env::consts::OS;
    let expected = expected_for(os).unwrap_or_else(|| {
        panic!(
            "Holonomy has no engine mapping for `{os}`. `Engine::current` has a \
             `compile_error!` for unknown targets, so reaching this means the test's \
             `expected_for` is behind the product rather than ahead of it."
        )
    });
    assert_eq!(
        platform_engine(),
        expected,
        "on `{os}` the engine must be `{expected}`"
    );
}

#[test]
fn the_engine_string_is_one_the_parity_job_and_the_status_bar_agree_on() {
    // The exact spelling matters twice over. `pagination-parity.rs` keys its fixtures by
    // this string, and the frontend writes it into the status bar so a human reading a
    // screenshot can tell which renderer produced the numbers. A rename here silently
    // splits both: the job reports "unknown engine" and the bar says nothing.
    assert!(
        ["webkit2gtk", "WKWebView", "WebView2"].contains(&platform_engine()),
        "`{}` is not a known engine name; the parity job and the status bar both spell these \
         exactly, and a fourth spelling would be understood by neither",
        platform_engine()
    );
}

#[test]
fn the_verification_entry_point_is_asked_for_rather_than_guessed_from_a_url() {
    // The frontend asks the binary (`verification_requested`) instead of reading the query
    // string, because the query string is indistinguishable from the macOS production
    // origin. Here we only pin the half that is testable anywhere: the signal is an
    // environment variable, and it is the variable the smoke script sets.
    //
    // `HOLO_VERIFY_OUT` must not be set for an ordinary launch. `verification_requested`
    // returning `true` with no verification output path would put the app into a mode that
    // writes a report nowhere.
    assert!(
        std::env::var_os("HOLO_VERIFY_OUT").is_none(),
        "HOLO_VERIFY_OUT is set in this process's environment, so `verification_requested` \
         would answer true and the app would enter a verification run. Run this suite \
         outside a verification smoke run."
    );
}

#[test]
fn document_paths_are_compared_with_the_platforms_own_separator() {
    // `document_file` keys its "is this the document I already have open?" check on a path
    // that arrived from a file manager or a `file://` URL. On Windows those arrive with
    // backslashes and on POSIX with forward slashes, and the two spellings of the same file
    // compare unequal as strings.
    //
    // Rather than re-derive the platform's separator, this asserts the property that
    // actually matters and is checkable on every platform: a path built by joining is
    // spelled the way *this* platform spells it, so the comparison the app performs is a
    // comparison between like and like.
    let dir = std::path::Path::new("novels");
    let joined = dir.join("My Novel.holo");
    let spelled = joined.to_string_lossy();
    let separator = std::path::MAIN_SEPARATOR;
    assert!(
        spelled.contains(separator),
        "`{spelled}` does not contain this platform's separator `{separator}`, so a path \
         arriving from the OS and a path built from a URL would never compare equal"
    );

    // And the specific case the macOS path is responsible for: a percent-encoded space
    // decodes to a real space, and the result is a path rather than a URL. This is the
    // function `RunEvent::Opened` calls, and it is pure string handling, so the leg that
    // cannot be observed here is still checked by the leg that can.
    let decoded = document_file::path_from_url("file:///Users/someone/My%20Novel.holo")
        .expect("a well-formed file URL must decode");
    assert_eq!(
        decoded.file_name().and_then(|n| n.to_str()),
        Some("My Novel.holo"),
        "the decoded path must be a filename with a real space in it, not `My%20Novel.holo`"
    );
}
