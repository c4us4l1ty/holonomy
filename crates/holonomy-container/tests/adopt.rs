//! **`Wavefunction::adopt` opens a container from a descriptor the caller already holds.** 5 tests.
//!
//! # Why this exists
//!
//! `Wavefunction::open(path, …)` opens its own path, and **the product cannot use it.** The boot opens
//! every descriptor at stage 4 and installs the seccomp filter at stage 8, and the allowlist has **no
//! `openat`** — so after sealing, no path can become a descriptor. The passphrase is read *after* sealing,
//! so the container cannot be decrypted during boot either. The only reachable route is a descriptor that
//! already exists, which is what `adopt` takes.
//!
//! The alternative — adding `openat` to the allowlist — would give up the rule the boot exists to enforce
//! (the set of open files is fixed before the world closes) for the sake of convenience. `adopt` gets to the
//! same place without weakening anything.
//!
//! | what it proves | test |
//! | --- | --- |
//! | `adopt` reads what `open` reads | [`adopt_and_open_read_the_same_document`] |
//! | it works on a descriptor from a *different* opener | [`adopt_works_on_a_descriptor_from_create_or_open`] |
//! | a wrong passphrase is still refused | [`adopt_still_refuses_a_wrong_passphrase`] |
//! | damage is still detected | [`adopt_still_detects_a_flipped_bit`] |
//! | and it is the same code path, not a copy | [`open_is_a_delegate_to_adopt`] |
//!
//! # The last test is the one that keeps this honest
//!
//! `open` is now two lines that call `adopt`. If someone later grows `open` a second implementation — an
//! early-out, a different salt read, a ring seeded differently — the two would diverge and every other test
//! here would still pass, because each tests one of them against the same *container*, not against the
//! other. So this gate asserts the relationship directly rather than the behaviour.

use holonomy_container::io::DirectFile;
use holonomy_container::Wavefunction;
use std::path::PathBuf;

const PASS: &str = "correct horse battery staple";
const ITER: u64 = 1;

fn scratch_dir() -> PathBuf {
    let dir = std::env::current_exe()
        .expect("test exe")
        .ancestors()
        .nth(3)
        .expect("target/<profile> layout")
        .join("holonomy-container-tests")
        .join("adopt");
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

fn content(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i.wrapping_mul(97).wrapping_add(13) % 251) as u8).collect()
}

/// Create a container and hand back its path.
fn make(len: usize) -> PathBuf {
    let path = scratch_dir().join(format!("adopt-{}-{}.wavefunction", len, unique()));
    Wavefunction::create(&path, PASS, "adopted", &content(len), ITER).expect("create");
    path
}

/// **The two entry points see the same document.** The comparison is content, not just success.
#[test]
fn adopt_and_open_read_the_same_document() {
    let doc = content(120_000);
    let path = make(doc.len());

    let mut via_open = Wavefunction::open(&path, PASS, ITER).expect("open");
    let mut via_adopt = Wavefunction::adopt(DirectFile::open(&path).expect("descriptor"), PASS, ITER)
        .expect("adopt");

    assert_eq!(via_open.title(), via_adopt.title(), "same title");
    assert_eq!(via_open.content_len(), via_adopt.content_len(), "same length");
    assert_eq!(via_open.omega(), via_adopt.omega(), "same derived omega");
    assert_eq!(
        via_open.read_content().expect("open read"),
        via_adopt.read_content().expect("adopt read"),
        "and the same bytes"
    );
    let _ = std::fs::remove_file(&path);
}

/// **The descriptor comes from a different opener.** `create_or_open` is what `main.rs` calls at stage 4 —
/// it opens with `O_CREAT` and sizes the file, which is a different syscall path from `DirectFile::open`.
/// If `adopt` secretly re-opened by path, this would still pass, which is why
/// [`open_is_a_delegate_to_adopt`] exists as the structural check.
#[test]
fn adopt_works_on_a_descriptor_from_create_or_open() {
    let doc = content(70_000);
    let path = make(doc.len());

    let fd = DirectFile::create_or_open(&path).expect("create_or_open -- the boot's route");
    let mut wf = Wavefunction::adopt(fd, PASS, ITER).expect("adopt");
    assert_eq!(wf.read_content().expect("read"), doc, "the boot's descriptor reads the document");

    let _ = std::fs::remove_file(&path);
}

