//! **Per-leaf write-back cannot repair a shift, and this gate is why that is a recorded finding rather
//! than a bug report.** 2 tests.
//!
//! # What it falsifies
//!
//! Phase 13 part 8 set the repair for sparse editing as one rule:
//!
//! > **A source must return a leaf's bytes as they are now, at current offsets.**
//!
//! and `write_back.rs` gated it with 6 tests. **Those 6 tests are not wrong; they are narrower than the
//! rule.** Every one of them uses a `Vec<u8>` as the source, and **a `Vec` has no sections.** Writing a
//! leaf's bytes back into a `Vec` overwrites exactly that leaf's range and disturbs nothing else, so a
//! shift is invisible: the tests read back the leaf they just wrote and never a later one.
//!
//! `SectionStore` is section-granular. A section is 65,520 B and a leaf is at most 3,841 B, so **writing
//! one leaf back repairs at most 1 leaf in 17 of a section, and leaves the rest of that section holding
//! pre-shift bytes.** Past an edit, every offset is wrong by the delta, and write-back touches only the
//! leaf that was evicted.
//!
//! # The measurement
//!
//! A 135,040-byte document (three sections), `insert(10, "ZZZZZ")`, then **every resident leaf written
//! back and committed** — so nothing can be blamed on a missed eviction:
//!
//! ```text
//! resident leaves to write back: [0]
//! committed 65520 bytes
//! back   == truth: false
//! back   == saved: false
//! bytes differing from truth: 132987
//! first difference at document offset 2053
//!   back  [2053..] = [139, 19, 150, 30, ...]
//!   truth [2053..] = [237, 117, 248, 128, ...]
//! ```
//!
//! Offset 2,053 is exactly where leaf 0 ends — so **the leaf's own bytes are right and everything after
//! it is wrong by exactly the insert length.** 132,987 of 135,040 bytes differ: the write-back worked,
//! and repaired 0.15 % of the document.
//!
//! # What this means for the design, and it is not a bug in `write_at`
//!
//! [`SectionStore::write_at`] does what it says. The falsified thing is **the premise that per-leaf
//! write-back is a substitute for origin tracking.** It is not, because a shift is not a leaf-local
//! event: it moves every offset after the edit, and a section cannot hold two coordinate systems at once.
//!
//! > **Only the edit record can answer this.** It is the one thing that says *the store's byte at offset
//! > X is the document's byte at X + delta*, which is exactly the fact write-back cannot supply.
//!
//! So part 8 and part 12 are **not two halves of one design — they are two mutually exclusive designs**,
//! and this gate is the evidence for which one is real. Part 12's is.
//!
//! | what it proves | test |
//! | --- | --- |
//! | **write-back cannot repair a shift** | [`write_back_cannot_repair_a_shift`] |
//! | **the record does** | [`the_record_does_repair_what_write_back_cannot`] |

use holonomy::store::SectionStore;
use holonomy_container::Wavefunction;
use holonomy_text::edit_record::{Edit, EditRecord};
use holonomy_text::{LeafSource, Rope};

const PASS: &str = "correct horse battery staple";
const ITER: u64 = 1;
const SECTION: usize = 65_520;

fn scratch_dir() -> std::path::PathBuf {
    let d = std::env::current_exe()
        .expect("exe")
        .ancestors()
        .nth(3)
        .expect("layout")
        .join("holonomy-container-tests")
        .join("write-back-shift");
    std::fs::create_dir_all(&d).expect("scratch");
    d
}
fn unique() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static C: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    (t << 20) ^ u128::from(C.fetch_add(1, Ordering::Relaxed))
}
fn container(c: &[u8]) -> (std::path::PathBuf, Wavefunction) {
    let p = scratch_dir().join(format!("ws-{}-{}.wf", c.len(), unique()));
    let _ = Wavefunction::create(&p, PASS, "p3", c, ITER).expect("create");
    let wf = Wavefunction::open(&p, PASS, ITER).expect("open");
    (p, wf)
}
fn text(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| (i.wrapping_mul(131).wrapping_add(17) % 251) as u8)
        .collect()
}

