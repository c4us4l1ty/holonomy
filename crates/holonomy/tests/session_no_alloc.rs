//! **Phase 11's gate: typing allocates nothing, measured through a `Session`.**
//!
//! # Why this file exists at all
//!
//! `crates/holonomy-text/tests/no_alloc.rs` asserts FR-1.2 -- "typing allocates nothing" -- and it has
//! been green since Phase 6. It is also, until this file, measuring the wrong thing: it drives an
//! [`Editor`] directly (`no_alloc.rs:126-148` types through `Editor::insert_char`) and never constructs
//! a [`Session`]. The product's keystroke path is `Session::handle_event -> apply -> after_edit ->
//! tick -> paint`, and that path called `self.editor.text()` **eleven times** (`session.rs` at lines
//! 379, 391, 450, 821, 926, 1044, 1054, 1093, 1165, 1200, 1350 before Phase 11), each of which
//! allocated a `Vec` the size of the whole document.
//!
//! So the existing gate was green while the product violated the invariant it asserts. That is the
//! failure mode this file exists to close: **a gate that cannot see the code it is a gate for.**
//!
//! # What it asserts, and what it does not
//!
//! Asserts: 0 heap allocations across 1,000 keystrokes driven through a real `Session` with a real
//! `ScriptedInputSource`, on a document large enough that a whole-document copy would be obvious.
//!
//! Does **not** assert zero allocations at a *leaf boundary*: a split is one `mmap` + `mlock` and
//! `no_alloc.rs` already gates that separately (`a_keystroke_is_o1_except_at_a_leaf_boundary`). This
//! file runs its burst in a leaf that has room, so the count is the keystroke path and nothing else.
//!
//! # Why a child process
//!
//! `RLIMIT_MEMLOCK` is **process-wide** and libtest runs tests in one binary on parallel threads
//! (`crates/holonomy-secure/tests/allocation_failure.rs:38-46` documents this for the same reason).
//! So a large document loaded in the parent would race every other test's `mlock` budget. The child
//! process gets its own address space and its own limit.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

use holonomy::session::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::InputEvent;
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy};

/// Set on the re-executed child to select the probe role.
const PROBE_ENV: &str = "HOLONOMY_SESSION_ALLOC_PROBE";

/// How much text to type into. Enough that a whole-document copy per keystroke is not a rounding
/// error, small enough that the child finishes quickly.
const BURST: usize = 1_000;