/// A wrong passphrase must be refused, **through `adopt` as well as `open`** — a new entry point that
/// skipped the KDF would accept anything, and that would be a catastrophic silent failure.
#[test]
fn adopt_still_refuses_a_wrong_passphrase() {
    let path = make(40_000);
    let fd = DirectFile::open(&path).expect("descriptor");
    let err = Wavefunction::adopt(fd, "correct horse battery stapl", ITER)
        .expect_err("a wrong passphrase must be refused");
    // **The refusal, not its kind.** `ContainerError` distinguishes `Io`, `Aead`, `Frame`, `Ring`,
    // `Chaff` and `Envelope`, and a wrong passphrase surfaces through whichever of those the failure
    // happened to reach first. Naming one here would pin a diagnostic rather than the security property,
    // and the property is only that a wrong passphrase **does not open the container**.
    assert!(
        !matches!(err, holonomy_container::ContainerError::Io(_)) && err.to_string().is_empty() == false,
        "a wrong passphrase must be refused with a real error, got {err:?}"
    );
    // And the right one still works on a fresh descriptor, so the failure was the passphrase.
    assert!(
        Wavefunction::adopt(DirectFile::open(&path).expect("descriptor"), PASS, ITER).is_ok(),
        "the correct passphrase must still be accepted"
    );
    let _ = std::fs::remove_file(&path);
}

/// **Damage is still detected.** `adopt` reads the salt from the descriptor rather than from a path, so a
/// corrupted salt must produce a failure and not a silently different root.
#[test]
fn adopt_still_detects_a_flipped_bit() {
    let path = make(40_000);
    // Flip a bit in the salt region, which the derive consumes.
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new().write(true).open(&path).expect("open rw");
        f.seek(SeekFrom::Start(1)).expect("seek");
        f.write_all(&[0x80]).expect("write");
    }
    // Either the salt changed (so the KDF differs and authentication fails) or the frame did. Both are a
    // refusal; what must not happen is a successful open of damaged bytes.
    let outcome = Wavefunction::adopt(DirectFile::open(&path).expect("descriptor"), PASS, ITER);
    assert!(
        outcome.is_err(),
        "damaged bytes opened cleanly, which means a flipped bit was not noticed"
    );
    let _ = std::fs::remove_file(&path);
}

/// **`open` is a delegate, not a second implementation.**
///
/// This is the structural guarantee. Every behavioural test above opens *one* of the two entry points and
/// checks the result, so a divergence — `open` growing an early-out, a different salt read, a differently
/// seeded ring — would leave them all green while the two entry points disagreed. Asserting the delegation
/// itself is the only check that catches it.
#[test]
fn open_is_a_delegate_to_adopt() {
    // Read the source. This is a source-level assertion and that is deliberate: the alternative is a
    // behavioural one, and a behavioural test cannot distinguish "open delegates" from "open happens to
    // agree today".
    let src = include_str!("../src/lib.rs");
    let open_at = src
        .find("pub fn open(")
        .expect("open exists");
    let adopt_at = src.find("pub fn adopt(").expect("adopt exists");
    let body_start = src[open_at..].find('{').map(|i| open_at + i).expect("open body");
    // The body is the delegate and nothing else: no second `DirectFile::open` call site, no salt read.
    let body_end = src[body_start..]
        .find("\n    }")
        .map(|i| body_start + i)
        .expect("open body end");
    let body = &src[body_start..body_end];
    assert!(
        body.contains("Self::adopt("),
        "`open` must delegate to `adopt`, or there are two implementations to keep in step. Body: {body}"
    );
    // **Exactly one** path open, and it is the one that produces the descriptor being handed over.
    //
    // The first version of this asserted `open`'s body contains *no* `DirectFile::open`, which is wrong:
    // opening the path is precisely `open`'s job, and it is what it delegates the result of. What would be
    // wrong is a second one, or any of the work `adopt` is supposed to own.
    assert_eq!(
        body.matches("DirectFile::open").count(),
        1,
        "`open`'s body must open the path exactly once -- the one it hands to adopt. Body: {body}"
    );
    for forbidden in ["SALT_OFFSET", "Ring::new", "Self::derive", "decode_frame_unchecked"] {
        assert!(
            !body.contains(forbidden),
            "`open`'s body does `{forbidden}` itself, so it is not a pure delegate and the two entry \
             points can drift. Body: {body}"
        );
    }
    assert!(adopt_at > 0, "adopt must exist");
}