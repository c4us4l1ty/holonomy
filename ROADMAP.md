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

**GATES** — ✅ **passed 2026-11-16** (CI run [43891997903](../../actions/runs/43891997903), all four jobs green)
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
- `SecureBlock`-style memory handling (guard pages, `mlock`/`VirtualLock`, zeroize-on-drop)
  ported and modernized from H1 `holonomy-secure` / v5 `holonomy-crypto::lock`.
- Explicit **capability tokens**: code that does not hold the token type cannot receive
  plaintext key bytes. The compiler enforces the boundary the audits used to patrol by hand.

**GATES**
- [ ] Adversarial audit of the crypto RFC closed: zero unresolved "impossible claim" items.
- [ ] Envelope round-trips with deliberately corrupted inputs fail with coarse errors only.
- [ ] No key material ever crosses an allocation that isn't zeroized under a counting test.

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
  as in H1's `aead.rs`), witness commits, atomic stage-and-replace writes with directory
  fsync (v5's tested 9-step save, simplified: no pad fold in the core path).
- Crash model implemented and tortured: torn writes, out-of-space, power loss mid-append —
  the log must always open at its last committed witness.
- Retention: old segments are the free-version history. Compaction exists **only** as an
  explicit, opt-in, passphrase-confirmed operation that rewrites history — never automatic.
- Cloud-proofing: append-only layout so any incremental sync tool uploads only new tail
  bytes; `holonomy verify` re-checks segment integrity after a copy, so a cloud-mangled file
  is detected, not silently trusted.

**GATES**
- [ ] Property tests: random crash injection at every append point; always opens at last
      good witness; zero silent corruption.
- [ ] A container grown to simulated 30 M words writes new saves touching only tail segments.
- [ ] `verify` catches flipped bytes anywhere, including in "old" history.

---

## Block 4 — The Spine
*Purpose: the document engine — where "never decrypt more than you look at" becomes true.*

Salvages H1's two crown jewels, redesigned onto the segment log: the **faulting leaf**
residency model (leaves fault in from the 64 KiB section store; bounded resident set) and
the **measured allocation-free keystroke path** — plus the Fenwick-over-heights index H1
kept in `holonomy-geometry`. The H1 mistakes being deleted: layout-as-lines (page breaks
were faked with newline padding) and any structure that walks the document on a keystroke.

**Deliverables**
- `holonomy-text`: an **immutable, path-copied slab tree** (rope-shaped Merkle spine over
  segment IDs). An edit path-copies O(log N) slabs; the old spine survives as history.
  Undo between any two points in the world-line is a pointer swap.
- **Resident-set scheduler:** plaintext slabs fault in under the viewport, fault out on a
  strict budget, wipe on exit. Document length cannot enter the memory equation.
- Fenwick index over *slab* heights for O(log segments) scroll-position math.
- Counting-allocator gate on the keystroke path: a typed character must make **zero**
  allocations outside slab boundaries — measured, per H1's gate pattern.

**GATES**
- [ ] Open a synthetic 30 M-word document: memory at open is within a fixed bound that
      identical input of 1 page also satisfies.
- [ ] Keystroke at offset 0 and at offset `N-1`: statistically identical latency curves.
- [ ] 1 M random-keystroke fuzz run: zero leaks, zero residency blowups, undo past every
      point in the log.

---

## Block 5 — The Eye
*Purpose: layout and paint geometry — the headless half of the UI, fully testable.*

This block is the v3 monument. v3's terminal failure was two geometries: text *painted*
with real glyph advances while caret/click selection *reasoned* in monospace cells. Block 5
makes that structurally impossible: **one layout engine, two consumers** (paint and hit-test).

**Deliverables**
- `holonomy-eye` (headless): viewport-driven layout — only segments under the viewport plus
  margin are shaped; everything else is a height read from the Spine's Fenwick index.
- Caret, selection, click-hit-test, and paint walk the **same** shaped runs through the
  **same** API. Proportional fonts are the *only* arithmetic — there is no grid mode.
- Style runs (bold/italic/heading size changes) as first-class layout data, so Block 7's
  toolbar never needs a second geometry.
- Golden-corpus tests at multiple zooms and DPIs: for a fixed document and viewport,
  paint decisions and hit-test decisions must be bit-derived from one source tree.

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

**Deliverables**
- GPUI shell: quiet-md-grade page (generous margins, paper feel, zero chrome competing with
  words). Unlock screen: drop a `.wavefunction`, passphrase, open.
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
5's single geometry and the Block 1 UI spec — same visual language, none of the split-brain.

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
