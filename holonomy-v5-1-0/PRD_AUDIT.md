# Holonomy `v5.0.0-OMEGA` — Buildability Audit

**Question asked:** is the app in `PRD.md` buildable, or are there fatal flaws?
**Answer:** The PRD is **not buildable as specified**. Seven of its ten "authoritative invariants" are
either internally unsound, physically impossible, or provably leak what they claim to hide. What remains
after removing them is a real, ordinary, shippable product — and `holonomy-old/` already implements most
of it.

This is an audit only. No application code was written.

---

## 1. Verdict in one table

| # | Invariant / claim | Status | Verdict |
| :-- | :--- | :--- | :--- |
| 1 | True 1:1 information-theoretic USB Vernam entanglement | sound idea, **broken construction** | **FATAL** — pad is reused, so it is not an OTP (§3.1) |
| 2 | Zero-plaintext-in-silicon homomorphic cut-through | restates a tautology, adds a leak | **FATAL** — AES/OTP argument is a single-SIMD-instruction illusion; plaintext still in the framebuffer (§3.2) |
| 3 | Constant-variance SPOC / IND-SPOC-VIDEO | **provably false** | **FATAL** — the PRD's own equations leak, and the display cannot render ±42 (§3.3) |
| 4 | Branchless wavefunction collapse / ZERO-BRANCH-ATTEST | **provably false** | **FATAL** — `if` comparisons are `JNE`; the deadlock bug; the decoy index is itself the tell (§3.4) |
| 5 | Cross-process `memfd_secret` split state | partial | **Not fatal**, but the `memfd_secret` claim is Linux-only and overstated (§3.5) |
| 6 | Forward-only duress co-evolution, retrospective `T_0` cleanliness | plausible | **Workable**, but see the structural-metadata objection (§3.6) |
| 7 | 15 ms decoupled PHY reaper | sound design, mostly | **Buildable**; the 15 ms *ceiling* is not defensible on the stated baseline (§3.7) |
| 8 | `CMT-4` key commitment + 50.000% Hamming | sound | **Buildable**; the tolerance is the wrong shape (§3.8) |
| 9 | Zero-disk airlock & CoW/NVMe incineration | sound | **Buildable** (§3.9) |
| 10 | 2 GiB legacy envelope, 16.60 MiB RSS, 10,000 pages, zero-heap | **arithmetically impossible** | **FATAL** — the budget omits 384+ MiB, and the design does not fit in the container (§3.10) |

Beyond the invariants, the PRD cannot be compiled as written: the §10 `Cargo.toml` does not resolve, the
§7 reference code does not do what its comments say, and the declared UI foundation has a different
architecture and licence position than the document assumes (§4).

---

## 2. How this was checked

* Constants and formulas from `PRD.md` were re-derived arithmetically (struct sizes, Feistel domain,
  Hamming statistics, bit-reuse probabilities, memory budgets).
* Every external claim was checked against the live upstream repositories over `api.github.com`
  (`storytold/wordcraft`, `RustCrypto/KEMs`, `dalek-cryptography/curve25519-dalek`,
  `rust-windowing/softbuffer`, `darrenldl/reed-solomon-erasure`).
* `holonomy-old/` (14 crates, ~142k lines, 1,711 tests) was read as prior art — it is the same product
  line at `v3.0.0-SINGULARITY`, and several of its source comments are themselves corrections of
  defects that the v5 PRD has **re-introduced**.
* **Not checked:** a real `cargo build`. This sandbox has no Rust toolchain and no route to
  `crates.io`/`static.crates.io`, so dependency claims below are from manifests and API metadata, not
  from a compile.

---

## 3. The fatal flaws

### 3.1 The Vernam pad is reused, so it is not a one-time pad — and the Feistel cannot fix it

FR-2.2 / Invariant 1 claim "true Shannon one-time-pad security": 128 MiB of container XOR 128 MiB of
physical USB entropy, selected by a 21-bit Format-Preserving Feistel permutation.

Three separate things break this.

