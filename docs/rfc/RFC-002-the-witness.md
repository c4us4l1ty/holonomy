# RFC-002 — The Witness (Block 3)

*Status: **Block 3 closed 2026-11-17 under scope amendment** (AUDIT.md, "gate-closure amendment").
Tests inject a short write at every append byte, fail each sync barrier, and fail tail-recovery
truncation; they trace `File::write` offsets and byte counts on a simulated 30 M-word file, and the
ext4 ENOSPC rehearsal runs in CI, and a named rsync client is measured and bound-checked by a CI
job on every push. The format freezes the write ordering; **physical power-loss qualification is not claimed and
is owed to the Block 6 kill-power gate and the Block 10 release gate**, where L-6 and L-10 stay
`spec`. The valid-prefix rollback limit is explicitly amended below and remains open as AUDIT OQ-7.*

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
   committed bytes. An incomplete or uncommitted tail is reported on open and discarded before
   the next append. Tests now trace the appender's `File::write` calls, confirming offsets start
   at the prior EOF and write bytes match the appended frames, even on a simulated 30 M-word
   history. This proves the local writer is tail-only. A concrete sync path has since been measured
   — rsync 3.2.7 against a local daemon, asserting literal data ≤ appended bytes + one block and
   application-layer bytes ≤ 8× appended — which is the L-10 measurement for *that client only*, not
   for backup clients or remote services in general.
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
   segment ciphertext bodies and superseded spine manifests, opening only the active manifest
   referenced by the final witness when one exists. Verify the witness chain and MMR roots, then
   hand active `LineSlab` spans to the Spine. Opening never bulk-reads or decrypts segment bodies
   (L-3's cost model). The current implementation scans all record metadata and witness frames;
   its work is O(number of records + active manifest slabs), not O(document plaintext bytes).
6. **Envelope↔line handshake**: every witness authenticates a fingerprint of the complete
   header epoch (file ID, KDF record, salt, and capsule). A capsule replayed across files or
   epochs cannot open the existing witness chain (RFC-001 A4 rollback case).
7. **Compaction**: explicit only (`WorldLine::compact_to`, `WorldLine::compact_active_to`, and CLI
   `holonomy compact <src> <dst>`). The caller supplies a new header/key pair and consumes a
   file-scoped approval minted by `holonomy-seal` only after re-entering and re-deriving the
   passphrase. Compaction writes and verifies a same-directory staging file (keeping at most one
   source segment's plaintext resident at a time), then publishes it with a no-replace link and
   directory sync; failure leaves the source untouched and cleans the staging path best-effort.
   Creation and compaction tests assert that a pre-existing destination is preserved. It re-bases
   the new witness chain and never runs automatically. Cloud mirrors may still retain the
   pre-compaction file and its history; users must prune those copies separately (AUDIT OQ-2).

## Wire shape (versions 1 and 2)

- The fixed header carries magic/version, a random 128-bit file ID, the fenced KDF record, a
  64-byte salt, and the fixed-size capsule. The KDF record is rejected before Argon2 unless
  memory is within the 64–256 MiB fence, rounded to a 4 MiB step, and time/lanes match the
  calibrated v1 schedule. `ParamsClass::Test` is not a valid production header.
- Every record has a bounded frame header and one of three types: sealed segment (`1`), sealed
  witness (`2`), or sealed spine manifest (`3`, `RECORD_MANIFEST`). Segment ciphertext is at most
  64 KiB plus the AEAD tag. An append-only world-line writes version-1 witnesses (MMR peaks only,
  zero manifest overhead). Once a structural edit (mid-segment split, insertion before the end, or
  deletion across sealed text) is committed, version-2 witnesses append a 44-byte `ManifestRef`
  (`sequence`, `ciphertext_len`, `tag`) naming the active `RECORD_MANIFEST` of `(segment, start,
  len)` spans — or zero spans when the entire document has been deleted (`OQ-29`, `OQ-30`).
- Once a manifest is active on a world-line, every subsequent witness must carry a `ManifestRef`
  so an unreferenced version-1 witness cannot strip a prior cut or deletion.
- The segment MMR leaf commits to file/epoch IDs, sequence, nonce, ciphertext length,
  predecessor tag, and CMT-4 tag. CMT-4 then authenticates the ciphertext when that segment is
  faulted in or during full `verify` (which also authenticates historical manifests against the
  segment bounds at their commit points).
- Mutating storage currently requires Unix directory-sync and advisory-lock primitives. Other
  targets return `UnsupportedDurability` for creation/appends instead of claiming L-6 without
  an implemented durable commit path.

## Attacks resisted

| Adversary | Mechanism | Control | Proving test |
| --------- | --------- | ------- | ------------ |
| A4 torn append / partial tail | short write within a segment or witness frame before its commit point | last complete authenticated witness remains authoritative; `RecoveryInfo` reports trailing bytes | synthetic every-byte cut matrix plus injected short-write failure after each append byte; reopen verifies the prior commit |
| A4 segment bit-flip | flip committed ciphertext or record metadata | witness MMR binds record metadata; per-segment CMT-4 binds the body | metadata-only open skips body; full `verify` rejects a flipped old segment; metadata/tag changes reject the witness root |
| A4 splice/reorder | rearrange segments across files or epochs | file ID, epoch, sequence, predecessor tag, and CMT-4 are bound into the segment chain | capsule-replay-across-files open-refusal test; sequence/root checks reject reorder |
| A4 valid-prefix rollback | truncate exactly to an earlier complete witness | **not detectable from the file alone** without an external monotonic anchor; no local test may claim otherwise | regression test documents the indistinguishable valid-prefix case; residual is AUDIT OQ-7 |
| — append I/O failure | partial write, failure of either sync barrier, or failure truncating a recovered tail | append errors poison the handle; open selects only the last complete authenticated witness; final-sync failure is reported as uncertain | byte-by-byte injected short writes, both sync failpoints, recovery-truncate failpoints, and reopen/verify assertions |
| — physical power loss | loss of power at arbitrary file/device-cache boundaries | required durability ordering is implemented, but physical persistence behavior depends on filesystem and hardware | not tested; no power-cut/VM snapshot or platform filesystem qualification has been run, so L-6 remains `spec`. Qualification is assigned to the Block 6 kill-power gate and Block 10 (AUDIT 2026-11-17) |
| A2 open-time exposure | bulk read at open | segment ciphertext is skipped by seek; only public metadata and bounded witness bodies are read | guarded reader test fails if open reads any segment body |

## Offline rollback limit (amendment)

The original “truncation = detectable gap” claim was too broad. A file-local authenticated hash
chain can detect malformed, reordered, or partial tails, but a byte-perfect prefix ending at an
earlier valid witness is indistinguishable from a file that legitimately stopped there. Detecting
that rollback requires trusted state outside the copied file (for example, a remote transparency
anchor); v6 has no such mandatory service because passphrase portability is constitutional.
Accordingly, the format claims integrity of the bytes present, not freshness against
whole-prefix rollback. Injected I/O failures exercise the append recovery logic, but platform
power-loss durability remains unqualified. The rollback limit remains an explicit A4 residual,
not a hidden property.

## Rejected alternatives

- **Fixed padded container, grown larger** (multi-GiB v5 successor): wastes disk, complicates
  sync, still caps a lifetime. Killed in planning (K-10).
- **Delta compression / content-defined chunking** in v6.0: real space wins across decades,
  but interacts with compaction and A4-reordering analysis; deferred past 6.0. Version 1 fixes
  the segment quantum at 64 KiB; changing it requires a format-version change, with an explicit
  segment-size field reserved for a later version.
- **Directory-of-segments**: best incremental-sync story, worst drag-and-drop story; one
  file wins because L-9's portability is constitutional. Appending preserves the old file prefix;
  end-to-end sync efficiency is measured for one named client (rsync) and remains unmeasured for
  every other client and remote backend.
