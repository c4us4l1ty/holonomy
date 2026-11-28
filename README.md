## One-command Linux desktop (no source build)

```sh
git pull origin main
./holonomy notes.wavefunction
```

Or run `./holonomy` for Home. This downloads the source-matched **experimental**
Linux x86_64 desktop once, verifies its SHA-256, caches it under `target/`, and runs
GPUI. Subsequent launches use the cache: no Cargo, GCC, compiler settings, GitHub
login, or development packages. Python 3 and Git are required for this checkout
launcher; standard desktop graphics libraries/drivers are still required. It never
runs sudo, installs system packages, or silently switches to the old renderer.

Publication is gated on encrypted runtime tests of the **exact downloadable binaries**
in Fedora 44 and 45 containers, at 100% and 200%, using X11/Xvfb and software Vulkan.
This is a concrete compatibility test, **not a guarantee for every GPU, Wayland session,
Fedora variant, or future system update**. Test logs record installed package versions.
The fixture harness runs without Cargo or GCC in those Fedora containers and is never
included in the downloadable bundle. Real-monitor visual approval remains outstanding.

The first download needs internet; cached launches do not. A new desktop-source revision
needs its matching bundle; if qualification/publication has not finished, the launcher
reports this instead of compiling, choosing an older version, or changing toolchains.
Documentation-only pulls reuse the previous matching desktop build. Checksums detect
corruption but are not independent cryptographic signatures. Use test documents.

On a fresh checkout the bootstrap also creates `target/release/holonomy-gui` and its
companion as links, restoring the familiar command:

```sh
./target/release/holonomy-gui notes.wavefunction
```

It does not overwrite existing source-built executables or unrelated links. If you
already have those, **use `./holonomy notes.wavefunction`** to reliably select the
source-matched downloaded build. The source-build helper below remains available,
but is **not** required by the prebuilt launcher. Your old Cargo compiler overrides
have no effect on downloaded builds.

## Desktop UI: GPUI

The normal desktop entry points (`holonomy-gui` and `holonomy gui`) now launch the
GPUI frontend, not the legacy enlarged pixel surface. Build **both** the launcher
and its GPUI companion, or use:

```sh
scripts/run-desktop.sh /absolute/path/notes.wavefunction
```

### Building from source (optional)

Rust/Cargo via rustup is required; `rust-toolchain.toml` selects the pinned toolchain.
On Ubuntu/Debian, install the native build dependencies once:

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config clang cmake libxcb1-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev \
  libfontconfig1-dev libasound2-dev libssl-dev
