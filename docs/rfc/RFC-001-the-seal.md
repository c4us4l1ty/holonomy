# RFC-001 — The Seal (Block 2)

*Status: **closed 2026-11-17 by amendment**, not by proving the original wording. The zeroization
claim is narrowed to project-owned buffers and its dependency-side remainder is named as a residual;
the calibration mechanism ships, reference-hardware numbers are owed to Block 6/10 budget
certification. See `AUDIT.md`, "gate-closure amendment".*

## Context

v5.1.0's envelope is the strongest thing either generation built: two-phase, non-circular,
proven so by a test that unlocks a deliberately garbage capsule from a known passphrase
(`two_phase.rs`). Its Argon2id → `K_unwrap` → (ML-KEM-1024 × X25519) → `K_DEM` schedule and
CMT-4 key-committing AEAD are inherited intact in shape. What RFC-001 changes is the *role*:
v5 hid the whole design behind a mandatory USB pad; v6's threat model makes the passphrase
the sole required factor (L-9) and demotes USB to an optional hardener.

## Decision

1. **One crate boundary:** `holonomy-seal` is the only crate that links crypto primitives.
   Every other crate receives keys as opaque capability tokens (`DEMKey`, `UnwrapKey`) whose
   types enforce flow at compile time: a token can be *used* to request an operation from the
   seal, never read as bytes outside it. The KEM trait, its concrete implementation, and raw
   `SharedSecrets` stay crate-private; exporting that testable plumbing would expose raw shared
   key material.
2. **Envelope (inherited):** passphrase → Argon2id (memory-hard, host-calibrated cost,
   recorded ledger-style) → `K_unwrap` → phase-1 unwrap of the per-save hybrid decapsulation
   key → ML-KEM-1024 + X25519 decapsulation → `K_DEM`. Key-committing AEAD (CMT-4) per sealed
   object; project-owned secret buffers have explicit zeroizing guards, and available dependency
   wipe-on-drop features are enabled where reviewed. Dependency-internal scratch is not covered
   by that statement; its current evidence and known gaps are recorded below and in `AUDIT.md`.
   OS page protections are reported honestly: Unix `mlock` / Windows `VirtualLock`, plus Linux
   `MADV_DONTDUMP` where accepted. `memfd_secret` availability is probed on supported Linux
   architectures, but key cells are not backed by it; there is no process-wide core-dump disable
   or `DONTFORK` claim.
3. **USB, truly optional:** the KDF info string carries a domain-separated *optional* USB
   secret slot; absent → passphrase-only derivation passes the same code path with the slot
   inactive. No boolean fork of security logic; one path, one code review.
4. **Rejected from the lineage:** strata/decoy (deferred past 6.0), the pad-fold in the save
   path (moved out of the core), every v5.0-OMEGA theatricality (they remain on AUDIT.md's
   wall of killed claims).

## Attacks resisted

| Adversary (TM §2) | Mechanism | Control | Proving test |
| ----------------- | --------- | ------- | ------------ |
| A1 harvest-later | PQ break of classical ECDH | ML-KEM-1024 hybrid; classical break alone insufficient | envelope round-trip + known-answer vectors |
| A5 offline brute | KDF cost | memory-hard Argon2id, calibrated per host, params recorded | calibration bounds test (H1's measured-correction pattern) |
| A2 key lifetime | keys dumping with process memory | best-effort zeroization and the reported OS memory-lock rung; Linux `MADV_DONTDUMP` only where accepted; opaque capabilities prevent API copy-out | registered-allocation wipe test; Linux dump-region map check; platform caveats |
| A1/A4 wrong-capsule oracles | decrypt-with-garbage distinguishers | coarse errors only; phase commitments (CMT-4) checked before use | garbage-capsule unlock test (inherited from v5 `two_phase.rs` pattern) |
| A4 rollback: old keys presented for new file | envelope/witness mismatch | envelope commits to witness root at seal time (RFC-002 handshake) | cross-file rejection test |

## Rejected alternatives

- **v3 VDF (Wesolowski squaring in the unlock path):** beautiful time-delay primitive, but it
  serializes unlock against *both* attacker and user identically and cost H1 a measured
  calibration subsystem for marginal gain over a well-parametrized Argon2id. Deferred as an
  optional "time-lock" research item, not a dependency.
- **Passphrase-normalization lowercasing** (v5 legacy mapping): silently weakens entropy;
  rejected. v6 uses exact bytes + NFC normalization, documented.
- **Any custom primitive:** the seal composes audited constructions only.

## Closed by amendment (2026-11-17)

- **Argon2id calibration target.** The RFC commits to the *mechanism*, not a guessed constant: the
  host-calibrated selector picks and records a schedule inside a fenced band, replay refuses
  below-floor or unbounded records before Argon2 runs, and a byte-equality test pins the
  caller-owned arena against argon2's own allocator. What is still missing is a published number on
  the *weakest supported reference laptop*; that measurement belongs to the Block 6/10 budget
  certification, not to the seal's design. H1's 57× miss (K-2) was an unmeasured constant — this
  design contains no unmeasured constant to miss.
- **Dependency-temporary zeroization: amended, with its remainder still an open claim.** The test-only
  allocator checks only project-owned ranges explicitly registered at their owners; counting total
  allocations does not show that unregistered dependency buffers are erased. Static review of the
  locked source set found that `unicode-normalization` 0.1.26 buffers long combining runs in
  non-zeroizing TinyVecs, so passphrase input is capped at 4,096 raw UTF-8 bytes before NFC. That
  bounds the input driving scratch growth but neither wipes it nor covers the caller's borrowed
  source text. Direct `sha3`/`shake` zeroize edges (and the Poly1305 edge) enable selected
  state-drop hooks, but reviewed Argon2/BLAKE2 and ML-KEM stack intermediates, plus XChaCha's
  HChaCha state/subkey copy, remain unzeroized or unproven. See the pinned-source findings and exact
  limits in `AUDIT.md`.

  **What the closed gate claims:** every *project-owned* secret buffer is zero before free — the
  audit fails the build on a nonzero registered buffer, a reallocation of a secret, or a registry
  overflow — and the reviewed wipe features are enabled and CI-enforced. **What it does not
  claim:** that dependency internals are erased, at any level below that. Any future entry that
  treats this as proven by the registered-buffer test, the input bound, or feature activation is
  invalid; the residual lives in `docs/THREAT-MODEL.md` §5 and allocator-wide proof is future
  hardening.
