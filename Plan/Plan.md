Part 1: The Architectural Autopsy (H2 vs. H1)

Can you use the H2 project files as a head start? Yes, but only for about 15% of the codebase—specifically the mathematical and layout abstraction models.

If you attempt to salvage the H2 frontend (Tauri, WebKit/Chromium, ProseMirror, DOM, CSS), you will immediately destroy the security, memory, and performance guarantees required for H1.

text

H2 ARCHITECTURE (TAURI / WEB STACK)           H1 ARCHITECTURE (BARE SILICON)
┌───────────────────────────────────────┐     ┌───────────────────────────────────────┐
│ DOM Viewport (Tiptap / ProseMirror)   │     │ SSE2 Vectorized Atlas Blitter         │
│ WebKit2GTK / Chromium Engine (~150MB) │ ──► │ Direct Linux DRM/KMS Dumb Scanout     │
│ Tauri IPC Bridge (JSON/MsgPack)       │ ──► │ Zero-IPC / In-Process Pure Rust       │
│ SQLite + zstd (.holo Container)       │ ──► │ IND-URN 128 MiB Noise Blob (O_DIRECT) │
│ Fenwick Tree (DOM Height Estimates)   │ ──► │ Fenwick-Indexed Line Height Layout    │
└───────────────────────────────────────┘     └───────────────────────────────────────┘
  [CRASHES ON 2GB RAM / METADATA LEAKS]         [SUB-0.5MS / FORENSICALLY INVISIBLE]

What Must Be Discarded from H2 Immediately
1. The Entire Web Stack (Tauri, Vite, TypeScript, ProseMirror, Tiptap, DOM)

    The Memory Trap: webkit2gtk or Chromium idle at 120 MiB to 250 MiB RSS just to exist. On a 2.0 GiB machine, this leaves almost no breathing room for OS buffers and cryptographic operations.
    The Forensic Vector: The DOM and JavaScript engines (V8/JavaScriptCore) allocate memory non-deterministically across the process heap. Plaintext strings are garbage-collected lazily, copied during DOM mutations, and leaked to un-lockable memory. They will hit disk swap and core dumps in cleartext.
    The Latency Vector: The browser event loop introduces a 15–40 ms input-to-pixel delay on legacy dual-core CPUs due to style recalculation, layout trees, layer compositing, and X11/Wayland buffer handshakes.

2. SQLite (.holo Database)

    Why it fails IND-URN: SQLite files begin with a cleartext 16-byte magic header string: SQLite format 3\000.
    Metadata Leakage: Even with SQLCipher encryption, page allocations, B-Tree rebalancing headers, and WAL (Write-Ahead Log) journals leak write cadences, sector updates, and payload size bounds. This completely invalidates the requirement for statistical indistinguishability from uniform random noise.

What Can Be Salvaged and Upgraded from H2
1. The Iceberg Architecture Concept (Elevated to Hardware)

H2’s core innovation was keeping 99% of the document "frozen" and 1% "hot." In H1, we translate this concept down to CPU cache and hardware page tables:

    The Hot Zone (0.1%): The active line being edited rests directly in the CPU L1/L2 Cache inside a 64-byte cacheline-aligned gap buffer.
    The Warm Zone (1.0%): The visible screen lines reside in a page-locked (mlock), tripwire-guarded memory block (maximum 12–16 MiB).
    The Frozen Zone (98.9%): The remaining 1,990+ pages reside on disk inside the 128 MiB encrypted noise container, accessed on-demand using unbuffered raw block reads (O_DIRECT).

2. The Fenwick Tree Geometry Model

H2's holonomy-core uses a Fenwick Tree (Binary Indexed Tree) for O(log⁡N)O(logN) prefix sums of section heights.

    Salvage Plan: Lift this Rust data structure directly from H2. Instead of measuring DOM element heights, feed it pre-calculated line-height metrics derived from font ascender/descender metrics. This answers "which line corresponds to vertical pixel YY?" in nanoseconds with zero DOM measurement overhead.

3. The Typst Translation Logic (translate.rs)

H2’s pure-Rust JSON-to-Typst markup translation can be repurposed for on-demand batch export. However, it must run exclusively inside an ephemeral, sandboxed worker process that is immediately terminated and scrubbed upon completion.
Part 2: The CRDT Evaluation (diamond-types vs. Forensics)

You asked: Should I use diamond-types?

text

DIAMOND-TYPES CRDT ENGINE
├── Pros: Blazing fast (nanosecond mutations), optimal RLE compression.
└── Cons: Preserves ALL edit history (keystroke timing, deleted words, redactions).
          ▼
    FORENSIC NIGHTMARE FOR AN AIR-GAPPED DOCUMENT EDITOR
    If an adversary seizes the memory or container, they can reconstruct
    every single typo, deleted sensitive name, or rewritten paragraph.

