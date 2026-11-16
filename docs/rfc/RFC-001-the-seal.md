# RFC-001 — The Seal (Block 2)

*Status: open attack target. File attack cases in `AUDIT.md`.*

## Context

v5.1.0's envelope is the strongest thing either generation built: two-phase, non-circular,
proven so by a test that unlocks a deliberately garbage capsule from a known passphrase
(`two_phase.rs`). Its Argon2id → `K_unwrap` → (ML-KEM-1024 × X25519) → `K_DEM` schedule and
CMT-4 key-committing AEAD are inherited intact in shape. What RFC-001 changes is the *role*:
v5 hid the whole design behind a mandatory USB pad; v6's threat model makes the passphrase
the sole required factor (L-9) and demotes USB to an optional hardener.

## Decision

1. **One crate boundary:** `holonomy-seal` is the only crate that links crypto primitives.
   Every other crate receives keys as opaque capability tokens (`DEMCap`, `SlabKeyCap`) whose
   types enforce flow at compile time: a token can be *used* to request an operation from the
   seal, never read as bytes outside it.
2. **Envelope (inherited):** passphrase → Argon2id (memory-hard, host-calibrated cost,
   recorded ledger-style) → `K_unwrap` → phase-1 unwrap of the per-save hybrid decapsulation
   key → ML-KEM-1024 + X25519 decapsulation → `K_DEM`. Key-committing AEAD (CMT-4) per sealed
   object, `ZeroizeOnDrop` and `DONTDUMP`-laddered buffers throughout.
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
| A2 key lifetime | keys dumping with process memory | `DONTDUMP` ladder + zeroize; capability tokens prevent copy-out | counting/zeroize tests; dump-region map check |
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

## Open questions

- Argon2id calibration target on the weakest supported reference laptop (≤1s at ≥ what memory
  fence?). Numbers land in the Block 2 calibration gate; until then the RFC commits only to
  the *mechanism* of calibration, not a guessed constant — the exact discipline H1 paid 57×
  to learn.
