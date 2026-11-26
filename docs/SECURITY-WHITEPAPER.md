# HOLONOMY v6 — SECURITY WHITEPAPER

*Companion reference to [`docs/THREAT-MODEL.md`](THREAT-MODEL.md), [`docs/GATES.md`](GATES.md),
and [`docs/WIPE_SUPERVISOR_REVIEW.md`](WIPE_SUPERVISOR_REVIEW.md).*

Holonomy v6 is a long-horizon zero-trust desktop/terminal document editor designed around **one
structural thesis**: you do not secure a 30-million-word document at rest and in RAM by trying to
wipe 180 MiB cleanly during an attack — you ensure **only the visible viewport neighborhood
(`<= 8 MiB`, target `4 MiB`) ever exists as decrypted plaintext in RAM**, and authenticate every
append-only version along an immutable, portable `.wavefunction` file.

---

## 1. Cryptographic Design (`holonomy-seal` & `holonomy-line`)

### 1.1 Key Schedule & Post-Quantum Hybrid Capsule

1. **Passphrase canonicalization (`PassphraseInput`):**
   - Bounded to `4,096` UTF-8 bytes prior to Unicode **NFC** normalization, rejecting empty
     inputs and embedded NUL (`\0`) bytes.
   - Normalized bytes stay inside `zeroize::Zeroizing<String>` and are scrubbed immediately on drop.
2. **Memory-hard root KDF (`Argon2id`):**
   - Parameter schedule (`ParamsClass::Calibrated`, stored inside the container header as
     `KdfParamsRecord`) derives root material bound to a fresh 32-byte per-container salt.
3. **Hybrid Post-Quantum Key Encapsulation (`ML-KEM-1024` + `X25519`):**
   - Every container header embeds a `LineHeader` (`4 KiB` header) carrying a fresh hybrid
     encapsulation under the container's PQ + classical keypair.
   - `MasterKeys` splits `K_unwrap` (`UnwrapKey`) from `K_DEM` (`DEMKey`) via domain-separated
     **BLAKE3** key derivation (`derive_key`), encapsulated behind Rust crate-level capability
     boundaries (`holonomy-seal` owns raw key bytes; `holonomy-text` and `holonomy-shell` cannot
     read `K_DEM` directly).
4. **Authenticated Container & Witness Chain (`HOLWAV6\0`):**
   - Each sealed text leaf (`RECORD_SEGMENT`, `<= 64 KiB`, live typing seals `<= 16 KiB`) is
     encrypted with **XChaCha20-Poly1305** using a unique random 192-bit nonce, committed via
     authenticated AAD binding container `file_id`, sequence index, and record tag.
   - Each manifest / save point appends a chained **BLAKE3** witness (`RECORD_WITNESS`, `v2`
     format carrying active `StyleMap` runs and deduplicated link URLs + timestamps/bookmarks).
   - `Document::verify()` streams and authenticates every segment and witness tag without trusting
     filenames, timestamps, or cached lengths.
5. **Re-keyed Compaction (`holonomy compact`):**
   - Deleting prose commits a new `RECORD_MANIFEST` spine excluding the deleted bytes so old text
     never loads on reopen (`OQ-29`/`OQ-30`), while prior encrypted witnesses remain navigable in
     read-only history until the user explicitly invokes `compact` with passphrase re-confirmation
     (`confirm_compaction`), which generates a fresh salt, fresh PQ capsule, and fresh `K_DEM` over
     only the active spine (`OQ-14`).

### 1.2 Honest Cryptographic Boundaries

- **Prefix rollback:** As recorded in `docs/THREAT-MODEL.md` §5 and `AUDIT OQ-7`, a cloud adversary
  who replaces a newer `.wavefunction` with a byte-exact older prefix produces a self-consistent
  authenticated file. Standalone single-file authentication proves integrity of the bytes present,
  not freshness against a third-party rollback without external monotonic state.
