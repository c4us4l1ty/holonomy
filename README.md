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
positions correct while the spine is stale, but with no editor above them yet) and a law-carrying
stub for the Eye, plus `xtask`. Every unmeasured law reads
`spec` in `GATES.md`; that column is the honest one and nothing is promoted to `gated` without a
measurement in CI.
