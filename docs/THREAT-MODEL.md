# HOLONOMY v6 — THREAT MODEL
*Status: Block 1 working canon. Versioned with the repo; changes require an AUDIT.md entry.
Amended 2026-11-17 — see AUDIT.md, "gate-closure amendment" (zeroization scope, L-6 deferral,
scroll timing).*

This is the legal system the rest of v6 is built under. It says what we protect, against
whom, at which moments — and, just as importantly, what we **refuse to pretend** to protect.
A protection that cannot name its attack is decorative, and the v5.0-OMEGA audit showed what
decorative costs.

## 1. The asset

1. **Document plaintext, at rest** — the `.wavefunction` file, in cloud copies, on any disk.
2. **Document plaintext, in use** — the resident plaintext slabs while editing.
3. **Passphrase and derived key material** — `K_unwrap`, `K_DEM`, slab keys.
4. **Editing metadata with causal value** — undo history, write times, file growth shape.
5. **History** — the world-line is the asset. Protecting "the document" without protecting
   every draft is protection theater.

## 2. The adversaries

| ID | Adversary | What they can do | In scope? |
| -- | --------- | ---------------- | --------- |
| A1 | **Archive & wait** | Copy `.wavefunction` today (disk, cloud breach, interception); attack it later, including with quantum hardware (harvest-now-decrypt-later) | **Yes** — the core case |
| A2 | **Live-memory intruder** | Read RAM while the editor runs (post-exploit, forensic live capture, cold boot) | **Yes** — bounding this is the singularity |
| A3 | **OS-level observer** | Read clipboard, swap, crash dumps, page cache, thumbnails, IME/AT text events | **Yes, partially** — mitigations per platform; see §5 |
| A4 | **Cloud integrity attacker** | Truncate, reorder, or flip bytes in the synced copy | **Partially** — malformed edits, non-prefix truncation, reorder, and flips are detected; a byte-perfect rollback to an earlier valid witness prefix is not detectable from the file alone (AUDIT OQ-7) |
| A5 | **Online brute forcer** | Unlimited offline KDF attempts against a stolen file | **Yes** — cost is the defense (memory-hard KDF + PQ envelope), not secrecy of the format |
| A6 | **Physical observer / hardware implant** | Cameras reading the screen, keyloggers below the OS input API, compromised firmware | **No** — stated openly, not pretended away |

## 3. Boundaries of trust

v6 trusts, in order of decreasing trust:

1. **The Rust process itself** — and inside it, only `holonomy-seal` holds key material,
   enforced by capability tokens (RFC-001) so the compiler polices the inner boundary.
2. **The OS kernel** — memory isolation and the platform memory-protection APIs we request.
   Key cells attempt `mlock` on Unix or `VirtualLock` on Windows; Linux additionally attempts
   page-aligned `MADV_DONTDUMP` after a successful `mlock`. `memfd_secret` availability is
   probed where this build supports the syscall, but key cells are not backed by it. The reported
   rung can be lower when the OS denies a request. This code does not disable process-wide core
   dumps or request `DONTFORK`,
   and zeroization is a best-effort process action, not a kernel secure-erase guarantee.
3. **The GPU/compositor (GPUI pipeline)** — pixels of the page being viewed are, at paint
   time, observable by definition. We bound this by law L-2: it is the viewport only.

**Explicitly distrusted:** disk state (sealed), cloud copies (sealed + verified), the
clipboard (egress door — flagged confidential, cleared on blur), and crash infrastructure.
On Linux, successfully locked key pages are marked `MADV_DONTDUMP` when the kernel accepts
it; this is not a process-wide core-dump disable and is not implemented as `DONTFORK`. Other
platforms may only reach the resident-memory rung. The Security panel must report the actual
per-cell rung, not a global claim.

## 4. Invariant moments

The threat model bites hardest at specific instants; each has a designated behavior:

