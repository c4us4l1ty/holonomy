//! **How much would write-through cost per keystroke?** 3 tests.
//!
//! # The question
//!
//! Phase 13 part 7 lists three ways to let a sparse document be edited, and **A (write-through)** is the
//! one whose cost is a *number* rather than a structure: re-encrypt and write the section holding the
//! edited leaf, immediately, on every keystroke. Part 7 recommends measuring A before choosing between
//! A, B (origin tracking) and C (dirty-region pin), because if A's price is small then B's and C's
//! structural complexity is not warranted.
//!
//! **This file is that measurement.** It is a gate rather than a benchmark because the answer changes
//! which design is correct, and a design chosen on a remembered number is a design chosen wrongly.
//!
//! # The measurement, and why it separates two costs
//!
//! Write-through's per-keystroke charge is exactly two operations, from `Ring::commit`
//! (`crates/holonomy-container/src/ring.rs:326-332`):
//!
//! | step | what it is | what it depends on |
//! | --- | --- | --- |
//! | **seal** | `aead::seal_in_place` — XChaCha20-Poly1305 over 65,520 B | CPU only, machine-independent-ish |
//! | **write** | `DirectFile::write_exact_at` — one `O_DIRECT` `pwrite` of 65,536 B | the storage device |
//!
//! **These have completely different magnitudes and completely different remedies.** If seal dominates,
//! the charge is arithmetic and roughly the same everywhere. If write dominates, the charge is the disk,
//! and no amount of cleverness in this crate changes it — only a write-back journal would.
//!
//! So this measures them separately, and reports the split, because **a single combined number cannot
//! distinguish "we chose the wrong design" from "the disk is slow"** and those need different answers.
//!
//! # The budget it is measured against
//!
//! §7 item 6: **a keystroke is 176–419 µs** on this host, with 1 MiB of framebuffer resident. That is the
//! charge write-through would be *added to*. It is a measured range rather than a single number, so the
//! assertions here are against its **low end** — if write-through is small against 176 µs it is certainly
//! small against 419 µs, and a threshold chosen at the pessimistic end would hide a cost that matters.

use holonomy_container::aead;
use holonomy_container::io::{AlignedBuf, DirectFile};
use holonomy_container::layout::{CHUNK_PLAINTEXT, CHUNK_SLOT};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// `target/<profile>/holonomy-container-tests/write-through`. **Not `/tmp`**: `DirectFile` opens with
/// `O_DIRECT` and tmpfs does not support it, so a `/tmp` container would fail to open — which would make
/// this file measure an error rather than a cost.
fn scratch_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("test exe path");
    let dir = exe
        .ancestors()
        .nth(3)
        .expect("target/<profile> layout")
        .join("holonomy-container-tests")
        .join("write-through");
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn unique() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    (t << 16) ^ n as u128
}

/// Median of `n` durations, in microseconds. **Median, not mean**: a write to a cold page cache is a
/// multi-millisecond outlier, and a mean would report an average that no keystroke ever experiences.
fn median_micros(mut samples: Vec<u128>) -> f64 {
    samples.sort_unstable();
    samples[samples.len() / 2] as f64 / 1000.0
}

fn nanos(d: Duration) -> u128 {
    d.as_nanos()
}

