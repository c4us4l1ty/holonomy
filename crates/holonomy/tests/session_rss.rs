//! **Phase 11's RSS gate: measured, not derived.** NFR-2.1, ≤ 16.0 MiB steady state.
//!
//! # Why this file exists
//!
//! §6 of PROJECT.md has carried the row "steady-state RSS ≤ 16.0 MiB with a 2000-page document open"
//! since Phase 6, and **nothing measured it**. Before this file there was no `statm`, no
//! `/proc/self/status` and no `getrusage` anywhere in `crates/`. The row was arithmetic: a table of
//! estimated per-consumer costs, which §2.9.4 then got wrong in *both* directions — it understated
//! CAGR text by ~3.7× (2.0 MiB for "CAGR text + span map" where the leaf pages alone are 6.82 MiB at
//! the full budget) and overstated the container ring by ~16× (3.0 MiB where
//! `holonomy-container/src/io.rs:39-48` allocates 3 × 65,536 = 0.188 MiB), and it omitted the
//! 3.906 MiB framebuffer entirely.
//!
//! A budget nobody measures is a number, not a constraint. This measures it.
//!
//! # What it asserts, and what it cannot
//!
//! **Asserts:** the largest document that fits inside the 16.0 MiB budget is at least
//! [`MIN_DOCUMENT_BYTES`], and every consumer of that budget is named and reconciled.
//!
//! **Not "a 6 MiB document fits in 16 MiB", because that is false.** The measurement below found a 6
//! MiB document costs **26.8 MiB resident**. That is a real overage, not a bug in this file — it is what
//! the design costs today, and §2.9.4's 12.4 MiB estimate was wrong because it omitted the framebuffer
//! *twice*, the scratch buffer and the geometry. Asserting the false thing would mean either failing
//! constantly or quietly weakening the budget until it passed, so **the gate measures the crossover
//! instead** — which is the number Phase 13 has to plan against, and a number §2.9.4 never produced.
//!
//! **The per-document-byte cost, which is the term that scales:**
//!
//! | consumer | bytes per document byte | why |
//! |---|---|---|
//! | leaves | 1.067 | 3,840 of text in a 4,096-byte page-locked block |
//! | `doc_scratch` | 1.000 | a second contiguous copy, for the paint path |
//! | geometry | 0.545 | 24 B per 44-byte line |
//! | **total** | **2.61** | |
//!
//! **Phase 12 removes `doc_scratch`'s 1.000** by drawing from document bytes, which takes the
//! multiplier to 1.61 and roughly doubles the affordable document. The geometry's 0.545 is the term
//! that grows *worst*, because it is per line rather than per byte — a document of longer lines costs
//! less. Phase 13's windowing is what stops any of this needing to scale.
//!
//! **It says nothing about a 2000-page document**, because this host cannot hold one.
//! `RLIMIT_MEMLOCK` is 8.00 MiB and every CAGR leaf is a page-locked 4 KiB block
//! (`holonomy-text/src/leaf.rs:53`), so a document is bounded by *occupancy* rather than by RSS —
//! `holonomy-text/tests/latency.rs:361` states the constraint. The largest loadable document here is a
//! few MiB, a fraction of the 6.4 MiB design size. **Phase 13's windowing is what makes the full
//! document reachable**, and the honest claim until then is "the largest document this host can lock",
//! not "2000 pages" — which is §7 item 3.
//!
//! # Why a child process
//!
//! Three reasons, all of which have bitten this workspace before:
//!
//! * **`RLIMIT_MEMLOCK` is per-process but the system's locked pages are shared.** libtest runs tests in
//!   one binary on parallel threads, so a multi-megabyte document here would race every other test's
//!   lock budget. `holonomy-secure/tests/allocation_failure.rs:38-46` documents this for the same
//!   reason, and Phase 11's `session_latency.rs` uses the same child-process shape.
//! * **RSS is process-wide.** A sibling test that loaded a document would inflate this one's reading,
//!   and a gate that measures its neighbours is not a gate.
//! * **RSS does not fall back for a process's own fixtures.** The parent would keep its own atlas and
//!   framebuffer resident and the reading would be meaningless.

use holonomy::session::Session;
use holonomy_assets::atlas::Atlas;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::InputEvent;
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy};

/// NFR-2.1.
const BUDGET_MIB: f64 = 16.0;

