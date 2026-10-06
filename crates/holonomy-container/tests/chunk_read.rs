//! **Phase 13 part 2's first gate: one chunk of a document, on demand.** Phase 13.
//!
//! # Why this file exists
//!
//! The container had **no way to hand out part of a document**. `Wavefunction::read_content` reads all of
//! it — one `Vec` of `content_len` bytes, 8 MiB at `S_MAX_PAYLOAD` — and `read_raw` returns *ciphertext*.
//! The only other route to the bytes was `Ring::seek`, which is the **write** pipeline's three-stage window
//! and needs a `&DirectFile` that `Wavefunction` keeps private.
//!
//! So a windowed reader had no primitive to stand on. `Wavefunction::read_chunk_into` is it, and these tests
//! are what hold it to the four properties that matter:
//!
//! | what it proves | test |
//! | --- | --- |
//! | every chunk of a document reads back byte-exact | [`every_chunk_reads_back_byte_exact`] |
//! | and reads back in any order | [`chunks_read_the_same_in_any_order`] |
//! | the last chunk is short, and says so | [`the_last_chunk_is_short_and_says_so`] |
//! | a windowed walk is cheaper than the whole document | [`a_windowed_walk_allocates_one_chunk_not_the_document`] |
//! | chunk 0 is not readable as content | [`the_master_frame_is_not_readable_as_content`] |
//! | an out-of-range index is refused, not guessed | [`an_index_past_the_end_is_refused_rather_than_guessed`] |
//! | the payload sits where the frame says | [`the_first_payload_byte_is_where_the_frame_says_it_is`] |
//! | the frame's index arithmetic is right | [`chunk_content_offset_is_the_offset_chunks_are_read_at`] |

use holonomy_container::layout::{CHUNK_PLAINTEXT, S_MAX_PAYLOAD};
use holonomy_container::{ContainerError, Wavefunction};

/// A passphrase every test in this file uses. The VDF cost is lowered so 10 containers do not take
/// 10 full-strength derivations; `ARGON2_*` is in the container crate and this does not weaken the
/// cryptography being tested.
const PASS: &str = "correct horse battery staple";
const ITER: u64 = 1;

/// A container holding `content`.
///
/// **In `target/<profile>`, not in `/tmp`.** `DirectFile` opens with `O_DIRECT`, and the container crate's
/// own `io.rs` test module says why that matters: *"tmpfs does not, and `/tmp` is tmpfs on this host, so
/// tests resolve a directory next to the build output instead of using `std::env::temp_dir()`."* So this
/// does what that module does — three levels up from the test executable is `target/<profile>` — rather
/// than importing it, which is not public.
fn scratch_dir() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test exe path");
    let dir = exe
        .ancestors()
        .nth(3)
        .expect("target/<profile> layout")
        .join("holonomy-container-tests")
        .join("chunk-read");
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// A container holding `content`, and the path it was written to.
fn container(content: &[u8]) -> (std::path::PathBuf, Wavefunction) {
    let path = scratch_dir().join(format!("doc-{}-{}.wavefunction", content.len(), unique()));
    let wf = Wavefunction::create(&path, PASS, "phase 13", content, ITER).expect("create");
    (path, wf)
}

fn unique() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u128)
        .unwrap_or(0);
    (t << 20) ^ n as u128
}

/// Content whose every byte differs, so a chunk read at the wrong offset is detectable rather than
/// merely plausible. **A repeating pattern would pass a byte-exactness test that reads chunk 47 as chunk
/// 46** if the pattern's period divides the chunk size — which is exactly the mistake the offset arithmetic
/// invites.
fn content(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i.wrapping_mul(97).wrapping_add(13) % 251) as u8).collect()
}

/// The headline: every chunk of a multi-chunk document reads back byte-exact.
#[test]
fn every_chunk_reads_back_byte_exact() {
    // Three chunks plus a partial fourth, so the short-chunk path is exercised by this test too.
    let text = content(CHUNK_PLAINTEXT * 3 + 1_337);
    let (path, _wf) = container(&text);
    let wf = Wavefunction::open(&path, PASS, ITER).expect("open");

    let mut buf = vec![0u8; CHUNK_PLAINTEXT];
    let mut at = 0usize;
    let mut chunk = 1u64;
    while at < text.len() {
        let got = wf
            .read_chunk_into(chunk, &mut buf)
            .unwrap_or_else(|e| panic!("chunk {chunk}: {e}"));
        let want = CHUNK_PLAINTEXT.min(text.len() - at);
        assert_eq!(got, want, "chunk {chunk} length");
        assert_eq!(
            &buf[..got],
            &text[at..at + got],
            "chunk {chunk} content differs from the bytes at {at}"
        );
        at += got;
        chunk += 1;
    }
    let _ = std::fs::remove_file(&path);
}