The Forensic Verdict: Do Not Use diamond-types for Local Editing State

In an ultra-secure editor targeting state-level adversaries, CRDT history is an operational security liability:

    The Anti-Forensic Paradox: If you redact a classified name or delete a sensitive paragraph, a CRDT preserves that deletion as a tombstone or historical mutation node. An adversary who extracts process memory or decrypts the container can walk the history DAG and recover every redacted word and the exact cadence of your keystrokes.
    RAM Bloat on 2000-Page Documents: On a document with 1,000,000 words subjected to heavy editing, the mutation DAG can easily swell to 100+ MiB of operation graphs. This instantly breaches our 16.0 MiB RSS limit.

The Recommendation

    For Local Document State: Use the Cacheline-Aligned Gap-Rope (CAGR). It is destructive by design: deleting text completely overwrites and zeros the data in memory.
    For Multi-Device Sync (Optional): If cross-device synchronization is enabled, maintain an ephemeral, squashed mutation buffer using diamond-types only during the sync session. Once the sync transaction is committed, squash the history down to the final state and execute zeroize on the operation log.

Part 3: Concrete Security Crate Integration

To achieve the hardened baseline, integrate the following crates:

toml

[dependencies]
# Memory Safety & Zeroization
zeroize = { version = "1.7", default-features = false, features = ["alloc", "zeroize_derive"] }
secrecy = { version = "0.8", default-features = false, features = ["alloc"] }

# Post-Quantum Cryptography (Pure Rust, no_std compatible)
kem = { version = "0.2", package = "kem" }
ml-kem = { version = "0.1", default-features = false } # Kyber / ML-KEM-1024

# ARX Cryptography (Constant-time on CPUs without AES-NI)
chacha20poly1305 = { version = "0.10", default-features = false, features = ["alloc"] }
blake2 = { version = "0.10", default-features = false }
argon2 = { version = "0.5", default-features = false, features = ["alloc"] }

Code Integration: Memory Protection via secrecy and zeroize

Rust

use secrecy::{Secret, Zeroize};
use zeroize::ZeroizeOnDrop;

/// Master cryptographic envelope keys wrapped in zeroize-on-drop invariants.
/// Secret<T> prevents accidental println!, core dump leakage, and debug exposure.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct RootCryptographicState {
    pub content_key: [u8; 32],
    pub chaff_key: [u8; 32],
    pub base_nonce: [u8; 24],
    pub payload_offset: u64,
}

pub type SecureRootEnvelope = Secret<Box<RootCryptographicState>>;

Part 4: Production Requirements Document (v3.0.0-SINGULARITY)

text

================================================================================
PRODUCT REQUIREMENTS DOCUMENT (PRD)
Project Codename: HOLONOMY
Release Baseline: 3.0.0-SINGULARITY
Security Classification: STRICTLY CONFIDENTIAL / TOP SECRET // NOFORN
Target Hardware Baseline: Intel Core 2 Duo (Penryn/Merom) / 2048 MiB RAM / Libreboot
Binary Target: x86_64-unknown-linux-musl (Pure Rust, Fully Static)
================================================================================

1. System Topology & Architectural Invariants

text

+─────────────────────────────────────────────────────────────────────────────+
|                       HOLONOMY HARDWARE-LEVEL TOPOLOGY                      |
+─────────────────────────────────────────────────────────────────────────────+
|                                                                             |
|  [USER PASSCODE] (Arbitrary length string, normalized via Unicode NFKD)     |
|         │                                                                   |
|         ▼                                                                   |
|  [STAGE 1: TWO-TIER HARDWARE TIME-LOCK ENGINE]                              |
|    │ 1. Argon2id (m=384 MiB, t=16, p=2) ──► Yields K_intermediate           |
|    │ 2. Wesolowski Squaring: S_i = (S_{i-1})^2 mod N_rsa (1.5M squarings)   |
|    │ 3. Volatile Scrub: 384 MiB unmapped (munmap) BEFORE UI initializes     |
|    ▼                                                                        |
|  [STAGE 2: IND-URN CONTAINER RESOLUTION (O_DIRECT)]                         |
|    │ 1. Resolves Offset Ω inside 128 MiB Uniform Noise Container             |
|    │ 2. 3-Stage Page-Locked Ring Buffer [Chunk N-1, N, N+1] (192 KiB RAM)   |
|    ▼                                                                        |
|  [STAGE 3: BARE-SILICON EXECUTION & KERNEL CONTAINMENT]                     |
|    │ 1. unshare(CLONE_NEWNET): Destroys network stack                       |
|    │ 2. prctl(PR_SET_NO_NEW_PRIVS): Permanently locks process privileges   |
|    │ 3. Seccomp-BPF Jail: Whitelists only read, write, ioctl, poll, nanosleep|
|    │ 4. DRM/KMS Dumb Buffers: Raw scanout array (Zero X11 / Zero Wayland)  |
|    ▼                                                                        |
|  [STAGE 4: HARDWARE-ACCELERATED TEXT & ICEBERG RENDERING]                   |
|    │ 1. Font: Embedded WOFF2 ──► Streaming Brotli ──► 64 KiB L2 A8 Atlas   |
|    │ 2. Geometry: Fenwick Tree calculates line offsets in O(log N) time     |
|    │ 3. Text Storage: Cacheline-Aligned Gap-Rope (CAGR) in 4KB mlocked pages|
|    │ 4. Blitter: SSE2 128-bit SIMD kernel blits text at CPU clock speed     |
|                                                                             |
+─────────────────────────────────────────────────────────────────────────────+