```

On macOS, install the Xcode command-line tools (`xcode-select --install`). Then,
from your repository checkout in a graphical desktop session:

```sh
git pull origin main
./scripts/run-desktop.sh
# Or open an existing document (quote paths containing spaces):
./scripts/run-desktop.sh "$HOME/Documents/notes.wavefunction"
```

The helper builds both binaries in one Cargo invocation. The first build downloads
and compiles GPUI dependencies and can take several minutes; subsequent unchanged
builds are incremental. Relative document paths resolve from your current directory.
If your Linux driver reports Vulkan initialization errors, install your distribution's
Vulkan driver (for Mesa, typically `mesa-vulkan-drivers`); do not force a scale factor
as a substitute for configuring the desktop's actual 200% scaling.

For an optimized build (slower initial compile):

```sh
HOLONOMY_BUILD_PROFILE=release ./scripts/run-desktop.sh
```

Without Bash, build and run directly (also works in Windows PowerShell):

```sh
cargo build --locked -p holonomy-native -p holonomy-gui --bin holonomy-native --bin holonomy-gui --features holonomy-native/native-ui
# Linux/macOS:
./target/debug/holonomy-gui
# Windows PowerShell: .\target\debug\holonomy-gui.exe
```

**Windows currently cannot create/open encrypted documents** because the storage
backend refuses unqualified durability primitives. Building the UI does not remove
that restriction. On Linux/macOS, choose New for a new path or Open for an existing
`.wavefunction`, enter the path, Tab to the passphrase, and confirm. Use Ctrl+S
(Cmd+S on macOS); confirm save success before closing. Start with test documents.

Omit the path for Home. This uses device-scale vector document rendering and native
GPUI chrome; it does not scale a completed low-resolution framebuffer. The companion
must remain beside the launcher in installed bundles. Missing companions produce an
error, never a silent fallback to the old renderer. For explicit legacy compatibility
testing only, set `HOLONOMY_LEGACY_GUI=1`. The TUI/CLI is unchanged.

**Experimental, use test documents:** changing the desktop entry point does not certify
visual sharpness on your monitor or close the remaining security/platform/parity gates.
Windows encrypted persistence is still refused. See
[`crates/holonomy-native/README.md`](crates/holonomy-native/README.md) for exact limits.

# Holonomy

A writing instrument for one document that grows for a lifetime: thirty million words, sixty
years of pages, one sealed file — and only the page on your screen is ever unlocked.

- **`VISION.md`** — the constitution. The single law everything descends from:
  *never decrypt more than you look at.*
- **`ROADMAP.md`** — ten construction blocks, each behind measurable gates.
- **`docs/GATES.md`** — the budget contract, generated from code (`cargo run -p xtask -- gates`).
- **`docs/THREAT-MODEL.md`**, **`AUDIT.md`**, **`docs/rfc/`**, **`docs/UI-SPEC.md`** — the legal
  system the code builds under.

## Prior generations (read-only)

| Folder | Generation | Lesson |
| ------ | ---------- | ------ |
| `holonomy-old/` | H1 — v3.0.0-singularity, ~142k LOC, bespoke bare-silicon editor | The engine was real; the graphically unprincipled UI ate the project |
| `holonomy-v5-1-0/` | v5.1.0, ~23k LOC, audited PQ envelope + pad store | The trusted core was real; it never became a product |

Neither folder is a workspace member; both are prior art, kept unmodified.

## Building

Rust pinned to 1.100.0 (see `rust-toolchain.toml`). `cargo test --workspace` compiles and passes
today (CI proves it every push). The workspace holds the gates machinery (`holonomy-ledger`), the
seal (`holonomy-seal`, Block 2 — closed 2026-11-17 by scope amendment), the append-only world-line
(`holonomy-line`, Block 3 — closed in scope, with L-6/L-10 qualification owed to Blocks 6 and 10),
the Spine's first three slices (`holonomy-text`: immutable slab tree, history, capacity-bounded
resident set, an in-place keystroke path whose zero-allocation claim is measured by a counting
allocator in CI, the commit boundary that seals an edited slab into the world-line and path-copies
the spine onto it — proven against the real container — and the live-length overlay that keeps
positions correct while the spine is stale), the Eye's headless geometry (`holonomy-eye`: clusters,
one run tree, zoom/DPI, a counted height index, and the window join and reader D-18 added), the
editor loop above them (`holonomy-shell`: intents in, measured frames out — no window and no font),
the window over it (`holonomy-gui`: keymaps, a cell surface, the terminal front end, and the
security panel — the part you actually run, as
`cargo run -p holonomy-gui --bin holonomy -- notes.wavefunction`), plus `xtask`. Every unmeasured
law reads `spec` in `GATES.md`; that column is the honest one and nothing is promoted to `gated`
without a measurement in CI.

## What works today

**Experimental, not production-ready.** Do not rely on the current build for sensitive or
irreplaceable work. Security and durability gates remain open; see
[`docs/BOUNDED_SEARCH_AND_WIPE.md`](docs/BOUNDED_SEARCH_AND_WIPE.md) and `AUDIT.md`.

You can open a document and write in it, and what you write survives closing the app and opening
it again.

```text
cargo run -p holonomy-gui --bin holonomy -- notes.wavefunction
```

The file is created if it does not exist and opened if it does; either way it asks for a
passphrase. When launched without a file, the desktop app opens a **Home** library instead: a
sidebar of folders containing `*.wavefunction` documents, a searchable card grid, and `+` to create
a new encrypted document. Set `HOLONOMY_NOTES_DIR` to point the library at another folder; by
default it scans the working directory. The Home grid and the writing view share quiet.md-style chrome — a frosted folder rail, stacked folder cards, and a breadcrumb row — painted from file and folder names only, never from document plaintext (`docs/UI-SPEC.md` §10). `Ctrl+J` focuses search, `Ctrl+Q` closes the window, and the Home button, the rail, or `Ctrl+Shift+H` saves
and returns to the library from a document. Display-scale shortcuts work in Home too, and the
Settings panel shows the active scale.

Then: printable text types, arrows and Home/End/PageUp/PageDown move the caret
(`Shift` extends, `Ctrl/Option+Arrow` steps by word), backspace and delete remove
(`Ctrl/Option+Backspace/Delete` removes by word), `Ctrl+Z` undoes a burst and `Ctrl+Y` redoes one,
`Ctrl+A` selects all, `Ctrl+S` writes through, `Ctrl+F` opens find (Tab switches to replace-all),
`Ctrl+K` edits hyperlinks (GUI), `Ctrl+R` (TUI) / `Ctrl+H` (GUI) opens witness history,
`Ctrl+Shift+D` toggles dark paper (`#1A1A19`), `Ctrl+L` requests a cooperative emergency wipe,
`Ctrl+G` shows what the process can claim about its own memory and OS window protections,
`Ctrl+/` opens the static shortcut sheet, `Esc` dismisses overlays, and `Ctrl+Q` or `Ctrl+C` quits after sealing what is open — if that last save
is refused, the window says so and exits non-zero rather than claiming success (D-49).