/// The **seal** cost alone: XChaCha20-Poly1305 over a full section, in place, no I/O.
///
/// This is the CPU half of write-through and it is **the same work a fault already does in reverse** —
/// `open_chunk` is the decrypt of these exact bytes. The gate measures both so the ratio is against a
/// cost the product already pays, rather than against an abstract number.
#[test]
fn sealing_one_section_is_a_pure_cpu_cost() {
    const N: usize = 200;
    let k = [7u8; 32];
    let n_root = [9u8; 24];

    // A slot sized exactly as the ring's, holding a full section of plaintext.
    let mut slot = AlignedBuf::zeroed(CHUNK_SLOT as usize);

    let mut seal = Vec::with_capacity(N);
    let mut open = Vec::with_capacity(N);
    for i in 0..N {
        // Refill with plaintext each time: sealing in place is destructive, so the next
        // iteration needs its bytes back or it would be measuring ciphertext.
        slot.as_mut_slice()[..CHUNK_PLAINTEXT].fill((i % 251) as u8);

        let t = Instant::now();
        aead::seal_in_place(&k, &n_root, 1, CHUNK_PLAINTEXT, slot.as_mut_slice()).expect("seal");
        seal.push(nanos(t.elapsed()));

        // And the decrypt the fault path already pays for the same bytes.
        let t = Instant::now();
        aead::open_chunk(&k, &n_root, 1, slot.as_mut_slice()).expect("open");
        open.push(nanos(t.elapsed()));
    }

    let seal_us = median_micros(seal);
    let open_us = median_micros(open);
    println!("seal {seal_us:.1} us/open {open_us:.1} us per {CHUNK_PLAINTEXT} B section");

    // **A measurement, with one assertion that it is a real number.** The seal must cost a bounded
    // amount of time and must not be free: a seal that took 2 s would be a broken cipher setup, and a
    // seal at ~0 would mean the loop optimised the call away — both are failures that a bare print
    // would have reported identically to a healthy result.
    assert!(
        seal_us > 1.0,
        "sealing a section took {seal_us:.1} us, which is suspiciously fast -- the call may have been \
         optimised out, and a zero here would make the whole measurement meaningless"
    );
    assert!(
        seal_us < 10_000.0,
        "sealing a section took {seal_us:.1} us, which is not a CPU cost"
    );

    // Seal and open are the same cipher, so they must be the same magnitude. A large divergence means
    // one of them is doing something extra -- a copy, an allocation, a fallback path -- and that would
    // change the accounting rather than just the number.
    let ratio = seal_us / open_us.max(0.001);
    assert!(
        (0.25..4.0).contains(&ratio),
        "seal/open ratio was {ratio:.2} ({seal_us:.1} vs {open_us:.1} us) -- these are the same cipher, \
         so a large divergence means one path is doing extra work the other is not"
    );
}

/// The **write** cost alone: one `O_DIRECT` `pwrite` of a full section, no crypto.
///
/// Measured on the same file write-through would use, at a real chunk offset, so the number is the
/// device's answer rather than a buffered write's.
#[test]
fn writing_one_section_is_the_device_s_answer() {
    const N: usize = 40;
    let path = scratch_dir().join(format!("wt-{}.wavefunction", unique()));
    // A container-sized file: the payload sits inside it, so the offsets are real ones.
    let file = DirectFile::create_or_open(&path).expect("create or open");
    let mut slot = AlignedBuf::zeroed(CHUNK_SLOT as usize);
    slot.as_mut_slice()[..CHUNK_PLAINTEXT].fill(0xA5);

    // Warm the file's allocation once so the first measured write is not charged for extending it.
    // **Excluded from the samples on purpose**: an allocation write is not what a keystroke does, and
    // including it would report a cost the product never pays.
    let at = holonomy_container::layout::chunk_offset(65_536, 1);
    file.write_exact_at(at, &slot).expect("warm write");

    let mut samples = Vec::with_capacity(N);
    for _ in 0..N {
        let t = Instant::now();
        file.write_exact_at(at, &slot).expect("write");
        samples.push(nanos(t.elapsed()));
    }

    let write_us = median_micros(samples);
    println!("O_DIRECT pwrite of {CHUNK_SLOT} B: {write_us:.1} us");

    // Reported, and bounded from above so a genuinely pathological device cannot quietly make this
    // gate pass. The interesting comparison is in the third test.
    assert!(
        write_us < 20_000.0,
        "an O_DIRECT write of a section took {write_us:.1} us, which is a device problem rather than a \
         number write-through can be reasoned about"
    );

    drop(file);
    let _ = std::fs::remove_file(&path);
}