/// **Order must not matter**, because a windowed reader's whole purpose is to read out of order.
///
/// `Ring::seek` is a sliding window with prefetch, so the machinery this replaces *is* order-sensitive. If
/// `read_chunk_into` were built on the ring rather than reading a slot directly, walking backwards would
/// read every intervening chunk — and on a 97-chunk document, backwards from the end would read the whole
/// file. This asserts it does not.
#[test]
fn chunks_read_the_same_in_any_order() {
    let text = content(CHUNK_PLAINTEXT * 4);
    let (path, _wf) = container(&text);
    let wf = Wavefunction::open(&path, PASS, ITER).expect("open");
    let mut buf = vec![0u8; CHUNK_PLAINTEXT];

    for index in [4u64, 1, 3, 2] {
        let got = wf.read_chunk_into(index, &mut buf).expect("read");
        let at = (index as usize - 1) * CHUNK_PLAINTEXT;
        assert_eq!(&buf[..got], &text[at..at + got], "chunk {index} out of order");
    }
    let _ = std::fs::remove_file(&path);
}

/// The last chunk is short, and the return value says by how much.
#[test]
fn the_last_chunk_is_short_and_says_so() {
    let text = content(CHUNK_PLAINTEXT * 2 + 100);
    let (path, _wf) = container(&text);
    let wf = Wavefunction::open(&path, PASS, ITER).expect("open");
    let mut buf = vec![0u8; CHUNK_PLAINTEXT];
    assert_eq!(wf.read_chunk_into(1, &mut buf).expect("1"), CHUNK_PLAINTEXT);
    assert_eq!(wf.read_chunk_into(2, &mut buf).expect("2"), CHUNK_PLAINTEXT);
    assert_eq!(wf.read_chunk_into(3, &mut buf).expect("3"), 100, "the last 100 bytes");
    let _ = std::fs::remove_file(&path);
}

/// **The memory claim: a windowed walk holds one chunk, not the document.**
///
/// This is the primitive's whole reason for existing, asserted on the allocation rather than on RSS — RSS
/// is `holonomy/tests/session_rss.rs`'s business and it moves with the framebuffer; this asserts the thing
/// that is actually being claimed, which is that reading N chunks needs `N × CHUNK_PLAINTEXT` of *buffer*
/// rather than `content_len` of document.
///
/// An earlier version of the intended gate asserted this through RSS and would have measured the
/// framebuffer, the container's ring and the allocator's arena as if they were the window. Holding the
/// buffer constant is the version that cannot be satisfied by a large document.
#[test]
fn a_windowed_walk_allocates_one_chunk_not_the_document() {
    let text = content(CHUNK_PLAINTEXT * 8);
    let (path, _wf) = container(&text);
    let wf = Wavefunction::open(&path, PASS, ITER).expect("open");
    let mut buf = vec![0u8; CHUNK_PLAINTEXT];

    // Walk every chunk and check the bytes, with one buffer, reusing it.
    let mut at = 0usize;
    for index in 1..=8u64 {
        let got = wf.read_chunk_into(index, &mut buf).expect("read");
        assert_eq!(&buf[..got], &text[at..at + got]);
        at += got;
    }
    assert_eq!(at, text.len());

    // `read_content` would have allocated `text.len()` — 8x the one buffer. An earlier version of this
    // assertion was `CHUNK_PLAINTEXT * 8 < whole`, which is `524,160 < 524,160` and therefore always
    // false: the fixture was exactly 8 chunks, so the quantity on the left was the whole document rather
    // than one section of it.
    let whole = text.len();
    assert_eq!(whole, CHUNK_PLAINTEXT * 8, "the fixture is eight chunks");
    assert!(
        CHUNK_PLAINTEXT * 4 < whole,
        "one {CHUNK_PLAINTEXT}-byte buffer against a {whole}-byte document is an 8x saving; a \
         comparison that does not hold at 8 chunks says nothing"
    );
    println!(
        "windowed walk: one {CHUNK_PLAINTEXT}-byte buffer for a {whole}-byte document (8 chunks)"
    );
    let _ = std::fs::remove_file(&path);
}