The **display scale** control (`Scale 100%` in the toolbar, or `⋯` when the window is narrow)
cycles the entire desktop view through `100 / 125 / 133 / 150 / 175 / 200%` — the paper, toolbar,
status strip, history drawer, dialogs, and click targets all scale together. `Ctrl+Alt+=` /
`Ctrl+Alt+-` step the same presets and `Ctrl+Alt+0` resets to `100%`. The choice is remembered in
`~/.config/holonomy/display-scale.txt` (`%APPDATA%\holonomy\display-scale.txt` on Windows); set
`HOLONOMY_UI_SCALE=150` to override it at startup. This is window/display scaling, distinct from
the document zoom buttons (`-` / `+`).

A wipe is reported by its own evidence, on that same policy (D-89). The window keeps the reaper's
measured outcome instead of discarding it: **confirmed** prints the bytes, arenas, canaries and
microseconds it actually scrubbed and exits 0; an outcome that is absent, that scrubbed no arena,
or in which a canary still held a non-zero byte prints the reason — `emergency wipe FAILED
verification` when a ghost survived — and **exits 1**. That is a receipt about the arenas the
editor registered, not a proof of complete coverage; see the row below.

A **seal** is the unit of durability: when the 16 KiB buffer fills, its bytes are appended to the
world-line and witnessed, so text is on disk once it has sealed. The open buffer and any structural
edits (mid-segment insertions, cuts, or post-save undos) are committed by `Ctrl+S`, by an autosave
1.5 s after typing stops, and on quit (`RECORD_MANIFEST` + Witness v2, OQ-29 / OQ-30).

**Deleting text removes it from the document; only compaction removes its ciphertext from the
log.** An append-only world-line keeps prior segments by design (OQ-14): saving after a deletion
commits the updated sub-segment spine manifest with zero segment rewrites so deleted text never
returns on reopen, while `holonomy compact <source.wavefunction> <destination.wavefunction>`
re-confirms your passphrase and writes a fresh, re-keyed world-line containing only the surviving
active text (`holonomy verify <document.wavefunction>` authenticates every segment and manifest).

## What is real, and what is not

Said here, in a table, because this is the page a reader meets first and `AUDIT.md` is where the
detail lives.

