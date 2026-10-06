# PROJECT.md — Holonomy H1 build plan

H1 is the bare-silicon Holonomy: a single static `x86_64-unknown-linux-musl` Rust binary that
is a word processor, an encrypted container, and a sealed OS jail. It replaces H2's
Tauri + TypeScript + SQLite stack entirely.

This file is the execution order. Every phase ends with a gate that can be run
unprivileged on this machine and produces evidence. Nothing in a later phase starts on
a claim.

**Sources of truth.** `PRD.md` §7–8 (data structures, test matrix) and `Plan/Plan.md` Part 4
(the v3.0.0-SINGULARITY requirements) are authoritative. `PRD.md` lines 1–1100 and 1400–1869
are two *superseded* revisions that disagree with Part 4 (softbuffer/tiny-skia/X11,
4 GiB Argon2id, 12-word BIP-39, CRDT sync). Where they conflict, Part 4 wins. `H2/` is a
reference for what to salvage and what to refuse; it is not a dependency.

---

## 1. Measured facts about this machine

Everything below was executed, not assumed. Re-running is cheap but re-deriving is not.

| fact | value | how |
|---|---|---|
| rustc / cargo | 1.98.1, musl target installed | `rustc --version` |
| static musl build | works, `ldd` → `statically linked` | built a probe |
| baseline binary size | 381 KB (hello world, `opt-level="z"` + lto + strip) | probe |
| crypto + PDF probe binary | 516 KB static musl | probe |
| DRM card | `/dev/dri/card1`, i915, **no root needed** | `CREATE_DUMB` succeeded unprivileged |
| dumb buffer | 1280x800 bpp32, pitch 5120, size 4,096,000 (3.91 MiB) | ioctl round-trip |
| `MAP_DUMB` + `mmap(MAP_SHARED)` | works, pixel write/readback verified | ioctl round-trip |
| `DESTROY_DUMB` | works | ioctl round-trip |
| DRM ioctl constants | **PRD's `0xC02064B2` / `0xC01064B3` / `0xC00464B4` are correct** | verified against `/usr/include/drm/drm_mode.h` |
| KMS presentation (`SETCRTC`) | **not testable here** — needs DRM master, which needs the VT | probe returned `EACCES` on `SET_MASTER` |
| evdev `/dev/input/event*` | **not testable here** — `EACCES`, not in `input` group | probe |
| `/dev/uinput` | **not testable here** — `EACCES` | probe |
| `O_DIRECT` | works on `/tmp` and `/home/von` | probe |
| ImageMagick `convert` | present (PPM→PNG for headless screenshots) | `command -v` |

### 1.1 Performance constants that override the PRD

The PRD's timing table was written by estimation. Two numbers in it are wrong by an order
of magnitude. Both are measured here.

**Argon2id, `argon2` crate 0.6, native release, this host:**

| m (KiB) | t | p | GiB-passes | measured |
|---|---|---|---|---|
| 393,216 | 16 | 2 | 6.0 | **10,180 ms** |
| 262,144 | 16 | 2 | 4.0 | 8,382 ms |
| 131,072 | 16 | 2 | 2.0 | 3,247 ms |
| 65,536 | 16 | 2 | 1.0 | 1,896 ms |
| 65,536 | 4 | 2 | 0.25 | 419 ms |

Cost is linear in GiB-passes at ≈1.7 ms/GiB-pass. **The PRD's "m=384 MiB, t=16 → 180 ms"
is 57× optimistic.** NFR-1.3 (400–550 ms total
`t_kdf`) is unreachable at PRD parameters and is replaced in §2.4.

**Wesolowski VDF, 2048-bit Montgomery square, 32 u64 limbs, `opt-level=3` + lto +
`codegen-units=1` + `target-cpu=native`:**

| | measured |
|---|---|
| ns per squaring | **2,077 ns** |
| PRD's claim | 300 ns |
| T = 1,500,000 → measured here | **3,115 ms** |

The PRD's 450 ms is 7× optimistic here. `T` is therefore derived
from a measured per-squaring cost at build time, not hard-coded. See §2.4.

### 1.2 Dependency resolution, verified by compiling and running

| need | choice | evidence |
|---|---|---|
| KDF | `argon2` 0.6 (`default-features=false`, `alloc`) | runs; m=128 MiB t=2 p=2 → 524 ms |
| AEAD | `chacha20poly1305` 0.11, `XChaCha20Poly1305` | encrypt/decrypt + tamper-rejection verified |
| KDF expansion | `hkdf` 0.13 + `sha2` 0.11, `Hkdf::<Sha512>::from_prk` | **needs a 64-byte PRK** — matches PRD's `K_int` |
| hash | `blake2` 0.11 | resolves |
| scrubbing | `zeroize` 1.9 + `secrecy` 0.10 | resolves |
| PDF | `pdf-writer` 0.9.3 | valid 649-byte PDF written |
| PNG dump | `png` 0.17 *or* `convert` | `convert` present |
| WOFF2 | **unavailable** — see §2.1 | `woff2` 0.2.1 and 0.3.0 both fail to compile (23 errors, `safer-bytes` 0.2 API drift) |

`pdf-writer` API notes for whoever writes it: `aead::Aead` and `KeyInit` traits must be in
scope; `pdf.page(id).media_box(..).parent(..).contents(..).resources().fonts().pair(..)`
is a chain ending in a dropped temporary, not a `finish()` call.

---

## 2. Decisions taken, with the reason

### 2.1 Fonts: brotli-compressed TTF, not WOFF2

The only pure-Rust WOFF2 decoder on crates.io does not compile. Writing the WOFF2 glyf
transform reversal ourselves would add 2–3 days to the critical path to change one label.

Pipeline, all steps verified:

1. `hb-subset` (system HarfBuzz) subsets Inter-Regular, Inter-SemiBold, JetBrainsMono-Regular
   to ASCII + Latin-1 + General Punctuation + Arrows + Box Drawing, dropping
   `GDEF,GPOS,GSUB,kern,fpgm,prep,cvt,gasp,DSIG` — hinting tables are dead weight because
   `ttf-parser` never interprets them.
2. The subset TTFs are **committed** to `crates/holonomy-assets/assets/fonts/`. Subsetting is
   a one-time human step, not a build dependency, so the build needs no HarfBuzz.
3. `build.rs` brotli-compresses each to quality 11 and `include_bytes!`s it.

Measured embedded sizes:

| font | subset TTF | brotli q11 |
|---|---|---|
| Inter-Regular | 40,272 | 22,905 |
| Inter-SemiBold | 40,792 | 23,361 |
| JetBrainsMono-Regular | 45,552 | 20,804 |
| **total** | **126,616** | **67,070 B (65.5 KiB)** |

Brotli round-trip is byte-identical, `ttf-parser` parses the result, outlines are reachable
(`'A'` = 19 ops / 17 points, quadratic). **PRD FR-2.1/FR-2.2 are amended** to read
"Brotli-compressed TrueType", per your instruction.

One gap to close in Phase 4: cmap coverage. Inter's subset resolved 190 of ~319 requested
codepoints — it has no Arrows block. Either add a fallback or drop UI glyphs that need
arrows. A test asserts every codepoint the UI draws is present in all three faces.

### 2.2 Glyph atlas: packed, variable-width, 512 KiB budget

PRD FR-2.3's fixed 256×256 (64 KiB) atlas cannot hold 191 glyphs × 3 sizes × 3 styles.
The invariant that actually matters is L2 residency on a 3–6 MiB L2. 512 KiB is 3% of the
16 MiB RSS budget and comfortably inside L2. Phase 4 measures the real footprint and Phase 9
asserts the budget.

### 2.3 Scope: editor core + native export. No sync, no CRDT.

Per your decision. Deferred and why:

- **Cross-device sync / yrs CRDT / ML-KEM relay / Axum+PostgreSQL** — impossible inside
  `unshare(CLONE_NEWNET)`, which the PRD itself mandates. Plan.md Part 1 also rules out CRDT
  history as a forensic liability. Deferred, not rejected.
- **Typst PDF export** — H2's `translate.rs` is reusable but pulls a multi-megabyte
  dependency tree into a 2.5 MiB binary. Replaced by `pdf-writer`, which is 80 KB, needs no
  `fork`/`exec` (so it runs inside the jail), and emits vectors directly.
- **Tables, LaTeX math, inline images** — Phase 2 of the PRD's own roadmap. **Promoted: this is
  Phase 9.** See §2.9 for the arithmetic that constrains all three, and for the two measured facts
  that changed the specification.

### 2.4 Key derivation: replace the PRD's parameters with a measured budget

The PRD's 384 MiB / t=16 / p=2 costs 10.2 s here. That
is not an unlock, that is a denial of service on the user's own device. NFR-1.3's
400–550 ms total cannot be met at PRD parameters, so the budget is restated:

| parameter | value | justification |
|---|---|---|
| Argon2id m | 131,072 KiB (128 MiB) | 128 MiB is a real memory fence; a GPU attacker must supply it per guess. Measured 524 ms here. |
| Argon2id t | 2 | minimum that is not degenerate |
| Argon2id p | 2 | dual-core, no thrash |
| VDF T | `floor(target_vdf_ms / ns_per_squaring × 1e6)` | derived, not guessed |

`build.rs` runs a `vdf-calibrate` bin once and bakes `NS_PER_SQUARING` into the binary; T is
a compile-time constant computed from it. Changing the unlock budget is changing one number
and rebuilding. The `vdf-calibrate` bin is the honest artifact — it prints measured ns per
squaring so `T` can be re-derived for a different host without a rebuild.

This is a deliberate deviation from FR-4.3/FR-4.4 and is recorded as such. The security
argument is preserved (memory fence + non-parallelizable serial chain); only the constant is
corrected. §5 records where the numbers are measured now.

### 2.5 The VDF modulus `N_pub` is the RSA-2048 semiprime, hardcoded

**Amended 2026-10-03.** Superseded: the "generate a safe prime and Pocklington-prove it"
instruction. Two reasons, one engineering and one cryptographic.

**Engineering.** Generating a 2048-bit prime plus a recursive Pratt/Pocklington certificate
takes minutes to hours. That is not acceptable inside `cargo test` or `cargo build`, and a
gate that occasionally stalls the box for an hour is a gate that gets skipped. `N_pub` is a
public cryptographic parameter with no secret input: it is a compile-time constant in
`.rodata`, and the gate checks it in O(1).

**Cryptographic.** The VDF is the chain `S_i = S_{i-1}² (mod N)`. Its soundness rests on
`N` being a composite whose factorisation is *unknown*, so that `φ(N)` and `λ(N)` are
unknown and the exponent `2^T` cannot be reduced at all. That is the setting of the
Rivest–Shamir time-lock puzzle and of Boneh–Bonneau–Bünz–Fischlin.

**On the "a prime modulus is broken by Fermat" argument — it is not, and the reasoning is
worth recording because it is an easy mistake.** If `N` is prime then `φ(N) = N − 1` is
public, so `S_T = S_0^(2^T mod (N−1)) (mod N)`, and the reduction looks free. It is not:
obtaining `2^T mod (N−1)` is *itself* a sequential squaring chain of `T` steps modulo
`N−1`. Square-and-multiply on the 20-bit integer `T` yields `2^T` as an integer with `T`
bits — not `2^T mod (N−1)` — and reducing a `T`-bit integer is the same hard problem. The
residue cannot be obtained in ~21 squarings. Primality alone does not collapse the chain.

What *does* matter is different, and it is why the original `N = 2pq+1` shape is rejected:
**publishing a complete factorisation of `N − 1` is strictly worse than leaving the group
order unknown.** With `N − 1 = 2pq` fully factored, an attacker computes `2^T mod (N−1)`
by CRT into `2^T mod p` and `2^T mod q`, then recurses against `p − 1` and `q −1`. The
recursion terminates in a shortcut as soon as any modulus in the chain has smooth order,
and publishing `p` and `q` hands the attacker the complete map of that recursion. With
`N = ab` composite and `a`, `b` unknown, `φ(N)` is unavailable and no such recursion
exists at all. The rule is the opposite shape to the one first written here: publish
nothing about the order of the group.

**The constant.** `N_pub` is the RSA Laboratories RSA-2048 challenge number: a 2048-bit
semiprime whose factors RSA Laboratories generated and destroyed in 1991. Chosen because
it is the most widely replicated "random 2048-bit semiprime with unknown factorisation" in
existence, so its soundness does not rest on our own key generation.

```
c7970cedcc3b0754490201a7aa613cd73911081c790f5f1a8726f463550bb5b
7ff0db8e1ea1189ec72f93d1650011bd721aeeacc2acde32a04107f0648c28
13a31f5b0b7765ff8b44b4b6ffc93384b646eb09c7cf5e8592d40ea33c80039f
35b4f14a04b51f7bfd781be4d1673164ba8eb991c2c4d730bbbe35f592bdef5
24af7e8daefd26c66fc02c479af89d64d373f442709439de66ceb955f3ea37d5
159f6135809f85334b5cb1813addc80cd05609f10ac6a95ad65872c909525bdad
32bc729592642920f24c61dc5b3c3b7923e56b16a4d9d373d8721f24a3fc0f1b
3131f55615172866bccc30f95054c824e733a5eb6817f7bc16399d48c6361cc7e5
```

Cross-checked by extracting the 617 decimal digits from two independent renderings of the
Wikipedia `RSA numbers` article and confirming the 512-nibble hex reproduces them exactly.

**Consequence for the gate.** Pocklington is gone: a composite has no primality certificate,
and by construction its factorisation must not be published. The gate is now (a) the
constant matches, is 2048 bits and odd, (b) Montgomery multiplication round-trips against
it, (c) a short chain matches an independent implementation.

**Consequence for the chain.** `S_0 = K_int mod N` must satisfy `gcd(S_0, N) = 1`. If it
does not, `S_0` is a zero divisor, the chain degenerates into one CRT component collapsing
to zero, and both the delay and the derived key are meaningless. This is a hard error, not
a warning: the probability is ~2^-1024 by accident, but a duress or wrong-passcode path must
not be able to reach it silently. `ST` is the Montgomery chain in §1.1, started from
`K_int mod N` and squared `T` times.

### 2.6 Sandboxing: allowlist from measurement, not from the PRD

FR-5.3 lists 7 syscalls. That list is not achievable — a Rust binary needs `rt_sigaction`,
`futex`, `clock_gettime`, `mmap`/`munmap` during allocator use, and `pread64`/`pwrite64` for
`O_DIRECT`. `strace` is not installed and needs root.

So Phase 7 does it the other way round: install the seccomp filter with `SECCOMP_RET_TRAP`
(not `KILL`), let the SIGSYS handler log `si_syscall`, run the full session, and emit the
exact set of syscalls actually issued. That set becomes the allowlist, which is then switched
to `SECCOMP_RET_KILL_PROCESS` and asserted. A test runs the whole session under the final
filter and fails if anything is refused.

### 2.7 Verification: dual backend, and DRM is better than expected

`DrmScanout` is **fully exercisable on this machine** — allocate, map, write, read back,
destroy all work unprivileged. Only `SETCRTC` presentation is out of reach. So:

- `Scanout` trait: `DrmScanout` (real, default on Linux) and `HeadlessScanout` (anonymous
  mmap, PPM dump) for deterministic pixel assertions.
- `InputSource` trait: `EvdevSource` (`/dev/input/event*`, blocked here) and
  `ScriptedInputSource` (raw `input_event` byte stream — the decode path is byte-identical).
- Hardware-only paths sit behind `--features hardware` so CI never depends on them.

---

## 2.9 Phase 9 media and layout: the arithmetic, and two facts that changed the specification

Tables, math and inline images look like three independent features. They are one budget problem, and
two measurements taken on 2026-10-04 changed what Phase 9 can actually be.

### 2.9.1 Binary headroom: 1.018 MiB to the new 2.0 MiB gate

| quantity | bytes | MiB |
|---|---|---|
| release binary at `f1bb594` | 1,029,688 | 0.982 |
| Phase 9 gate | 2,097,152 | 2.000 |
| absolute ceiling | 2,621,440 | 2.500 |
| **headroom to the gate** | **1,067,464** | **1.018** |
| headroom to the ceiling | 1,591,752 | 1.518 |

The gate moves from 2.5 MiB to 2.0 MiB. That costs 0.5 MiB of slack and buys the property that a
regression in any dependency shows up before it breaches the hard ceiling rather than after.

Tables and math cost **nothing measurable**: tables are integer geometry plus the Phase 4 box-drawing
table, and math is an AST plus procedural rules plus atlas glyphs. The entire binary cost of Phase 9 is
the image decoder. A PNG decoder needs inflate; `miniz_oxide` with a hand-written PNG chunk reader is
**30–60 KiB**, about 6% of the headroom. `png` proper is 80–120 KiB with `flate2` and `crc32fast`
pulled in, still affordable but harder to defend. Budget: **60 KiB, hard.**

### 2.9.2 Measured fact: the packed fonts contain no math and no Greek