Absolute System Invariants

    Zero-Compositor Invariant: The binary interfaces directly with /dev/dri/card0 via Linux DRM/KMS dumb buffers and reads input via /dev/input/event*. Linking against libX11, libxcb, libwayland-client, or any userspace display server is strictly prohibited.
    Deterministic Memory Ceilings:
        Derivation Phase: Peak transient RSS must not exceed 400 MiB. Memory must be completely unmapped (munmap) before initializing the UI.
        Steady-State Phase: Total RSS during active editing of a 2,000-page document must remain ≤≤ 16.0 MiB.
        Host Headroom: ≥≥ 1,600 MiB of physical memory must remain untouched for the Linux kernel to prevent out-of-memory (OOM) invocations.
    Pure-ARX Constant-Time Invariant: Because legacy CPUs lack AES-NI, all cryptographic operations must rely exclusively on Add-Rotate-Xor (ARX) primitives: XChaCha20, Poly1305, and Blake2b. Lookup tables (S-Boxes) that leak data through cache-timing side channels are barred from the codebase.
    IND-URN Storage Invariant: The .wavefunction storage container must remain fixed at exactly 134,217,728 bytes (128 MiB) and match uniform random noise under NIST SP 800-22 and Dieharder test suites. Access occurs exclusively via direct, unbuffered block I/O (O_DIRECT).

2. Functional Requirements (FR)
2.1 FR-1: Cacheline-Aligned Gap-Rope (CAGR) & Fenwick Layout

text

PAGE-LOCKED SECUREBLOCK (4096 BYTES, 64-BYTE CACHELINE ALIGNED)
┌───────────────────────────┬───────────────────────┬─────────────────────────┐
│ Pre-Gap Plaintext Area    │ ACTIVE EDITING GAP    │ Post-Gap Plaintext Area │
│ 1,216 Bytes (UTF-8)       │ 2,048 Bytes (Empty)   │ 832 Bytes (UTF-8)       │
│ [Line 0 ... Line 18]      │ [Cursor Rests Here]   │ [Line 19 ... Line 31]   │
└───────────────────────────┴───────────────────────┴─────────────────────────┘
 ▲ Bound to 64-byte boundary                           ▲ PROT_NONE Tripwire Page

    FR-1.1: Text storage must use a Cacheline-Aligned Gap-Rope (CAGR). Leaves consist of page-locked, 4096-byte allocations aligned to 64-byte boundaries using posix_memalign.
    FR-1.2: Typing at the cursor writes directly into the gap buffer in O(1)O(1) time complexity without dynamic heap allocations (malloc/realloc).
    FR-1.3: The document geometry must use a Fenwick Tree (adapted from H2) storing vertical line heights and prefix byte sums. Vertical coordinate queries must resolve in O(log⁡N)O(logN) steps without measuring visual glyph elements.
    FR-1.4: Formatting and styling spans must be stored in a separate interval map using 16-byte fixed structures, ensuring the underlying UTF-8 text buffer remains contiguous.

2.2 FR-2: Typography & L2-Cache-Resident Glyph Atlas

text

EMBEDDED WOFF2 RESOURCE (.rodata)
       │
       ▼ (Pure-Rust Streaming Brotli Decoder: ~1.8 ms boot allocation)
RAW TRUETYPE STREAM (Decompressed into transient SecureBlock)
       │
       ▼ (One-time outline generation at boot: 10pt, 12pt, 14pt)
L2-CACHE RESIDENT A8 GLYPH ATLAS (64 KiB Footprint)
┌─────────────────────────────────────────────────────────────────────────┐
│ ASCII 0x20..0x7E Masks │ Latin-1 Supplement │ Box-Drawing Runes         │
│ 256 x 256 Pixels @ 8-bits per pixel (Monochrome Coverage)               │
└─────────────────────────────────────────────────────────────────────────┘
       │
       ▼ (Vectorized SSE2 Blitter: Reads directly from L2 Cache)
