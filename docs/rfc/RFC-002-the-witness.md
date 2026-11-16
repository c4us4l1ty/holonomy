# RFC-002 — The Witness (Block 3)

*Status: Block 3 implementation and test draft; local Rust validation is pending because the
pinned toolchain is not installed in the current sandbox. The valid-prefix rollback limit is
explicitly amended below and remains open as AUDIT OQ-7.*

## Context

v5's container was honest and audited — and hard-capped at 2,046 payload slots ≈ 134 MiB
(verified in `layout.rs`: `PAYLOAD_SLOTS = TOTAL_BLOCKS - 2`). H1's container was chunked and
quest-capable but its nonce design only survived because an implementer noticed the
reused-nonce-on-resave trap (K-1 on the wall). Neither can hold a sixty-year document. The
v6 answer is to stop treating the file as a *container of the text* and make it a *world-line
of sealed slabs*.

## Decision

1. **Segment** ≈ 64 KiB quantum (the O_DIRECT-friendly unit both ancestors converged on),
   sealed independently with CMT-4. Segment associated data binds the random file identifier,
   complete envelope epoch, global segment sequence, plaintext length, and predecessor
   CMT-4 tag. Reordering or relocating a record therefore fails authentication.
2. **Witness**: every save appends a small sealed witness committing to an append-only
   Merkle-mountain-range root over public segment-record metadata. The witness chain binds the
   previous witness tag, cumulative segment count, last segment tag, and current envelope
   epoch. A witness is the only commit point; a complete segment frame without a following
   witness is uncommitted.
3. **Append-only growth**: a save appends changed slabs plus one witness. Nothing overwrites
   committed bytes. A crashed, uncommitted physical tail is reported on open and discarded
   before the next append. Cold storage after N appends transfers the appended tail only
   (L-10).
4. **Atomic commit**: for an existing file: segment writes → file sync → witness write → file
   sync. The witness is the commit point. Initial creation writes a complete staged inode,
   syncs it, links it into place without replacing an existing path, then syncs the containing
   directory. An append does not change the directory entry, so file sync is the durability
   boundary for that operation. Open takes a shared per-inode advisory lock so it cannot observe
   an in-flight local append. Appends take an exclusive lock; stale writers fail instead of
   writing a duplicate sequence. A lock-contention test checks that a shared open blocks an
   exclusive append until the scan lock is released. Before writing, an append re-scans the
   authenticated header, record metadata, and witness chain (but still seeks over segment bodies)
   so same-length metadata edits cannot turn a save into a newly unreadable log.
5. **Open**: read the fixed header, segment metadata, and authenticated witnesses; seek over
   segment ciphertext bodies. Verify the witness chain and MMR roots, then hand segment
   descriptors to the Spine. Opening never bulk-reads or decrypts segment bodies (L-3's cost
   model). The current implementation scans all record metadata and witness frames; its work
   is O(number of records), not O(document plaintext bytes).
6. **Envelope↔line handshake**: every witness authenticates a fingerprint of the complete
   header epoch (file ID, KDF record, salt, and capsule). A capsule replayed across files or
   epochs cannot open the existing witness chain (RFC-001 A4 rollback case).
7. **Compaction**: explicit only. The caller selects the history to retain, supplies a new
   header/key pair, and consumes a file-scoped approval minted by `holonomy-seal` only after
   re-entering and re-deriving the passphrase. Compaction writes and verifies a same-directory
   staging file, then publishes it with a no-replace link and directory sync; failure leaves the
   source untouched and cleans the staging path best-effort. Creation and compaction tests assert
   that a pre-existing destination is preserved. It re-bases the new witness chain and never runs
   automatically. Cloud mirrors may still retain the pre-compaction file and its history; users
   must prune those copies separately (AUDIT OQ-2).

## Wire shape (version 1)

- The fixed header carries magic/version, a random 128-bit file ID, the fenced KDF record, a
  64-byte salt, and the fixed-size capsule. The KDF record is rejected before Argon2 unless
  memory is within the 64–256 MiB fence, rounded to a 4 MiB step, and time/lanes match the
  calibrated v1 schedule. `ParamsClass::Test` is not a valid production header.
- Every record has a bounded frame header and one of two types: sealed segment or sealed
  witness. Segment ciphertext is at most 64 KiB plus the AEAD tag. Witness plaintext stores
  the MMR peaks, allowing the root and count to be recomputed without a growing witness body.
- The segment MMR leaf commits to file/epoch IDs, sequence, nonce, ciphertext length,
  predecessor tag, and CMT-4 tag. CMT-4 then authenticates the ciphertext when that segment is
  faulted in or during full `verify`.
- Mutating storage currently requires Unix directory-sync and advisory-lock primitives. Other
  targets return `UnsupportedDurability` for creation/appends instead of claiming L-6 without
  an implemented durable commit path.

## Attacks resisted

| Adversary | Mechanism | Control | Proving test |
| --------- | --------- | ------- | ------------ |
| A4 torn append / partial tail | cut a segment or witness frame before its commit point | last complete authenticated witness remains authoritative; `RecoveryInfo` reports trailing bytes | every byte-prefix cut of a multi-segment append reopens at the prior commit; a subsequent append removes only the uncommitted tail |
| A4 segment bit-flip | flip committed ciphertext or record metadata | witness MMR binds record metadata; per-segment CMT-4 binds the body | metadata-only open skips body; full `verify` rejects a flipped old segment; metadata/tag changes reject the witness root |
| A4 splice/reorder | rearrange segments across files or epochs | file ID, epoch, sequence, predecessor tag, and CMT-4 are bound into the segment chain | capsule-replay-across-files open-refusal test; sequence/root checks reject reorder |
| A4 valid-prefix rollback | truncate exactly to an earlier complete witness | **not detectable from the file alone** without an external monotonic anchor; no local test may claim otherwise | regression test documents the indistinguishable valid-prefix case; residual is AUDIT OQ-7 |
| — torn write (crash) | power loss at append boundaries | segment batch sync precedes the witness; incomplete tail is never a commit | every-byte cut matrix plus torn-tail recovery test |
| A2 open-time exposure | bulk read at open | segment ciphertext is skipped by seek; only public metadata and bounded witness bodies are read | guarded reader test fails if open reads any segment body |

## Offline rollback limit (amendment)

The original “truncation = detectable gap” claim was too broad. A file-local authenticated hash
chain can detect malformed, reordered, or partial tails, but a byte-perfect prefix ending at an
earlier valid witness is indistinguishable from a file that legitimately stopped there. Detecting
that rollback requires trusted state outside the copied file (for example, a remote transparency
anchor); v6 has no such mandatory service because passphrase portability is constitutional.
Accordingly, the implementation claims crash consistency and integrity of the bytes present,
not freshness against whole-prefix rollback. This remains an explicit A4 residual, not a hidden
property.

## Rejected alternatives

- **Fixed padded container, grown larger** (multi-GiB v5 successor): wastes disk, complicates
  sync, still caps a lifetime. Killed in planning (K-10).
- **Delta compression / content-defined chunking** in v6.0: real space wins across decades,
  but interacts with compaction and A4-reordering analysis; deferred past 6.0. Version 1 fixes
  the segment quantum at 64 KiB; changing it requires a format-version change, with an explicit
  segment-size field reserved for a later version.
- **Directory-of-segments**: best incremental-sync story, worst drag-and-drop story; one
  file wins because L-9's portability is constitutional. Sync efficiency is achieved by
  appended tails instead.
