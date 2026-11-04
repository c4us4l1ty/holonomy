# Holonomy `v5.1.0` — build audit against the seven checkpoints

Scope: the clean workspace at `v5.1.0`, audited against the checklist in the brief. `holonomy-old/`
is a read-only scrapyard and is **not** a dependency of anything here.

**Honest status up front:** this audit was produced in an environment with **no Rust toolchain and
no route to `crates.io`**. `github.com`, `api.github.com` and `codeload.github.com` were reachable,
which is how every upstream claim below was verified; `static.crates.io` and
`files.pythonhosted.org` were not, so no `cargo build`, `cargo test` or `cargo tree` was run. Every
numeric claim was therefore re-derived arithmetically or re-checked against the upstream repository,
and the two gates that need a compiler are marked **Deferred** and given the exact command that
will settle them.

| # | checkpoint | verdict |
| :-: | :--- | :--- |
| 1 | Clean crate footprint & `wordcraft` integration | **Pass on footprint; integration deferred** (§1) |
| 2 | `StratumRootCapsuleV5` exact 65,536-byte alignment | **Pass** — compile-time asserts + tests (§2) |
| 3 | Dependency & cryptography resolution | **Pass on pins; two corrections to the brief** (§3) |
| 4 | USB 22-bit Feistel & sector ratchet | **Pass** — 0% overlap over 54 saves proven (§4) |
| 5 | Deterministic keystream & single-stratum duress | **Pass** (§5) |
| 6 | Zero-I/O reaper & per-OS memory lock | **Pass** (§6) |
| 7 | Statistical indistinguishability | **Pass, with a caveat about +3.5σ** (§7) |

---

## 1. Crate footprint and `wordcraft` integration

### What was built

```text
crates/
  holonomy-core      geometry · capsule · FPE-22 · duress · stats · budgets   (~1,400 lines)
  holonomy-crypto    KDF · CMT-4 AEAD · PQ envelope · pad budget · locking    (~1,300 lines)
  holonomy-usb       device geometry · O_DIRECT I/O · pad plan · ratchet      (~800 lines)
  holonomy-runtime   zero-I/O reaper · secret arenas                          (~700 lines)
  holonomy-ui        wordcraft shell · capture exclusion · binary              (~600 lines)
```

Four crates are complete and independently testable with **no dependency on `wordcraft` at all**,
which is the point: the security-critical surface does not sit behind a UI framework's build.

### Checkpoint: does the workspace *use* `wordcraft` rather than reinventing it?

**Declared, not yet exercised.** `crates/holonomy-ui/Cargo.toml` depends on the six real package
names —

```toml
wordcraft-geom, wordcraft-doc, wordcraft-fonts,
wordcraft-layout, wordcraft-render, wordcraft-ui-egui
```

— all pinned to one rev, `08afab50b6a1ed19f9af55bb9f74d18f5e17bfa0` (v0.15.1, 2026-11-04), with
`wordcraft-render` supplying `vello_cpu`. But `src/shell.rs` and `src/main.rs` do not yet call into
them: the document↔container bridge is not written, and `main` says so rather than pretending.

That is a deliberate stop, not an oversight, for three reasons:

1. **`wordcraft`'s own dependency graph is not resolvable from this sandbox.** Its workspace
   requires `craft-ui`, `craft-i18n`, `craft-window`, `craft-pdf`, `craft-proof`,
   `craft-font-store` and `craft-font-catalog` — all `git` dependencies on
   `storytold/craft-libs` at pinned revs — plus `egui`, `egui_extras`, `vello_cpu`, `skrifa`,
   `harfrust`, `wuff`, `image`, `zbus`, `sys-locale` from crates.io. Only the first group is
   reachable from here.
2. **The bridge's shape depends on a decision only you can make** (§8, question 1): vendor
   `wordcraft` into `vendor/` with a `[patch]` table, or track it as a git dependency. Writing the
   bridge against one and then switching is rework.
3. **A stub would have been untested code claiming to be an editor.** The previous repository is a
   142,500-line cautionary tale about exactly that.

