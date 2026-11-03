//! The 192 KiB claim, asserted against a real allocator rather than against a comment.
//!
//! FR-3.1 and PRD's "192 KiB 3-stage ring buffer" are the reason this crate exists: the
//! container is 128 MiB and the reader must never hold more than three chunk slots. Every other
//! claim in the crate is about bytes on a disk; this one is about bytes in the heap, and the
//! only honest way to check it is to count.
//!
//! # What is and is not proved here
//!
//! **Proved:** from the moment [`arm`] is called until the test ends, the largest single heap
//! allocation is under 1 MiB and the peak *additional* live bytes are under a quarter of the
//! container's size — while a document spanning many chunks is read out of a 128 MiB file. A
//! reader that buffered the file, or mapped it, or slurped the payload, would trip this
//! immediately.
//!
//! **Not proved:** anything about Argon2id. Key derivation genuinely allocates 128 MiB, so the
//! allocator is disarmed across `create` and `open` and armed only around the read. That is a
//! real exemption, not a measurement, and it is stated here rather than glossed:
//!
//! * `create` allocates an aligned 4 KiB page buffer, two 64 KiB slot buffers, and the
//!   chaff keystream for one page at a time.
//! * `open` allocates the three 64 KiB ring slots (192 KiB, the figure under test) and
//!   `read_content`'s `Vec` for the caller's own result.
//!
//! So the armed window covers exactly the streaming path, and the one large allocation it is
//! allowed is the returned document buffer.
//!
//! This lives in its own integration-test binary so that its `#[global_allocator]` applies to
//! nothing else: a global allocator installed in the lib's unit tests would instrument every
//! other test in the crate and any thread-safety in the counters would show up as noise in
//! *their* measurements.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use holonomy_container::{layout, Wavefunction};

/// Re-exported for convenience; the crate's own `layout::CONTAINER_SIZE` is the definition.
const CONTAINER_SIZE: u64 = holonomy_container::layout::CONTAINER_SIZE;

/// Bytes currently live, updated on every alloc/realloc/dealloc.
static LIVE: AtomicUsize = AtomicUsize::new(0);
/// High-water mark of `LIVE` since arming.
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// Largest single allocation since arming.
static LARGEST: AtomicUsize = AtomicUsize::new(0);
/// Number of allocations of at least 1 MiB since arming.
static BIG_COUNT: AtomicUsize = AtomicUsize::new(0);
/// Total bytes in those allocations.
static BIG_SUM: AtomicUsize = AtomicUsize::new(0);
/// Whether peak and largest are being recorded at all.
static ARMED: AtomicBool = AtomicBool::new(false);

struct Tracking;

/// Registering the allocator is the step that makes this test mean anything.
///
/// Without this attribute `Tracking` is never installed, `record` is dead code, `LIVE` stays
/// at 0, and every assertion below passes trivially -- the exact failure mode a test about
/// "nothing allocated too much" is most prone to. `the_allocator_is_actually_installed`
/// exists to catch that, and it caught it here: the first version of this file omitted the
/// attribute and produced a green test that measured nothing.
#[global_allocator]
static ALLOCATOR: Tracking = Tracking;

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim to the system allocator.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            record(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim to the system allocator.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            record(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        // SAFETY: `p`/`layout` came from this allocator.
        unsafe { System.dealloc(p, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded verbatim to the system allocator.
        let q = unsafe { System.realloc(p, layout, new_size) };
        if !q.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            record(new_size);
        }
        q
    }
}

fn record(size: usize) {
    let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
    if ARMED.load(Ordering::Relaxed) {
        PEAK.fetch_max(live, Ordering::Relaxed);
        LARGEST.fetch_max(size, Ordering::Relaxed);
        if size >= 1 << 20 {
            BIG_COUNT.fetch_add(1, Ordering::Relaxed);
            BIG_SUM.fetch_add(size, Ordering::Relaxed);
        }
    }
}

/// Start measuring. `PEAK` begins from whatever is live now.
fn arm() {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    LARGEST.store(0, Ordering::Relaxed);
    BIG_COUNT.store(0, Ordering::Relaxed);
    BIG_SUM.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
}

fn big_count() -> usize {
    BIG_COUNT.load(Ordering::Relaxed)
}

fn big_sum() -> usize {
    BIG_SUM.load(Ordering::Relaxed)
}

fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

fn largest() -> usize {
    LARGEST.load(Ordering::Relaxed)
}

/// Serialises the tests in this binary.
///
/// The counters are process-global, so a measurement window is only meaningful if nothing
/// else in the process allocates during it. Run with `--test-threads=2` and the sibling test's
/// Argon2id call -- a genuine 128 MiB allocation -- lands inside the window and both tests
/// fail with the *other* test's numbers. The first version of this file did exactly that, and
/// the resulting failures read as though the container were buffering itself.
///
/// A mutex is the right fix rather than a note asking for `--test-threads=1`: the harness
/// chooses the thread count, and a test that only passes under one particular invocation is
/// not a test. Each test below takes [`WINDOW`] for its whole body, which also covers the
/// un-armed create/open calls whose allocations must not be attributed to a neighbour.
static WINDOW: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn arming<T>(f: impl FnOnce() -> T) -> T {
    arm();
    let out = f();
    ARMED.store(false, Ordering::Relaxed);
    out
}

/// Take the measurement window for the duration of a test.
fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    // A poisoned lock means another test panicked mid-window; the counters are still
    // consistent, so recover rather than cascade the panic into an unrelated test.
    WINDOW.lock().unwrap_or_else(|e| e.into_inner())
}

