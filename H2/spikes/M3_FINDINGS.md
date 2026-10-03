# M3 — global undo and boundary traversal

**Status:** both directives implemented and verified. 39 tests, `app/test/run.ts`.
Typecheck clean. Tests run in real Chromium against real ProseMirror instances,
because the behaviour under test is `handleKeyDown`, selection resolution, and
the interaction between an editor's view and its document state — none of which a
DOM shim reproduces faithfully.

## Directive 1 — global undo/redo across instances

M0 chose one editor per section, so each editor's history is its own. Undo would
have trapped inside the focused section forever.

**Approach.** Tiptap's per-instance `UndoRedo` is disabled. All history flows
through `UndoCoordinator`, which stores ProseMirror `Step`s rather than document
snapshots (~24KB per keystroke) or hand-written inverse operations. Each entry
captures both the forward steps and `step.invert(doc)` for the rollback, plus a
`Mapping` of everything that changed in that section afterwards, so an entry's
absolute positions can be re-based onto the live document before being applied.

**Verified:**

| behaviour | result |
|---|---|
| undo crosses into a previous section | `undo 1 → s1`, `undo 2 → s0` |
| undo reaches a section that has unmounted | re-mounts and reverts |
| undo is not self-recording | third undo returns `null` |
| redo replays in reverse order | `redo 1 → s0`, `redo 2 → s1` |
| a new edit clears the redo branch | `redoDepth: 0` |
| a typing burst is one undo step | 10 keystrokes → `undoDepth: 1` |
| switching section breaks the group | two sections → `undoDepth: 2` |
| a merge is one undo step | 150 → 139 in one press |

## Directive 2 — boundary traversal

ArrowUp at position 0, ArrowDown at the end, and Backspace at position 0 are
intercepted before ProseMirror's default handling and handed to the registry,
which is the only object that knows the full section list.

**Verified:** all three cross; none of them fire mid-section, at the document
start, or at the document end; crossing into an unmounted section mounts it; and
typing continues correctly into the section just crossed into.

The user-facing form of the last one matters most — after ArrowDown the caret is
in the next section and `insertContent` lands there, so the seam is invisible.

## Visual column preservation

The first implementation carried a **character offset** from the section edge
(`COLUMN_ATTEMPTS = [0, 4, 8, 16, 32]`) and hoped the neighbour's line structure
was similar. It is not: an offset of 8 lands mid-word in a 40-character line and
at the end of a 5-character one, so crossing between two differently-formatted
paragraphs moved the caret somewhere unrelated.

Replaced with ProseMirror's own coordinate translation, which converts in both
directions and needs no guess:

```ts
// leaving: read the column from the live layout
const left = view.coordsAtPos(pos).left
// entering: ask the target's layout where that column lands
prevView.posAtCoords({ left, top: prevView.dom.getBoundingClientRect().bottom - 4 })
```

The 4px inset puts the probe inside the line box rather than on the boundary
between two lines, where `posAtCoords` is ambiguous. A hint that cannot be
resolved (no layout box, `inside: -1`, a position that is not selectable) falls
back to the offset path rather than failing, because refusing to move the caret
would trap the user at the seam.

**Verified:** the resolved position equals what `posAtCoords` independently
reports (`caretFrom === layoutPos`), and it is *not* the offset answer
(`57` vs `1` on a fixture built to make the two disagree). Drift across the seam
is under one character width.

Two findings worth recording:

- **The distinguishing test needs an asymmetric fixture.** The default fixture
  gives every section identically shaped lines, so a character-offset
  implementation and a coordinate-based one land on the *same character* and the
  test passes either way. `loadRaw` exists so a test can control line lengths.
  Two earlier versions of this test were vacuous for exactly that reason.
- **The tolerance is one character width, not a pixel count.** A caret can only
  sit between characters, so resolving column X on a line of different content
  snaps to the nearest character boundary, up to a full glyph away. Measured
  drift was 1.97px against a 3.88px character. A fixed 1px bound fails on that,
  and would pass a real jump on a narrow font.

## Six bugs this found

Every one produced a plausible-looking result rather than an error, which is why
the tests assert on document state and not on "did it throw".

1. **The boundary plugin was never installed.** `boundaryTraversal` returned a
   bare ProseMirror `Plugin` for Tiptap's `extensions` array, which accepts only
   `Extension` instances and **silently discards** anything else. No arrow key
   crossed a boundary and no Backspace merged. This is the same bug as M0 finding
   #3 — documented in the M0 findings, then repeated verbatim.
2. **`capture()` read `editor.state.tr`,** which after a dispatch is a fresh
   *empty* transaction. Nothing was ever captured, `undoDepth` stayed 0, and undo
   did nothing with no error. The dispatched `transaction` is now passed in.
3. **`Mapping.appendMapping` takes a `Mapping`,** not a `StepMap` and not an array
   of them. Passing `tr.mapping.maps` threw
   `Cannot read properties of undefined (reading 'length')` from inside
   ProseMirror, surfacing as an exception during ordinary typing.
4. **Undo recorded itself as a new edit.** Undo dispatches through the editor,
   which fires the same `transaction` event `capture()` listens to. The trace
   showed `undoDepth: 1` with `pending: 1` right after the first undo, and a
   second undo popped the same entry and re-applied it — so `BBB` came back and a
   third undo did the same. Fixed with a per-section `applyingTo` guard.
5. **`tr.insert` rejected both raw JSON and a `Slice`.** It wants a `Fragment`.
   Both wrong shapes fail identically at every position, so they read as position
   bugs: `Can not convert [object Object] to a Fragment` and
   `Can not convert <heading("Section 1")>(0,0) to a Fragment`. Four call shapes
   were compared directly to find the one that appends a child.
6. **The merge inserted at the wrong position.** `content.size - 1` is the last
   block's *closing token*, not its end, so merged text landed inside the previous
   section's final paragraph and the section got *smaller* (137 → 136). The test
   asserted growth, which is what exposed it.

### One design change the tests forced

A boundary merge is a single user action that necessarily changes two documents.
Modelling an entry as one section plus flat step arrays could not express it: the
earlier version suppressed capture for coordinator-initiated transactions, so the
merge was never recorded and could never be undone — text moved into the previous
section with no way to take it back.

Entries are now a list of per-section `UndoPart`s, applied in reverse on undo and
forward on redo, and **fully planned before anything is dispatched** so a
multi-section entry can never land half-applied.

## Decisions taken

- **Cross-section selection is an accepted limitation.** Clicking and dragging
  across a seam selects to the section edge and stops. A virtual selection layer
  would mean drawing synthetic overlays across multiple `contenteditable`
  boundaries, which brings its own accessibility and maintenance cost for a
  capability the architecture does not require. Recorded here as an intended
  constraint of the multi-instance strategy, not an oversight.
- **Undo is in-memory only, 500 entries, not persisted across a reload.** This
  follows from the no-version-history decision. The `undo_log` table in
  `schema.rs`, which had been designed for a snapshot-based undo, was removed:
  with no writer and no reader it was dead schema implying a capability that does
  not exist.

## Open gaps

- **IME composition across a seam is untested**, as it was in M0. Untestable in
  headless Chromium.
- **`mergeBackward` only merges a leading textblock.** A table, image, or equation
  at the seam is deliberately left alone rather than moved implicitly.
- The section splitter does not yet consult the thresholds that
  `mergeBackward` changes, so merging can leave a section over budget. The
  accounting is deferred to `onChange`, and the Rust side has the thresholds, but
  nothing enforces them on the JS path yet.
