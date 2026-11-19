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

**GATES** — none passed; the third gate has a runner, three bug fixes, and two open findings
*(the keystroke *deliverable* is measured; these gates are document-scale, latency and fuzz
measurements on reference hardware and share nothing with it)*
- [ ] Open a synthetic 30 M-word document: memory at open is within a fixed bound that
      identical input of 1 page also satisfies.
- [ ] Keystroke at offset 0 and at offset `N-1`: statistically identical latency curves.
- [ ] 1 M random-keystroke fuzz run: zero leaks, zero residency blowups, undo past every
      point in the log. **The runner is in the tree; the gate is not claimed.** A seeded generator
      over a real `Typing` session, a real `ResidentSet` and one shared segment store, with an
      independent `String` model and the document read back through `copy` — the Eye's and the
      shell's reader. Six seeds of 20 000 actions on every push, **every** action checked; the
      gate's million keystrokes in release, four seeds of 250 000, printing `HOLONOMY_KEYSTROKE_FUZZ`
      per seed. The two clauses about leaks and residency hold at that scale. The third does not, yet.
      Earning the gate has so far taken three fixes and left two findings: a seal that wrote the bytes
      an undo had taken back (D-31), a burst that filled the buffer inside a character and lost the
      keystroke (D-32), and a redo after an undo of a root that came back one byte short (OQ-21, fixed
      by D-33 — a root step's inverse keeps the bytes its undo drops, so its redo re-attaches them).
      What remains is the same family one layer down: the walk still reports a redo that re-attaches
      one pending byte at the wrong offset (OQ-22, seed 0, redo five, 276 bytes where 275 was
      expected), and with undo in the million-keystroke mix `Typing::pending` asserts on a live
      length longer than its buffer (OQ-23). Both are why the million run still excludes undo, and
      why the checkbox stays empty.

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

**Status (2026-11-19, in progress — no gate claimed)**
- `holonomy-eye` has: integer geometry (`Fixed`, 64ths), grapheme-cluster segmentation whose
  property data is generated from UCD 18.0.0 rather than typed out (D-27), one run tree both
  consumers walk, zoom/DPI as a single exact view transform with no inverse to round against, and
  style runs as layout data. L-8's property test exists and passes over a corpus of scripts at 8
  zooms × 5 DPIs, on a fake shaper.
- A Fenwick height model over leaf heights exists, and its cost is *counted* rather than asserted
  (a viewport over 2^18 leaves costs < 160 index reads; 16× the leaves costs < 2× the reads).
- The Spine wiring exists: `byte_window` turns a leaf range into a byte range, `window_text` reads it
  out of the resident set and the pending buffer, and `holonomy-shell` runs the loop that turns a
  scroll position into a laid-out frame and writes the measured heights back (D-18, D-19). Gate 2's
  *inputs* are therefore measured end to end on a 4,000-leaf document.
- Not yet: BiDi reordering (gate 3 — the segmentation rules are the standard's through GB13 as of
  D-16 and D-28, and the property data is the database's as of D-27), a real shaper (Block 8), and
  seconds rather than counts for gate 2 — without those, L-8 stays `spec` rather than `enforced`.

**GATES**
- [ ] Property test: for arbitrary documents, `hit_test(paint_to_point(g)) == g` for every
      shaped glyph — the v3 regression made impossible, proven.
- [ ] Layout cost on a 30 M-word doc is invariant in document length (viewport-shape cost only).
- [ ] Bi-directional and multi-script text handle cluster boundaries correctly (no caret
      grapheme-splitting).

---

## Block 6 — First Light
*Purpose: the MVP. A shippable, beautiful, password-opened, GPUI windowed editor.*

Everything before this block is plumbing. This is the first thing a user can hold, and it
already obeys every constitutional law: viewport plaintext, one file, password-only,
world-line history internal (no history UI yet), smooth at any size.

**Status (2026-11-18) — none of this block's gates claimed, and no pixel drawn.** Its *headless
half* exists as `holonomy-shell` (D-19): intents (type, backspace, delete, select, click, drag,
undo, redo, scroll, resize, zoom) produce frames, the frames measure the heights the Eye indexes,
and the caret-boundary check OQ-15 asked for passes over every caret a frame offers. What is not
here: GPUI, quiet-md's page, a font, a palette, IME, autosave, the unlock screen, and every gate
in this block — which are about a shipped app on reference hardware and cannot be faked by a
headless loop.

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

**GATES**
- [ ] Reaper fire test: time from tripwire to wipe-verified measured; canary ghosts: zero.
- [ ] Security panel claims cross-checked by test (no status a test cannot prove).
- [ ] Implementation audit closed; all findings fixed or written down as user-visible limits.

---

## Block 10 — The Horizon
*Purpose: v6.0.0 — the release gate, the docs, the promise kept.*

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
