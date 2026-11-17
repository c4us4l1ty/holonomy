# HOLONOMY v6 — AUDIT (living document)

> This is a living record, not a post-mortem. It follows the v5.1.0 cycle — spec, attack,
> verdict, then (and only then) build — and it carries the wall of killed claims so dead
> ideas cannot be resurrected by enthusiasm alone.

## Protocol

1. Every block starts from an RFC in `docs/rfc/`. An RFC is a *target for attack*, not a
   celebration of intent: it must name its adversary cases (from `docs/THREAT-MODEL.md`) and
   the test or gate that would falsify it.
2. Anyone may file an attack case in this file against an open RFC. A case names: the claim
   attacked, the mechanism of failure, and the smallest demonstration.
3. Verdicts: **killed** (claim removed from the design), **amended** (claim survives in
   weakened, explicitly-stated form), **survived** (attack answered by construction or gate).
4. Implementation audits (Block 9) re-attack *code*, not just specs.
5. Nothing lands in `GATES.md` status `gated` without its attack cycle closed.

## The wall of killed claims

Claims that were proposed somewhere in Holonomy's history and destroyed by their own audit.
Listed so they stay dead.

| # | Claim | Killed by | Generation |
| - | ----- | --------- | ---------- |
| K-1 | One-time-pad reuse across saves is acceptable | counter-mode nonce reuse = worn-twice pad; found in H1's own spec during implementation | H1 |
| K-2 | 384 MiB × t=16 Argon2id at "180 ms" | measured 10,180 ms — 57× off; a DoS on the user's own machine | H1 |
| K-3 | π(0) KEM self-encapsulation unlocks the capsule that holds it | circular by construction — deadlock at slot `π(0)` | v4 |
| K-4 | "CryptScanline" — zero-plaintext rendering as v5.0 specified it | plaintext exists at paint or it is not on screen; claim impossible as written | v5.0-OMEGA |
| K-5 | SPOC ±42 visual flicker for covert signaling | cross-correlationable; an epilepsy hazard; decorative | v5.0-OMEGA |
| K-6 | PEED self-modifying anti-debug keystream | breaks its own execution invariants | v5.0-OMEGA |
| K-7 | 16.60 MiB RSS budget for a full editor | impossible arithmetic, claimed anyway | v5.0-OMEGA |
| K-8 | Monospace-cell caret geometry under proportional paint | 41-bug audit; caret/selection/click point at different bytes — the two-geometry disease | H1 UI |
| K-9 | Page breaks by newline padding | corrupts document semantics to please layout | H1 UI |
| K-10 | Fixed-size 134 MiB payload as a lifetime document format | L-1 (60 years of growth) kills it; v6 container is append-only | v5.1 format choice |

## Open attack ledger (seeded)

| Case | Target | Status |
| ---- | ------ | ------ |
| OQ-1 | Toolchain: root `rust-toolchain.toml` used floating `stable`. Pinned to Rust 1.100.0 after observing the CI compiler; no local sandbox toolchain was available for an independent pin check. | **closed 2026-11-16** |
| OQ-2 | Undo exposure: world-line keeps all drafts (Vision III) vs. an adversary who wants the right to truly delete a draft. Compaction is explicit, file-scoped, and passphrase-confirmed; RFC-002 states that cloud mirrors may retain pre-compaction files and their history, so users must also prune external backups. | **closed — residual disclosed** |
| OQ-3 | Clipboard egress on each OS: flag-confidential + clear-on-blur must be verified per platform (Block 9) or labeled unverified in the Security panel | open |
| OQ-4 | Scroll-timing side channel: slab fault-in pattern leaks *where* you read, to whoever sees slab access timing. | **closed 2026-11-17 — accept and disclose.** The pattern is already visible at the I/O layer: fetching an old page reads its ciphertext at a known file offset, so any observer who can time slab faults can observe the reads themselves. Smoothing would buy nothing and would spend L-5 (idle is idle) on traversal cost the architecture exists to delete. Residual recorded in `docs/THREAT-MODEL.md` §5. |
| OQ-5 | Argon2's internal working arena: `MasterKeys::derive`, `kdf::derive_root_secret*`, and the calibration probe all allocated the m-cost arena inside `hash_password_into` and freed it unwiped (v5's H1 hit exactly this and switched to caller-owned memory + a scrubbed block). Fixed by making the arena caller-owned everywhere: `kdf::Arena(Vec<argon2::Block>)` zeroizes the slab on Drop via argon2's `zeroize` feature, and the single `run_argon2` runner serves creation, replay, *and* the calibration probe, with a test pinning byte-equality against argon2's own allocator | **closed** 2026-11-16 |
| OQ-6 | `ParamsClass::Test` misuse guard: a release build could derive a user container at the weakened schedule, and replayed records had no fence. Closed at both boundaries: `derive_root_secret` refuses `Test`; `derive_root_secret_with` refuses below-floor schedules and accepts only records emitted by the calibrated production selector; Block 3's header rejects weak memory, out-of-range memory, non-step-aligned memory, or unbounded time/lanes **before Argon2 runs**. Test/debug profiles admit only the exact test tuple. | **closed** 2026-11-16 |
| OQ-8 | Undo depth as memory: `History` keeps one `Spine` root per state. Roots share subtrees, so the cost per edit is O(log N) nodes, but an unbounded history over years of typing still grows with the **number of edits**, not with the document — and the world-line on disk already holds that history. Either bound the in-memory history (a ring of roots plus reconstruction from the log) or state the growth openly. Decide before Block 6, where a session is long enough for it to matter. | **open — found 2026-11-17 by the bug hunt, no fix yet** |
| OQ-7 | Valid-prefix rollback: a byte-perfect earlier witness prefix is indistinguishable from a file that legitimately stopped there without trusted state outside the file. RFC-002 amended to claim integrity of present bytes and crash consistency, not freshness against full-prefix rollback. An external transparency/monotonic anchor would resolve this but cannot be mandatory under passphrase portability (L-9). | **open — accepted limitation, not locally solvable** |
| OQ-10 | A full resident slab has **no continuation policy** — solved by D-7: a leaf names a *source*, and a source is **open** (bytes in a resident buffer) or **sealed** (bytes in the world-line), at the same address. Text being typed is a leaf from its first byte, so `byte_window` and the height index see it; sealing is a state change of the same source and **leaves the root untouched**, so history and `verify` never observe it; each byte is written once, which removes rather than manages option A's quadratic cost; and the commit no longer needs to know where the caret is. Costs: ids become identities (allocated at seal, recorded in the segment header), residency gains a source lookup, and at most a handful of sources may be open at once. | **decided 2026-11-17 (D-7); register + insert path pending** |
| OQ-9 | Leaf fragmentation: a split that lands inside a leaf leaves the segment spread over two leaves, and `join` never coalesces adjacent fragments of the same segment. Content stays correct and the fragments share one segment's storage, but churn inflates the leaf count and with it the height and every slab-count-derived budget. **Narrowed 2026-11-17 by D-3:** the commit boundary does not create fragments — it replaces exactly the extent one leaf covers, and the commit tests assert the leaf count is unchanged — so the remaining exposure is structural inserts (`insert_slab` cutting a leaf) that are never rejoined, plus a document loaded from a log whose geometry is already fragmented. Coalescing at commit, or a pass in `from_slabs`, is still unimplemented; the cheaper of the two is coalescing same-segment neighbours when a document is built from the log. | **open — narrowed, no fix yet** |
| OQ-11 | Cluster segmentation is a hand-written approximation of UAX #29, chosen because `holonomy-eye` must stay dependency-free (no ICU, no `uniseg`). It is conservative — it never splits a mark from its base — but it leaves some sequences UAX #29 would join as separate clusters (Hangul jamo above all), so a caret can sit where a real renderer would not allow it. Block 5's third gate (BiDi and multi-script cluster-correct carets) therefore **cannot** be claimed yet: BiDi reordering is not implemented at all, and grapheme honesty is only as good as this table. Replacing it with a real UAX-29 implementation is Block 9 work; the API surface that has to change is `cluster.rs` alone. | **open — deliberate gap, found 2026-11-17 while writing the segmentation table** |
| OQ-12 | **The window is viewport-sized, but no editor calls it yet.** `layout_styled` shapes exactly the text it is handed, so Block 5's cost gate is a claim about the caller. The chain exists — `HeightModel` finds a leaf range in O(log n) counted reads, `Spine::slab_span` turns a leaf position into a byte range in one descent, `holonomy_eye::byte_window` joins them contiguously — and D-7 removed the last structural objection by making text being typed an ordinary leaf. What is missing is the editor loop that calls all three and the measurement that proves the gate on a 30 M-word document; neither exists before Block 6. | **open — chain landed, no caller or measurement yet** |
| OQ-13 | **How quiet-md enters the tree — decided: as source, not as a dependency.** Cargo ignores a dependency's `[patch.crates-io]`, and quiet-md's app code calls API that only its 43 GPUI patches add, so it is not consumable as a dependency at all; reproducing the patches at our root is the fork-in-the-core-tree Block 1 forbids, and exiling the UI to a second workspace is the other shape it forbids. quiet-md is therefore prior art and an attributed source of MIT code (`THIRD_PARTY.md`), with GPUI pinned upstream and no patch section. The cost is named in `docs/UI-SPEC.md` §8: CJK/bidi line breaking and font fidelity are ours to earn, and quiet-md's patches #4/#23/#30/#31/#32 are the reference for the first of those — the same gap OQ-11 records. | **closed 2026-11-17 — source, not dependency** |

