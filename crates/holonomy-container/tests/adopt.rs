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
        // **`!is_empty()` and not `is_empty() == false`.** The property is "the error has a
        // message", and spelling it as an equality with `false` makes a reader check which way round
        // it goes. **The two halves are still both asserted**: the error is not an `Io`, and it is
        // not silent -- a wrong passphrase that says nothing is as much a failure as one that opens
        // the container.
        !matches!(err, holonomy_container::ContainerError::Io(_)) && !err.to_string().is_empty(),
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

/// **A container that asks for an absurd VDF count is refused, and refused quickly.**
///
/// # The attack
///
/// The VDF's iteration count `T` is written into a **cleartext** page beside the salt, precisely so
/// that a reader knows what the derivation will cost before it pays for it. **So the file chooses
/// the length of the computation.** Anyone who can write the file — anyone who can hand the user
/// a `.wavefunction` — can put `T = u64::MAX` there.
///
/// Before the ceiling, that was not an error. It was 584 billion years of Montgomery squarings:
/// the unlock never returned, the CPU sat at 100 %, and the document could not be opened at all.
/// **A file must not be able to hang the program that opens it.**
///
/// # What is asserted
///
/// The refusal, the *kind* of refusal (so a future "just clamp it" edit fails this test), and the
/// fact that it costs a 4 KiB read rather than a 128 MiB Argon2 allocation — the check happens
/// before the KDF, which is the whole point of checking it at the boundary.
#[test]
fn a_container_that_asks_for_an_absurd_vdf_count_is_refused() {
    let path = make(40_000);

    // **Rewrite the recorded count in place.** Offset 32, little-endian, eight bytes.
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open rw");
        f.seek(SeekFrom::Start(
            holonomy_container::layout::VDF_ITERATIONS_OFFSET,
        ))
        .expect("seek");
        f.write_all(&u64::MAX.to_le_bytes()).expect("write");
    }

    let started = std::time::Instant::now();
    let err = Wavefunction::adopt(DirectFile::open(&path).expect("descriptor"), PASS, ITER)
        .expect_err("an absurd iteration count must be refused, not obeyed");
    let elapsed = started.elapsed();

    assert!(
        matches!(
            err,
            holonomy_container::ContainerError::VdfIterationsTooLarge { .. }
        ),
        "expected the ceiling to be named, got {err:?}"
    );
    // **Fast.** The Argon2 stage alone is half a second and 128 MiB; a refusal that paid for it
    // would still be a denial of service, just a smaller one. A second is a generous ceiling for
    // "read one page and refuse".
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "the refusal took {elapsed:?}, so the KDF was paid before the count was checked"
    );
    let _ = std::fs::remove_file(&path);
}

/// **The same ceiling applies to a count the *caller* asks for.** A caller that passes
/// `u64::MAX` is asking for the same hang a hostile file would produce, and the check costs
/// nothing.
#[test]
fn create_refuses_an_absurd_vdf_count() {
    let path = scratch_dir().join(format!("vdf-ceiling-{}.wavefunction", unique()));
    let err = Wavefunction::create(&path, PASS, "t", b"content", u64::MAX)
        .expect_err("an absurd iteration count must be refused at create time");
    assert!(
        matches!(
            err,
            holonomy_container::ContainerError::VdfIterationsTooLarge { .. }
        ),
        "expected the ceiling to be named, got {err:?}"
    );
    let _ = std::fs::remove_file(&path);
}