Probed every face's `cmap` on 2026-10-04. Results, per face:

| codepoint | glyph | in all four faces? |
|---|---|---|
| α β γ π σ (U+03B1…03C3) | Greek letters | **no — 0/5** |
| Σ (U+03A3), ∑ (U+2211) | sum | **no — 0/2** |
| ∫ (U+222B) | integral | **no — 0/1** |
| √ (U+221A) | radical | **no — 0/1** |
| − (U+2212) | minus | **no — 0/1** |
| ⋅ (U+22C5) | dot operator | **no — 0/1** |
| ≠ ≤ ≥ (U+2260, 2264, 2265) | relations | **no — 0/3** |
| ± × ÷ | Latin-1 supplement | **yes — via U+00B1, U+00D7, U+00F7** |

Each face carries 190–191 codepoints. The atlas windows are `0x20..0x100` (224) and `0x2500..0x2580`
(128), so U+03B1 and U+221A are outside both *and* absent from the fonts.

**The specification's "map math symbols directly to the atlas codepoints" is not implementable as
written.** So the decision, made on measurement rather than optimism:

1. **Add a fifth face: Noto Sans Math**, present on this machine at
   `/usr/share/fonts/google-noto/NotoSansMath-Regular.ttf`, **2,919 codepoints, 15/15 of the required
   set.** It is TrueType `glyf`, so `holonomy-assets`' existing rasteriser handles it with **no CFF
   path** — which is the reason it was chosen over the other candidate.
2. **STIX Two Math is rejected**, despite better coverage (4,605 codepoints). It ships as
   `.otf`/CFF, so adopting it means writing a CFF outline interpreter, and the Zero-Bézier Invariant
   (§2.2) exists precisely to keep outline evaluation out of this codebase.
3. **Add a third atlas window** `0x0370..0x03FF` (Greek, 144) plus a hand-enumerated math-operator set
   (32). Table cost: `(144 + 32) × 4 styles × 2 sizes × 10 B = 14,080` bytes, against §2.2's 512 KiB
   budget — **2.7%**, and the existing `slot_of` already handles a window table rather than two
   hardcoded comparisons.
4. `√` is drawn **procedurally**, not from the font. The radical's shape is part of the layout — it
   must stretch to the height of its radicand — so a fixed glyph is wrong at every size. This is the
   same reason the box-drawing arms are procedural, and it is the Zero-Bézier Invariant applied to a
   glyph rather than to a curve.

### 2.9.3 Measured fact: the ±1 page policy only works if images are downscaled at decode time

The specification says a 1080p RGBA image is 8.3 MiB and that "two images would trigger an OOM". The
arithmetic is exact and it is tighter than it looks:

| raster | bytes | MiB | fits an 8.0 MiB decoded budget |
|---|---|---|---|
| 1920×1080 RGBA (native) | 8,294,400 | **7.910** | exactly **one** |
| 1280×720 RGBA | 3,686,400 | 3.516 | two |
| 640×360 RGBA (page column width) | 921,600 | **0.879** | **nine** |

So a ±1-page policy with native-resolution decoding can hold **one** 1080p image. A document with two
photos on facing pages breaches the budget at the moment the second decodes, which is the OOM the
specification is trying to prevent — the policy as written does not prevent it, it only makes the
breach happen at a viewport boundary instead of at open time.

**Decision: the Iceberg cache stores page-column-width rasters, not native rasters.** The container
holds the original bytes, encrypted and on disk; the cache holds only what the viewport can show. For
the 80-column page of §5's chrome that is 640 px wide. Consequences, all intended:

* the ±1-page policy now holds ~9 images inside 8.0 MiB, so it is a policy rather than a formality;
* the SSE2 scaler is exercised on *every* image, because a 1080p source is always downscaled — which
  is the only honest way to test a scaler;
* `PaintStats::missing` gains a sibling, `resampled`, so a frame records how much work the scaler did
  rather than hiding it.

The cache still keeps **only** ±1 page, still allocates inside `SecureBlock` (which is `mmap`+`mlock`
+`MADV_DONTDUMP`, and `munmap`/`madvise`/`mlock`/`munlock` are all already in the 50-entry allowlist),
and still zeroizes on eviction — `SecureBlock::zeroize_and_release()` is the single call that does
both, and the cache must use it rather than `Drop`, so that eviction is synchronous and observable.

### 2.9.4 The RSS budget, accounted

16.0 MiB steady state, and Phase 9 is the first phase that puts *media* in it:

| consumer | MiB | note |
|---|---|---|
| decoded images (Iceberg) | ≤ 8.0 | §2.9.3, gate-enforced |
| glyph atlas | 0.5 | §2.2, measured in Phase 4 |
| CAGR text + span map | ~2.0 | 2000-page document |
| container ring (3 stages) | ~3.0 | fixed by the container design |
| chrome, damage, surface tree | ~1.0 | Phase 8 measured |
| **unallocated headroom** | **~1.5** | |

The headroom is thin and the atlas window from §2.9.2 spends 14 KiB of it. If the 8.0 MiB image
ceiling is ever raised, this table is the thing to re-derive first.

### 2.9.5 What Phase 9 explicitly does not do

* **No `png` crate.** Hand-written chunk reader plus `miniz_oxide`. §2.9.1.
* **No runtime curve evaluation anywhere.** Zero-Bézier Invariant, §2.2, extended to the radical sign.
* **No image in the export path's way.** HTML and PDF get `<img>`/XObject references resolved at
  export time, and **never from a decoded cache entry**, because a PDF must not depend on scroll
  position. **Amended 2026-10-06:** the rule stands; the mechanism does not, and could not. "Read it
  from the container fd with `pread64`" names a thing the export path does not have -- the session
  holds an `Editor` and no container -- so the exporters read `Editor::assets()`, the payload's catalog,
  which is always present. Nothing is decoded unless an image is actually reached, and neither exporter
  *can* reach `IcebergCache`: `export` takes an `&Editor` and options, so there is no parameter through
  which a cache could arrive, and adding one would stop this crate compiling. Asserted structurally
  rather than in a comment; see §9C's delivered section.
* **No SVG, no JPEG, no WebP.** One decoder, and PNG is the lossless one that suits documents.


---

## 3. What is salvaged from H2

Read by a scout against every file. Verdicts:

| H2 file | verdict | note |
|---|---|---|
| `geometry.rs` — `Fenwick` (55–170) | **copy verbatim** | the only genuinely portable structure: 2 fields, no deps, 4 direct tests. Switch `f64`→`u32` weights so `lower_bound` returns to binary lifting at O(log n) — the ulp-disagreement rationale for the O(log² n) version disappears with integer weights |
| `geometry.rs` — `Geometry`, `GeometryCalibration`, `estimate_height`, `CHARS_PER_PARAGRAPH` | **discard** | fitted constants (22.70 / 28.32 / 72.6) are Chromium DOM measurements. H1 computes line heights from font ascender/descender |
| `geometry.rs` — `scroll_compensation` (406–419) | **keep the rule** | compensate by `delta` iff `offset_of(i) + height_of(i) <= viewport_top`; DOM-agnostic |
| `order.rs` | **adapt** | u64 + 1024 gaps + rebalance is a real total order, 11 tests, zero deps. Swap `anyhow` error for the H1 error enum. Fix H2's string-matching-on-error retry (`manifest.rs:160`) rather than porting it |
| `manifest.rs` | **adapt** | ordered `Vec<ManifestEntry>` with no I/O. `mark_count` → count of interval-map spans. `block_count` survives and matters more here |
| `split.rs` — `should_split` shape | **adapt** | keep the two-check shape; both thresholds are re-derived in Phase 6 |
| `split.rs` — `MarkCostModel`, `SplitReason::Unsplitable` | **discard** | `Unsplitable` is never constructed in Rust — dead variant |
| `schema.rs`, `holo.rs`, `store.rs`, `wal.rs`, `asset_gc.rs`, `test_support.rs` | **discard** | all SQLite-coupled. `holo.rs` exists to detect the `SQLite format 3\0` magic that FR-4.1 forbids |
| `store.rs::analyze` (1301–1420) + its 11 tests | **port** | block-counting reference: counts only `doc`'s children (nested paragraphs render inside parents), and excludes inserted newlines from `char_count` |
| `error.rs` | **adapt the shape** | keep `Corrupt { section_id, reason }` — exactly right for a failed Poly1305 on a 64 KiB block. Drop `Db`/`Migration`/`SchemaVersion`/`NotADocument`. No `anyhow` catch-all: a closed enum is what a seccomp-jailed process deserves |

**Kept H2 decisions**, with the measurement behind each:

- Section ≈ 1500 words, re-derived for H1 hardware (Phase 6).
- Undo is **one 500-entry in-memory stack**, never persisted. H2 measured no version history
  as the right call; H1's threat model makes it more so.
- Height constants are measured against real rendered output, never reasoned about.
- Every harness must prove it can detect the failure it exists to detect (DOCTRINE §4).

**Rejected outright**, from `H2/spikes/REJECTED.md`: taino-edit (delegates all layout to the
browser), crdt-richtext (prototype, no undo, append-only op log = forensic liability),
CryptPad (real-time collab is an explicit anti-goal), Joplin (272 MB for the least value),
Tiptap Pages (infinite layout loop on oversized blocks — the single most important entry).

---

## 4. Target layout

```
crates/holonomy-secure      SecureBlock: mmap, mlock, MADV_DONTDUMP, PROT_NONE guards
crates/holonomy-crypto      Argon2id, VDF, HKDF, XChaCha20-Poly1305, the derived-key root
crates/holonomy-container   .wavefunction: 128 MiB IND-URN, O_DIRECT, 3-stage ring, duress
crates/holonomy-text        CAGR leaves, style interval map, undo, search
crates/holonomy-geometry    Fenwick (ported), line metrics from font metrics
crates/holonomy-assets      build.rs brotli, glyph atlas rasterizer, icon masks
crates/holonomy-render      SSE2 blitter, damage tracking, surface tree (text + icons + rects)
crates/holonomy-input       evdev, keymap, script injection
crates/holonomy-display     Scanout trait: Drm | Headless
crates/holonomy-export      streaming HTML, pdf-writer PDF
crates/holonomy-jail        unshare, no_new_privs, seccomp, tripwire handler, teardown
crates/holonomy-media       Phase 9: PNG chunk reader, Iceberg cache, AssetId, SSE2 scaler
crates/holonomy             the binary: boot sequence, session loop, CLI
```

Phase 9 adds one crate and extends three, and the placement is not arbitrary:

* **`holonomy-media`** is new and separate because it is the only part of Phase 9 that touches the
  filesystem at all. Everything else in the system is pure computation over the container fd; media
  is where a `pread64` on an encrypted chunk becomes pixels, and it deserves its own dependency edge so
  that `miniz_oxide` is reachable from exactly one place and its binary cost is one `cargo bloat` line.