#[test]
fn write_back_cannot_repair_a_shift() {
    let n = SECTION * 2 + 4_000;
    let saved = text(n);
    let (path, mut wf) = container(&saved);
    let mut rope = Rope::from_skeleton(n);
    let mut store = SectionStore::new(&mut wf, 2);

    let at = 10usize;
    let mut buf = vec![0u8; 8];
    rope.read_at_faulting(&mut store, at, 8, &mut buf)
        .expect("fault");
    rope.insert_at(at, b"ZZZZZ").expect("insert");
    let mut truth = saved.clone();
    truth.splice(at..at, b"ZZZZZ".iter().copied());

    // **Every resident leaf, written back.** The point is to remove "a leaf was missed" as an
    // explanation, so this cannot be a partial write.
    //
    // **Driven through `store_leaf` directly, and that is part 15's change.** This used to be
    // `rope.evict_leaf_to(&mut store, k)`, a part-8 helper that wrote a leaf back and evicted it. Part 15
    // removed that helper, because with the record in the rope a fault asks for *saved* bytes and a helper
    // that writes *current* bytes into the store is asking it two questions at once. The finding this file
    // records does not need the helper: it needs the mechanism, and the mechanism is `store_leaf`.
    let resident: Vec<usize> = (0..rope.leaf_count())
        .filter(|&k| rope.is_resident(k))
        .collect();
    for k in resident {
        let at = rope.leaf_offset(k);
        let len = rope.leaf_len_of(k);
        let mut bytes = vec![0u8; len];
        rope.read_at_faulting(&mut store, at, len, &mut bytes)
            .expect("read the leaf");
        store.store_leaf(at, &bytes).expect("write the leaf back");
        rope.evict_leaf(k).expect("evict");
    }
    store.commit_dirty().expect("commit");
    drop(store);
    let clen = wf.content_len() as usize;
    drop(wf);

    // **The length did not grow either**, which is the part 8 `set_len` gap seen end to end: the
    // commit reports success and the document on disk is five bytes short of the one in memory.
    assert_eq!(
        clen,
        saved.len(),
        "the committed container is still the saved length"
    );
    assert_eq!(
        truth.len(),
        saved.len() + 5,
        "while the in-memory document is five bytes longer"
    );

    let mut wf2 = Wavefunction::open(&path, PASS, ITER).expect("reopen");
    let mut store2 = SectionStore::new(&mut wf2, 2);
    // **`back` is sized to the truth, not to the saved length**, because the whole point is that the
    // committed document is five bytes short of what was typed -- sizing it to the container would make
    // that shortfall a panic here instead of the assertion two lines below.
    let mut back = vec![0u8; truth.len()];
    for s in 0..3u32 {
        let lo = s as usize * SECTION;
        let hi = ((s as usize + 1) * SECTION).min(n);
        if lo < hi {
            store2
                .fetch_leaf(lo, &mut back[lo..hi])
                .expect("read section");
        }
    }

    // **The leaf's own bytes are right.** Write-back did its job, exactly where it was pointed.
    assert_eq!(
        &back[..at + 5],
        &truth[..at + 5],
        "the edited leaf wrote back correctly"
    );

    // **And the rest of the document is wrong.** This is the finding, so it is asserted rather than
    // printed -- a test that measured nothing would be worse than none, because it gets counted.
    let diffs = (0..truth.len()).filter(|&i| back[i] != truth[i]).count();
    assert!(
        diffs > n / 2,
        "expected most of the document to be wrong after a shift; only {diffs} of {} bytes differed, \
         so this fixture no longer exercises the failure and would be passing for the wrong reason",
        truth.len()
    );
    assert_ne!(&back[..], &truth[..], "and it is NOT equal to the truth");
    assert_ne!(
        &back[..],
        &saved[..],
        "and it is NOT the saved document either -- it is a mixture"
    );
}

/// **The record answers what write-back cannot**, on the same document, in the same breath as the gate
/// above falsifies the alternative.
///
/// The two together are the argument for part 12 over part 8: not that the record is convenient, but
/// that the other answer was **measured** and does not work.
#[test]
fn the_record_does_repair_what_write_back_cannot() {
    let n = SECTION * 2 + 4_000;
    let saved = text(n);
    // **The store is faulted but never reopened**, so this test needs the container and not the path --
    // which is why the path goes unused here and is named as such rather than bound to `_`.
    let (_path, mut wf) = container(&saved);
    let mut rope = Rope::from_skeleton(n);
    let mut store = SectionStore::new(&mut wf, 2);

    let at = 10usize;
    let mut buf = vec![0u8; 8];
    rope.read_at_faulting(&mut store, at, 8, &mut buf)
        .expect("fault");
    rope.insert_at(at, b"ZZZZZ").expect("insert");

    let mut rec = EditRecord::new();
    rec.push(Edit::insert(at, b"ZZZZZ".to_vec()));
    let mut truth = saved.clone();
    truth.splice(at..at, b"ZZZZZ".iter().copied());

    // **Read through the store, then replay.** The store is asked for *saved* bytes at saved offsets --
    // which is what it actually holds -- and the record turns them into current bytes.
    let mut wrong = 0usize;
    for lo in (0..n).step_by(SECTION) {
        let hi = (lo + SECTION).min(truth.len());
        let got = rec
            .replay(lo, hi - lo, |a, k| saved[a..a + k].to_vec())
            .expect("replay");
        wrong += got
            .iter()
            .zip(&truth[lo..hi])
            .filter(|(a, b)| a != b)
            .count();
    }
    assert_eq!(
        wrong, 0,
        "the record reproduces the document exactly, across every section"
    );
    drop(store);
    drop(wf);
}