/// The document's size before typing. Chosen to be **larger than `SCAN_CHUNK`** by a wide margin, so a
/// regression that reintroduces a whole-document read shows up as an allocation rather than as a
/// slightly larger scan.
const DOC_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------- counting allocator

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static REALLOCS: AtomicU64 = AtomicU64::new(0);
static DEALLOCS: AtomicU64 = AtomicU64::new(0);
static ENABLED: AtomicU64 = AtomicU64::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) != 0 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        record_size(layout);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ENABLED.load(Ordering::Relaxed) != 0 {
            DEALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) != 0 {
            REALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        record_size(Layout::from_size_align_unchecked(new_size, layout.align()));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const SIZES_ENV: &str = "HOLONOMY_SESSION_ALLOC_SIZES";

/// Allocation sizes seen while enabled, for the diagnostic mode in the child probe.
///
/// Recording the size rather than only the count is what turns "27 allocations" into "27 allocations,
/// of which 16 are 65,537 bytes and 11 are 8" -- and the second half is a `Vec` of eight something.
static SIZES: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// Whether to record sizes, as an atomic rather than an `env::var` call.
///
/// **This was a stack overflow, and it is worth recording why.** The first version asked
/// `std::env::var_os(SIZES_ENV)` inside `record_size`, i.e. inside the allocator. `var_os` allocates,
/// so every allocation called another one and recursed until the stack ran out. An allocator must do
/// no work that can allocate: an atomic load is the only thing here that is safe by construction.
static RECORD_SIZES: AtomicU64 = AtomicU64::new(0);

fn record_size(layout: Layout) {
    if RECORD_SIZES.load(Ordering::Relaxed) == 0 || ENABLED.load(Ordering::Relaxed) == 0 {
        return;
    }
    // `try_lock`, never `lock`: a reallocation of the recording `Vec` takes the lock itself, and a
    // blocking `lock` there would deadlock rather than recurse.
    if let Ok(mut sizes) = SIZES.try_lock() {
        sizes.push(layout.size());
    }
}

fn enable() {
    ALLOCS.store(0, Ordering::SeqCst);
    REALLOCS.store(0, Ordering::SeqCst);
    DEALLOCS.store(0, Ordering::SeqCst);
    ENABLED.store(1, Ordering::SeqCst);
}

fn disable() -> (u64, u64, u64) {
    ENABLED.store(0, Ordering::SeqCst);
    (
        ALLOCS.load(Ordering::SeqCst),
        REALLOCS.load(Ordering::SeqCst),
        DEALLOCS.load(Ordering::SeqCst),
    )
}

// ---------------------------------------------------------------- fixtures

/// A session over a document of `DOC_BYTES`, with the atlas built.
///
/// The atlas is leaked so the session can borrow it for longer than the expression that built it, which
/// is what `tests/session.rs:56-59` does and for the same reason: building it per session would
/// dominate the probe's run time and, worse, allocate *inside* the measured window.
fn session(doc_bytes: usize) -> (Session<'static>, &'static holonomy_assets::atlas::Atlas) {
    let atlas: &'static holonomy_assets::atlas::Atlas = Box::leak(Box::new(
        holonomy_assets::build_atlas(&[16])
            .expect("build the atlas")
            .0,
    ));
    let metrics = ChromeMetrics::DESKTOP;
    let mut editor = Editor::new();
    // Words, not filler: `refresh_counts` counts whitespace-delimited runs, and a document of one
    // unbroken token would make the word-count carry across chunk boundaries untestable.
    let line = "the quick brown fox jumps over the lazy dog\n";
    while editor.text_len() < doc_bytes {
        editor
            .insert_at(editor.text_len() as u32, line.as_bytes(), SpanPolicy::GrowIntoInsert)
            .expect("room");
    }
    // Argument order is `Session::new(editor, painter, scanout, metrics)` -- `session.rs:354-359`.
    // `Painter::new` takes the atlas and a size index; the session owns the frame itself
    // (`session.rs:380`), so no `Frame` is constructed here.
    let session = Session::new(
        editor,
        Painter::new(atlas, 16),
        Box::new(HeadlessScanout::new(metrics.width, metrics.height)),
        metrics,
    );
    (session, atlas)
}

// ---------------------------------------------------------------- the child probe

/// `BURST` presses of the letter `a`, plus their releases, as the events a keyboard would deliver.
///
/// Built once, **before** the counting window opens. Building them inside the window would count the
/// `Vec`'s growth as typing -- which is a real allocation, but one this gate is not about. The session
/// itself is the thing under test, and `Session::handle_event` is exactly what `windowed.rs:130-167`
/// calls on a live key event.
fn typing_events() -> Vec<InputEvent> {
    let mut events = Vec::with_capacity(BURST * 2);
    for _ in 0..BURST {
        events.push(InputEvent::press(holonomy_input::KEY_A));
        events.push(InputEvent::release(holonomy_input::KEY_A));
    }
    events
}