/// Scratch directory on a filesystem that supports `O_DIRECT`. `/tmp` is tmpfs here and tmpfs
/// rejects it, so the files go next to the build output like every other container test.
fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test exe path");
    let base = exe.ancestors().nth(3).expect("target/<profile> layout");
    let dir = base.join("holonomy-container-tests").join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Derivation iterations for the gate. Same rationale as the lib's `TEST_VDF_ITERATIONS`.
const T: u64 = 8;

/// The measuring instrument must be plugged in.
///
/// A guard, not a formality: this file's whole value is the allocator, and an unregistered
/// allocator turns every assertion below into a tautology over the constant 0. The check is
/// that a live measurement window sees at least some real traffic.
#[test]
fn the_allocator_is_actually_installed() {
    let _guard = exclusive();
    // The allocation has to happen *inside* the armed window: arming resets the high-water
    // marks, so anything reserved beforehand is invisible by construction. Getting this wrong
    // is the second way this test could pass vacuously.
    let seen = arming(|| {
        let v: Vec<u8> = vec![0u8; 300_000];
        std::hint::black_box(&v);
        (largest(), peak())
    });
    assert!(
        seen.0 >= 300_000,
        "the tracking allocator saw a largest allocation of {} bytes after a 300,000-byte \
         Vec was reserved, so it is not installed",
        seen.0
    );
    assert!(seen.1 > 0, "peak live bytes never moved");
}

/// Reading a multi-chunk document out of a 128 MiB container must not scale with the file.
///
/// The document is 1.5 MB across 24 content chunks, so a correct reader touches 24 x 64 KiB of
/// disk with three slots resident. The bounds below are deliberately loose where the test's own
/// bookkeeping needs room (`read_content` returns a `Vec` for the caller) and tight where the
/// property lives.
#[test]
fn reading_does_not_scale_with_the_container() {
    let _guard = exclusive();
    let dir = scratch_dir("resident");
    let path = dir.join("doc.wavefunction");

    let doc: Vec<u8> = (0..1_500_000).map(|i| (i % 251) as u8).collect();

    // Derivation allocates 128 MiB for Argon2id, so neither call is measured.
    drop(Wavefunction::create(&path, "pw", "t", &doc, T).expect("create"));
    let mut wf = Wavefunction::open(&path, "pw", T).expect("open");
    assert_eq!(
        wf.frame().chunk_count,
        24,
        "24 content chunks plus the master frame"
    );

    // The claim under test, stated as a number rather than a comment.
    assert_eq!(
        wf.ring_resident_bytes(),
        196_608,
        "the ring must be exactly 3 x 65,536 bytes"
    );
    assert_eq!(wf.ring_resident_bytes(), 192 * 1024);

    let read_back = arming(|| wf.read_content().expect("read_content"));
    assert!(read_back == doc, "content did not survive the round trip");

    // No single allocation near the size of the file. Argon2id's 128 MiB is behind us; the
    // ring's 192 KiB is already live; `read_content`'s Vec is 1.5 MB.
    assert!(
        largest() >= 1_000_000,
        "the read path's largest allocation was only {} bytes, which means the allocator is \
         not measuring and this whole test is vacuous",
        largest()
    );
    assert!(
        largest() < CONTAINER_SIZE as usize,
        "a single allocation of {} bytes happened during the read; the container is {CONTAINER_SIZE}",
        largest()
    );
    assert!(
        largest() <= 2_000_000,
        "largest single allocation was {} bytes, which is not the document buffer",
        largest()
    );

    // And the total additional live bytes across 24 chunks stayed small. Reading every chunk
    // twice, so a reader that quietly cached the payload would be caught.
    arming(|| {
        for _ in 0..2 {
            let again = wf.read_content().expect("second pass");
            assert!(again == doc);
        }
    });

    assert!(
        peak() < 8 * 1024 * 1024,
        "peak additional live bytes were {} while reading a 128 MiB container",
        peak()
    );

    drop(wf);
}

