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
