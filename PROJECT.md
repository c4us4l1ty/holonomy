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
is 57× optimistic.** On the Core 2 Duo it would be far worse. NFR-1.3 (400–550 ms total
`t_kdf`) is unreachable at PRD parameters and is replaced in §2.4.

**Wesolowski VDF, 2048-bit Montgomery square, 32 u64 limbs, `opt-level=3` + lto +
`codegen-units=1` + `target-cpu=native`:**

| | measured |
|---|---|
| ns per squaring | **2,077 ns** |
| PRD's claim | 300 ns |
| T = 1,500,000 → measured here | **3,115 ms** |

The PRD's 450 ms is 7× optimistic here and worse on the target. `T` is therefore derived
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
- **Tables, LaTeX math, inline images** — Phase 2 of the PRD's own roadmap. Deferred.

### 2.4 Key derivation: replace the PRD's parameters with a measured budget

The PRD's 384 MiB / t=16 / p=2 costs 10.2 s here and would be ~30 s on a Core 2 Duo. That
is not an unlock, that is a denial of service on the user's own device. NFR-1.3's
400–550 ms total cannot be met at PRD parameters, so the budget is restated:

| parameter | value | justification |
|---|---|---|
| Argon2id m | 131,072 KiB (128 MiB) | 128 MiB is a real memory fence; a GPU attacker must supply it per guess. Measured 524 ms here, ~1.5 s on target. |
| Argon2id t | 2 | minimum that is not degenerate |
| Argon2id p | 2 | dual-core, no thrash |
| VDF T | `floor(target_vdf_ms / ns_per_squaring × 1e6)` | derived, not guessed |

`build.rs` runs a `vdf-calibrate` bin once and bakes `NS_PER_SQUARING` into the binary; T is
a compile-time constant computed from it. Changing the unlock budget is changing one number
and rebuilding. The `vdf-calibrate` bin is the honest artifact — it prints measured ns per
squaring so the target's T can be re-derived without a rebuild.

This is a deliberate deviation from FR-4.3/FR-4.4 and is recorded as such. The security
argument is preserved (memory fence + non-parallelizable serial chain); only the constant is
corrected. Phase 9 re-measures on the Core 2 Duo and the numbers go into STATUS.md.

### 2.5 The VDF modulus `N_pub` must be a real, checked safe prime

The PRD says "RSA-2048 safe prime" and never says where it comes from. Phase 1 generates it:
2048-bit p with p ≡ 3 (mod 4), q = 2p+1 prime, `N = 2pq+1` prime. Prime proof is
Pocklington with a witness set, not Miller–Rabin alone. The modulus is a committed constant
plus a `tools/verify-modulus` bin that re-proves it from the committed factors. `ST` is the
Montgomery chain in §1.1, started from `K_int mod N` and squared `T` times.

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
crates/holonomy             the binary: boot sequence, session loop, CLI
```

`holonomy-assets` builds with `build.rs` and cannot be a dependency of anything else —
`include_bytes!` data is only visible to the crate that declares it. `holonomy` re-exports.

---

## 5. Phase plan

Phases 1–8 are strictly ordered. Phase 9 is the target-hardware run. Every phase lists the
gate that must pass before the next starts.

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

Then `tools/verify-modulus`: generate the 2048-bit safe prime, Pocklington-prove it, and
commit `N_pub` with its factors. The verifier re-proves from the committed factors on every
build.

**Gate.** A test allocates 4096 bytes and confirms the pages below and above fault.
`cargo test -p holonomy-secure` passes. `verify-modulus` re-proves `N_pub` from scratch.

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

### Phase 9 — Target-hardware run (ThinkPad X200, Core 2 Duo, GM45)

Run `verify-target.sh` on the bare-silicon machine. It measures: boot to passcode prompt,
`t_kdf` split by stage, the RSS ceiling during a 2000-page editing workload, the VDF per-
squaring cost feeding T, keystroke-to-pixel percentiles over 500,000 events, and idle CPU
over 30 minutes. KMS `SETCRTC` presentation is exercised here for the first time.

Results go into `STATUS.md` with the measurements that produced them, per H2's convention.

**Gate.** Every NFR in §6.

---

## 6. Gates, restated as numbers

| requirement | source | gate |
|---|---|---|
| binary static, stripped | NFR-2.3 | `ldd` → `not a dynamic executable`; ≤ 2.5 MiB |
| steady-state RSS | NFR-2.1 | ≤ 16.0 MiB with a 2000-page document open |
| KDF peak RSS | NFR, §1.2 | ≤ 400 MiB |
| `t_kdf` | §2.4 | measured, budget restated — the PRD's 400–550 ms is replaced |
| keystroke→pixel p99.9 | NFR-1.1 | ≤ 0.50 ms on target |
| idle CPU | NFR-1.2 | ≤ 0.001%, process blocked in `epoll_wait` |
| atlas footprint | §2.2 | ≤ 512 KiB |
| container entropy | FR-4.1 | NIST SP 800-22 subset passes, Shannon ≥ 7.99999 |
| container size | FR-4.1 | exactly 134,217,728 bytes |
| no heap allocation while editing | invariant | counting allocator asserts 0 |
| forbidden syscalls | FR-5.3 | SIGKILL under the measured allowlist |

---

## 7. Open items needing you

1. **`SETCRTC` needs DRM master.** Verified everything else on the DRM path unprivileged;
   presentation needs either the VT or root. Confirm whether Phase 9 runs on hardware you
   control, or whether I should add a `mode-setting` fallback that renders into the dumb
   buffer and presents via `drmModePageFlip` when master is unavailable.
2. **Arrows in the UI.** Inter has no Arrows block. Either pick a different glyph for the
   sidebar back button or add a second small face. Cosmetic; I will default to a drawn
   triangle mask and note it.

Nothing else is blocked. Phases 0–8 are fully executable on this machine, unprivileged,
as they stand.

---

## 8. What is deliberately not being done

- **Sync, CRDT, ML-KEM, relay server** — §2.3. Deferred, not rejected.
- **Tables, LaTeX math, inline images** — PRD Phase 2.
- **A second compositor path (softbuffer / tiny-skia)** — the superseded PRD revision. The
  H1 stack is DRM/KMS only.
- **H2's Typst export pipeline** — replaced by `pdf-writer`, §2.3.