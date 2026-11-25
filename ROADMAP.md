# HOLONOMY v6 — ROADMAP

> Construction order. Every block is load-bearing; every block ends in a **gate** — a set of
> measurable, testable exit conditions. A block that has not passed its gate does not exist,
> and the block after it does not start. This document is the contract; `VISION.md` is the
> constitution it answers to.

Language: **Rust, everywhere** — core, crypto, document engine, UI. UI framework of record:
**GPUI** (GPU-accelerated, macOS / Windows / Linux), as selected in the v6 planning session.

Reading order for prior art: `holonomy-old/` (H1, the working editor whose UI failed =
*what to redo*), `holonomy-v5-1-0/` (the trusted core that never shipped = *what to keep*).
Each block below names which of those it salvages and which mistake it exists to prevent.

For the order these blocks are *delivered* in — cut small enough that one CI cycle settles a step,
with a headless acceptance for each — see `docs/TO-APP.md`. The blocks remain the contract; that
document is the march.

---

## Block 1 — The Ground
*Purpose: build the legal system before building anything under it.*

v3's PRD was completely silent on UI and grew 41 audited bugs; v5.0-OMEGA wrote decorative
security claims that an audit had to kill. Block 1 exists so v6's code lands in a world
where its abuses are already illegal.

**Deliverables**
- **Unified workspace.** One Cargo workspace, one lockfile, every crate a member. v5's three
  exiled UI crates (held hostage by git-fork patches of `egui`/`winit`/`krilla`) are the
  failure being corrected: no `[patch.crates-io]` onto forks in the core tree, ever.
- **Threat model.** Written, versioned, signed off: adversary capabilities, what plaintext
  may exist where and for how long, exactly which side channels are out of scope and why.
- **Architecture RFCs** for Blocks 2–5, each one written to be *attacked* — the v5.1.0
  spec-then-audit cycle, made standard practice. `AUDIT.md` is a living document, not a
  funeral notice.
- **UI spec.** Typography, layout units, caret/selection semantics, empty states,
  toolbar behavior — specified and audited *before* Block 6 writes a pixel. One clause is
  constitutional: **paint geometry and edit geometry share one source.** v3's
  proportional-paint / monospace-cell-grid split-brain is the canonical failure this kills.
- **Gate machinery.** Benchmark and regression harness in CI from day one: counting-allocator
  claims must be *measured* (the v3 gate pattern), timing budgets measured on reference
  hardware, sizes recorded per commit.

