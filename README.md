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
passphrase. Then: printable text types, arrows and Home/End/PageUp/PageDown move the caret
(`Shift` extends), backspace and delete remove, `Ctrl+Z` undoes a burst and `Ctrl+Y` redoes one,
`Ctrl+A` selects all, `Ctrl+S` writes through, `Ctrl+F` opens find (Tab switches to replace-all),
`Ctrl+L` requests a cooperative emergency wipe, and `Ctrl+G` shows what the process can claim
about its own memory, `Esc` dismisses that panel, and `Ctrl+Q` or `Ctrl+C` quits after sealing what
is open — if that last save is refused, the window says so and exits non-zero rather than claiming
success (D-49).

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
| Eye geometry: clusters, UAX #9 BiDi, cell display width, run tree, zoom/DPI, height index | **Real, measured in CI** | `holonomy-eye`; 30 M-word real-`Face` viewport latency (`OQ-12`) + UAX #9 `script_gate` (`OQ-11`) |
| Editor loop: intents in, measured frames out | **Real, measured in CI** | `holonomy-shell` |
| Terminal window & CLI: open, type, save, unlock, panel, `verify`, `compact` | **Real — this is the app you run** | `holonomy-gui` |
| That binary, driven end to end by CI | **Real** — created, sealed, reopened, verified, compacted, wrong and empty passphrases refused | `scripts/smoke-install.sh` (D-72) |
| A packaged release or installer | **An unsigned Linux x86_64 tarball** per `v*` tag, built from the lockfile and smoke-tested before packaging (D-90). No installer, no signature, no macOS/Windows build | `docs/TO-APP.md` M8, `.github/workflows/release.yml` |
| GPU backend, font shaping, IME | **Not built** — the window is a grid of cells (`GSUB`/`GPOS` glyph shaping is OQ-24) | `docs/TO-APP.md` M4b-ii |
| Wide characters (`中`, Hangul, emoji) | **Real** — two-cell UCD 18.0.0 width in both `CellShaper` and terminal renderer (`Ambiguous` = 1 cell) | OQ-25 |
| Right-to-left & mixed BiDi text (Hebrew, Arabic, …) | **Real UAX #9 visual ordering + L-8 caret bijection** (`displaced = 0`, split selection spans); contextual glyph joining (`GSUB`/`GPOS`) is not built | OQ-11 (D-77), OQ-24 |
| Emergency wipe & zeroize canaries (`L-7`) | **Not qualified.** Historical headless tests wipe selected buffers and canaries; the live reaper is cooperative and can stall with the UI, so a blocked window cannot meet the 50 ms deadline. The window now reports a wipe by the reaper's own outcome and exits 1 when it cannot be confirmed (D-89), but that is a receipt for *registered* arenas, not complete plaintext coverage. A Linux isolated-supervisor test is not wired into the editor. | `holonomy-seal::reaper`, `holonomy-text/tests/wipe_gate.rs`, `docs/BOUNDED_SEARCH_AND_WIPE.md` (D-82–D-89) |
| Crash before a save | **Unsaved edits can be lost.** The open typing buffer and unsaved structural changes are not durable until a successful commit; physical power-loss behavior remains unqualified. | D-64, OQ-29/OQ-30 |
| Search and replace | **Available with limits.** Find streams matches; replace-all refuses more than 16,384 matches when the needle and replacement differ. Total-process-memory and dense-replacement gates remain open. | `holonomy-text::search`, `docs/BOUNDED_SEARCH_AND_WIPE.md` (D-85) |
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

**Zero of the ten laws are `gated`.** `docs/GATES.md` lists all ten as `spec`; that file is
generated from `crates/holonomy-ledger/src/laws.rs`, and `xtask gates --check` fails the build if
the two ever disagree. A law becomes `gated` when an enforcing block lands and CI measures it — not
before, and not by editing prose.

`docs/TO-APP.md` tracks the app in milestones; `ROADMAP.md` tracks the ten construction blocks.
**M0–M7 are done. M8 is partially done and M4b-ii is blocked** — see `docs/TO-APP.md`. The build is
not packaged and there is no per-platform release; the gates that remain are the ones about a
shipped app on reference hardware. Block 6's deliverable — a GPUI window with quiet-md's page — is
**not** met; its terminal half is.

## Installing it

**There is no installer, and nothing is signed.** What exists is an unsigned, experimental tarball
for one platform, published only on a `v*` tag by `.github/workflows/release.yml` (D-90). It is
built from the committed lockfile, driven through `scripts/smoke-install.sh` *before* it is packaged,
and it carries a `.sha256`, a `.provenance.txt`, and a `README-RELEASE.txt` whose first line says it
is unsigned and not for irreplaceable work. There is no macOS or Windows build, so for anyone else
the only way to run the app is from source with a Rust toolchain — clone the repository and run it
from there:

```text
cargo run --release -p holonomy-gui --bin holonomy -- notes.wavefunction
```

The binary is named `holonomy` if you want to install it yourself
(`cargo install --path crates/holonomy-gui`); note that the crate is marked `publish = false` and
that command is **not exercised by CI**, so treat it as untested. The `cargo run` line above is the
one the smoke test uses and the one this README can stand behind.

That is a real gap rather than a stylistic one: the remaining artifacts need signing and two more
platforms this project does not have, and L-9's "opens on any machine" claim cannot be measured
until the same file has been opened on a second machine — a tarball is the prerequisite for that
drill, not the drill itself.

## Verifying it yourself

The same commands CI runs, in order:

```text
cargo test --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo run -p xtask -- gates --check
scripts/smoke-install.sh
```

The last one builds the release binary and drives it through a real terminal — a passphrase, a
sentence, `Ctrl+Q` — then reopens it, and checks that a wrong and an empty passphrase are both
refused. It is the only check here that runs the binary you run, not a library underneath it.

`.github/workflows/ci.yml` is the compiler of record (AUDIT OQ-1): it runs `fmt`, `clippy`, `test`,
the gates contract, the L-6 and L-10 qualifications, the million-keystroke walk, the smoke test, and
posts diagnostics to issue #51 when a step fails. A change is verified when that run is green, not
when it reads correctly.