- **Third-party dependency intermediates:** Holonomy enables `zeroize` features on reviewed crypto
  crates, but cannot assert erasure of compiler/stack temporaries inside external Argon2/ML-KEM
  crates or `unicode-normalization` internals (`THREAT-MODEL.md` §5).

---

## 2. Bounded Plaintext Residency (`L-1`, `L-2`, `holonomy-text`)

Traditional editors decrypt a file into a monolithic UTF-8 gap buffer (`180 MiB+` at 30M words).
Holonomy v6 replaces that model with a persistent B-tree **Spine** (`Arc`-shared `Slab` descriptors
plus `StyleMap`) paired with **`ResidentSet`**:

- **Cold open (`L-2`):** Opening a 30-million-word (`> 180 MiB`) `.wavefunction` parses only the
  manifest and active `Slab` coordinates, fault-decrypting only the viewport neighborhood (`<= 2 MiB`
  at cold open, hard cap `BUDGET = 8 MiB`).
- **Scrolling & editing:** `ResidentSet::evict_if_needed` automatically evicts cold segments and
  overwrites each evicted segment's plaintext buffer with `zeroize::Zeroize` before deallocation.
- **No-plain-copy guarantee (`L-1`):** `Typing`, `Editor`, `WindowText`, and `FrameBuffer` never
  hold a whole-document string; searches (`find_nth`, `replace_all`) stream one decrypted leaf at a
  time in bounded memory (`docs/BOUNDED_SEARCH_AND_WIPE.md`).

---

## 3. Memory Protection, Cooperative Reaper & Canary Verification (`L-7`)

### 3.1 Key & Plaintext Allocation Accounting

`holonomy-seal` maintains a lock-free fixed-capacity table `SecretArenas` (`MAX_ARENAS = 2,048`
slots) with an atomic per-slot state machine (`EMPTY`, `INIT`, `ARMED`, `WIPING`, `UNREGISTERING`,
`READING`). On initialization of an editor session:

1. Master key cells attempt OS page locking:
   - **Unix:** `mlock` (`LockCapability::Resident`) + Linux `MADV_DONTDUMP`
     (`LockCapability::ResidentNoDump`) when permitted. (`memfd_secret` availability is probed for
     reporting, not used for key cell storage.)
   - **Windows:** `VirtualLock`.
   - When the OS refuses (`RLIMIT_MEMLOCK` / container cap), `Facts` degrades and reports
     `LockCapability::Unlocked` honestly — never claiming protection it did not obtain.
2. Live plaintext regions (`ResidentSet`, `Typing`, `WindowText`) are bridged into `SecretArenas`
   via `SealWipeBridge` (`WipeRegistry`), and key cells (`UnwrapKey`, `DEMKey`) arm into the same
   registry prior to wipe.
3. **Zeroize canaries (`CanarySet`):** 8 non-zero pseudo-random canary slabs (`256 B` each) are
   allocated on the same heap path and armed alongside real buffers. After `wipe()`, volatile
   read-back counts every surviving non-zero byte (`ghost_bytes`): any skipped or unarmed canary
   causes the process to emit `WipeUnconfirmed` and exit non-zero (`1`).

### 3.2 Emergency Triggers (`TripReason`)

| Trigger | How it fires |
| --- | --- |
| `TripReason::Manual` | User presses `Ctrl+L` in TUI or GUI |
| `TripReason::Suspend` | Win32 `WM_POWERBROADCAST` (`PBT_APMSUSPEND`), macOS `NSWorkspaceWillSleepNotification`, Unix `SIGUSR2`/`SIGTSTP`/`SIGPWR` |
| `TripReason::DeadmanTimeout` | Optional `--deadman <ms>` heartbeat deadline expires in background `ReaperGuard` |

When triggered, the window zeroes IME preedit buffers, clears OS clipboard ownership, arms
keys/plaintext, acknowledges quiescence, runs `SecretArenas::wipe()` (zero allocations, zero
locks, zero syscalls) plus `editor.wipe_all_in_place()`, and audits canary `ghost_bytes == 0`.

