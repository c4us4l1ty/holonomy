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
| OQ-4 | Scroll-timing side channel: slab fault-in pattern leaks *where* you read, to whoever sees slab access timing. Accept (viewport is presumably visible to that adversary anyway) or smooth? Decide before Block 4 gate | open |
| OQ-5 | Argon2's internal working arena: `MasterKeys::derive`, `kdf::derive_root_secret*`, and the calibration probe all allocated the m-cost arena inside `hash_password_into` and freed it unwiped (v5's H1 hit exactly this and switched to caller-owned memory + a scrubbed block). Fixed by making the arena caller-owned everywhere: `kdf::Arena(Vec<argon2::Block>)` zeroizes the slab on Drop via argon2's `zeroize` feature, and the single `run_argon2` runner serves creation, replay, *and* the calibration probe, with a test pinning byte-equality against argon2's own allocator | **closed** 2026-11-16 |
| OQ-6 | `ParamsClass::Test` misuse guard: a release build could derive a user container at the weakened schedule, and replayed records had no fence. Closed at both boundaries: `derive_root_secret` refuses `Test`; `derive_root_secret_with` refuses below-floor schedules and accepts only records emitted by the calibrated production selector; Block 3's header rejects weak memory, out-of-range memory, non-step-aligned memory, or unbounded time/lanes **before Argon2 runs**. Test/debug profiles admit only the exact test tuple. | **closed** 2026-11-16 |
| OQ-7 | Valid-prefix rollback: a byte-perfect earlier witness prefix is indistinguishable from a file that legitimately stopped there without trusted state outside the file. RFC-002 amended to claim integrity of present bytes and crash consistency, not freshness against full-prefix rollback. An external transparency/monotonic anchor would resolve this but cannot be mandatory under passphrase portability (L-9). | **open — accepted limitation, not locally solvable** |

## Audit trail

- **2026-11-16** — Block 3 implementation draft: added the fixed `.wavefunction` header,
  fenced KDF replay, sealed segment frames, MMR witness commits, lazy body reads, explicit
  passphrase-confirmed compaction, and same-directory atomic publication. Tests specify
  every-byte append-tail cuts, 30-million-word simulated growth with exact-prefix preservation,
  historical metadata/body tampering, stale-writer refusal, shared-open/exclusive-append lock
  contention, no-replace creation/compaction, and compaction behavior. Local Rust tooling is
  absent; the first CI run (43922948005) found a missing `Debug` derive used by MMR tests, fixed
  in `50ac36f`. CI run 43923405534 then passed rustfmt, clippy, workspace tests, and the gates
  contract. Block 3 remains a draft, not a passed L-6/L-10 gate: the crash matrix uses synthetic
  byte-prefix truncations and does not inject real power loss or filesystem failures.
  `metadata_digest` uses variable-output BLAKE2b-256 rather than truncating BLAKE2b-512; its
  reference-vector regression test passed in workspace CI.

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