## Audit trail

- **2026-11-17 — D-7 — OQ-10 solved: a leaf names a place, and a place can be open.** D-6 was
  correct about the arithmetic and wrong about the shape. It accepted the frame "a leaf names a
  sealed segment, so text that is not sealed has no leaf" and then paid for that frame with a third
  overlay case, a caret-dependent commit, and a hole in `byte_window` exactly where the user is
  typing. The frame was the bug.

  **The turn.** Separate two questions that were tangled: *where does this leaf's text live* and
  *which sealed segment is it in*. A leaf needs the first; only the storage layer cares about the
  second. Let a leaf name a **source**, and let a source be **open** (bytes in a resident buffer,
  being typed into) or **sealed** (bytes in the world-line). Same address either way.

  **What that buys, in order of how much it matters.**

  *The typed text is a leaf.* It gets a leaf index, a `slab_span`, a height and a byte range from
  its first byte. `byte_window` sees it with no new case, so OQ-12's caller is no longer blocked by
  a hole in the middle of the viewport. The "overlay cannot express an insertion" problem was never
  about insertions: it was about leaves being restricted to sealed territory.

  *Sealing is a state change, not an edit.* When the buffer fills, the source is sealed — same id,
  bytes appended once — and a fresh open source takes over. **The root does not change.** So the
  world-line, `History`, undo and `verify` never observe a seal at all; the document never changed,
  only where its bytes live did. That also answers the question D-6 left dangling: undo across a
  seal is not "an ordinary commit's undo", it is *no undo event at all*.

  *Each byte is written once.* Typed into an open source, sealed once. One megabyte of typing seals
  one megabyte, against option A's 8.5 MiB for the same megabyte and 8 GiB by thirty-two — and the
  difference is structural rather than tuned: nothing is ever re-sealed, because nothing that is
  sealed is ever typed into again.

  *The caret stops mattering.* D-6 needed the commit to know where the caret was, because it was
  sealing an edited leaf's prefix. With open sources the editor only ever seals a source it owns,
  whole — no split, no prefix, no caret. Where the caret sits is a *view* concern again, which is
  where it belonged.

  **What it costs, stated before it is built.** Ids become identities rather than positions, so a
  segment id is allocated when its bytes are sealed and the world-line records it in the segment
  header; a world-line whose ids are not positions must say so, and `verify` reads the id from the
  segment rather than inferring it. Residency gains a lookup: a source resolves to a buffer or to a
  segment. And at most a handful of sources may be open at once, because each one is a resident
  buffer and residency is a bounded resource (L-2) — the editor's insert path must seal before it
  opens another, which is one `if` and one test, not a policy.

  **What it deletes.** D-6's third overlay case, D-6's empty-leaf workaround, D-6's caret-dependent
  commit, and the sentence in OQ-12 that said the viewport could not see what the user is typing.
  The overlay keeps exactly one job — the live length of a source being edited — which is the job it
  had all along (D-2).

