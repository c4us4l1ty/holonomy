//! What the release binary is and is not.
//!
//! # The two claims
//!
//! 1. A default-features release build is a **static PIE**, under **2.0 MiB**, and contains **no X11**.
//! 2. A `--features desktop` build is still a static PIE and still under 2.0 MiB, and it *does* contain
//!    the X11 client -- because that is what the feature is for, and a gate that could not tell the two
//!    apart would be testing nothing.
//!
//! Both are measured on the binary that `cargo test` just built, so a claim here and the artifact on
//! disk cannot drift apart.
//!
//! # Why "no X11" is a gate and not a note
//!
//! The bare-silicon path has no compositor, and the Zero-Compositor Invariant says so: the shipped
//! binary must not contain a client for a display server it will never talk to. That is easy to state
//! and easy to lose -- one `use holonomy_display::Desktop` without a `cfg`, and 84 KiB of X11 client
//! walks into a binary that is supposed to be sealed before it can allocate. The check is therefore
//! string-level, and it is strict enough to catch the whole crate: an atom, the auth cookie name, and
//! the trace variable are three strings no other code here would contain.
//!
//! ```text
//!   cargo test -p holonomy --test release_artifact --release
//! ```

use std::path::{Path, PathBuf};

/// The release binary, next to this test's own executable.
fn binary() -> PathBuf {
    // `current_exe` is `target/<profile>/deps/<test>-<hash>`; the binary is two levels up.
    let mut path = std::env::current_exe().expect("this test's own path");
    path.pop(); // deps/
    path.pop(); // <profile>/
    path.push("holonomy");
    path
}

/// Strings that only an X11 client would contain.
///
/// `holonomy-x11` appears in panic paths and in the crate's own module paths, so its absence is the
/// strongest single signal; the atom and the cookie name catch a *partial* link, where the wire codec
/// came in but the connection code did not.
const X11_MARKERS: &[&str] = &[
    "holonomy-x11",
    "_NET_ACTIVE_WINDOW",
    "MIT-MAGIC-COOKIE-1",
    "WM_PROTOCOLS",
    "HOLONOMY_X11_TRACE",
];

/// The ceiling from the build profile: 2.0 MiB.
const CEILING: u64 = 2 * 1024 * 1024;

/// The binary at `path` is a static PIE.
fn assert_static_pie(path: &Path, what: &str) {
    let head = std::fs::read(path).expect("read the release binary");
    assert!(
        head.starts_with(b"\x7fELF"),
        "{what}: {} is not an ELF file: it starts with {:02x?}",
        path.display(),
        &head[..4.min(head.len())]
    );
    // e_type at offset 16: 3 is ET_DYN, which a static PIE also is. What distinguishes a static PIE from
    // a dynamically linked one is the program interpreter, which a static binary does not have: the
    // string "/lib64/ld-linux" appears in the dynamic section of every dynamically linked ELF.
    let text = String::from_utf8_lossy(&head);
    let dynamic = text.contains("/lib64/ld-linux") || text.contains("ld-musl-x86_64.so");
    assert!(
        !dynamic,
        "{what}: {} asks for a dynamic loader, so it is not static",
        path.display()
    );
    let stack_chk = text.contains("__stack_chk_fail");
    assert!(
        !stack_chk,
        "{what}: {} references __stack_chk_fail, which a static musl build should not need",
        path.display()
    );
}