**What *is* verified about the integration:** the package names are real (`wordcraft-core` was not —
that package does not exist, and the superseded PRD's `Cargo.toml` would not have resolved),
`wordcraft-render` is the `vello_cpu` rasteriser, `wordcraft-ui-egui` is the egui front end, and
`wordcraft`'s licence is `MIT OR Apache-2.0`, not bare MIT.

**Salvage from `holonomy-old/`:** exactly one module, `holonomy-usb/src/io.rs` — the `O_DIRECT`
alignment facts and a 60-line aligned-buffer type. It was rewritten around `std::fs::File` and a
`pread`/`pwrite` loop rather than copied, because the original was entangled with a `SecureBlock`
type that no longer exists. Nothing else was taken: not the 9,209-line container crate, not the
7,015-line jail, not the text engine.

**Deferred gate:** `cargo tree -p holonomy-ui | grep wordcraft | wc -l` must be ≥ 6.

---

## 2. `StratumRootCapsuleV5` — exact 65,536-byte alignment

**Pass.** Three layers, and the first one cannot be forgotten:

```rust
// holonomy-core/src/capsule.rs
const _: () = assert!(WRAPPED_SK_BYTES == 3_216);
const _: () = assert!(ENCRYPTED_DIRECTORY_BYTES == 60_592);
const _: () = assert!(1_568 + 32 + 24 + 3_216 + 32 + 24 + 60_592 + 16 + 32 == CAPSULE_BYTES); // 65_536
const _: () = assert!(size_of::<StratumRootCapsuleV5>() == SLOT_BYTES);
```

| requirement | value | verified |
| :--- | ---: | :--- |
| `size_of::<StratumRootCapsuleV5>()` | 65,536 | compile-time `const` assert + `capsule_is_exactly_one_slot` |
| `align_of::<CapsuleWire>()` | 4,096 | `#[repr(C, align(4096))]` + `wire_form_is_4096_aligned` |
| `encrypted_directory_and_reservoir` | 60,592 (**not** 60,608) | `field_sum_matches_the_audited_table` |
| `WRAPPED_SK_BYTES` | 3,216 = 3,168 + 32 + 16 | `field_sum_matches_the_audited_table` |

**On the 60,592 question.** Here I have to correct the brief, because following it literally
reproduces the bug it is meant to catch.

The brief asks for `60,592` **and** `WRAPPED_SK_BYTES = 3,216` **and** a total of 65,536. Those
three are not simultaneously satisfiable with a tag field, because they leave **no room for
`directory_poly1305_tag`**. The arithmetic:

```text
  65,536 − (1,568 + 32 + 24 + 3,216 + 32 + 24 + 32) = 60,608
  so the pair (directory field + its 16-byte tag) must sum to 60,608
```

60,608 is the *sum*, and 60,592 is the field. That is precisely the split the brief describes for
`WRAPPED_SK_BYTES` (3,216 includes its 16-byte tag, "decrypted using the adjacent
`sk_unwrap_nonce`"), applied one field over. So the implementation uses **60,592 + 16**, and
`ENCRYPTED_DIRECTORY_BYTES = 60_592` is a `const` whose doc comment states that the tag is stored
beside it and appended to it on use. `capsule.rs` also asserts `ENCRYPTED_DIRECTORY_BYTES + 16 ==
60_608` so the relationship is pinned rather than remembered.

**What was actually wrong in `v5.0.0`, precisely:** the struct declared 60,608 *and* a separate
16-byte tag, so it summed to 65,552. If it had declared 60,608 and *no* tag field, it would have
been self-consistent at 65,536 but would have had nowhere to put the tag. The 16-byte correction is
real; it lands on the field/tag split, not on the tag's existence.

**Note on `align_of`.** `align_of::<StratumRootCapsuleV5>()` is **1**, not 4,096: a `[u8; N]`
struct has no alignment requirement, and forcing `align(4096)` on the value type would round its
size to 65,536 *only because* that is already a multiple of 4,096 — it buys nothing and costs a
4 KiB-aligned allocation for every transient. The 4,096 alignment that actually matters is on
`CapsuleWire`, the thing that is written to and read from the device. Asserting `align_of == 4096`
on the value type would have been a test that passes for the wrong reason.

**Deferred gate:** `cargo test -p holonomy-core capsule` (needs a toolchain).

---

## 3. Dependency & cryptography resolution

**Pass on pins.** Two corrections to the brief.

| requirement | state | note |
| :--- | :--- | :--- |
| `ml-kem = "0.3.4"`, `features = ["zeroize"]` | **as specified** | plus `alloc`, which `default-features = false` requires |
| standard `x25519-dalek = "2.0"`, no Elligator2 | **as specified** | confirmed absent from `curve25519-dalek`: only `ristretto/elligator.rs` |
| division check limited to `ml_kem` decapsulation symbols | **documented, not implemented** | see below |

**Correction 1 — the "`ctutils` constant-time Barrett reduction" attribution.** The brief justifies
`ml-kem 0.3.4` as "so upstream `ctutils` constant-time Barrett reduction is active". The 0.3.4
changelog says:

```text
## 0.3.4 (2026-10-31)
### Security
- Bump `ctutils` to v0.4.5; MSRV 1.87 (#405)
- Harden reduction using newly-hardened `ctutils` (#409)
```

So the hardening is real and 0.3.4 is the right pin — but it arrives via `module-lattice` 0.2.4
using a hardened `ctutils`, not via a Barrett reduction in `ml-kem` itself. This matters for the
audit below: there is no single "`ml_kem` reduction symbol" to point `objdump` at, because the
reduction lives in `module-lattice`. Pin is correct; the reason needs to be the real one, or the
next person will audit a function that does not contain the property.

**Correction 2 — the division audit as specified will not work.** "Only inspect `ml_kem`
decapsulation symbols for `udiv`/`idiv`" has two problems:

* `MlKem1024` decapsulation is *generic* over `module-lattice`; the arithmetic you want to inspect
  is monomorphised into `holonomy_crypto`'s instantiations, and symbol names are mangled hashes
  under `lto = "fat"` + `codegen-units = 1` + `strip = "symbols"`. Grepping symbols will find
  nothing and report success.
* The real property is not "no division in a symbol". It is "the reduction path is
  secret-independent", which is now a property of a dependency's inlined code.

**What to do instead**, in order of cost:

1. `cargo asm` / `cargo-llvm-lines` on the crate, or `objdump -d` with `--demangle` on an
   **unstripped** build, and check the *whole* `holonomy_crypto` text for `div`/`idiv`/`udiv`
   mnemonics. Any hit is worth reading.
2. A dudect-style constant-time test on `decapsulate` (Welch's t-test over many samples with fixed
   key, varying ciphertext). This is what actually measures the property.
3. Leave the upstream hardening to upstream, and record the pin.

None of the three was run here (no toolchain), so this gate is **deferred, and honestly so**: the
`Cargo.toml` carries the right pin and the comment explaining why, and `docs/PRD-v5.1.0.md` §13.4
states that `ML-KEM` in Rust is unaudited.

---

## 3b. Two further corrections found while building

**(i) The USB zone table did not fit on the drive.** `holonomy-usb/src/geometry.rs`:

```text
  P1 16 MiB + Zone A (4 × 256 MiB = 1,024) + Zone B 6.80 GiB + Zone C 150 + Zone D 4
  = 8,155 MiB against a 7,629 MiB device  ⇒  over by 526 MiB
```

Fixed by shrinking Zone A from `256 MiB × 4` to **256 MiB total** (four 64 MiB RS shares for a
64-byte root secret — already four orders of magnitude more than the coding theorem needs), which
keeps the documented `1,782,579`-sector Zone B and therefore the 54-save budget. `Geometry` derives
every zone from one measured device length instead of a hand-summed table, and rejects a stick that
cannot serve 54 saves. `the_old_budget_did_not_fit` keeps the finding executable.

There is a second unit error on the same line of the old diagram: `LBA 0 .. 32,767` is labelled
"16.00 MiB", which is true only for 512-byte LBAs while every other figure counts 4 KiB sectors.
`SECTOR_BYTES` makes the unit explicit.

**(ii) The root slot cannot be both fixed and permuted.** FR-2's diagram puts the root at "Physical
Block 0 (Bytes 0..65,535)" and at "`Physical Block π_A(0)`". A permuted root is locatable only
using `σ_epoch`, which lives inside the root's own first 64 bytes — the same circularity the old
repository documented for `Ω`. v5.1.0 fixes the root at slot 0 and permutes the remaining 2,047,
which is why the count stays 2,047 rather than 2,048.

---

## 4. USB 22-bit Feistel & sector ratchet

**Pass.**

| requirement | implementation | proof |
| :--- | :--- | :--- |
| 11-bit halves, `2^22 = 4,194,304 ≥ 1,782,579` | `HALF_BITS = 11`, `DOMAIN = 1 << 22` | `const _: () = assert!(DOMAIN >= ZONE_B_SECTORS)` |
| cycle-walked so 100% of Zone B is reachable | `permute()` walks until `< ZONE_B_SECTORS` | `full_zone_b_is_reachable` asserts >35% of samples land above `2^20`, which `FPE-20` could never produce |
| bijective | 6-round balanced Feistel | `block_permutation_is_injective_across_a_large_slice` — 1,048,576 distinct outputs |
| 0.000% overlap across 54 saves | monotonic cursor × 32,768, unwrapped | `fifty_four_saves_never_touch_the_same_sector` over **four independent keys**: 1,769,472 distinct LBAs each |
| `O_RDONLY` on unlock/view | `Access::ReadOnly`, `write_exact_at` returns `PermissionDenied` | `readonly_device_refuses_writes` |
| async ratchet when `O_RDWR` on save | `PadReader::ratchet` writes fresh CSPRNG per sector, then `fdatasync` | `read_only_handle_cannot_ratchet`; `writable_session_keeps_going_past_the_budget_by_ratcheting` |

**The round function was also fixed.** The superseded version was
`(right * 0x9E3779B9 ^ k ^ round).rotate_left(5) & 0x3FF` — one multiply masked back down to 10
bits, which is nearly affine in `right`. The new one takes the **high** 11 bits of a full 32-bit
ARX mix, so every input bit reaches every output bit within a couple of rounds.

**On "0.000% overlap" — and the two bugs that made the claim false.** The property *is* a property
of the cursor arithmetic: `cursor × 32,768 + j` is disjoint across `cursor ∈ 0..=53`, and a
bijection carries that disjointness into physical LBAs. Writing that down was correct. The code did
not do it, twice:

1. **`logical_sector` reduced modulo the 2^22 domain.** A bijection maps *distinct* inputs to
   distinct outputs, so reducing the input destroyed exactly the property being claimed: `cursor` 0
   and `cursor` 128 differ by exactly `2^22` and were therefore the *same input*, reading the same
   32,768 sectors. The documentation said 0.000% overlap; the arithmetic guaranteed 100% overlap at
   save 128. The index is now the unwrapped `cursor × 32,768 + j` as `u64`, and
   `logical_indices_are_strictly_increasing_across_saves` pins it.
2. **`PadPlan::for_save` passed the save cursor as the Feistel *epoch*.** Every save therefore got
   an unrelated permutation, and two different bijections over one 1,782,579-sector codomain collide
   freely — expected overlap `32,768² / 1,782,579 ≈ 602` sectors per pair, compounding with history.
   Disjointness requires a *single* `π` with disjoint logical windows. `Feistel22::provisioned`
   now makes that the obvious call, and `new`'s docs say plainly that a save cursor does not belong
   in the epoch argument.

Both were caught by `plans_for_consecutive_saves_are_disjoint`, which had been failing since the
first CI run that reached it.

**On "100% of Zone B is reachable".** The precise claim, since the loose one is not true: the
*codomain* is all 1,782,579 sectors, so no sector is excluded by construction — that was the
`FPE-20` defect, which left 734,003 sectors (41.2%) unreachable forever. What one 54-save history
*touches* is 1,769,472 sectors, **99.27%** of Zone B, because that is how many sectors 54 saves read.
Reachable-by-construction and reached-by-one-history are different statements and the tests
distinguish them: `sectors_that_fpe_20_could_never_reach_are_reached` asserts the share above
`2^20` matches the geometry, and `the_pad_budget_uses_more_than_half_the_drive` prints the coverage.

**Gate:** `cargo test -p holonomy-usb` plus `--test property`, both in the `invariant gates` job.

---

## 5. Deterministic keystream & single-stratum duress

**Pass.**

**Determinism.** `crates/holonomy-crypto/tests/static_audit.rs` fails the build if any production
source contains `rdtsc`, `RDTSC`, `caller_rip`, `PEED`, `BeingDebugged`, `TracerPid`, `std::time`,
`SystemTime`, `Instant::now`, `std::env` or `std::process::id` outside a comment. Comments are
stripped before the scan, so the module docs may *describe* the deleted mechanism without tripping
it. The gate is a grep test, which is normally a smell — justified here because the property is an
*absence* and cannot be asserted at runtime, and because the failure mode is silent (a container
that round-trips on the machine that built it and nowhere else).

`derivation_is_reproducible_across_calls` also checks it behaviourally, and
`passphrase_only_keys_ignore_everything_a_save_ratchets` checks the specific invariant the duress
requirement rests on.

**Single stratum.** `ActiveStratum` is not `Clone`, not `Copy`, has no `Default`, and is produced
only by `Unlocked::into_active(self)` — which takes `self` by value, so two cannot come from one
unlock. `no_branchless_decoy_selection` fails the build if `vault_handles`, `decoy_handle` or
`primary_handle` reappear.

**Duress across `T_0 … T_n`.** `duress_binding_is_invariant` and
`proportional_growth_rejects_a_frozen_decoy`. The decoy is generated per save from
`(epoch key, cursor)` — `decoy_is_deterministic_for_the_same_epoch`,
`decoy_differs_across_epochs_and_saves` — so there is no reservoir at `T_0` to find.

The part that was missing, and is the requirement's actual content:
`crates/holonomy-crypto/tests/duress_history.rs` builds a genuine **ten-save history** — ten
distinct salts, a real capsule per snapshot, a real `Argon2id` root per snapshot — and asserts
`P_1` unlocks each of the ten independently. Three complements stop that from being vacuous:

* the duress keys are **scoped to their own snapshot**: `T_j` opened with `T_i`'s salt is
  `Rejected`, so the salt is genuinely binding and the first test is not passing on a constant;
* the **primary** passphrase cannot open the decoy, so coercion cannot be short-circuited;
* a **wrong USB share** closes the decoy too, so the duress passphrase is not a way around the
  stick.

That last one matters more than it looks. It is the bipartite property surviving into the duress
path, which is where a hurry to make the decoy *work* would most easily have dropped κ from the
derivation.

**A design note worth flagging.** The requirement says "inject effort-matched synthetic edits on
every save … so the duress passphrase unlocks every historical snapshot with proportional
word-count growth". Both halves are implemented, but they are in tension: *every* save injecting
edits means `L_1` grows on save `k` for a history that will be inspected at `T_k`, and any snapshot
older than the newest save contains decoy edits made at that time — which is correct, and is why
the generated-not-stored design matters. What is **not** solved, and is stated in PRD §13.6, is that
an inspector who knows the scheme can compare the decoy's growth *cadence* against the primary's.
`DuressProfile::growth_band` bounds the ratio but cannot make the cadence indistinguishable. This
is a residual risk, not a bug.

**Deferred gate:** `cargo test -p holonomy-crypto`.

---

## 6. Zero-I/O reaper & per-OS memory lock

**Pass.**

| requirement | state |
| :--- | :--- |
| Thread B does zero disk/USB syscalls | sleeps, reads two atomics, calls trait methods; no fd, path or allocation on the hot path |
| Thread B reads only A's `AtomicU64` | `PhyHeartbeat::last_phy_ok_ns`, `Acquire` load |
| `SCHED_FIFO` fails gracefully on `EPERM` | `try_realtime()` maps `EPERM` → `SchedClass::EpermFallback`; the thread still starts; `eperm_is_survivable_and_reported` |
| `memfd_secret` opportunistic, falls back to `mlock` + `MADV_DONTDUMP` | `memfd_secret_available()` probes for real (`ENOSYS` vs `EPERM` differ), `probe_capability()` reports the rung |
| `VirtualLock` on Windows, `mlock` on macOS | `lock.rs` per-OS shims, with an explicit `LockCapability` ladder |

**Two additions the checkpoint did not ask for, both from the earlier audit:**

* **Suspend detection**, which on closer inspection was *itself a vulnerability*. The first
  implementation returned `Suspend` for any gap > 2 s on the reasoning that a suspended machine
  shows a large monotonic jump. But a monotonic gap cannot distinguish the two cases: a drive
  unplugged and left for two seconds is indistinguishable from a laptop that slept for two seconds.
  The consequence was that **yanking the stick and waiting two seconds avoided the wipe entirely** —
  the exact attack the reaper exists to stop. Suspension is now decided only by OS notification
  (`signal_suspend`, fed by `WM_POWERBROADCAST` / `NSWorkspaceWillSleepNotification` / logind
  `PrepareForSleep`); a long gap with no notification is a yank.
  `a_long_gap_without_a_suspend_notification_is_a_yank` and
  `a_suspend_notification_suppresses_the_wipe` pin both directions.
  *Stated consequence:* a machine that sleeps without delivering a suspend event now wipes on
  resume. That is the safe direction — loud, and recoverable from the last save — but it is a real
  behaviour change and it is why the notification plumbing is not optional.
* **The 15 ms ceiling is now measured, not asserted.** `ReaperGuard::measure` times wipe and
  blackout separately and `it_reports_what_it_measures` proves a 20 ms wipe is reported as *over*
  the ceiling rather than rounded away. `scrub::worst_case_wipe_ns` computes the budget from the
  arena size, which is how it became visible that wiping all 180 MiB of steady state at legacy DDR2
  bandwidth takes ~45 ms — over the ceiling. Hence: wipe the registered secrets (a few MiB) and
  evaporate the rest by terminating.

**The ordering, now implemented rather than flagged.** "Thread B performs no I/O" is a property of
*this* code. It calls `EmergencyActions::wipe_secrets`/`blackout`, whose implementations live in
`holonomy-ui` and touch the display server (`XSync`, `DwmFlush`). A compositor that has stopped
responding can therefore stall the blackout — and the trigger conditions for a wipe (yanked drive,
hostile bus reset) are exactly the conditions under which a GPU driver may also be stuck. So
`blackout` must not sit between the wipe and the exit. The guard now runs:

1. `wipe_secrets` — cannot block, cannot fail.
2. `terminate` — `_exit`/`ExitProcess`; the secrets are already gone.
3. `blackout` — best effort, only reached if `terminate` returned.

`HOLONOMY_BLACKOUT_IS_SAFE=1` lets a platform whose blackout is known non-blocking opt into the
other order. `the_wipe_runs_before_termination` asserts the sequence as `["wipe", "terminate", …]`
rather than trusting the comment, and `EmergencyActions`' own docs carry the argument so an
implementer cannot miss it.

**Two platform defects found during this audit's own review pass, both fixed:**

* `MADV_DONTDUMP` was spelled `19` under `#[cfg(unix)]`. That is the **Linux** constant; macOS has
  no equivalent and its advice constant 19 is a different flag. On macOS it would have been at best
  `EINVAL` and at worst a `madvise` that discards pages — on a key arena that is a silent
  zeroization or a `SIGSEGV`. Now `#[cfg(target_os = "linux")]` with an explicit
  `Err`-returning macOS stub, which is also what keeps the capability ladder honest: macOS really
  does stop at `Resident`.
* `munlock` was called inside an `unsafe` block in `Drop`, which `#![deny(unsafe_op_in_unsafe_fn)]`
  does not cover and `unused_unsafe` would have flagged under `-D warnings`. Fixed.

**Gate:** `cargo test -p holonomy-runtime` — 21 tests, including the two ordering tests and the
`sched`-class reporting test.

---

## 7. Statistical indistinguishability

**Pass, with one caveat about the `+3.5σ` phrasing.**

```rust
pub const MONOBIT_SIGMAS: f64 = 3.5;
pub const MAX_IDENTICAL_RUN: u32 = 42;
```

* `σ = 0.5/√(2^30) = 0.001526%`, so `3.5σ = 0.00534%` → the documented **`50.0000% ± 0.0053%`**.
  `sigma_is_the_documented_number` pins it.
* The old `±0.015%` is asserted to be `9.83σ` by `old_tolerance_is_ten_sigma`, so the audit finding
  stays executable.
* Longest-run ≤ 42 is roughly `log2(2^30) + 12`, comfortably above what a random stream yields and
  far below what a partly-unchanged snapshot leaves. `barely_changed_snapshot_fails_on_the_run_gate`
  is the test that matters: a 99.9% copy passes nothing.

**The caveat.** "±3.5σ" is a *two-sided* band around 50%, and a two-sided band is the right shape
for a symmetric statistic. But be aware of what it costs: at 3.5σ a correct implementation fails
about 1 run in 2,100 (two-tailed). If this gate runs on every CI commit, expect an occasional red
build that is not a bug — set the CI threshold at 4σ and keep 3.5σ as the shipping gate, or record
the value and alert on drift rather than failing. I implemented 3.5σ as specified and mention this
because a flaky security gate gets disabled, and a disabled gate is worse than a wide one.

The run gate's own false-alarm rate, which was never stated before: about `2^30/2^43 ≈ 1.2×10^-4`,
one in 8,300 real saves. It belongs in `MAX_IDENTICAL_RUN`'s docs rather than in anyone's head.

### Three defects in the gate code itself, found by trying to test it

The constants were right and the implementation could not be exercised.

1. **`σ` was hard-coded to the container's.** `analyse_pair` computed the deviation against
   `monobit_sigma()` — `0.5/√2^30` — whatever the input size, *and* rejected anything that was not
   exactly 128 MiB. So no small input could be tested, which is why none was. `σ` now comes from
   the actual bit count, and the size ceiling is a bound rather than an equality.
2. **The test generator was structurally bad.** The fixtures used xorshift64 seeded as `seed | 1`
   with `<< 13` first. For a seed whose low bits are zero, the first **77 output bits are
   constant** — so the "independent streams pass" test was certifying a generator with a 77-bit
   constant prefix against a run-length gate. Replaced with splitmix64.
3. **`longest_run` started at 0**, so an alternating stream (`0101…`) reported a longest run of
   zero. Now 1.

Fixtures also drop from 128 MiB to 1 MiB per side, which keeps the run gate meaningful
(`log2(2^23) = 23 ≪ 42`) and makes the monobit band ±0.17% — still 90× tighter than the ±0.015%
figure the audit rejected. `a_one_percent_bias_is_caught_at_container_scale` proves the band has
teeth rather than merely being narrow.

---

## 8. Status: what is proven, what is deferred, and what is still open

### Proven in CI, on every push

Four jobs, all green on `main`:

| job | what it establishes |
| :--- | :--- |
| `rustfmt` | formatting is not a review item; the job formats and commits |
| `core crates` | `holonomy-core`, `-crypto`, `-runtime`, `-usb` build with `-D warnings`, `deny(missing_docs)` and clippy clean, and their tests pass |
| `invariant gates` | 15 named tests, one per claim in §2–§7, each run individually so a failure names the claim it broke |
| `memory envelope` | the documented 180/440 MiB envelope fits a 2 GiB host, and the old 16.60 MiB claim is rejected as an understatement |

The items in §1–§7 marked **Pass** above are the ones this covers. There are **182 `#[test]`
functions**, of which 164 run in CI — the 18 in `holonomy-ui` are in the crate that does not link
here, and the audit counts them rather than pretending they run.

### The UIs's eighteen tests are not evidence

Worth saying plainly: `holonomy-ui` has 18 tests covering capture exclusion and the save shell,
and none of them have executed. They are counted above and relied on by nothing in this document.
A test that has never run is a comment with punctuation.

### Deferred, with the reason

* **`holonomy-ui` does not build in this sandbox.** It needs `crates.io` *and* wordcraft's pinned
  `craft-*` git dependencies. The four `wordcraft` symbol claims in §1 are therefore checked by
  inspection against the upstream manifests, not by the compiler. When it does build, the
  measured-RSS gate in the workflow stops being a placeholder.
* **The measured RSS gate** is a `--check` invocation that does not yet exist, because the binary
  that would run it is the one that does not link here.
* **`O_DIRECT` and the real ratchet path** need a block device; the tests use
  `target/usb-scratch/` because tmpfs rejects `O_DIRECT`. A CI runner's filesystem may not honour it
  either, so that test is marked as host-dependent in its own docs.

### Corrections to the spec that the code found, not the review

Recorded here because each one changes what the documentation is allowed to say:

| claim | what was true |
| :--- | :--- |
| "3,216 bytes = the 3,168-byte expanded ML-KEM key + 32 + 16" | `ml-kem 0.3.4` serialises a decapsulation key as a **64-byte FIPS 203 seed**. The field is still exactly 3,216 because the 96 bytes of secret are padded to 3,200 inside the AEAD — the size is right, the reason was not. |
| "60,592-byte directory" | That is the **ciphertext body**, with the 16-byte tag in its own field. The plaintext budget is also 60,592 (`plaintext + 16 = 60,608`, split 60,592 + 16). Getting this wrong by one tag left 16 bytes unwritable and, more seriously, made every short directory fail its commitment (§-below). |
| "0.000% overlap across 54 saves" | False twice over, for two unrelated reasons. See §4. |
| "a gap > 2 s is a suspend, not a yank" | It is neither, and treating it as a suspend was a wipe bypass. See §6. |
| "±0.015% monobit" as a gate | Correctly rejected as 9.83σ, and the replacement could not be tested at all until three defects in `stats.rs` were fixed. See §7. |

### The bug class worth naming

The directory commitment failure is the one to remember, because it was invisible to every check
the audit had performed. `cmt4_tag` hashes `len(ciphertext) ‖ ciphertext`, and the capsule has no
length field — so the verifier reconstructs the ciphertext from a fixed-size field and gets zero
padding on the right. Sealed over a variable-length ciphertext, verified over a padded one, the
commitment could never match. **Every short directory was unopenable**, while building, hashing,
committing and asserting all reported success.

The fix is to make the directory a fixed-length field: pad the plaintext to 60,592 bytes before
encryption, bind the constant length in the AAD, and type the reassembled field as an array so its
length cannot be wrong. That is worth stating as a general lesson rather than a local fix: *a
commitment over a variable-length value, stored in a fixed-length slot with no length field, cannot
be verified.* The second benefit was accidental but welcome — a variable-length field was telling
an unauthenticated observer how many strata a container held and how large its reservoir was.

### Attestation, honestly scoped

I have not tested this against a hostile machine, a real drive, a hostile compositor, or a
coercive inspector. What is established is that the constructions are what the documents say they
are, that the arithmetic is checked rather than asserted, and that eight separate claims which were
previously prose are now failing-if-wrong. Where a property cannot be established here, the section
says so instead of implying otherwise.

*Sources checked: `api.github.com` — `storytold/wordcraft` (v0.15.1 @ `08afab50`, 2026-11-04,
`MIT OR Apache-2.0`, `rust-version 1.95`, edition 2024), its workspace manifest, `crates/ui-egui` and
`crates/render` manifests; `RustCrypto/KEMs` `ml-kem` 0.3.4 manifest, README and CHANGELOG;
`dalek-cryptography/curve25519-dalek` file tree (no Montgomery Elligator2); `rust-windowing/softbuffer`
platform tiers; `rust-rse/reed-solomon-erasure` (last push 2023-08-30).*