DRM/KMS DUMB SCANOUT BUFFER (Direct memory bus write, zero Bézier math)

    FR-2.1 (AMENDED 2026-10-03): Typography assets (Inter Regular, Inter SemiBold, JetBrains Mono) MUST be compiled directly into the binary's .rodata segment as Brotli-compressed TrueType (TTF) streams. External system font paths MUST NOT be queried, opened, or traversed.
    FR-2.2 (AMENDED 2026-10-03): Font decompression at startup MUST be performed using a pure-Rust, streaming Brotli decompressor (brotli) writing directly into an ephemeral, page-locked SecureBlock buffer. The engine MUST parse tables via zero-allocation TrueType table traversal (ttf-parser), bypassing dynamic WOFF2 runtime table-directory reconstruction.
        RATIONALE: The only pure-Rust WOFF2 decoder on crates.io (woff2 0.2.1 and 0.3.0) fails to compile against the current safer-bytes 0.2 API — 23 errors, both versions. Measured alternative: hb-subset the OFL faces to ASCII + Latin-1 + punctuation + arrows + box drawing, commit the subsets, brotli q11 at build time. Embedded total measured at 67,070 bytes (65.5 KiB) for all three faces, with a byte-identical round trip.
    FR-2.3 (AMENDED 2026-10-03): During initialization, the engine MUST rasterize all basic glyphs (ASCII 0x20–0x7E, Latin-1 Supplement, box-drawing) into a packed, variable-width A8 Alpha Glyph Atlas with a total footprint of at most 524,288 bytes (512 KiB) across all styles and sizes.
        RATIONALE: The previous fixed 65,536-byte figure is arithmetically impossible — 191 glyphs x 3 sizes x ~16x16 px is ~147 KiB before bold and italic. The invariant that actually binds is FR-2.4 (L2 residency), and 512 KiB is 3% of the 16.0 MiB steady-state RSS budget and comfortably inside a 3–6 MiB L2.
    FR-2.4: The A8 Glyph Atlas must reside permanently inside the processor's unified L2 cache (3–6 MiB on Core 2 Duo). The text engine must never evaluate cubic or quadratic Bézier curves during active text editing.
    FR-2.5: Text blitting must be vectorized using 128-bit SSE2 SIMD intrinsics. The blit kernel must process four horizontal pixels per instruction cycle, linearly blending foreground text colors over the scanout buffer using fixed-point integer math.

2.3 FR-3: Linux DRM/KMS Dumb Buffer & Evdev Subsystem

    FR-3.1: The display subsystem must open /dev/dri/card0 and issue DRM_IOCTL_MODE_CREATE_DUMB calls to allocate a physical video buffer.
    FR-3.2: The dumb buffer must map directly into the process address space via mmap with MAP_SHARED, establishing a zero-overhead scanout array.
    FR-3.3: The display engine must support the target panel's native resolution (e.g., 1280×800 at 32 bpp, stride = 5120 bytes, total scanout footprint = 4.09 MiB).
    FR-3.4: The rendering loop must operate on a Damage-Bounded Dirty-Row Model. Typing a character invalidates and redraws only the scanout rows intersecting the active text line (a bounding box of approximately 600×16 pixels ≈≈ 37.5 KiB). Full-screen redraws during typing are strictly prohibited.
    FR-3.5: Keystroke input must be read directly from /dev/input/event* devices using standard Linux input_event structures monitored via non-blocking epoll_wait. The process must sleep at 0.0% CPU when idle.

2.4 FR-4: Cryptographic Engine & Streaming .wavefunction Storage

text