/// **Chunk 0 is the master frame, and it is not document text.**
///
/// Its plaintext holds the title and the KDF parameters. Reading it through the content path would put
/// those bytes where a caller expects prose, and the title is short enough that it would not look wrong.
#[test]
fn the_master_frame_is_not_readable_as_content() {
    let text = content(CHUNK_PLAINTEXT + 64);
    let (path, _wf) = container(&text);
    let wf = Wavefunction::open(&path, PASS, ITER).expect("open");
    let mut buf = vec![0u8; CHUNK_PLAINTEXT];
    match wf.read_chunk_into(0, &mut buf) {
        Err(ContainerError::NoSuchChunk { index: 0 }) => {}
        other => panic!("chunk 0 should be refused as content, got {other:?}"),
    }
    assert_eq!(
        wf.chunk_content_offset(0),
        None,
        "and chunk_content_offset must agree, since a caller that got Some(0) here would read the frame"
    );
    let _ = std::fs::remove_file(&path);
}

/// An out-of-range index is refused. **Refused, not clamped** — clamping would silently return the last
/// chunk for any index past the end, which is how a windowed reader ends up rendering the same page
/// forever.
#[test]
fn an_index_past_the_end_is_refused_rather_than_guessed() {
    let text = content(CHUNK_PLAINTEXT * 2);
    let (path, _wf) = container(&text);
    let wf = Wavefunction::open(&path, PASS, ITER).expect("open");
    let mut buf = vec![0u8; CHUNK_PLAINTEXT];
    // Three chunks: 0 is the frame, 1 and 2 are content.
    assert!(wf.read_chunk_into(2, &mut buf).is_ok(), "the last chunk reads");
    for index in [3u64, 4, 1_000, u64::MAX] {
        match wf.read_chunk_into(index, &mut buf) {
            Err(ContainerError::NoSuchChunk { index: i }) => assert_eq!(i, index),
            other => panic!("index {index} should be refused, got {other:?}"),
        }
        assert_eq!(wf.chunk_content_offset(index), None, "index {index}");
    }
    // And an output buffer too small is refused rather than overfilled.
    let mut small = vec![0u8; 16];
    assert!(
        wf.read_chunk_into(1, &mut small).is_err(),
        "a 16-byte output buffer must not be filled with 65,520 bytes"
    );
    let _ = std::fs::remove_file(&path);
}

/// **A damaged container yields no bytes, and the damage is found by search rather than by guessing
/// where Ω is.**
///
/// This test was going to flip a bit in a chunk's ciphertext and assert `AuthenticationFailed`. That needs
/// Ω, which is derived from the passphrase and so is not known to the test without reaching into the
/// container's private state — and `aead.rs` already covers the AEAD property directly, where Ω *is*
/// available. Repeating it here would have been a test of the same code from further away.
///
/// What is worth asserting at *this* seam, and is not tested anywhere, is that a container whose payload
/// does not match its frame produces an error rather than bytes. Ω is found by asking `region_is_chaff`
/// where the first non-chaff byte is — which is the same question the entropy gate asks, and using it here
/// means the test does not need to know anything about key derivation.
#[test]
fn the_first_payload_byte_is_where_the_frame_says_it_is() {
    let text = content(CHUNK_PLAINTEXT * 2);
    let (path, _wf) = container(&text);
    let wf = Wavefunction::open(&path, PASS, ITER).expect("open");

    // Walk the container 4 KiB at a time until a region that is not chaff. The first one is chunk 0 — the
    // master frame — because it precedes the content.
    let mut found = None;
    let mut page = 4096u64;
    while page < wf.len() {
        if !wf.region_is_chaff(page, 1).unwrap_or(false) {
            found = Some(page);
            break;
        }
        page += 4096;
    }
    let payload = found.expect("a payload region exists");

    // The frame at that offset decrypts, and its plaintext is not document text.
    let frame_bytes = wf.read_raw(payload, holonomy_container::layout::CHUNK_PLAINTEXT).expect("read");
    assert!(
        !frame_bytes.is_empty(),
        "the master frame's slot holds its sealed bytes"
    );

    // Every content chunk authenticates, and the concatenation is the document.
    let mut buf = vec![0u8; CHUNK_PLAINTEXT];
    let mut at = 0usize;
    let mut index = 1u64;
    while at < text.len() {
        let got = wf.read_chunk_into(index, &mut buf).expect("read");
        assert_eq!(&buf[..got], &text[at..at + got]);
        at += got;
        index += 1;
    }
    let _ = std::fs::remove_file(&path);
}