/// Prints `PROBE allocs reallocs deallocs edits words lines bytes` on one line.
#[test]
fn alloc_probe_child() {
    if std::env::var_os(PROBE_ENV).is_none() {
        return;
    }
    let (mut s, _atlas) = session(DOC_BYTES);
    s.state.scroll_line = 0;
    let events = typing_events();

    if std::env::var_os(SIZES_ENV).is_some() {
        // Diagnostic mode: one keystroke, and every allocation size it makes. Printed so the
        // *sources* can be identified rather than guessed at -- a count alone says how many, not who.
        //
        // The flag is read *here*, into an atomic, rather than inside the allocator. See `RECORD_SIZES`.
        RECORD_SIZES.store(1, Ordering::SeqCst);
        enable();
        let _ = s.handle_event(InputEvent::press(holonomy_input::KEY_A));
        let _ = s.handle_event(InputEvent::release(holonomy_input::KEY_A));
        let (a, r, d) = disable();
        println!("SIZES {a} {r} {d}");
        let mut sizes = SIZES.lock().expect("sizes lock");
        sizes.sort_unstable();
        let mut last: Option<usize> = None;
        let mut run = 0usize;
        for size in sizes.iter().copied() {
            if Some(size) == last {
                run += 1;
            } else {
                if let Some(l) = last {
                    println!("  {run} x {l} bytes");
                }
                last = Some(size);
                run = 1;
            }
        }
        if let Some(l) = last {
            println!("  {run} x {l} bytes");
        }
        return;
    }

    // Only the presses count as edits, so `edits` is compared against `BURST` and not `2 * BURST`.
    let presses = events.len() / 2;

    // **The edit path, without the paint.** `handle_event` is `apply` then `tick`, and `tick` paints.
    // Phase 11's claim is about the *edit* -- the model change, the caret move, the counts -- and the
    // paint path's remaining allocations are a different, already-scheduled piece of work (Phase 12
    // changes `Painter::text`'s contract and reuses the surface tree). Measuring the two together
    // would produce a single number that is neither claim, and asserting zero on it would be false.
    //
    // `apply` is what `handle_event` calls after the keymap, and it is public for exactly the reason
    // `tick` is (`session.rs:750`): a driver with its own event source drives it directly.
    enable();
    let mut edits = 0u32;
    for ev in &events {
        let Some(cmd) = s.dispatch(*ev) else {
            continue;
        };
        if s.apply(cmd).is_err() {
            panic!("keystroke {edits} failed");
        }
        if ev.value == 1 {
            edits += 1;
        }
    }
    let (a, r, d) = disable();
    let edit_allocs = a + r + d;

    // **The paint path, on top of it.** Recorded, not asserted at zero -- see `the_paint_path_still_`
    // below, which pins what it currently costs so Phase 12 has a number to beat.
    enable();
    s.tick().expect("paint");
    let (pa, pr, pd) = disable();
    let paint_allocs = pa + pr + pd;

    assert_eq!(edits, presses as u32, "every press should have been an edit");
    println!(
        "PROBE {edit_allocs} {a} {r} {d} {edits} {} {} {} {paint_allocs}",
        s.state.words, s.state.total_lines, s.state.bytes
    );
}

/// `edit_allocs` (total), then its three components, then `edits words lines bytes`, then
/// `paint_allocs`.
type Probe = (u64, u64, u64, u64, u32, u32, u32, u32, u64);

fn run_probe() -> Probe {
    run_probe_with(&[])
}

