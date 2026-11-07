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
| USB pad | A device-backed Zone B reader and ratchet exist; without `--device`, a **deterministic demo pad** is used instead. Partial or invalid ratchet counts after replacement require recovery; a full count is only the source's report, not independent proof of fresh pad. Reporters carry pad freshness as *fresh*, *reused*, or *not established* — the store itself never claims the first two. A save reads the replacement image back and compares its digest before ratcheting, so a torn or substituted replacement is refused while the old pad window is still intact. |
| USB provisioning / Zone A | Not implemented end to end. The CLI does not extract the root share from the device or initialize its entropy reservoir. |
| Editor | A restricted plain-text demo window uses the container session and deterministic demo pad. The `holonomy-ui` binary and `holonomy-rich-view` preview alias can render a container in read-only Wordcraft mode, with persistence hooks refusing and editing input dropped. The unencrypted scratch preview remains unconnected to containers. Nothing here edits a rich container document yet. |
| Reaper and capture protection | Library-level components exist; they are **not integrated into a running editor session**. No end-to-end wipe-latency guarantee is made. |
| Crash recovery | Unix saves stage and sync a replacement before renaming it, then sync the parent directory, read the replacement back before ratcheting, and hold an advisory save lock. New staged names carry a forgeable device/inode plus prior-image digest marker; legacy names remain unbound and require `recover-copy --allow-unbound`. `holonomy remnants` lists regular files matching staged-image names; `holonomy recover` scans cursors; `holonomy recover-copy` can copy one candidate that opens at the supplied cursor to a new path without replacing either source. This is not a complete recovery protocol: it does not choose the newest copy, repair the original, or reconcile pad state. Non-Unix saves are currently refused. |
| Pad budget | Provisioning starts at cursor 0; saves reject cursors 54 and above. This conservative bound is **not** durable pad accounting and cannot detect reuse across containers or restored backups. |

Unix staged saves replace the file inode. They preserve its permission bits, but may not preserve
ownership, ACLs, extended attributes, or other filesystem metadata; writable saves now refuse a
container with multiple hard links to avoid leaving a stale fork. Saves take an advisory lock on a
`*.holonomy-lock` sidecar, normalizing the parent directory so relative-path spellings share the
lock. The lock cannot stop a program that ignores it or all aliases/races, so it is coordination,
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

A fixed, keyed, custom 22-bit Feistel permutation maps logical window positions into Zone B.
It is not a reviewed pseudorandom permutation and provides no access-pattern concealment guarantee;
the relied-on property is injective placement for valid inputs. Cycle-walking starts inside Zone B
and therefore returns within at most `2^22` steps. The often-quoted ~2.35 steps is only an
ideal-random-permutation average, not a latency or security bound.

With one unchanged permutation, windows 0–53 cover disjoint logical positions. Only 13,107 Zone B
sectors remain afterward, fewer than one full window; any complete next window would reuse at least
19,661 sectors. This is a **capacity calculation**, not evidence that physical media were provisioned
with secret independent entropy or that the CLI can verify device history. Checked planning and save
paths reject cursor 54 and above; they do not detect a reused stick, another container, or rollback.
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
| [`holonomy-preview`](crates/holonomy-preview) | Unencrypted scratch window, restricted container demo, and read-only Wordcraft viewer. | Separate workspace; demo pad only; **never use sensitive data** |
| [`holonomy-ui`](crates/holonomy-ui) | Read-only Wordcraft container viewer plus capture/status helpers. | Standalone workspace; demo pad only; no editing, saving, physical USB, or recovery |

[`holonomy-old/`](holonomy-old/) is a read-only prior generation, not a workspace dependency. Its
aligned direct-I/O rules informed the current USB I/O module.

## Open-source foundations & inspiration

Roles below distinguish current dependencies from planned integration. See [NOTICE](NOTICE) and the
upstream repositories for attribution and their license terms.

