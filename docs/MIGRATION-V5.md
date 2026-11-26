# HOLONOMY v5.1.0 → v6 Migration Guide (`MIGRATION-V5.md`)

This guide explains how to migrate an existing **Holonomy v5.1.0** `128 MiB` `.holo` container
(`134,217,728` bytes, `2,046` permuted `64 KiB` slots) into an authenticated **Holonomy v6**
`.wavefunction` world-line.

---

## 1. Why Migration Is Explicit and One-Way

v5.1.0 and v6 have fundamentally different storage layouts and guarantees:

| Property | Holonomy v5.1.0 (`.holo`) | Holonomy v6 (`.wavefunction`) |
| --- | --- | --- |
| Container size | Fixed `128 MiB` (`2,048 × 64 KiB` slots) | Grows from ~10 KiB with prose; `16 KiB`–`64 KiB` segment chunks |
| Privacy & pad masking | `Feistel22` slot permutation + optional 128 MiB Zone-B USB pad (`54`-save domain budget) | Append-only authenticated world-line (`HOLWAV6\0`); zero raw-sector USB pad dependency |
| KEM / KDF envelope | Argon2id (`ParamsClass::Legacy`) + hybrid ML-KEM-1024 + X25519 + BLAKE2b512 root secret | Argon2id (`ParamsClass::Calibrated`) + ML-KEM-1024 + X25519 capsule + `BLAKE3` domain-separated `UnwrapKey` / `DEMKey` |
| Resident plaintext RAM | Loads full `128 MiB` image + `128 MiB` pad buffer | Bounded **streaming `O(CAPACITY)`** (`<= 16 KiB` scratch) during migration; `<= 8 MiB` viewport residency during editing |
| Compatibility | **One-way upgrade** — `.wavefunction` cannot be downgraded to `.holo` | Every migrated document immediately supports full witness history, `compact`, and multi-OS portability |

**Critical safety rule:** `holonomy migrate-v5` opens the v5.1.0 source **read-only**. It never
overwrites or erases your `.holo` container or Zone-B USB device. Keep your v5.1.0 backup until you
independently `holonomy verify` the migrated `.wavefunction`.

---

## 2. Streaming Memory Guarantee

Unlike v5.1.0's CLI, `holonomy migrate-v5` (`crates/holonomy-seal/src/v5.rs` and
`crates/holonomy-gui/src/transfer.rs`) **never materializes the 128 MiB container or 128 MiB pad
buffer in RAM**:

1. Authenticates `slot 0` (root capsule + wrapped ML-KEM-1024 / X25519 keys) and `slot 1`
   (`σ_epoch`, `64` bytes).
2. Unwraps and verifies the active stratum directory (`stratum 1..=4`, monotonic pad `cursor`).
3. Unfolds and decrypts **one `64 KiB` physical slot (`16 × 4 KiB` sectors)** at a time through
   `zeroize::Zeroizing` buffers.
4. Validates UTF-8 across slot seams (`Utf8Error` refuses corrupt slots cleanly without writing a
   partial container) and re-seals `<= 16 KiB` (`store::CAPACITY`) segments into the new
   `.wavefunction`.
5. Immediately calls `WorldLine::verify()` on the newly written `.wavefunction` before returning.

---

## 3. Command Reference & Pad Modes

```text
holonomy migrate-v5 [PAD OPTIONS] [--usb-key <path>] <source.holo> <destination.wavefunction>
```

### Prompt Sequence

`migrate-v5` prompts for two passphrases (read with echo disabled on TTY):
1. `unlock v5 container` — your existing v5.1.0 passphrase (raw bytes up to 255 UTF-8 bytes,
   matching v5.1.0's `V5Passphrase` rules).
2. `create v6 destination` — your new v6 passphrase (NFC-normalized, up to 4,096 bytes; you may
   reuse a strong passphrase or rotate to a new one during migration).

### Pad Modes (pick the mode matching how the `.holo` was saved)

1. **Software-derived pad (`--derived-cursor <n>`, default `0`):**
   Use when the v5.1.0 image was XOR-masked with v5's deterministic derived pad at save cursor `n`.
   ```bash
   holonomy migrate-v5 --derived-cursor 7 legacy.holo notes.wavefunction
   ```

2. **Automatic cursor scan (`--scan-cursor`):**
   If you forgot the save cursor (`0..=53`), `--scan-cursor` tries each candidate window against
   the slot-0 root commitment until the directory authenticates:
   ```bash
   holonomy migrate-v5 --scan-cursor legacy.holo notes.wavefunction
   ```

3. **Hardware Zone-B USB pad (`--device <path> [--cursor <n>] [--lba <n>]`):**
   Use when the v5.1.0 image was masked against physical Zone-B sectors (default `--lba 32768`):
   ```bash
   holonomy migrate-v5 --device /dev/disk/by-id/usb-zoneb --scan-cursor legacy.holo notes.wavefunction
   ```

4. **Unpadded raw image (`--unpadded`):**
   Use for legacy test or `--no-pad` containers without Zone-B XOR folding:
   ```bash
   holonomy migrate-v5 --unpadded legacy.holo notes.wavefunction
   ```

### Optional Hardware Token (`--usb-key <path>`)

If the v5.1.0 capsule folded a 32-byte physical USB token secret (`κ_USB`):
```bash
holonomy migrate-v5 --usb-key /mnt/token/kappa.bin --scan-cursor legacy.holo notes.wavefunction
```
The file must be **exactly 32 raw bytes**; it is read into `Zeroizing<[u8; 32]>` and wiped
immediately after key derivation.

---

## 4. Verification & Post-Migration Checklist

After migration prints:
```text
notes.wavefunction: migrated from v5.1.0 legacy.holo (stratum 1, cursor 7, 12 v5 slots -> 48 v6 segments, 785856 plaintext bytes, 48 verified)
```
complete these verification steps before retiring your `.holo`:

1. **Full authentication check:**
   ```bash
   holonomy verify notes.wavefunction
   ```
2. **Optional spot export to verify readable prose:**
   ```bash
   holonomy export --format plain notes.wavefunction verify.txt
   ```
   Remember that `export` writes unencrypted plaintext to disk (emitting
   `PLAINTEXT_EGRESS_DISCLOSURE`) — wipe `verify.txt` securely once checked.
3. **Open in Holonomy v6 GUI or TUI:**
   ```bash
   holonomy gui notes.wavefunction
   ```
   Open the security panel (`Ctrl+G`) to inspect live residency, `mlock` lock tier, and witness
   status.
4. **Preserve or shred your legacy v5.1.0 container:** Once `verify` succeeds on at least one
   copied backup of `notes.wavefunction`, retire `legacy.holo` according to your operational
   storage policy.
