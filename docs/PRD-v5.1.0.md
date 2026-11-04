# PRODUCT REQUIREMENTS DOCUMENT — Holonomy `v5.1.0`

**Product:** Holonomy — a daily-use, hard-security document workspace.
**Supersedes:** `v5.0.0-OMEGA` (see `PRD_AUDIT.md` for the teardown that motivated every change here).
**Classification:** none. This document is meant to be read by users, because its claims are the ones
that must be true.

---

## 1. What this product is

A rich word processor that stores your document in a fixed-size encrypted container instead of a
file format. The container is safe to keep in cloud storage: it leaks nothing about its contents,
including across many revisions. Unlocking it requires your passphrase **and** a physical 8 GB USB
stick. Losing either one makes the cloud copy unrecoverable; losing both makes it unrecoverable by
anyone.

It is built on [`storytold/wordcraft`](https://github.com/storytold/wordcraft) (MIT OR Apache-2.0),
which provides the document model, text shaping, layout, CPU rasteriser and UI. Holonomy adds the
container, the key schedule, the USB pad, the watchdog and the duress strata.

**It is not** a bare-metal operating system, a bootloader, a display server, or an anti-forensics
tool. Those were in previous drafts and are out of scope.

---

## 2. What changed from `v5.0.0-OMEGA`, and why

Seven claims were removed. Two were unfixable as designed, three were false, one was impossible on
the stated hardware, and one was miscounted by 16 bytes.

| Removed | Why |
| :--- | :--- |
| **SPOC** (±42 constant-variance flicker) | A cross-correlating video scraper recovers the text: the phase relation *is* the signal. Also a photosensitive-epilepsy hazard, and off-the-shelf panels cannot hit `86`/`170` in 8.3 ms. |
| **`CryptScanline`** (pre-baked 1-bpp encrypted pages) | 10,000 Letter pages of 1-bpp bitplanes is 1,027 MiB — **eight times the whole container**. And `(C ⊕ R) ⊕ K` leaves `G ⊕ R` in a writable buffer with `R` passed alongside it. |
| **`PEED` anti-debug** (`.text`‖`RIP`‖`RDTSC` in the keystream) | Two fatal defects: the compare constant is in `.rodata` and patchable, and *any* patch or relocation changes the key, so the document simply stops decrypting. |
| **Two live `vault_handles`** | The decoy was `[1]` and the primary `[0]`. A reverse engineer reads one pointer instead of defeating a predicate. |
| **16.60 MiB RSS / zero heap** | Off by 16× in steady state and 33× at the unlock peak; it omitted the 128 MiB container, the 128 MiB pad and the 256 MiB `Argon2id` arena. `wordcraft` allocates; a "zero heap" claim would have been false the day it shipped. |
| **`FPE-20`** (2^20 sector permutation) | The domain was smaller than Zone B: **41.2% of the drive was unreachable**, permanently. |
| **`±0.015%` Hamming tolerance** | 9.83σ on 2^30 bits. Not a test; no cipher output could fail it. |

---

## 3. Architecture

```text
   8 GB USB (read-only unless saving)                 cloud storage
   ┌──────────────────────────────┐                  ┌────────────────────────┐
   │ P1  LBA 0..4,095   16 MiB    │                  │ vault.wavefunction     │
   │ Zone A   256 MiB  AONT+RS    │  XOR fold        │ 134,217,728 bytes      │
   │ Zone B   ≥ 6.80 GiB pad      │ ────────────────▶│ 2,048 × 64 KiB slots   │
   │ Zone C   150 MiB  ratchet log│  32,768 × 4 KiB  │ no header, no magic    │
   │ Zone D     4 MiB  heartbeat  │  sectors/save    │ IND-URN noise          │
   └──────────────────────────────┘                  └────────────────────────┘
              │                                                  │
              ▼                                                  ▼
   ┌─────────────────────────────────────────────────────────────────────────┐
   │ holonomy-core      geometry, capsule (65,536 B), FPE-22, duress, stats  │
   │ holonomy-crypto    Argon2id → subkeys, CMT-4 AEAD, ML-KEM-1024 envelope │
   │ holonomy-usb       geometry, O_DIRECT I/O, pad plan, ratchet            │
   │ holonomy-runtime   zero-I/O PHY reaper, secret arenas                   │
   │ holonomy-ui        wordcraft shell: save cadence, capture exclusion     │
   └─────────────────────────────────────────────────────────────────────────┘
              │
              ▼
   wordcraft (upstream, pinned): doc · fonts · layout · render(vello_cpu) · ui-egui
```

---

## 4. Container

**FR-4.1 — Geometry.** Exactly `134,217,728` bytes. `2,048` slots of `65,536`. No magic bytes, no
version tag, no plaintext header. Slot 0 is the stratum root node at a **fixed** offset; slots
`1..=2,047` are permuted per epoch.

> The fix here: `v5.0.0` put the root at a fixed offset *and* at `π(0)`. Both cannot hold, and a
> permuted root is locatable only if you already hold the salt inside it. Fixed offset 0 wins; the
> permutation covers the other 2,047 slots.

**FR-4.2 — Salt.** `σ_epoch` is 64 bytes at offset 0, regenerated on every save, and it is itself
under the pad — so it is not the one stable fingerprint in the file.

**FR-4.3 — Root capsule.** `size_of::<StratumRootCapsuleV5>() == 65_536` and
`align_of::<CapsuleWire>() == 4_096`, both as `const _: () = assert!(...)`. Field table:

| field | bytes |
| :--- | ---: |
| `ct_ml_kem_1024` | 1,568 |
| `ct_x25519` | 32 |
| `sk_unwrap_nonce` | 24 |
| `wrapped_stratum_sk` (3,168 ML-KEM dk + 32 X25519 sk + 16 Poly1305) | 3,216 |
| `phase1_cmt4_tag` | 32 |
| `directory_nonce` | 24 |
| `encrypted_directory_and_reservoir` (**excludes** the tag) | 60,592 |
| `directory_poly1305_tag` | 16 |
| `phase2_cmt4_tag` | 32 |
| **total** | **65,536** |

**FR-4.4 — Indistinguishability.** Every save re-permutes all 2,047 slots, regenerates `σ_epoch`,
and folds against a fresh pad window. Gate: consecutive snapshots `T_k−1 ⊕ T_k` must pass
**Monobit at `50.0000% ± 0.0053%` (3.5σ)** and **Longest-Run ≤ 42**, plus NIST SP 800-22 and
`dieharder -a` over the whole file.

> `±0.0053%` is the corrected constant. The old `±0.015%` was 9.83σ — see §2.

---

## 5. Keys

**FR-5.1 — Determinism.** Nothing environmental enters a key or a keystream. Not the clock, not
`RDTSC`, not `.text`, not `RIP`, not the pid, not the build, not the host. Identical inputs produce
identical keys on every machine. Enforced by `crates/holonomy-crypto/tests/static_audit.rs`.

**FR-5.2 — Stage 1.** `Argon2id`, `m = 262,144 KiB`, `t = 4`, `p = 2`, salt = `σ_epoch`, output
64 bytes. This is the 256 MiB term in the unlock peak, and it is the reason the unlock peak is not
the steady state.

**FR-5.3 — Stage 2 (deleted VDF, kept hardening).** The old `Holonomy-Lattice-Walk` is retained as
a **2 MiB in-L2 sequential walk, `T = 2,000,000`**, whose only job is to raise the cost of offline
passphrase search on a machine that has already paid for `Argon2id`. It is **not** claimed to defeat
an ASIC, because a sequential function with a public structure does not.

**FR-5.4 — Subkey separation.** `BLAKE2b-512(len‖label‖0x00‖root‖len‖context)` truncated to 32
bytes, with the length prefixes and the `0x00` separator so no label is a prefix of another. Labels:
`K_PERM`, `K_WHITEN`, `K_UNWRAP`, `K_DEM`, `N_EPOCH`, `K_DECOY`.

**FR-5.5 — Two-phase envelope.** `K_unwrap` comes from the passphrase and **never from the
capsule**, which is what makes slot 0 readable without the circularity that deadlocked `v4.0.0`.
Order: unwhiten → unwrap `SK_stratum` → decapsulate `ML-KEM-1024 + X25519` → derive
`K_DEM ‖ N_epoch` → zeroize `SK_stratum`. Only `PK_stratum` survives, so a background save
encapsulates without the private key present.

**FR-5.6 — `CMT-4`.** `BLAKE2b-256("CMT4" ‖ 0x00 ‖ K ‖ N ‖ len‖A ‖ len‖C)`, verified **before** the
AEAD tag. `A` binds the stratum id and slot index, so a ciphertext cannot be relocated within the
container. Two commitments per capsule (phase 1 and phase 2), both pinned by the size assertion.

---

## 6. USB pad — the honest version

**FR-6.1 — What is folded.** All `134,217,728` container bytes XOR `134,217,728` physical bytes,
`32,768` distinct 4 KiB sectors per save. One save = one container = one pad window.

**FR-6.2 — The permutation.** `FPE-22`: 6-round balanced Feistel over 11-bit halves, domain
`2^22 = 4,194,304 ≥ 1,782,579`, cycle-walked into Zone B. **Every Zone B sector is reachable.**
`FPE-20` reached 58.8% of it.

**FR-6.3 — The 54-save budget.**

```text
  Zone B  = 1,782,579 sectors
  per save=    32,768 sectors
  budget  =        54 saves   (54 × 32,768 = 1,769,472 ≤ 1,782,579)
```

Saves `0..=53` read mutually disjoint sectors. Save 54 is the first that wraps. This is a **budget
with a stated consequence**, not a guarantee:

* **Writable USB (`O_RDWR`), save path:** after each save the spent sectors are overwritten with
  fresh CSPRNG bytes. The window restarts. Indefinite fresh pad.
* **Write-blocked USB (`O_RDONLY`), unlock and view:** zero writes to the bus, so a hardware
  write-blocker sees nothing. But no ratchet, so past save 54 the sectors are reused.

**Past the budget, the protection is `ML-KEM-1024` + `X25519` + `XChaCha20-Poly1305` with `CMT-4`,
and it is not a one-time pad.** A reused pad byte gives `C₀ ⊕ C₅₄ = M₀ ⊕ M₅₄` to anyone holding
both snapshots *and* the USB. The UI says this in the status line
(`ProtectionLayer::describe`), and `PadBudget::complete_save` refuses the save outright rather than
degrading silently. Saying so is the requirement; pretending otherwise is what §2 removed.

**FR-6.4 — Write-blocker compatibility.** Unlock and view open the device `O_RDONLY` only. A ratchet
requires an explicit `O_RDWR` handle, acquired for the save operation and released after.

---

## 7. Duress strata

**FR-7.1 — One stratum per unlock.** `ActiveStratum` is move-only, non-`Clone`, and produced only by
`Unlocked::into_active(self)`. There is no second handle to select from, so there is no index to
patch and nothing extra to leak.

**FR-7.2 — `P_1` opens `T_0 … T_n`.** The duress stratum's `K_unwrap` is derived from
`(passphrase, σ_epoch)` only — never from anything a save ratchets. Keypair rotation (FR-5.5) does
not touch it. `duress_binding_is_invariant` asserts it across a save history.

**FR-7.3 — No future state at `T_0`.** Decoy edits are **generated**, not stored: derived per save
from `(epoch key, cursor)` by a deterministic PRNG, and applied to `L_1` at save time. There is no
reservoir, so there is nothing at `T_0` that references the future. This is strictly stronger than
`v5.0.0`'s parasitic reservoir, which had to be authored in advance.

**FR-7.4 — Proportional growth.** The decoy grows at `62% ± 4%` of the primary's net character
delta and shrinks when the primary shrinks. Cumulative word counts must stay in `[0.45, 0.85]` of
each other across `T_0 … T_n`. An exactly proportional decoy is itself a fingerprint, which is why
the ratio is a band and not a constant — the same reasoning that killed `50.000%`.

**FR-7.5 — Duress saves are non-destructive.** A duress save re-whitens unallocated blocks under
the new `σ_epoch` rather than overwriting them with fresh chaff, so the primary stratum survives.

---

## 8. Rendering and capture

**FR-8.1 — Render normally.** `wordcraft` + `vello_cpu`. Plaintext exists in RAM while you are
editing; that is what being able to edit means, and no amount of intra-register masking changes it.
The protection is that the process is not dumpable and its pages are locked.

**FR-8.2 — Capture exclusion, where the platform has it.**

| platform | API | result |
| :--- | :--- | :--- |
| Windows 10 2004+ | `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` | enforced |
| macOS 10.15+ | `NSWindow.sharingType = .none` | enforced |
| Linux | — | **unavailable, and reported as such** |

Linux has no portable equivalent: a compositor-level screencast reads the same buffers the display
does. `CaptureExclusion::LinuxUnavailable` is a first-class outcome shown in the status line, not an
error logged and hidden. On Linux, the security claim is about the **container**, not the window.

**FR-8.3 — Memory locking, with the real ladder.**

| mechanism | property | available |
| :--- | :--- | :--- |
| `memfd_secret(0)` | out of the kernel direct map | Linux 5.14+, often seccomp-blocked |
| `mlock` + `MADV_DONTDUMP` | resident, not in core dumps | Linux, macOS |
| `VirtualLock` | resident only; `ReadProcessMemory` still reads it | Windows |

`LockCapability` reports the rung actually obtained. "Out of the direct map" is only claimed on
rung 4.

---

## 9. The watchdog

**FR-9.1 — Decoupled.** Thread A does all device I/O and may block forever in `pread` after a yank
(the SCSI handler holds it 5–30 s in `TASK_UNINTERRUPTIBLE`). Thread B performs **no** I/O: it sleeps
1.5 ms, reads two atomics, and either loops or wipes. That structure is correct and is kept.

**FR-9.2 — Suspend is not a yank.** A gap greater than 2 s is treated as suspend, not disconnect. A
laptop that slept for an hour must not wipe the document.

**FR-9.3 — `SCHED_FIFO` is attempted, not assumed.** `EPERM` is expected on an unprivileged desktop
account; the reaper starts at normal priority and *reports* that it did. Priority is an optimisation
of latency, not a precondition of function.

**FR-9.4 — The 15 ms budget, decomposed.**

```text
  1.5 ms   poll quantum                       fixed by design
  +        wipe of registered secret arenas   measured, a few MiB
  +        blackout (scanout zero + flush)    measured
  =        total                              must be <= 15 ms
```

Wiping all 180 MiB of steady state at legacy DDR2 bandwidth is ~45 ms, which is **over** the
ceiling. The reaper therefore wipes the registered *secret* arenas — keys, scanout, plaintext pages
— and evaporates everything else by terminating, rather than pretending it scrubbed 180 MiB in
15 ms. `ReaperGuard::measure` records what it actually took on the host.

---

## 10. Memory — the re-baselined numbers

**FR-10.1 — Steady state: ≤ 180 MiB.**

| component | MiB |
| :--- | ---: |
| `mlock`ed container (must be resident) | 128 |
| `wordcraft` state: doc model, glyph cache, layout, two `vello_cpu` surfaces | 50 |
| reaper stack, sentinel channel, sector-index buffers | 2 |
| **total** | **180** |

**FR-10.2 — Unlock peak: ≤ 440 MiB.** Steady state + `Argon2id` arena (256 MiB), freed before the
first slot is decrypted.

**FR-10.3 — Envelope.** `440 + 512` MiB of host floor = 952 MiB, inside 2,048 MiB.
`check_against_envelope` is called in CI with measured RSS, so the table cannot drift from the code
again.

**FR-10.4 — No zero-heap claim.** Sensitive data lives in `zeroize`d, page-locked arenas owned by
`holonomy-crypto`. The rest of the editor allocates, as editors do.

---

## 11. Interface

Native desktop window via `wordcraft-ui-egui`. No compositor, no display server, no GPU requirement
— `vello_cpu` rasterises on the CPU. The status line reports, from live state rather than from
copy: protection layer per save, fresh-pad saves remaining, capture exclusion, memory-locking rung,
reaper priority.

---

## 12. Verification gates

Every gate is machine-checkable and runs in CI.

| gate | assertion |
| :--- | :--- |
| `capsule_is_exactly_one_slot` | `size_of == 65,536`, compile-time and runtime |
| `encrypted_directory_field_is_60_592` | plus 16-byte tag = 60,608 of AEAD output |
| `zero_overlap_across_fifty_four_saves` | 1,769,472 distinct LBAs over 54 saves |
| `full_zone_b_is_reachable` | >35% of samples above `2^20` |
| `monobit`/`longest_run` | `50.0000% ± 0.0053%`, run ≤ 42 |
| `no_environmental_input_enters_a_key` | source audit; no `rdtsc`/`RIP`/`.text`/clock |
| `unlock_recovers_the_directory` | real ML-KEM-1024 + X25519 round trip |
| `wrong_usb_cannot_open_the_directory` | κ participates in `K_DEM` |
| `read_only_session_refuses_at_the_pad_budget` | save 55 on `O_RDONLY` errors, does not degrade |
| `thread_b_fires_and_wipes` | wipe exactly once, blackout, terminate |
| `suspend_is_not_confused_with_a_yank` | 2 s gap ⇒ `Suspend` |
| `duress_binding_must_be_invariant` | `P_1` opens every snapshot |
| `proportional_growth_rejects_a_frozen_decoy` | cross-snapshot tell is caught |
| budget gates | `check_against_envelope(180, 440)` passes; 16.60 MiB fails |

---

## 13. Known limits, stated rather than buried

1. **Past 54 write-blocked saves the pad is reused.** Post-quantum layer only. The UI says so and
   the save path refuses rather than degrades.
2. **Linux cannot exclude a window from screen capture.** Windows and macOS can.
3. **`memfd_secret` is Linux 5.14+ and frequently seccomp-blocked.** The fallback is `mlock` +
   `MADV_DONTDUMP`, which is weaker and is reported as weaker.
4. **`ML-KEM` in Rust has not been independently audited.** The crate says so in its own README and
   so does this document.
5. **`wordcraft` is a pinned git dependency**, not a crates.io release. Building needs `github.com`.
6. **The duress stratum's plausibility is a heuristic**, not a proof. An inspector who knows you and
   knows the scheme is the threat model's hardest case, and effort-matching raises the cost of the
   attack rather than closing it.