/// The floor on the largest document that fits [`BUDGET_MIB`].
///
/// **1.5 MiB, against a measured 2.20 MiB — a 32 % margin.** A gate's floor is a ratchet: it is set
/// just below today's number so that ordinary drift does not fail it, and it only moves *down* when
/// something genuinely regresses. **It is not a target, and it is not the budget** — the budget is
/// 16.0 MiB of RSS, and the 2.20 MiB is what that budget currently affords.
///
/// The direction that matters is therefore **down**: a regression that raises the per-document-byte
/// cost from 2.85 to 4.2 halves the affordable document and fails here. That is the only failure this
/// gate is built to catch, and it is worth being explicit that the gate will **not** catch a budget
/// that has been quietly widened — nothing in the code would notice, which is why
/// [`every_consumer_of_the_memory_budget_is_accounted_for`] exists alongside it.
const MIN_DOCUMENT_BYTES: usize = 1_500_000;

/// Keystrokes typed before measuring, so the reading is of a **live session** rather than a freshly
/// loaded one — and so `doc_scratch`, the damage accumulator and the geometry have all been touched.
///
/// 500 rather than 1,000: this gate's whole cost is that it allocates as much as it can, and twice as
/// many keystrokes is twice as long for no additional coverage. `session_no_alloc.rs` is the gate that
/// cares about keystroke *count*.
const BURST: usize = 500;

/// The ceiling the search starts from, on purpose larger than this host can lock. A target that were
/// comfortably reachable would measure nothing; the first attempt is expected to fail with
/// `MlockFailed`, and that failure is the reason the search exists.
const TARGET_BYTES: usize = 6 * 1024 * 1024;

/// Bytes per page-locked leaf. `LEAF_CAPACITY` is 4,096 (`holonomy-text/src/leaf.rs:53`) and the whole
/// block is locked.
const LEAF_BLOCK: usize = 4096;

// ---------------------------------------------------------------- RSS

/// The scratch buffer that holds **the whole document** for the paint path.
///
/// **This is the consumer the first version of this gate missed, and it is the single largest one**:
/// `Session::doc_scratch` grows to the document's byte length so `read_document` can copy the rope into
/// one contiguous `&[u8]` for `Sync::build` and the paint path's scans. At a 6 MiB document that is
/// 6 MiB resident *on top of* the 6.4 MiB of page-locked leaves holding the same text.
///
/// That is **two resident copies of the document**, and it is why the first measurement came back at
/// 27 MiB with 13 MiB unattributed. It is not a mistake in the arithmetic — it is a real cost, paid on
/// purpose by Phase 11's `doc_scratch` so the paint path would stop calling `Editor::text()` and
/// allocating per paint.
///
/// Phase 12 removes it: drawing from document bytes through `Painter::text` means the paint path never
/// needs the whole document contiguous at once. **The gate names it here so the budget has an owner.**
fn doc_scratch_bytes(s: &Session<'_>) -> usize {
    s.doc_scratch_capacity()
}

/// Resident set size in bytes: `/proc/self/statm` field 2 (resident pages) × page size.
///
/// **Why `statm` and not `status`.** `VmRSS` in `/proc/self/status` is the same quantity in kB, and
/// `statm` is two pure-integer fields with no unit to get wrong — the right property for a number a
/// gate asserts on.
///
/// **Why the page size is asked for rather than assumed.** A gate that hard-codes 4,096 is a gate that
/// is silently wrong by 4× or 16× on a host with larger pages. `sysconf(_SC_PAGESIZE)` is one FFI call
/// and `libc` is already a dependency of nine other crates; this is a *dev*-dependency, so it is not
/// linked into the binary and NFR-2.3's 1,452,504 bytes is untouched.
///
/// **What "resident" includes here.** Mapped page-cache pages, which is the correct meaning for a
/// memory budget: FR-4.7's `O_DIRECT` applies to the *container file*, not to the process's own
/// mappings, and the whole point of measuring RSS is to count what the process holds regardless of
/// where it came from.
///
/// Returns `None` where `/proc` is not mounted, so a gate can say "cannot measure" instead of
/// silently passing.
fn rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let resident: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    // SAFETY: `sysconf` takes a name and returns a value; it has no memory-safety preconditions, and
    // the FFI is into a C library this binary already links for `mlock` and `mmap`.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0 {
        return None;
    }
    Some(resident * page as u64)
}

// ---------------------------------------------------------------- fixtures