/// **The answer, and what it decides.**
///
/// Measured on this host: **seal ≈ 175 µs, `O_DIRECT` write ≈ 950 µs, total ≈ 1,130 µs — about six and a
/// half times a whole keystroke**, against §7 item 6's 176 µs. And **the write is 84 % of it.**
///
/// That split is the finding, and it is why the gate asserts the *split* rather than the total:
///
/// * A **device-dominated** charge cannot be optimised away by anything in this crate. It is the disk.
/// * So **batching the writes does not help proportionally** — B (origin tracking) defers the write, it
///   does not make it cheaper, and a deferred write is still ~1 ms when it happens.
/// * What helps is **not writing during editing at all**, which is C's rule — and C's rule is available
///   for free once the store is not being asked to be authoritative on the edited region.
///
/// **So write-through is out, and the measurement says so in a way an argument could not**: it is not
/// "slow", it is *six times a keystroke*, and four fifths of it is not ours to reduce.
///
/// # Why the total is reported and not asserted
///
/// An assertion like `charge < KEYSTROKE_US` would be a **hardware-dependent gate**, and a hardware-
/// dependent gate is a lie waiting for different hardware: it would pass on an NVMe drive and fail on
/// this host's, and in both cases it would be reporting the disk rather than the design. The decision
/// this number drives is recorded in PROJECT.md Phase 13 part 7 instead, where it belongs.
///
/// What *is* gated is the structural fact: **the write dominates the seal by a wide margin on any
/// device where writing costs more than encrypting.** If that ever inverts — a RAM-backed or
/// write-back-caching device — the finding would in fact change, and this gate is what would notice.
#[test]
fn write_throughs_charge_is_dominated_by_the_device_not_the_cipher() {
    const SEAL_N: usize = 200;
    const WRITE_N: usize = 40;
    /// §7 item 6's **low** end of the measured keystroke range, for the ratio below.
    const KEYSTROKE_US: f64 = 176.0;

    let k = [7u8; 32];
    let n_root = [9u8; 24];
    let mut slot = AlignedBuf::zeroed(CHUNK_SLOT as usize);

    let mut seal = Vec::with_capacity(SEAL_N);
    for i in 0..SEAL_N {
        slot.as_mut_slice()[..CHUNK_PLAINTEXT].fill((i % 251) as u8);
        let t = Instant::now();
        aead::seal_in_place(&k, &n_root, 1, CHUNK_PLAINTEXT, slot.as_mut_slice()).expect("seal");
        seal.push(nanos(t.elapsed()));
    }
    let seal_us = median_micros(seal);

    let path = scratch_dir().join(format!("wt-charge-{}.wavefunction", unique()));
    let file = DirectFile::create_or_open(&path).expect("create or open");
    let at = holonomy_container::layout::chunk_offset(65_536, 1);
    file.write_exact_at(at, &slot).expect("warm write");
    let mut samples = Vec::with_capacity(WRITE_N);
    for _ in 0..WRITE_N {
        let t = Instant::now();
        file.write_exact_at(at, &slot).expect("write");
        samples.push(nanos(t.elapsed()));
    }
    let write_us = median_micros(samples);

    let charge = seal_us + write_us;
    let share = charge / KEYSTROKE_US;
    let wshare = write_share(seal_us, write_us);

    println!(
        "write-through per keystroke: seal {seal_us:.1} us + write {write_us:.1} us = {charge:.1} us \
         ({:.1}x a {KEYSTROKE_US:.0} us keystroke); the write is {:.0}% of the charge",
        share,
        wshare * 100.0
    );
    println!(
        "for the record: write-through is OUT. PROJECT.md Phase 13 part 7 records the decision; this \
         file records the measurement."
    );

    // **The gated claim: the charge is the device, not the cipher.** Encrypting 65 KB costs about as much
    // as decrypting it (same cipher, asserted in the sibling test), and both are far less than putting
    // the result on the disk. A factor of 3 is a wide margin — measured it is ~5.4× — chosen so the
    // assertion survives a device that is fast but not absurdly so.
    assert!(
        wshare > 0.75,
        "the write is only {:.0}% of write-through's charge (seal {seal_us:.1} us, write {write_us:.1} \
         us) -- the charge is no longer device-dominated, so Phase 13 part 7's finding needs revisiting",
        wshare * 100.0
    );

    drop(file);
    let _ = std::fs::remove_file(&path);
}

/// The write's share of the total charge, as a fraction.
fn write_share(seal_us: f64, write_us: f64) -> f64 {
    write_us / (seal_us + write_us).max(0.001)
}