| Area | State | Where the truth is |
| ---- | ----- | ------------------ |
| Sealed world-line, append + witness + spine manifest + compaction | **Real, measured in CI** | `holonomy-line`; L-6 and L-10 are separate CI jobs |
| Slab spine, history, resident set | **Real, measured in CI** | `holonomy-text`; zero-allocation keystroke claim counted by an allocator in CI |
| Commit boundary (seal + manifest → world-line) | **Real, measured in CI** | `holonomy-text`; proven against the real container |
| Eye geometry: clusters, UAX #9 BiDi, `GDEF`/`GSUB`/`GPOS` OpenType shaping, cell display width, run tree, zoom/DPI, height index | **Real, measured in CI (`L-8` gated)** | `holonomy-eye`; 30 M-word real-`Face` viewport latency (`OQ-12`), `GSUB`/`GPOS` (`OQ-24`), + UAX #9 `script_gate` (`OQ-11`) |
| Editor loop: intents in, measured frames out | **Real, measured in CI** | `holonomy-shell` |
| Terminal window, cross-OS native desktop (`X11` / `Cocoa` / `Win32`), & CLI: open, type, save, unlock, panel, `gui`, `import`, `export`, `migrate-v5`, `verify`, `compact` | **Real** (`holonomy <doc>` PTY verified + `holonomy-gui` Xvfb framebuffer/xdotool E2E verified on Linux + cross-compiled/tested on macOS & Windows) | `holonomy-gui`, `holonomy-eye::raster`, `scripts/qualify-x11-gui.sh`, `AUDIT.md` D-94/D-95 |
| That binary, driven end to end by CI | **Real** — created, sealed, reopened, `verify`, `compact`, `import`, `export`, wrong/empty passphrases refused | `scripts/smoke-install.sh` (D-72), `tests/transfer_migrate.rs` |
| A packaged release or installer | **Unsigned archives for Linux x86_64, macOS arm64, and Windows x86_64** per `v*` tag, built from the lockfile and smoke-tested before packaging. No installer, no signing/notarization | `docs/TO-APP.md` M8, `.github/workflows/release.yml` |
| Opening a copied file on a fresh profile (`L-9`) | **Enforced (`L-9` gated)**: single-host `env -i` zero-state drill (`portability`) + Linux `x86_64` → macOS `aarch64` & Windows `x86_64` cross-OS hand-off with passphrase alone (`l9-cross-os`) | `scripts/qualify-l9-portability.sh`, `.github/workflows/ci.yml` |
| Platform IME / dead keys & OS suspend tripwires | **Implemented in native windows** — `NSTextInputClient` + `NSWorkspaceWillSleepNotification` (`Cocoa`), `imm32.dll` `WM_IME_*` + `PBT_APMSUSPEND` (`Win32`), dead-key/`Multi_key` compose + `SIGUSR2`/`SIGTSTP`/`SIGPWR` (`X11` & TUI). GPUI desktop now builds under `--locked`; full native IME/suspend qualification remains open | `holonomy-gui/src/{cocoa,win32,x11,canvas,bin/holonomy}.rs` (`D-96`) |
| Wide characters (`中`, Hangul, emoji) | **Real** — two-cell UCD 18.0.0 width in both `CellShaper` and terminal renderer (`Ambiguous` = 1 cell) | OQ-25 |
| Right-to-left & mixed BiDi text (Hebrew, Arabic, …) | **Real UAX #9 visual ordering + L-8 caret bijection + `GDEF`/`GSUB`/`GPOS` shaping** (`displaced = 0`, split selection spans, kerning/ligatures/marks) | OQ-11 (D-77), OQ-24 (D-95) |
| Emergency wipe & zeroize canaries (`L-7`) | **Not qualified.** Historical headless tests wipe selected buffers and canaries; the live reaper is cooperative and can stall with the UI, so a blocked window cannot meet the 50 ms deadline. The window now reports a wipe by the reaper's own outcome and exits 1 when it cannot be confirmed (D-89), but that is a receipt for *registered* arenas, not complete plaintext coverage. A Linux isolated-supervisor test is not wired into the editor. | `holonomy-seal::reaper`, `holonomy-text/tests/wipe_gate.rs`, `docs/BOUNDED_SEARCH_AND_WIPE.md` (D-82–D-89) |
| Crash before a save | **Unsaved edits can be lost.** The open typing buffer and unsaved structural changes are not durable until a successful commit. `SIGKILL` after a witnessed commit is now exercised end to end against the shipped binary (`HOLONOMY_CRASH_RECOVERY`); physical power-loss behavior remains unqualified. | D-64, OQ-29/OQ-30, D-97 |
| Search and replace | **Streaming find + streaming dense replace-all** — `find_all` still refuses > 16,384 positions, while `replace_all` streams dense replacements (`30,013,649` matches in CI, `1.83 MiB` heap delta vs `4 MiB` bound) via `StreamingTx` whenever the sink supports streaming | `holonomy-text::search`, `AUDIT.md` D-94, `docs/BOUNDED_SEARCH_AND_WIPE.md` |
| Documents open at once | **One**, one window, one caret. No panes or settings | — |
| DEM key & passphrase buffers | **Owned and zeroized on drop** — `WorldLine` owns `DEMKey` (`OQ-27`), and prompt input is pre-allocated + NFC-normalized in `PassphraseInput` (`OQ-28`) | `holonomy-seal`, `holonomy-line`, `holonomy-gui` |
| Height index | Exact for measured leaves; a reveal measures the run to the caret | D-62 |
| Mid-document edits & deletions | **Durable across reopen** via sealed sub-segment spine manifests (`RECORD_MANIFEST`), with zero segment rewrites on deletion | OQ-29, OQ-30 |
| Deleted bytes on disk before compaction | **Retained as ciphertext** in the append-only log until explicit `holonomy compact <src> <dst>` writes a fresh re-keyed world-line | OQ-14 |
| Power-loss safety | **Not qualified** — L-6 is an ENOSPC rehearsal and says so in its job name | Block 10 |