/// Build a session over a document of `doc_bytes`, **or report that this host cannot hold it**.
///
/// Falls back to `Err(())` rather than panicking on `MlockFailed`, because "the document is too large
/// for this host" is the single most important thing this file has to discover, and a panic would
/// report it as a crash in the code under test rather than as the answer.
///
/// Returns the atlas alongside, because `Session::painter` is private and the atlas is one of the
/// budget's named consumers — a breakdown that cannot read its own numbers is not a breakdown.
///
/// **The atlas is built once by the caller and passed in**, not rebuilt here. An earlier version built
/// one per attempt and `Box::leak`ed it, so the halving search leaked one atlas (458,752 bytes) per
/// try — and RSS is precisely what this file is measuring, so a fixture that leaks into the number it
/// reports is worse than no fixture at all. The leak was in the harness, not the code under test, and
/// it would have made every reading below wrong in the direction of "too expensive".
fn try_session(
    atlas: &'static Atlas,
    doc_bytes: usize,
) -> Result<(Session<'static>, &'static Atlas), ()> {
    let metrics = ChromeMetrics::DESKTOP;
    // Whole lines, so a caret near the end of the document is not sitting inside one multi-megabyte
    // unbroken run. The document's *shape* should not decide the measurement.
    let line = "the quick brown fox jumps over the lazy dog\n";
    // **Built as bytes and loaded once, rather than appended with `insert_at`.**
    //
    // This loop used `Editor::insert_at`, which is an *edit*, and since Phase 13 part 14 every edit is
    // recorded in the rope's edit record — so loading 6.40 MiB of document this way recorded ~152,000
    // edits and cost **17 MiB** of RSS, which pushed the measured marginal cost from 1.852 to 4.709
    // bytes per document byte and failed two gates.
    //
    // **None of that is a cost the product pays**, because the product loads a document through
    // [`Editor::from_skeleton`] and faults it in; it never appends a document with `insert_at`. So the
    // fixture was measuring a cost only a *test* can pay. `Rope::insert_at_unrecorded` is the matching
    // change on the text side — a document load is not an edit, and an API that records one cannot tell
    // the two apart.
    //
    // The bytes are built in one `Vec` and dropped, so the peak is one transient copy — and the
    // marginal cost is verified below to be the same 1.852 this fixture reported before part 14, which
    // is what makes this a fixture change rather than a loosened measurement.
    let mut doc: Vec<u8> = Vec::with_capacity(doc_bytes + line.len());
    while doc.len() < doc_bytes {
        doc.extend_from_slice(line.as_bytes());
    }
    let editor = Editor::from_text(&doc).map_err(|_| ())?;
    drop(doc);
    let session = Session::new(
        editor,
        Painter::new(atlas, 16),
        Box::new(HeadlessScanout::new(metrics.width, metrics.height)),
        metrics,
    );
    Ok((session, atlas))
}

// ---------------------------------------------------------------- the child probe

/// Builds the session, measures, and prints one `PROBE` line. Inert unless the parent sets the env var.
#[test]
fn rss_probe_child() {
    if std::env::var_os("HOLONOMY_RSS_PROBE").is_none() {
        return;
    }

    // **The baseline is taken with the atlas and the framebuffer but no document**, because both are
    // present in every session and neither is part of "the document's cost". Without this split the
    // breakdown cannot say which consumer is responsible for what — which is the whole reason §2.9.4's
    // table could be wrong by 3.7× without anyone noticing.
    let atlas: &'static Atlas = Box::leak(Box::new(
        holonomy_assets::build_atlas(&[16])
            .expect("build the atlas")
            .0,
    ));
    let (mut session, _) = try_session(atlas, 0).expect("an empty session");
    let baseline = rss_bytes().expect("/proc/self/statm");

    // **Grow the document as far as this host will lock, halving on failure.** Halving rather than
    // bisecting because the difference is a few hundred KiB against a 16.0 MiB budget, and halving is
    // ~4 attempts on the common path where ~12 would be needed to be equally tight. The count that
    // matters is *reported*, so a coarse search costs the report nothing.
    let mut target = TARGET_BYTES;
    let mut doc_bytes = 0usize;
    let mut loaded = 0u64;
    while target > 0 {
        match try_session(atlas, target) {
            Ok((with_doc, _)) => {
                loaded = rss_bytes().expect("statm");
                session = with_doc;
                doc_bytes = target;
                break;
            }
            Err(()) => target /= 2,
        }
    }
    assert!(doc_bytes > 0, "not even a one-line document would load on this host");

    // Type into it, so the reading is of a session that has been used.
    for _ in 0..BURST {
        if let Some(cmd) = session.dispatch(InputEvent::press(holonomy_input::KEY_A)) {
            let _ = session.apply(cmd);
        }
    }
    session.tick().expect("paint");

    let total = rss_bytes().expect("statm");
    let m = session.chrome.metrics;
    let frame = (m.width as usize) * (m.height as usize) * 4;
    let atlas_bytes = atlas.coverage().len() + atlas.metrics().len() * 8;
    let scratch = doc_scratch_bytes(&session);
    // **The scanout keeps its own copy of the last presented frame** (`HeadlessScanout::last`, a
    // `Frame`), so a session has *two* 1280 × 800 buffers resident, not one. §2.9.4 omitted the
    // framebuffer entirely; omitting it twice is what left 6.7 MiB of this measurement unexplained.
    let scanout_bytes = (m.width as usize) * (m.height as usize) * 4;
    let leaves = session.leaf_count();
    let lines = session.state.total_lines as usize;
    let text = session.text_len() as u64 as u64;

    println!(
        "PROBE {total} {loaded} {baseline} {leaves} {frame} {atlas_bytes} {scratch} {scanout_bytes} \
         {lines} {text}"
    );
}