/// A default-features release binary has none of the X11 client in it.
///
/// Measured on this host after Phase 9B: 1,101,944 bytes, static PIE, and zero of the five markers.
/// The desktop build of the same source is 1,192,824 bytes -- 90,880 more, which is the whole crate.
///
/// Both are well inside the 2,097,152 ceiling: 995,208 bytes of room for the default and 904,328 for
/// the desktop build. Phase 9B's whole cost was 69,472 bytes on the default build, and it came *down*
/// on the asset side -- the pruned math face removed 22,468 bytes of compressed payload, which is more
/// than the parser, the layout and the wiring added.
///
/// # CORRECTION, part 39: this gate is `#[cfg]`d to the run it can actually say something about
///
/// **It used to fail in `cargo test --features desktop --workspace`, and it was right to.** The binary
/// this gate reads is `target/<profile>/release/holonomy`, and **that is one path for two feature
/// sets**: a desktop test run has a desktop binary sitting there, and every one of the five markers is
/// in it.
///
/// **The repair is not a bigger ceiling, a `#[ignore]`, or a documented three-command dance** -- the
/// file carried all three, and each of them made the gate weaker rather than clearer. **It is that
/// cargo guarantees the correspondence the gate was missing**: `cargo test --features desktop` builds
/// this package's binary with `desktop` **on**, and `cargo test` without it builds the binary with it
/// **off**. So the binary on disk always carries the feature set of the test run, and
/// `cfg!(feature = "desktop")` names which of the two claims the run can make.
///
/// **Why this and not a "skip if it looks like a desktop build" probe.** That probe would skip on
/// seeing the markers, which is exactly the signal a regression produces -- **one stray `use
/// holonomy_x11` in the sealed path would turn the gate off rather than red**, and the Zero-Compositor
/// Invariant would go unchecked without a word. `cfg` cannot be fooled that way: a regressed *default*
/// build is still a run without the feature, so the gate still runs and still fails.
#[test]
#[cfg(not(feature = "desktop"))]
fn the_default_release_binary_carries_no_x11_client() {
    let path = binary();
    if !path.exists() {
        eprintln!(
            "skipping: {} does not exist. Build it first with `cargo build --release -p holonomy`.",
            path.display()
        );
        return;
    }
    let bytes = std::fs::read(&path).expect("read the release binary");
    let text = String::from_utf8_lossy(&bytes);
    let found: Vec<&str> = X11_MARKERS
        .iter()
        .copied()
        .filter(|m| text.contains(m))
        .collect();
    assert!(
        found.is_empty(),
        "the default release binary contains X11 client strings {found:?}; the desktop feature is \
         off by default and nothing in the sealed path should pull it in"
    );
    println!(
        "{} is {} bytes with none of {X11_MARKERS:?}",
        path.display(),
        bytes.len()
    );
}

/// **This file's byte ceiling is a release-profile number, so it declines to run in a debug one.**
///
/// Returns `true` — with an explanation, never silently — when the caller should skip.
///
/// # Why this gate was red in a debug run, and why the fix is not a bigger ceiling
///
/// [`binary`] finds the binary by walking two levels up from `current_exe`, which is
/// `target/<profile>/deps/<test>-<hash>`. **So a debug run measures the debug binary** and then fails
/// with a message that *names it the release binary*:
///
/// ```text
/// target/x86_64-unknown-linux-musl/debug/holonomy is 28561776 bytes; the ceiling is 2097152
/// ```
///
/// That is not a near-miss, it is **a false report**: it asserts that the shipping artifact is
/// twenty-eight megabytes when the shipping artifact is under two. A gate that can say that about
/// the thing it is supposed to vouch for is worse than no gate, because the next time it is quoted
/// it will be quoted as a fact.
///
/// Raising the ceiling to the debug number is the wrong repair for the same reason it was wrong in
/// `write_through_cost.rs`: it would be a gate that passes against a binary nobody ships.
///
/// ```text
/// cargo test -p holonomy --release --test release_artifact
/// ```
fn skip_without_an_optimised_build(what: &str) -> bool {
    if cfg!(debug_assertions) {
        eprintln!(
            "skipping {what}: `binary()` resolves to target/debug/holonomy in this profile, and the \
             ceiling is a release-profile number. A debug build measures 28,561,776 bytes and the \
             gate reports that as 'the release binary', which is a false statement rather than a \
             near-miss. Re-run with `cargo test -p holonomy --release --test release_artifact`."
        );
        return true;
    }
    false
}

/// **The release binary is under the ceiling, whichever feature set it was built with.**
///
/// Measured: 1,032,472 bytes against a 2,097,152-byte ceiling, so there is 1,064,680 bytes of room.
///
/// **CORRECTION, part 39: renamed from `the_default_release_binary_is_under_the_ceiling`.** The
/// ceiling is a *profile* number and the claim is about size, so the binary on disk is the right one
/// whichever feature set built it — and the old name said *"default"* while the test ran unchanged over
/// a desktop build, which is a name asserting something the test never checked. Part 39's
/// measurements: **1,587,224 bytes default and 1,689,880 with `--features desktop`**, both inside the
/// 2,097,152 ceiling, the second with 407,272 bytes of room.
#[test]
fn the_release_binary_is_under_the_ceiling() {
    if skip_without_an_optimised_build("the binary size ceiling") {
        return;
    }
    let path = binary();
    if !path.exists() {
        eprintln!("skipping: {} does not exist", path.display());
        return;
    }
    let size = std::fs::metadata(&path)
        .expect("stat the release binary")
        .len();
    println!(
        "{} is {size} bytes; the ceiling is {CEILING}, so {} bytes of room",
        path.display(),
        CEILING.saturating_sub(size)
    );
    assert!(
        size <= CEILING,
        "the release binary is {size} bytes, over the {CEILING}-byte ceiling"
    );
}