**(a) The pad is reused roughly every 1.3 saves.** Zone B holds `1,782,579` sectors. Each save draws
`32,768` of them. If the set of drawn sectors were re-randomised per epoch (which "regenerated
`σ_epoch`" implies), the probability that a save collides with an *earlier* save's sectors is:

| save # | expected fraction of drawn sectors already used in an earlier save |
| ---: | ---: |
| 2 | 3.1 % |
| 5 | 11.8 % |
| 10 | 24.5 % |
| 22 | 48.1 % |
| 54 | 80.9 % |

The PRD's own epoch formula hardcodes that reuse instead: `Epoch_Counter × 32,768 + i (mod 1,782,579)`
means epoch `e` reads the same physical sectors as epoch `e + 54`, deterministically. `C_0 ⊕ C_54 =
USB[·] ⊕ USB[·]` (which mostly cancels) `⊕ M_0 ⊕ M_54`. Two cloud snapshots and one USB seizure then
give a *difference* of two plaintexts — the classic two-time-pad break, which is exactly the class of
attack FR-1.3's ratchet exists to prevent.

**(b) 128 MiB of pad bytes never changes.** Every container byte is folded against USB bytes that are
written **once at provisioning**. A pad byte that is used and never rotated is not a pad. The stated
absolute guarantee ("even an unbounded quantum hypercomputer... recovers strictly 0.000 bits")
reduces to whatever the AEAD gives you, i.e. it is false. The only construction that restores it is
FR-1.3's ratchet: *write fresh CSPRNG bytes to the just-used sectors on every save*, and burn the old
ones. That is incompatible with the `O_RDONLY` write-blocker mandate (FR-1.1.3) and with 3 saves/minute.

**(c) The permutation has a dead 41 % of the drive.** The Feistel is defined over `[0, 2^20−1]` and
cycle-walked into `[0, 1,782,578]`. `1,782,579 − 2^20 = 734,003` sectors (41.2 % of Zone B, ~2.8 GiB)
are **outside the codomain of a bijection on `2^20`**, so they are unreachable by construction. The
"uniform random pad" is drawn from 59 % of the drive, always.

Not fatal to the *product*; fatal to the *claim*. Also note the round function is `10-bit halves ×
6 rounds` with `f` masked to 10 bits — a 20-bit block cipher. That is fine for collision-freedom, and it
should be described as a sector-index scrambler, not as anything stronger.

### 3.2 "Zero plaintext in silicon" is a restatement of a tautology, with a leak added

The claim is that `G` never exists in RAM, cache or a register; only `G ⊕ R_t` does.

**It is true and it means nothing.** "Never compute `X`" is satisfied by "compute `X ⊕ r`", for any `r`.
The system still deterministically derives `G` from data it holds; you have only chosen a different
representation. The moment `R_t` is handed to the blitter (one register, one cacheline — the code passes
it as `r_t_ptr`), `G` is one `pxor` away. "Plaintext exists only after two XORs instead of one" is not a
security boundary.

Worse, the claim does not survive the pipeline it is embedded in:

* **The framebuffer holds `G ⊕ R_t`.** The very next stage is `spoc_modulate_subframe_pair`, which takes
  `masked_glyph_row` and `r_t_row` and *branches on `g_xor_r`* to emit `170` vs `86`. So `G` is
  recoverable by definition from the frame buffer plus your own share buffer, both of which are on the
  hostile host. The scanout surface is the one place a screen scraper can always read.
* **`CryptScanline` cannot fit, and layout leaks text.** One bitplane page at 816×1056 (96 dpi Letter)
  is 107,712 B. 10,000 pages is 1,027 MiB — **8× the entire 128 MiB container**. At a toy 640×480 it is
  still 2.9×. So either pages are stored as text and rasterised on the fly (which contradicts the
  "pre-baked at edit-time" claim), or the page budget collapses to roughly one page per 64 KiB slot.
* **A final XOR before the framebuffer does not work when the pixel is output.** Here is the honest
  version, and the old repo's blit is it: decrypt into a store the hostile process cannot read, then XOR
  the visual share at the last opportunity. The `memfd_secret` sentinel (FR-6.3) is the only part of this
  invariant that carries weight.

### 3.3 SPOC constant-variance is provably false, and the panel cannot render it

Invariant 3 and FR-7.1 claim that a multi-frame video recorder extracts 0 bits, because every pixel has
mean 128 and variance 1,764 everywhere.

**It is false by the PRD's own equations.** FR-7.1.3 states the phase relation
`E[F_t(x,y) · F_{t+1}(x+v_x, y)]` is driven by `G(x,y)`. A sign flip *is* a covariance sign flip, and
covariance is a plain second-order statistic. A scraper computes
`R(Δx, Δy) = Σ_t F_t(x,y)·F_{t+1}(x+Δx, y)` over a grid of `Δx`; the `Δx` that maximises it exposes `v_x`,
and the *sign map* of `R` at that lag is the glyph bitmap. This costs a few milliseconds per frame pair
and no knowledge of where the human is looking. Amplitude invariance / flat mean / flat variance defeat
only *zero-order* (mean) and *stationary* (variance) analysis; they cannot defeat cross-correlation,
because cross-correlation is the textbook defaulter's attack on any two-frame visual cryptography scheme
(Naor–Shamir shares included). "Text exists solely in the spatiotemporal cross-correlation phase" is not
a security property — it is a precise description of the leak.

**And the panel cannot display it.** Every pixel must take only `86` or `170`. `|Δ| ≡ 42` on every pixel
in every frame means the panel is driven at its maximum contrast on every pixel every frame — a full
black/white checkerboard flickering at 60–120 Hz. Two effects: (i) it is a photosensitive-epilepsy
hazard, and any product shipping it has a serious duty-of-care problem; (ii) real LCD grey-to-grey
transitions are 5–20 ms and gamma/asymmetric, so at 8.3 ms frame spacing most pixels will not reach
`86`/`170` — the constant-variance premise fails *at the hardware layer* on exactly the legacy panel
class the PRD targets. On many TN panels, dithering the same pixel ±42 at 60 Hz is visible as a
shimmer, which is its own metadata leak ("this window is Holonomy").

**Keep instead:** what `holonomy-old` already had — one-time-pad visual shares, `WDA_EXCLUDEFROMCAPTURE`
/ `sharingType = .none`, and per-frame re-blinding. Those are real, testable mitigations against
`BitBlt` and Recall. They do not need SPOC, and SPOC's own test (`TC-SPC-06`) cannot pass.

### 3.4 "Zero anti-debug branches" cannot exist, and the collapse has two hard bugs

**(a) The property is vacuous.** The branchless compare in §7.2 ends in a `for` loop and a mask select;
the *resulting index* is then used as `[primary, decoy][beta]`, and the compiler emits a conditional
move plus, at the caller, a conditional jump on which document to render. The claim "the binary contains
zero `if/else` that can be patched to `NOP`" is a statement about source syntax. Any of it can be patched:
the constant `Expected_PEED_Const`, the mask, the table. Security properties have to be stated as
"attacker must invert/forge X", not as "we did not write `if`".

**(b) The construction is self-defeating.** `PEED = BLAKE2b-256(CodeBytes[fn_start..fn_end] ‖ Caller_RIP ‖
…)`. The decryption keystream is *keyed by the hash of the code that is running*. Any tool that changes
the code — an in-memory patch, an ASLR relocation of `Caller_RIP`, a different compiler, a JIT, a
different kernel layout, a CPU that relocates pages — yields a different `PEED` and therefore a
different `K`, and **the renderer cannot decrypt the primary stratum at all**. That is not "seamless
collapse to Decoy `L_1`"; it is "the app shows garbage and the user loses their document". `Caller_RIP`
being in the descriptor is especially bad: it makes decryption depend on the call site, so the same block
decrypts differently from a different code path. It also hard-codes defeats for legitimate things ASLR
and `prelink` exist to do, and it makes `SIGSEGV`/`INT3` handling mandatory rather than optional.

**(c) The decoy index is the tell.** The API is `vault_handles: &[[u8; 32]; 2]` — index `0` primary,
index `1` decoy. A reverse engineer reading the caller learns exactly which handle is the real vault,
then NOPs the select. A "decoy" that is *distinguishable in the data structure* is not a decoy; it is a
label. The whole stratum-`L_1`-under-debugging story dies at the first `cargo build --symbols`.

**(d) `Min3_RDTSC_Bucket` is a hash input with no stable value.** It is a *measured quantity*. Two
adjacent renders differ; the keystream differs; nothing decrypts. A branchless design that folds an
unpredictable timing measurement into the decryption key is not anti-debugging, it is non-determinism in
the AEAD.

**Verdict:** keep the anti-analysis goal, drop the mechanism. What actually works is boring: strip, LTO,
`panic=abort`, no symbols, integrity-checked `.rodata`, and mapping *core dumps are unavailable*
(`PR_SET_DUMPABLE`). Any honest anti-debug story has to admit it is best-effort.

### 3.5 `memfd_secret` is Linux-only; the Windows/macOS halves do not exist

Invariant 5 asserts `memfd_secret(0)` and "DACL-stripped `VirtualLock` on Windows" as equivalent. They
are not. `VirtualLock` prevents paging; it does not remove the page from the direct map, and
`ReadProcessMemory`/`MiniDumpWriteDump` on a same-user process still sees it. The mitigation on
Windows/macOS is `PROCESS_PROTECTION`/`SetProcessMitigationPolicy` and `VM_PROTECT_TERMINATE`, plus not
being dumpable — none of which is "the MMU unmaps it". The invariant should say "Linux: strong;
Windows/macOS: best-effort".

Also: `memfd_secret` is a *Linux kernel* feature (5.14+), not a buildable constant. On the PRD's own
baseline (a 2008 Core 2 Duo, "Libreboot/Coreboot with Intel ME neutralized"), a 5.14+ kernel is a
separate and possibly unmet requirement.

### 3.6 The structural-metadata hole the PRD never addresses

The PRD's threat model includes "an inspector holding all historical cloud snapshots `T_0 … T_n`" who
"inspects both `T_n` and `T_0` for hidden decoy queues or edit-volume anomalies".

The invariant it offers is that `T_0`'s decoy stratum is clean. Fine. But the inspector does not need the
decoy queue: **he has the multiset of snapshots and the coercion transcript.** If `T_0` unlocks with the
duress passphrase and `T_17` does not, he learns (i) the duress passphrase was rotated or the duress
layer was removed, and (ii) which snapshots are which. If the duress stratum's *word count* is flat across
`T_0 … T_n` while the primary's grows, that is the tell. The PRD's effort-matching (FR-5.2) addresses the
*contents* but not the *cross-snapshot consistency*. This is a design gap, not a fatal flaw — but the
"100 % T_0 cleanliness" claim is doing more work than it can support, and the retrospective-audit
test (`TC-DUR-03`) as written will pass while the scheme still fails.

### 3.7 The 15 ms ceiling is a nice target attached to an impossible budget

The decoupled-reaper architecture (Thread A does I/O and stamps an atomic; Thread B does no I/O and only
reads the stamp) is **correct and buildable** — it genuinely sidesteps the uninterruptible `pread` hang.
Keep it.

Two caveats. (i) `SCHED_FIFO` 99 requires `CAP_SYS_NICE` (root, or an `rtprio` rlimit); an unprivileged
desktop user gets `EPERM`. (ii) The 15 ms *p99* figure on a 2008 Core 2 Duo with DDR2, while zeroizing
16.6 MiB of arena, is asserted, not derived: a single memset of 16.6 MiB at DDR2 bandwidth (~5 GB/s
realistic) is ~3.3 ms, the scanout zero is ~0.8 ms, and `XSync`/`DwmFlush`/`wl_display_flush` round-trips
to a compositor on the same loaded machine can each exceed the remainder. The design is right; the number
needs a measured basis, per host class.

### 3.8 `CMT-4` is sound; the Hamming spec is measured in the wrong units

`Tag = BLAKE2b-256("CMT4" ‖ K ‖ N ‖ j ‖ Poly1305_tag)` is a fine key-commitment construction (it is the
standard `commit = H(K, N, A, C)` shape) and defeats multi-key/partitioning-oracle and Invisible
Salamanders attacks. Keep. The `[u8; 32]`-tags-everywhere story is a little over-engineered given
`XChaCha20Poly1305`'s native 16-byte tag already binds `K` — but it is cheap and harmless.

The problem is `50.000 % ± 0.015 %` (FR-2.1, Invariant 8). For `1,073,741,824` independent bits the
standard deviation of the Hamming fraction is `0.5/√(2^30) ≈ 0.00153 %`. The stated tolerance is therefore
**9.83 σ** — an acceptable range so wide that a snapshot could be 0.015 % *identical* and still pass, while
a correct implementation misses it ~1 time in 10⁴⁴. Either state it as `50 % ± 9σ` (i.e. no constraint) or
use the right statistic for the property you actually want: "no long run of equal bits" / longest-run and
monobit tests. As written the test is both unfalsifiable and meaningless.

### 3.9 The zero-disk airlock is fine — with one correction

FR-7.3 is the most defensible requirement in the document. Two notes. (i) The "`mlock`ed local loopback
WebSocket drop" still requires the *browser* to have read the file from disk; the only zero-disk variant
is a user-mediated over-the-network path (the container syncing straight to the app from the cloud API),
so the airlock should be described as "the app never writes it", not "it never touches the SSD". (ii)
`fallocate(PUNCH_HOLE | ZERO_RANGE)` deallocates extents but cannot trigger NVMe FTL erasure reliably on
wear-levelled consumer SSDs; the claim should be "the filesystem no longer references these blocks", and
anyone storing state secrets should be told full-disk encryption is the real answer.

### 3.10 The 2 GiB / 16.60 MiB / 10,000-page envelope is arithmetically impossible

The §6.2 budget sums to 16.60 MiB and lists: scanout 7.81, atlas 0.13, Fenwick 0.50, CryptRope 6.50,
piece table 1.20, sentinel 0.46. It **omits**:

| omitted | size |
| :--- | ---: |
| the `mlock`ed 128 MiB `.wavefunction` arena (must be resident to be decrypted/folded) | 128 MiB |
| the 128 MiB USB Vernam pad (or 32,768 random 4 KiB sector reads per save — see below) | 128 MiB |
| Argon2id arena, `m = 262,144 KiB` (transient but `mlock`ed and unavoidable) | 256 MiB |

Honest steady-state is **≥ 272 MiB**; honest unlock peak is **≥ 544 MiB**. Both fit in 2 GiB. Neither
fits in "16.60 MiB", and the zero-heap invariant ("no dynamic allocation while the mutation lock is
held") is impossible with `wordcraft`'s architecture. This invariant is aspirational, not a spec.

Two further arithmetic failures inside FR-10:

* **"17 L2-cache operations ≈ 12 ns" for a Fenwick query is off by ~10×.** 17 dependent loads at a
  4-cycle load-to-use plus add is ≈ 68–85 cycles, i.e. ~30 ns at 2.4 GHz — and a *dependent* chain of
  17 misses into a 512 KiB tree cannot all be L2 hits. The L2-resident premise is also wrong: 512 KiB
  does not fit in the baseline's 3–6 MiB L2 *alongside* 128 MiB of container pages streaming through it.
* **The NFR blit budget implies DRAM bandwidth the target does not have.** Cut-through on 3.91 MiB ×
  5 streams in 0.31 ms is **66 GB/s**. DDR2-800 dual-channel peak is 12.8 GB/s theoretical, ~5–6 GB/s
  realistic. The correct number is ~1–4 ms/frame on the baseline, i.e. the "locked 60/120 FPS at
  0.45 ms" claim fails on the stated hardware by 10–40×.

---

## 4. Why it will not compile

### 4.1 The §10 `Cargo.toml` does not resolve

| declared | actual | problem |
| :--- | :--- | :--- |
| `ml-kem = "0.2"` | current is **0.3.4** (2026-10-31) | pinning the *older* line contradicts FR-3.2, whose whole point is already upstream in 0.3.4: its changelog ships "Harden reduction using newly-hardened `ctutils`", "Harden equality testing", "Harden zeroization". Pin 0.3.4 and enable its `zeroize` feature, or the KyberSlash hardening the PRD demands is not there |
| `x25519-dalek = "2.0"` | current is 2.0.x | OK |
| `curve25519-dalek` "Elligator2 bijective point mapping" | **only `ristretto/elligator.rs` exists** | there is **no Elligator2 for `X25519`/Montgomery in dalek**; `x25519-dalek` has no `elligator` feature. FR-3.1's `ct_x25519_elligator` and the whole "uniform 32-byte ciphertext" argument cannot be built from the named crates. Ephemeral X25519 public keys are *already* near-uniform, so the requirement is unnecessary as well as unavailable. |
| `wordcraft-core = { path = "./vendor/wordcraft" }` | no such package | upstream is a workspace of `wordcraft-geom/-doc/-fonts/-layout/-render/-ui-egui/...` |
| `winit 0.30` + `softbuffer 0.4` for "zero-GPU CPU scanout" | — | **this is the architecture the PRD bans.** `wordcraft`'s renderer is `vello_cpu` (a CPU rasteriser) behind an **egui** UI. `softbuffer` + `winit` layered under it is a rewrite; the "vendor the block hierarchy, rich span model and shortcut router" step is the *entire product*. |
| `brotli-decompressor = "4.0"`, `fontdue 0.9` for "WOFF2 typography" | upstream uses `wuff 0.2.9`, `skrifa 0.47`, `harfrust 0.13` | **the PRD re-solves a problem already solved, and solves it worse.** `holonomy-old/PROJECT.md §2.1` already recorded that the pure-Rust WOFF2 path "does not compile" and shipped brotli-compressed TTF instead. The v5 PRD re-introduces WOFF2 plus `fontdue`, abandoning shaping (`harfrust`) — which means no kerning, no ligatures, no complex-script support in a "rich document" editor. |
| `mitex 0.2` for "`typst-layout`" | `mitex` parses TeX; `typst-layout` is a separate crate and is **not** in the manifest | FR-10.3's "vendored `mitex` + `typst-layout`" cites a crate that is not declared |
| `zune-jpeg 0.4`, `zune-png 0.4`, `qoi 0.4`, `lz4_flex 0.11` | several of these are on different major lines today | version drift; not separately fatal |
| `reed-solomon-erasure = "6.0"` | moved to `rust-rse/reed-solomon-erasure`, **last push 2023-08-30** | unmaintained (3 years stale); needs a maintenance decision before it is load-bearing on the user's only backup |
| `[package] version = "5.0.0-omega"` | — | fine (pre-release semver) |
| `license = "MIT"` | `wordcraft` is **`MIT OR Apache-2.0`** | vendoring it under a bare MIT claim is a licence-compliance error; you must carry both texts and the NOTICE |

**FR-3.2's build-time audit is stated too broadly.** "Hardware integer division instructions (`udiv`,
`idiv`, `div`) are strictly prohibited in the compiled binary and audited at build time" would fail the
build on divisions in unrelated dependency code (formatting, allocation, hashing) and cannot distinguish
the secret-dependent reduction from a `u64` split in `blake2`. The property that actually matters is "the
`ml-kem` reduction path contains no secret-dependent division" — which 0.3.4's `ctutils` hardening gives
you, and which you test with a targeted `objdump` check on one function plus a constant-time test, not a
global grep.

Also missing from the manifest but required by the body text: `libc` features for `memfd_secret`,
`io_uring`, `seccomp`, `nix`, a `softbuffer` `kms` path (`softbuffer`'s DRM/KMS backend is **Tier 3** —
"builds in CI"), and any `MITE`/math layout dependency.

### 4.2 The §7 reference code does not do what its comments say

* **`StratumRootCapsuleV5` is 65,552 bytes.** Field-by-field: `1,568 + 32 + 24 + 3,216 + 32 + 24 + 60,608 +
  16 + 32 = 65,552`. The slot is 65,536. It **overflows by exactly 16 bytes** — the appended Poly1305 tag.
  This is byte-for-byte the same defect `holonomy-old/crates/holonomy-container/src/layout.rs` documents and
  fixes ("PRD §5.4 describes 64 KiB block + 16-byte appended Poly1305 tag = 65,552 bytes… not a multiple of
  512, let alone 4096"). v5 has re-introduced it, and now the 60,608-byte directory field has to shrink.
* **`WRAPPED_SK_BYTES = SK_PQ + SK_ECC + 16 = 3,216` does not hold a wrapped `SK`.** The comment says
  "SK_PQ + SK_ECC + Poly1305 Tag", but an AEAD ciphertext is `plaintext + tag`, and `3,168 + 32 + 16` is
  the sum *with* the tag counted once — so the struct is really `3,184` bytes of ciphertext plus a 16-byte
  nonce and 16-byte tag that are counted elsewhere. There is no room for the `sk_unwrap_nonce` inside the
  field it claims to secure.
* **`homomorphic_cut_through_blit_sse2` is a no-op on its own premise.** It computes
  `((C ⊕ R) ⊕ K1) ⊕ K2 = G ⊕ R`. So `s_out` is a buffer holding `G ⊕ R` — i.e. **the plaintext is now in a
  writable buffer**, one XOR from `G`, and `r_t_ptr` is passed unencrypted alongside it. The function's
  doc-comment ("Plaintext `G` NEVER exists in any register at any clock cycle!") is true only because the
  function never uses `G`; it is not a security statement about anything downstream.
* **`spoc_modulate_subframe_pair` branches on the secret.** It contains
  `let phase_flip = 1 - 2 * (g_xor_r ^ r_bit);` — a data-dependent value that selects `170` vs `86`. That
  is the exact "branch on decrypted glyph bit" that FR-6.1's oblivious-multiplexer story exists to prevent,
  and it is what makes the cross-correlation attack of §3.3 work.
* **`usb_feistel_sector_trajectory` indexes `k_perm[round*4 .. round*4+4]` for `round in 0..6`** — fine
  (24 of 32 bytes used), but the `f` function output is masked `& 0x3FF`, so each round contributes 10
  bits of a 20-bit state: the whole permutation has ~20 bits of block entropy. It is a scrambler, not a
  cipher, and the code's "ARX PRF" comment oversells it.
* **`{ beta }` is computed with a comparison.** `(diff as u32).wrapping_sub(1) >> 31` is branchless in Rust
  *source*, but `computed_peed[i] ^ expected_peed_tag[i]` compiles to `cmp`/`setne`; and `vault_handles` is
  `&[[u8;32];2]`, so the "arithmetic mask select" is followed by a real conditional in any caller. Source-
  level branchlessness is not observable in the binary.

---

## 5. What *is* buildable — and what `holonomy-old/` already proves

Strip Invariants 1–4 and 10 as stated, and the document keeps: an encrypted fixed-size container, Argon2id
+ VDF key derivation, XChaCha20-Poly1305 chunk AEAD, a sealed musl binary, CPU-only scanout, a real text
engine, HTML/PDF export, an image airlock, and the decoupled reaper.

**`holonomy-old/` has already built that.** 14 crates, ~142,500 lines, 1,711 tests, two CI gates
(`verify-release.yml` enforces stripped-static size ceilings — **1,863,000 B sealed / 3,333,880 B desktop**
— and `editor-snapshot.yml` renders the real UI for visual review), plus measured facts about the target
machine that the v5 PRD does not have. Its own docs record corrections that v5 has undone:

| v5 PRD says | `holonomy-old` measured |
| :--- | :--- |
| Argon2id 256 MiB, t=4, p=2 → 1,180 ms | **8,382 ms** at t=16 (≈1.7 ms/GiB-pass); the v5 figure is ~7× optimistic |
| §7.4 blit kernel is correct | "dimensionally wrong, not just off by a rounding" — `blit.rs` rewrites it |
| 64 KiB block + 16 B tag | "65,552 is not a multiple of 512" — `layout.rs` moves the tag inside the slot |
| "WOFF2 typography" | "**unavailable** — `woff2` 0.2.1 and 0.3.0 both fail to compile"; shipped brotli-compressed TTF |
| `Ω` at an arbitrary offset | "`O_DIRECT` requires the *file offset* of each transfer to be a multiple of the logical block size" — Ω must be chunk-aligned |
| ml-kem / PQ core | **not implemented.** `grep` finds `ml-kem`/`kyber` only in prose (`PROJECT.md`, `OLDPRD.md`); `holonomy-crypto`'s deps are `argon2` + the VDF. The PQ half is genuinely new work. |
| SPOC / cut-through | **not implemented.** The old `blit.rs` is an A8-over-XRGB8888 alpha blit, i.e. it paints *plaintext* glyph coverage. This is the one place v5 is new, and it is the one place §3.2/§3.3 say the design does not work. |
| 10,000 pages at 60 FPS | old code is a CAGR gap-rope text engine with damage tracking; it does not claim the impossible RAM/bandwidth numbers |

So the honest build order is: **`holonomy-old` is the product.** v5 should be treated as a requirements
*document to prune*, and the surviving new work is (a) the PQ KEM envelope + `CMT-4`, (b) the reaper,
(c) the airlock, (d) the duress strata. Everything else is either already built or provably wrong.

---

## 6. Fatal flaws, ranked by how much they force a redesign

1. **§3.10 — the memory/latency envelope is false.** Re-baseline every NFR against measured numbers (the
   old repo has them). This is the only one that changes the product's *shape*.
2. **§3.1 — the OTP is not an OTP.** Either ratchet the pad on every save (and drop the write-blocker
   guarantee, or accept 3 saves/minute), or drop the Shannon/information-theoretic claim. You cannot have
   `O_RDONLY` + "0.000 bits without the USB" + a static pad.
3. **§3.3 — SPOC does not survive cross-correlation and cannot be displayed.** Replace with honest
   per-frame re-blinding + OS capture exclusion. Delete `TC-SPC-06`; add a test that *tries* the
   cross-correlation attack and asserts something true about what it finds.
4. **§3.4 — the anti-debug scheme is both patchable and self-defeating.** Replace with best-effort
   hardening. Delete `FR-6.2`; the keystream must be deterministic.
5. **§3.2 — "zero plaintext in silicon" is a slogan, not a boundary.** Say what is actually true: *decrypted
   pixels live only in a `memfd_secret`/protected-process store, and OS capture is excluded*. That is
   defensible and implementable.
6. **§4 — the manifest and reference code.** One day of mechanical work, but nothing builds until it is
   done.

---

## 7. What I need from you before writing any code

1. **Which product is this?** A hard-security-workspace *usable daily*, or a research demo of the seven
   breakthroughs? They have different success criteria, and §3.10 says you cannot have both on a Core 2 Duo.
2. **May I prune?** My recommendation: keep invocations 6–9, rewrite 1–4 as honest mitigations, delete 10
   as written, and re-baseline every number in §6 against `holonomy-old`'s measurements.
3. **Is `holonomy-old/` the base?** If yes, the port is "add PQ envelope + reaper + airlock to a working
   editor", not "vendor `wordcraft` + `winit`/`softbuffer` and rebuild the editor".
4. **Licence.** `wordcraft` is `MIT OR Apache-2.0`; the PRD declares bare `MIT`. Confirm you will carry both.
5. **The SPOC/duress claims in the README.** These will ship to users as marketing. They are currently
   false (§3.3, §3.6). Do you want them reworded before I touch any code?

---

*Audit performed against `PRD.md` @ `dc93f557`, `holonomy-old/` @ same commit. No source was modified;
`holonomy-old/` was not deleted, renamed or moved.*