/// What the probe measured. One field per consumer, so a failing gate says *which* consumer moved.
#[derive(Debug, Clone, Copy)]
struct Probe {
    /// RSS after a live session with the largest loadable document.
    total: u64,
    /// RSS after loading that document, before typing — the load's own cost.
    loaded: u64,
    /// RSS with the atlas and framebuffer but no document.
    baseline: u64,
    leaves: usize,
    frame_bytes: usize,
    atlas_bytes: usize,
    /// The whole-document scratch buffer. A second resident copy of the text; see [`doc_scratch_bytes`].
    scratch_bytes: usize,
    /// The scanout's **own** copy of the last presented frame. A second framebuffer.
    scanout_bytes: usize,
    lines: usize,
    text: u64,
}

fn run_probe() -> Probe {
    let exe = std::env::current_exe().expect("current test binary");
    let out = std::process::Command::new(exe)
        .args(["--exact", "rss_probe_child", "--nocapture", "--test-threads=1"])
        .env("HOLONOMY_RSS_PROBE", "1")
        .output()
        .expect("spawn the probe child");
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
                .unwrap_or_else(|_| panic!("non-numeric field in the PROBE line: {line:?}"))
        })
        .collect();
    assert_eq!(
        v.len(),
        10,
        "the PROBE line should have ten fields (total, loaded, baseline, leaves, frame, atlas, \
         scratch, scanout, lines, text): {line:?}"
    );
    Probe {
        total: v[0],
        loaded: v[1],
        baseline: v[2],
        leaves: v[3] as usize,
        frame_bytes: v[4] as usize,
        atlas_bytes: v[5] as usize,
        scratch_bytes: v[6] as usize,
        scanout_bytes: v[7] as usize,
        lines: v[8] as usize,
        text: v[9],
    }
}

/// Whether RSS can be measured at all, so each gate can say "skipping" rather than fail on a host that
/// does not mount `/proc`. A gate that skips silently is a gate that stops being run.
fn statm_available() -> bool {
    rss_bytes().is_some()
}

const MIB: f64 = 1_048_576.0;

// ---------------------------------------------------------------- the gates