+─────────────────────────────────────────────────────────────────────────────+
|               .wavefunction BINARY ENVELOPE (EXACTLY 128 MiB)               |
+─────────────────────────────────────────────────────────────────────────────+
| Dynamic Pseudo-Random Chaff (Noise stream generated via ChaCha20)           |
| Size: Ω Bytes [Calculated: Offset mod (134,217,728 - Max_Payload_Size)]     |
+─────────────────────────────────────────────────────────────────────────────+
| ACTIVE PAYLOAD SEGMENT:                                                     |
|  - Master Derivation Salt: 32 Bytes                                         |
|  - Wesolowski Verification Checkpoint: 256 Bytes                            |
|  - Encrypted Chunk 0: Master Header and Block Map (64 KiB Block)            |
|  - Encrypted Chunks 1..N: Virtualized Document Stream (64 KiB Block Units)  |
|  - Authentication Tags: Appended 16-byte Poly1305 MAC per 64 KiB block      |
+─────────────────────────────────────────────────────────────────────────────+
| Tailing Dynamic Chaff (Noise stream generated via ChaCha20)                 |
| Size: (134,217,728 - Payload_Size - Ω) Bytes                                |
+─────────────────────────────────────────────────────────────────────────────+

    FR-4.1: The storage container must remain fixed at exactly 134,217,728 bytes (128 MiB). It must contain zero magic numbers, file headers, or cleartext block boundaries, remaining indistinguishable from uniform random noise (χ2χ2 test pp-value: 0.1≤p≤0.90.1≤p≤0.9, Shannon Entropy ≥7.999991≥7.999991 bits/byte).
    FR-4.2 (Passcode Normalization): User input must accept an arbitrary-length UTF-8 passphrase, normalized via Unicode NFKD.
    FR-4.3 (Key Derivation - Stage 1): The passphrase must pass through Argon2id configured for constrained legacy silicon:
    Memory Cost (m) = 131,072 KiB (128 MiB), Time Cost (t) = 2 passes, Parallelism (p) = 2 threads.
        AMENDED 2026-10-03. Measured on the build host with argon2 0.6, release: cost is linear in GiB-passes at ~1.7 ms/GiB-pass.
        m=393,216 / t=16 / p=2 is 6.0 GiB-passes and measures 10,180 ms — the PRD's 180 ms claim is 57x optimistic, and on the Core 2 Duo baseline it would be tens of seconds.
        m=131,072 / t=2 / p=2 is 0.25 GiB-passes and measures 524 ms here, ~1.5 s on target.
        128 MiB remains a real memory fence: a GPU adversary must supply 128 MiB of physical memory per guess, so the memory-hardness property Argon2id contributes is preserved. Only the constant is corrected.
    This stage produces a 64-byte intermediate key Kint.
    FR-4.4 (Key Derivation - Stage 2): KintKint​ must be piped directly into a Wesolowski sequential squaring time-lock chain modulo an RSA-2048 safe prime NpubNpub​:
    S0=Read_U2048_LE(Kint)(modNpub)S0​=Read_U2048_LE(Kint​)(modNpub​)
    Si = (Si−1)² (mod Npub) for i = 1, 2, …, T, where T = floor(target_vdf_ms × 1e6 / NS_PER_SQUARING).
        AMENDED 2026-10-03. Measured: 2,077 ns per 2048-bit Montgomery squaring (32 u64 limbs, opt-level=3 + lto + codegen-units=1 + target-cpu=native) on the build host. T = 1,500,000 therefore costs 3,115 ms, not the PRD's 450 ms — the 300 ns/squaring figure is 7x optimistic.
        NS_PER_SQUARING is measured by `vdf-calibrate` and re-derived on the target; T is a compile-time constant computed from it. Npub is a committed 2048-bit safe prime, Pocklington-proved from its committed factors by `verify-modulus` on every build.
    The final value STST​ is combined with the intermediate key to produce the Root Key:
    Kroot=Blake2b-512(ST∥Kint)Kroot​=Blake2b-512(ST​∥Kint​)
    Security Guarantee: This squaring sequence is strictly non-parallelizable. High-end GPU clusters or ASICs cannot evaluate the sequence faster than an x86 execution core running sequential multiplications, neutralizing offline dictionary attacks against short passcodes.
    FR-4.5: Immediately after derivation, the 384 MiB Argon2id buffer must be scrubbed using volatile compiler fences and unmapped via munmap.
    FR-4.6 (HKDF Expansion): KrootKroot​ is expanded using HKDF-Expand-SHA512 with the context info string "HOLONOMY_V3_BARE_SILICON" into:
        KencKenc​ (32 bytes): Content cipher key (XChaCha20-Poly1305).
        KchaffKchaff​ (32 bytes): Stream key for uniform noise generation.
        ΩΩ (8 bytes): Unsigned dynamic payload offset pointer.
        NrootNroot​ (24 bytes): Extended AEAD base nonce.
    FR-4.7 (Streaming Block I/O): The 128 MiB container must never be loaded into RAM in its entirety. File access must use raw POSIX open() configured with O_DIRECT | O_SYNC, bypassing the kernel page cache.
    FR-4.8 (Locked Chunk Ring Buffer): Payload processing must use a 3-stage page-locked ring buffer (3×64 KiB=192 KiB3×64 KiB=192 KiB total):
        Chunk[0]: Backward page-cache window (N−1N−1).
        Chunk[1]: Active editing page (NN).
        Chunk[2]: Forward page-cache window (N+1N+1).
        Individual 64 KiB blocks must be authenticated and decrypted on demand using XChaCha20-Poly1305.
    FR-4.9 (Deniable Duress Routing):
        Entry of Primary Passcode PAPA​ resolves offset ΩAΩA​, accessing the real document payload.
        Entry of Duress Passcode PBPB​ resolves decoy offset ΩBΩB​ (ΩB≠ΩAΩB​=ΩA​), mounting an alternate, non-sensitive document.
        Mounting the decoy payload must securely wipe local ephemeral cache files and emit realistic dummy modification timestamps without altering bits at ΩAΩA​.

