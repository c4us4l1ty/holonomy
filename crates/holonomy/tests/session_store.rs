//! **Phase 13's item 3 gate: resident text is bounded, and eviction releases memory.** 7 tests.
//!
//! # The claim
//!
//! A store over a document many times its budget reads correct bytes forever, **never holds more than its
//! budget**, and **every eviction actually released memory** rather than merely dropping a struct.
//!
//! | what it proves | test |
//! | --- | --- |
//! | the bound holds *at every step*, not at the end | [`the_resident_set_never_exceeds_its_budget`] |
//! | a document far larger than the budget is fully readable | [`a_document_far_larger_than_the_budget_reads_correctly`] |
//! | eviction releases memory, counted | [`every_eviction_released_memory`] |
//! | and the victim is the least recently used, by name | [`the_victim_is_the_least_recently_used_section`] |
//! | a hit is not a reload | [`a_section_already_resident_is_not_read_again`] |
//! | a budget of 0 is a legal configuration | [`a_budget_of_zero_holds_nothing_and_still_returns_bytes`] |
//! | `evict_all` is observable at a point | [`evict_all_empties_the_store_immediately`] |

use holonomy::manifest::SECTION_BYTES;
use holonomy::store::SectionStore;
use holonomy_container::Wavefunction;

const PASS: &str = "correct horse battery staple";
const ITER: u64 = 1;

/// Scratch under `target/<profile>`, because `DirectFile` opens `O_DIRECT` and `/tmp` is tmpfs here.
fn scratch_dir() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test exe path");
    let dir = exe
        .ancestors()
        .nth(3)
        .expect("target/<profile> layout")
        .join("holonomy-container-tests")
        .join("section-store");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn unique() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    /// A process-wide counter, so two tests creating containers in the same nanosecond do not collide on
    /// a path. The timestamp is the outer term because two runs of the suite in one process is the case;
    /// the counter is the inner one because the tests are short.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u128)
        .unwrap_or(0);
    (t << 20) ^ u128::from(COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// A container over `content`, whose every byte differs so a wrong chunk is detectable.
fn container(content: &[u8]) -> (std::path::PathBuf, Wavefunction) {
    let path = scratch_dir().join(format!("doc-{}-{}.wavefunction", content.len(), unique()));
    let wf = Wavefunction::create(&path, PASS, "phase 13", content, ITER).expect("create");
    (path, wf)
}

fn content(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i.wrapping_mul(97).wrapping_add(13) % 251) as u8).collect()
}

/// Read section `s` and assert it is the right bytes.
fn read(store: &mut SectionStore<'_>, s: u32, text: &[u8]) {
    let mut buf = vec![0u8; SECTION_BYTES];
    let got = store.copy_into(s, &mut buf).expect("read section");
    let at = s as usize * SECTION_BYTES;
    let want = SECTION_BYTES.min(text.len().saturating_sub(at));
    assert_eq!(got, want, "section {s} length");
    assert_eq!(
        &buf[..got],
        &text[at..at + got],
        "section {s} content differs from the bytes at {at}"
    );
}