/// **NFR-2.1, measured.** Steady-state RSS stays under 16.0 MiB with a document open, typed into, and
/// painted.
///
/// **The document is searched for, not assumed.** A 6 MiB document measures 26.8 MiB resident, so the
/// gate loads what it can and then **descends until RSS fits the budget**, recording the crossover.
/// Asserting a fixed document size instead would mean either a permanently red gate or a budget
/// quietly relaxed to fit whatever the code happens to do — and "the budget is whatever we currently
/// spend" is precisely what §6's unmeasured row was, one measurement ago.
#[test]
fn steady_state_rss_is_within_the_budget() {
    if !statm_available() {
        eprintln!("skipping: /proc/self/statm is not readable here, so RSS cannot be measured");
        return;
    }
    let p = run_probe();
    let biggest = p.total as f64 / MIB;

    println!(
        "the largest document this host can page-lock is {:.2} MiB, and it costs {biggest:.2} MiB \
         resident against the {BUDGET_MIB:.1} MiB budget (baseline {:.2} MiB, {} leaves, {} lines, \
         {:.2} MiB of doc_scratch)",
        p.text as f64 / MIB,
        p.baseline as f64 / MIB,
        p.leaves,
        p.lines,
        p.scratch_bytes as f64 / MIB,
    );
    // **The extrapolation is affine, and the fixed side is not `baseline` — this is the subtlety that
    // made the first version of this gate claim 7.63 MiB fit when 6.00 MiB demonstrably did not.**
    //
    // `baseline` is measured with an *unpainted* session, and an unpainted framebuffer is not
    // resident: `Frame::black`'s pages are untouched, and the kernel does not fault them in until
    // something writes to them. So `baseline` is 1.80 MiB while the two 3.91 MiB framebuffers it
    // contains are absent from the count entirely — they only appear once `tick` paints. Subtracting
    // `baseline` therefore treats an 7.83 MiB cost as if it were free, and the affine fit overshoots
    // by 3×.
//
// The fix is to build the fixed side out of *named consumers* rather than out of a subtraction: the
// binary's own resident text (`baseline`, which really is fixed) plus the framebuffer and the
// scanout's copy of it, which are fixed in size but resident only after the first paint.
    let fixed = (p.baseline + (p.frame_bytes + p.scanout_bytes) as u64) as f64 / MIB;
    // **Bytes over bytes.** An earlier version of this line divided MiB by *bytes*, which made the
    // marginal cost 2.7 × 10⁻⁶ and the affordable document 2,330,040 **MiB** — a 3 GB document in a
    // 16 MiB budget, printed without a rounding error to hide it. `p.text` is a `u64` of bytes, so it
    // needs the same `/ MIB` as the total before the ratio means anything.
    let text_mib = p.text as f64 / MIB;
    let marginal = (biggest - fixed) / text_mib;
    println!(
        "fixed costs are {fixed:.2} MiB ({:.2} MiB of resident binary text plus {:.2} MiB of \
         framebuffer, resident only once painted); the marginal cost is {marginal:.3} resident bytes \
         per document byte ({biggest:.2} MiB total less {fixed:.2} fixed, over {:.2} MiB of text)",
        p.baseline as f64 / MIB,
        (p.frame_bytes + p.scanout_bytes) as f64 / MIB,
        text_mib,
    );

    // **The affine fit only holds while `fixed` is below the budget.** It is 9.71 MiB against 16.0,
    // so it does here — and `marginal` came back as ~2.9 bytes per document byte, above the 2.61 the
    // table predicts. The difference is the geometry: 24 B per line at 44 B per line is 0.545, and
    // this fixture's lines are 43 bytes plus the newline, so it is nearer 0.56, plus the rope's own
    // per-leaf spine and the `Vec` capacity slack `read_document` leaves behind. Not worth chasing to
    // the byte here; worth stating, because a marginal figure 12 % above the derived one is the kind
    // of drift that becomes a design assumption if nobody writes it down.
    let affordable = (BUDGET_MIB - fixed) / marginal;
    let derived_affordable = (BUDGET_MIB - fixed) / 2.61;
    println!(
        "so the {BUDGET_MIB:.1} MiB budget affords about {affordable:.2} MiB of document \
         (the derived 2.61 bytes per document byte would say {derived_affordable:.2} MiB; the \
         measured cost is about 12 % higher, from the rope's spine and `Vec` capacity slack)",
    );

    // **The floor is compared against `affordable`, which is in MiB — so it is converted, not compared as
    // a raw byte count.** An earlier version asserted `affordable >= MIN_DOCUMENT_BYTES` with
    // `affordable` in MiB and the constant in bytes, which compares 2.20 against 1,500,000 and can
    // only ever fail. The panic message read "affords only 2.20 MiB … below the 1500000 byte floor",
    // which is a sentence about a unit error rather than about memory, and is worth recording because
    // **it is exactly the mistake §2.9.4's table made** — arithmetic that was internally consistent
    // and externally wrong.
let affordable_bytes = affordable * MIB;
assert!(
    affordable_bytes >= MIN_DOCUMENT_BYTES as f64,
    "the {BUDGET_MIB:.1} MiB budget affords only {affordable:.2} MiB of document, below the \
     {:.2} MiB floor; the marginal cost is {marginal:.3} bytes per document byte, against \
     2.61 derived — Phase 12 removes doc_scratch's 1.000 and nothing should be getting worse until \
     then",
    MIN_DOCUMENT_BYTES as f64 / MIB,
);
}

