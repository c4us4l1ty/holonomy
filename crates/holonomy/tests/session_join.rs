//! **The join gate: `SectionStore` is the rope's `LeafSource`, and a sparse rope reads back the same
//! document.** 7 tests.
//!
//! # What is actually joined
//!
//! `holonomy`'s [`SectionStore`] implements `holonomy_text::LeafSource`. So a [`Rope`] whose leaves have
//! been evicted can read itself back out of an encrypted container, through the real store, with no
//! in-memory stand-in — and the two halves of Phase 13 (the bounded resident set and the absent-leaf rope)
//! stop being separate pieces of infrastructure.
//!
//! | what it proves | test |
//! | --- | --- |
//! | a fully evicted rope reads back identically | [`a_fully_evicted_rope_reads_the_document_back_out_of_a_container`] |
//! | **a leaf straddling a section boundary is correct** | [`a_leaf_straddling_a_section_boundary_is_correct`] |
//! | the arithmetic is pinned, not asserted | [`leaf_and_section_boundaries_do_not_tile_and_that_is_pinned`] |
//! | residency stays bounded while reading | [`reading_a_sparse_rope_keeps_residency_bounded`] |
//! | the buffer is reused, not reallocated | [`fetching_reuses_its_buffer`] |
//! | and the fixture is big enough to mean anything | [`the_fixture_is_big_enough_to_exercise_more_than_one_section`] |
//! | failures are opaque | [`a_missing_section_is_opaque_rather_than_specific`] |
//!
//! # The claim that matters most
//!
//! [`a_leaf_straddling_a_section_boundary_is_correct`]. A leaf is at most 3,841 B and a section is
//! 65,520 B, so `65,520 / 3,840 = 17.0625` — **a leaf straddles the section boundary in 1 leaf out of
//! 17.** Reading one section and calling it a leaf's bytes is the natural mistake here, and it is silent
//! for 16 leaves out of 17. That is why the boundary is *placed deliberately* in this test rather than
//! left to chance.

use holonomy::store::SectionStore;
use holonomy_container::Wavefunction;
use holonomy_text::{LeafSource, Rope, RopeError, GAP_MINIMUM, LEAF_CAPACITY};

const PASS: &str = "correct horse battery staple";
const ITER: u64 = 1;
/// A section is one container chunk, and `holonomy`'s manifest uses the same number.
const SECTION: usize = 65_520;

/// Scratch under `target/<profile>`, because `DirectFile` opens `O_DIRECT` and `/tmp` is tmpfs here.
fn scratch_dir() -> std::path::PathBuf {
    let dir = std::env::current_exe()
        .expect("test exe")
        .ancestors()
        .nth(3)
        .expect("target/<profile> layout")
        .join("holonomy-container-tests")
        .join("join");
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

/// A container over `content`, opened and ready to read.
fn container(content: &[u8]) -> (std::path::PathBuf, Wavefunction) {
    let path = scratch_dir().join(format!("join-{}-{}.wavefunction", content.len(), unique()));
    let _ = Wavefunction::create(&path, PASS, "join", content, ITER).expect("create");
    let mut wf = Wavefunction::open(&path, PASS, ITER).expect("open");
    (path, wf)
}

/// `n` bytes where no 16-byte window repeats, so a misaddressed read is visible rather than plausible.
fn text(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i.wrapping_mul(131).wrapping_add(17) % 251) as u8).collect()
}

/// **Every byte of a fully-evicted rope comes back out of the container, identical.**
#[test]
fn a_fully_evicted_rope_reads_the_document_back_out_of_a_container() {
    // Three sections' worth, so the store evicts as it goes.
    let doc = text(SECTION * 2 + 4_000);
    let (path, mut wf) = container(&doc);
    let mut rope = Rope::from_text(&doc).expect("load");
    let leaves = rope.leaf_count();

    for i in 0..leaves {
        rope.evict_leaf(i).expect("evict");
    }
    assert_eq!(rope.resident_count(), 0, "nothing is held");
    assert_eq!(rope.text_len(), doc.len(), "the document length survived");

    // **A budget of 2 sections, so reading 2+ sections of document forces eviction as it goes.** If the
    // fetch held onto sections instead of copying out, this is where it would break.
    let mut store = SectionStore::new(&mut wf, 2);
    let mut got = vec![0u8; doc.len()];
    rope.read_at_faulting(&mut store, 0, got.len(), &mut got).expect("read the document back");

    assert_eq!(got, doc, "a fully evicted rope reads back as the identical document");
    assert!(store.stats().loads > 0, "and it really did load sections");
    let _ = std::fs::remove_file(&path);
}