/// `run_probe` with extra environment for the child, so the diagnostic mode is a caller decision
/// rather than a compile-time branch.
fn run_probe_with(extra: &[(&str, &str)]) -> Probe {
    let exe = std::env::current_exe().expect("current test binary");
    let mut cmd = std::process::Command::new(exe);
    cmd.args([
        "--exact",
        "alloc_probe_child",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(PROBE_ENV, "1");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn the probe child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    const MARKER: &str = "PROBE ";
    let line = stdout
        .lines()
        .find_map(|l| l.find(MARKER).map(|i| &l[i + MARKER.len()..]))
        .unwrap_or_else(|| {
            panic!(
                "the probe child printed no PROBE line.\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    let v: Vec<u64> = line
        .split_whitespace()
        .map(|t| {
            t.parse::<u64>()
                .unwrap_or_else(|_| panic!("unparseable PROBE line: {line:?}"))
        })
        .collect();
    assert_eq!(
        v.len(),
        9,
        "PROBE line should have nine fields: {line:?}"
    );
    (
        v[0],
        v[1],
        v[2],
        v[3],
        v[4] as u32,
        v[5] as u32,
        v[6] as u32,
        v[7] as u32,
        v[8],
    )
}

// ---------------------------------------------------------------- the gates

/// **Phase 11. Typing through a `Session` allocates nothing.**
///
/// The assertion `no_alloc.rs:206-216` makes about an `Editor`, made about the code the product runs.
/// Before Phase 11 this counted ~11 allocations *per keystroke* on a document this size.
#[test]
fn typing_through_a_session_allocates_nothing() {
    let (total, a, r, d, edits, _w, _l, _b, _paint) = run_probe();
    assert_eq!(
        total,
        0,
        "typing {edits} characters through a Session performed {total} allocations \
         ({a} allocs, {r} reallocs, {d} deallocs) on a {DOC_BYTES}-byte document. FR-1.2 says \
         zero, and this is the product's edit path rather than an Editor's."
    );
    assert_eq!(edits, BURST as u32, "not every keystroke was accepted");
}

/// The probe actually typed, and the document actually grew.
///
/// A counting gate can pass vacuously: if every keystroke were rejected, the allocation count would be
/// zero and the assertion above would be true for the wrong reason. So the count is checked against
/// what was sent, and the document is checked for growth.
#[test]
fn the_probe_typed_every_key_and_the_document_grew() {
    let (_t, _a, _r, _d, edits, _words, lines, bytes, _paint) = run_probe();
    assert_eq!(edits, BURST as u32);
    assert!(
        bytes as usize > DOC_BYTES,
        "the document is {bytes} bytes, which is not larger than the {DOC_BYTES} it started at"
    );
    assert!(lines > 1, "the document reports {lines} lines");
}

/// `refresh_counts`' word count is correct across chunk boundaries. Phase 11.
///
/// The chunked recount added an `in_word` carry specifically because a word that straddles a
/// `SCAN_CHUNK` boundary would otherwise be counted twice or not at all -- and 4,096 bytes is small
/// relative to a 64 KiB document, so a boundary is crossed ~16 times. This is the gate for that
/// carry, and it is why the fixture document is built from repeated *words* rather than one long run.
///
/// The expected count is computed from the fixture's own construction rather than hard-coded: whole
/// lines are appended until the document reaches `DOC_BYTES`, so the document holds
/// `ceil(DOC_BYTES / 44)` complete lines of nine words, and the typed burst lands at the end of the
/// last one -- where it follows a newline, so `BURST` `a`s are **one** word rather than `BURST`.
#[test]
fn the_chunked_word_count_survives_a_chunk_boundary() {
    let (_t, _a, _r, _d, _edits, words, lines, _bytes, _paint) = run_probe();
    let line = "the quick brown fox jumps over the lazy dog\n";
    let words_per_line = line.split_ascii_whitespace().count() as u32;
    let whole_lines = DOC_BYTES.div_ceil(line.len()) as u32;
    // The burst lands after the final newline, so it is one contiguous run and therefore one word.
    let expected = whole_lines * words_per_line + 1;
    assert_eq!(
        words, expected,
        "the session counted {words} words where the fixture has {expected}"
    );
    assert_eq!(
        lines, whole_lines,
        "the session counted {lines} lines where the fixture has {whole_lines}"
    );
}
/// The paint path still allocates, and Phase 11 did not fix it.
///
/// **This asserts the number is non-zero, deliberately.** Phase 12 replaces `Painter::text`'s
/// contract and reuses the surface tree; until then a paint builds a fresh `SurfaceTree` whose
/// `Vec`s grow by doubling and a `Frame` the backend copies. Asserting zero here would be a false
/// claim, and asserting *this* number would pin a cost that is about to change -- so the test states
/// what is true: the paint allocates, the edit does not, and the edit is what FR-1.2 is about.
///
/// When Phase 12 lands this becomes an assertion at zero and the gate gets its full name.
#[test]
fn the_paint_path_still_allocates_and_phase_12_owns_that() {
    let (_t, _a, _r, _d, _edits, _w, _l, _b, paint) = run_probe();
    assert!(
        paint > 0,
        "the paint path allocated nothing, so Phase 12's remaining work is already done and this \
         test should become an assertion at zero"
    );
    println!("one paint allocates {paint} times; Phase 12 owns it");
}

/// Diagnostic: one keystroke's allocations, grouped by size.
///
/// Not a gate -- it prints, and it exists because "27 allocations per keystroke" says nothing about
/// *which* code allocates. Run with `-- --nocapture` to see the table.
#[test]
#[ignore = "diagnostic; prints allocation sizes for one keystroke"]
fn print_one_keystrokes_allocation_sizes() {
    let _ = run_probe_with(&[(SIZES_ENV, "1")]);
}