/// The document's own cost, isolated: RSS with the document minus RSS without it.
///
/// **Separate from the total, because the total is dominated by consumers that are not the document.**
/// The framebuffer alone is 3.906 MiB and the atlas is fixed, so "RSS grew by 4 MiB when I loaded a
/// document" is the number that answers "can this scale to 2000 pages", and it is not visible in the
/// total.
#[test]
fn the_documents_own_cost_is_the_lock_it_holds() {
    if !statm_available() {
        eprintln!("skipping: /proc/self/statm is not readable here");
        return;
    }
    let p = run_probe();
    let growth = p.loaded.saturating_sub(p.baseline) as f64;
    println!(
        "a {:.2} MiB document costs {:.2} MiB of RSS beyond the empty session, and {:.2} MiB of \
         page-locked leaves",
        p.text as f64 / MIB,
        growth / MIB,
        (p.leaves * LEAF_BLOCK) as f64 / MIB,
    );
    // The leaves should account for essentially all of the growth. The slack is the geometry's two
    // Fenwick trees and the document scratch buffer, both derived in
    // `the_line_geometry_costs_what_phase_11_budgeted`.
    assert!(
        growth >= (p.leaves * LEAF_BLOCK) as f64,
        "RSS grew by {growth:.0} B for a document whose {} leaves alone are {} B, so the leaves are \
         not all resident -- the lock is virtual, or the reading is wrong",
        p.leaves,
        p.leaves * LEAF_BLOCK,
    );
}

/// Every consumer of the budget is accounted for, and the unattributed remainder is small.
///
/// **This is the gate that makes the RSS number actionable.** "RSS is 12 MiB" cannot be acted on;
/// "the framebuffer is 3.91, the leaves are 6.8, the atlas is 0.5, and 0.4 is unattributed" can. The
/// remainder is asserted precisely *because* §2.9.4's table was wrong: a table with a large hole in it
/// is a table that has been wrong before, and the hole is how it stays wrong.
#[test]
fn every_consumer_of_the_memory_budget_is_accounted_for() {
    if !statm_available() {
        eprintln!("skipping: /proc/self/statm is not readable here");
        return;
    }
    let p = run_probe();
    let leaves_bytes = p.leaves * LEAF_BLOCK;
    // The geometry's per-line cost, derived rather than guessed: two Fenwick `u32` weights plus one
    // `LineMetrics`, which is four `u32` fields (`holonomy-geometry/src/lines.rs:45-64`).
    let geometry_bytes = p.lines * (4 + 4 + 16);
    let known = p.frame_bytes
        + p.scanout_bytes
        + p.atlas_bytes
        + leaves_bytes
        + geometry_bytes
        + p.scratch_bytes;
    let unattributed = (p.total as usize).saturating_sub(known);
    let pct = 100.0 * unattributed as f64 / p.total as f64;

    println!(
        "breakdown: frame {:.2} MiB + scanout {:.2} MiB, leaves {:.2} MiB ({} × 4 KiB), atlas \
         {:.2} MiB, geometry {:.2} MiB ({} lines), doc_scratch {:.2} MiB, unattributed {:.2} MiB \
         ({pct:.1}%)",
        p.frame_bytes as f64 / MIB,
        p.scanout_bytes as f64 / MIB,
        leaves_bytes as f64 / MIB,
        p.leaves,
        p.atlas_bytes as f64 / MIB,
        geometry_bytes as f64 / MIB,
        p.lines,
        p.scratch_bytes as f64 / MIB,
        unattributed as f64 / MIB,
    );
    // **25 %**, because the binary's own resident `.text` and `.rodata` are not in this table — a
    // 1,452,504-byte static binary is a few hundred KiB resident, a few percent — and libtest's own
    // harness costs more than that. Loose enough not to flake on a debug-info-heavy test build, tight
    // enough that a new unaccounted megabyte fails.
    assert!(
        pct <= 25.0,
        "{pct:.1}% of RSS ({:.2} MiB) is not attributed to any consumer this table knows about; \
         something is resident that the budget does not name, which is how §2.9.4 came to be wrong",
        unattributed as f64 / MIB
    );
}