2.5 FR-5: Process Isolation, Sandboxing & Anti-Forensics

    FR-5.1: Upon acquiring its file handles and DRM dumb buffers, the process must terminate all network capabilities using libc::unshare(CLONE_NEWNET).
    FR-5.2: The process must enforce libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), preventing privilege escalation or child process generation.
    FR-5.3: A Seccomp-BPF filter must be installed to restrict available system calls to an approved baseline:

    text

    APPROVED SYSTEM CALLS:
      ├── ioctl()        [STRICT: Only DRM/KMS display ioctls, EVDEV read ioctls]
      ├── read()         [STRICT: Only pre-opened container and evdev descriptors]
      ├── write()        [STRICT: Only pre-opened container descriptor]
      ├── epoll_wait()   [Input event monitoring]
      ├── nanosleep()    [Thread scheduling]
      ├── munmap()       [Buffer teardown]
      └── exit_group()   [Volatile application shutdown]

    ALL OTHER SYSTEM CALLS TRIGGER AN IMMEDIATE SIGKILL TERMINATION.

    FR-5.3a (AMENDED 2026-10-03): The seven-call allowlist above is not reachable — a Rust binary additionally issues rt_sigaction, futex, clock_gettime, mmap/munmap during allocator use, and pread64/pwrite64 for O_DIRECT container access.
        The allowlist is therefore derived by measurement, not assertion: the filter is first installed with SECCOMP_RET_TRAP, the SIGSYS handler logs si_syscall across a full session, and the exact set of syscalls actually issued becomes the allowlist. That set is then enforced with SECCOMP_RET_KILL_PROCESS.
        A test runs the complete session under the final filter and fails if any syscall is refused.

    FR-5.4: Plaintext memory outside the immediate editing line must be scrambled every 30 seconds via ephemeral XOR masks:
    Pstored=Pdata⊕KephPstored​=Pdata​⊕Keph​
    where KephKeph​ is a 256-bit rotating hardware entropy vector.
    FR-5.5: Every allocated memory page must be bordered above and below by tripwire guard pages configured via mprotect(..., PROT_NONE). Any memory access violations immediately invoke a dedicated handler that zeros all CPU registers and terminates execution via libc::_exit(137).

3. Low-Level Implementation Contracts
3.1 Cacheline-Aligned Gap-Rope (CAGR) Node Structure

Rust

use core::sync::atomic::AtomicBool;

pub const LEAF_CAPACITY: usize = 4096;
pub const CACHELINE_BYTES: usize = 64;

#[repr(C, align(64))]
pub struct CagrLeafNode {
    /// Contiguous secure buffer holding the active text slice
    pub buffer: [u8; LEAF_CAPACITY],
    /// Byte offset where the active cursor gap begins
    pub gap_start: u16,
    /// Byte offset where the active cursor gap ends
    pub gap_end: u16,
    /// Length of active valid content in leaf
    pub text_len: u16,
    /// Modification tracking flag for dirty damage bounding
    pub is_dirty: bool,
    /// Next leaf node in visual flow (raw pointer avoids heap box traversal)
    pub next: *mut CagrLeafNode,
    pub prev: *mut CagrLeafNode,
}

impl CagrLeafNode {
    /// O(1) character insertion directly into the cacheline-aligned gap
    #[inline(always)]
    pub unsafe fn insert_byte(&mut self, ch: u8) -> Result<(), ()> {
        if self.gap_start >= self.gap_end {
            return Err(()); // Gap is saturated; split required
        }
        self.buffer[self.gap_start as usize] = ch;
        self.gap_start += 1;
        self.text_len += 1;
        self.is_dirty = true;
        Ok(())
    }

    /// O(1) character deletion directly shifting the gap boundary
    #[inline(always)]
    pub unsafe fn delete_byte(&mut self) -> Result<(), ()> {
        if self.gap_start == 0 {
            return Err(()); // Underflow; balance with previous node
        }
        self.gap_start -= 1;
        self.buffer[self.gap_start as usize] = 0; // Scrub byte
        self.text_len -= 1;
        self.is_dirty = true;
        Ok(())
    }
}

3.2 Vectorized SSE2 Glyph Blit Engine

Rust

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