/// What `open` actually allocates, stated as a measurement rather than a wish.
///
/// An earlier version of this file asserted that `open` never allocates anything the size of
/// the container. That is false, and not because of the container: `Wavefunction::open` runs
/// Argon2id at 128 MiB, which is a real allocation of the same magnitude as the file. The test
/// failed at `134217728 == 134217728` and the failure was the correct outcome.
///
/// So the honest version measures instead:
/// * exactly one allocation of at least 1 MiB -- Argon2id's contiguous pool. Argon2id asks for
///   one block of `m` KiB; the two lanes are carved out of it, so this is 1, not 2.
/// * nothing at all between 1 MiB and 64 KiB *above* the pool, i.e. `big_sum` is the pool and
///   the ring. The container's payload is never buffered: the only large thing in `open` is
///   key derivation, which FR-4.5 is about and which this crate does not change.
/// * and the ring, which is the product's own steady-state footprint, stays at 192 KiB.
///
/// What this does *not* establish is that `open` never maps the file; it establishes that the
/// only large allocation is derivation. Isolating the file-buffering question at `open` time is
/// not possible from outside without knowing where derivation ends.
#[test]
fn opening_allocates_only_the_key_derivation_pool() {
    let _guard = exclusive();
    let dir = scratch_dir("resident_open");
    let path = dir.join("doc.wavefunction");
    let doc = vec![7u8; 100_000];
    {
        let _wf = Wavefunction::create(&path, "pw", "t", &doc, T).expect("create");
    }

    let (omega, largest_open, bigs, big_bytes) = arming(|| {
        let wf = Wavefunction::open(&path, "pw", T).expect("open");
        let om = wf.omega();
        assert_eq!(
            wf.ring_resident_bytes(),
            196_608,
            "the ring must be 192 KiB even at open"
        );
        (om, largest(), big_count(), big_sum())
    });

    assert!(omega >= layout::CHUNK_SLOT && omega % layout::CHUNK_SLOT == 0);

    // Argon2id's pool, and nothing else that large.
    assert_eq!(
        bigs, 1,
        "open made {bigs} allocations of at least 1 MiB (total {big_bytes} bytes); \
         expected exactly one -- Argon2id's m = 131072 KiB pool. A second one would mean the \
         file or the payload is being buffered."
    );
    assert!(
        largest_open >= 131_072 * 1024,
        "largest allocation during open was {largest_open} bytes, less than Argon2id's \
         131072 KiB pool, so derivation did not run inside the window"
    );
}

/// The container must be 128 MiB on disk, so the "not 128 MiB in memory" claim means
/// something.
#[test]
fn the_container_on_disk_is_128_mib() {
    let _guard = exclusive();
    let dir = scratch_dir("resident_size");
    let path = dir.join("doc.wavefunction");
    {
        let _wf = Wavefunction::create(&path, "pw", "t", b"tiny", T).expect("create");
    }
    let len = std::fs::metadata(&path).expect("stat").len();
    assert_eq!(len, CONTAINER_SIZE);
    assert_eq!(len, 134_217_728);
    assert_eq!(len, 128 * 1024 * 1024);
}