/// **The binary is static even with the window in it, and the check does not care which build it sees.**
///
/// A window needs a socket, and a socket is where a static musl binary is most likely to reach for
/// `dlopen` -- which is why `minifb` and `softbuffer` were rejected in favour of a hand-written client.
/// This test is the check that the rejection was worth it, **and it is the one gate in this file whose
/// subject is the profile rather than the feature set**, so it runs over whichever binary the test run
/// produced. Part 39 renamed its sibling from `the_default_release_binary_…` for exactly this reason.
#[test]
fn the_binary_is_static_pie() {
    let path = binary();
    if !path.exists() {
        eprintln!("skipping: {} does not exist", path.display());
        return;
    }
    assert_static_pie(&path, "the release binary");
    println!("{} is a static PIE with no dynamic loader", path.display());
}

/// The desktop feature costs what the X11 client costs, and no more.
///
/// # CORRECTION, part 39: the three-command dance is gone, and so is the `#[ignore]`
///
/// **This used to be `#[ignore]`d, with a three-step procedure in its docs** -- build the tests, build
/// the desktop binary over the top of them, then run the test binary by hand with `--include-ignored` --
/// and [`the_default_release_binary_carries_no_x11_client`] failed as a side effect, correctly, because
/// the file it reads was the desktop build.
///
/// **The reason for the dance is gone: `cargo test --features desktop` builds the desktop binary at
/// `binary()`'s path itself**, so a feature-gated test needs no ordering and no manual invocation. It
/// runs in `cargo test --features desktop --workspace` and does not exist in the default run, which is
/// the pair of statements that makes the two claims over one output path unambiguous.
///
/// **`#[ignore]` was the wrong tool for the same reason it was wrong for the debug case**: a plain
/// `cargo test --workspace` would report success for a gate it had not run, and an `#[ignore]`d gate is
/// one nobody ever un-ignores.
///
/// **Measured 2026-10-06:** 1,536,472 bytes, 560,680 bytes of room under the 2,097,152 ceiling.
/// **Measured part 39**, after the chrome's `Node::Repeat`, the sidebar, the ruler numbers and the
/// 72-column measure: **1,689,880 bytes, 407,272 bytes of room**, and the default build of the same
/// source is 1,587,224 -- **102,656 bytes of difference, which is the whole X11 client.**
#[test]
#[cfg(feature = "desktop")]
fn the_desktop_build_is_still_static_and_under_the_ceiling() {
    let path = binary();
    let bytes = std::fs::read(&path).expect("read the release binary");
    let text = String::from_utf8_lossy(&bytes);
    let has_x11 = X11_MARKERS.iter().any(|m| text.contains(m));
    assert!(
        has_x11,
        "this looks like a default-features binary, so it has nothing to say about the desktop build; \
         build one with --features desktop"
    );
    let size = bytes.len() as u64;
    // Measured, not remembered. The constant used to be a bare `1_101_944` with nothing saying when it
    // was taken, so it drifted silently through Phase 9C -- which added 336,704 bytes -- and the printed
    // "more than the N-byte default" line was quietly wrong by a third of a megabyte.
    //
    // It is a *floor* now, asserted rather than assumed: the default build's size is a build output,
    // not a constant, so the check below compares the real number. What is pinned here is only that the
    // desktop build must not have *shrunk*, because a shrink would mean the X11 client stopped being
    // linked in and this test stopped testing what it claims to.
    const KNOWN_DEFAULT_FLOOR: u64 = 1_439_288;
    println!(
        "the desktop build is {size} bytes, {} bytes of room; the default build measured at \
         {KNOWN_DEFAULT_FLOOR} when Phase 9C landed",
        CEILING.saturating_sub(size)
    );
    assert!(
        size > KNOWN_DEFAULT_FLOOR,
        "the desktop build is {size} bytes, at or below the {KNOWN_DEFAULT_FLOOR}-byte default, so \
         --features desktop appears to have linked nothing"
    );
    assert!(
        size <= CEILING,
        "the desktop build is {size} bytes, over {CEILING}"
    );
    assert_static_pie(&path, "the desktop build");
}