### 3.3 Why `L-7` Remains `spec` (External-Kill Limitation)

As audited in [`docs/WIPE_SUPERVISOR_REVIEW.md`](WIPE_SUPERVISOR_REVIEW.md), an **in-process**
cooperative reaper cannot intercept an external `SIGSTOP` + `SIGKILL` (`kill -9`) delivered by an
adversary to the live editor process itself. `crates/holonomy-seal/tests/supervisor_probe.rs`
demonstrates the Linux `clone3(CLONE_PIDFD)` + shared `memfd` out-of-process wipe prototype
(measured in CI under `gates`), while full shipped out-of-process supervisor isolation of live
editor plaintext across Unix/macOS/Windows remains incomplete and keeps `L-7` in `spec`.

---

## 4. Platform Window Security & Input Isolation

Every native backend (`cocoa.rs`, `win32.rs`, `x11.rs`) reports real capability status into the
7-line Security Panel (`Ctrl+G`) via `WindowSecurityInfo`:

| Surface | Screen-capture exclusion | Secure keyboard input | Clipboard & IME lifetime |
| --- | --- | --- | --- |
| **macOS AppKit (`cocoa.rs`)** | `NSWindow setSharingType:NSWindowSharingNone` (`Excluded` when confirmed) | HIToolbox `EnableSecureEventInput` (`Active` when confirmed) | `NSPasteboard` cleared via `changeCount` on `windowDidResignKey:` & exit; `NSTextInputClient` preedit scrubbed on focus loss/sleep |
| **Windows Win32 (`win32.rs`)** | `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` (`WDA_MONITOR` fallback) | `Unsupported` (Win32 shares desktop queue within integrity level) | `CF_UNICODETEXT` cleared on `WM_KILLFOCUS`, `WM_POWERBROADCAST`, `WM_DESTROYCLIPBOARD` & exit; `imm32.dll` preedit scrubbed on blur |
| **Unix X11 (`x11.rs`)** | `Unsupported` (X11 allows peer X clients to read root/window pixels) | `Unsupported` (X11 delivers unisolated key events) | `CLIPBOARD` ownership & `Zeroizing` payload cleared on `FocusOut`, `SelectionClear`, suspend & exit; dead-key/compose state cleared on blur |
| **Terminal TUI (`bin/holonomy`)** | `Unsupported` (outer terminal emulator owns display) | `Unsupported` (outer tty/compositor handles input) | Unmanaged by Holonomy; `TTY` raw echo disabled during passphrase prompts |

---

## 5. Verification Ledger (`L-1`–`L-10` Truth Status)

Holonomy maintains strict gate non-falsification in `crates/holonomy-ledger/src/laws.rs` and
`docs/GATES.md`:

- **Enforced (`gated`):** `L-1` (no whole-file plaintext copy; 30M-word cold-open & `find_nth` /
  `replace_all`), `L-2` (8 MiB `ResidentSet` bound + 1M random keystrokes + wipe-on-evict), `L-8`
  (coordinate-unified `View` hit-testing + RTL/BiDi UAX #9 ordering), and `L-9` (single-host
  zero-state `env -i` drill AND real Linux `x86_64` → macOS `aarch64` / Windows `x86_64` cross-OS
  file hand-off).
- **Partially measured (`spec`, still owed):** `L-3` (needs dedicated hardware NVMe/SSD baseline),
  `L-4` (software `PixelSurface` echo measured at 30M words; physical display/GPUI latency owed),
  `L-5` (headless `App::draw()` idle-skip verified; live desktop compositor power-draw measurement
  owed), `L-6` (short-write/fsync/ENOSPC loop ext4 verified; physical power-cut SSD/NVMe test owed),
  `L-7` (cooperative reaper & `clone3` supervisor probe verified; full out-of-process editor
  supervisor owed), `L-10` (rsync TCP delta over `.wavefunction` verified; Dropbox/iCloud/OneDrive
  sync clients owed).
