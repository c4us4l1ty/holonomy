# RFC-002 — The Witness (Block 3)

*Status: open attack target. File attack cases in `AUDIT.md`.*

## Context

v5's container was honest and audited — and hard-capped at 2,046 payload slots ≈ 134 MiB
(verified in `layout.rs`: `PAYLOAD_SLOTS = TOTAL_BLOCKS - 2`). H1's container was chunked and
quest-capable but its nonce design only survived because an implementer noticed the
reused-nonce-on-resave trap (K-1 on the wall). Neither can hold a sixty-year document. The
v6 answer is to stop treating the file as a *container of the text* and make it a *world-line
of sealed slabs*.

## Decision

1. **Segment** ≈ 64 KiB quantum (the O_DIRECT-friendly unit both ancestors converged on),
   sealed independently with CMT-4, carrying a predecessor commitment (H1's chain trick —
   `nonce/ciphertext binds the previous tag` — applied at segment scope, so splice/reorder is
   cryptographically expensive).
2. **Witness**: every save appends a small sealed **witness slab** committing to the hash of
   the current segment-set root (a compact Merkle root over segment headers). The chain of
   witnesses is the truth about order and completeness; segment bodies may be present or
   compacted, but a valid witness chain is required to *open*.
3. **Append-only growth**: a save = append changed slabs + witness. Nothing overwrites. Cold
   storage after N appends transfers the appended tail only (L-10).
4. **Atomic commit**: stage → segment writes → file sync → witness write → directory fsync.
   Power loss anywhere lands on the previous witness (L-6). Inheritance: v5.1.0's tested
   9-step ordering, minus pad fold, plus witness-as-commit-point.
5. **Open**: read witness chain tail (bounded), verify chain, hand segment descriptors to the
   Spine. The document *body* is never read at open (L-3's cost model: witness + hot slab
   metadata only).
6. **Envelope↔line handshake**: witness root commits to the current envelope epoch (RFC-001),
   closing the cross-file rollback case.
7. **Compaction**: explicit, passphrase-confirmed, writes a *new* file and re-bases the
   witness chain. Never automatic. AUDIT OQ-2 records the residual (cloud mirrors keep
   pre-compaction tails — a documented property, not a hidden one).

## Attacks resisted

| Adversary | Mechanism | Control | Proving test |
| --------- | --------- | ------- | ------------ |
| A4 truncation | cut tail, revert to older world-line | witness chain is sealed & ordered; truncation = detectable gap | truncate-at-N reopen test |
| A4 bit-flip | flip history bytes | per-segment AEAD + chain | flip-every-byte fuzz: verify reports, open refuses |
| A4 splice/reorder | rearrange segments across files | predecessor commitments + per-save window identity | crossfile splice rejection |
| — torn write (crash) | partial sector group at tail | commit-point order above; reopen at last witness | crash-injection matrix at every append boundary |
| A2 open-time exposure | bulk read at open | body never touched at open | L-2/L-3 harness asserts slab read-set |

## Rejected alternatives

- **Fixed padded container, grown larger** (multi-GiB v5 successor): wastes disk, complicates
  sync, still caps a lifetime. Killed in planning (K-10).
- **Delta compression / content-defined chunking** in v6.0: real space wins across decades,
  but interacts with compaction and A4-reordering analysis; deferred past 6.0 with the seam
  (segment size is a header field) preserved.
- **Directory-of-segments**: best incremental-sync story, worst drag-and-drop story; one
  file wins because L-9's portability is constitutional. Sync efficiency is achieved by
  appended tails instead.

## Open questions

- Segment-size ladder (64 KiB now, larger for media later): needs Eye demand data (Block 5
  tells us real hot-set sizes).