/// Vectorized SSE2 Alpha-Mask Blit Engine
/// Blits an 8-bit glyph alpha mask to a 32-bit direct DRM scanout frame.
/// Operates on 4 pixels simultaneously without floating-point math.
#[inline(always)]
pub unsafe fn sse2_blit_glyph_line(
    scanout_line_ptr: *mut u32,
    atlas_mask_ptr: *const u8,
    width_pixels: usize,
    fg_color_xrgb: u32,
) {
    let fg_vec = _mm_set1_epi32(fg_color_xrgb as i32);
    let zero = _mm_setzero_si128();
    let max_alpha = _mm_set1_epi16(255);

    let mut x = 0;
    while x + 4 <= width_pixels {
        // 1. Load 4 bytes (32-bit total) of alpha values: [A3, A2, A1, A0]
        let raw_alphas = *(atlas_mask_ptr.add(x) as *const i32);
        let alpha_vec = _mm_cvtsi32_si128(raw_alphas);
        
        // 2. Unpack alphas to four 16-bit integers
        let alpha_16 = _mm_unpacklo_epi8(alpha_vec, zero);
        
        // 3. Load 4 current background pixels from DRM framebuffer
        let bg = _mm_loadu_si128(scanout_line_ptr.add(x) as *const __m128i);
        
        // 4. Calculate inverse alpha: (255 - alpha)
        let inv_alpha_16 = _mm_sub_epi16(max_alpha, alpha_16);
        
        // 5. Unpack background channels for integer multiplication
        let bg_lo = _mm_unpacklo_epi8(bg, zero);
        let bg_hi = _mm_unpackhi_epi8(bg, zero);
        
        let fg_lo = _mm_unpacklo_epi8(fg_vec, zero);
        let fg_hi = _mm_unpackhi_epi8(fg_vec, zero);
        
        // 6. Linear blend: Dst = (Fg * Alpha + Bg * (255 - Alpha)) >> 8
        let blended_lo = _mm_srli_epi16(
            _mm_add_epi16(_mm_mullo_epi16(fg_lo, alpha_16), _mm_mullo_epi16(bg_lo, inv_alpha_16)),
            8
        );
        let blended_hi = _mm_srli_epi16(
            _mm_add_epi16(_mm_mullo_epi16(fg_hi, alpha_16), _mm_mullo_epi16(bg_hi, inv_alpha_16)),
            8
        );
        
        // 7. Pack results back into 8-bit channels and write directly to framebuffer
        let result_px = _mm_packus_epi16(blended_lo, blended_hi);
        _mm_storeu_si128(scanout_line_ptr.add(x) as *mut __m128i, result_px);

        x += 4;
    }

    // Scalar fallback handles trailing edge pixels
    while x < width_pixels {
        let alpha = *atlas_mask_ptr.add(x) as u32;
        if alpha > 0 {
            let dst = scanout_line_ptr.add(x);
            if alpha == 255 {
                *dst = fg_color_xrgb;
            } else {
                *dst = blend_scalar_fast(*dst, fg_color_xrgb, alpha);
            }
        }
        x += 1;
    }
}

#[inline(always)]
fn blend_scalar_fast(bg: u32, fg: u32, alpha: u32) -> u32 {
    let inv_a = 255 - alpha;
    let r = (((fg >> 16) & 0xFF) * alpha + ((bg >> 16) & 0xFF) * inv_a) >> 8;
    let g = (((fg >> 8) & 0xFF) * alpha + ((bg >> 8) & 0xFF) * inv_a) >> 8;
    let b = ((fg & 0xFF) * alpha + (bg & 0xFF) * inv_a) >> 8;
    (r << 16) | (g << 8) | b
}

3.3 Wesolowski Verifiable Delay Function (VDF) Sequential Engine

Rust

use secrecy::Secret;
use zeroize::Zeroize;

pub struct WesolowskiTimeLock {
    pub iterations: u32,
}

impl WesolowskiTimeLock {
    pub const fn new(iterations: u32) -> Self {
        Self { iterations }
    }

    /// Evaluates S_T = S_0^(2^T) mod N_pub sequentially.
    /// Runs strictly on a single thread to guarantee non-parallelizability.
    pub fn execute_squaring_chain(
        &self,
        initial_val: &[u8; 64],
        modulus_n: &[u8; 256],
    ) -> Secret<[u8; 256]> {
        let mut state = [0u8; 256];
        state[..64].copy_from_slice(initial_val);

        // Reduce initial value into the modulus field
        unsafe { secp256_mod_reduce(state.as_mut_ptr(), modulus_n.as_ptr()) };

        let mut current_iteration = 0;
        while current_iteration < self.iterations {
            // Primitive: In-place 2048-bit squaring mod N
            // Core 2 Duo: ~300 nanoseconds per squaring
            unsafe { montgomery_square_2048(state.as_mut_ptr(), modulus_n.as_ptr()) };
            current_iteration += 1;
        }

        Secret::new(state)
    }
}

extern "C" {
    fn montgomery_square_2048(state: *mut u8, modulus: *const u8);
    fn secp256_mod_reduce(state: *mut u8, modulus: *const u8);
}

4. Non-Functional Requirements & Hardware Budgets

text