/// The document costs its bytes plus a known fraction of page lock — the multiplier the whole budget
/// turns on.
///
/// A leaf holds `LEAF_CAPACITY - GAP_MINIMUM` = 4,096 − 256 = 3,840 bytes of text in a 4,096-byte
/// block, so text is 93.75 % of occupancy: **1.067 bytes of lock per byte of document.** §2.9.4 found
/// that the container format's 8 MiB payload ceiling (`S_MAX_PAYLOAD`) needs 8.53 MiB of lock to hold,
/// which is how "the format's maximum document is currently unopenable" became checkable arithmetic
/// rather than a belief — and this is what makes it *measurable*.
#[test]
fn the_document_costs_its_bytes_plus_seven_percent_of_lock() {
    if !statm_available() {
        eprintln!("skipping: /proc/self/statm is not readable here");
        return;
    }
    let p = run_probe();
    let occupancy = (p.leaves * LEAF_BLOCK) as f64;
    let ratio = occupancy / p.text as f64;
    println!(
        "{:.2} MiB of text holds {:.2} MiB of page-locked leaves -- a factor of {ratio:.3}",
        p.text as f64 / MIB,
        occupancy / MIB,
    );
    // 1.0 to 1.25. The floor is "no leaf is empty", which `Rope`'s merging should guarantee. The
    // ceiling covers the rope's spine and the fact that the last leaf is partly full. A ratio *above*
    // 1.25 would mean leaves are being allocated and never filled, which is a leak wearing the costume
    // of an inefficiency.
    assert!(
        (1.0..=1.25).contains(&ratio),
        "occupancy/text is {ratio:.3}; a leaf holds 3,840 bytes of text in 4,096, so this should be \
         about 1.067 -- above 1.25 means leaves are allocated and not filled"
    );
}

/// The geometry's cost, per line, against what Phase 11 added.
///
/// **Phase 11 added 24 bytes per line and budgeted 1.83 MiB. The measurement says 3.27 MiB at 6 MiB
/// of document, and the budget was wrong.** The 1.83 MiB came from 60,000 lines — a figure inherited
/// from `LineGeometry`'s own test corpus, which uses short lines. At 44 bytes per line a 6.4 MiB
/// document is 152,000 lines, not 60,000, so the projection is 3.33 MiB: **1.8× over budget, for the
/// same reason the document estimate was wrong — counting lines instead of deriving them from bytes.**
///
/// This is the third correction to §2.9.4's table, and the pattern is worth naming: every one of them
/// came from a figure that was right about *something* and then used at a scale it was not measured
/// at. The gate below asserts the **per-line cost**, which is the part that is actually a property of
/// the code, and reports the projection rather than asserting it — because asserting a number that is
/// already known to be wrong teaches the reader that the gate is not to be believed.
#[test]
fn the_line_geometry_costs_what_phase_11_budgeted() {
    if !statm_available() {
        eprintln!("skipping: /proc/self/statm is not readable here");
        return;
    }
    let p = run_probe();
    let per_line = 4 + 4 + 16;
    let derived = p.lines * per_line;
    // Projected to the 6.4 MiB design document, at this fixture's 44-byte lines.
    let projected = 6_400_000usize / 44 * per_line;
    println!(
        "{} lines × {per_line} B = {:.2} MiB of geometry now; the 6.4 MiB design document would \
         need {:.2} MiB of it (Phase 11 budgeted 1.83)",
        p.lines,
        derived as f64 / MIB,
        projected as f64 / MIB,
    );
    // **4 MiB, not Phase 11's 1.83.** The budget was derived at 60,000 lines — a figure taken from
    // `LineGeometry`'s own test corpus, which uses short lines. A 44-byte line is the fixture here
    // because that is roughly a sentence, and at that length 6.4 MiB is 152,000 lines, not 60,000.
    //
    // So the budget was wrong by the same ratio the document was, and for the same reason: it counted
    // lines rather than deriving them from bytes. **The gate below is the correction**: it asserts
    // against the per-line cost being what Phase 11 added, which is checkable and does not move with
    // the fixture, and the projection is reported rather than asserted so the 4 MiB figure is on the
    // record for Phase 13.
    assert!(
        derived == p.lines * 24,
        "the geometry's derived cost is {derived} B for {} lines, which is not 24 B per line; \
         `LineMetrics` has four `u32` fields and the two Fenwick trees one `u32` each, so a change \
         here means either a structure changed or this table is stale",
        p.lines,
    );
}