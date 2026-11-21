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

## What is real, and what is not

Said here rather than only in `AUDIT.md`, because this is the page a reader meets first.

**Real, and run by CI on every push:** the sealed world-line and its append/witness path
(`holonomy-line`, with the L-6 crash rehearsal and the L-10 transfer measurement as separate CI
jobs); the Spine — slab tree, history, capacity-bounded resident set, a keystroke path whose
zero-allocation claim is measured by a counting allocator, and the commit boundary that seals an
edited slab into the world-line; the Eye's geometry — clusters, run tree, zoom/DPI, a counted
height index; the editor loop; and a **working terminal window** you can open and type into:

```text
cargo run -p holonomy-gui --bin holonomy -- notes.wavefunction
```

You will be asked for a passphrase, and the file is created if the path does not exist. Text types,
the caret moves, `Ctrl+S` writes through, `Ctrl+Z`/`Ctrl+Y` undo and redo a burst, `Ctrl+G` shows
what the process can honestly claim about its own memory, and what you typed is there when you open
it again.

**Not real yet, in the order it would bite a user:**

- **No GPU backend.** The window draws into a grid of characters. The Eye's geometry is the real
  measured one and the terminal is a surface over it, but there is no font shaping or pixel
  rendering: wide characters are not modelled (a `中` claims one cell and the terminal draws two —
  OQ-25), and there is no selection painting beyond what the cell grid can show.
- **A crash can lose up to 16 KiB of the most recent typing** (`store::CAPACITY`). Sealed text is
  durable the moment its buffer fills; the open buffer is written by `Ctrl+S`, by the autosave
  after 1.5 s of no typing, and on quit (D-64).
- **One document at a time**, one window, one caret. No panes, no search, no settings.
- **The DEM key of every opened document is leaked** for the life of the process (OQ-27), and the
  passphrase typed at the prompt is zeroed but was never a `Zeroizing` buffer (OQ-28).
- **The height index is exact only for what has been measured.** `Editor::reflow` measures the whole
  document — one leaf at a time, never more than one faulted in — and is called when a document is
  opened; a reveal on a document nobody measured measures the run to the caret instead (D-62).
- **No power-loss qualification.** The L-6 rehearsal drives a real ext4 filesystem out of space and
  checks the refusal is clean; it says so in its own job name because it is not a power-cut test,
  which is Block 10's.
