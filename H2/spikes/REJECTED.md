# Rejected alternatives

The audits behind these were done by reading vendored upstream source. Those
repos are no longer in the tree (see `.gitignore`); this file records what was
found, because the *reasoning* for rejecting them is part of the architecture
record and is not recoverable from the code.

## taino-edit (v0.7.0) — Rust ProseMirror, pure-Rust option

Vendored as `taino-edit-main`. The plan listed it as the "Rust-pure" alternative
to Tiptap. **Rejected: it delegates all layout to the browser, which is exactly
what a paginated word processor cannot do.**

- Framework-agnostic core + a `contenteditable` DOM bridge. No layout engine, no
  pagination, no text shaping — verified by search: no `swash`, `cosmic-text`,
  `rustybuzz`, `ttf-parser`, `wgpu`, `vello`, or `tiny-skia` in its `Cargo.lock`.
  The browser does all of it.
- Text is a plain `String` per text node. No rope. `ResolvedPos::resolve` is a
  linear scan per tree level, and `EditorView::update` re-walks every top-level
  child on every keystroke.
- Its own risk register targets "documents >10k nodes" with rope deferred. A
  2000-page document is far past that.
- CRDT support is a design document only: the `collab` feature is an empty flag,
  `loro` is not a dependency, and `COLLAB_DESIGN.md` is marked "draft for review
  — no implementation started."
- 214 host tests and CI-green, but ~4 months old, solo-maintained, and the author
  states it is their final planned contribution to the Rust ecosystem.

Its one transferable asset is the invertible `Step`/`Transform`/`Mapping` core,
which is conceptually what `UndoCoordinator` reimplements against Tiptap's steps.

## crdt-richtext (v0.1.1) — Peritext + Fugue CRDT

Vendored as `crdt-richtext-main`. **Rejected: research prototype, and a worse
version of what Loro already provides.**

- README: "The interface is not yet stable and is subject to changes. Do not use
  it in production."
- Flat text with marks. No block/paragraph tree, so a word processor's document
  model would have to be built on top of it.
- **No undo.** No history stack, no inverse ops. Erase is a new annotation with
  `Behavior::Delete`.
- No transactions, no snapshots, and the op log is append-only with tombstones
  that are never purged.
- ~123 Rust tests, of which 24 belong to a deprecated `legacy` module and 20 are
  minimized fuzz regression seeds.
- Requires a sibling `../generic-btree` checkout at a hardcoded path to build.
- Benchmarks are respectable (beats automerge ~5x and yjs ~6x on the B4 trace)
  but Loro's own wasm is faster on wall clock, and memory was never measured.

Note it is a Loro project (`loro-dev/crdt-richtext`), so choosing it over Loro
would mean forking a less mature implementation of the same author's ideas.

## CryptPad — E2E encrypted collaborative docs

Vendored as `cryptpad-main`, listed in the plan for CRDT and sync patterns.
**Not used.** Its relevance is real-time multi-user collaboration, which is an
explicit anti-goal. Nothing in its architecture applies to a single-user,
local-first document.

## Joplin — notes with sync

Vendored as `joplin-dev`, listed for "sync/storage patterns". **Not used.** It
is a note-taking app, not a word processor, and its sync is built around a
server-authoritative account model that a single-user local-first app does not
have. At 272MB it was also the largest thing in the tree for the least
architectural value.

The one durable idea taken from it: a write-ahead log plus periodic compaction,
which is what `crates/holonomy-core/src/wal.rs` implements.

## Tiptap Pages (paid) — pagination

Not vendored; a paid closed-source `@tiptap-pro/extension-pages`. **Rejected
even setting cost aside**, because its own documentation describes the failure
mode that matters most here:

> "Oversized non-splittable blocks cause an infinite layout loop. ... The layout
> will keep trying to push it forward, page after page, and never reach a stable
> state. The result is an infinite layout loop and an unresponsive editor. This is
> the most common way to render Pages unusable."

It also has no browser-print integration, and it would not fix the DOM scaling
problem, which is the actual constraint.

## What this means for re-deciding

If any of these choices is revisited, re-fetch the specific repo rather than
trusting this summary:

```
git clone --depth 1 https://github.com/<org>/<repo>
```

`tiptap-main` and `loro-main` remain the two whose internals are most likely to
need re-reading — Tiptap for the pagination question if the pageless decision is
ever revisited, Loro if mark-accumulation behaviour needs revisiting at a larger
section size.