/// **The bound holds at every step, not at the end.**
///
/// **This is the assertion that a store is a bound and not a hint, and the way to make it fail is to check
/// it in the wrong place.** Checking `resident() <= budget` after the loop passes for any store that
/// happens to be under budget when the loop ends — including one that peaked at `budget + 1` and came back
/// down. So the check is *inside* the loop, after every access, and the loop walks far more sections than
/// the budget so a peak is inevitable if one exists.
#[test]
fn the_resident_set_never_exceeds_its_budget() {
    let text = content(SECTION_BYTES * 40);
    let (path, _wf) = container(&text);
    let mut wf = Wavefunction::open(&path, PASS, ITER).expect("open");

    for budget in [1usize, 2, 5, 8, 39, 40, 64] {
        let mut store = SectionStore::new(&mut wf, budget);
        let mut buf = vec![0u8; SECTION_BYTES];
        for s in 0..40u32 {
            store.copy_into(s, &mut buf).expect("read");
            assert!(
                store.resident() <= budget,
                "budget {budget}: after reading section {s} the store holds {} sections",
                store.resident()
            );
        }
        assert_eq!(
            store.resident_bytes() <= budget * SECTION_BYTES,
            true,
            "and resident *bytes* are bounded too, not just the count"
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// **A document many times the budget reads correctly.** The whole point: 40 sections through a 5-section
/// budget, every one byte-exact.
#[test]
fn a_document_far_larger_than_the_budget_reads_correctly() {
    let text = content(SECTION_BYTES * 40 + 777);
    let (path, _wf) = container(&text);
    let mut wf = Wavefunction::open(&path, PASS, ITER).expect("open");
    let mut store = SectionStore::new(&mut wf, 5);

    for s in 0..=40u32 {
        read(&mut store, s, &text);
        assert!(store.resident() <= 5, "section {s}");
    }
    let stats = store.stats();
    assert_eq!(stats.loads, 41, "every section was a miss: nothing fits in a 5-section budget");
    assert_eq!(
        stats.evictions, 36,
        "and 36 were evicted -- 41 loads into 5 slots is exactly 36 evictions, which is the arithmetic \\
         that says the bound held"
    );
    let _ = std::fs::remove_file(&path);
}

/// **Eviction released memory, counted rather than assumed.**
///
/// `zeroize_and_release` returns the bytes it scrubbed and unmapped, so the sum over every eviction is
/// measurable — and it must equal the bytes of every section that left. **An earlier framing of this test
/// wanted to read the pages back and assert zeros, and that is not possible**: `munmap` has already
/// unmapped them, so the addresses are no longer ours to read. Asserting it anyway would have been
/// asserting a property of memory the process no longer owns. The zeroing itself is `SecureBlock`'s, gated
/// in `crates/holonomy-secure/tests/`; what this holds down is that eviction *calls* it.
#[test]
fn every_eviction_released_memory() {
    let text = content(SECTION_BYTES * 12);
    let (path, _wf) = container(&text);
    let mut wf = Wavefunction::open(&path, PASS, ITER).expect("open");
    let mut store = SectionStore::new(&mut wf, 4);

    let mut buf = vec![0u8; SECTION_BYTES];
    for s in 0..12u32 {
        store.copy_into(s, &mut buf).expect("read");
    }
    let stats = store.stats();
    assert_eq!(stats.evictions, 8, "12 loads into 4 slots is 8 evictions");
    assert!(
        stats.released_bytes >= stats.evictions as u64 * SECTION_BYTES as u64,
        "each eviction released at least a section: {} bytes over {} evictions",
        stats.released_bytes,
        stats.evictions
    );
    // And the survivors still hold real bytes, so the store is not holding zeroed blocks.
    assert_eq!(
        store.resident_bytes(),
        4 * SECTION_BYTES,
        "four full sections are still resident, so the released count is not the whole story"
    );
    let _ = std::fs::remove_file(&path);
}

/// **The victim is the least recently used, named rather than counted.**
///
/// "Something was evicted" passes for any LRU, FIFO, or random policy. The name is the assertion: load
/// 0..4 with a 4-slot budget, touch section 0 so it is the newest, then load 5 — and section 1 must be the
/// one to go, because 1, 2, 3, 4 are older than the just-touched 0 and 1 is the oldest of those.
#[test]
fn the_victim_is_the_least_recently_used_section() {
    let text = content(SECTION_BYTES * 8);
    let (path, _wf) = container(&text);
    let mut wf = Wavefunction::open(&path, PASS, ITER).expect("open");
    let mut store = SectionStore::new(&mut wf, 4);
    let mut buf = vec![0u8; SECTION_BYTES];

    for s in 0..4u32 {
        store.copy_into(s, &mut buf).expect("fill");
    }
    assert_eq!(store.resident(), 4);
    // Touch section 0 so it is the most recent, then section 2.
    store.copy_into(0, &mut buf).expect("touch 0");
    store.copy_into(2, &mut buf).expect("touch 2");

    store.copy_into(4, &mut buf).expect("evict one");
    assert_eq!(store.stats().evictions, 1);
    assert_eq!(store.resident(), 4, "still full -- one in, one out");

    // Section 1 was the oldest and is gone. Re-reading it must cost a load.
    let before = store.stats().loads;
    store.copy_into(1, &mut buf).expect("reload 1");
    assert_eq!(
        store.stats().loads,
        before + 1,
        "section 1 was evicted, so reading it again must be a miss. If it were still resident the \\
         policy would have evicted a newer section, and 'one eviction happened' would not have caught it"
    );
    // And section 0, which was touched most recently, was not the victim.
    let before = store.stats().loads;
    store.copy_into(0, &mut buf).expect("reread 0");
    assert_eq!(store.stats().loads, before, "section 0 was the newest and must still be resident");
    let _ = std::fs::remove_file(&path);
}

/// A hit is a hit. **Asserted on the load count rather than on timing**, because a timing assertion on a
/// 65,520-byte read would be measuring the page cache.
#[test]
fn a_section_already_resident_is_not_read_again() {
    let text = content(SECTION_BYTES * 3);
    let (path, _wf) = container(&text);
    let mut wf = Wavefunction::open(&path, PASS, ITER).expect("open");
    let mut store = SectionStore::new(&mut wf, 8);
    let mut buf = vec![0u8; SECTION_BYTES];

    store.copy_into(1, &mut buf).expect("first");
    let loads = store.stats().loads;
    for _ in 0..50 {
        store.copy_into(1, &mut buf).expect("repeat");
    }
    assert_eq!(store.stats().loads, loads, "50 rereads of a resident section, 0 loads");
    assert_eq!(store.stats().hits, 50, "and 50 hits");
    let _ = std::fs::remove_file(&path);
}

/// **A budget of 0 is a legal configuration**, and it is the one that makes "the store holds nothing" a
/// claim rather than an accident of arithmetic.
///
/// An earlier version clamped 0 to 1, which is friendlier and dishonest: it makes the degenerate case
/// unreachable, and the degenerate case is exactly what a caller under memory pressure wants to be able to
/// ask for.
#[test]
fn a_budget_of_zero_holds_nothing_and_still_returns_bytes() {
    let text = content(SECTION_BYTES * 2);
    let (path, _wf) = container(&text);
    let mut wf = Wavefunction::open(&path, PASS, ITER).expect("open");
    let mut store = SectionStore::new(&mut wf, 0);
    let mut buf = vec![0u8; SECTION_BYTES];

    read(&mut store, 0, &text);
    read(&mut store, 1, &text);
    assert_eq!(store.resident(), 0, "a zero budget holds nothing, at any point");
    assert_eq!(store.stats().evictions, 0, "and there is nothing to evict");
    assert_eq!(store.stats().loads, 2, "every read was a miss, which is the cost of no cache");
    let _ = std::fs::remove_file(&path);
}

/// **`evict_all` empties the store at a point**, not in a `Drop`.
///
/// This is what 9C requires of evicted rasters — "eviction must be synchronous and observable" — applied
/// to text, and the assertion is that the *caller* can observe it. Without this method a caller has no way
/// to make resident memory fall before `commit()` or before a KDF, and the only lever is dropping the
/// store.
#[test]
fn evict_all_empties_the_store_immediately() {
    let text = content(SECTION_BYTES * 6);
    let (path, _wf) = container(&text);
    let mut wf = Wavefunction::open(&path, PASS, ITER).expect("open");
    let mut store = SectionStore::new(&mut wf, 6);

    let mut buf = vec![0u8; SECTION_BYTES];
    for s in 0..6u32 {
        store.copy_into(s, &mut buf).expect("fill");
    }
    assert_eq!(store.resident(), 6);
    assert_eq!(store.stats().evictions, 0, "a full budget evicts nothing");

    store.evict_all();
    assert_eq!(store.resident(), 0, "and the caller can see it empty before anything else happens");
    assert_eq!(store.resident_bytes(), 0);
    assert_eq!(
        store.stats().evictions, 6,
        "six evictions, each releasing a section"
    );
    // And the store still works afterwards, because evicting text is not a shutdown.
    read(&mut store, 3, &text);
    assert_eq!(store.resident(), 1);
    let _ = std::fs::remove_file(&path);
}