- **2026-11-17 — D-6 — OQ-10, first answer: seal at the caret, and the pending leaf is an overlay, not a leaf.** *(Superseded the same day by D-7: the awkward part was not the overlay's shape but the assumption that a leaf names a *sealed* segment. Kept because the arithmetic it records is still the reason option A is dead.)*
  `SlabText::insert` refuses with `WouldSplit` when a resident slab is full, and nothing said what
  happens next. There are exactly two continuations, and one of them does not scale.

  **Why not re-seal the leaf.** Committing the full leaf and re-faulting it works and is one line of
  wiring, but each exhaustion seals the leaf's whole text again: at 64 KiB, typing past a full leaf
  seals 64 KiB, then 128, then 192 — the k-th seal writes k·64 KiB, so n typed bytes seal about
  n²/128 KiB. The first megabyte costs 8.5 MiB of redundant sealed plaintext; by 32 MiB it costs
  8 GiB, and every byte is a ciphertext a future `verify` must walk. The ratio is not a constant,
  which is the tell: 8× at one megabyte, 256× at thirty-two.

  **What we do instead.** Seal only the prefix, at the caret: `commit_slab` the resident prefix and
  `Spine::replace_range(leaf_start, caret, new_slab)`. This is expressible with the API the commit
  boundary already has — `replace_range` collapses a range to one slab — and the tail leaf keeps
  naming its old segment, unchanged, because the bytes after the caret were not edited. Total
  sealed: exactly the new text. The world-line grows with what was typed, not with what exists.

  **Why the pending leaf is not a spine leaf.** The cleanest identity for the new text would be a
  leaf of its own, so that `slab_span` gives it an index and `byte_window` covers it. `tree.rs`
  refuses it: `assert!(!slab.is_empty(), "a leaf must carry bytes")`. That refusal is right — a leaf
  that names nothing is a handle to nothing, the same reason `Draft` returns drafted leaves whole
  instead of clipping them. And `Draft` cannot host it either: it maps a leaf that **exists** to the
  length its resident text now has, and an insertion is text that no leaf names.

  So the pending text is a third case in the live-document overlay, beside `LiveSpan::Sealed` and
  `LiveSpan::Draft`: an insertion with an offset and a length and **no slab** — which is why
  `segment_offset` is already `Option` and why the case costs no new lie. `byte_window` then reports
  the pending extent to the Eye along with the spine range, so newly typed bytes are shaped like any
  other.

  **The ordering this exposes.** OQ-12 was recorded as waiting on OQ-10, and the reason is now
  concrete rather than procedural: `byte_window` maps leaves to bytes, and the bytes the user is
  actively typing are not in a leaf. A viewport chain that cannot see the caret's own text is not a
  caller. The overlay's insertion case is therefore the next slice, and the gate claim waits for it.

  **One thing this does not change.** Undo after a seal-at-the-caret commit is an ordinary commit's
  undo: the root after `replace_range` is a normal root naming two leaves instead of one, and
  `History` returns to the root before it. No new mechanism, no new invariant — which is the
  standard this record has to meet before it is believed.

- **2026-11-17 — D-5 — OQ-13 closed: quiet-md is source, not a dependency.** The question was
  presented as three shapes (published dependency, vendored subtree, fork of a fork) and one of them
  turned out not to exist.

  **The dependency is impossible, not merely unattractive.** Cargo applies `[patch.crates-io]` only
  from the workspace root, so consuming `quiet_md` builds it against stock GPUI 0.2.2. quiet-md's own
  code calls what its patches *add*: `grid_col_widths(Vec<f32>)`, `Window::paint_pixels`,
  `Window::replace_root`, `Window::set_window_min_size`, `RenderImage::with_scale_factor`,
  `Path::transformed`, `WindowTextSystem::glyph_outline`, `PathPromptOptions.extensions`,
  `ListState::estimated_scroll_metrics`. Three tables, three vendor trees and 43 patches are
  load-bearing, so the crate as published (if it were published) would not compile for us.

  **The other two shapes are what Block 1 exists to forbid.** Copying `vendor/gpui` in means a
  `[patch.crates-io]` onto a fork inside the core workspace — the exact mechanism that held v5's UI
  crates hostage. Moving the UI to its own workspace to keep the patch out of the core tree is the
  other half of the same law ("one workspace, one lockfile, every crate a member"). The law decided
  this, which is the point: the decision was available before quiet-md was ever opened, and the
  research only confirmed it.

  **What we therefore do not inherit, said out loud.** quiet-md's patches #4, #23, #30, #31, #32 are
  CJK 避头尾 line breaking, script-aware word classes for Hangul/Hebrew/Arabic/Syriac and RTL
  wrapping with per-line reordering — which is *the Eye's territory*, not GPUI's, because the Eye
  lays out the page. Those five patches are now the best available reference for what OQ-11's
  segmentation must eventually do, and #30 in particular is a worked example of the bidi line-order
  problem. Patches #1, #12 and #14 are rendering fidelity; without them the page is stock GPUI, and
  `docs/UI-SPEC.md` §8 forbids ever measuring a gate against quiet-md's pixels instead.

  **The ledger, because MIT requires one.** `THIRD_PARTY.md` records adapted source: nothing yet,
  and the four rules that govern the first file that ever is (provenance line in the file, licence
  text travelling with it, same pull request, and nothing that needs a patch section to build).

- **2026-11-17 — Block 5, fifth slice: the viewport chain closes.** Three pieces that had been built
  for each other but never introduced: the height index answers *which leaves*, the spine answers
  *where those leaves live*, and nothing joined them.

  **`Spine::slab_span(index)`** is an order statistic: descend the cached slab counts, O(log N),
  no scan. Without it the Eye would have to read `spans_in` and count leaves to answer "which leaf is
  the twelfth", and the cost of finding the viewport would grow with the document — the thing Block
  5's second gate forbids.

  **`byte_window`** is the join, and its interesting decision is that the byte range is *contiguous*
  from the first covered leaf's start to the last covered leaf's end. Building it leaf by leaf would
  be the obvious implementation and would leave holes: an unmeasured leaf has zero height, so a
  viewport can skip over it, and shaping the covered leaves individually would silently drop the text
  in between. The range therefore includes it, and the docs say why. An unmeasured *prefix* or
  suffix is still excluded — that remains the provisional-scroll limitation recorded with the model.

  **The dependency, stated.** `holonomy-eye` now depends on `holonomy-text` (workspace path, no
  registry cost) for exactly this join. Geometry still enters through `Shaper` alone: the Eye reads
  byte offsets and leaf geometry, never text, and the Spine still holds no plaintext.

  **Still open.** OQ-12's caller — the editor loop — and its 30 M-word measurement arrive with Block
  6. OQ-10's pending leaf has no leaf index and no `slab_span`, so it is in no window this function
  produces; that decision is unchanged and still gates the editor.

- **2026-11-17 — The UI basis is quiet-md, recorded rather than assumed.** The user named
  [`s1dashu/quiet-md`](https://github.com/s1dashu/quiet-md) — a Rust + GPUI notes app, MIT, an app in
  `src/`, a library in `crates/core`, and a **patched GPUI under `vendor/`** — as the codebase the
  editor's shell is built on, with a Google-Docs-shaped toolbar above the page. Two parts of that
  were already the plan; one was not, and the difference is the point of this entry.

  **Already specified.** The toolbar is Block 7's entire subject ("the Google-Docs toolbar inside the
  calm", `ROADMAP.md`), with `docs/UI-SPEC.md` §3 specifying its behaviour — focus returns after a
  click, read-only actions grey out, the security dot at the right end. Block 6 already says the
  shell is a GPUI window with a quiet-md-*grade* page. What did **not** exist anywhere in this repo
  was quiet-md as a dependency: it appeared only as that adjective, an aesthetic benchmark, with no
  licence, version or commit recorded.

  **What the decision adds.** quiet-md is the basis of the UI rather than a reference to it, so Block
  6 starts from a codebase that already solves GPUI text drawing, IME and the page feel. Its licence
  is MIT, compatible with this workspace's `MIT OR Apache-2.0`.

  **The law it collides with, stated before anything depends on it.** Block 1 forbids
  `[patch.crates-io]` onto forks in the core tree, and quiet-md's tree carries a *patched* GPUI with
  a numbered patch list (`vendor/PATCHES.md`, #43 as of 0.1.13). Both cannot stand unchanged. The
  three shapes that can: depend on quiet-md's published crate and leave GPUI alone; vendor quiet-md
  under an audited patch list of our own; or upstream the patches we need and keep the core tree on
  upstream GPUI. Each has a different failure mode, none should be chosen by accident, so it is
  **OQ-13** and it must be settled before Block 6 opens rather than during it.

  **What this does not change.** The Spine stays authoritative for text, styles and history:
  quiet-md is the shell and the editing surface, not the document engine. The Eye is headless, so
  L-8 and the single-geometry law are untouched by which framework draws the result.

- **2026-11-17 — D-4 — the height index lives beside the tree, not inside it.** `tree.rs` ends with a
  standing invitation: "Weights are byte lengths today; the Eye's layout heights will ride the same
  aggregate once Block 5 defines them." Block 5 has now defined them — `Fixed` 64ths, per style, per
  leaf — and they still do not ride that aggregate. This entry is the argument, because the
  alternative is a comment left standing that the code silently contradicts.

  **Why not the tree.** A leaf's height is *measured*, lazily, at the current wrap width. A window
  resize invalidates every measured height at once, and an immutable path-copied tree can only
  express that by rewriting all of its nodes: O(n) nodes for an event as common as dragging a window
  edge. The Fenwick model is invalidated in O(1) and re-measures only what the viewport looks at.

  **Why this is not the K-8 disease.** The danger `tree.rs` guards against is duplicate *geometry* —
  two arithmetics that must agree and therefore can disagree. `HeightModel` stores the Eye's own
  measurements, produced by the one geometry, and nothing computes a height twice: it holds numbers,
  it does not hold a second opinion about where text goes. The tree keeps its byte, slab and height
  aggregates, and `Spine::slab_span` is the join between the two indexes — leaf position to byte
  range in one descent — so neither side ever walks the other's structure.

- **2026-11-17 — Block 5, bug hunt before merge: a break that stopped being a break.** Three real
  defects in the segmentation and transform, found by reading the code adversarially rather than by
  a failing user report.

  **1. A mark after a newline swallowed the line break.** Segmentation joined a combining mark to
  whatever preceded it, including `"\n"`. The cluster then read `"\n\u{301}"`, which is not
  `"\n"`, so the layout never took the hard-break branch: the newline was laid out as nothing and
  the next line's text joined the previous one. The caret and any renderer would have disagreed
  about where the line ended — the exact failure class this crate exists to make impossible.

  **2. A zero-width joiner reached across a newline** for the same reason, from the other side: the
  ZWJ rule was unconditional, so `👨\u{200D}\n` was one cluster.

  **3. A lone carriage return was not a break at all.** UAX #29 makes CR, LF and CR LF all mandatory
  breaks (GB3/GB4); the layout only knew two of the three, so a CR-only document rendered its line
  ends as glyphs and its carets ran through them.

  The fix is a single guard: nothing joins across or onto a break, and CR LF is the only pair that
  merges. The regression tests are the interesting part. Besides six segmentation assertions, the
  layout now has an invariant that would have caught all three at once: **the window's bytes,
  reassembled from the run tree in line order, must equal the window**. Clusters carry their text
  and lines carry their boundaries, so the bytes between one line's end and the next line's start
  are exactly the break that separated them — nothing at all for a soft wrap. A swallowed newline
  disappears from that reconstruction. The check runs over a fixed corpus and inside the randomized
  property test, so future segmentation edits have to keep the bytes.

  **Also hardened, not fixed.** `View::to_device` multiplied a coordinate by the zoom numerator in
  `i128` and cast the result with `as i64`: an absurd zoom (or a coordinate near the end of the
  range) wrapped in release and panicked in debug. It now saturates, because a coordinate that
  large is nowhere near a viewport and a saturated number stays visibly absurd where a wrapped one
  looks like geometry. `View::hit_test` debug-asserts that a line box does not map to zero device
  units, which is the one input that would make "which line is this click on" undefined.

- **2026-11-17 — Block 5, fourth slice: the height index, and a cost claim that is counted.**
  Block 5's second gate says layout cost must be invariant in document length. Half of that is the
  index that finds the viewport without reading the document; the other half is the caller that
  supplies windowed input, and the caller does not exist yet (OQ-12).

  **What landed.** `HeightModel` keeps one measured height per leaf — Block 4's slab, the unit the
  Spine already splits and re-faults, so the Eye does not invent a second partition of the text —
  in a Fenwick tree. `prefix`, `set`, `leaf_at` and `window` are O(log n) cell reads. A window is a
  leaf range plus its two document coordinates, so a scrolled viewport can be described without
  touching the index again.

  **Why the cost is counted.** A wall-clock test would measure the runner; the gate is a claim about
  the algorithm. The model counts the index cells it reads, and the tests assert numbers: a window
  over 2^18 leaves costs fewer than 160 reads, and sixteen times as many leaves costs less than
  twice the reads. This is the same discipline as the counting-allocator gate, applied to reads
  instead of allocations.

  **Zero is a value, not a hole.** A leaf is unmeasured until it has been laid out, and its height
  is zero until then. `leaf_at` steps over unmeasured leaves rather than pretending they occupy
  space, which means a window that contains them is provisional — the docs say so, and a caller that
  trusts such a window too early will see its scroll geometry move as measurement catches up. That
  is a real limitation, recorded rather than smoothed over.

  **What this does not do.** It does not decide where a leaf's text comes from when the leaf is a
  *pending* draft that exists in no spine (OQ-10), and it does not yet ask the Spine for anything.
  The next slice is that wiring, and it is the slice where OQ-10's answer has to be chosen, because
  the same walk that picks leaves must decide what a pending leaf's height means before it is
  committed.

- **2026-11-17 — Block 5, second and third slices: zoom as a view transform, style runs as layout
  data.** Both slices answer the same question — *which arithmetic is allowed to change a
  coordinate* — and both answer it by adding none.

  **Zoom/DPI has no inverse.** The tempting shape is paint multiplying by the scale and hit-test
  dividing by it: two roundings, two answers, and zoom becomes the seam Block 5 exists to close.
  [`View`] therefore exposes one mapping, `to_device`, and `hit_test` never maps a device point back
  into layout space — it compares the point against the device positions paint would draw, computed
  by that same mapping. The transform is exact rational integer scaling (`logical × zoom_milli × dpi
  / 96_000`, rounded half away from zero in `i128`). A line box maps as a whole, so one line's
  bottom is exactly the next line's top and a caret's own `y` always selects its own line; the
  caret's painted height is the distance between the mapped top and bottom, so the box a renderer
  draws is the box hit-testing tested. `Layout::paint_point` and `Layout::hit_test` are now
  `View::IDENTITY`'s answers — one implementation of the walk, not two that must agree.

  **Styles are geometry, not decoration.** A heading is not the same layout drawn bigger: bigger
  fonts have bigger advances and taller line boxes, so a renderer that applies a style the layout
  did not shape with owns a second geometry — the v3 disease wearing a hat. `Style` (size in
  thousandths of a logical unit, bold, italic) now travels as `StyledRun`s in `StyledText`; the
  `Shaper` is asked every metric question *with* the style it applies to; `Placed` carries the style
  it was shaped with; a line box is as tall as its tallest cluster and an empty line takes the
  height of the style that reaches it. Font families are deliberately absent — they are Block 8's
  pipeline decision and cannot be tested before that pipeline exists.

  **What the tests pin.** L-8's property test now sweeps a ten-string corpus (Latin, combining
  marks, ZWJ emoji, CJK, hard breaks, trailing newline, empty text) over six wrap widths × eight
  zoom steps × five DPIs, and asserts the strong form — *the same byte back* — wherever device
  positions are distinct; a zero-width-cluster shaper pins the tie rule where they are not, because
  two carets at one device position cannot be told apart by a click and the answer must not depend
  on rounding. Style tests pin the run boundary, a wrapping window with three mixed-size runs, and
  empty lines inheriting the style that reaches them. `View::IDENTITY` is asserted to answer exactly
  as the layout does, so the transform is provably the only difference between the two spaces.

  **Still `spec`.** `L-8` remains `spec`: the property holds for a fake shaper, and Block 8's real
  pipeline has not been through it. Block 5's second gate (layout cost invariant in document length)
  has no evidence at all — `layout_styled` shapes whatever window it is handed, and nothing yet
  constrains that window to a viewport (OQ-12) — and its third gate (BiDi and multi-script
  cluster-honest carets) still rests on the approximate segmentation recorded in OQ-11.

- **2026-11-17 — Block 5, first slice: one geometry, two consumers.** The Eye's geometry core lands
  as four modules with no dependencies, no font, and no window, because the property that matters
  is provable without any of them.

  **Coordinates are integers.** [`Fixed`] is an `i64` in 64ths of a logical unit. L-8 compares
  positions for equality, and floats would make "the same point" a question about rounding: paint
  would round one way and hit-test the other, reopening the seam inside the arithmetic itself. The
  only float in the crate is `to_f32`, for the GPU, and nothing compares against it.

  **Caret positions are cluster boundaries.** Segmentation is a hand-written, conservative
  approximation of UAX #29: combining marks, variation selectors, ZWJ sequences, skin tones,
  regional-indicator pairs and CRLF merge; **nothing ever splits a mark from its base**. The
  approximation errs in the safe direction — a caret may sit where a real renderer would not allow
  one (Hangul jamo stays split; OQ-11), but it can never produce two answers for one point.

  **The one list.** `layout_window` builds lines of placed clusters; `Line::boundaries` yields
  `(x, byte)` for every caret position on a line, and `paint_point` and `hit_test` are the same
  walk in opposite directions. A soft-wrapped line deliberately exposes **no** end caret, because
  wrapping inserts no character: offering one would give a byte two visual positions, which is the
  drift this block exists to prevent. Ties in `hit_test` go to the earlier boundary, so the answer
  never depends on storage order.

  **What the tests pin.** Golden geometry (`"abc"` at three-unit glyphs, exact 64ths), wrap and
  hard-break line splitting, empty lines keeping exactly one caret, click tie-breaks, every caret
  position in nine corpus strings round-tripping at six widths, and 64 randomized windows over an
  alphabet of combining marks, ZWJ emoji, skin tones, wide clusters and newlines. Advances enter
  only through the `Shaper` trait, so a fake shaper proves the law without a font.

  **Still `spec`.** `L-8` stays `spec` for two reasons, both stated in the crate docs rather than
  hidden: segmentation is approximate (gate 3's BiDi half is not attempted at all), and the
  zoom/DPI view transform does not exist yet, so "at every zoom and DPI" quantifies over nothing.
  The transform is the next slice; promoting `L-8` before it lands would be the audit failure this
  document exists to prevent.

- **2026-11-17 — Block 4, fourth slice: the live-length overlay, so a stale spine still answers.**
  D-2 recorded its own cost in plain words: the resident slab is authoritative while it is edited,
  the spine is path-copied at commit, and *the editor must carry a live-length overlay until
  commit*. Without it, a keystroke followed by a viewport query is answered by geometry that is
  wrong by exactly the number of characters typed. [`Draft`] is that overlay.

  **What it is.** A record of a handful of leaves and the lengths their resident text now has.
  Every query partitions the walk at those leaves and asks the spine its own O(log N) question per
  region, translating offsets by the accumulated difference. `document_len`, `locate` and
  `spans_in` therefore stay correct while the spine is stale, and **the view does not move when the
  commit lands**: the integration test types a character, records the draft, queries offsets,
  lengths and spans against the stale root, commits, forgets the draft, and asserts the answers are
  the same against the sealed one.

  **Why it is a projection and not a document.** It holds no plaintext, so the wipe set is
  untouched; nothing in the spine consults it, so geometry stays the only thing the tree knows; and
  `forget`/`clear` restore the sealed view exactly. That last property is what makes
  undo-after-commit sound: the old root is unchanged, so the overlay must go back to matching it.

  **Two asymmetries, both deliberate and tested.**
  - *Sealed leaves are clipped to the requested range; drafted leaves are returned whole.* A
    drafted leaf's visible portion cannot be expressed as a `Slab` — its bytes exist in no segment
    until commit — so a partial extent would name nothing. The caller clips inside the resident
    buffer it already holds.
  - *An emptied leaf occupies no space.* A drafted leaf whose text was deleted out is not a span
    and is never located; the commit boundary removes it rather than sealing an empty segment. The
    two agree because both treat "no bytes" as "not there".

  **What this entry does not claim.**
  - **No measurement, again.** Re-recording a leaf that is already drafted updates in place and
    allocates nothing; the *first* record for a leaf may allocate unless `Draft::with_capacity`
    covers the dirty set, and the editor sizes it from the viewport the way it sizes the resident
    budget. None of that is measured: the L-2 gate's window is still `locate` → resident hit →
    in-place edit, and the overlay sits outside it until a future slice folds it in or measures it
    separately. No law is promoted.
  - **Still no editor.** The overlay answers geometry questions; caret placement, selection,
    scrolling and paint are Block 5's Eye, and the wiring that decides *when* to commit — a slab
    change, a scroll, an idle timer — is a policy this crate does not make.

- **2026-11-17 — Block 4, third slice: the commit boundary, proven against the real container.**
  D-2 said the spine is path-copied "at commit" and admitted the commit boundary did not exist.
  It exists now: `commit::commit_slab` seals an edited resident slab into the world-line, takes
  the witness, and returns a new root. The chain is executed, not asserted — `holonomy-line`'s
  real `WorldLine`, `holonomy-seal`'s real capsule and DEM key, the real durability barrier, the
  real MMR — in `crates/holonomy-text/tests/commit_boundary.rs`.

  **D-3 — the commit unit is one slab's document extent, not one segment's, and the fence precedes
  the append.** Two decisions, both forced:
  - *One slab's extent.* A re-seal replaces exactly the range one leaf covers
    (`Spine::replace_range`), so no offset outside that range moves and no neighbour is rewritten.
    Committing at **segment** granularity instead would mean rewriting every leaf naming the edited
    segment and shifting their start offsets after the edit point — the offset arithmetic that
    silently duplicates or loses bytes, in every leaf, on every commit. A segment already cut into
    several leaves is committed leaf by leaf; each commit seals that leaf's bytes as a fresh
    segment.
  - *The fence precedes the append.* `commit_slab` refuses a span the spine no longer holds
    (offset moved, leaf replaced, or offset no longer at a leaf start) **before** the world-line is
    touched. Appending first and discovering the span was stale afterwards would leave an orphan
    segment — durable, authenticated, unreferenced, and indistinguishable from a real one in the
    file. Undo-then-recommit is exactly how a real editor would produce that, so it is a test, not
    a hypothetical.

  **What the tests prove.** One typed character inside a resident slab becomes segment 1; the new
  root names it over the whole extent the old leaf covered; the old root still describes the
  document it was cut from; a fresh handle on the file sees both segments, the same segment root,
  and passes a full `verify()`; a `ResidentSet` bound to that world-line faults in exactly the
  committed bytes for the segment the new root names. Committing an emptied slab removes the leaf
  and appends **nothing** (no empty segment, no witness), committing the same span twice is refused
  with the world-line byte-for-byte unchanged, and a refused append leaves the spine alone. Undo
  across a commit walks back to a root whose segment was never rewritten.

  **What this entry does not claim.**
  - **No measurement.** Commitment allocates — path copies, the segment record, the witness — and
    is supposed to. L-2's zero-allocation claim stays exactly as narrow as the keystroke binary
    measures it: `locate` → resident hit → in-place edit. Nothing here promotes L-2, and no new
    number was taken.
  - **No editor.** The spine stays stale between commits by design; the editor's live-length
    overlay is still unbuilt, so a keystroke followed by a viewport query is *not* answered by this
    code — the overlay answers it. That overlay, and wiring it to this boundary, is Block 5's.
  - **No coalescing.** The commit replaces one leaf and never merges neighbours, so it creates no
    fragments (the tests assert the leaf count is unchanged across a commit) but it does not repair
    fragments either. OQ-9 is narrowed below, not closed.

  **What the first push cost.** Two compile-level findings, both about the test, neither about the
  boundary: `drop(sink)` on a type that implements no `Drop` (clippy's `drop_non_drop`; NLL already
  ends the borrow at the sink's last use) and `assert_eq!` over `Result<Committed, _>`, which cannot
  work because `Committed` holds a `Spine` and `Spine` deliberately has no `PartialEq` — two spines
  with equal bytes are not the same spine. The refusals are compared as errors instead. A design
  constraint became a compile error in the tests, which is where it belongs.

  **Dependency direction.** `holonomy-line` and `holonomy-seal` are **dev**-dependencies of
  `holonomy-text`: the shipped Spine links neither, and speaks to both through `SegmentSink` /
  `SegmentSource`. The adapters live in the test file because the application crate that will own
  the production wiring does not exist yet — putting them there means CI executes the seam rather
  than a comment describing it.

- **2026-11-17 — bug hunt before the commit boundary: property tests and three hardening fixes.**
  The Spine's own tests were scripted — a 4,096-append sequence, every byte of a 7-slab split — and
  scripted tests cannot find the shape nobody wrote down. This entry adds the machinery that can,
  and fixes what inspection found in the meantime.

  **Property tests now in CI:**
  - `tree::check_invariants` recomputes bytes, slab count and height from the leaves and panics on
    any disagreement with the cached aggregates, on an AVL violation, on an `Empty` child that
    should have been normalized away, or on a zero-byte leaf.
  - `random_join_and_split_shapes_keep_every_invariant` — 400 random appends, splits, rejoins and
    block concatenations per seed, over four seeds, checking aggregates against an independent
    count after **every** operation.
  - `random_edits_match_a_byte_level_model` — 250 random appends, insertions, removals and
    split/rejoins per seed, over four seeds, verified against a trivial `Vec<(segment, offset)>`
    model: same length after every step, spans tile the document contiguously, the expanded
    geometry is byte-identical to the model, and `locate` agrees with it. This is the test that
    catches a tree that keeps the right length while scrambling where the bytes came from.
  - `random_edits_match_a_string_model` — 500 random insertions and deletions per seed against a
    `String`, over four seeds, using multi-byte characters (é, 中, 🙂) so the boundary rules are
    exercised at every width.

  **Fixed by inspection (each now has a test or an invariant that would catch a regression):**
  - Zero-length slabs could enter the tree as leaves that carry no bytes, inflating `slab_count`,
    path copies and any budget computed from slab counts. `from_slabs`, `append` and `insert_slab`
    now drop them, and `check_invariants` rejects a zero-byte leaf outright.
  - `Slab::new` accepted `start + len` beyond `u32`, which would have made `end()` (and every
    offset derived from a slab) lie. It now refuses at construction.
  - `SlabText::insert` computed `len + text.len()` in a plain addition; it now saturates, so the
    refusal condition cannot wrap on a 32-bit target, and the one remaining panic in
    `from_plaintext` (an absurd configured slack) is documented rather than discovered.

  **Found and not fixed — recorded as OQ-8:** `History` keeps one root per state, so an unbounded
  undo history grows with the number of *edits*, not with the document. Subtrees are shared, so
  each root costs O(log N) nodes, but years of typing is still an unbounded set of them, and the
  world-line on disk already stores the history. Bounding the in-memory history is a design
  decision (ring of roots plus reconstruction from the log), not a patch, and it belongs before
  Block 6 rather than after it.

  **What CI said about the hunt itself, on the first push.** The property tests were written
  without a local compiler, and CI (run
  [44046796516](https://github.com/c4us4l1ty/holonomy/actions/runs/44046796516)) found exactly the
  class of error that reading cannot: `check_invariants` reported no drift, the AVL checks held,
  and the only failures were in the *test's own model*. The new shape test compared the tree's leaf
  count with the model's slab count, and seed 1 disagreed by one — six leaves against five slabs.
  The tree was right and the model was wrong: a cut inside a leaf produces two leaves for one
  segment and `join` never merges them back, which is the documented design (a segment's bytes may
  span several slabs) and the reason `splitting_and_rejoining_every_byte_preserves_the_sequence`
  already compares bytes rather than leaves. Counting "inside the document" was still one leaf too
  generous, and the second red run said so (28 modelled against 27 real): a cut can be strictly
  inside the document and still fall exactly on a leaf boundary, which spends nothing. The test now
  carries an independent set of leaf-start offsets and asserts leaves equal its size, which is exact
  — and it was replayed against a mirror of `join`/`split` for 16,000 random operations before the
  third push rather than trusting a fourth blind run. Two `clippy::unusual_byte_groupings` lints in
  the new seed arrays were the only other findings.

  **A bug in the diagnostic channel itself.** The third push went red on the `rustfmt` job while
  the bridge reported `cargo fmt --all -- --check` as **exit 0** — because the bridge ran
  `cargo fmt --all` (to capture the formatter's patch) *before* the loop, so the check it printed
  was inspecting a tree the formatter had already rewritten and could never fail. The check now
  runs first and the rewrite happens after it; the patch is appended to the report afterwards, so
  the verbatim patch is still what gets applied and the printed status is finally the real one.
  This is the second time this session a tool that reported success was the problem — the first
  was the keystroke gate measuring the default allocator.

  **What this entry does not claim.** The hunt covered `holonomy-text` — the code written this
  session and the only part not yet exercised by a property test. `holonomy-seal` and
  `holonomy-line` were read for consistency with their audits but not re-attacked; that is Block 9's
  implementation audit, still owed. No new measurement was taken: these are tests that run in CI,
  and every one of them is a claim a future change can fail.

- **2026-11-17 — Block 4, second slice: the keystroke path is measured, not asserted.** CI run
  [44045023810](https://github.com/c4us4l1ty/holonomy/actions/runs/44045023810) is green, and the
  workspace test job now executes a binary whose global allocator **counts**.

  **The claim.** `locate` (a descent over cached aggregates) → resident hit → in-place insert of one
  typed character performs **zero** allocations. Also zero: a backspace, filling the slab's spare
  capacity, and the refusal that follows when it is full. The count is taken by
  `holonomy_ledger::alloc::claim_quiet`, which panics with the measured delta rather than logging
  it. A control in the same binary asserts that faulting a segment in on a *miss* **does** allocate:
  if that control ever stops counting, the binary is not measuring anything and every zero above is
  worthless — the exact failure Block 1 recorded when its first unit-test version measured the
  default allocator.

  **Design decisions this slice forced, both recorded as decisions rather than preferences:**

  - **D-2 — edits are in place per keystroke; the spine is path-copied at commit.** With an
    immutable path-copied tree, updating a leaf's length per character would allocate O(log N)
    nodes per keystroke and make the L-2 gate unreachable by construction. So the resident slab is
    authoritative while it is being edited, and the spine is path-copied when the slab is re-sealed
    or the viewport moves — counted, occasional work rather than per-character work. The consequence
    is explicit: the editor must carry a live-length overlay until commit, and that overlay is not
    built. Nothing here claims a keystroke updates the spine.
  - **Recency is a counter, not a deque.** A `VecDeque` hit moves the entry to the back, and
    `push_back` may reallocate — an allocator call inside the very path being measured. The resident
    set now scans for the smallest `last_used` on a *miss* instead, which cannot allocate and costs
    O(resident), and the resident set is viewport-sized.

  **The budget got more honest.** `ResidentSet` now accounts its bound against **allocated
  capacity**, not text length: the slack reserved for typing is memory, and a bound that ignored it
  would be a bound in name only. Allocation-side accounting is what a memory adversary actually
  measures. Plaintext length is still reported separately.

  **Growth is refused, never done quietly.** `SlabText::insert` returns
  `EditOutcome::WouldSplit { needed, capacity }` and leaves the buffer untouched when the edit does
  not fit. A buffer that grew instead would hide an allocation inside the keystroke path and break
  the law silently; refusing turns the event into a structural one, where allocation is expected
  and counted.

  **UTF-8 is checked where the cost is, not everywhere.** Whole-buffer validation on each keystroke
  would cost O(slab) per character — precisely the "cost proportional to what changed" failure the
  architecture exists to prevent. The buffer is validated once at fault-in; each edit validates its
  own inserted bytes and its own boundary bytes. Grapheme clusters remain the Eye's business, not
  this type's.

  **CI trail.** Run 44044481285 caught rustfmt drift plus one real test-arithmetic error: an
  insertion at offset 9 into a buffer that had become 8 bytes long — caught by the test asserting
  the outcome of an earlier insert, not by review. 44045023810 is green, including the new counting
  binary. Clippy was clean on the first pass this time.

  **What this does not claim.** L-2 as a law stays `spec`: the keystroke half is measured, but the
  document-scale half — the same resident bound at one page and at thirty million words on reference
  hardware — has not been measured, and the commit boundary, the editor, and the Eye do not exist.
  The counting binary measures one test function in its own process; the module docs explain why
  sibling tests would race the counters. Nothing here promotes a law in `docs/GATES.md`.

- **2026-11-17 — Block 4, first slice: the Spine and the resident set.** `holonomy-text` now
  holds the structural half of RFC-003 and nothing of its measured half. CI run
  [44042785220](https://github.com/c4us4l1ty/holonomy/actions/runs/44042785220) is green: rustfmt,
  Clippy, 35 in-crate tests, the gates contract, and both qualification rehearsals.

  **Landed.** `Spine` — an immutable tree over `Slab { segment, start, len }` references, one
  slab being a byte range of one sealed segment. It is AVL-balanced, so ordered appends cannot
  degenerate into a list (a 4,096-append test pins height ≤ 24 against a degenerate 4,096);
  interior nodes carry bytes, slab count and height, so `locate` and `spans_in` are descents.
  Every edit returns a new root and path-copies O(log N) nodes — a test counts the shared
  `Arc` pointers and fails if an append copies more than its path. `History` keeps roots, so
  undo and redo are pointer swaps and no plaintext enters history. `ResidentSet` enforces a byte
  budget fixed at construction, evicts LRU from the front, wipes on eviction through
  `Zeroizing`, and offers `retain` for viewport moves; `SegmentSource` is the seam the
  world-line will fill, so the Spine still does not depend on a crypto crate.

  The L-2 *mechanism* is visible in test form: a 916-slab, 60,000,000-byte simulated lifetime
  scrolls end to end at a four-segment budget, resident bytes never exceed 262,144, each segment
  is faulted exactly once, and the same budget serves a two-slab document unchanged. This is the
  mechanism, not the gate: L-2 still reads `spec` in `docs/GATES.md`, and no reference-hardware
  memory measurement exists.

  **Decision D-1 — the flat Fenwick is subsumed by the tree's aggregates.** RFC-003 §4 named a
  Fenwick over slab heights. A flat Fenwick would need rebuilding beside every path copy and
  could disagree with the tree it indexes while computing the same prefix sums; the tree already
  carries bytes, slab count and height per subtree, so the descent *is* the index. Adopted
  subtree aggregates (with a note that the Eye's layout heights will ride the same aggregate),
  rejected the second structure, and amended RFC-003 §4 and the roadmap deliverable. The trade
  is explicit: one index cannot drift, and a future Eye height update path-copies O(log N) nodes
  instead of updating one array slot.

  **What this slice does not do.** No mutable resident text buffer, therefore no keystroke path,
  therefore no zero-allocation claim — asserting that before building it is the exact unmeasured
  promise VISION §5 refuses. No Eye, so no layout heights, no paint/hit-test geometry. Wipe is by
  construction (`Zeroizing` on eviction), not proven: the canary and the counting allocator that
  would prove it belong to Block 9. And the Block 4 gates below remain untouched:
  open-memory equivalence at 30 M words, offset-invariant keystroke latency, and the 1 M-keystroke
  fuzz run have not been attempted on any hardware.

  **CI trail for this slice** (recorded because "the compiler is CI" is a claim worth checking):
  run 44041603841 caught formatting, a missing `is_empty` companion and four `assert_eq!`s
  comparing `Result<Spine, _>` (Spine deliberately has no `PartialEq`); 44041980922 caught the
  same comparison errors after the formatter was satisfied; 44042360027 caught a real test-logic
  error — the LRU assertion expected eviction before the budget was full; 44042785220 is green.
  The diagnostics bridge was also made lossless in this slice: it now runs `cargo fmt --all`,
  posts the formatter's patch verbatim for `git apply`, and base64-chunks its report, because the
  previous single 60,000-byte comment silently dropped every command after the fmt diff.

- **2026-11-17 — gate-closure amendment: Block 2 closed, Block 3 closed in scope, Block 4
  unblocked.** Two gate items could not be answered with evidence available to this project — an
  allocator-wide proof of dependency-internal wiping, and physical power-loss/device qualification —
  and leaving them "open" had the same effect as killing the roadmap, because Block 4 may not
  start. The protocol already defines the third path: **amended** — the claim survives in weakened,
  explicitly stated form. Each claim below is narrowed to what a test establishes, the remainder is
  named as an unproven residual, and it is assigned to the gate that can measure it. Earlier audit
  entries that still describe these gates as open are left exactly as written — the trail records
  what was known on the day, and this entry supersedes them without rewriting them.

  **Evidence base:** the code and test tree at `8ac3618`, whose CI run
  [44034435423](https://github.com/c4us4l1ty/holonomy/actions/runs/44034435423) passed rustfmt,
  Clippy, the feature-graph check, workspace tests, the gates contract, and both the rsync and
  ext4-ENOSPC rehearsals. Nothing in this entry is a new measurement; it is a decision about what
  the existing measurements are allowed to mean.

  | Item | Verdict | Now claimed | Not claimed | Owed to |
  | ---- | ------- | ----------- | ----------- | ------- |
  | Block 2 — dependency-temporary zeroization | **amended** | every project-owned secret buffer is zero before free under the registered-allocation audit (nonzero-at-free, secret reallocation, and registry overflow all fail the gate); the reviewed wipe features are enabled and CI-enforced | that dependency-internal stack/heap temporaries are erased; a whole-allocator proof; any register or physical-memory guarantee | future hardening; residual in TM §5 |
  | Block 2 — RFC-001 audit closure | **amended** | every open RFC-001 item is killed, scoped with its exact limit, or assigned to its owning block | that the original unbounded wording was ever proven | — |
  | Block 3 — L-6 crash consistency | **amended** | the append/commit/recover ordering behaves correctly under injected short writes at every byte, both sync barriers, truncate failure, and real ENOSPC on an ext4 loop image | power-loss, controller/device-cache durability, journal replay, any supported-device matrix | Block 6 *kill-power-at-random-instant* gate; Block 10 release gate |
  | Block 3 — L-10 tail transfer | **amended** | one named client (rsync 3.2.7, protocol 31, 64 KiB block, local daemon over TCP) transfers literal data ≤ appended bytes + one block and ≤ 8× appended application-layer bytes for the measured workload | that any other client, backend, or remote service behaves the same | Block 10 backend matrix, or an explicit narrowing of L-10 in `LAWS` |
  | OQ-4 — scroll-timing side channel | **closed — accept and disclose** | the slab fault-in pattern is observable, and is already observable at the I/O layer: reading an old page reads its ciphertext at a known offset, so timing smoothing would hide the pattern from nobody who can observe the reads | that the pattern is hidden from a timing observer | TM §5 residual |

  **Protocol consequences, written into the repo by this entry.** `ROADMAP.md` Blocks 2 and 3 now
  carry these wordings and the named deferrals; `RFC-001` and `RFC-002` status lines and their
  zeroization/power-loss paragraphs match them; `docs/THREAT-MODEL.md` §4 and §5 gain the
  dependency-scratch, power-loss and scroll-timing residuals; OQ-4 moves from open to closed.
  `crates/holonomy-ledger/src/laws.rs` and the generated `docs/GATES.md` are **untouched**: no law is
  promoted, and L-6 and L-10 still read `spec` because neither has been measured to its bound.

  **What this entry does not do.** It does not certify v6, promote a law, or convert a residual into
  a claim. It moves an obligation from "nobody owns this" to "these gates own this". A future entry
  that treats these obligations as settled because this one filed them is invalid. If Block 10
  cannot obtain the hardware matrix, the honest outcome is to narrow L-6's bound in `laws.rs` — a
  visible edit to the machine-readable contract — not to pass it silently.

- **2026-11-16 — Block 2 dependency-temporary zeroization audit (gate remains open):**
  Reviewed the selected secret-bearing source paths at the release revisions below. `Cargo.lock`
  pins registry archives by version and checksum; the source links identify the corresponding
  release commits used for this static review. This is not a binary, allocator-wide, register, or
  physical-memory erasure proof.

  | Review area | Pinned source revisions reviewed |
  | ----------- | ------------------------------- |
  | NFC normalization | [`unicode-normalization` 0.1.26 (16c9597)](https://github.com/unicode-rs/unicode-normalization/tree/16c9597b73ec5b6b8ed55f4249a5ac854892544c); [`tinyvec` 1.13.3 (e343bbb)](https://github.com/Lokathor/tinyvec/tree/e343bbbb5a6594c9d57de718e6f232e6c0115dcb) |
  | KDF/hash | [`argon2` 0.5.3 (2b6f968)](https://github.com/RustCrypto/password-hashes/tree/2b6f9688625db3db798b35c8a209e4799bb60e11/argon2); [`blake2` 0.10.6 (1f727ce)](https://github.com/RustCrypto/hashes/tree/1f727ce37ff40fa0cce84eb8543a45bdd3ca4a4e/blake2); `digest` 0.10.7 ([3443894](https://github.com/RustCrypto/traits/tree/344389411fd9718a0742435152e933a9e71461ee/digest)) |
  | Hybrid KEM / DH | [`ml-kem` 0.3.4 (c53fd02)](https://github.com/RustCrypto/KEMs/tree/c53fd02ba7dcf5dc2c9287f814d1e401f26bfcce/ml-kem); [`x25519-dalek` 2.0.1 (4ac84dd)](https://github.com/dalek-cryptography/curve25519-dalek/tree/4ac84dd0668b1d2e51654fcdffe4e6a687bef00/x25519-dalek); `curve25519-dalek` 4.1.3 ([5312a03](https://github.com/dalek-cryptography/curve25519-dalek/tree/5312a0311ec40df95be953eacfa8a11b9a34bc54/curve25519-dalek)) |
  | AEAD / MAC | [`chacha20poly1305` 0.10.1 (c5c1999)](https://github.com/RustCrypto/AEADs/tree/c5c199953aba561261bed1c55f62b9c19d0f49cf/chacha20poly1305); [`chacha20` 0.9.1 (9c40806)](https://github.com/RustCrypto/stream-ciphers/tree/9c4080658507df4994508c1ef788f7eb89fe628c/chacha20); `poly1305` 0.8.0 ([35de31a](https://github.com/RustCrypto/universal-hashes/tree/35de31af7396fe57fa28265e91532c816183c47f/poly1305)) |
  | Sponge state / wipe support | [`sha3` 0.12.0 (0bdea4f)](https://github.com/RustCrypto/hashes/tree/0bdea4f919f86c17f75c6177567520010fedb735/sha3); [`shake` 0.1.0 (8f3e56d)](https://github.com/RustCrypto/XOFs/tree/8f3e56dc4e197b8adbc3d5a9ed109e326d6b159c/shake); [`sponge-cursor` 0.1.0 (9c8674f)](https://github.com/RustCrypto/utils/tree/9c8674f4fdf00fb524cd06d89da29d125db07582/sponge-cursor); `digest` 0.11.3 ([2fb9ed8](https://github.com/RustCrypto/traits/tree/2fb9ed8922e244117040bb037a7d141a6a2b8228/digest)), `block-buffer` 0.12.1 ([cbd0963](https://github.com/RustCrypto/utils/tree/cbd0963c685df025c42bed31f78c50d1bada3805/block-buffer)), and `zeroize` 1.9.1 ([34a6ebc](https://github.com/RustCrypto/utils/tree/34a6ebcb9b920ec7ae5809e470fce94fe7c0cb55/zeroize)) |

  The ML-KEM source checkout was at peeled commit `c53fd02ba7dcf5dc2c9287f814d1e401f26bfcce`;
  its annotated release tag `ml-kem/v0.3.4` has tag-object ID
  `6855b16a6cca9fc1c3eaea76776a7cd50ad67846`. This resolves the earlier tag-object/commit
  ambiguity.

  Findings and scoped mitigations:

  - **Passphrase normalization:** `unicode-normalization`'s NFC iterator stores pending canonical
    decomposition/recomposition sequences in `TinyVec` buffers with inline capacity four
    (`src/decompose.rs`, `src/recompose.rs`). Long combining runs spill to heap. The reviewed
    `tinyvec` source has no `Zeroize` or wipe-on-drop integration; normal drop releases its owned
    buffer but does not erase its bytes, and a moving growth allocation cannot recover/wipe old
    storage. `PassphraseInput::new` checks raw UTF-8 length before invoking NFC, so the 4,096-byte
    cap bounds the input driving that scratch. The normalized output is separately written to a
    fixed-capacity `Zeroizing<String>` and is not grown. A regression now fills the full 4,096-byte
    input allowance with one long U+0301 combining run and verifies rejection at 1,026 normalized
    bytes. This test exercises the cap/guard path; it does not observe or prove wiping of TinyVec
    scratch. The input is borrowed as `&str`, so the caller's original text is not wiped by this
    API. The raw-input cap also rejects inputs over 4,096 bytes even before their normalized
    length is known.

  - **Argon2 and BLAKE2:** the `argon2` `zeroize` feature gives `Block` an explicit `Zeroize`
    implementation, but not automatic drop wiping. Holonomy's caller-owned `Arena` iterates over
    all blocks and wipes them before freeing the slab; the byte-equality test and registered
    allocation test cover that project-owned arena. In the reviewed Argon2 source, the 1 KiB
    password-derived `hash` array is loaded into a block but not then wiped; `Block::compress`
    creates secret-derived `Copy` temporaries (`r` and `q`) without visible wipes; and
    `blake2b_long`/BLAKE2 hasher state and intermediate digest material have no reviewed wipe
    hooks. Argon2 explicitly wipes `initial_hash` after seed-block generation and its final block
    plus byte copy after successful final hashing, but error returns can precede those wipes. The
    direct `blake2` 0.10.6 pin has no `zeroize` feature. Holonomy also wipes BLAKE2 digest output
    arrays in `derive_subkey` and CMT-4, but those output wipes do not erase the dependency's
    internal hasher state (CMT-4's input includes the key).

  - **ML-KEM:** with the enabled `zeroize` feature, `DecapsulationKey::drop` wipes stored `dk_pke`,
    seed `d` when present, and `z`; the PKE decryption key exposes `Zeroize` for its stored
    `s_hat`. That covers the retained key object, not its working copies. Reviewed key-generation
    and PKE locals (`sigma`, `s`, `e`, `s_hat`/`e_hat` and arithmetic results), decrypt locals
    (`sTu`, `w`, and message output), and the decapsulation `mp`, `Kp`/`rp`, `Kbar`, and reencryption
    `cp` have no visible zeroize-on-drop path. The feature-graph check cannot establish cleanup of
    these locals.

  - **X25519 and XChaCha20-Poly1305:** the active `x25519-dalek` `zeroize` edge enables drop wipes
    for `StaticSecret` and `SharedSecret`. `SharedSecret::to_bytes()` makes an ordinary array copy;
    Holonomy wraps its copies in `Zeroizing` at the envelope call sites. This is not proof that
    every curve-operation stack intermediate is erased. The pinned AEAD wipes its stored key on
    drop, its local Poly1305 key after MAC-state construction, and the ChaCha core state under the
    feature. A direct `poly1305/zeroize` edge is required because `chacha20poly1305` 0.10.1 does
    not forward that optional feature; its state now has a feature-gated drop wipe. In the used
    XChaCha path, `hchacha`'s stack state and the returned local derived-subkey copy have no
    explicit wipe before return/drop.

  - **SHA-3/SHAKE:** direct `sha3/zeroize` and `shake/zeroize` edges enable feature-gated state
    and reader-state wiping; the `sponge-cursor` implementation itself zeroizes only its position
    field. `digest` 0.11.3's `zeroize` feature weakly forwards the feature to `block-buffer` when
    that optional dependency is present; `block-buffer` 0.12.1's gated Drop implementation wipes
    its internal buffer and cursor. The CI feature-graph check requires the SHA-3/SHAKE, digest,
    and sponge-cursor edges, along with Argon2, ChaCha20, curve25519-dalek, ML-KEM, Poly1305, and
    X25519; it passed after the lockfile correction. Feature activation is useful coverage, not
    evidence that every implementation or compiler-generated temporary is wiped.

  The first follow-up run, [44031374849](https://github.com/c4us4l1ty/holonomy/actions/runs/44031374849),
  exposed a stale `Cargo.lock`: the SHA-3/SHAKE feature-unified dependency resolution produced a
  `block-buffer` 0.12.1 package entry and disambiguated package edges, but the earlier manual lock
  update had omitted them. The lockfile now records that metadata (commit `8e796f7`). The earlier failed-run logs for
  [44031050938](https://github.com/c4us4l1ty/holonomy/actions/runs/44031050938) were unavailable;
  no cause is inferred for that run. CI [44031769120](https://github.com/c4us4l1ty/holonomy/actions/runs/44031769120)
  passed rustfmt, Clippy, the feature-graph check, workspace tests, the gates contract, and both
  L-6/L-10 rehearsals. Those two gates remain `spec`; Block 4 has not started. The next run,
  [44032166408](https://github.com/c4us4l1ty/holonomy/actions/runs/44032166408), exercised the
  maximum-input combining-sequence regression: its test, Clippy, gates, and both qualification
  jobs passed, while rustfmt requested a line-wrap correction. Diagnostics confirmed the failure
  was formatting-only. Commit `a190519` applies that correction and adds an explicit `digest/zeroize`
  graph assertion; CI [44033037131](https://github.com/c4us4l1ty/holonomy/actions/runs/44033037131)
  then passed rustfmt, Clippy, the strengthened feature-graph check, workspace tests, the gates
  contract, and both L-6/L-10 rehearsals. All code and feature-graph checks are green; L-6 and
  L-10 remain `spec` and Block 4 has not started.

  **Verdict:** the dependency-temporary zeroization gate remains open. Current bounded input and
  enabled feature edges narrow exposure and improve cleanup for selected retained states, but
  known stack/heap temporaries remain unwiped or not proven wiped. The registered-allocation test
  only checks project buffers explicitly registered with it; neither that test nor source review
  establishes universal secret erasure. See RFC-001's remaining zeroization question.

- **2026-11-16 — Block 2/3 CI repair and qualification evidence review (Block 4 not started):**
  CI run [44017105999](https://github.com/c4us4l1ty/holonomy/actions/runs/44017105999) passed
  rustfmt, clippy (`-D warnings`), workspace tests, the gates contract, and both qualification
  jobs; the diagnostics bridge was skipped because no job failed. The rustfmt failure from run
  44013849921 was actual formatting drift under pinned Rust 1.100.0 (the diagnostics reproduction
  also exited 1), not a toolchain mismatch. Clippy's three `needless_borrows_for_generic_args`
  findings were corrected with byte-slice borrows, avoiding by-value secret-array copies. The NFC
  expansion test now expects 1,026 bytes: the first two-byte mark from input 257 crosses the
  1,024-byte bound, and the bounded normalizer does not write the second mark. A first ext4-test
  repair still failed in run 44016177594 because the filler reported ENOSPC with 352,256 bytes
  still available. The final harness aligns the prior witness to the 4,096-byte block boundary
  and makes one append request more than the entire bounded filesystem using repeated borrowed
  64 KiB slices; it verifies the appender's actual ENOSPC error and recovery.
  - **L-6:** the [recorded ext4 result](https://github.com/c4us4l1ty/holonomy/issues/51#issuecomment-6938306377)
    used a 128 MiB image, ext4 on `/dev/loop0`, Linux 7.0.0-1017-azure x86_64, and e2fsprogs
    1.47.0. The filesystem reported 108,974,080 bytes total, 4,096-byte blocks, and 106,266,624
    initially free. The filler consumed 105,906,176 bytes and received ENOSPC on write, with
    352,256 bytes still available. The append requested 1,663 segments (108,986,368 plaintext
    bytes, more than the filesystem size) and returned OS error 28 (`ENOSPC`). Reopen recovered
    one committed segment, retained a 352,256-byte uncommitted tail, and verified that segment.
    This is evidence for that ext4 loop configuration only, not physical power loss, controller
    caches, or a supported-device matrix; **L-6 remains `spec`**.
  - **L-10:** the [passing rsync run](https://github.com/c4us4l1ty/holonomy/issues/51#issuecomment-6938309911)
    used rsync 3.2.7 / protocol 31 over a local daemon on TCP with a 65,536-byte block. Old/new
    file sizes were 60,093,554 / 60,356,483 bytes; the append was 262,929 bytes. Delta literal
    data was 325,507 bytes; client bytes sent/received were 329,363 / 5,545, for 334,908 total
    application-layer bytes. This remains one client/build/configuration/workload rather than a
    qualification of remote services or other clients; **L-10 remains `spec`**.
  - **CI trigger review:** `.github/workflows/ci.yml` contains a `pull_request` trigger. PRs run
    the read-only validation jobs; `diag`, `sync`, and `filesystem` are explicitly push-only
    before their issue-writing steps can run. The earlier check that reported no PR trigger was
    a false negative. The diagnostics bridge now captures each command to a file before capping
    displayed output, preserving its actual exit status instead of risking SIGPIPE from `head`.
  The allocation audit still does not cover every dependency-owned temporary, so RFC-001's
  dependency-temporary zeroization gate remains open.

- **2026-11-16 — follow-up Block 2/3 review (implementation pending CI):** found and corrected
  five concrete boundary defects: Linux `MADV_DONTDUMP` used the wrong advice value and did not
  page-align the common small heap-cell range; `LockedSecret` declared its boxed bytes before the
  lock guard, so field drop order could free the allocation before `munlock`; page-granular
  `munlock` calls on separate tiny heap boxes could unlock a page still holding another key; the
  public `HybridKem`/`MlKem1024X25519` test abstraction could return `SharedSecrets` with raw key
  bytes outside the capability API; and `x25519-dalek` had default features disabled without
  enabling its optional `zeroize` feature, leaving `StaticSecret`/`SharedSecret` drops unscrubbed.
  The KEM plumbing is now crate-private, the lock advice is page-aligned and uses libc's Linux
  constant, each secret cell gets a separate 64 KiB-aligned lock unit with the guard dropping
  before its allocation is freed, and the X25519 `zeroize` feature is enabled and locked.
  The threat model/RFC wording was also corrected: `memfd_secret` is probed on supported Linux
  architectures but not used, and there is no global core-dump disable or `DONTFORK` implementation.
  Added a test-only registered-allocation
  wipe audit, AEAD/envelope corruption tests, an ignored rsync transfer harness, and an ext4-loop
  ENOSPC/recovery rehearsal. Follow-up review also fixed the private `MlKem1024X25519` import,
  wrapped ML-KEM shared-key/seed copies, and changed AEAD encryption to use wipe-on-drop
  in-place buffers. A further passphrase review replaced `NFC.collect::<String>()` with a
  pre-sized, wipe-on-drop buffer and bounded pushes, avoiding growth reallocations before the
  guard exists; an NFC-expansion regression test covers the size boundary. At that point, the
  Rust changes, tests, and manual lockfile feature edges had not been compiled or independently
  validated; workflow YAML parsing and shell syntax checks passed. The allocation test does not by
  itself audit every dependency-owned temporary; RFC-001's zeroization gate remains open. Local
  execution was blocked because `cargo`, `rustc`, `rsync`, `mkfs.ext4`, and QEMU were absent;
  invoking `scripts/qualify-ext4-enospc.sh` exited 127 at line 20 (`mkfs.ext4: command not
  found`). No filesystem ENOSPC result or rsync measurement had been collected yet. The ext4
  rehearsal cannot emulate physical power loss or controller caches, and the rsync run is limited
  to its measured version/configuration/workload; keep L-6 and L-10 `spec` pending evidence review.

- **2026-11-16** — Block 3 fault-injection and tail-write evidence: routed append data, witness,
  sync, and recovery truncation through private I/O wrappers with `cfg(test)` failpoints only.
  Tests inject a short write after every byte of a two-segment append, fail the data and final
  witness sync barriers separately, and fail both tail truncation and its sync. Each uncertain
  operation poisons the handle; reopen/verify finds only the last complete authenticated witness
  or a complete authenticated new witness. A 30-million-word simulated file now records the
  completed byte counts returned by the appender's actual `File::write` calls and offsets sampled
  immediately before them; writes begin at the previous EOF, total `AppendReceipt::bytes_appended`,
  and preserve the old prefix.
  This is local writer evidence, not physical disk-sector or backup-network measurement. Injected
  errors are returned before OS sync/truncate operations; no real ENOSPC, filesystem failure,
  process/device power cut, or filesystem/hardware durability qualification was run, so **L-6
  remains `spec`**. No concrete backup client was measured, so **L-10 remains `spec`**. OQ-7's
  valid-prefix rollback limitation remains accepted and open. CI run 43926260051 first caught
  Clippy's `missing_const_for_thread_local`; fixed in `58ee6ac`. CI run 43926666233 then passed
  rustfmt, clippy, workspace tests, and the gates contract.

- **2026-11-16** — Block 3 implementation draft: added the fixed `.wavefunction` header,
  fenced KDF replay, sealed segment frames, MMR witness commits, lazy body reads, explicit
  passphrase-confirmed compaction, and same-directory atomic publication. Tests specify
  every-byte append-tail cuts, 30-million-word simulated growth with exact-prefix preservation,
  historical metadata/body tampering, stale-writer refusal, shared-open/exclusive-append lock
  contention, no-replace creation/compaction, and compaction behavior. Local Rust tooling is
  absent; the first CI run (43922948005) found a missing `Debug` derive used by MMR tests, fixed
  in `50ac36f`. CI run 43923405534 then passed rustfmt, clippy, workspace tests, and the gates
  contract. Block 3 remains a draft, not a passed L-6/L-10 gate: the initial crash matrix used
  synthetic byte-prefix truncations only, which did not establish real power-loss or filesystem
  failure behavior. `metadata_digest` uses variable-output BLAKE2b-256 rather than truncating
  BLAKE2b-512; its reference-vector regression test passed in workspace CI.

- **2026-11-16** — Block 2 residuals closed. **OQ-5**: Argon2 now runs exclusively through
  `kdf::run_argon2`, which derives over a caller-owned `Arena` whose `Drop` zeroizes the
  whole m-cost slab before free (argon2 `zeroize` feature enabled at the workspace pin);
  byte-equality with argon2's internal-allocator output is test-pinned so the scrub layer
  can never silently alter derivations. **OQ-6**: `derive_root_secret` carries the
  `TestScheduleRefused` guard, active exactly in non-debug non-test builds; the nil-profile
  twin keeps call sites identical. The replay path (`derive_root_secret_with` →
  `MasterKeys::derive_with`) gained its first consumer plus a pairwise test pinning
  `derive == derive_with` for equal parameters — the cross-machine portability contract
  Block 3's header will rely on.

- **2026-11-16** — Block 2 implementation + RFC-001 attack cycle. Verdicts: **AC-1**
  (capsule replay across files) → amended — RFC-002 §6 makes the witness commit to the
  envelope epoch; Block 3's gate gains a cross-file open-refusal test. **AC-2** (argon2
  arena wipe) → open as OQ-5 (v5's fix is the port reference). **AC-3** (wrong-passphrase
  distinguishability) → survived — KDF draws no USB material, the `UnwrapCapsule`
  commitment runs before any KEM at fixed schedule, and `OpenRejected` stays coarse with
  tests pinning indistinguishability. **AC-4** (USB slot malleability) → survived — the
  presence flag sits inside both the unwrap context and the commitment's hashed material,
  and the tests pin the cross-state refusal. **AC-5** (`ParamsClass::Test` misuse) → open
  as OQ-6. **AC-6** (Elligator absence re-attack) → survived — no Montgomery-format
  Elligator2 exists in the dalek ecosystem; the capsule remains sealed inside the wrapped
  slot. Corruption-matrix tests (garbage capsule, wrong-key fast-no, bit-flip, wrong
  offset, wire truncation) run in-tree. Allocation hygiene is by construction today:
  capability tokens are fixed arrays behind zeroize, passphrases are callback-scoped
  `Zeroizing<String>`, and calibrated parameters are plain records; the heap-proof
  counting-allocator pass is scheduled for Block 9.

- **2026-11-16** — Block 1 ground laid: workspace unified (the v5 fork-exile failure closed),
  gates machinery in `holonomy-ledger`, this protocol instituted, RFC-001…004 filed as
  attack targets, UI spec filed *before* UI code (the H1 inversion corrected).
- **2026-11-16** — Block 1 gates **passed** (CI all green). Process notes recorded for later
  blocks: (a) counting-allocator proofs must live in binaries that *install* the allocator —
  the first unit-test version measured nothing and CI caught it; the same honesty rule
  applies to every future gate; (b) the diagnostics bridge (issue #51) remains in CI,
  dormant unless a step fails.