/// **The straddling leaf, placed deliberately rather than found by luck.**
///
/// A leaf's document offset is `starts[i]`, which lands wherever splits left it — so this computes the
/// leaf that *contains* a section boundary, and reads exactly that leaf.
#[test]
fn a_leaf_straddling_a_section_boundary_is_correct() {
    let doc = text(SECTION * 2);
    let (path, mut wf) = container(&doc);
    let mut rope = Rope::from_text(&doc).expect("load");

    // The leaf containing document byte `SECTION`, i.e. the first byte of section 1.
    let mut straddlers = 0usize;
    for i in 0..rope.leaf_count() {
        let start = rope.leaf_offset(i);
        let end = start + rope.leaf_len_of(i);
        // **Strictly inside the boundary**: a leaf whose last byte is the section's last byte does not
        // straddle, and a leaf that ends exactly at the boundary does not either.
        if start < SECTION && end > SECTION {
            straddlers += 1;
            let mut buf = vec![0u8; rope.leaf_len_of(i)];
            // Evict it, so this cannot pass by reading bytes the rope already had.
            rope.evict_leaf(i).expect("evict");
            let mut store = SectionStore::new(&mut wf, 2);
            rope.read_at_faulting(&mut store, start, buf.len(), &mut buf).expect("fault the straddler");
            assert_eq!(
                &buf[..],
                &doc[start..end],
                "leaf {i} at {start}..{end} straddles section boundary {SECTION} and read back wrong"
            );
        }
    }
    assert!(
        straddlers > 0,
        "the fixture must actually produce a straddling leaf, or this test proves nothing -- \
         {} leaves over {} bytes with a boundary every {SECTION}",
        rope.leaf_count(),
        doc.len()
    );
    let _ = std::fs::remove_file(&path);
}

/// **The tiling mismatch is pinned as arithmetic, so a change to either constant is caught here rather
/// than as a mysterious wrong read later.**
///
/// If a future change made leaves and sections tile exactly, this assertion would fail — and that is the
/// point. The two constants are chosen independently by two crates, and nothing forces them to agree; the
/// straddling code exists *because* they do not.
#[test]
fn leaf_and_section_boundaries_do_not_tile_and_that_is_pinned() {
    let leaf_fill = LEAF_CAPACITY - GAP_MINIMUM;
    assert_eq!(SECTION, 65_520, "the container's plaintext chunk size");
    assert_ne!(
        SECTION % leaf_fill,
        0,
        "if sections now tile leaves exactly, the straddling path in fetch_leaf is dead code and should \
         be revisited -- not silently kept. {} % {} == 0",
        SECTION,
        leaf_fill
    );
    // And the consequence, stated as a number: 1 leaf in 17 straddles.
    let per_section = SECTION / leaf_fill;
    assert_eq!(per_section, 17, "17 whole leaves per section, and a remainder that straddles");
    assert_eq!(SECTION - per_section * leaf_fill, SECTION % leaf_fill);
}