* **Tables** go in `holonomy-text` (the span is data, next to `TextIntervalSpan`, so undo covers them
  for free) and `holonomy-render` (the grid is geometry, next to the chrome's integer layout).
* **Math** splits: the parser produces `MathNode` in `holonomy-text`, and the procedural layout and
  drawing live in `holonomy-render` beside the box-drawing table they share machinery with.
* `holonomy-media` depends on `holonomy-secure` for `SecureBlock` and on `holonomy-render` for the
  scaler's scanout traits. It does **not** depend on `holonomy-jail`, so it is testable in-process
  like everything except the boot chain.

`holonomy-assets` builds with `build.rs` and cannot be a dependency of anything else —
`include_bytes!` data is only visible to the crate that declares it. `holonomy` re-exports.

---

## 5. Phase plan

Phases 1–9 are strictly ordered, and there is no Phase 10. Phases 11–14 were added on 2026-10-05, after
the Phase 9C gate, and are ordered among themselves. Every phase lists the gate that must pass before the
next starts.

Phases 11–14 exist because Phases 0–9 built the parts and never assembled the product: no document is
loaded from the container, no document body text is drawn, and the per-keystroke path copies the whole
document. Their ordering is **speed before UI** — Phase 11 makes the edit path `O(edited line)`, and
Phase 14 makes a rich chrome demonstrable around a path that is not. Read the audit at the head of
Phase 11 before starting any of them; most of it is derived arithmetic rather than measurement, which is
why Phase 11's first gate replaces the arithmetic with measured numbers.

**The target-hardware run is removed, by decision, on 2026-10-05.** The laptop this is being built on,
running an ordinary X11 desktop through the `desktop` feature, is the **designated daily-driver
target**, and every performance, latency and memory number is measured and asserted against it. The
ThinkPad X200 / Core 2 Duo / GM45 run is not deferred any more; it is not going to happen.

What that changes, stated plainly rather than softened:

* **The 2 MiB binary ceiling, the 16.0 MiB steady-state RSS ceiling, the 8.0 MiB decoded-image ceiling
  and the ≤ 0.50 ms keystroke→pixel p99.9 are all re-baselined to this machine.** They are asserted in
  §6 against numbers measured here, and a number measured here is a real number — which is more than
  the plan had. What it no longer is is a claim about the hardware the design was originally sized
  for.
* **The Core 2 Duo arithmetic in §1.1 and the VDF cost of ~2,077 ns per squaring are no longer the
  inputs `T` is derived from.** `T = floor(target_vdf_ms / ns_per_squaring × 1e6)` used a per-squaring
  cost measured on hardware this code has never run on, while the measured cost *here* is ~2,657 ns.
  The gate now derives `T` from the host it runs on, so the derivation and the calibration are the same
  machine. The ~30% CIOS throughput gap noted in §1.1 is therefore still unclaimed, and is now worth
  claiming against a target that exists.
* **DRM presentation on bare silicon is no longer a target.** The `desktop` window is the compositor
  path, and §1.2's finding that DRM needs `SETCRTC` — and therefore DRM master — stops being an open
  question about hardware nobody has. `SETCRTC` stays unimplemented and unimplemented-on-purpose; the
  `Scanout` trait already has two real backends behind it.
* **What is genuinely lost:** a bare-silicon boot was going to be validated on real hardware, and now
  it is not. The jail, the container and the crypto are validated here; the *boot* on a VT with no
  desktop environment is not, and cannot be from this machine.

### Phase 0 — Repository and build skeleton

Create the workspace, `rust-toolchain.toml` pinning 1.98.1, the release profile
(`opt-level="z"`, `lto=true`, `codegen-units=1`, `panic="abort"`, `strip="symbols"`), and
`.cargo/config.toml` defaulting to `x86_64-unknown-linux-musl`. `holonomy-jail` starts empty
so every crate depends on it transitively and there is exactly one place that can allocate.

Wire a `--features hardware` flag that nothing else references yet, so the dual-backend shape
is established from commit one.

**Gate.** `cargo build --release --target x86_64-unknown-linux-musl` produces a static binary;
`ldd` prints `statically linked`; `cargo clippy --all-targets -- -D warnings` is clean.

### Phase 1 — SecureBlock and the VDF modulus

`SecureBlock::allocate(bytes)` → `mmap(MAP_PRIVATE|MAP_ANONYMOUS)`, `mlock`,
`madvise(MADV_DONTDUMP|MADV_DONTFORK)`, guard pages above and below set to `PROT_NONE`. Expose
`as_ptr`, `as_mut_slice`, `zeroize_and_release`. `Drop` scrubs with `core::sync::atomic::compiler_fence`
before `munmap`.

Then `holonomy-crypto::modulus`: the 2048-bit RSA-2048 semiprime from §2.5, hardcoded as a
`const N_PUB: [u64; 32]`. The Montgomery chain `S_i = S_{i-1}² (mod N_pub)` lives next to
it, plus the `gcd(S_0, N_pub) == 1` precondition from §2.5.

**Gate.** A test allocates 4096 bytes and confirms the pages below and above fault.
`cargo test -p holonomy-secure` passes. `cargo test -p holonomy-crypto modulus` passes: `N_pub` is 2048 bits, odd, matches the committed literal, Montgomery round-trips against it, and a short chain agrees with an independent implementation. All O(1) — no search happens in the gate.

### Phase 2 — Cryptographic envelope

Argon2id (parameters from §2.4) → 64-byte `K_int` → VDF Montgomery chain `T` times →
`K_root = Blake2b-512(S_T || K_int)` → `Hkdf::<Sha512>::expand(b"HOLONOMY_V3_BARE_SILICON", …)`
→ `K_enc(32) K_chaff(32) Ω(8) N_root(24)`. Wrap the whole root in `Secret<…>` + `ZeroizeOnDrop`.
Passphrase normalized NFKD. `mlockall(MCL_CURRENT|MCL_FUTURE)`, `setrlimit(RLIMIT_CORE, 0)`,
`prctl(PR_SET_DUMPABLE, 0)` before anything else touches memory.

**Gate.** `cargo test -p holonomy-crypto`: KDF determinism, wrong-passcode rejection, HKDF
output length 96, zeroize-on-drop observable, and a measured `t_kdf` printed and asserted
against §2.4's budget.

### Phase 3 — `.wavefunction` container

Format per PRD §7.1 with the salt at a **fixed offset 0** (not at Ω — Ω is derived from the
passphrase, so a salt stored at Ω is circular). Chaff is `ChaCha20(K_chaff, nonce=i)` filling
`[32, Ω)`. Payload: 32-byte salt, master frame (block count, doc title, KDF params), then
N × 64 KiB XChaCha20-Poly1305 chunks with `nonce_i = N_root XOR i`. Opens with
`O_DIRECT|O_SYNC`, seeks to Ω, reads only the 3-stage ring `[N−1, N, N+1]` = 192 KiB.
Create (write a fresh 128 MiB file) and open paths. Duress: Ω_A and Ω_B, both in the same
container; opening with B never touches bytes at Ω_A.

**Gate.** Round-trip test: create → close → open → read → write → close → open → verify.
Entropy: a NIST SP 800-22 subset (frequency, block-frequency, runs, cumulative sums,
`pi`, entropy) over the produced file, asserting p in range and Shannon ≥ 7.99999.
A test asserts the steady-state resident memory is ≤ 192 KiB + overhead, not 128 MiB.

### Phase 4 — Typography and the A8 atlas

`build.rs`: brotli q11 each subset TTF, emit sizes. Boot: `brotli::Decompressor` streaming
into a `SecureBlock`, `ttf_parser::Face::parse` on the slice, walk the outline once per
glyph per style per size, flatten quadratics, scanline-fill into a packed variable-width A8
atlas. Budget 512 KiB, asserted. Boot cost measured and reported.

Codemepoint coverage is a test, not a comment: every character the UI draws must resolve in
all three faces.

**Gate.** `cargo test -p holonomy-assets`: brotli round-trip is byte-identical, atlas ≤ 512 KiB,
every required codepoint present, `'A'` rasterizes to the expected coverage histogram, and
boot-to-ready is printed.

### Phase 5 — Renderer: SSE2 blitter and surface tree

`blit_a8_line` — the PRD's SSE2 kernel, with one correction: `(fg*a + bg*(255-a)) >> 8` is
wrong by up to 1/255 because 255·255 = 65025 overflows a 16-bit intermediate. Use
`>> 8` on a `u32` intermediate, or `((fg*a) + (bg*(255-a)) + 127) / 255`. The PRD's own scalar
fallback has the same bug; the test asserts a 50% alpha pixel lands within 1/255 of the
exact blend.

Surface tree: rects, text runs, icon masks. Icons are hand-authored 1-bit masks compiled into
`.rodata` — no SVG runtime, no font parsing for UI chrome. Damage tracking accumulates a
dirty-rect union per frame; only that union is touched.

**Gate.** `cargo test -p holonomy-render`: blend accuracy vs an exact reference, damage-rect
union correctness (typing at row 420 touches rows 420–436 and nothing else), and a
`HeadlessScanout` PPM dump of a known frame committed as a fixture.

### Phase 6 — Text engine and geometry

CAGR leaves: 4096-byte `SecureBlock`s, 64-byte aligned, `gap_start/gap_end/text_len`.
O(1) insert into the gap; delete shifts the boundary and scrubs the byte. Split/merge across
leaves. Style spans as a parallel interval map of the PRD's 16-byte `TextIntervalSpan`.

Geometry: port `Fenwick` from H2 with `u32` weights. Two trees — line heights and byte
prefix sums (FR-1.3). Line height comes from font ascender/descender, not from measurement.
Apply H2's `scroll_compensation` rule. Re-derive the section/split threshold by measurement in
this phase, and write the number down.

Undo: one 500-entry in-memory stack, one for the document.

**Gate.** `cargo test -p holonomy-text`: insert/delete O(1) with no allocation (asserted by
a counting global allocator), 2000-page document edits within latency budget, span
consistency across splits, undo depth. `cargo test -p holonomy-geometry`: port H2's 36
`geometry.rs` tests and the 4 `Fenwick` tests, re-targeted at the new tree.

### Phase 7 — Sandbox

`unshare(CLONE_NEWNET)`, `prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0)`, then the measured seccomp
filter from §2.6. SIGSEGV/SIGBUS handler on a guard-page fault: zero registers and all
`SecureBlock`s, then `_exit(137)`. Teardown path: zeroize every buffer, overwrite the ring
buffer with noise, `DRM_IOCTL_MODE_DESTROY_DUMB`, `_exit(0)`.

Ordering is load-bearing: `mlockall` → open container → allocate everything → open DRM →
open evdev → `unshare` → `no_new_privs` → seccomp. Nothing after seccomp may allocate.

**Gate.** `cargo test -p holonomy-jail`: the trap-mode syscall census; the derived allowlist;
then the same session re-run under `SECCOMP_RET_KILL_PROCESS` and asserted to complete. A
guard-page fault test asserts exit 137 and a zero-length core file.

### Phase 8 — Editor, surface, export

Assemble the binary and the session loop. Input: `EvdevSource` and `ScriptedInputSource`
behind the `InputSource` trait. Keymap with `code`-based matching (H2's rule — `key` is
layout-dependent, `code` is not). Chrome: document tabs sidebar, toolbar, ruler, page canvas,
zoom, menus — laid out from the screenshots in `Plan/`, drawn by Phase 5's surface tree.

Export: streaming HTML from CAGR leaves + span map, and PDF via `pdf-writer`. Both write to a
pre-opened fd (the jail has no `open`).

**Gate.** `cargo test -p holonomy-export`: HTML escapes correctly and applies spans; PDF opens
and its text extracts to the source. Full-session integration: load a container, type a
sentence, scroll, undo, export, save, reopen — verified against a scripted event stream, with
a PPM dump of the final frame.

### Phase 9 — Complex media and structured layout

Tables, inline math, and viewport-bounded images. §2.9 is the budget and the two measured facts that
constrain this phase; read it first, because two parts of the original specification turned out not to
be implementable and were changed rather than attempted.

Worked in three parts, each with its own gate, because they share the budget and nothing else.

#### 9A — Tables

An integer cell matrix, not a web-style reflowing table model.

```rust
pub struct TableSpan {
    pub rows: u16,
    pub cols: u16,
    pub col_widths: [u16; 8],   // in character cells
}
```

**Eight columns maximum**, a compile-time constant with a test, because `col_widths` is a fixed-size
array and a variable column count would mean a second representation with a conversion between them.

* **Representation.** `TableSpan` lives in `holonomy-text`'s span map alongside `TextIntervalSpan`, so
  the existing undo machinery covers table edits with no new code. Cell contents are byte ranges in the
  CAGR buffer separated by **U+001F UNIT SEPARATOR** — chosen because it is a C0 control character,
  which is exactly what §5's Phase 4 text window (`0x20..0x100`) excludes from the atlas, so it can
  never be mistaken for a glyph and never widens a font subset.
* **Borders.** `┌ ┬ ┐ ├ ┼ ┤ └ ┴ ┘ ─ │`, all ten verified in Phase 4's table
  (`0x2500..=0x257F`, re-asserted for exactly these ten). Drawn through the existing procedural arm
  table, never as `+`, `-` and `|`, which are monospaced *text* and leave a one-pixel gap at every cell
  boundary. Widths and padding are integer-only; a table's total width is
  `sum(col_widths) + (cols + 1) * pad`, asserted equal to the page measure.
* **Navigation.** Tab advances to the next cell, Shift+Tab to the previous, both wrapping at the ends.
  Enter inserts a newline *within* a cell without disturbing geometry. Arrows cross cell boundaries via
  `holonomy-geometry`'s Fenwick mapper, which already maps offsets to rows and columns.

**Gate.** A 4×3 table with populated cells, navigated with Tab and Shift+Tab across every boundary,
edited in place; borders asserted to land on exact integer pixel coordinates — not "within a pixel".

#### 9B — LaTeX math

A micro-parser and a procedural layout. **No TeX engine** — one would be several megabytes against a
2.0 MiB gate, and §2.3 already rejected Typst for the same reason.

```rust
pub enum MathNode {
    Text(String),
    Symbol(u16),
    SuperSub { base: Box<MathNode>, sup: Option<Box<MathNode>>, sub: Option<Box<MathNode>> },
    Fraction { num: Box<MathNode>, den: Box<MathNode> },
    Sqrt(Box<MathNode>),
    Row(Vec<MathNode>),
}
```

Syntax scope: `^`, `_`, `\frac{}{}`, `\sqrt{}`, Greek letters, `\sum`, `\int`, and bracket sizing.

* **Drawing is procedural.** Fraction bars are 1- and 2-pixel horizontal fills via direct scanout blit.
  The radical is a procedural tick plus a stretched overbar — §2.9.2 point 4. Symbols come from the new
  Noto Sans Math atlas window.
* **Editing is a mode switch, not a separate buffer.** With the caret outside the formula the box
  renders as compiled math. With the caret inside, it expands in place to show the raw LaTeX with
  syntax styling and recompiles on blur. Expansion is one line of height per `\frac` level, computed
  in integers.
* **No allocation after the AST is built.** Layout writes into a caller-supplied scratch buffer.
  `MathLayout::measure()` is `const`-callable and the gate asserts the allocation count is zero from
  `layout()` onward, which is the same counting-allocator discipline Phase 6 established.

**Gate.** Parse and render `\frac{-b \pm \sqrt{b^2 - 4ac}}{2a}`. Assert the bounding box against
hand-computed dimensions, and assert zero allocations between `MathNode` and the finished frame.

##### 9B, delivered — what was built and what the measurements said

Delivered in two commits' worth of work: the parser and layout first (`3a93b68`), then the asset
rework and the session wiring. Four things in this section turned out to be wrong as written, and each
correction is recorded here with the number that corrected it rather than quietly in a commit message.

**The math face rasterises at boot, and the budget paid for it by shrinking.** §2.9.2 point 1 planned a
fifth face; the first implementation *skipped* it at boot and rasterised on demand, because a 5-style,
773-codepoint, 2-size table is 77,300 B and 491,520 of coverage plus that is 568,820 against the
524,288 ceiling. On-demand was the wrong answer for a reason that has nothing to do with bytes: it is
**curve evaluation at runtime**, which §2.9.5 forbids outright, and a cache in front of it is the same
violation with bookkeeping. No allocation-counting gate can see that, which is exactly why it is
written down here. The face now rasterises at boot, and the cost was paid in the budget:

| | before 9B | after 9B |
|---|---|---|
| `MATH_RANGES` codepoints | 421 (four whole Unicode blocks) | **108** (exactly what `SYMBOLS` names) |
| math glyphs | 507 | **160** |
| packed payload | 78,354 B | **55,886 B** |
| `STYLE_COUNT` | 4 | **5** |
| metric-table codepoint windows | 2 | **8** |
| `CODEPOINTS` | 352 | **464** |
| `ATLAS_HEIGHT` | 480 | **448** |
| atlas + table | 519,680 (99.1%) | **505,152 (96.4%)**, 19,136 spare |
| ink occupancy | 76.2% | **83.7%** |
| boot to ready (release) | 41.6 ms | **34.0 ms** |

The height had to drop because the table is linear in both the style count and the window count: at 480
the pair is 537,720, which is 102.6% of the ceiling. **The math face was paid for out of the coverage's
slack, not out of headroom that existed** — the pair got *less* full while every one of its parts grew.
A sixth style fits at no height.

The operators are **four** windows, not one. A single 0x2200..0x22C6 span cost 198 slots, 146 of them
unreachable, for **14,600 B of table**. Three extra comparisons in the least-taken path in the renderer
bought that back. `metric::slot_of`'s neighbours show the spans; `metric::codepoint_of` is the inverse,
added because a test that inverted the layout itself was silently wrong the moment the layout grew.

**Boot got faster, which was not expected.** Pruning removed 22,468 B of compressed payload and 67
glyphs' worth of rasterisation, so the 34.0 ms is 7.6 ms *under* the 41.6 ms this phase started from.

**The parser's symbol table is the payload's coverage definition.** `tools/build_font_payload.py`
derives `MATH_RANGES` from what `holonomy_render::SYMBOLS` can name, and
`crates/holonomy-render/tests/math_coverage.rs` asserts the two agree in both directions — no symbol
outside the declared ranges, no symbol missing from the face. The first version listed Unicode *blocks*
"because a partial range in a subsetter produces a `cmap` with holes", which was true and became the
wrong answer once the face rasterised at boot: then every listed codepoint is coverage *and* a metric
slot whether or not a formula draws it.

**Two bugs the gates could not see, found by rendering the frame and looking at it.**

1. `emit` put a superscript and a subscript at the *same x*. `measure` reserved `max(sup, sub)` — one
   script's width, side by side — so every box dimension was correct and the pixels overlapped. It was
   invisible on the fixed 8 px grid, where a one-character script is exactly 5 px wide and two scripts
   at one x cannot overlap, and every existing test laid out `b^2`, which has no sibling script.
   `a_subscript_sits_beside_the_superscript_not_under_it` in `tests/math.rs` is the gate.
2. The layout sat on the page's 8 px text grid while the fonts are proportional. Measured at 16 ppem:
   Inter Italic advances 9–10 px, JetBrains Mono 10, and `\sum` **14**. So glyphs overlapped by 1–2 px
   and `\sum_{i=0}^{n} i` rendered as `∑ in0i`. `MathMetrics::advance` now takes an optional
   per-codepoint advance; `None` keeps the fixed grid, which is what the hand-computed gate asserts and
   **its expected numbers did not move**. `emit_math` supplies the real one.
   `MathMetrics` lost its `PartialEq`/`Eq` for this: rustc is right that comparing
   `Option<fn(u32) -> u32>` fields is meaningless, and two metrics differing only in advance function
   would have compared *equal*.

**The remaining fidelity limit, stated rather than left to be discovered.** The renderer is a **fixed
8 px cell grid**: `Painter::text` calls `blit_coverage` with `cell_width()` = `ppem / 2` rather than
`GlyphMetric::width`, so every glyph wider than 8 px has its right-hand columns clipped — 9–10 px for
most letters, **14 px for `\sum`**, which loses 6 of them. For body text this is masked, because runs
advance by the same 8 px so the next glyph covers the clipped part. For a formula it shows, because the
layout now advances by real advances while the painter still blits a cell. The fix is one argument, and
it is **not** taken here because it changes the ink of every glyph on the page — a visual-baseline
change for the whole product, not a 9B one. It is first on the 9C list.
`a_formula_laid_out_on_real_advances_is_wider_than_the_fixed_grid_model` pins the exact geometry (28 px
for three letters) so the two numbers cannot drift apart unnoticed.

**Editing is a mode switch on the caret, and the delimiters are document bytes.** `Ctrl+M` inserts
**four** bytes — `$$$$` — and leaves the caret at `start + 2`, so the formula exists the instant the key
is pressed rather than depending on the user remembering to close it. Spans are derived by scanning for
`$$` pairs (`holonomy_text::math_span`), which is the opposite of 9A's table and the reason is worth
stating: a table's shape is **not** derivable from its bytes (2×3 and 1×6 are the same six separators),
whereas a formula's span is, so undo, save, load and export need no special case.

An unpaired `$$` runs to the end of its line rather than vanishing — dropping it would make a
half-written formula blink out of existence mid-edit. That forced a `closed` flag on `MathSpan`: the
first version computed `inner()` as `start + 2 .. end - 2` unconditionally and silently ate the last
two bytes of every half-written formula.

**A caret move is a repaint, and it was not one.** `caret_to` set no damage and `tick` only paints when
damage is non-empty, so pressing an arrow moved the caret in the model and left the pixels alone — the
old cell stayed drawn and the new one waited for the next blink toggle. Every scripted gate missed it,
because they assert on `Caret::locate`'s arithmetic and never on a frame after a bare movement. The live
window found it in the worst possible way: leaving a formula with Right changed every number in
`SessionStats` *except the ones the frame was drawn from*, so the window kept showing raw source while
the log said the caret was outside. `caret_to` now damages both the old and the new caret cell.

**Live verification.** `crates/holonomy/examples/xtype.rs`, single invocation so the XTEST grab is taken
once:

```text
$ xtype 'ctrl-m \frac{-b \pm \sqrt{b^2 - 4ac}}{2a} right right' ctrl-q
holonomy: math: 1 inserted, 1 compiled and 0 raw in the last frame, 3 procedural fills, 0 parse errors
holonomy: formula at 0..=34 is "\frac{-b\pm\sqrt{b^2-4ac}}{2a}"
holonomy: caret is not inside a formula

$ xtype 'ctrl-m \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}' ctrl-q
holonomy: math: 1 inserted, 0 compiled and 1 raw in the last frame, 0 procedural fills, 0 parse errors
holonomy: caret inside a formula at bytes 0..=34 (closed)
```

Three procedural fills is the right answer and not a typo: the fraction bar, the radical's overline and
its tick. The second run is the same document with the caret left inside, drawing source and no bars —
drawing a fraction bar over the literal text `\frac{1}{2}` would be a lie about what the document says.

Getting there took **three harness bugs**, all of which had produced a plausible-looking wrong answer:
`38 + (c - 'a')` is not a letter keycode table (the letters are not contiguous; `\frac{1}{2}` arrived as
`uhvad23`), `keycode_for_keysym` matches only a keycode's *first* keysym so every `}` was dropped, and
shift was *tapped* rather than held across the key. The middle one is a real API gap, not a harness
one: `XTest::keycode_for_keysym_shifted` now returns the keycode **and** whether shift is needed, since
two lookups get the keycode but leave the shift flag to guesswork. The third is the same trap this
harness already documents for Ctrl.

**Cost.** Default release binary 1,101,944 B against the 2,097,152 ceiling — **995,208 B of room**, up
69,472 B from before 9B. The `--features desktop` build is 1,192,824 B, 904,328 B of room. Payload
55,886 B of an 81,920 B budget.

#### 9C — Iceberg media cache

Inline images, without breaching 16.0 MiB on a 2000-page document. §2.9.3 has the arithmetic that
dictates the design; this is the mechanism.

**Before the images: one line of the renderer.** `Painter::text` blits `cell_width()` columns rather
than `GlyphMetric::width`, so the whole product draws on a fixed 8 px grid and clips every glyph wider
than that. 9B measured it (9–10 px for most letters, 14 px for `\sum`) and left it, because fixing it
changes the ink of every glyph on the page and therefore every visual baseline — a change that wants its
own phase rather than riding along inside a media one. It is here because it is the largest known
correctness gap in the renderer, and because 9C's decoder and scaler are also "measure the real width
instead of the assumed one" and doing both together is cheaper than doing them apart.

* **Decoder.** Hand-written PNG chunk reader plus `miniz_oxide` for inflate. Budget 60 KiB of binary,
  asserted by the size gate.
* **Storage.** Images are encrypted chunks in the `.wavefunction` payload section, addressed by a
  32-byte BLAKE2b `AssetId`. They are read with `pread64` on the container fd, which is allowlisted —
  **no `openat` after sealing**, so an image path must never exist at runtime.
* **Cache.** `IcebergCache` holds decoded, **page-column-width** RGBA rasters (§2.9.3). Policy: only
  rasters intersecting the viewport ± 1 page exist in decoded form. Eviction calls
  `SecureBlock::zeroize_and_release()` — synchronously, so RSS falls before the next frame, and
  observably, so the gate can assert it.
* **Scaler.** SSE2, fixed-point bilinear, integer-only. It runs on *every* image because every image
  is a downscale, which is the only way a scaler gets honestly tested.

**Gate.** A document with 10 distinct images, scrolled from page 1 to page 50, with the counting
allocator asserting **decoded image memory never exceeds 8.0 MiB** and that every evicted raster was
scrubbed to zero. Binary ≤ 2.0 MiB.

**Phase 9 gate.** `cargo test -p holonomy-render` plus the integration harness, plus all of §6.

##### 9C delivered — 2026-10-06

The gate passes. Four measurements, then the four things that were not as specified.

**Cost, measured on this host.** Default release binary **1,439,288 B** against the 2,097,152 ceiling:
**657,864 B of room**, down 336,704 B from 9B's 1,101,944 B. `--features desktop` is **1,536,472 B**,
560,680 B of room. The decoder's own cost, attributed by symbol size out of an unstripped build
(`strip = "none"`, so `nm -S` can see it, grouping each symbol to the innermost non-`std` crate in its v0
mangled path): `holonomy_image` 26,388 B + `miniz_oxide` 26,426 B = **52,814 B = 51.6 KiB**, inside
§2.9.1's 60 KiB. For scale, the same measurement puts `holonomy_assets` at 69,145 B, `ttf_parser` at
62,944 B and `brotli_decompressor` at 178,743 B -- so the decoder is not the largest thing the font
payload costs, and 9C's 336,704 B is the *whole* phase: decoder, payload serializer, catalog, export
path and session plumbing.

**The frozen container held, and that is now a measurement rather than a hope.** Nothing outside the
payload has an opinion about a payload byte: `MasterFrame` records only `content_len` and the container
chops it into 65,520-byte chunks by offset. `crates/holonomy-container/tests/commit_then_read.rs` is
untouched by 9C and still passes. Assets went into the payload's tail:

```text
[Doc Header 16B][Text & Spans][Table & Math States][Asset Catalog Header: count u32]
[Asset: Blake2b 32B | w u16 | h u16 | len u32 | PNG]*
```

Which meant writing the first document serializer rather than appending to one -- the payload *was* the
UTF-8 and nothing else.

**Four things were not as specified, and the specifications are amended here rather than quietly met.**

1. **§2.9.5's "resolved at export time from the container" is not what the exporters do, and cannot
   be.** They read `Editor::assets()` -- the payload's catalog -- which is always present. There is no
   `pread64` on the container fd in the export path, and there cannot be: the session has no container,
   so "read it from the container fd" names a thing the export path does not have. The *rule* §2.9.5
   states is honoured exactly and the *mechanism* it names is not implemented: neither exporter can
   reach `IcebergCache`, which is asserted structurally by the fact that `export` takes an `&Editor` and
   nothing else. **Amended §2.9.5 accordingly.**
2. **An image's position is a character.** U+FFFC OBJECT REPLACEMENT CHARACTER in the document's own
   bytes; the *n*-th anchor is served by the *n*-th catalog entry. This was forced: the frozen
   per-entry shape has nowhere to record which anchor an asset belongs to, so the pairing is positional
   whether it is wanted or not. Everything else falls out -- edits slide the anchor for free, and save
   and load need no new payload section. The cost, stated rather than glossed: images cannot be
   reordered without rewriting the text.
3. **Ctrl+I inserts a committed PNG, not a file the user chose.** `crates/holonomy/assets/test-chart.png`,
   1920x1080, `include_bytes!`-ed. FR-5.1's `unshare(CLONE_NEWNET)` forbids the network and the sealed
   50-syscall allowlist has no `openat` on a user path, so "insert image from disk" is a Phase 13
   question. Everything either side of that is real end to end, and the chart is 1920x1080 precisely so
   the scaler runs, because a fixture at or below the column width never would.
4. **At the product's own ratio the scaler is a decimator, not an average.** §2.9.3's headline
   arithmetic is 1920 -> 640: **exactly 3:1**. `axis_map` places destination pixel `i` at
   `(i + 0.5) * src/dst - 0.5`, which at `src/dst == 3` is `3i + 1` -- an exact integer, so every
   interpolation weight is zero and bilinear reduces to nearest. The worse general fact is that bilinear
   reads *two* adjacent source pixels, so at 6.86:1 a hard edge produces no intermediate values at all.
   `a_downscale_averages_the_pixels_it_covers` was a 2x1 -> 1x1 case, which is the only integer ratio at
   which the two samples are guaranteed to straddle everything between them. `axis_map` is right and its
   pixel-centre convention is pinned; an **area** filter is the correct answer for the downscale path and
   is **the first thing to change if the images look crunchy**. Recorded in §8. Two tests pin both
   halves so this cannot change silently, and the chart fixture was rebuilt twice with detail at the
   *destination* pixel scale before the test could see it at all.

**Three defects found and fixed on the way, all of them silent.**

* **`Editor::delete_range_in_rope` could not delete a multi-byte character.** Undoing an insertion
  walked forward one byte at a time, re-seeking the cursor to `offset + 1` before each delete, which
  lands mid-character on the second byte of anything wider than one. Six phases did not notice because
  every byte offset any test touched was ASCII. Ctrl+I hits it on its first keystroke. Nine tests in
  `crates/holonomy-text/tests/multibyte_undo.rs`; reverting the fix fails all nine.
* **The anchor-to-asset pairing was not maintained.** Entry `i` serves anchor `i`, so deleting an anchor
  without its asset renumbers every later anchor and each then shows the picture that used to be one
  higher -- invisible while the images are identical, which they are in the fixture, and a **wrong
  picture** the moment they are not. The payload round trip cannot catch it either: both the text and
  the catalog are individually valid and mutually inconsistent, so the AEAD tag is valid and the payload
  decodes. `AssetCatalog` is now maintained at the same five sites `TableMap` is, with an `undo_assets`
  shadow. The first version's doc comment claimed none of that was necessary and it was wrong; the claim
  is quoted and retracted in `AssetCatalog`'s own header rather than quietly deleted.
* **`Painter::image` mixed `/255` and `>>8`.** Red divided by 255, green and blue shifted right by 8 --
  the standard fast blend, off by up to 1 in 255 on *every* pixel, so an **opaque** pixel came out as
  (10, 19, 29) where the source said (10, 20, 30). An opaque blit has to be the identity.

**Two pre-existing flakes, both now deterministic.** `no_alloc.rs` caught a full-document `Vec` per
delete introduced by the obvious way to find an anchor's ordinal; the prefix is now counted in chunks
through `Rope::read_at`, so a document with a thousand images deletes a character as cheaply as one with
none. `sp800_22.rs`'s `a_single_flipped_bit_is_below_the_floor_of_these_tests` asserted
`p_value < 1.0` on a *random* monobit count -- a coin flip, since the p-value is 1.0 exactly when the
flipped bit moves the count toward the mean. It passed 500-odd runs and then failed one, several crates
away from anything that could affect it.

**Where 9C stands against §6.** Binary ≤ 2.0 MiB: **657,864 B of room**. Decoded image memory ≤ 8.0 MiB
over a page-1-to-50 scroll: asserted, and the resident raster is the 640x360 one, not the 7.910 MiB
native one that §2.9.3's arithmetic says would fit exactly once. Evicted rasters scrubbed: asserted
through a pointer captured before eviction. Zero-Bézier Invariant: intact -- the scaler is integer
fixed point, the radical and the box-drawing arms are procedural, and `Asset::dimensions` reads the
`IHDR` rather than evaluating anything.

**Not done, and named.** `Painter::text` still advances by `cell_width()` rather than the glyph's own
advance; 9C's commit message calls it and Phase 12 owns it. A document image's alpha is composited onto
white in the PDF path rather than carried as an `/SMask` -- a budget decision, stated in
`holonomy-export/src/asset.rs`, and the first thing to change if a document needs a transparent image
in a PDF.

---

### Phase 9X — The developer window (not a product path)

Added 2026-10-04, after the Phase 8 gate. As of 2026-10-05 it is also the **product's** presentation
path, not only a development convenience — see §5. It exists so a person can type a document on an
ordinary desktop without `sudo`, and it is behind the `desktop` feature, which is off by default.

The reasoning is in §8's amendment, but the shape is: a hand-written X11 core-protocol client in
`crates/holonomy-x11` (only `libc`; no `minifb`, no `softbuffer`, because both reach X11 through
`x11-dl`'s `dlopen`, which a static musl binary cannot do and the Zero-Compositor Invariant forbids), a
`Desktop` backend implementing the existing `Scanout` trait including the new `present_damage`, and a
`holonomy-input::x11key` that turns an X key event into an `InputEvent` by subtracting 8 — the same
offset `holonomy_input::Keymap` already assumes, checked against 23 keys on a live server.

**Gate.** `cargo test -p holonomy-x11`, plus `HOLONOMY_X11_LIVE=1 cargo test -p holonomy-x11 --test live`
against a real server: a byte-identical 1,024,000-pixel round trip, a 4 MiB frame in 16 chunked requests,
23 keycodes equal to `KEY_* + 8`, an `Expose` after `MapWindow`, and both events of two synthesised
taps arriving in order. `crates/holonomy/tests/release_artifact.rs` asserts the default binary holds
none of it: measured 1,032,472 bytes with zero of five X11 marker strings, against a desktop build of
1,116,536 — a difference of exactly 84,064 bytes.

---

### Phase 10 — Removed

Removed on 2026-10-05, with the reasoning at the head of §5. There is no target-hardware run.

### Phases 11–14 — Added 2026-10-05, after the Phase 9C gate

Four phases, added rather than folded backward, because each one answers a question that the Phase 0–9
gates were never asking. Phases 0–9 built the *parts*: a text engine, a renderer, a container, a jail, a
window. What was never assembled is the *product* — a document is never loaded from the container, no
document body text is ever drawn, and the per-keystroke path is superlinear in document length. The
audit that motivated these phases is below, and it is worth reading because most of it is arithmetic
rather than measurement, which is exactly why Phase 11's first act is to replace the arithmetic with
numbers.

The ordering is **speed before UI**, and it is load-bearing rather than stylistic. A menu bar is a dozen
clickable rectangles; it is also a way to make the application's cost model visible to a person, and
there is no value in making a fast-looking shell demonstrable around a 300×-over-budget edit path. Phase 14
therefore comes last, and Phase 11 comes first.

#### The audit that produced these phases

**Nothing in the product loads a document.** `main.rs:208` and `main.rs:242` construct `Editor::new()`
— an empty editor. The container is opened as a raw fd (`main.rs:247`), the passphrase is read at
`main.rs:276` and discarded at `main.rs:295` (`let _ = &phrase;`). The only code path that goes
container → editor is `crates/holonomy-jail/examples/census_session.rs:308-311`. And
`Wavefunction::read_content` (`crates/holonomy-container/src/lib.rs:344`) is not a paged read — it
streams every chunk through the 192 KiB ring and then concatenates the whole document into one `Vec`,
so loading is fully resident with a transient 2× peak at open.

**Nothing draws document body text.** `Chrome::tree` documents it: *"draws no document body text at all
— the page behind the chrome is blank"* (`crates/holonomy-render/src/chrome.rs:445-448`). `Painter::text`
is private and rasterises a contiguous codepoint run rather than bytes read from a document. There is no
function anywhere that takes a document line and paints it.

**Every keystroke copies the entire document, eleven times over.** `Session` calls
`self.editor.text()` at eleven sites (`crates/holonomy/src/session.rs:379, 391, 450, 821, 926, 1044,
1054, 1093, 1165, 1200, 1350`), and `Editor::text` → `Rope::to_vec` allocates a `Vec` the size of the
document (`crates/holonomy-text/src/rope.rs:428`). `Rope::read_at` fills it byte-at-a-time
(`rope.rs:418-420`), so each copy is a per-byte call rather than a memcpy. Three of those sites are on
the caret path itself — `line_start` (`session.rs:1043`) and `line_index` (`session.rs:1053`) are called
from every `caret_to`, and `refresh_counts` (`session.rs:1092`) then makes three more full passes for
bytes, words and lines.

The consequence, derived rather than measured: at a 6.4 MiB document, one keystroke moves on the order
of 40 MiB and touches 6.7 M per-byte accessor calls, repeated eleven times. The `no_alloc.rs` gate is
green through all of this because it drives `Editor` directly and never constructs a `Session` — so
"no heap allocation while editing" is currently asserted about a code path the product does not use.

**The geometry engine is built, proven, and disconnected.** `LineGeometry` — the two Fenwick trees,
`O(log n)` `y_of`/`line_at`/`byte_of`/`line_of_byte`, with exact-inverse tests at 60,000 lines — appears
only in `holonomy-text/tests/latency.rs`, `holonomy-geometry/tests/h2_port.rs`, and one jail example. It
is in no production path. `Session` uses `ChromeState::scroll_line` plus a newline count.

**The 16.0 MiB gate has never been measured.** There is no `statm`, no `/proc/self/status` and no
`getrusage` anywhere under `crates/`. §6's row "steady-state RSS ≤ 16.0 MiB with a 2000-page document
open" is arithmetic, not a test. The arithmetic, corrected against the source: §2.9.4's table
understates CAGR text by ~3.7× (it says ~2.0 MiB for "CAGR text + span map"; measured leaf data alone is
6.82 MiB at the full budget, `crates/holonomy-text/tests/latency.rs:368`) and overstates the container
ring by ~16× (it says ~3.0 MiB; `crates/holonomy-container/src/io.rs:39-48` allocates 3 × 65,536 =
0.188 MiB), and it omits the 3.906 MiB framebuffer (`crates/holonomy-display/src/frame.rs:132`). §2.9.4
is left as written because it is the record of what was believed at the time; **the corrected
derivation is in Phase 11 and it is the number that governs** — and Phase 11 then measured it, which
found *three* further omissions in §2.9.4's table. See "the 16.0 MiB gate, measured" below.

#### The 16.0 MiB gate, measured — `crates/holonomy/tests/session_rss.rs`

**The measurement, on a 6.00 MiB document (the largest this host can page-lock), typed into and
painted:**

| consumer | MiB | what it is |
|---|---|---|
| framebuffer | 3.91 | `Session::frame`, 1280 × 800 × 4 |
| scanout copy | 3.91 | `HeadlessScanout::last` — **a second framebuffer** |
| leaves | 6.40 | 1,639 × 4 KiB page-locked |
| `doc_scratch` | 6.00 | **a second contiguous copy of the whole document** |
| geometry | 3.27 | 142,989 lines × 24 B |
| atlas | 0.46 | |
| unattributed | 2.89 | binary `.text`/`.rodata`, stack, allocator |
| **total** | **26.82** | budget 16.0 |

**So the budget is not met at 6 MiB, and the crossover is 2.21 MiB.** The gate asserts the affordable
document is at least 1.5 MiB — a ratchet set 32 % below today's number, which catches a regression
that *raises* the per-document-byte cost and deliberately cannot catch a budget that has been quietly
widened. The measured marginal cost is **2.851 resident bytes per document byte**.

**Three more omissions in §2.9.4's table, found by measuring rather than adding up.** It omitted the
scanout's second framebuffer (3.91 MiB), the `doc_scratch` buffer (a whole second copy of the
document, 6.00 MiB), and the line geometry (3.27 MiB). §2.9.4 said 12.4 MiB; the truth is 26.8 MiB, so
**it was wrong by more than 2×, having been right about nothing that mattered at this scale** — every
number in it was correct about its own subject and none of the subjects were in the table.

**Phase 11's own geometry budget was wrong by 1.8×, and it is corrected here.** PROJECT.md budgeted
1.83 MiB for the Fenwick trees, derived at 60,000 lines — a figure inherited from `LineGeometry`'s
test corpus, which uses short lines. At 44 bytes per line a 6.4 MiB document is 152,000 lines, so the
projection is **3.33 MiB**. Same failure as the original: a line count used at a scale it was not
measured at. `the_line_geometry_costs_what_phase_11_budgeted` now asserts the **per-line cost**
(24 B — two Fenwick `u32` weights plus one `LineMetrics`, four `u32` fields) and *reports* the
projection.

**What Phase 12 removes, and it is the largest single term.** `doc_scratch`'s 1.000 bytes per document
byte goes away when `Painter::text` draws from document bytes, taking the marginal cost to ~1.85 and the
affordable document to ~3.3 MiB. The geometry's 0.545 is the term that grows *worst*, being per line
rather than per byte — a document of longer lines costs less. **Both have to go for 2000 pages, and
only Phase 13's windowing removes the need for either to scale.**

**One subtlety worth recording, because it produced a confident wrong number twice.** An unpainted
framebuffer is **not resident** — `Frame::black`'s pages are untouched and the kernel does not fault
them in — so an RSS "baseline" taken before the first paint omits 7.81 MiB of framebuffer entirely.
Subtracting that baseline treats a 7.81 MiB cost as free, and the affine fit overshot by 3× (it claimed
7.63 MiB would fit). The fixed side is now built from *named consumers* rather than from a subtraction.
Separately, the floor comparison initially compared MiB against a byte constant — the same class of
mistake §2.9.4 made: arithmetic that is internally consistent and externally wrong.

**Two ceilings bind before RSS does, and they were not accounted for.** `S_MAX_PAYLOAD` is 8 MiB
(`crates/holonomy-container/src/layout.rs:85`), so the format caps plaintext at 8 MiB; 8 MiB of text needs
`8 MiB × 4096/3840 = 8.53 MiB` of `mlock`, and this host's `RLIMIT_MEMLOCK` is 8.00 MiB. **The
format's maximum document is therefore unopenable**, and the real ceiling is set by `mlock` occupancy,
not by RSS. `RLIMIT_MEMLOCK` is raised soft→hard by the boot chain
(`crates/holonomy-jail/src/rlimits.rs:138-155`, invoked at `main.rs:385`), which is why
`latency.rs:396` passes on this host and why it will fail on a host whose *hard* limit is 8 MiB.

#### Phase 11 — The keystroke path: one document, no copies

Make the product's edit path `O(edited line)` instead of `O(document)`, and replace §6's derived memory
numbers with measured ones. **Nothing about security, the container format, or the jail changes here** —
this phase makes an existing claim true rather than making a new one.

1. **A session-owned scratch buffer, and `read_into` everywhere.** `Editor::read_into`
   (`crates/holonomy-text/src/editor.rs:774`) already reads a byte range into a caller-supplied buffer
   without allocating. `Session` grows one scratch sized to the widest *visible region* rather than the
   document, and every one of the eleven `editor.text()` sites goes through it or through the Fenwick
   trees. The pattern is already in the tree: `with_table` (`session.rs:766-784`) holds a
   `table_scratch` and its doc comment states the cost of not doing so.
2. **`Rope::read_at` copies per leaf, not per byte.** The inner loop at `rope.rs:418-420` becomes one
   `copy_from_slice` against the leaf's data pointer. This is a change to the hottest function in the
   crate and it must be gated on byte-identical output, not on a benchmark.
3. **`LineGeometry` goes into the session.** `line_start`, `line_index` and `refresh_counts` become
   `O(log n)` Fenwick queries and incremental deltas instead of scans. `LineGeometry::damage_rect_for`
   already returns the repaint region a keystroke needs, and `Editor` already has a seam for attaching
   geometry (`crates/holonomy-text/src/editor.rs:123-125`) that currently returns `None` in the product.
   **Cost, stated up front:** two trees at 60,000 lines plus per-line metrics is ~1.8 MiB resident, which
   §2.9.4 budgeted at 1.20 MiB. That is charged against Phase 11's budget, not against 9's.
4. **Incremental counts.** Word and line totals are maintained as deltas at the edit site, with a full
   recount available as a repair path and a test that forces it.

**Gate.** A `#[global_allocator]` counting test that drives a **`Session`**, not an `Editor`, through
1,000 edits on a document of the largest prefix this host can lock, asserting **0 heap allocations** —
today's `no_alloc.rs:126-148` drives `Editor` and passes while the product allocates per keystroke.
Plus an RSS test that reads `/proc/self/statm`, loads the largest lockable document, and asserts
≤ 16.0 MiB, printing the per-consumer breakdown. Plus a latency test at **full document size**, not the
70 %-of-ceiling prefix `latency.rs:504` deliberately uses. Keystroke→pixel p99.9 ≤ 0.50 ms or the phase
does not pass. `RLIMIT_MEMLOCK`'s hard limit is asserted to cover the design document, so a host that
cannot open a 2000-page document fails here rather than at a user.

##### Phase 11, delivered, part 1 — the document copies are gone

The eleven `editor.text()` sites are down to zero whole-document allocations on the edit path, and the
copy itself got 30× cheaper. Five changes, each gated on output rather than on a timer:

1. **`Rope::read_at` copies per leaf, not per byte.** The inner loop was
   `for k in 0..take { out[written + k] = leaf.byte_at(w + k)? }` — one call per byte, so a
   full-document read cost 6.7 M calls. `CagrLeaf::copy_text_to` (`leaf.rs:759`) does the same work as
   two `copy_from_slice`s, because the gap splits a leaf's text into at most two contiguous runs.
   **Measured: 3.52 MiB in 387 µs — 9.51 GB/s**, from `a_full_document_read_is_bounded_by_bandwidth_not_by_per_byte_dispatch`.
   The byte-at-a-time figure for the same read is ~11 ms by arithmetic at 3 ns per call, so ~28×.
   Gated on byte-identical output by `a_bulk_read_matches_the_byte_at_a_time_reader`, which reads ranges
   that **straddle a leaf's gap** — the case a naive single `copy_from_slice` gets wrong and the
   per-byte loop got right for free.
2. **`line_start`, `line_index` and `refresh_counts` stream in 4 KiB chunks** through `read_into`,
   holding one `[u8; SCAN_CHUNK]` instead of the document. `refresh_counts` carries `in_word` across
   chunk boundaries, because counting words per chunk and adding undercounts every document whose
   words straddle a boundary — 4,096 bytes against a 64 KiB fixture is 16 crossings, so that is a wrong
   answer rather than a rounding one. Gated by `the_chunked_word_count_survives_a_chunk_boundary`.
3. **The five paint-path `editor.text()` calls share one buffer** (`Session::doc_scratch`). It is a free
   function taking `&Editor` and `&mut Vec<u8>` rather than a `&self` method, because a method
   returning `&[u8]` out of a `Session` field borrows *all* of `self` and every paint-path caller also
   needs `&self.chrome` and `&mut self.math_scratch` — five call sites became five borrow errors. The
   buffer grows to a `SCAN_CHUNK` multiple, because resizing to exactly `len` reallocates on the next
   keystroke.
4. **`apply`'s `Command::Insert` arm no longer allocates.** It was
   `c.encode_utf8(&mut buf).as_bytes().to_vec()` — a four-byte heap block allocated and freed on every
   keystroke. Found by the gate after items 1–3 were done, which is the gate working.
5. **`Session::dispatch` was split out of `handle_event`**, so the edit path and the paint path can be
   measured apart.

**`tests/session_no_alloc.rs` is the gate, and it is the answer to a question this audit raised:** how
did FR-1.2 stay green while the product allocated per keystroke? Because `no_alloc.rs` drives an
`Editor`, and the product's path is `Session::handle_event`. A gate that cannot see the code it is a
gate for is not a gate. The new file drives `Session`, on a 64 KiB document, and asserts **0**
allocations across 1,000 keystrokes — measured at **2,000 before item 4** and 0 after.

**Two honest limits, recorded rather than papered over:**

* **The paint path still allocates, and that is Phase 12's.** A paint builds a fresh `SurfaceTree`
  whose `Vec`s grow by doubling (192 → 384 → 768 → 1664 → 3328 → 6656 → 13312, measured by the file's
  size diagnostic) and the `HeadlessScanout` copies the whole 4 MiB frame. So the gate is split: the
  *edit* path is asserted at zero, and the *paint* path is asserted only to be non-zero —
  `the_paint_path_still_allocates_and_phase_12_owns_that`, which says so in its own name and becomes an
  assertion at zero when Phase 12 lands. Asserting zero on the combined number would have been false.
* **`line_index` is still `O(bytes before the caret)`.** The allocation is gone; the scan is not, and
  the fix is the Fenwick tree, which is item 3 below. At 3.5 MiB that is ~900 `read_into` calls, which
  is microseconds — but microseconds per keystroke is not the 0.50 ms budget, it is most of it.

**Cost: 2,624 bytes** — 1,439,288 → 1,441,912 against the 2,097,152 ceiling. The baseline is worth
correcting in place of the stale 1,101,944 in §9B's cost table: that figure predates 9C and 9X, and the
measured binary at `e89825c` was 1,439,288.

##### Phase 11, delivered, part 2 — the Fenwick trees, and the 122× that followed

Part 1 removed the allocations. This part removes the *scans*, and the headline number is:

| position in a 3.1 MiB document | median | worst | before |
|---|---|---|---|
| start | **106 µs** | 177 µs | **12,954 µs** |
| middle | **2 µs** | 58 µs | — |
| end | **1 µs** | 26 µs | — |

**`tests/session_latency.rs` is the gate, and it is a session-level measurement on purpose.**
`holonomy-text/tests/latency.rs` reports single-digit microseconds for `Rope::insert_byte` at 3.5 MiB,
and that number is real — it is just not the keystroke. The product's path is
`Session::handle_event → apply → after_edit → tick`, and `after_edit` did six things the text engine's
gate never sees. The new gate drives `Session`, at three caret positions, and reports the *edit* and the
*paint* separately because they are different claims.

**The diagnostic that found what was left is `print_where_a_keystrokes_time_goes`** (`#[ignore]`d), and
it is the most useful thing in this phase:

```text
     apply (whole keystroke):   4376.6 us/call
                caret_to:         0.1 us/call
          refresh_counts:  11477.9 us/call
             sync_lines:   3906.3 us/call
            tick (paint):    799.0 us/call
```

"12.9 ms per keystroke" says nothing actionable; "11.5 ms of it is the status bar's word count" says
exactly what to do. Three fixes, in the order the diagnostic named them:

1. **`LineGeometry` is wired in** (`crates/holonomy/src/doclines.rs`). `line_index` and `line_start` were
   `O(bytes before the caret)`; they are now `O(log n)` from the two Fenwick trees Phase 6 built and no
   production path used. `caret_to` went from microseconds to **0.1 µs**.
2. **`TextCounts` maintains word and line totals as deltas** (`crates/holonomy/src/counts.rs`).
   `refresh_counts` was the single largest cost in the keystroke. **A Fenwick tree cannot fix this one,
   and that is why it is worth writing down**: line count is a prefix sum, so a tree answers it exactly,
   but *word* count is not a prefix sum over anything — a word start depends on the whitespace on both
   sides of a byte, so "words before offset `o`" is not a function of `o` alone and cannot be made into a
   tree weight. Deltas are the correct shape: bytes before an edit are unchanged, so only the edited run
   and its two seams can move a word start. Typing a letter is `O(1)`; a 64 KiB paste is `O(64 KiB)`,
   which it was going to be anyway.
3. **`sync_lines` takes the line count as an argument** rather than reading the document to count
   newlines. This is the part that looks obvious only after the first version was measured at
   **3,906 µs** — a whole-document read inside the one function whose entire purpose is to remove
   `O(document)` work. The count was already maintained, by `TextCounts`. Passing it in turns the
   newline case into a comparison and everything else into one local scan.

**A real bug this phase found in itself, and where it surfaced.** `TextCounts`'s seam arithmetic
compared the *old* right seam against `byte_at(offset)` — which after an insertion is the run's *first*
byte, not the byte that used to follow it. Every insert into an **empty** document therefore netted zero
words. It surfaced as an **arithmetic underflow in a delete**, several keystrokes downstream, in a
different function, during the full-session gate — and the unit tests missed it because every one of them
started from a document that already had text, which is the only situation where the old and new seam
cannot differ by construction. `inserting_into_an_empty_document_counts_one_word` exists because of that,
and the story is in its doc comment because "my tests all passed" and "the gate caught it" are both worth
remembering.

**A second bug, found the same way, that a reader should not repeat.** `insert_math`, `insert_table`,
`insert_image` and `append_table_row` mutate the document *without* going through `Session::insert`, so
they stopped folding the counts. `insert_math` now folds them; the other three rescan, because the editor
does not hand back the bytes it wrote. All four are named in `recount_words_and_lines`'s doc comment,
because the failure mode is an underflow in a function that has nothing to do with the edit that caused it.

**Two gates that were failing before this phase, and are not now.** `holonomy-text`'s
`base_edits_stay_within_the_keystroke_budget` and `boot_to_ready_is_under_the_budget` both fail
intermittently on this host — **verified on the unmodified tree at `f19f1d7`**, so neither is a Phase 11
regression. The cause is the same in both: `RLIMIT_MEMLOCK` is per-*process* but the system's locked
pages are shared, so under `cargo test --workspace` a parallel test binary takes the headroom between a
bisection's "this loads" and its load. The first is fixed by letting the measurement back off and report
what it measured; the second is not fixed and remains a flake worth watching.

**Cost: 3,592 bytes** for part 2, 1,441,912 → **1,452,504** against the 2,097,152 ceiling. The geometry
tables are `Vec`s of `u32` and 3.1 MiB of document is 71,500 lines, so ~0.6 MiB of resident weight is
the honest cost of making the lookup `O(log n)`; PROJECT.md's §Phase 11 budgeted 1.83 MiB for it and
1.44 MiB is inside that.

**What is still `O(document)`, stated plainly:**

* **The paint path allocates and copies.** 164 µs at 64 KiB, 749 µs at 3 MiB — *not* linear in document
  size, which is the property that matters, and it is asserted as such by
  `a_paint_does_not_get_more_expensive_as_the_document_does`. Phase 12 owns it.
* **A newline is `O(document)`.** `LineGeometry::resize_lines` rebuilds both trees because a Fenwick tree
  supports point updates, not insertion, and it says so itself. Typing a letter is a point update;
  pressing Enter is a rebuild. One keystroke in forty.
* **`undo` and `redo` rescan.** `Editor::undo` returns an offset and a length but not the bytes, and the
  bytes are what the seam arithmetic needs. The alternative — threading them out of `UndoStack` — would
  put an undo-format detail into the counting path, where a format change becomes a counting bug.
* **The document is never 2000 pages.** `RLIMIT_MEMLOCK` is 8.00 MiB and every leaf is a page-locked
  4 KiB block, so a document is bounded by *occupancy*, not by RSS. 3.1 MiB is the largest this host
  holds. **Phase 13's windowing is what makes the full document reachable**, and until it lands the
  honest claim is "3.1 MiB", not "2000 pages".

#### Phase 12 — Draw the document

The page stops being blank. Body text is rendered from document bytes, wrapped to the page measure, and
scrolled through `LineGeometry`. This is the first phase that touches `Painter::text`, whose current
contract takes a *codepoint run* rather than *document bytes*; that contract is replaced, and every
caller is migrated.

Also here, because it is the same seam: the 9C item left open — `Painter::text` blits `cell_width()`
columns instead of `GlyphMetric::width`, clipping every glyph wider than the 8 px grid. `PROJECT.md:846`
records this as "the largest known correctness gap in the renderer." With real document text on the page
it stops being cosmetic, so it is fixed in this phase and not deferred again. The fixed-8-px-grid
assumption is pinned by `a_formula_laid_out_on_real_advances_is_wider_than_the_fixed_grid_model`
(`crates/holonomy-render/tests/math.rs`), and that pin moves deliberately, in a phase that is about
pixels.

**Gate.** A `HeadlessScanout` PPM fixture of a known document frame, byte-compared. Scrolling to a
known line index lands on the expected pixel row at 60,000 lines. A 2000-page document's page count
derives from `LineGeometry`, not from a constant. The clipped-glyph regression test: a `W`, a `∑` and an
italic `f` at 16 ppem each draw every column the font advances, asserted against `GlyphMetric`.

##### Phase 12, part 1 — the document is on the page

The page behind the chrome is no longer blank. `crates/holonomy/tests/session_body_text.rs`, 12 tests.

**`TextRun`'s contract was extended, not replaced**, and that is a correction to the paragraph above.
`TextRun` is `(first_codepoint, len)` — consecutive codepoints — which is exactly what a chrome label, a
box-drawing rule and a table border are. Document text is not that shape: it is UTF-8, so **consecutive
bytes are not consecutive codepoints**, and `TextRun` cannot express a multi-byte character without
lying about its length. Replacing the contract would mean every synthetic caller grew a document-byte
representation of a string that is not in the document. So `Node::DocText(DocRun)` is a second kind,
carrying a byte offset resolved through a `TextSource` — the same argument-for-pixels shape
`RasterSource` already is, and for the same reason: `holonomy-display` must not depend on `holonomy-text`.

**The advance is now the font's.** This is what PROJECT.md:943 named "not done, and named". §9B measured
real advances of 9–10 px for Latin letters and 14 px for `∑`; 9C fixed the *blit* to use the metric's
width; the *advance* stayed on the 8 px grid, so a 10 px glyph either overlapped its neighbour by 2 px or
left a gap. `a_glyph_advances_by_its_own_width_not_the_grid` is the regression test, and it is worth
reading: **the correct answer is *smaller* than the grid's** — `"mmm"` occupies ~29 px of ink against the
grid's 24 — so a test asserting "more than the grid" would have asserted the bug.

**Two more things were needed before the page was readable, and neither was in the plan.**

1. **Lines claimed by other emitters.** A formula, a table and an image are all inline in a line of
   prose. Without a claim list the body text and the formula were painted on top of each other — and
   **the only thing that noticed was two of Phase 9B's math gates**, whose ink-extent assertions read
   9 px wider. Neither gate was wrong; the page was. The claims are computed from the page buffer, which
   is sound because `for_each_math_span`'s own docs say an unpaired `$$` "runs to the end of its line" —
   every span lies inside one line.
2. **`total_lines` was stale at construction.** `ChromeState::default()` has `total_lines: 1`, and
   nothing called `publish_counts` at construction, so a session opened on a three-line document reported
   one line — in the status bar, where it was wrong and unnoticed because every gate started from an
   empty document or made an edit first. Phase 12 made it visible because the emitter clamps its loop to
   `total_lines`: **it drew one line and stopped.**

**A third thing was needed because I wrote the first version of the emitter wrong, twice.**

* `line_start(at)` takes a **byte offset** and answers "where does the line containing this byte begin".
  The renderer has a line *index* and wants "where does line N begin". Passing the index made every line
  report offset 0, so the page drew `one`, then `one\ntwo`, then the whole document. Both functions were
  correct; `usize` is `usize`. `DocLines::line_begin` is the index-shaped twin, and the trap is named in
  its doc comment.
* **A one-line read buffer cannot serve 43 runs.** A `DocRun` is resolved by the painter *after* every
  line is emitted, so the bytes a run names must still be present. One buffer per line would be correct
  for one run and silently wrong for the other 42. The buffer therefore holds **the whole page** —
  [`LINE_SCRATCH_BYTES`] = 48 KiB, sized from a constant rather than from `Layout::rows` so no panel size
  can resize it on the paint path.

**What did not land, stated plainly: the memory.** Phase 12 was supposed to remove `doc_scratch`'s
**1.000 resident bytes per document byte**, and it did not. `tests/session_rss.rs` still measures
**26.85 MiB with the same 6.00 MiB `doc_scratch`**, and its **2.85 bytes per document byte** is still the
true figure.

The reason is `publish_line_heights` → `math_blocks_for`, which calls `read_document`
**unconditionally**: it must, because `for_each_math_span` is a cursor over bytes and *"does this
document contain any math"* cannot be answered without reading the bytes to find the `$$`. **`Editor` has
no math accessor at all** — zero hits in `editor.rs`. A gate asserting `doc_scratch_capacity() == 0`
measured **221,184 bytes** and is now, in its own doc comment, the record of that failure.

So the honest statement of what Phase 12 part 1 delivered is **a page read, not a memory saving**: the
body-text emitter reads at most `rows × 1,024` bytes and is gated on that
(`the_body_text_emitter_reads_one_page_and_not_the_document`). The remaining work is one question in
`holonomy-text` — *does this document contain a formula, a table or an image?* — and once that exists,
`publish_line_heights`, `emit_tables`, `emit_math` and `emit_images` all take it and `doc_scratch` goes
to zero.

**Cost: 15,840 bytes** — 1,452,504 → **1,468,344** against the 2,097,152 ceiling. `Layout::rows` is
**23** at 1280x800, not the 43 first written, so a page is 23,552 bytes and 48 KiB is twice that.

**Not done:** wrapping. A line longer than the page's measure is **truncated**, counted in
`PaintStats::runs_truncated`. `LineHeights` is indexed by *document line*, so a wrapped line would have no
row to be on — `caret_line`, the scroll model, the table anchors and the formula anchors are all
document-line indices. Wrapping needs a document-line → visual-rows index, which is the same
"structure without content" problem Phase 13's section manifest is, and building it twice would be worse
than building it once.

#### Phase 13 — Sections: H2's iceberg for text

Make document length a function of the *window*, not of the process. This is the port of H2's
large-document strategy, and it is a port of the **idea**, not of H2's numbers.

**What H2 actually did, and what of it was Rust.** H2 measured its own soak at 1,014,000 words over
1,300 sections: peak 167 DOM nodes, 30 resident sections, 35 ms worst frame gap
(`H2/STATUS.md:377-390`). The mechanism was a structure-without-content manifest plus a bounded content
cache plus viewport-bounded mounting. The manifest and the Fenwick tree were Rust
(`H2/crates/holonomy-core/src/manifest.rs`, `geometry.rs`); **the scroll, hydration and virtualisation
hot path was TypeScript**, and H2 says so itself — there is no Fenwick tree anywhere under `H2/app/`,
the runtime geometry is a TypeScript prefix-sum mirror, and `H2/STATUS.md:23-29` states plainly that the
Fenwick tree "is not earning its keep on speed." So the portable insight is the *windowing discipline*,
which is language-independent; the fast part of H2 was its renderer, and here the renderer is Rust.

1. **A section manifest**, ported from `H2/crates/holonomy-core/src/manifest.rs` — a sorted vector of
   per-section metrics with no I/O, so a 2000-page document's structure is tens of kilobytes and touches
   no ciphertext. Adapt: serde derive off, `mark_count` becomes a count of interval-map spans
   (`PROJECT.md:388` already says this).
2. **Section size is re-derived by measurement here, and H2's 1500 words is not carried forward as a
   decision.** H2's `MAX_WORDS_PER_SECTION = 1500` (`H2/crates/holonomy-core/src/split.rs:17`) came from
   Chromium window-slide costs and Loro styled-read costs (`H2/spikes/m0-section-seam/FINDINGS.md:25-38`)
   — neither exists in H1. H1's binding per-section cost is section-height granularity against the
   8 px grid and the cost of a `pread64`, so the H1 optimum is expected to be *larger*. Phase 6's own
   instruction (`PROJECT.md:570-571`) already requires re-derivation; this is where it happens, and the
   number gets written down here the way every other measured constant in this file is.
3. **On-demand load and evict**, with eviction calling `SecureBlock::zeroize_and_release()` rather than
   `Drop`, for the same reason 9C requires it: eviction must be synchronous and observable.
4. **`RLIMIT_MEMLOCK` stops being the document-size ceiling**, because a document's text is no longer
   entirely page-locked at once. This is the phase that retires the Phase 11 §audit finding that "the
   format's maximum document is unopenable" — the answer is that a document is never fully resident, so
   `S_MAX_PAYLOAD`'s 8 MiB becomes reachable. **The container format does not change.**

**Two things H2 could not do, which H1 must solve and which are called out now rather than discovered:**

* **Cross-section selection.** H2 accepted this as a known limitation of its multi-instance strategy —
  dragging across a seam stops at the section edge, and a virtual selection layer was "judged not worth
  its maintenance and accessibility cost" (`H2/README.md:80-82`). H1 needs a selection model that spans
  section boundaries, because the alternative is a user-visible defect inherited deliberately.
* **Search.** H2 used SQLite FTS5 and, before it fixed the schema, rebuilt "the whole document — 1.33M
  words per keystroke" (`H2/STATUS.md:704-716`) — a scaling landmine H1 never inherits, since H1 has no
  database at all. H1's search must be built, and it must be built incrementally against an encrypted
  container with no database to lean on. It is scoped, not deferred.

**Gate.** Open a 2000-page document and assert a wall-clock open time, printed and gated. Scroll page 1
→ page 50 with the counting allocator asserting resident text stays bounded by the section budget, and
assert every evicted section was scrubbed to zero. Assert the manifest alone can size the scrollbar for
a document that has never been rendered. RSS re-measured ≤ 16.0 MiB with the images from §2.9.3 resident
too — **the 8.0 MiB image budget and a full document have never been asserted together**, and
`crates/holonomy-image/tests/scale_cache.rs:307` passes because it runs with no document in the process.

##### Phase 13, part 1 — the manifest, and the 6 MiB it removed

`crates/holonomy/src/manifest.rs` (items 1 and 2) and `crates/holonomy/tests/session_manifest.rs`, 9
tests. Items 3 and 4 — on-demand load/evict, and `RLIMIT_MEMLOCK` ceasing to be the ceiling — are not
started and say so at the end.

**The memory claim is met, and it is the whole of what this part is.** Phase 12 ended by recording that it
had failed: *"Phase 12 was supposed to remove `doc_scratch`'s **1.000 resident bytes per document byte**,
and it did not."* The reason was one missing capability, named in the code and in that note:

> `publish_line_heights` → `math_blocks_for` → `read_document` **unconditionally**, because
> `for_each_math_span` is a cursor over bytes and *"does this document contain any math"* cannot be asked
> without reading the bytes to find the `$$`.

`Manifest::span_total()` is that question, answered at open and maintained per keystroke. Four emitters now
guard on it. **`tests/session_rss.rs`, same host, same 6.00 MiB document:**

| | Phase 11/12 | Phase 13 part 1 |
|---|---|---|
| `doc_scratch` | **6.00 MiB** | **0.00 MiB** |
| total RSS | 26.85 MiB | **20.82 MiB** |
| marginal cost per document byte | 2.852 B | **1.852 B** |
| document the 16.0 MiB budget affords | 2.20 MiB | **3.40 MiB** |

**1.852 measured against 1.067 leaves + 0.545 geometry = 1.612 derived**, so the 15 % gap that Phase 11
attributed to "the rope's spine and `Vec` capacity slack" is now the *whole* of it — the third term is
gone rather than reduced, which is the check that says the removal is real and not an offsetting
regression elsewhere.

**Section size is 65,520 bytes, and the number is derived from the format rather than chosen.**
`CHUNK_PLAINTEXT = CHUNK_SLOT − TAG_LEN = 65,536 − 16`. Item 2 predicted "the H1 optimum is expected to be
*larger*" and that is what happened, for a reason the plan did not anticipate: the container addresses
plaintext as a slot array (`chunk_offset(omega, i) = omega + i·CHUNK_SLOT`,
`holonomy-container/src/layout.rs:149`), so a section that is one chunk is **one authenticated `pread64`
with no offset arithmetic in the load path**. A 64,000-byte section would straddle two chunks and need a
straddling check in the load path forever. 4 KiB — one rope leaf, the obvious candidate — would give 1,536
sections and 30,720 bytes of manifest for 6 MiB, and 1,536 reads to walk the document where 97 do.
`a_section_is_the_size_of_one_container_chunk` fails if the format's chunk size ever moves.

**The design decision that was *not* ported, and the arithmetic that forced it.** Item 1 describes the
manifest as sizing the scrollbar, which is H2's role for H2's sections. **H1's sections cannot have that
role.** A 2000-page document at 43 rows a page is 86,000 lines, 1,548,000 px of content; for a 600 px
scrollbar to move in sub-screen increments the manifest would need **2,580 sections**, which at 6 MiB is
**2,436 bytes per section** — smaller than a line of prose. H2's sections were *user content*: addressable,
reorderable, listed in an outline. H1's are an implementation detail, and a user who can see a section
boundary will ask for it to mean something. **So the scroll geometry stays on `DocLines` — per line,
`O(log n)`, exact — and the manifest is the residency index.** This is the same rejection of an H2 constant
that Phase 12 made about the 1500-word section, and it is the second time H2's *numbers* turned out to be
about a renderer H1 does not have.

**The manifest is 8 bytes per section, and it was 12 until the latency gate deleted a field.** Two
`Fenwick` trees of `n + 1` `u32`s; 97 sections of 6 MiB is **784 bytes, 0.0013 % of the text**. H2's own
figure is ~50 KB for 1,300 sections (`H2/crates/holonomy-core/src/manifest.rs:1-6`) = 38 B/section, so this
is 4.7x cheaper, for the reason that is visible in the two types: H2's entry carries a `String` id, a
title, two `i64` timestamps and an `OrderKey`, and none of that exists here.

**The field the gate deleted is the part worth recording.** The manifest carried `newlines` — a per-section
newline count, so a manifest of heights can size a scrollbar. It is *free* to compute here (H2 had to pay
Loro for its block count: an 8,000-character section that is one paragraph renders 2,070 px while the same
characters as ten paragraphs render 2,373 px, `H2/.../manifest.rs:29-38`). And **free and exact was still
wrong**, because keeping it exact is not free:

* Every section but the last has a **constant** length — the cut is at `SECTION_BYTES` from the section's
  start — so inserting a byte at offset 0 moves no boundary and shifts every section's *content*.
* So a newline can move from section `k` into section `k + 1`, and **any** section's count can change: all
  97 are stale, not three of them.
* `newlines.total() + 1` then disagrees with the line count the editor maintains, and the disagreement was
  the rebuild trigger. **Measured: 3,300 µs, on one keystroke in ~21 at offset 0 — 6.6x the 500 µs budget**,
  and it was the entire "worst" column.

Nothing read the distribution, because the scroll geometry is `DocLines`' job. So the field is **removed**
rather than maintained approximately. A second copy of a quantity another structure maintains exactly is a
copy that drifts, and this one drifted into a 6.6x budget overrun. The keystroke gate went from
**worst 5,746 µs** (failing) to **worst 398–419 µs**, and the gate is what caught it —
`no_keystroke_rebuilds_the_manifest_including_a_newline` is the regression test.

**Three more things the tests caught, each of which was a confident wrong answer rather than an error.**

1. **Anchors were not counted, and six image gates failed at once.** The manifest said zero spans,
   `emit_images` returned early, and the image simply did not draw — no crash, no log, `doc_glyphs == 0`. A
   guard whose condition is *derived from a summary* can fail in the direction of quietly drawing nothing,
   and no amount of unit-testing the summary catches it; only the end-to-end gate for the thing being
   skipped does. Six tests failing together is the good case.
2. **`$$` was counted per *byte*, not per *occurrence*.** Toggling a flag once per `$` meant `$$` — two
   bytes — toggled twice and cancelled, so a formula contributed **zero** spans. Then a second version
   counted *pairs*, which reported zero for `$$hello` — and that is wrong too: `for_each_math_span` treats
   an unpaired `$$` as a span running to the end of its line, because that is the normal state while
   someone is halfway through typing a formula. **One span per opener, paired or not.**
3. **A mid-document insertion moves the *next* section, not just this one.** Two earlier versions read a
   section to its own recorded weight, so a `$$` typed mid-document was truncated off the end of the read
   and never seen; and one read the last section only when the caret was elsewhere, which is wrong for every
   append because an append's caret *is* in the last section. The end is now re-derived from the cut rule
   with a `SECTION_BYTES + 4` byte window, because the bytes after the insertion are the only place the
   new cut can be seen.

**Cost: the keystroke.** Three sections re-measured per keystroke is 196,572 bytes, and the first
`measure` was a byte-at-a-time loop: **+254 µs, half the budget**, median 106 → 360 µs. It is now a
word-at-a-time mask test — one branchless test per 8 bytes for the four marker bytes (`$`, and U+FFFC's
`EF BF BC`), with the byte fallback reached only on a hit, which for prose is once per section. Measured
**55 µs for three sections**, median at the document's start 176–271 µs and worst 398–419 µs, inside the
500 µs budget. **The mask test is not sufficient on its own and the unit test says so**:
`measure_agrees_with_a_byte_scan_on_every_marker_at_every_alignment` puts each marker at every offset from
0 to 45 and caught that a word-only test **reported zero spans for an anchor at offset 0** — an anchor is
most of an image's content, so that is a class of error where the manifest says "no images" about a
document full of them.

**Cost: 15,800 bytes.** 1,468,344 → **1,473,144** against the 2,097,152 ceiling.

**Not started, and named.** Item 3, on-demand load and evict: the manifest knows *where* a section is and
nothing yet reads it from the container, so `doc_scratch` is 0 because no emitter reads the document, not
because a windowed reader replaced one. Item 4, `RLIMIT_MEMLOCK` ceasing to be the ceiling: `S_MAX_PAYLOAD`
is still 8 MiB and still needs 8.53 MiB of `mlock` against this host's 8.00 MiB limit, so **the format's
maximum document is still unopenable.** Both are item 3's work; this part made the index it will use.

##### Phase 13, part 2, step 1 — the primitive: one chunk of a document, on demand

`Wavefunction::read_chunk_into` and `Wavefunction::chunk_content_offset`;
`crates/holonomy-container/tests/chunk_read.rs`, 9 tests.

**The container had no way to hand out part of a document.** This is the fact that stopped item 3, and it
is worth stating plainly because it is not obvious from the API's shape:

* `read_content` reads **all** of it — one `Vec` of `content_len`, 8 MiB at the ceiling.
* `read_raw` returns **ciphertext**.
* The only other route to plaintext was `Ring::seek`, which is the **write** pipeline's three-stage sliding
  window, and it needs a `&DirectFile` that `Wavefunction` keeps private.

So a windowed reader had nothing to stand on. `read_chunk_into` is **one chunk, one authenticated read, no
allocation**, and a section is `CHUNK_PLAINTEXT` bytes by construction — so one call is one section and a
load is one `pread64` plus one `open_chunk`.

**It deliberately does not go through the ring.** `seek` prefetches its neighbours, which is right for
sequential `read_content` and wrong here: a windowed reader asking for chunk 47 does not want 46 and 48, and
on a 97-chunk document prefetching every request would read the whole file. `chunks_read_the_same_in_any_order`
is the gate for that, and it matters because `Ring::seek` is *order-sensitive by construction* — a
windowed reader built on it would read every intervening chunk when walking backwards.

**Security properties, all inherited and one new.** `open_chunk` authenticates, so a flipped bit is an error
rather than plaintext. **Chunk 0 is refused** — it is the master frame, and its plaintext holds the title and
the KDF parameters, which are short enough not to look wrong in place of prose. **The index is bounds-checked
against the frame's own `chunk_count`**, which is authenticated data read from chunk 0, so the check is
against a number that came from the seal rather than from a caller. The slot is **wiped on every path
including the error path**.

**Two bugs the tests caught, both silent.**

1. **The copy came after the wipe, so every chunk read back as 65,520 zeros.** `aead::open_chunk` decrypts
   **in place** — the AEAD module's own doc says "sealed or opened in place" (`aead.rs:4`) — so the plaintext
   lands in the first `CHUNK_PLAINTEXT` bytes of the slot that held its own ciphertext. Wiping first and
   reading after is *wipe-then-copy*, and it authenticates perfectly while returning an empty document.
   **Six of nine tests failed on it and every failure looked like an empty document rather than a bug.**
2. **The maximum document is 8,321,040 bytes, not `S_MAX_PAYLOAD`.** `chunks_for(n) = n.div_ceil(65,520) + 1`
   reserves a whole slot for the master frame, and `payload_len(chunks) ≤ 8,388,608` then forces
   `chunks ≤ 128`, so content is capped at `127 × 65,520`. The 128th slot's trailing **2,032 bytes are
   unreachable as content — 0.024 % of the payload.** Not fixed here: a partial trailing chunk is a format
   change and the frame's `chunk_count` is authenticated data. Recorded because it is a number a reader
   should not have to derive, and because it is smaller than the ceiling it is derived from.

**What this makes possible, and what it does not do.** The maximum document now reads back through **one
65,520-byte buffer** — `maximum document: 8321040 bytes in 127 chunks` — which is the mechanism item 4
needs. **It does not yet stop anything being resident.** `Editor` still holds the whole document in a rope of
page-locked 4 KiB leaves, so `mlock` occupancy is unchanged and **the format's maximum document is still
unopenable**: 8,321,040 bytes of text needs 8.46 MiB of `mlock` against this host's 8.00 MiB limit.

**The concrete blocker for the rest of item 3, which is a design decision and not a bug.** `Editor` assumes
the document is contiguous and resident: `text_len()`, `read_into`, the caret, every edit path, the span
maps and the undo stack all read it as one rope. Making it sparse means `read_into` pulls from the store and
**an edit that is not in a resident section has to make it resident first** — so the store and the editor
have to agree on which sections are resident, and the manifest is the only thing that knows. That is the
next step, and it is a change to `holonomy-text` rather than to the container.

##### Phase 13, part 2, step 2 — the mechanism: a bounded set of resident sections

`SectionStore` in `crates/holonomy/src/store.rs`; `crates/holonomy/tests/session_store.rs`, 7 tests.

**This is the thing that makes document length a function of the window rather than of the process.** The
store holds a fixed *budget* of sections, loads one from the container on a miss, and on a budget violation
evicts the least recently used with `SecureBlock::zeroize_and_release()` — **synchronously, so resident
memory falls before the next frame and a gate can observe it.** That is 9C's rule ("eviction must be
synchronous and observable") applied to text, and it is why `evict_all` is a public method rather than only
a `Drop`: a caller that needs memory to fall *before* `commit()` or before a KDF has to be able to say so.

**It is built and gated on its own, and nothing reads it yet.** `Editor` still holds the whole document in a
rope of page-locked 4 KiB leaves, so **a session's residency is unchanged and the document is still
entirely resident.** The mechanism is separate because it is the new and risky part — bounded, scrubbed,
observable — and because wiring it in requires changing `Editor`'s core assumption that the document is
contiguous. That change is the next step.

**`copy_into` fills a caller's buffer rather than returning `&[u8]`.** That is the same shape as
`Editor::read_into` and `Wavefunction::read_chunk_into`, and the reason to choose it is that a store
returning a borrow into its own resident map would need interior mutability for a hit — touching the LRU
tick is a write, and the caller is holding the borrow. A lock on the read path or a raw pointer are the
alternatives; **a caller-supplied buffer is the option with no `unsafe` in it**, and the cost is one
`copy_from_slice` of a section, which the read was doing anyway.

**Eviction frees a slot *before* allocating, not after.** Otherwise the store peaks at `budget + 1`
sections — and the peak is the number worth asserting. Freeing afterwards is the classic way a bound becomes
`budget + 1` and nobody notices, because `resident()` reads `budget` again by the time anyone looks. The gate
checks the bound *inside* the loop, after every access, over 40 sections and seven budgets, because checking
it after the loop passes for any store that happens to be under budget when the loop ends.

**A budget of 0 is a legal configuration meaning "nothing stays resident",** and is not silently promoted to
1. The degenerate case is unreachable if you clamp it, and the degenerate case is exactly what a caller
under memory pressure wants to be able to ask for.

**One bug the tests caught, and it is the same trap as step 1's, one layer up.** `chunk_of` originally
returned `Wavefunction::chunk_content_offset(section + 1)`, which answers *"where in the document's text
does this chunk start"* — a **byte offset** — and the `Some(0)` it returned for section 0 was then passed to
`read_chunk_into` as a **chunk index**. Chunk 0 is the master frame, which is refused. **All seven gates
failed identically with `StoreError::Read`,** the most opaque error the type has, and the symptom — every
read of every section failing the same way — is *consistent with* an off-by-one that never varies, so
nothing in the failure pointed at the off-by-one. `chunk_of` now validates through the container and returns
the index.

**The stronger scrubbing claim cannot be made here, and the doc says so.** `released_bytes` sums what
`zeroize_and_release` reported releasing, which proves eviction *released* memory rather than merely dropping
a struct whose `Drop` was never reached. **"The pages read back as zero" is not checkable from here**,
because `munmap` has already unmapped them — a test asserting it would be asserting a property of memory
the process no longer owns. The zeroing is `SecureBlock`'s and is gated in `crates/holonomy-secure/tests/`;
what is gated here is that eviction *calls* it.

#### Phase 13, part 2, step 2b — the `mlock` ceiling: what it actually is

**Decision, 2026-10-06: `mlockall` is KEPT, and it should be.** `main.rs:260` is untouched. The reason is
now a measurement rather than a preference, and it is the *opposite* of what this section previously said.

**Correction, 2026-10-06: an earlier version of this section claimed the opposite and was wrong.** It said
*"`mlockall` locks the process's address space, so `RLIMIT_MEMLOCK` is spent on pages rather than on the
document, and windowing cannot move the ceiling by a byte."* **That is false.** A locked page that is
unmapped releases its charge against `RLIMIT_MEMLOCK`, so the locked set tracks the **resident** set — and a
bounded resident set therefore bounds the page-lock ceiling.

Measured by `unmapping_releases_the_page_lock_charge` (`rlimits.rs`), which runs the probe in a child
process because `mlockall` is process-wide and libtest cannot measure it:

```text
PROBE mlock_rc=0 errno=0 base=0 full=2048 after=0 region_kib=2048
```

`VmLck` goes **0 → 2048 kB** as a 2 MiB region is mapped and faulted in, and **2048 → 0 kB** on `munmap`.
The charge is real while the pages are there and is gone when they are not.

**So `SectionStore`'s budget *is* the page-lock budget**, `mlockall` costs nothing on top of it beyond
whatever is genuinely resident, and **item 4 is reachable without touching `mlockall` and without raising
`RLIMIT_MEMLOCK`.** The maximum document needs `8,321,040 × 4096/3840 = 8,875,776 B = 8.46 MiB` of page-locked leaves *only
if all of it is resident*; at a 4-section budget it needs about 256 KiB. What matters is the **peak**, so the
windowing has to be in place *before* the document is loaded — a fully-resident `Editor` peaks at 8.46 MiB
and fails, and there is no recovering from that after the fact.

**The ceiling still cannot be raised, and that is now a side note rather than a blocker.** Measured, for the
record: soft == hard == 8.00 MiB, `CapEff` is `0000000000000000` so there is no `CAP_SYS_RESOURCE`,
`setrlimit` returns `EPERM` for a hard-limit raise and `EINVAL` for a soft one, and `ulimit -l unlimited`
is refused by the shell. **None of that needs fixing, because a bounded window does not approach 8 MiB.**
The ceiling is still printed at boot and pinned against a live `getrlimit`
(`the_reported_memlock_ceiling_is_the_real_one`) so that it reads as a host property rather than surfacing as
an unexplained `SecureBlockError::MlockFailed` from inside a leaf allocation.

**And the retirement question closes, in the direction of keeping it.** The audit of what retiring `mlockall`
would cost stands, and it is a real reduction: `MCL_FUTURE` marks *every* future mapping `VM_LOCKED`, so
plain heap buffers holding derived plaintext — `Vec<u8>`, `String`, the manifest's trees, `line_scratch` — are
locked *incidentally* today and would lose that coverage. Per-block locking is as strong as `mlockall` for
every byte H1 has deliberately chosen to protect (`Editor`'s leaves, the section store, the alt stack) and
strictly weaker for those heap buffers. **But with the corrected understanding there is no longer any reason
to do it**: `mlockall` is no longer what stops the maximum document from opening. The buffer audit is
therefore no longer a gate on anything — it becomes ordinary hygiene, worth doing on its own merits and not
worth spending a security reduction on.

**What the mistake was, so it is not repeated.** The claim was reasoned rather than measured, from
"`mlockall` locks every page forever" — which is true about *mapped* pages and silent about the fact that
unmapping returns them. Two `SecureBlock` facts were already in the tree and would have answered it: leaves
are `munmap`ped when dropped (`SecureBlock::zeroize_and_release`), and `munlock` is allowlisted
(`seccomp/table.rs:99`) precisely because a freed slot must not leak against the ceiling. The design already
knew the answer; the note did not ask it.

#### Phase 13, part 3 — wiring `Editor` to `SectionStore`: the impedance mismatch

This is the design for item 3's remaining half, recorded **before** it is built, because two measurements
change the shape of it.

**Correction, 2026-10-06: the page-lock figure was wrong by a unit.** Earlier text said the maximum document
needs `8.88 MiB`. The byte count `8,321,040 × 4096/3840 = 8,875,776 B` was right; `8,875,776` bytes is
**8.46 MiB**, and 8.88 was that number in decimal MB mislabelled as MiB. The conclusion is unchanged — it is
over the 8.00 MiB ceiling — but by **0.46 MiB, i.e. 119 leaves**, not by whatever 8.88 implied. `main.rs`
already computed it correctly.

**Measurement 1 — the ceiling is exactly 2,048 leaves, and that is where 8.00 MiB comes from.** Each leaf is
one 4 KiB page-locked `SecureBlock`, so `2,048 × 4,096 = 8,388,608 B = 8.000 MiB` precisely. The maximum
document needs `ceil(8,321,040 / 3,840) = 2,167` leaves — **119 more than the ceiling holds.**

**Measurement 2 — a section is not a whole number of leaves, and this is the real obstacle.**

```text
leaf fill   LEAF_CAPACITY - GAP_MINIMUM = 4,096 - 256 = 3,840 B
section     CHUNK_PLAINTEXT                           = 65,520 B
65,520 / 3,840 = 17.0625      -> not integral
```

`SectionStore` budgets and evicts **per section**; `Rope` addresses **per leaf**. So **the two units do not
tile, and a leaf can straddle a section boundary** — faulting one absent leaf in can require reading **two**
sections, and the pair has to be pinned for as long as the leaf is resident, or the second fault-in returns
bytes from a different pair than the first. This is the thing that has to be designed, and it is why "just
point `Editor` at the store" is not a small change.

**Measurement 3 — `Rope::insert_at` is byte-at-a-time, so there is no streaming load today.**

```rust
pub fn insert_at(&mut self, offset: usize, bytes: &[u8]) -> Result<(), RopeError> {
    for &b in bytes { self.insert_byte(b)?; }
}
```

`Editor::from_text(&[u8])` is the only load, and it requires the **whole document as a contiguous
`&[u8]`** before the first byte lands. So "load then window" is not merely bad for peak `mlock`, it is
**structurally unavailable** — there is no API that consumes a document incrementally. A `&[u8]` of
8,321,040 bytes is itself 7.94 MiB of ordinary (unlocked) heap, on top of the rope.

**What the design has to satisfy, gathered:**

| requirement | consequence |
| --- | --- |
| `locate` is a binary search over `starts` — **pure geometry, no leaf bytes** (rope.rs:212) | an absent leaf needs only its **length**; the spine stays fully resident and costs 24 B/leaf (≈ 52 KB for 2,167 leaves) |
| leaf 3,840 B vs. section 65,520 B, 17.0625 sections/leaf | a fault-in may need **two** sections, pinned together |
| edits shift every offset after them | a leaf's bytes stop aligning to any section boundary after the first edit; the manifest's `sync` is what re-establishes alignment, so the store and the rope **cannot disagree about what is resident** — that is the coupling the summary named |
| caret, `insert_byte`, `delete_byte`, `set_gap_offset`, split/merge all mutate leaf bytes | "edit an absent leaf" must **fault it in first**; the alternative is a leaf that is silently wrong |
| `any_leaf_contains` is linear in total size and is the destructive-delete gate | a full scrub cannot see through an absent leaf, so eviction must not be allowed to hide bytes from it |

**Sequencing, and why B before A.** B (absent leaves + fault-in) is what removes the 8.46 MiB peak and is
therefore the change that unblocks the maximum document. A (a streaming load, so no whole-document `&[u8]`
exists) removes a 7.94 MiB heap spike but **does not reduce residency on its own** — the rope would still
hold every byte. Doing A first would be a smaller, independently testable change that does not move the
number anyone is waiting for. **B is being done first.**

#### Phase 14 — The chrome: pointer input, menus, icons

Drawn natively, by the existing surface tree, at the Phase 5 blitter. Not a web interface, not a
component library, not an SVG runtime — `crates/holonomy-render/src/tree.rs:1-34` already sets that
rule and the zero-Bézier invariant already forbids the alternative.

**Features explicitly removed from the reference screenshots, by decision on 2026-10-05:** Share,
Upgrade, cloud sync, comments, Extensions, and AI. They are removed because the sandbox forbids the
network that every one of them requires (`FR-5.1`'s `unshare(CLONE_NEWNET)`), not because they are
hard to draw. A menu entry for a feature that cannot work is worse than no entry.

1. **Pointer input.** The input layer is keyboard-only today: `decode()` keeps `EV_KEY` and drops
   `EV_REL` as "never a keystroke" (`crates/holonomy-input/src/event.rs:144-160`), the X11 event mask
   omits `PointerMotionMask` (`crates/holonomy-x11/src/window.rs:248-256`), `Event` has no
   `MotionNotify` variant (`proto.rs:531-609`), and the one `ButtonPress` consumer throws the
   coordinates away in order to take focus (`crates/holonomy/src/windowed.rs:195-199`). Motion, buttons,
   and coordinates are carried end to end. `Chrome::toggle_rect` and `Chrome::scroll_thumb`
   (`chrome.rs:804, 826`) are the hit-test primitives and are currently reachable only from drawing code.
2. **Menus.** A dropdown needs layered input: a popup drawn above the page, a click outside it closing
   it, and hover state on the row under the pointer. The z-order it needs already exists —
   `SurfaceTree` has `before`/`after` child lists with before→self→after draw order
   (`tree.rs:316-329, 395-413`, test `draw_order_is_before_then_self_then_after`), and `Chrome::tree`
   already composes three root groups. What does not exist is popup *state*, a grab, or hit-testing.
3. **Icons, hand-authored.** `Icon { bits, width, height, x, y, colour }` exists (`tree.rs:203-260`) and
   **zero icons exist** — `PROJECT.md:418`'s "icon masks" is aspirational, and the painter currently
   *refuses* to draw them, counting each as `rects_skipped` (`crates/holonomy-display/src/paint.rs:196-202`).
   Cost is authorial labour, not bytes: a 20×20 1-bit mask is 50 bytes.
4. **The glyphs a menu wants are outside the font.** `▶` U+25B6, `✓` U+2713 and `…` U+2026 are in
   neither `TEXT_RANGES = [(0x20,0x7E), (0xA0,0xFF)]` (`crates/holonomy-assets/src/payload.rs:125`) nor
   `MATH_RANGES` (`payload.rs:128-139`), and `BOX_RANGE` stops at 0x257F. Per §7 item 2 and the
   box-drawing precedent (`chrome.rs:29-32`), they are **drawn as 1-bit masks**, not added to the subset.
   Growing the font would spend atlas slots the 9B finding shows are already at 96.4 % of the 512 KiB
   ceiling (`PROJECT.md:704`).
5. **The sidebar.** A document-tabs sidebar is named in Phase 8's chrome list (`PROJECT.md:598`) and
   exists in no crate — `Layout` (`chrome.rs:253-354`) has no sidebar field. It is a band in `Layout`
   plus a click target, and it is the largest single piece of this phase.

**Security, stated plainly rather than buried.** The `desktop` window path runs inside an ordinary X
session, where any other client with access to the same display could in principle observe the screen or
inject events. That is the accepted cost of the 2026-10-05 decision at the head of §5 — the desktop is
the designated target, and the sealed path has no display at all. It does not change with a menu bar,
and it is recorded here so that "add a rich UI" is never mistaken for a security-neutral change.

**Gate.** `cargo test -p holonomy-x11 --test live` with a synthesised pointer motion and click landing
on a toolbar button and a menu row, asserting the clicked action fired. A menu opens, closes on an
outside click, and survives a window resize mid-open with its layout reflowed. A PPM fixture of the
chrome. Icon count asserted non-zero and each icon's bounds asserted integral. `release_artifact.rs`
still fails if any of it reaches a default-features binary. Binary ≤ 2.0 MiB — the room is 904,328 B on
the desktop build, and none of this needs more than a few tens of KiB.

## 6. Gates, restated as numbers

| requirement | source | gate |
|---|---|---|
| binary static, stripped | NFR-2.3 | `ldd` → `not a dynamic executable`; **≤ 2.0 MiB** (was 2.5; §2.9.1); **1,473,144 B measured** at Phase 13 part 1 |
| keystroke→pixel p99.9 | NFR-1.1 | ≤ 0.50 ms on this host. **Measured at Phase 13 part 1: median 176–271 µs / worst 398–419 µs at the start of a 3.1 MiB document, 59–93 µs at its middle, 2–4 µs at its end; paint 144–245 µs, flat in document size.** Phase 11's 106 µs median was before the manifest's three section reads; Phase 12 removed a 749 µs paint term and Phase 13 added 55 µs of edit |
| image decoder cost | §2.9.1 | ≤ 60 KiB of the binary, measured by section delta |
| atlas footprint incl. math window | §2.2, §2.9.2 | ≤ 512 KiB |
| decoded image memory | §2.9.3 | **≤ 8.0 MiB at every point** of a page-1→50 scroll, 10 images |
| evicted rasters scrubbed | §2.9.3 | zero after every eviction, asserted by the allocator |
| math layout allocations | §2.9 9B | **0** between `MathNode` and frame |
| table border alignment | §2.9 9A | exact integer pixel coordinates, not ±1 |
| steady-state RSS | NFR-2.1 | ≤ 16.0 MiB with a 2000-page document open — **measured as of Phase 13 part 1, and still not met at document scale**: **20.82 MiB for a 6 MiB document, crossover 3.40 MiB.** `doc_scratch` is 0.00 MiB, down from 6.00. §2.9.4's 12.4 MiB estimate omitted the scanout's second framebuffer, the scratch buffer and the geometry; what remains is 1.067 for page-locked leaves + 0.545 for line geometry + 0.24 for rope slack |
| per-keystroke allocations | invariant | **0 on the edit path**, driven through a `Session` (`tests/session_no_alloc.rs`); the paint path is non-zero until Phase 12 |
| section residency index | §Phase 13 | **8 bytes per section** (`2 × (n+1) × u32`); 97 sections of a 6 MiB document = **784 B, 0.0013 % of the text**. A section is one container chunk, so a load is one `pread64` — `tests/session_manifest.rs` |
| markers without reading | §Phase 13 | `Manifest::span_total()` answers "does this document contain a formula or an image" with **0 bytes read**, and four emitters guard on it — `a_prose_document_never_allocates_the_whole_document_buffer` |
| read one section of a document | §Phase 13 | `Wavefunction::read_chunk_into` reads **one chunk, one authenticated read, no allocation**, in any order. The **maximum document — 8,321,040 B in 127 chunks — reads back through one 65,520-byte buffer** — `tests/chunk_read.rs` |
| container reads one section, refused cases | §Phase 13 | chunk 0 refused as content (it is the master frame), out-of-range refused not clamped, short output refused, and the slot wiped on every path including the error path |
| resident text is bounded | §Phase 13 | `SectionStore` holds **at most its budget sections at every point** — checked inside the loop over 40 sections and 7 budgets, not after it — and **a document 8× its budget still reads back byte-exact** through a 5-section budget. Page-locked, so an unlocked block holding text could not hide here — `tests/session_store.rs` |
| eviction releases memory | §Phase 13 | every eviction calls `zeroize_and_release` synchronously, and the released bytes are **counted** (12 loads into 4 slots = 8 evictions, ≥ a section each). The victim is **named**, not merely counted: the least recently used section is the one that goes, and re-reading it costs a load. **"The pages read back as zero" is not asserted here** — `munmap` has already unmapped them; that is `SecureBlock`'s claim and is gated in `holonomy-secure` |
| page-lock ceiling vs. residency | §Phase 13 | **`munmap` releases the charge**, so the locked set tracks the *resident* set: `VmLck` 0 → **2048 kB** as a 2 MiB region is faulted in, → **0 kB** on `munmap`. **So `SectionStore`'s budget *is* the page-lock budget** and item 4 is reachable with `mlockall` untouched. `mlockall` **kept** — it costs nothing beyond what is resident. The ceiling (8.00 MiB, unraisable here: `CapEff` = 0, `setrlimit` `EPERM`) is printed at boot and pinned against a live `getrlimit` — `unmapping_releases_the_page_lock_charge` |
| binary static, stripped, default build | §6 | **1,441,912 B measured** at Phase 11; the 1,101,944 in §9B predates 9C/9X and was stale |
| document open time | §13 | printed and gated; a 2000-page document is a target, not an extrapolation |
| KDF peak RSS | NFR, §1.2 | ≤ 400 MiB |
| `t_kdf` | §2.4 | measured, budget restated — the PRD's 400–550 ms is replaced |
| keystroke→pixel p99.9 | NFR-1.1 | ≤ 0.50 ms on this host, the designated target since 2026-10-05 — superseded by the measured row above as of Phase 11 |
| idle CPU | NFR-1.2 | ≤ 0.001%, process blocked in `epoll_wait` |
| container entropy | FR-4.1 | NIST SP 800-22 subset passes, Shannon ≥ 7.99999 |
| container size | FR-4.1 | exactly 134,217,728 bytes |
| no heap allocation while editing | invariant | counting allocator asserts 0 |
| forbidden syscalls | FR-5.3 | SIGKILL under the measured allowlist |

---

## 7. Open items needing you

1. **`SETCRTC` needs DRM master**, and there is no longer a bare-silicon target to need it.
   Verified everything else on the DRM path unprivileged. **Closed 2026-10-05:** with the desktop
   window as the designated target, bare-metal presentation is out of scope rather than deferred, and
   a `mode-setting` fallback that renders into the dumb buffer is not worth building for a path
   nothing uses. The `Scanout` trait keeps the seam if that changes.
2. **Arrows in the UI.** Inter has no Arrows block. Either pick a different glyph for the
   sidebar back button or add a second small face. Cosmetic; I will default to a drawn
   triangle mask and note it. **Decided and folded into Phase 14:** drawn 1-bit mask, together
   with `▶` `✓` `…`, all of which are outside every declared font range.

3. **An area filter for the downscale path.** §2.9.3 makes every image a downscale to page-column
   width, and the product's own ratio is exactly 3:1 -- at which bilinear's interpolation weights are all
   zero and the filter is a decimator. At 6.86:1 it reads two of every seven source pixels and a hard
   edge produces no intermediate values at all. `axis_map` is correct and its pixel-centre convention is
   pinned; the *filter choice* for large reductions is what is wrong, and an area average is the fix.
   Pinned from both sides by `an_exact_integer_ratio_has_zero_weights_and_is_a_decimation` and
   `a_non_integer_ratio_has_fractional_weights_and_still_reads_only_two_pixels` in
   `crates/holonomy-image/tests/scale_cache.rs`, so it cannot change silently either way. **Not** done
   in 9C: it is a rewrite of a mutation-verified module and wants its own gate. Full statement in §9C.

4. **Which sizes are we allowed to claim?** Phase 11's gate runs at "the largest prefix this host can
   lock" and therefore passes today at roughly 3.5 MiB, not at 2000 pages. Reaching the full design
   document needs Phase 13's windowing. So until Phase 13 lands, the honest claim is **~950 leaves of
   editing latency, not 2000 pages** — and §6's RSS row is not yet measured at any size.

5. **The affordable document is 3.40 MiB, not 8 MiB, and that is an `mlock` ceiling rather than an RSS
   one.** Phase 13 part 1 moved §6's RSS row from 2.20 MiB to 3.40 MiB, which is real, and the number it
   competes against is the format's maximum document, which is **8,321,040 bytes, not `S_MAX_PAYLOAD`** —
   `chunks_for` reserves a whole slot for the master frame and refuses a partial trailing chunk, so 2,032
   bytes of the payload are unreachable as content. **The gap between 3.40 MiB and 8.32 MB is not memory.**
   A document's text is page-locked while it is resident, and 8,321,040 bytes of text fully resident needs
   `8,321,040 × 4096/3840 = 8,875,776 B = 8.46 MiB` of `mlock` against this host's 8.00 MiB ceiling — so **the format's
   maximum document is still unopenable today**, exactly as Phase 11's audit found. Phase 13 part 2 step 1
   made it *readable in principle* (it comes back through one 65,520-byte buffer) and did not make it
   *openable*, because `Editor` still holds the whole document resident.
   **Until then the honest claim is a 3.4 MiB document, and the reason is `mlock` rather than RAM.**

   **What changes this: the bound is a *peak*, not a floor.** `munmap` releases the page-lock charge, so a
   bounded resident set bounds the locked set (item 7). The 8.46 MiB above is what a **fully resident**
   document costs; a 4-section window needs about 256 KiB. **So the ceiling is reachable by windowing alone**
   — the 3.40 MiB number is a property of `Editor` being fully resident, not a property of the host, and
   wiring `Editor` to `SectionStore` is what removes it.

7. **Item 4 is reachable, and the thing that makes it reachable is item 3.** An earlier version of this item
   said the opposite — that `mlockall` spends `RLIMIT_MEMLOCK` on the process's *address space*, so
   windowing "cannot move the ceiling by a byte" and the maximum document would need a host with
   `LimitMEMLOCK=infinity`. **That was wrong**, and it was reasoned rather than measured: it is true that
   `mlockall` locks every page the process maps, and silent that **unmapping returns them**.
   `unmapping_releases_the_page_lock_charge` measures it: `VmLck` 0 → 2048 kB → 0 kB across
   mmap/touch/`munmap` of 2 MiB. **So the locked set tracks the resident set, `SectionStore`'s budget is the
   page-lock budget, and `mlockall` costs nothing beyond what is genuinely resident.** Item 4 needs no host
   change, no raised limit and no retirement — it needs `Editor` to be sparse *before* the document is loaded,
   because what binds is the **peak**. A fully-resident `Editor` peaks at 8.46 MiB and fails; a 4-section
   budget needs ~256 KiB. See §Phase 13, part 2, step 2b.

8. **`SectionStore` is gated and unused.** A session's residency is unchanged, because `Editor` does not read
   from it. It is not dead code in the sense of "will be deleted" — it is the mechanism, gated on its own
   because it is the risky part — but **no product path reaches it, and §6's RSS row must not be read as if
   it did.**

6. **A paint is 144–245 µs and a keystroke is 176–419 µs, and both are inside the budget — on this host,
   with this document size, and with 1 MiB of framebuffer resident.** None of those numbers is a
   projection, and none of them is measured at 2000 pages, because the document is not loadable at 2000
   pages yet (item 5). §6's row is a measurement at 6 MiB; it is not a claim about the design size.

Nothing else is blocked. Phases 0–9 are fully executable on this machine, unprivileged, as
they stand — Phase 9's two measured dependencies are both satisfied here: `/usr/share/fonts/google-noto/
NotoSansMath-Regular.ttf` is present for the math face, and `munmap`/`madvise`/`mlock` are already in
the 50-entry allowlist.

---

## 8. What is deliberately not being done

- **Sync, CRDT, ML-KEM, relay server** — §2.3. Deferred, not rejected.
- **Share, cloud sync, comments, Extensions, AI** — **removed 2026-10-05, by decision.** These are
  features of the reference screenshots in `Plan/` that require the network `FR-5.1`'s
  `unshare(CLONE_NEWNET)` forbids. Not deferred: they cannot work in this product, and a menu entry
  for a feature that cannot function is worse than no entry. Phase 14 builds the rest of the chrome.
- **A GUI framework** — `winit`, `egui`, `iced`, `slint`, `softbuffer`, `tiny-skia`: all rejected on
  binary size, on the Zero-Bézier Invariant (each brings its own outline rasteriser), and on
  `x11-dl`'s `dlopen`, which a static musl binary cannot perform. Chrome is the existing surface tree.
  `crates/holonomy-x11/Cargo.toml:1-19` records this and depends on `libc` alone.
- **A second compositor path (softbuffer / tiny-skia)** — the superseded PRD revision. The
  H1 stack is DRM/KMS only. **Amended 2026-10-04:** the `desktop` feature adds a window for development,
  which is not a second *product* compositor path: it is off by default, it is not reachable from the
  sealed boot chain, and `release_artifact.rs` fails if any of it reaches a default-features binary. What
  it does share with the product is the `Scanout` trait, which is why `present_damage` was added to the
  trait rather than to one implementation.
- **H2's Typst export pipeline** — replaced by `pdf-writer`, §2.3.
- **H2's 1500-word section constant** — **not carried forward, 2026-10-05.** It was measured against
  Chromium window-slide costs (`H2/spikes/m0-section-seam/FINDINGS.md:25-38`) and Loro styled-read costs,
  neither of which exists in H1. Phase 13 re-derives it. What *is* carried forward is the discipline:
  window-bounded residency, a structure-without-content manifest, and a bounded content cache.
- **The Fenwick tree as H2's speed story** — H2 says otherwise itself, twice (`H2/STATUS.md:23-29`:
  0.46 µs at 667 sections, 0.003 % of a frame). Phase 13 ports the windowing, and the trees come along
  because they are correct and free, not because they earned their place in H2.
- **A TeX engine** — replaced by the micro-parser of §2.9 9B, on binary-size grounds.
- **CFF outlines / STIX Two Math** — the math face is Noto Sans Math precisely so no CFF
  interpreter is needed, §2.9.2.
- **SVG, JPEG, WebP** — one decoder, §2.9.5.
- **Variable-length tables** — eight columns, a compile-time constant, §2.9 9A.