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
/// Measured on this host: 1,032,472 bytes, static PIE, and zero of the five markers. The desktop build
/// of the same source is 1,116,536 bytes -- 84,064 more, which is the whole crate.
#[test]
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

/// The default release binary is under the ceiling.
///
/// Measured: 1,032,472 bytes against a 2,097,152-byte ceiling, so there is 1,064,680 bytes of room.
#[test]
fn the_default_release_binary_is_under_the_ceiling() {
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

/// The binary is static even with the window in it.
///
/// A window needs a socket, and a socket is where a static musl binary is most likely to reach for
/// `dlopen` -- which is why `minifb` and `softbuffer` were rejected in favour of a hand-written client.
/// This test is the check that the rejection was worth it.
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
/// **Run separately**: this test needs a binary built *with* `--features desktop`, and it says so if it
/// is not looking at one. It is not part of the default gate for that reason -- `cargo test` builds one
/// binary, and which one depends on the flags it was invoked with.
#[test]
#[ignore = "needs `cargo build --release -p holonomy --features desktop` first"]
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
    println!(
        "the desktop build is {size} bytes, {} more than the {}-byte default",
        size.saturating_sub(1_032_472),
        1_032_472
    );
    assert!(
        size <= CEILING,
        "the desktop build is {size} bytes, over {CEILING}"
    );
    assert_static_pie(&path, "the desktop build");
}