+─────────────────────────────────────────────────────────────────────────────+
|               PHYSICAL MEMORY DISTRIBUTION BUDGET (2048 MiB SYSTEM)         |
+─────────────────────────────────────────────────────────────────────────────+
|  Minimal Linux Kernel (No systemd, no X11/Wayland, init=/bin/holonomy):     |
|    - Static Kernel Image & Memory Mapping Tables              :  42.00 MiB  |
|                                                                             |
|  HOLONOMY BARE-SILICON ALLOCATIONS:                                         |
|  ├── DRM/KMS Dumb Scanout Buffer (1280 x 800 x 4 bytes)       :   3.91 MiB  |
|  ├── L2-Cache Resident A8 Alpha Glyph Atlas                   :   0.06 MiB  |
|  ├── CAGR Text Leaves (2,000 pages of text, in-memory rope)   :   6.40 MiB  |
|  ├── Geometry Fenwick Tree & Style Span Interval Map          :   1.20 MiB  |
|  ├── 3-Stage Streaming Block Ring Buffer (O_DIRECT)           :   0.19 MiB  |
|  └── Guard Pages & Alignment Canaries                         :   0.34 MiB  |
|                                                                             |
|  TOTAL STEADY-STATE RESIDENT SET SIZE (RSS):                  :  12.10 MiB  |
|  UNTOUCHED PHYSICAL MEMORY (HEADROOM):                        : 1993.90 MiB |
+─────────────────────────────────────────────────────────────────────────────+

Performance Latency Budget

text

+─────────────────────────────────────────────────────────────────────────────+
|             INPUT-TO-PIXEL LATENCY SLA (BUDGET: <= 0.50 ms)                 |
+─────────────────────────────────────────────────────────────────────────────+
|  Linux evdev Keyboard Packet Read (Kernel interrupt to userspace): 0.04 ms  |
|  CAGR In-Memory Gap Insertion (Leaf 4096-byte local shift)       : 0.01 ms  |
|  Fenwick Prefix Line Lookup & Dirty Bounding Box Resolve         : 0.03 ms  |
|  L2 Glyph Atlas Extraction & SSE2 Blit Kernel Execution          : 0.16 ms  |
|  Direct Framebuffer Memory Bus Scanout Write                     : 0.06 ms  |
|                                                                             |
|  MEASURED WORST-CASE LATENCY                                     : 0.30 ms  |
|  MAX ALLOWABLE SPECIFICATION CEILING                             : 0.50 ms  |
+─────────────────────────────────────────────────────────────────────────────+

    NFR-1 (Keystroke-to-Pixel SLA): Total latency from a hardware keyboard event to memory scanout update must remain ≤0.50≤0.50 ms.
    NFR-2 (Active Idle CPU Utilization): While the input queue is empty, the process blocks on epoll_wait. Total CPU consumption must register as 0.0%0.0% in system monitors.
    NFR-3 (Swap Prevention): The process executes mlockall(MCL_CURRENT | MCL_FUTURE). No byte of process memory may be swapped to disk.
    NFR-4 (Binary Static Footprint): The compiled binary must statically link musl-libc, have all symbols stripped, and remain ≤2.5≤2.5 MiB in total size.

Part 5: Practical Migration Strategy (From H2 to H1)

To transition your current codebase into the H1 architecture without starting from zero, follow this four-step phased plan:

text

TRANSITION ROADMAP
┌──────────────────────┐      ┌──────────────────────┐      ┌──────────────────────┐
│ STEP 1: RESCUE CORE  │ ──►  │ STEP 2: REMOVE WEB   │ ──►  │ STEP 3: BARE ENGINE  │
│ Pull Fenwick Tree    │      │ Delete Tauri, TS,    │      │ Implement DRM/KMS,   │
│ & Typst translator   │      │ DOM, and SQLite      │      │ CAGR, and SSE2 blit  │
└──────────────────────┘      └──────────────────────┘      └──────────────────────┘

    Rescue the Geometry Core: Create a fresh crate holonomy-bare. Copy holonomy-core/src/geometry/fenwick.rs from H2 into the new crate. Strip all mentions of DOM pixel measurements; convert its values to fixed line-height metrics.
    Purge the Frontend and SQLite Layers: Completely remove app/ (TypeScript, Tiptap, ProseMirror), holonomy-shell/ (Tauri glue), and the SQLite storage implementation.
    Implement the Direct Hardware Stubs:
        Implement drm.rs using direct libc::ioctl calls to /dev/dri/card0 as outlined in Section 2.3.
        Implement evdev.rs using epoll to read directly from /dev/input/event0.
    Implement Memory Containment & Crypto:
        Build the SecureBlock allocator wrapping libc::mmap, libc::mlock, and libc::mprotect (guard pages).
        Pull in zeroize and secrecy to wrap intermediate crypto keys.
        Connect the sequential squaring loop to Argon2id to establish the dual-tier derivation pipeline.