- **Open:** plaintext delta is the viewport neighborhood (L-2), never the file (RFC-002).
- **Scroll/edit:** slabs wipe as they exit residency; keystrokes allocate zero (L-2).
- **Save:** append sealed segments and sync a witness commit point. The ordering is exercised by
  injected short writes at every byte, both sync barriers, truncate failure, and real ENOSPC on an
  ext4 loop image. Actual power-loss/device-cache durability remains unqualified and is owned by the
  Block 6 kill-power suite and the Block 10 release gate (L-6 is still `spec`).
- **Blur/lock:** plaintext pages shed residency and wipe; clipboard cleared (Block 9).
- **Suspend/quit/tripwire:** L-7 verified wipe — wipe set is the (bounded) resident set, so
  the v5 problem of a 180 MiB wipe exceeding its budget is deleted by construction.
- **Copy to a new machine:** the whole security decision travels as *one passphrase* — A1's
  file reveals nothing without it (L-9).

## 5. Honest residual list (what we do NOT claim)

Recorded so no future version quietly upgrades these into claims:

- A live kernel-level adversary can in principle read the viewport's painted plaintext. We
  shrink it to the screen, we do not pretend it is zero.
- A3 mitigations vary by OS; the Security panel reports the actual ladder rung, not the wish.
- Timing side channels on the KDF (wrong-passphrase latency) are bounded to a fixed cost
  profile; nothing beyond that uniformity is claimed.
- **Undo history doubles as exposure history**: the world-line keeps old drafts by design
  (Vision III). Deleting text and saving commits a sub-segment spine manifest (`RECORD_MANIFEST`)
  so deleted text stays deleted on reopen (AUDIT OQ-29, OQ-30), while the old ciphertext remains
  in the append-only `.wavefunction` until explicit compaction (`holonomy compact <src> <dst>`,
  gated by passphrase re-confirmation via `confirm_compaction`) rewrites only the active spine
  into a fresh, re-keyed world-line (RFC-002 §7, AUDIT OQ-14). Cloud mirrors may still retain
  copies of the pre-compaction file.
- A byte-perfect rollback to an earlier valid witness prefix cannot be distinguished from a
  legitimately shorter file without trusted monotonic state outside the copied `.wavefunction`.
  v6 claims integrity of the bytes present and crash consistency, not freshness against this
  rollback; see RFC-002 and AUDIT OQ-7.
- **Dependency-internal scratch is not claimed erased.** Project-owned secret buffers are zeroized
  under the registered-allocation audit, and the reviewed crates' wipe features are enabled and
  CI-enforced. Stack and heap temporaries *inside* those dependencies — Argon2/BLAKE2 intermediates,
  ML-KEM stack state, XChaCha's HChaCha state/subkey copy, and `unicode-normalization`'s non-wiping
  `TinyVec` buffers for long combining runs — are bounded where possible (4,096-byte passphrase cap
  before NFC) and recorded in AUDIT.md, but not proven wiped. No allocator-wide, register, or
  physical-memory erasure is claimed anywhere.
- **Physical power loss is an obligation, not a result.** Crash consistency is evidenced under a
  software failure model plus one ext4 loop-filesystem ENOSPC rehearsal. A controlled power cut,
  controller-cache evaluation, and a supported-device matrix have not been run; they are owed to the
  Block 6 kill-power gate and the Block 10 release gate, and L-6 stays `spec` until then.
- **Which pages you read is observable.** Slab fault-in timing is a side channel, accepted rather
  than smoothed (AUDIT OQ-4): the same adversary can see the ciphertext reads at known file offsets,
  and smoothing would cost idle work proportional to traversal. This is a disclosed property of the
  residency model, not a defended one.

## 6. Registry: law → adversary

| Law | Serves adversary case |
| --- | --------------------- |
| L-1, L-6, L-10 | A4 (integrity), A1 (sealed growth) |
| L-2 | A2, A3 |
| L-7 | A2 |
| L-8 | (correctness law — integrity of intent, not confidentiality) |
| L-9 | A1 + usability: security that requires hardware excludes most users |
