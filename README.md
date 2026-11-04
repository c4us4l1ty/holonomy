# Holonomy `v5.1.0`

**A daily-use, hard-security document workspace.** A rich word processor that keeps your document in
a fixed-size encrypted container instead of a file, and needs a physical USB stick to unlock.

Built on [`storytold/wordcraft`](https://github.com/storytold/wordcraft) for the editor and on
RustCrypto for the cryptography. Linux, Windows and macOS.

---

## What actually protects your document

Three layers, and it is worth being precise about which one covers what.

**1. The container leaks nothing, even across revisions.**
Every save is exactly `134,217,728` bytes of indistinguishable noise — no magic bytes, no header, no
version tag, no plaintext metadata. Every save regenerates the 64-byte epoch salt, re-permutes all
2,047 payload slots and re-encrypts under a fresh `K_DEM`. Consecutive cloud snapshots XOR to
`50.0000% ± 0.0053%` differing bits (3.5σ monobit) with no identical run longer than 42 bits, so a
diff of `T_0` against `T_n` reveals neither where you edited nor how much.

**2. Unlocking needs your passphrase *and* your USB stick.**
The document key is derived from a `ML-KEM-1024` + `X25519` decapsulation **and** a 32-byte root
share `κ_USB` that exists only on the stick. The cloud copy alone is not decryptable; the stick
alone is not decryptable. Your passphrase alone is not decryptable.

* Passphrase → `Argon2id` (256 MiB, t=4, p=2) → a 64-byte root secret → labelled subkeys.
* Root secret → unwhitens slot 0 → unwraps `SK_stratum` → decapsulates → derives `K_DEM`.
* Slot 0 is readable without circularity because `K_unwrap` comes from the passphrase and never
  from the capsule. That was `v4.0.0`'s deadlock and it is fixed.

**3. The pad is a one-time pad for 54 saves, then it is something weaker and says so.**
Each save folds all 128 MiB of the container against 32,768 distinct 4 KiB sectors of physical
entropy from the stick, chosen by a 22-bit Feistel permutation that reaches every sector of the
6.80 GiB reservoir. Zone B holds `1,782,579` sectors; a save consumes `32,768`; so **54 saves** read
mutually disjoint pad material and save 55 is the first that wraps.

* **Saving with the stick writable** ratchets the spent sectors to fresh CSPRNG bytes, so the window
  restarts and the pad stays fresh indefinitely.
* **Saving/inspecting through a hardware write-blocker** opens the device `O_RDONLY`, issues zero
  writes, and therefore cannot ratchet. Past save 54 the sectors are reused.

Past save 54, protection is `ML-KEM-1024` + `X25519` + `XChaCha20-Poly1305` with `CMT-4` commitment
— a strong claim, but **not** a one-time pad. A reused pad byte gives `C₀ ⊕ C₅₄ = M₀ ⊕ M₅₄` to
anyone holding two snapshots *and* the stick. The status line states which layer you are on, and the
save path refuses at the boundary rather than silently degrading.

---

## What it does not claim

This is the part previous drafts got wrong, and it is why the doc is this blunt.

| not claimed | why |
| :--- | :--- |
| Plaintext never exists in RAM | It exists in RAM while you edit. That is what editing is. Pages are locked and the process is not dumpable; registers are not a security boundary. |
| Screenshots can never see the document | Windows and macOS can exclude a window from capture (`WDA_EXCLUDEFROMCAPTURE`, `sharingType = .none`). **Linux has no equivalent** and the UI says so. |
| A video recorder cannot reconstruct the text | The ±42 flicker scheme that claimed this is deleted: the phase relation *is* the signal, and a cross-correlating scraper recovers it. |
| Anti-debugging that cannot be bypassed | Deleted. The old scheme mixed `.text` bytes and `RDTSC` into the keystream, which is patchable *and* made the document undecryptable on any other machine. |
| 16.6 MiB resident, zero heap | Off by 16×. Real budget: **≤180 MiB steady, ≤440 MiB unlock peak.** |
| `memfd_secret` everywhere | Linux 5.14+ only, often blocked by seccomp. The fallback is `mlock` + `MADV_DONTDUMP`, and the UI reports which rung you got. |

---

## Memory

| state | budget | breakdown |
| :--- | ---: | :--- |
| steady | **≤ 180 MiB** | 128 container (mlocked) + 50 `wordcraft` + 2 reaper/buffers |
| unlock peak | **≤ 440 MiB** | + 256 `Argon2id` arena, freed before the first slot decrypts |

`440 + 512` MiB of host floor is 952 MiB, comfortably inside 2,048 MiB. The numbers are constants in
`holonomy-core::budgets` and the gate is **two-sided**: over-budget fails, and so does any claim
below the mandatory floor (the container plus the `Argon2id` arena cannot both be absent). That
second direction is what the old 16.60 MiB figure needed and did not get — an upper-bound check
accepts any small number, which is how a claim off by 16× survived review.

The *measured* RSS gate is still a placeholder. It needs the `holonomy` binary, which needs
`wordcraft` to link; until it does, CI checks the documented envelope and says plainly that it has
not measured anything.

---

## The watchdog

Pulling the stick triggers a wipe. The mechanism is two threads with one rule between them:

```text
 Thread A (may block forever)          Thread B (never does I/O)
 pread / ioctl on the USB              sleep 1.5 ms
 on success: stamp LAST_PHY_OK_NS      read two atomics
 on suspend:  signal_suspend()         disconnected      →  wipe
                                       suspend_signalled →  wait, do not wipe
                                       gap > 12 ms       →  wipe
```

**Suspension is decided by OS notification, never by elapsed time**, and that distinction is load
bearing. A monotonic gap cannot tell a suspend from a yank: a drive unplugged and left for two
seconds is indistinguishable from a laptop that slept for two seconds. An earlier version of this
diagram said `gap > 2 s → suspend`, which meant pulling the stick and waiting two seconds avoided
the wipe — the exact attack the reaper exists to stop. A long gap with no notification is now a
yank. The consequence is stated rather than hidden: a machine that sleeps *without* delivering a
suspend event wipes on resume. Every supported platform does deliver one (`WM_POWERBROADCAST`,
`NSWorkspaceWillSleepNotification`, logind `PrepareForSleep`), and the failure mode of the safe
direction — a loud wipe recoverable from the last save — is the right one to have.

A `pread` on a yanked USB device hangs in `TASK_UNINTERRUPTIBLE` for 5–30 s, so any watchdog that
touches the device is dead exactly when it is needed. Thread B touches two atomics and nothing else.
`SCHED_FIFO` is attempted and `EPERM` is expected on a normal desktop account: the reaper starts
anyway and reports that it did, because priority is an optimisation of latency, not a precondition
of function.

Wiping all 180 MiB at legacy bandwidth would take ~45 ms — over the 15 ms target. So the reaper
wipes the registered *secret* arenas (keys, scanout, plaintext pages) and evaporates the rest by
terminating, rather than claiming it scrubbed 180 MiB in 15 ms.

The order is `wipe → terminate → best-effort blackout`, not `wipe → blackout → terminate`. A blackout
is a compositor call that can block, and the conditions that trigger a wipe (yanked drive, hostile
bus reset) are exactly the conditions under which a GPU driver may also be stuck. `terminate` is
what makes the secrets unreachable; the blackout is a courtesy and must not sit in front of it.
`HOLONOMY_BLACKOUT_IS_SAFE=1` opts into the other order on platforms whose blackout cannot block.

---

## Layout

```text
crates/
  holonomy-core      container geometry · 65,536-byte capsule · FPE-22 · duress · stats · budgets
  holonomy-crypto    Argon2id → subkeys · CMT-4 AEAD · ML-KEM-1024 envelope · pad budget · locking
  holonomy-usb       device geometry · O_DIRECT aligned I/O · pad plan · sector ratchet
  holonomy-runtime   zero-I/O PHY reaper · secret arenas
  holonomy-ui        wordcraft shell · capture exclusion · the `holonomy` binary
holonomy-old/        READ-ONLY scrapyard. Not a dependency. Do not build on it.
docs/
  PRD-v5.1.0.md      the authoritative spec
  AUDIT-v5.1.0.md    build audit against the seven checkpoints
PRD_AUDIT.md         the teardown of v5.0.0-OMEGA that motivated all of this
```

`holonomy-old/` is 142,500 lines at `v3.0.0-SINGULARITY`, kept as a reference. Exactly one module
was salvaged from it: the `O_DIRECT` alignment rules in `holonomy-usb/src/io.rs`.

---

## Build

```bash
cargo build --release
./target/release/holonomy --check     # verify the invariants on this machine
./target/release/holonomy --version
```

**Status: v5.1.0 is the audited core, not a finished editor.** The container, key schedule, USB pad,
permutation, duress bookkeeping, watchdog and capture exclusion are implemented; 164 of the 182
tests execute in CI and all four gate jobs are green. The `wordcraft` document↔container bridge is
not yet wired.

`holonomy` (the binary) lives in `holonomy-ui`, which pulls in six `wordcraft` crates and their
pinned `craft-*` git dependencies. That crate has **never been compiled** — the authoring sandbox
had no route to `crates.io` — so its 18 tests have never run and nothing in the audit relies on
them. `docs/AUDIT-v5.1.0.md` §8 separates what is proven from what is deferred and why.

Requires Rust 1.95+ (`wordcraft`'s MSRV) and network access to `github.com` for its pinned `craft-*`
dependencies.

---

## Licence

`MIT OR Apache-2.0`, matching `wordcraft` and RustCrypto. `LICENSE-MIT`, `LICENSE-APACHE` and
`NOTICE` carry the dual grant and the upstream attribution.

---

## Honest credits

The architecture in `PRD_AUDIT.md` and `docs/PRD-v5.1.0.md` is the result of an adversarial teardown
of five prior drafts. Most of what was removed was removed because the numbers did not work, not
because the ideas were uninteresting: the 54-save budget, the 41.2% unreachable drive, the 1,027 MiB
of scanline bitplanes, the 9.83σ tolerance and the 65,552-byte capsule are all arithmetic, and all
of them are now asserted in code.
