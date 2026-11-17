# RFC-003 — The Spine (Block 4)

*Status: **four pieces implemented 2026-11-17** (`holonomy-text`: spine, aggregates, history,
resident set; the measured keystroke path; the commit boundary — seal → append → witness → new
root, proven against a real `holonomy-line` world-line in `tests/commit_boundary.rs`; and the
live-length overlay that keeps position queries correct while the spine is stale), still an open
attack target for the rest. No editor exists above them — caret, selection and paint are Block 5 —
and every Block 4 gate is a document-scale measurement that has not been run. Two deliverables are
amended: §4 (AUDIT decision **D-1**) and §5 (AUDIT decisions **D-2**, **D-3**).*

## Context

H1 built a genuinely beautiful text engine — faulting gap-rope leaves with a *bounded
resident set*, a *measured* zero-allocation keystroke path, a Fenwick over line heights —
and then buried it under a UI that screamed. The failure list is instructive: page breaks by
newline padding (K-9), a caret grid disconnected from paint (K-8), style undo divorced from
text undo. v6's Spine takes H1's engine lessons and bolts them to the Witness, so the engine
is born living inside the security model instead of being wrapped by it later.

## Decision

1. **Immutable slab tree**: a rope-shaped Merkle spine over segment IDs. Interior nodes hold
   aggregate lengths/heights; leaves reference sealed segments on the world-line. An edit
   path-copies O(log N) nodes; old spines stay valid forever.
2. **Undo = pointer walk**: undo/redo and history navigation are root-pointer swaps. **No
   undo buffer may hold plaintext.** This is not convenience; it is security geometry: the
   wipe set (L-7) stays O(view) precisely because history costs pointers, not memory.
3. **Residency scheduler**: plaintext slabs fault in under the viewport plus a small margin;
   fault out on LRU beyond the budget; wipe-on-exit via `holonomy-seal` zeroizing pages. The
   budget is a constant set at startup from the viewport, **never** derived from document
   length (L-2).
4. **Prefix sums over subtree aggregates** (amended 2026-11-17, AUDIT decision D-1): scroll
   position ⇄ content position in O(log segments) by descending each node's cached bytes,
   slab count and height; usable before the target slabs are resident (skeleton layout — H1's
   `fault_residency` lineage). The original text named a separate flat Fenwick over slab
   heights. That structure would have to be rebuilt alongside every path copy and could drift
   from the tree it indexes — the K-8 two-geometry disease in index form — while computing the
   same prefix sums. One index, held by the tree, is the amendment.
5. **Keystroke path**: insert/delete within a slab is an in-place edit of the resident
   plaintext + leaf metadata; cross-slab edits split/merge path-copied nodes. The counting
   allocator (holonomy-ledger) gates it: zero allocations within-slab, counted otherwise.
   Document-length independence is the measured invariant (L-4 in Block 6 consumes this).
   **Amended 2026-11-17 (AUDIT D-2, D-3):** leaf metadata is *not* rewritten per keystroke. With
   an immutable path-copied tree that would allocate O(log N) nodes per character and put the L-2
   gate out of reach by construction, so the resident slab is authoritative while it is edited and
   the spine is path-copied at the **commit boundary**: the edited slab is sealed as a new segment,
   the world-line commits it with a witness, and the spine's leaf is replaced by one naming the new
   segment. One structural event per burst of typing, never one per character. The stale geometry
   is bridged in memory by `Draft`, which answers lengths, offsets and ranges against the
   committed tree plus the live lengths of the leaves being edited, so the document view is correct
   before the seal and byte-identical after it.

## Attacks resisted

| Adversary / failure | Mechanism | Control | Proving test |
| ------------------- | --------- | ------- | ------------ |
| A2 memory mass | resident plaintext grows with doc | fixed residency scheduler | L-2 gate: 1-page vs 30M-word windows measure equal residency |
| (correctness) latency drift | cost grows with offset/size | balanced tree with subtree aggregates | L-4 harness at offsets {0, N/2, N-1} (unbuilt) |
| (correctness) edit at think-speed | allocator stalls in the hot path | zero-alloc claim within slab | counting-allocator gate (ledger) |
| A2 undo leakage | old plaintext lingering for history | undo holds pointers only | resident-set audit test: history deep → resident small |
| A4/A1 history integrity | segment references point to real slabs | Merkle leaves; witness covers the set | corrupt-segment detection on open |

## Rejected alternatives

- **Piece table / gap buffer**: excellent for one big mutable text; both keep the whole text
  resident or require bulk IO — incompatible with L-2.
- **CRDT bases (yrs lineage)**: v4-era audits recorded heap-leak pain and collaboration infra
  that v6's single-user constitution doesn't need. Deferred asInterop research only.
- **Per-line immutable tree (novelty design)**: shape cost per line dominates at 30M words;
  slab grain (≈ 2 pages) is the measured sweet spot inherited from H1's leaf sizing.

## Open questions

- Style runs layering: styles as sibling overlay tree vs. slab-inline runs — Block 5's
  geometry contract decides; documented seam, not a guess.
- AUDIT OQ-4 (fault-timing leakage) is decided here before the Block 4 gate closes.