CI exercises the implemented paths under the scopes stated in the linked tests and audit entries.
A passing test is not qualification for every workload, platform, security property or failure mode.
One document per window is a scope decision, not a defect.

## Status

**Four of the ten laws (`L-1`, `L-2`, `L-8`, `L-9`) are `gated`.** The remaining six (`L-3`..`L-7`,
`L-10`) remain `spec` in `docs/GATES.md`; that file is generated from
`crates/holonomy-ledger/src/laws.rs`, and `xtask gates --check` fails the build if the two ever
disagree. A law becomes `gated` only when its enforcing measurements run cleanly in CI — not by
editing prose.

`docs/TO-APP.md` tracks the app in milestones; `ROADMAP.md` tracks the ten construction blocks.
**M0–M7 are done, M8 produces unsigned release bundles for Linux, macOS, and Windows, and
pure-FFI X11/Cocoa/Win32 desktop windows replace GPUI in this offline `--locked` tree** — see
`docs/TO-APP.md` and `AUDIT.md` D-94/D-95.

## Installing it

**There is no installer, and nothing is signed or notarized.** `.github/workflows/release.yml`
builds unsigned experimental archives for Linux (`x86_64`), macOS (`aarch64`), and Windows
(`x86_64`) on `v*` tags from the committed lockfile, drives the Linux binary through
`scripts/smoke-install.sh` before packaging, and ships `.sha256`, `.provenance.txt`, and
`README-RELEASE.txt`. You can also build from source with a Rust toolchain:

```text
cargo run --release -p holonomy-gui --bin holonomy -- notes.wavefunction
cargo run --release -p holonomy-gui --bin holonomy-gui -- notes.wavefunction
```

**Every documented block gate is now checked only where measured:** `L-1`, `L-2`, `L-8`, and
`L-9` (`4/10` laws) are `gated`; hardware reference-benchmark (`L-3`, `L-4`, `L-5`), physical
power-cut (`L-6`), out-of-process `SIGSTOP`/`SIGKILL` editor supervisor (`L-7`), and external
third-party sync clients (`L-10`) remain honestly open in `docs/GATES.md`.

Documentation: see [`docs/MIGRATION-V5.md`](docs/MIGRATION-V5.md) for v5.1.0 `.holo` → v6
migration and [`docs/SECURITY-WHITEPAPER.md`](docs/SECURITY-WHITEPAPER.md) for the full
cryptographic, memory-protection, and platform-boundary specification.

## Verifying it yourself

The same commands CI runs, in order:

```text
cargo test --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo run -p xtask -- gates --check
scripts/smoke-install.sh
scripts/qualify-crash-recovery.sh
```

The last one builds the release binary and drives it through a real terminal — a passphrase, a
sentence, `Ctrl+Q` — then reopens it, and checks that a wrong and an empty passphrase are both
refused. It is the only check here that runs the binary you run, not a library underneath it.

`.github/workflows/ci.yml` is the compiler of record (AUDIT OQ-1): it runs `fmt`, `clippy`, `test`,
the gates contract, the L-6 and L-10 qualifications, the million-keystroke walk, the smoke test, and
posts diagnostics to issue #51 when a step fails. A change is verified when that run is green, not
when it reads correctly.