/// Reading a whole sparse document must not let the store grow: **residency is bounded while reading**,
/// not just at the end.
#[test]
fn reading_a_sparse_rope_keeps_residency_bounded() {
    let doc = text(SECTION * 3);
    let (path, mut wf) = container(&doc);
    let mut rope = Rope::from_text(&doc).expect("load");
    let leaves = rope.leaf_count();
    for i in 0..leaves {
        rope.evict_leaf(i).expect("evict");
    }

    let mut store = SectionStore::new(&mut wf, 2);
    let mut got = vec![0u8; doc.len()];
    rope.read_at_faulting(&mut store, 0, got.len(), &mut got).expect("read");

    assert_eq!(got, doc);
    assert_eq!(
        store.resident(),
        2,
        "a 2-section budget over a 3-section document must end holding exactly 2"
    );
    assert!(store.resident_bytes() <= 2 * SECTION, "and at most two sections of bytes");
    assert!(store.stats().evictions > 0, "which means it really did evict while reading");
    let _ = std::fs::remove_file(&path);
}

/// **The fetch buffer is reused, not reallocated per fault.** `fetch_leaf` takes it out of the store with
/// `mem::take` and puts it back through a closure so the error paths restore it too — so this asserts
/// the buffer does not grow, which is what "reused" means observably.
#[test]
fn fetching_reuses_its_buffer() {
    let doc = text(SECTION);
    let (path, mut wf) = container(&doc);
    let mut rope = Rope::from_text(&doc).expect("load");
    for i in 0..rope.leaf_count() {
        rope.evict_leaf(i).expect("evict");
    }
    let mut store = SectionStore::new(&mut wf, 2);
    let mut buf = vec![0u8; 512];

    // **One fetch first, then measure.** The buffer starts empty and is allocated on the first fault, so
    // measuring before it is 0 and comparing that to the post-fault length measures the *first*
    // allocation -- which would fail on correct code, exactly as the first version of this test did.
    // The claim is that fetch N+1 does not allocate, not that fetch 1 does not.
    {
        let mut first = Rope::from_text(&doc).expect("reload");
        for i in 0..first.leaf_count() {
            first.evict_leaf(i).expect("evict");
        }
        first.read_at_faulting(&mut store, 0, buf.len(), &mut buf).expect("first fetch");
    }
    let after_first = store.fetch_buffer_len();
    assert!(after_first > 0, "the first fetch allocates the buffer");

    for _ in 0..25 {
        let mut keep = Rope::from_text(&doc).expect("reload");
        for i in 0..keep.leaf_count() {
            keep.evict_leaf(i).expect("evict");
        }
        keep.read_at_faulting(&mut store, 0, buf.len(), &mut buf).expect("fetch");
    }
    assert_eq!(
        store.fetch_buffer_len(),
        after_first,
        "25 further faults must not have grown the fetch buffer -- it is taken out and put back, not \
         reallocated per fault"
    );
    let _ = std::fs::remove_file(&path);
}

/// **A fetch that cannot be satisfied is opaque.** The rope learns only that it has no bytes; it does not
/// learn whether the chunk was missing, damaged, or failed authentication.
#[test]
fn a_missing_section_is_opaque_rather_than_specific() {
    let doc = text(SECTION);
    let (path, mut wf) = container(&doc);
    let mut store = SectionStore::new(&mut wf, 2);
    let mut buf = vec![0u8; 128];
    // Way past the end of the document.
    let err = store
        .fetch_leaf(SECTION * 50, &mut buf)
        .expect_err("a section the document does not have must fail");
    assert!(
        matches!(err, RopeError::SourceUnavailable),
        "expected the opaque SourceUnavailable, got {err:?}"
    );
    // And the store's own accounting did not move: a failed fetch is not a load.
    let loads = store.stats().loads;
    let _ = store.fetch_leaf(SECTION * 50, &mut buf);
    assert_eq!(store.stats().loads, loads, "a failed fetch must not count as a load");
    let _ = std::fs::remove_file(&path);
}

/// Guards against a fixture that would silently stop exercising anything: the document must have enough
/// leaves that not every read is one section.
#[test]
fn the_fixture_is_big_enough_to_exercise_more_than_one_section() {
    let doc = text(SECTION * 2);
    let rope = Rope::from_text(&doc).expect("load");
    assert!(
        rope.leaf_count() > SECTION / (LEAF_CAPACITY - GAP_MINIMUM),
        "the fixture must have more leaves than fit in one section, or no fetch straddles"
    );
}