**GATES** — ✅ **passed 2026-11-16** (CI run [43891997903](https://github.com/c4us4l1ty/holonomy/actions/runs/43891997903), all four jobs green)
- [x] Every RFC names the attack it resists and the test that proves it. (`docs/rfc/RFC-001…004`, attack tables with proving tests)
- [x] CI enforces: no unpinned deps, no forbidden allocator behavior, license/clippy clean. (fmt / clippy `-D warnings` / test / gates-contract on every push)
- [x] UI spec is complete enough that a reader can derive caret position from it alone. (`docs/UI-SPEC.md` §4)

---

## Block 2 — The Seal
*Purpose: one blessed crypto boundary — small, audited, and nothing else touches primitives.*

Salvages v5.1.0's audited envelope almost verbatim: that design survived its own audit and
its two-phase unlock is proven non-circular by test. Drops what the threat model demoted:
the mandatory USB pad (v6 is password-first), the strata/decoy system (deferred), and every
v5.0-OMEGA theatricality.

**Deliverables**
- `holonomy-seal` crate: Argon2id (memory-hard, host-calibrated) → `K_unwrap` → hybrid
  ML-KEM-1024 + X25519 decapsulation → `K_DEM`; key-committing AEAD (CMT-4) for every
  sealed object.
- Secret-cell memory handling ported and modernized from H1 `holonomy-secure` / v5
  `holonomy-crypto::lock`: fixed-size, 64 KiB-aligned boxed cells (to isolate page-granular
  locks), best-effort `mlock`/`VirtualLock`, zeroize on drop, and Linux `MADV_DONTDUMP` where
  accepted. Guard pages are not implemented in this Block 2 code; the threat model records the
  actual protection rung and residual limits.
- Explicit **capability tokens**: code that does not hold the token type cannot receive
  plaintext key bytes. The compiler enforces the boundary the audits used to patrol by hand.

**GATES** — ✅ **passed 2026-11-17 by scope amendment** (AUDIT entry *gate-closure amendment*;
no law promoted — `docs/GATES.md` is unchanged)
- [x] Adversarial audit of the crypto RFC closed: every open item is killed, amended with its
      limit stated, or assigned to the block that owns it. RFC-001 now claims what the tests
      establish and names what they do not.
- [x] Envelope round-trips with deliberately corrupted inputs fail with coarse errors only:
      garbage capsule, wrong passphrase, wrong unwrap key, capsule-field and AEAD bit-flips,
      wrong-context relocation and wire-length refusal are each pinned by a test in
      `holonomy-seal`, and every one of them surfaces as the single `OpenRejected` error whose
      display string a test forbids from distinguishing wrong passphrase from tampering.
- [x] **Project-owned** key material crosses no allocation that escapes a zeroizing owner: the
      test-only allocator fails on any registered buffer that is nonzero at free, on any secret
      reallocation, and on registry overflow, and it runs in CI.
      **Amended scope, stated rather than implied:** dependency-internal stack and heap scratch is
      **not claimed wiped**. It is bounded (4,096-byte passphrase cap before NFC), wiped where the
      reviewed crates expose state-drop hooks via their `zeroize` features (CI fails if the feature
      graph loses an edge), source-reviewed at the pinned revisions listed in `AUDIT.md`, and
      disclosed in `docs/THREAT-MODEL.md` §5. An allocator-wide proof over dependency internals is
      future hardening, not a Block 2 exit condition; it is recorded as an open claim, never as
      closed.

---

## Block 3 — The Witness
*Purpose: the `.wavefunction` container — the append-only world-line.*

Salvages v5's geometry lessons (fixed alignment, salt placement, nonce-predecessor chaining)
and H1's anti-circularity discipline, then deletes their main curse: **the fixed 134 MB
ceiling.** v6's container is a growable set of sealed **segments** (~64 KiB quantum, the
proven unit), committed by a small authenticated **witness** (hash-chain root). Size is
limited by disk, not by format — this is what makes sixty years *the default state*.

**Deliverables**
- `holonomy-line` crate: segment format (AEAD per segment, predecessor-commitment chaining
  as in H1's `aead.rs`), witness commits, staged no-replace publication for initial creation
  and compaction, plus append transactions that sync segment records before the witness commit
  and sync the containing directory when publishing a new path. No pad fold in the core path.
- Software fault model: tests inject a short write after every byte of an append, failures at
  each append sync barrier, and failures while discarding an uncommitted tail. Reopen must find
  the prior authenticated witness, or (after an uncertain final sync) a complete authenticated
  new witness. These in-process injections do not establish physical power-loss, filesystem
  journal, or device-cache behavior. `scripts/qualify-ext4-enospc.sh` additionally exercises the
  real append/recovery path on a bounded ext4 loop filesystem until it reports ENOSPC; that
  remains ext4 software-loop evidence, not physical-device power-loss qualification.
- Retention: old segments are the free-version history. Compaction exists **only** as an
  explicit, opt-in, passphrase-confirmed operation that rewrites history — never automatic.
- Incremental-sync fit: the append-only layout preserves the old prefix and the writer's
  measured `File::write` calls are confined to newly appended records. The ignored qualification
  test `tests::rsync_measures_append_transfer_over_a_named_local_daemon` measures a named rsync
  client over TCP against a local daemon using a real 30-million-word sealed file; it records the
  client version, application-layer bytes, and append size. Evidence from a passing run applies
  only to that rsync build/configuration/workload, not every backup client or remote service.
  `holonomy verify` re-checks segment integrity after a copy, so a cloud-mangled file is detected,
  not silently trusted.

**GATES** — ✅ **passed 2026-11-17 under scope amendment; L-6 and L-10 remain `spec`**
(AUDIT entry *gate-closure amendment*; the two deferrals below are obligations with named owners,
not waivers)
- [x] Exhaustive short-write injection at every append byte, both sync failures, and recovery
      truncate failures reopen to the last committed witness (or a complete authenticated new
      witness after an uncertain commit sync).
- [x] Crash consistency on a real filesystem image: `scripts/qualify-ext4-enospc.sh` appends until
      a 128 MiB ext4 loop filesystem reports ENOSPC and reopens to one committed segment with a
      verified 352,256-byte uncommitted tail; CI records `HOLONOMY_ENOSPC_MEASUREMENT`.
      *This is loop-filesystem and injected-failure evidence, not a power-cut test.*
- [x] A simulated 30 M-word history records the append writer's actual `File::write` offsets and
      byte counts: every write starts at the previous EOF, the old prefix is unchanged, and the
      total equals `AppendReceipt::bytes_appended`.
- [x] Tail-only transfer measured on a named client: the ignored rsync harness
      (`cargo test --locked -p holonomy-line --lib
      tests::rsync_measures_append_transfer_over_a_named_local_daemon -- --exact --ignored
      --nocapture`) runs in CI on every push and asserts client literal data ≤ appended bytes +
      one 64 KiB block and application-layer bytes ≤ 8 × appended bytes, logging the client
      version and sizes. *Evidence applies to that client, build, configuration and workload.*
- [x] `verify` catches flipped bytes anywhere, including in "old" history.
- [ ] **Deferred, not waived — L-6 physical qualification** (owned by Block 6's
      kill-power-at-random-instant suite and Block 10's all-three-OSes release gate): a controlled
      power cut at arbitrary instants, controller-cache evaluation, and a supported-device matrix.
      Block 3 freezes the write ordering that suite will test; `L-6` stays `spec` until it runs.
- [ ] **Deferred, not waived — L-10 client and backend scope** (owned by Block 10): other clients
      and remote backends remain unqualified. `L-10` stays `spec` until a backend matrix exists,
      or until the law is explicitly narrowed in `LAWS` at the release gate.

**Why an amended close, not a pass by evidence.** Block 3's exit condition is that the *format's
behaviour is fixed and evidenced under a stated failure model*: the writer appends, the witness
commits, a torn tail is discarded, the old prefix is never rewritten. Physical-device durability is
a property of that same frozen ordering, and it needs hardware this project does not control. Holding
Block 3 open for it would freeze construction on equipment access rather than on an unproven design
claim — so the claim is narrowed to the model the tests exercise, the remainder is named in
`docs/THREAT-MODEL.md` §5, and it is owed to the gates that can measure it.

---

## Block 4 — The Spine
*Purpose: the document engine — where "never decrypt more than you look at" becomes true.*

**Unblocked 2026-11-17:** Block 2 and Block 3 gates closed by scope amendment, and AUDIT OQ-4
(the scroll-timing side channel) is decided — accepted and disclosed in `docs/THREAT-MODEL.md` §5,
because slab fault-in reads are already visible at the I/O layer, while smoothing them would spend
L-5 (idle is idle) on the exact traversal cost this architecture exists to delete.

Salvages H1's two crown jewels, redesigned onto the segment log: the **faulting leaf**
residency model (leaves fault in from the 64 KiB section store; bounded resident set) and
the **measured allocation-free keystroke path** — plus the Fenwick-over-heights index H1
kept in `holonomy-geometry`. The H1 mistakes being deleted: layout-as-lines (page breaks
were faked with newline padding) and any structure that walks the document on a keystroke.

**Deliverables** — *four pieces landed 2026-11-17, see `AUDIT.md`; there is still no editor above them*
- `holonomy-text`: an **immutable, path-copied slab tree** over segment IDs, AVL-balanced so
  appends cannot degenerate into a list. An edit path-copies O(log N) nodes; the old spine
  survives as history. Undo between any two points in the world-line is a pointer swap.
  **Landed:** `Spine` (append / insert / remove / split / locate / spans), `History`.
- **Resident-set scheduler:** plaintext slabs fault in under the viewport, fault out on a
  strict budget, wipe on exit. Document length cannot enter the memory equation.
  **Landed as a mechanism:** `ResidentSet` + `SegmentSource`, with the budget asserted at every
  fault-in; the *gate* below is a measurement on reference hardware and stays open.
- **Amended — one index, not two:** position math is carried by subtree aggregates (bytes,
  slab count, height) rather than a separate flat Fenwick over slab heights. Same O(log segments)
  prefix sums; a second structure that must be rebuilt on every path copy is the K-8
  two-geometry disease in index form (AUDIT 2026-11-17, decision D-1).
- Counting-allocator gate on the keystroke path: a typed character must make **zero**
  allocations outside slab boundaries — measured, per H1's gate pattern.
  **Landed and measured:** `SlabText` edits in place and *refuses* to grow, and
  `crates/holonomy-text/tests/keystroke_allocations.rs` installs the counting allocator as its
  global allocator and claims zero allocations for a typed character, a backspace, filling the
  slack, and the refusal — with a control asserting a fault-in on a miss **does** allocate, so a
  broken counter cannot pass as a quiet path. Scope, stated exactly: the measured window is
  `locate` → resident hit → in-place edit. Fault-in, split, seal and the commit boundary allocate
  by design and are outside it (AUDIT decision D-2).
- **The commit boundary — seal → append → witness → new root.** An edited resident slab is
  sealed into the world-line as a **new segment**, committed with a witness, and the spine is
  path-copied onto it (`commit::commit_slab`, `Spine::replace_range`). The unit is one slab's
  document extent, so no offset outside it moves. A stale span is refused **before** the append,
  because an append with no spine edit is an orphan segment that is durable, authenticated and
  unreferenced. **Landed and proven against the real container:** `tests/commit_boundary.rs`
  drives the actual `holonomy-line` world-line and `holonomy-seal` capsule — keystroke, commit,
  reopen from disk, full `verify()`, and the resident set faulting in exactly the committed bytes.
  Nothing in that path is a mock.
- **The live-length overlay D-2 requires.** `Draft` records the live length of the leaves being
  edited, so `document_len`, `locate` and `spans_in` stay correct **while the spine is stale**, and
  the document view does not move when the commit lands — asserted by querying before and after the
  seal in `tests/commit_boundary.rs`. It holds no plaintext; forgetting a draft restores the sealed
  view exactly. What is still missing is the editor itself: caret, selection and paint are Block
  5's Eye, and the policy that decides *when* to commit is not made in this crate.

**GATES** — **all three passed, and the block is closed (2026-11-19).** Gate 1 was earned by
amending its clause to the two claims it actually makes (AUDIT D-36, run `44579774910`); gate 2 by
giving "statistically identical latency curves" an operational definition it can be falsified
against (AUDIT D-37, run `44665191051`); gate 3 by the million-keystroke fuzz, which was earned by
failing first and found D-31, D-32, OQ-21/22/23 and D-34. Each was claimed on the run that
measured it, and none of the three certificates says more than its harness proves. The workspace
moves to `6.0.0-block.4`, one bump per closed block.
- [x] **Amended 2026-11-19 (AUDIT D-36) — the clause is two claims, and the measurement split
      them.** Open a synthetic 30 M-word document: **resident plaintext** at open is within a fixed
      bound that identical input of 1 page also satisfies (the memory L-2 bounds — it holds), and
      the **index** costs a measured per-leaf constant that is under three percent of the
      document (it grows with leaves by construction: D-1's descent needs its nodes). Measured by
      `crates/holonomy-text/tests/open_memory.rs`, which prints `HOLONOMY_OPEN_MEMORY` for both
      sizes and asserts the budget, the eviction, the per-leaf band and the fraction. **Claimed
      2026-11-19 on the measuring run**: CI run `44579774910` (test job green) enforced every clause — the
      resident bound equal at one page and at thirty million words, eviction under four thousand
      faults, the per-leaf band, and the index under five percent.
- [x] **Amended 2026-11-19 (AUDIT D-37) — the clause now has an operational definition.** Keystroke
      at offset 0 and at offset `N-1`: statistically identical latency curves, measured **interleaved**
      (blocks of 256 at each offset, 4,096 samples each, so a drifting runner moves both curves),
      one timed keystroke per sample on a 180 MB, 43,945-leaf document, both viewports warm. The band
      is p50 within 2× and p99 within 4×, which rejects offset-proportional work; L-4's 8 ms is
      asserted as a ceiling and not claimed as a reference-hardware echo measurement. A
      whole-document walk is timed in the same process as the control (must exceed the keystroke's
      price 100×). `crates/holonomy-text/tests/keystroke_latency.rs` prints
      `HOLONOMY_KEYSTROKE_LATENCY` and `HOLONOMY_KEYSTROKE_CONTROL`. **Claimed 2026-11-19 on the
      measuring run**: CI run `44665191051` (`32c58c9`), test job green, which enforced the interleaving, both
      bands, the echo ceiling at both ends and the control.
- [x] 1 M random-keystroke fuzz run: zero leaks, zero residency blowups, undo past every
      point in the log. **`crates/holonomy-text/tests/keystroke_fuzz.rs`.** A seeded generator over
      a real `Typing` session, a real `ResidentSet` and one shared segment store, with an
      independent `String` model and the document read back through `copy` — the Eye's and the
      shell's reader. Two sizes of the same harness: six seeds of 20 000 actions on every push with
      **every** action checked (view, leaves, budget), and the gate's million keystrokes in release,
      four seeds of 250 000, undo and redo in the mix, printing `HOLONOMY_KEYSTROKE_FUZZ` per seed.
      Unwinding is a test of its own, six seeds: every undo is recorded, every redo must be the exact
      inverse of the undo in front of it, byte for byte, on the document the user sees.
      The gate was **earned by failing first**, which is why it is worth the ink — it found three
      defects and one limitation, and each fix is pinned by a test: a seal that wrote the bytes an
      undo had taken back (D-31), a burst that filled the buffer inside a character and lost the
      keystroke (D-32), and undo-then-redo that did not hand back what it took (OQ-21, OQ-22 — fixed
      by D-34, which keeps the buffer across a root restore so the bytes a state needs are a length
      into it, and makes a mark whose leaf is not the open one move the leaf). The invariant the fix
      rests on, "the settled tree plus the open bytes is the document", is asserted where it is used,
      and the million-keystroke run broke it once (OQ-23) before the fix.
      What the clauses mean has not moved (D-30): "zero leaks" is bytes that are backed and
      resident-zeroed — freed heap is `zeroize`'s and the counting allocator's subject — and "undo
      past every point" is the session's **retained** stack, `UNDO_DEPTH` 1024 marks and
      `ROOT_DEPTH` 64 roots.

---

## Block 5 — The Eye
*Purpose: layout and paint geometry — the headless half of the UI, fully testable.*

This block is the v3 monument. v3's terminal failure was two geometries: text *painted*
with real glyph advances while caret/click selection *reasoned* in monospace cells. Block 5
makes that structurally impossible: **one layout engine, two consumers** (paint and hit-test).

**Deliverables**
- `holonomy-eye` (headless): viewport-driven layout — only segments under the viewport plus
  margin are shaped; everything else is a height read from the Spine's aggregate index.
- Caret, selection, click-hit-test, and paint walk the **same** shaped runs through the
  **same** API. Proportional fonts are the *only* arithmetic — there is no grid mode.
- Style runs (bold/italic/heading size changes) as first-class layout data, so Block 7's
  toolbar never needs a second geometry.
- Golden-corpus tests at multiple zooms and DPIs: for a fixed document and viewport,
  paint decisions and hit-test decisions must be bit-derived from one source tree.

**Status (2026-11-22 — all three gates passed, block closed)**
- `holonomy-eye` has: integer geometry (`Fixed`, 64ths), grapheme-cluster segmentation and UAX #9
  bidirectional resolution whose property and paired-bracket tables are generated from UCD 18.0.0
  (`D-27`, `D-77`), one run tree both consumers walk (including disjoint visual selection spans
  when a logical selection crosses LTR/RTL runs), zoom/DPI as a single exact view transform with no
  inverse to round against, style runs as layout data, and a real OpenType metrics reader (`Face`
  over `Inter-Regular.ttf`, M3).
- A Fenwick height model over leaf heights exists, and its cost is *counted* rather than asserted
  (a viewport over 2^18 leaves costs < 160 index reads; 16× the leaves costs < 2× the reads).
- The Spine wiring and wall-clock invariance are measured end to end: `byte_window`, `window_text`,
  and `layout_styled` with the real `Face` are *indistinguishable* at 180 KB offset 0, 180 MB
  (30 M words, 43,946 leaves) offset 0, and 180 MB near the end (`D-78`). Their absolute latency is
  a property of the runner and is quoted as a measured range rather than a constant — two runs
  sixteen minutes apart on 2026-11-24 gave `p50 = 696,731 ns` and `1,045,075 ns` for the identical
  workload (`D-91`), against a control `76–90×` slower in all three runs.
- Residual, stated rather than hidden: `Face` reads `cmap`/`hmtx` advances without `GSUB`/`GPOS`
  complex-script glyph joining or ligatures (`OQ-24`), and `L-8` stays `spec` in `docs/GATES.md`
  until Block 6's GPU renderer draws from `Picture` alongside the terminal cell surface. The
  workspace moves to `6.0.0-block.5`, one bump per closed block.

**GATES** — ✅ **all three passed, and the block is closed (2026-11-22, CI run [45106234334](https://github.com/c4us4l1ty/holonomy/actions/runs/45106234334) on `3b54969`)**
- [x] Property test: for arbitrary documents, `hit_test(paint_to_point(g)) == g` for every
      shaped glyph — the v3 regression made impossible, proven. **Measured 2026-11-19 by
      `crates/holonomy-eye/tests/one_geometry.rs`**: generated documents, every painted caret,
      clicks on and between boundaries, 8 zooms × 5 DPIs, with printed counts
      (`HOLONOMY_ONE_GEOMETRY`) and a monospace control. **Claimed 2026-11-19 on the measuring
      run**: CI run `44672133052` (`2caa52a`), test job green, which enforced every caret and click count, at
      all 8 zooms and 5 DPIs, over 12 generated documents at 3 wrap widths (AUDIT D-38).
- [x] Layout cost on a 30 M-word doc is invariant in document length (viewport-shape cost only).
      **Measured 2026-11-22 by `crates/holonomy-eye/tests/viewport_latency.rs` (AUDIT D-78, OQ-12)**
      using the real OpenType `Face` (`Inter-Regular.ttf`), interleaved across 256 samples each at
      three offsets in one process: 180 KB offset 0, 180 MB / 30 M words / 43,946 leaves at offset
      0, and 180 MB near the end — with a 128-leaf control (`< 0.3%` of the document) costing
      `53,041,922 ns` (`76×` the viewport cost) in the claiming run `45106234334` (`3b54969`).
      **What is claimed is the invariance and the ratio, not the absolute number.** The gate is
      re-run on every push and lands anywhere in a measured range — `p50 = 696,731 ns` and
      `p50 = 1,045,075 ns` sixteen minutes apart on 2026-11-24 for identical inputs (AUDIT `D-91`),
      with the three offsets and the control ratio (`76–90×`) reproducing every time. A single run's
      `p50` would be a runner benchmark wearing a gate's clothes.
- [x] Bi-directional and multi-script text handle cluster boundaries correctly (no caret
      grapheme-splitting). **Both halves met and measured 2026-11-22 (AUDIT D-77, OQ-11 closed):**
      `crates/holonomy-eye/src/bidi.rs` + generated UCD 18.0.0 `bidi/tables.rs` implement full O(N)
      UAX #9 resolution (`P2`–`P3`, `X1`–`X10`, `W1`–`W7`, `N0`–`N2`, `I1`–`I2`, `L1`–`L2`) and
      bijective primary-caret boundary pairing, verified offline against all `91,707 / 91,707`
      `BidiCharacterTest-18.0.0` vectors and enforced on every push by
      `crates/holonomy-eye/tests/script_gate.rs` (`carets_inside_a_grapheme = 0`,
      `HOLONOMY_SCRIPT_GATE_L8 boundaries=27680 disagreements=0`,
      `HOLONOMY_SCRIPT_GATE_ORDER scripts=6 clusters=40 displaced=0 note=uax9-visual-order`) plus
      multi-leaf BiDi reflow in `holonomy-shell`. **Claimed on CI run `45106234334` (`3b54969`).**
      *Residual: OpenType `GSUB`/`GPOS` contextual glyph joining remains OQ-24.*

---

## Block 6 — First Light
*Purpose: the MVP. A shippable, beautiful, password-opened, GPUI windowed editor.*

Everything before this block is plumbing. This is the first thing a user can hold, and it
already obeys every constitutional law: viewport plaintext, one file, password-only,
world-line history internal (no history UI yet), smooth at any size.

**Status (2026-11-21) — no pixel, but it opens and it keeps what you write.** Its *headless half*
exists as `holonomy-shell` (D-19): intents produce frames, the frames measure the heights the Eye
indexes, and the caret-boundary check OQ-15 asked for passes over every caret a frame offers. The
window over it (`holonomy-gui`) is **real and runnable today**: a terminal in raw mode, keys decoded
from the bytes a tty actually sends, a `.wavefunction` created or opened under a passphrase,
autosave on pause, a seal on quit, the security panel on `Ctrl+G`, and a run that died mid-append
recovered on the next open. M5 and M6 are done in `docs/TO-APP.md`.

What is not here: **GPUI**, quiet-md's page, a font, a palette, IME — so the deliverable this block
describes is still not met, and **none of this block's gates is claimed**, because they are about a
shipped app on reference hardware and cannot be faked by a character-grid window. The terminal
window is M4b-i in `docs/TO-APP.md`: the thing this environment can build, not a substitute for the
block.

**Deliverables**
- GPUI shell built on **quiet-md** ([s1dashu/quiet-md](https://github.com/s1dashu/quiet-md), MIT):
  quiet-md-grade page (generous margins, paper feel, zero chrome competing with words), with the
  Google-Docs-shaped toolbar of Block 7 above it and `docs/UI-SPEC.md` §3 governing it. Unlock
  screen: drop a `.wavefunction`, passphrase, open. **The look is quiet-md's, to the pixel** — its
  page width, type scale, colours and rhythm are quoted as numbers in `docs/UI-SPEC.md` §8, which is
  the acceptance table for this block. Its code is not reused (OQ-13): a dependency's
  `[patch.crates-io]` does not travel and reproducing its GPUI patches is the shape Block 1 forbids,
  so `THIRD_PARTY.md` is the ledger if a file is ever adapted.
- Full editing surface on the Eye's geometry: selection, IME-safe input path, copy (with
  clipboard policy: contents flagged confidential, cleared on blur), paste sized to slabs.
- Autosave by log-append with the Block 3 atomic commit; explicit `Cmd/Ctrl-S`.
- First-run create flow, machine-change flow (open an imported file, continue), and an
  honest **Security panel** that reports what is resident/locked/wiped — status as evidence,
  as v5's `ProtectionLayer` pioneered.
- High-contrast and reduced-motion support; theming hooks (used in Block 7).

**GATES**
- [ ] The full VISION budget table is measured green in CI on reference hardware at 30 M words.
- [ ] Kill-power-at-random-instant test suite: always recovers, never loses committed work.
- [ ] Hand-off drill: file copied mid-editing-session to a fresh user account and machine
      profile opens, verifies, and continues with only a passphrase.
- [ ] Zero CPU repaint measured while idle; no timers polling on the text path.

---

## Block 7 — The Ribbon
*Purpose: the Google-Docs toolbar inside the calm — the familiar controls, re-founded.*

v3 died bolting a Wordcraft-shaped ribbon onto split geometry. v6 puts the toolbar on Block
5's single geometry and the Block 1 UI spec — same visual language, none of the split-brain. It
sits above the quiet-md page Block 6 installs, in the same window, in the same run tree.

**Draft status (2026-11-25, AUDIT D-94):** `StyleMap` spine formatting, ribbon controls
(`holonomy-gui/src/canvas.rs`), streaming search, single-commit sparse replace-all, and dense
streaming replace-all (> 16,384 matches via `WorldLine::StreamingTx` when supported by the sink)
are implemented and tested. Replacement restores a checkpoint of live edit history if a later commit
fails; the preliminary flush may still have been durable, and an uncertain I/O failure requires
reopening. Both sparse and dense 30 M-word encrypted replace-all measured under 4 MiB of
incremental live heap in CI run `45566921069` on `a047181` (`find_peak=1176232`,
`replace_peak=1783625`, `dense_replace_peak=1832689` across `30,013,649` dense matches,
`bound=4194304`), while `holonomy-gui/tests/persist.rs` verifies full dense-replacement + undo
persistence across reopen. Full Block 7 visual GPUI/desktop acceptance across all OSes and total-RSS
bounds under arbitrary allocator stress remain open.

**Deliverables**
- Toolbar per spec: font family/size, bold/italic/underline/strike, headings 1–3, alignment,
  lists (real list structure — not v3's literal-prefix `- ` fake), link, horizontal rule,
  quote, code span.
- Styles as **spine data**: formatting path-copies exactly like text; style-undo is the same
  pointer swap as text-undo (v3's non-transactional `replace_all` and separate style undo
  stacks are the deleted mistakes).
- Find-in-document over the sealed log: streamed, segment-at-a-time — results list without
  ever materializing the corpus (budget VI enforced on a hard feature on purpose).
- Replace-all is a single logged transaction — one undo step, atomic.

**GATES**
- [ ] Every toolbar action round-trips: apply → save → reopen → byte-identical style state.
- [ ] Find/replace-all over 30 M words: streaming memory bound unchanged (VI gate applies).
- [ ] Formatting torture corpus (mixed styles at every slab boundary): one undo reverts all.

---

## Block 8 — The Long Memory
*Purpose: user-facing history — sixty years, navigable.*

The world-line already holds everything; this block gives it a face.

**Draft status (2026-11-25, AUDIT D-94):** Core persistence and history APIs are implemented and
tested (`RECORD_BOOKMARK`, `Bookmark`, `WitnessSummary`, `WorldLine::{witness_history, bookmarks,
add_bookmark, open_manifest_at, diff_witnesses}`, and `Editor::restore_spine`), with end-to-end
persistence verified in `holonomy-line` and `holonomy-gui/tests/persist.rs`. Interactive 60-year
timeline scrubbing in the desktop UI and a simulated-decades residency benchmark remain open for the
gates below.

**Deliverables**
- Timeline UI: scrub a human timeline (day/month/year), watch the document state at that
  point — computed from spine pointers, not restored copies.
- Named bookmarks ("the day I moved to Kathmandu") — anchors into the log.
- Diff-between-points: what changed between any two bookmarks, rendered as style deltas over
  the *current* view — never a full second plaintext.
- Bounded-search across history using the same streaming discipline as Block 7.

**GATES**
- [ ] Scrubbing 60 years of simulated history stays interactive (viewport-bound work).
- [ ] Memory during history scrub obeys the same resident-set bound as live editing.

---

## Block 9 — The Walls
*Purpose: wire the runtime protections into the live editor — v5's orphans finally adopted.*

v5.1.0 *has* an audited reaper (zero-I/O emergency thread), a memory-lock ladder
(`memfd_secret` → `mlock`+`MADV_DONTDUMP` → `VirtualLock`), and capture-exclusion primitives —
all tested, none wired in. Block 9 ends that. This is also where viewport-bounded plaintext
pays its biggest dividend: **the reaper wipe set is segments-in-view, not a 180 MiB corpus** —
v5's 45 ms wipe-budget overshoot disappears by construction.

**Deliverables**
- Lock ladder applied to every plaintext slab + key buffer (platform graceful degradation,
  honestly reported in the Security panel).
- Reaper wired to real tripwires: USB-optional means the legacy yank-detector becomes
  configurable paranoia (dead-man timer, decoy-deadlock keys, OS suspend hooks).
- Screen-capture exclusion where the OS allows; blur/lock on focus loss elsewhere.
- **Zeroize canary:** inert patterned buffers riding every plaintext allocator path; the
  reaper samples them post-wipe — a leaked ghost proves the invariant broke *on this machine*
  and says so. The anti-decorative-security instrument.
- Forced recheck: adversarial re-audit of Blocks 2–9 implementations (not just RFCs).

**GATES** — Historical headless measurements below passed in CI run [45106234334](https://github.com/c4us4l1ty/holonomy/actions/runs/45106234334), but live runtime gates remain open following D-81–D-83; L-7 remains `spec`. The cooperative reaper does not establish the 50 ms deadline or complete wipe coverage.
- [x] Reaper fire test: time from tripwire to wipe-verified measured; canary ghosts: zero.
      **Measured 2026-11-22 by `crates/holonomy-text/tests/wipe_gate.rs` + `holonomy_seal::reaper`**
      across 24 end-to-end trials covering `Manual`, `Suspend`, `UmbilicalDisconnect`, and
      `DeadmanTimeout` over `1,041` registered arenas (`4,324,384` bytes: a full 4 MiB `ResidentSet`
      with editing slack, a live `DEMKey` capability token, and 32 × 4 KiB `0xA5` zeroize canaries):
      `HOLONOMY_WIPE_GATE trials=24 arenas=1041 bytes_wiped=4324384 ghosts=0 sched=NormalFallback
      reaper_p50_us=878 reaper_p99_us=922 reaper_max_us=922 wall_p50_us=913 wall_p99_us=949
      wall_max_us=949`, with an anti-decorative negative control (`HOLONOMY_WIPE_CONTROL sampled=32
      ghost_canaries=1 ghost_bytes=4096 quiet_allocs=0`) proving both ghost detection and
      zero-allocation scrubbing. *Scope residual: OS suspend/USB event listeners and native-window
      capture exclusion remain owned by Block 6's GPUI shell and Block 10's three-OS qualification,
      so L-7 stays `spec` in `docs/GATES.md` while its `BUDGETS` row cites `tests::wipe_gate`.*
      **Live-editor receipt added 2026-11-24 (D-89, not a gate):** the terminal used to discard the
      reaper's outcome and print a fixed sentence that neither affirmed nor denied a wipe. It now
      keeps `ReaperOutcome` and splits it three ways — scrubbed bytes with no surviving canary is
      reported with its arena/byte/µs counts and exits 0; a join with no outcome, a wipe over an
      empty registry, or any surviving canary prints *why* and exits 1, on the same policy as a
      refused save (D-49). This changes what the window *admits*, not what it *covers*: the
      cooperative watcher still cannot meet the 50 ms deadline from a blocked UI thread, and
      registration still covers only the allocations the editor arms, so **L-7 stays `spec`**.
- [x] Security panel claims cross-checked by test (no status a test cannot prove).
      **Enforced by `crates/holonomy-gui/tests/panel.rs` (M6):** every line rendered by
      `holonomy_gui::panel::Facts` is verified by mutating live `ResidentSet` byte/leaf counts,
      `Document` segment/witness/verification counts, and every `LockCapability` rung from
      `probe_capability()`, with `None` rendered as `"not verified"`. No clipboard status is claimed
      because the terminal front end has no OS clipboard integration (`OQ-3` stays open for Block 6).
- [x] Implementation audit closed; all findings fixed or written down as user-visible limits.
      **Closed 2026-11-22 (AUDIT D-78, D-79):** re-audit of Blocks 2–9 fixed
      `SlabText::from_plaintext`'s `Vec::reserve` `realloc` / invalid-UTF-8 unwiped-plaintext escape
      (`crates/holonomy-text/src/text.rs`) and `holonomy-shell`'s three-part leaf-seam defect
      (`tail_bytes` over-extension on clean cluster boundaries, `measure_window` overwriting clipped
      tail-leaf heights, and `cluster_after` absorbing a neighbour cluster across a clean leaf seam
      on `Delete` / `Move::Right` / `legal_caret`), and recorded remaining user-visible limits
      (`OQ-3` clipboard in GPUI, `OQ-24` OpenType `GSUB`/`GPOS`, terminal screen-capture exclusion).

---

## Block 10 — The Horizon
*Purpose: v6.0.0 — the release gate, the docs, the promise kept.*

**Draft status (2026-11-25, AUDIT D-94):** Streaming `.txt`/`.md` import, explicit-disclosure
plaintext export (`holonomy-gui::transfer`, `holonomy import`, `holonomy export`), and one-way
v5.1.0 `.wf5 → .wavefunction` migration (`holonomy_seal::v5`, `holonomy migrate-v5`) are
implemented and verified (`crates/holonomy-gui/tests/transfer_migrate.rs`). Reference-hardware
three-OS gate certification, migration prose/whitepaper deliverables, and the multi-year
self-hosting/fresh-user drill remain open for the gates below.

**Deliverables**
- Full VISION budget certification on reference hardware (published numbers, repro script).
- Import: `.md` / `.txt` inbound onto the spine (outbound export is plaintext-egress
  controlled by explicit user action, report included).
- Migration guide (from v5.1.0 containers: one-way importer via v5's `recover-copy` lineage).
- Security whitepaper: threat model + what v6 does not claim to defend.
- The **self-hosting test**: the v6 author writes the next block's spec *in v6*.

**GATES**
- [ ] Every gate in this document green simultaneously on one build, on all three OSes.
- [ ] A fresh user: install → create → write 24 simulated years → backup → wipe machine →
      restore → continue. Scripted, recorded, passing.

---

## Beyond the horizon (deliberately deferred)

Not killed — *deferred*, each waiting on v6.0's honest measuring:

- **Optional USB deadbolt**: pad-fold the newest segments as extra OTP hardening (v5's Zone
  B machinery, demoted from load-bearing to accessory).
- **Duress strata**: outer plausible world, inner real world (v5's `L_1` decoy, redesigned
  against the log).
- **Composition surfaces**: tables, images, math (H1's strength; needs the Eye's geometry
  extended first).
- **Multi-document vaults** (H1's `HVAULT1` lineage), **mobile builds**, **remote headless
  usage** (SSH-scrubbed remote-ext).

---

## The one rule of construction

Every block must be able to point at the constitutional law it serves. If it cannot, it is
dust — and dust is the one thing a singularity does not carry.
