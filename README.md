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

You can open a document and write in it, and what you write survives closing the app and opening
it again.

```text
cargo run -p holonomy-gui --bin holonomy -- notes.wavefunction
```

The file is created if it does not exist and opened if it does; either way it asks for a
passphrase. Then: printable text types, arrows and Home/End/PageUp/PageDown move the caret
(`Shift` extends), backspace and delete remove, `Ctrl+Z` undoes a burst and `Ctrl+Y` redoes one,
`Ctrl+A` selects all, `Ctrl+S` writes through, `Ctrl+G` shows what the process can honestly claim
about its own memory, `Esc` dismisses that panel, and `Ctrl+Q` or `Ctrl+C` quits after sealing what
is open — if that last save is refused, the window says so and exits non-zero rather than claiming
success (D-49).

A **seal** is the unit of durability: when the 16 KiB buffer fills, its bytes are appended to the
world-line and witnessed, so text is on disk once it has sealed. The open buffer is written by
`Ctrl+S`, by an autosave 1.5 s after typing stops, and on quit.

## What is real, and what is not

Said here, in a table, because this is the page a reader meets first and `AUDIT.md` is where the
detail lives.

| Area | State | Where the truth is |
| ---- | ----- | ------------------ |
| Sealed world-line, append + witness | **Real, measured in CI** | `holonomy-line`; L-6 and L-10 are separate CI jobs |
| Slab spine, history, resident set | **Real, measured in CI** | `holonomy-text`; zero-allocation keystroke claim counted by an allocator in CI |
| Commit boundary (seal → world-line) | **Real, measured in CI** | `holonomy-text`; proven against the real container |
| Eye geometry: clusters, run tree, zoom/DPI, height index | **Real, measured in CI** | `holonomy-eye` |
| Editor loop: intents in, measured frames out | **Real, measured in CI** | `holonomy-shell` |
| Terminal window: open, type, save, unlock, panel | **Real — this is the app you run** | `holonomy-gui` |
| GPU backend, font shaping, IME | **Not built** — the window is a grid of characters | `docs/TO-APP.md` M4b-ii |
| Wide characters (`中`, most emoji) | **Wrong** — claims one cell, terminal draws two | OQ-25 |
| Right-to-left text (Hebrew, Arabic, …) | **Drawn left-to-right.** A caret never splits a letter's mark, but the visual order is wrong — there is no bidi reordering | OQ-11, D-66 |
| Crash before a save | **Loses up to 16 KiB** of the most recent typing; nothing older | D-64 |
| Documents open at once | **One**, one window, one caret. No panes, search, or settings | — |
| DEM key of each opened document | **Leaked** for the process lifetime | OQ-27 |
| Passphrase at the prompt | Zeroed, but never held in a `Zeroizing` buffer | OQ-28 |
| Height index | Exact for measured leaves; a reveal measures the run to the caret | D-62 |
| Power-loss safety | **Not qualified** — L-6 is an ENOSPC rehearsal and says so in its job name | Block 10 |

Nothing in this table is aspirational. Every **real** row is exercised by a CI job on every push;
every **not built** or **wrong** row names an `AUDIT.md` entry, except the one that is a scope
decision rather than a defect — one document per window — which is the shape of the app and not
owed to anyone.

## Status

**Zero of the ten laws are `gated`.** `docs/GATES.md` lists all ten as `spec`; that file is
generated from `crates/holonomy-ledger/src/laws.rs`, and `xtask gates --check` fails the build if
the two ever disagree. A law becomes `gated` when an enforcing block lands and CI measures it — not
before, and not by editing prose.

`docs/TO-APP.md` tracks the app in milestones; `ROADMAP.md` tracks the ten construction blocks.
M0–M6 are done and M7/M8 are not: the build is not packaged, and the gates that remain are the ones
about a shipped app on reference hardware. Block 6's deliverable — a GPUI window with quiet-md's
page — is **not** met; its terminal half is.

## Installing it

**There is no packaged release.** Nothing is published and there is no installer, so the only way to
run the app today is from source with a Rust toolchain — clone the repository and run it from there:

```text
cargo run --release -p holonomy-gui --bin holonomy -- notes.wavefunction
```

The binary is named `holonomy` if you want to install it yourself
(`cargo install --path crates/holonomy-gui`); note that the crate is marked `publish = false` and
that command is **not exercised by CI**, so treat it as untested. The `cargo run` line above is the
one the smoke test uses and the one this README can stand behind.

That is a real gap rather than a stylistic one: a per-platform artifact needs signing and release
infrastructure this project does not have yet, and L-9's "opens on any machine" claim cannot be
measured until the same file has been opened on a second one.

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
