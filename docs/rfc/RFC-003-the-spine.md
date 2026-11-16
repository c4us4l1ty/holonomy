# RFC-003 — The Spine (Block 4)

*Status: open attack target. File attack cases in `AUDIT.md`.*

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
4. **Fenwick over slab heights**: scroll position ⇄ content position in O(log segments);
   usable before the target slabs are even resident (skeleton layout — H1's `fault_residency`
   lineage).
5. **Keystroke path**: insert/delete within a slab is an in-place edit of the resident
   plaintext + leaf metadata; cross-slab edits split/merge path-copied nodes. The counting
   allocator (holonomy-ledger) gates it: zero allocations within-slab, counted otherwise.
   Document-length independence is the measured invariant (L-4 in Block 6 consumes this).

## Attacks resisted

| Adversary / failure | Mechanism | Control | Proving test |
| ------------------- | --------- | ------- | ------------ |
| A2 memory mass | resident plaintext grows with doc | fixed residency scheduler | L-2 gate: 1-page vs 30M-word windows measure equal residency |
| (correctness) latency drift | cost grows with offset/size | tree + Fenwick | L-4 harness at offsets {0, N/2, N-1} |
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
