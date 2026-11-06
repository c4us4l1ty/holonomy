# Holonomy `v5.1.0`
### An experimental encrypted document workspace

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust](https://img.shields.io/badge/Rust-1.95%2B-dea584.svg)](https://www.rust-lang.org/)
[![CI](https://github.com/c4us4l1ty/holonomy/actions/workflows/audit.yml/badge.svg)](https://github.com/c4us4l1ty/holonomy/actions/workflows/audit.yml)
[![Post-quantum: ML-KEM-1024](https://img.shields.io/badge/KEM-ML--KEM--1024-blueviolet.svg)](https://github.com/RustCrypto/KEMs)
[![Editor: Wordcraft planned](https://img.shields.io/badge/editor-Wordcraft%20planned-orange.svg)](https://github.com/storytold/wordcraft)

> **Prototype, not a finished security product.** Do not use Holonomy for sensitive or irreplaceable
> documents. It has not received an independent security review; its editor, physical-key
> provisioning, and end-to-end protections are incomplete.

Holonomy explores a rich-document workspace backed by a fixed-size encrypted container and a
physical USB entropy reservoir. The current deliverables include an experimental CLI and a
plain-text demo editor, not a finished word processor. The CLI can provision, save, and open a
byte-oriented document using a supplied passphrase, USB root share, and pad source. An experimental Wordcraft codec, editing-engine adapter and
session bridge live in a separate workspace, but they are not connected to an editor window. A separate scratch-only
window preview does not use containers or provide encryption. A separate restricted text-only
demo window uses containers with a deterministic stand-in pad, not physical USB protection.

This README describes **what exists and what is intended**, not a promise that the prototype is
safe against a particular attacker. See the [v5.1 design](docs/PRD-v5.1.0.md),
[build audit](docs/AUDIT-v5.1.0.md), and [earlier adversarial teardown](PRD_AUDIT.md) for context;
those documents are historical and code and current CI results govern implementation status.

---

## Status at a glance

| Area | Current state |
| :--- | :--- |
| Container and cryptographic primitives | Implemented in the core, crypto, session, USB, and CLI crates; still requires independent review. |
| CLI container round trip | Experimental `provision`, `save`, and `open` commands; credentials can be entered at a hidden terminal prompt. |
| USB pad | A device-backed Zone B reader and ratchet exist; without `--device`, a **deterministic demo pad** is used instead. Partial or invalid ratchet counts after replacement require recovery; a full count is only the source's report, not independent proof of fresh pad. Reporters carry pad freshness as *fresh*, *reused*, or *not established* — the store itself never claims the first two.. A save reads the replacement image back and compares its digest before ratcheting, so a torn or substituted replacement is refused while the old pad window is still intact. |
| USB provisioning / Zone A | Not implemented end to end. The CLI does not extract the root share from the device or initialize its entropy reservoir. |
| Editor | A restricted plain-text demo window uses the container session and deterministic demo pad. A read-only Wordcraft window (`holonomy-rich-view`) renders a container document with all persistence hooks refusing and all editing input dropped. The unencrypted scratch preview remains unconnected to containers. Nothing here edits a rich container document yet. |
| Reaper and capture protection | Library-level components exist; they are **not integrated into a running editor session**. No end-to-end wipe-latency guarantee is made. |
| Crash recovery | Unix saves stage and sync a replacement before renaming it, then sync the parent directory, read the replacement back before ratcheting, and hold an advisory save lock. `holonomy remnants` lists staged files; `holonomy recover` scans cursors; `holonomy recover-copy` can copy one authenticated remnant to a new path without replacing either source. This is not a complete recovery protocol: it does not choose the newest copy, repair the original, or reconcile pad state. Non-Unix saves are currently refused. |
| Pad budget | Provisioning starts at cursor 0; saves reject cursors 54 and above. This conservative bound is **not** durable pad accounting and cannot detect reuse across containers or restored backups. |

Unix staged saves replace the file inode. They preserve its permission bits, but may not preserve
ownership, ACLs, extended attributes, hard links, or other filesystem metadata. Saves take an
advisory lock on a `*.holonomy-lock` sidecar, which refuses a second Holonomy save but cannot stop
a program that ignores it or one that reaches the same inode by another path; it is coordination,
not a guarantee. Provisioning is not crash-atomic. Always keep independently tested backups
together with the pad material needed to open them.

Command-line secrets can be exposed through process listings, shell history, logs, and other host
facilities. Omit `--passphrase` and `--kappa` to use hidden terminal prompts instead. The legacy
passphrase mapping lowercases input (so ASCII case variants are equivalent) and does **not** perform
Unicode normalization. Changing that mapping requires a versioned migration for existing containers. `--check` reports several deferred checks and a successful exit is **not** a security
certification. Platforms, storage behavior, memory locking, and device I/O need further testing on
real hardware.

---

## Design: container, envelope, and pad

The following is a description of this prototype's construction, not a proof of secrecy.

### Fixed-size container

A container has 2,048 slots of 65,536 bytes each:

$$
2{,}048 \times 65{,}536 = 134{,}217{,}728\text{ bytes} = 128\text{ MiB}.
$$

Slot 0 carries the root capsule, slot 1 includes the epoch salt, and the remaining **2,046** slots
are available for payload. The session layer reserves authentication data in each payload slot. The experimental Wordcraft bridge
stores versioned JSON **inside** the encrypted payload and refuses unrecognized nonempty CLI data.
The experimental version 1 reader also rejects unknown fields and noncanonical JSON rather than
silently losing data; this may reject files produced by different serializers or future versions.
Keep original files, and do not assume this prototype has a migration path.
Its frontend session tracks edits and avoids spending a cursor on an unchanged save; after any store
failure it conservatively reports unsaved changes until recovery. Wordcraft's in-memory document,
saved snapshot, undo history and intermediate copies are not guaranteed to be wiped. The engine
adapter exposes only a few editing commands; it does not expose Wordcraft's general file or export
commands, which require separate integration and plaintext-leak review.
The file's fixed size does not hide its existence, access times, save frequency, or other external
metadata. A fixed size or random-looking ciphertext alone is not a proof that revisions leak no
information.

### Two-phase key envelope

The design separates the key used to unwrap the capsule from the key derived by decapsulation:

```text
passphrase + epoch salt ── Argon2id ──► root secret ──► K_unwrap
                                                          │
root capsule ── unwrap secret ──► ML-KEM-1024 + X25519 decapsulation
                                                          │
                   shared secrets + ciphertexts + κ_USB ──► K_DEM
```

The CLI's legacy Argon2id parameters are 256 MiB, four iterations, and two lanes. The higher-level
document-session options use that profile; the low-level experimental store still exposes a
weaker test-only profile and must not be used with it for real data. The code uses
ML-KEM-1024, X25519, XChaCha20-Poly1305, and a BLAKE2b-based commitment construction. The
non-circular separation of `K_unwrap` and `K_DEM` is a Holonomy design choice, not a claim that
these primitives or the resulting protocol are novel or formally proven. The present CLI requires
`κ_USB` as an argument; it does not establish that the share exists only on a physical stick.

### Pad-window arithmetic

Zone B is modeled as 1,782,579 sectors, with 32,768 sectors of 4,096 bytes per container:

$$
32{,}768 \times 4{,}096 = 134{,}217{,}728,\qquad
\left\lfloor\frac{1{,}782{,}579}{32{,}768}\right\rfloor = 54.
$$

A fixed, keyed 22-bit Feistel-based sector permutation maps logical window positions into Zone B.
With one unchanged permutation, windows 0–53 cover disjoint logical positions; window 54 begins
to overlap previously used positions. This is a **capacity calculation**, not a guarantee that
physical media were provisioned with secret independent entropy or that the CLI prevents reuse.
The CLI refuses save cursors 54 and above even on writable devices until it can verify pad history.
This does not detect a reused stick, another container, or rollback to an older snapshot. The writable
device path attempts to refresh the previous window after writing the new image; a
ratchet failure can occur **after** the container was changed. The current window must not be
ratcheted while it is needed to read that image. Ratcheting old windows can also make older container
backups unreadable unless the corresponding pad material is retained; backup and recovery are not yet
solved.

A one-time pad requires independent, secret material that is never reused. If the same pad bytes
mask two corresponding plaintext bytes, they cancel: $(M_1 \oplus P) \oplus (M_2 \oplus P) =
M_1 \oplus M_2$. The demo pad is derived deterministically and is **not** a one-time pad. Reuse,
backups, partial ratchets, and shared devices all require careful accounting that the CLI does not
yet provide.

### Watchdog design, not a deployed guarantee

The runtime separates a potentially blocking device probe from an atomics-only reaper loop. Explicit
suspend notifications are distinguished from missed heartbeats, and the intended fail-safe order is
wipe registered secrets, terminate, then best-effort blackout. This is library-level design work:
the CLI does not currently start an end-to-end USB watchdog for an editor, and timing, actual
coverage, OS suspend integration, and reliable blackout have not been established as product
properties.

> **Deliberately not claimed:** plaintext-free RAM or CPU registers; screenshot-proof display on
> every OS; unbreakable anti-debugging; a 15 ms wipe guarantee; or information-theoretic security
> from the deterministic demo pad. Previous proposals for these claims are examined in the audit.

---

## Crates

| Crate | Responsibility | Status |
| :--- | :--- | :--- |
| [`holonomy-core`](crates/holonomy-core) | Container geometry, capsule layout, sector permutation, budget and bookkeeping types. | Workspace library |
| [`holonomy-crypto`](crates/holonomy-crypto) | Argon2id, hybrid envelope, committed AEAD, pad accounting, and memory-locking helpers. | Workspace library |
| [`holonomy-usb`](crates/holonomy-usb) | Device geometry, aligned I/O, sector plans, and ratchet operations. | Workspace library; physical-device provisioning incomplete |
| [`holonomy-runtime`](crates/holonomy-runtime) | Reaper and secret-arena primitives. | Workspace library; not wired into the CLI editor |
| [`holonomy-session`](crates/holonomy-session) | Container sealing/opening, directories, session status, and checks. | Workspace library |
| [`holonomy-cli`](crates/holonomy-cli) | `holonomy` binary plus experimental file-store and document-session APIs, pad sources, and diagnostics. | Workspace binary and library; UI integration pending |
| [`holonomy-wordcraft`](crates/holonomy-wordcraft) | Versioned Wordcraft codec, frontend session, and limited editing-engine adapter. | Experimental separate workspace; no editor UI |
| [`holonomy-preview`](crates/holonomy-preview) | Explicitly unencrypted Wordcraft scratch-window integration preview. | Separate workspace; **never use sensitive data** |
| [`holonomy-ui`](crates/holonomy-ui) | Intended Wordcraft shell and capture-reporting code. | Outside workspace; unfinished |

[`holonomy-old/`](holonomy-old/) is a read-only prior generation, not a workspace dependency. Its
aligned direct-I/O rules informed the current USB I/O module.

## Open-source foundations & inspiration

Roles below distinguish current dependencies from planned integration. See [NOTICE](NOTICE) and the
upstream repositories for attribution and their license terms.

| Project | Role in Holonomy |
| :--- | :--- |
| [`storytold/wordcraft`](https://github.com/storytold/wordcraft) | Document model used by the experimental codec and intended editor foundation; the UI is not linked into the current binary. |
| [`RustCrypto/KEMs`](https://github.com/RustCrypto/KEMs) | `ml-kem`, used for the ML-KEM-1024 portion of the hybrid envelope. |
| [`dalek-cryptography/curve25519-dalek`](https://github.com/dalek-cryptography/curve25519-dalek) | X25519 key agreement via `x25519-dalek`; no Elligator mapping is claimed. |
| [`RustCrypto/AEADs`](https://github.com/RustCrypto/AEADs) and [`RustCrypto/hashes`](https://github.com/RustCrypto/hashes) | XChaCha20-Poly1305 and hash primitives used by the crypto layer. |
| [`RustCrypto/password-hashes`](https://github.com/RustCrypto/password-hashes) | Argon2id passphrase derivation. |
| [`RustCrypto/utils`](https://github.com/RustCrypto/utils) | `zeroize` helpers for secret-handling code. |
| [`holonomy-old`](holonomy-old/) | Prior-generation code and design reference, including direct-I/O alignment rules. |

These projects supply components or inspiration; their maintainers do not endorse Holonomy, and
upstream library security does not validate Holonomy's composition or implementation.

---

## Build and inspect

Requirements: Rust **1.95 or later** and access to the dependency sources. The main workspace
contains the CLI and five libraries; `holonomy-ui`, `holonomy-preview`, and the experimental
`holonomy-wordcraft` bridge are not members. The bridge uses the pinned Wordcraft document model and engine in its own
workspace. Its manifest repeats Wordcraft's pinned `krilla` patch because Cargo does not inherit
patches from dependency workspaces.

```bash
cargo build --release -p holonomy-cli
./target/release/holonomy --version
./target/release/holonomy --help
./target/release/holonomy --check
cargo test --workspace
# Experimental Wordcraft codec (separate workspace)
cargo test --manifest-path crates/holonomy-wordcraft/Cargo.toml
```

The audit workflow runs build, tests, lint, and focused CLI safety regressions. Use the
[Actions page](https://github.com/c4us4l1ty/holonomy/actions/workflows/audit.yml) for the actual
result of a particular commit; a badge is not an independent security audit.

The separate `holonomy-preview` is a **plaintext scratch-only development preview**, not a
container editor. It launches only with an explicit opt-in; Wordcraft's own file, export, print,
and clipboard actions can expose plaintext. Do not enter sensitive or irreplaceable data. It needs
a graphical desktop, native windowing libraries, and the pinned upstream git dependencies:

```bash
cargo run --manifest-path crates/holonomy-preview/Cargo.toml -- --unencrypted-scratch-preview
```

A different, restricted **plain-text demo container editor** has no Wordcraft file/export menus.
Editing refuses rich documents that it cannot preserve and asks for credentials in the terminal. It
uses the deterministic demo pad, **not** a physical USB key; it is not for sensitive or valuable
data. Container creation is not crash-atomic; track the displayed cursor to reopen and do not save
concurrently. Saves run on a worker; editing and closing are paused until the operation finishes.
Forced termination or power loss can still leave the container or pad needing recovery. This
prototype does not guarantee plaintext is wiped from Wordcraft, egui, the clipboard, or
operating-system memory. Unix saves only. The session status panel reports the demo pad, capture,
memory, and watchdog limitations. Its remaining-save count is only the local cursor limit, **not**
a count of verified fresh pads; during a save or after an uncertain failure the current on-disk
cursor is not confirmed. At the cursor limit the demo disables editing and saving because it has
no fresh-pad migration path. Use Ctrl+S (Command+S on macOS) to save.
After a successful save, the session status panel shows its unkeyed image fingerprint for comparison
with `holonomy verify`. It is hidden while saving or when recovery is required, and it is not
recorded across restarts. It is not an authenticity or durability proof; sharing it can correlate
container copies. The panel also shows the protection layer attached to the last successful save and
whether a ratchet was attempted. The store reports pad freshness as *not established*,
and a reported ratchet count does not independently establish device writes. See
[GUI implementation status](docs/GUI-IMPLEMENTATION.md) for the PRD gaps.

```bash
cargo run --manifest-path crates/holonomy-preview/Cargo.toml --bin holonomy-demo-editor -- --demo-create /path/to/new-demo-container
# Later, pass the cursor displayed after the last save:
cargo run --manifest-path crates/holonomy-preview/Cargo.toml --bin holonomy-demo-editor -- --demo-open /path/to/demo-container 1
# Inspect a candidate image without enabling editing or saving:
cargo run --manifest-path crates/holonomy-preview/Cargo.toml --bin holonomy-demo-editor -- --demo-inspect /path/to/demo-container 1
```

Read-only inspection authenticates and decodes the candidate at the supplied cursor. It can display
a plain-text body preview of a rich document, but the preview may omit formatting, images, tables,
headers, footers and other content; it is not the complete document. Editing and saving remain
disabled. Inspection does not repair a container or pad, establish pad freshness, or prove recovery
is complete. It uses the
same demo pad and plaintext-memory limitations. The OS may still update file-access metadata.

The file store requires a standalone file of exactly 134,217,728 bytes. It refuses shorter or
longer files rather than interpreting an embedded image or discarding trailing data on save.
An editor session records the encrypted image it opened or last saved, and refuses to save if a
later read differs. Before replacement, the store also compares the original encrypted bytes against
the image it read. These are best-effort change detectors, not locks: changes may not appear in the
GUI until a save is attempted, modification can race a check, and pad state is not coordinated.
Concurrent writers remain unsafe.
Even a refused replacement may have staged an encrypted image; do not retry after a failed save
without investigating the container and pad state.

If you no longer know which cursor an image needs (the usual state after a crash),
`holonomy recover /path/to/container` tries every cursor a save may legally have written (0 to 53)
and reports the ones that open, with the same unkeyed digest `holonomy verify` prints. It is
read-only: no write, no ratchet, no document bytes printed. Each cursor costs one read of the image
and one key derivation, so a scan can take a while, and it stops at the first hit unless `--all` is
passed. A hit proves the image opens with those credentials at that cursor; it does not prove the
image is fresh, undamaged, the newest copy, or the only copy, and it is not a repair. `--remnants`
runs the same scan over each `.holonomy-save-*` copy beside the container, because after a crash the
newest image may be the one that never finished its rename; it reports which cursor opens each copy
and says plainly when a leftover is not a whole image. It still ranks nothing and changes nothing.

If a staged copy authenticates and you want to preserve it without replacing the current file, use
`holonomy recover-copy /path/to/container --remnant /path/to/.holonomy-save-… --cursor N --output /path/to/new-copy`.
Choose the remnant and cursor reported by `recover --remnants`. The command re-authenticates that
specific candidate and creates a separate copy only when the destination does not already exist.
It prints the unkeyed image digest for comparison; that digest is not proof of authenticity or
durability and can correlate identical copies. The source remnant and current container are left
unchanged; the pad is not written or ratcheted. This preserves a candidate; it does not promote it,
identify the newest image, reconcile pad history, or complete recovery. Do not overwrite or delete
the originals while deciding what to do.

A save that dies between staging its image and renaming it leaves a `.holonomy-save-*` file next to
the container. `holonomy remnants /path/to/container` lists those files with their sizes and unkeyed
digests, needs no credentials, and decrypts nothing. It is a directory report, not a recovery
procedure: it cannot tell you whether the container is readable, and a listing does not make a
rejected save safe to repeat. Delete a remnant only after deciding it is not the copy you need.

A read-only Wordcraft window can render a container's document without offering any way to change
or export it: `holonomy-rich-view --view /path/to/container CURSOR`, or
`--inspect-only` to decode and report without a window. Hidden prompts supply the same credentials;
the pad is the deterministic demo stand-in. The window drops every key, text, paste, copy and click
event before the editor sees it, and every host hook that could save, print, download or write the
clipboard refuses. Press Ctrl+Q to close. The decoding itself performs no write and no ratchet, but
the document is plaintext in memory, so this is a viewing aid, not a protected reader.

To authenticate a single candidate without printing its document bytes, use
`holonomy verify /path/to/container --cursor N` and answer the hidden credential prompts. The
command opens the file and selected pad read-only and does not ratchet. It does decrypt into memory
and does not verify application document format, freshness, rollback history, or recovery. Only
the directory and declared payload are authenticated, not unused image bytes. The OS may update
access metadata. Its unkeyed encrypted-image fingerprint can be compared with a save report, but
is not an authenticity or durability proof; concurrent changes may yield a mixed snapshot. Sharing
it may correlate copies of a container. Omit `--device` only for the deterministic demo pad.

For experimental CLI usage, `--help` documents the arguments and hidden credential prompts. The provisioned image's
cursor is 0; `open` needs the cursor used to write the image and
`save` needs the **next** cursor. Keep track of it externally. An omitted `--device` selects the
demo pad, not physical USB protection. Do not paste real secrets into a shell command or point the
experimental device path at a disk containing valuable data. This tool does not provision a USB
stick for you.

## Remaining work

See the [release roadmap](docs/RELEASE-ROADMAP.md) for dependencies and completion gates.

- Integrate the Wordcraft editor window with the document-session bridge; validate compatibility and migration of the experimental payload format.
- Implement safe USB initialization and root-share storage/retrieval, and test real devices and OS paths.
- Complete and test crash recovery, cross-platform replacement, locking that covers writers outside Holonomy, and durable pad-use/ratchet accounting.
- Introduce a versioned, case-sensitive passphrase scheme with migration; integrate memory, watchdog, suspend, and capture protections.
- Measure resource use and latency under real workloads, and obtain independent cryptographic and security review.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. See
[NOTICE](NOTICE) for upstream attribution.