| Project | Role in Holonomy |
| :--- | :--- |
| [`storytold/wordcraft`](https://github.com/storytold/wordcraft) | Document model and renderer used by the experimental codec and read-only viewer; editable UI is not integrated. |
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
`holonomy-wordcraft` bridge are separate workspaces. The UI has a separate manifest so its
Wordcraft dependencies stay out of the core workspace; it provides an experimental read-only
viewer, not a full editor. The viewer uses the deterministic demo pad, not physical USB. The bridge
and UI manifests repeat Wordcraft's pinned dependency patches because Cargo does not inherit
patches from dependency workspaces.

```bash
cargo build --release -p holonomy-cli
./target/release/holonomy --version
./target/release/holonomy --help
./target/release/holonomy --check
cargo test --workspace
# Experimental Wordcraft codec (separate workspace)
cargo test --manifest-path crates/holonomy-wordcraft/Cargo.toml
# Experimental UI (separate workspace)
cargo test --manifest-path crates/holonomy-ui/Cargo.toml
# Interactive read-only open screen (graphical desktop required)
cargo run --manifest-path crates/holonomy-ui/Cargo.toml
cargo run --manifest-path crates/holonomy-ui/Cargo.toml -- --view /path/to/container CURSOR
# Headless decode/report without opening a window
cargo run --manifest-path crates/holonomy-ui/Cargo.toml -- --inspect-only /path/to/container CURSOR
```

The audit workflow runs build, tests, lint, and focused CLI safety regressions. Use the
[Actions page](https://github.com/c4us4l1ty/holonomy/actions/workflows/audit.yml) for the actual
result of a particular commit; a badge is not an independent security audit.

The separate `holonomy-preview` package contains an opt-in unencrypted Wordcraft scratch window,
a restricted plain-text container demo, and the read-only rich viewer. These are integration
prototypes, not a production editor; all use the deterministic demo pad rather than physical USB.
The scratch window launches only with an explicit opt-in. Wordcraft's own file, export, print, and
clipboard actions can expose plaintext. Do not enter sensitive or irreplaceable data. The windows
need a graphical desktop, native windowing libraries, and the pinned upstream git dependencies:

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
runs the same scan over each regular `.holonomy-save-*` file beside the container. A genuine staged
image may be the newest image after a crash, but the name alone does not prove that any candidate
came from a save; the command reports which cursor opens each candidate and when its size is not a
whole image. It tests at most the 20 lexicographically first matching regular files and reports
truncation; this cap bounds expensive cursor scans, not the directory enumeration. `recover-copy`
checks an explicitly selected sibling directly, so the display cap does not block copying it. A
candidate that opens proves only that the format's authenticated
structures and declared payload open with those credentials and cursor; unused image bytes,
freshness and recency are not thereby authenticated. The filename's optional origin marker is only
operational provenance, not cryptographic authentication, as described below. To scan
one specific candidate beyond the listing cap, run `recover` on that candidate path.

Newly staged `.holonomy-save-v2-*` names encode the original container's Unix device/inode and a
32-byte unkeyed digest over the complete pre-save image. This catches common wrong-container selections and detects a
container replaced or changed in place. It is only unauthenticated provenance metadata: a writer of
the directory can forge it, and it proves neither who created the remnant nor freshness or recency.
The check is not atomic with later reads or authentication; concurrent writers remain unsafe.
Older `.holonomy-save-*` names have no such association and are reported as legacy/unbound.

If a staged copy authenticates and you want to preserve it without replacing the current file, use
`holonomy recover-copy /path/to/container --remnant /path/to/.holonomy-save-… --cursor N --output /path/to/new-copy`.
Legacy/unbound candidates additionally require `--allow-unbound`, an explicit acknowledgement that
the name cannot associate them with this container. Choose the remnant and cursor reported by
`recover --remnants`, or run `recover` on a specific candidate path if the listing was truncated.
The command re-authenticates that candidate and creates a separate copy only when the destination
does not already exist. It syncs the staged file and, on Unix, the containing directory; it then
reads the installed file back and compares its full-image byte digest with the candidate before
reporting success. These checks catch ordinary write and read-back errors, not hardware that lies
about flushes. The printed unkeyed digest is for comparison, not proof of authenticity or durability,
and can correlate identical copies. Neither container nor remnant contents are written;
filesystem access metadata may change, and the pad is not written or ratcheted. This preserves a
candidate; it does not promote it, identify the newest image, reconcile pad history, or complete
recovery. Do not overwrite or delete
the originals while deciding what to do.

A save that dies between staging its image and renaming it leaves a `.holonomy-save-*` file next to
the container. `holonomy remnants /path/to/container` lists those files with their sizes and unkeyed
digests, needs no credentials, and decrypts nothing. It is a directory report, not a recovery
procedure: it cannot tell you whether the container is readable, and a listing does not make a
rejected save safe to repeat. Delete a remnant only after deciding it is not the copy you need.

A read-only Wordcraft window can render a container's document without offering any way to change
or export it. The standalone UI binary opens an interactive screen with no arguments, or accepts
`--view PATH CURSOR` and `--inspect-only PATH CURSOR`; the preview alias `holonomy-rich-view` has
the same command-line modes. CLI modes use hidden terminal prompts; the interactive screen masks
credentials visually but does not guarantee that all UI allocations are locked or wiped. Every
mode uses the deterministic demo pad, not physical USB. The renderer drops editing input and every
host hook that could save, print, download or write the clipboard refuses. In the interactive viewer,
Ctrl/Cmd+O returns to the open form and Ctrl/Cmd+Q closes; the standalone `--view` mode supports
Ctrl/Cmd+Q to close. Decoding performs no write and no ratchet, but the document is plaintext in memory and capture
exclusion, editor-wide memory locking, and the watchdog are not integrated. This is a viewing aid,
not a protected reader.

The interactive screen also offers **Find cursor** for a container when its cursor is unknown. It
checks legal candidates read-only, reports progress and the first one that authenticates, and can be
cancelled between full-image attempts. Each attempt reads the image and derives a key, so the search
may take time. It does not identify the newest or only copy, establish pad history or freshness, or repair the
container; credentials are cleared after the search and must be entered again to open it.

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