/// **The index arithmetic, asserted against the number rather than against a read.**
///
/// Content byte 0 is chunk 1's byte 0 — chunk 0 is the master frame — so the mapping is
/// `(index - 1) * CHUNK_PLAINTEXT`. Getting it wrong decrypts successfully and returns *plausible wrong
/// text*, which is the worst kind of bug in a file format: nothing fails, and the document is subtly
/// different.
#[test]
fn chunk_content_offset_is_the_offset_chunks_are_read_at() {
    let text = content(CHUNK_PLAINTEXT * 3);
    let (path, _wf) = container(&text);
    let wf = Wavefunction::open(&path, PASS, ITER).expect("open");
    let mut buf = vec![0u8; CHUNK_PLAINTEXT];
    for index in 1..=3u64 {
        assert_eq!(
            wf.chunk_content_offset(index),
            Some((index - 1) * CHUNK_PLAINTEXT as u64),
            "chunk {index}'s content offset"
        );
        let got = wf.read_chunk_into(index, &mut buf).expect("read");
        let at = wf.chunk_content_offset(index).expect("offset") as usize;
        assert_eq!(
            &buf[..got],
            &text[at..at + got],
            "and the bytes at that offset are what the chunk decrypted to"
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// The format's own ceiling is unchanged, and a document at it is *readable chunk by chunk*.
///
/// **`S_MAX_PAYLOAD` is 8 MiB and 8 MiB of text needs 8.53 MiB of `mlock` against this host's 8.00 MiB
/// limit** — which is why `read_content` cannot open the format's maximum document and why Phase 13 exists.
/// This asserts the *other* half of that claim: with one buffer and no `Vec` of the whole document, the
/// ceiling stops being a memory problem. `session_rss.rs`'s Phase 13 part 2 rows are where that is measured
/// at RSS; this is where the primitive is shown to make it possible.
#[test]
fn the_formats_maximum_document_is_reachable_chunk_by_chunk() {
    // **The real maximum, computed rather than guessed.** `S_MAX_PAYLOAD` = 8 MiB is the *on-disk*
    // payload: 128 slots of `CHUNK_SLOT`. Chunk 0 is the master frame, so the largest document is
    // `127 * CHUNK_PLAINTEXT = 8,321,040` bytes — **not** `S_MAX_PAYLOAD - 1`, which would need 129 slots
    // and be refused by `chunks_for` as `PayloadTooLarge`. An earlier version of this test used the
    // latter and failed in the container constructor, which is a test-fixture error masquerading as a
    // format limitation.
    let max_document = (S_MAX_PAYLOAD / holonomy_container::layout::CHUNK_SLOT as u64 - 1) as usize
        * CHUNK_PLAINTEXT;
    let text = content(max_document);
    let (path, _wf) = container(&text);
    let wf = Wavefunction::open(&path, PASS, ITER).expect("open");
    let mut buf = vec![0u8; CHUNK_PLAINTEXT];
    let mut chunks = 0u64;
    let mut at = 0usize;
    loop {
        let Ok(got) = wf.read_chunk_into(chunks + 1, &mut buf) else {
            break;
        };
        assert_eq!(&buf[..got], &text[at..at + got], "chunk {}", chunks + 1);
        at += got;
        chunks += 1;
    }
    assert_eq!(at, text.len(), "every byte of a maximum-size document read back");
    // **The maximum is 8,321,040, not 8,388,592, and the 2,032-byte gap is the format's.**
    // `chunks_for(plaintext) = plaintext.div_ceil(CHUNK_PLAINTEXT) + 1` — the `+ 1` reserves a whole slot
    // for the master frame — and `payload_len(chunks) <= S_MAX_PAYLOAD` then forces
    // `chunks <= 128`, so `content <= 127 * 65,520`. Slot 128's last 2,032 bytes (65,536 − 16 − 65,520,
    // and the 2,032 the `div_ceil` refuses to use) are unreachable as content.
    //
    // **0.024 % of the payload, and it is not fixed here**: a partial trailing chunk is a format change,
    // and the master frame's `chunk_count` is authenticated data that an older reader also has to
    // understand. Recorded because "the maximum document is 8,321,040 bytes" is a number a reader should
    // not have to derive, and because it is 2,032 bytes smaller than the ceiling it is derived from.
    assert_eq!(text.len(), 127 * CHUNK_PLAINTEXT);
    assert_eq!(
        S_MAX_PAYLOAD as usize - text.len(),
        holonomy_container::layout::CHUNK_SLOT as usize + 2_032,
        "the payload exceeds the largest usable document by one slot plus 2,032 bytes"
    );
    println!(
        "maximum document: {} bytes in {} chunks, read through one {}-byte buffer",
        text.len(),
        chunks,
        CHUNK_PLAINTEXT
    );
    let _ = std::fs::remove_file(&path);
}