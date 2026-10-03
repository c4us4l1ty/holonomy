/**
 * The section registry: owns the live editor instances and mediates everything
 * that crosses a section boundary.
 *
 * # Why a registry
 *
 * M0 chose strategy C, one editor per mounted section, because window slides
 * measured 10-100x cheaper than re-serialising a merged document. The cost of
 * that choice is that the editor no longer knows it is part of a larger document:
 * undo stops at a section edge, arrow keys stop at a section edge, and
 * Backspace stops at a section edge.
 *
 * This registry is where the document-level behaviour is restored. It is the
 * only object that knows the full section list, so it is the only place that can
 * answer "what is before this?" or "which editor holds that history?".
 *
 * # Windowing
 *
 * Only `windowSize` editors exist at once, centred on the focused section. M0
 * measured that mounting three sections costs ~800 DOM nodes, which is the
 * mitigation for the webkit2gtk large-DOM weakness. Unmounting serialises the
 * section back to a plain JSON blob and hands it to the store, so nothing is
 * lost; the editor object itself is destroyed so its ProseMirror state and view
 * descriptions are collectable.
 *
 * Undo entries for unmounted sections are handled by the coordinator, which asks
 * this registry to re-mount on demand. That is what makes history global rather
 * than per-window.
 */

import { Editor } from '@tiptap/core'
import type { Schema } from '@tiptap/pm/model'
import {
  boundaryTraversal,
  isSectionEmpty,
  jsonToFragment,
  placeCaretAtEdge,
  type BoundaryHost,
  type CrossingTarget,
} from './boundary.js'
import { UndoCoordinator } from './undo.js'

export interface SectionRecord {
  id: string
  /** ProseMirror JSON. The source of truth while the section is unmounted. */
  json: unknown
  /**
   * Derived counts for this section.
   *
   * `blocks` is the top-level block count, and it is **required** rather than
   * optional. It was optional once, and the gap mattered more than it looked:
   * layout height depends far more on block structure than on character count — an
   * 8000-character section that is one paragraph renders 2070px and the same
   * characters as ten paragraphs render 2373px — so a caller that omitted it
   * silently pushed every height estimate onto a character-derived fallback that
   * measures at 225% error on short dense sections and 41% on long sparse ones.
   *
   * Making it required means that failure is a type error at the call site rather
   * than a quietly wrong scrollbar. See `estimateHeight` in `main.ts`,
   * which no longer has a fallback to take, and DOCTRINE.md §6.
   */
  metrics: { words: number; marks: number; chars: number; blocks: number }
  /**
   * Whether `json` is the section's real content.
   *
   * `false` means the manifest row arrived without its bytes — a section past the boot
   * window — and `json` is a placeholder. Nothing may mount it: an editor built on a
   * placeholder would destroy the stored section on its first keystroke.
   *
   * Required rather than optional because "is this record real?" has to be answered at
   * every construction site, and an optional field is answered by `undefined` meaning
   * "true" at some sites and "false" at others.
   */
  loaded: boolean
  dirty: boolean
}

export interface RegistryOptions {
  /** How many editors stay mounted. 3 is what M0 measured. */
  windowSize?: number
  /** Build the extensions for a section editor. Supplied by the caller so the
   *  schema lives with the app, not the registry. */
  buildExtensions: (sectionId: string) => unknown[]
  /** Called when a section is mounted, so the caller can attach it to the DOM. */
  onMount?: (editor: Editor, sectionId: string) => void
  /** Called when a section is unmounted, after its JSON has been captured. */
  onUnmount?: (sectionId: string, json: unknown) => void
  /** Called on any content change, for autosave. */
  onChange?: (sectionId: string, json: unknown, metrics: SectionRecord['metrics']) => void
  /**
   * Whether a section has a change that has not been persisted.
   *
   * Supplied by the caller because only the caller knows what has been sent. Optional, and
   * treated as "not dirty" when absent, so a caller that does not persist anything pays
   * nothing for the hook being here.
   */
  isDirty?: (sectionId: string) => boolean

  /**
   * Called with a section's snapshot immediately before its editor is destroyed.
   *
   * # Why this exists, and why it is not `onUnmount`
   *
   * `onUnmount` fires *after* the snapshot is taken, which is fine for reporting and
   * useless for guaranteeing persistence: by then the only copy of the unsaved bytes is
   * the object being handed over, and the caller has to have captured it already.
   *
   * This hook receives the snapshot, so an eviction path can persist it without reaching
   * into the editor. It is synchronous on purpose. Making it a promise would force
   * `unmount` to be async, which would ripple through `slideWindow`, `unmountAll` and the
   * scroller's own `unmount` — all of which are called from places that cannot await,
   * like a ProseMirror key handler.
   *
   * The contract is therefore: *the caller has the bytes by the time this returns*. The
   * commit it starts may still be in flight, but nothing further is read from the editor
   * afterwards, so the bytes cannot be lost.
   *
   * `dirty` is whether the section has an unsent change, so a scroll through a document
   * nobody edited can skip the flush entirely rather than folding on every section.
   */
  onBeforeUnmount?: (
    sectionId: string,
    snapshot: { json: unknown; metrics: SectionRecord['metrics'] },
    dirty: boolean,
  ) => void
  /**
   * The asset hashes a section's content references.
   *
   * # Not used for releasing, and that is the point
   *
   * There was a version where this drove a release on unmount. It was wrong: the image
   * NodeView already acquires a reference when it renders, and the editor is destroyed by this
   * very call, so two acquisitions per section met one release and the count climbed with every
   * scroll. The bytes are held by a *rendered* `<img>`, so the rendered node is the right owner
   * and `AssetImage`'s `destroy` is where the release belongs.
   *
   * It is kept because "which assets does this section reference" is a real question — the
   * verification asks it, and it is what a section-level preflight would need.
   */
  assetsOf?: (snapshotJson: unknown) => string[]
  /**
   * Called after a section's content has been written to the store.
   *
   * The registry has no way to know this on its own — persistence belongs to the caller —
   * so it asks, and the flag it clears is the one the prune guard reads. Without this,
   * `SectionRecord.dirty` is write-once: a section created by a split starts dirty and is
   * never clean again, so it can never be pruned and never stops being a candidate for a
   * refusal.
   *
   * Optional, and a caller that omits it simply keeps the flag set.
   */
  onPersisted?: (sectionId: string) => void
  /**
   * Called after `mergeBackward` has moved a block into the previous section.
   *
   * # Why the registry reports this rather than the caller detecting it
   *
   * Because the caller cannot detect it. `mergeBackward` is invoked from a
   * ProseMirror key handler inside `boundary.ts`, which knows only that a Backspace at
   * position 0 crossed a boundary; by the time the transaction has been dispatched,
   * nothing marks it as a merge rather than two ordinary edits. Observing the
   * transaction and inferring a merge from it would be a guess.
   *
   * The callback fires *after* the editors have been changed, because that is what the
   * caller has to persist — Rust's copy of the document has to end up agreeing with
   * the live editors, not with the state before them.
   */
  onMerge?: (sourceId: string, targetId: string) => void
  /**
   * Called after an empty section has been removed, before the registry is updated.
   *
   * # Why a prune is reported separately from a merge
   *
   * Because the store has to do two different things. A merge re-bodies the *target*
   * section, so the target's content must be re-sent. A prune removes a section
   * outright: the source's row has to be deleted and nothing needs replacing. Reporting
   * a prune as a merge would send a merge for a section whose content is empty and would
   * leave the removed section's row in SQLite forever -- a leak, and one that a reopen
   * would resurrect as an empty section between two populated ones.
   *
   * It fires *before* `remove`, while the section still exists, so the caller can look it
   * up. That is the opposite of `onMerge`, which fires after, because the merge's
   * interesting content is the target's *new* body.
   */
  onPrune?: (removedId: string, previousId: string) => void
}

/** All-zero metrics, for a section that had no snapshot. Never used to estimate. */
const EMPTY_METRICS: SectionRecord['metrics'] = { words: 0, marks: 0, chars: 0, blocks: 0 }

export class SectionRegistry {
  private sections: SectionRecord[] = []
  private mounted = new Map<string, Editor>()
  private hosts = new Map<string, HTMLElement>()
  private focusedId: string | null = null
  private readonly windowSize: number
  private readonly opts: RegistryOptions

  readonly undo: UndoCoordinator
  private readonly boundaryHost: BoundaryHost

  constructor(opts: RegistryOptions) {
    this.opts = opts
    this.windowSize = opts.windowSize ?? 3

    this.undo = new UndoCoordinator({
      resolveEditor: id => this.mount(id),
      onFocusSection: id => this.focus(id),
      onChange: () => this.flushPending(),
    })

    this.boundaryHost = {
      previousSection: id => this.previousOf(id),
      nextSection: id => this.nextOf(id),
      focusEdge: (id, edge, column) => this.focusEdge(id, edge, column),
      mergeBackward: (id, prev) => this.mergeBackward(id, prev),
      pruneSection: (id, prev) => this.pruneSection(id, prev),
    }
  }

  // -- document shape ----------------------------------------------------

  get length(): number {
    return this.sections.length
  }

  get focused(): string | null {
    return this.focusedId
  }

  ids(): string[] {
    return this.sections.map(s => s.id)
  }

  record(id: string): SectionRecord | undefined {
    return this.sections.find(s => s.id === id)
  }

  indexOf(id: string): number {
    return this.sections.findIndex(s => s.id === id)
  }

  previousOf(id: string): string | null {
    const i = this.indexOf(id)
    if (i <= 0) return null
    return this.sections[i - 1]!.id
  }

  nextOf(id: string): string | null {
    const i = this.indexOf(id)
    if (i < 0 || i >= this.sections.length - 1) return null
    return this.sections[i + 1]!.id
  }

  /** Load a document: replace all sections and reset history. */
  /**
   * Take ownership of `sections`, as the array itself.
   *
   * # Why the defensive copy was removed
   *
   * It read as harmless and it was the cause of the in-engine verification reporting a
   * section as both evicted and still-loadable. `sections.map(s => ({ ...s }))` gives
   * every section *two* objects: the caller's and the registry's. A mutation through one
   * is invisible to the other, and nothing said so.
   *
   * The mutation that found it: the LRU cache prunes an evicted section by writing
   * `registry.record(id)!.loaded = false`, while the scroller's mount path reads
   * `sections[index].loaded`. So after eviction the registry believed the section had no
   * content and the scroller believed it had all of it — and mounted an editor on the
   * placeholder. Typing into that editor would have overwritten the stored section, which
   * is the single failure this architecture is arranged to make impossible.
   *
   * Scrolling back did not re-hydrate either, and for the same reason: the mount path took
   * the "already loaded" branch, so no fetch was ever issued and the check saw zero
   * requests where it expected a round trip.
   *
   * A copy is only safe if nobody mutates through either reference, and the whole design
   * mutates records in place — that is what `record.json = …`, `record.metrics = …` and
   * `record.loaded = …` all are. Copying was not defensive, it was a second owner.
   */
  load(sections: SectionRecord[]): void {
    this.unmountAll()
    this.sections = sections
    this.focusedId = null
    // History refers to steps against documents that no longer exist.
    this.undo.clear()
  }

  /** Insert a section at an index. Used by the splitter when a section grows. */
  insert(index: number, record: SectionRecord): void {
    this.sections.splice(index, 0, record)
  }

  /** Remove a section and drop its history. */
  remove(id: string): void {
    const i = this.indexOf(id)
    if (i < 0) return
    this.unmount(id)
    this.sections.splice(i, 1)
    this.undo.forgetSection(id)
    if (this.focusedId === id) {
      this.focusedId = this.sections[Math.min(i, this.sections.length - 1)]?.id ?? null
    }
  }

  // -- mounting ----------------------------------------------------------

  /**
   * The live editor for a section, or null. Does **not** mount.
   *
   * Deliberately distinct from [`SectionRegistry.mount`]: "is this mounted?" and
   * "mount it" are different questions, and a caller that means the first will
   * otherwise bring a whole editor into existence as a side effect of asking. The
   * scroller needs the distinction on every window reconciliation, where most
   * sections are not mounted and must stay that way.
   */
  /**
   * The schema the section editors are built from.
   *
   * # Why it is read off a mounted editor rather than built here
   *
   * Because the registry does not own the extensions — the caller supplies them through
   * `buildExtensions` — so there is no spec here to build a `Schema` from. Reading it off a
   * live editor is asking the thing that has the answer.
   *
   * # Why that makes it null before the first mount
   *
   * It does, and that is the honest answer rather than a lazily-constructed schema that
   * might differ from the one the editors actually use. The only callers want to compare
   * this schema's atom types against the seam rule's list, which is a question about the
   * editor as installed, and a schema nobody has mounted is not that.
   */
  schema(): Schema | null {
    for (const editor of this.mounted.values()) return editor.state.schema
    return null
  }

  editorIfMounted(id: string): Editor | null {
    const ed = this.mounted.get(id)
    return ed && !ed.isDestroyed ? ed : null
  }

  /**
   * Get (mounting if needed) the editor for a section.
   *
   * This is the `resolveEditor` the undo coordinator calls, which is what lets
   * history reach a section that has scrolled out of the window.
   */
  mount(id: string): Editor | null {
    const existing = this.mounted.get(id)
    if (existing && !existing.isDestroyed) return existing

    const rec = this.record(id)
    if (!rec) return null

    // Ensure a host element exists for this section, so the view has
    // somewhere to attach even when the section is mounted headlessly (tests,
    // and undo reaching an unmounted section).
    this.hosts.get(id) ?? this.createHost(id)
    const editor = new Editor({
      // Per-instance undo is deliberately absent: all history flows through
      // UndoCoordinator so it is global. See src/core/undo.ts.
      extensions: [
        ...(this.opts.buildExtensions(id) as any[]),
        boundaryTraversal(id, this.boundaryHost),
      ],
      content: rec.json as any,
      // Only the focused section accepts input. This is strategy C's freeze
      // mechanism, and it is what M0 verified rejects edits to other sections.
      editable: id === this.focusedId,
    })

    // Capture every transaction for global history and autosave.
    //
    // The dispatched `transaction` is passed to the coordinator explicitly.
    // Reading `editor.state.tr` inside the handler would yield a fresh empty
    // transaction, because the state has already advanced by the time the event
    // fires, so nothing would ever be captured.
    editor.on('transaction', ({ transaction, editor: e }) => {
      if (transaction.docChanged) {
        this.undo.capture(id, e, transaction)
        const j = this.snapshot(id)
        if (j) {
          this.opts.onChange?.(id, j.json, j.metrics)
        }
      }
    })

    this.mounted.set(id, editor)
    this.opts.onMount?.(editor, id)
    return editor
  }

  private createHost(id: string): HTMLElement {
    let host = this.hosts.get(id)
    if (!host) {
      host = document.createElement('div')
      host.className = 'section-slice'
      host.dataset.sectionId = id
      this.hosts.set(id, host)
    }
    return host
  }

  /** Serialise a section, preferring the live editor when mounted. */
  private snapshot(id: string): { json: unknown; metrics: SectionRecord['metrics'] } | null {
    const editor = this.mounted.get(id)
    const rec = this.record(id)
    if (!rec) return null
    if (editor && !editor.isDestroyed) {
      return { json: editor.getJSON(), metrics: rec.metrics }
    }
    return { json: rec.json, metrics: rec.metrics }
  }

  /**
   * Unmount a section, capturing its content first.
   *
   * The editor is destroyed rather than just detached: a ProseMirror editor
   * holds view descriptions, a plugin state array, and DOM references, and
   * detaching alone would leak all of it as the user scrolls through a long
   * document.
   */
  unmount(id: string): void {
    const editor = this.mounted.get(id)
    if (!editor) return
    const snap = this.snapshot(id)
    if (snap) {
      const rec = this.record(id)
      if (rec) {
        rec.json = snap.json
        rec.metrics = snap.metrics
      }
    }
    // Before `destroy()`, and with the snapshot in hand. `dirty` comes from the caller
    // because "has an unsent change" is a question about the committer, not about the
    // registry: a section edited and already committed is not dirty, and folding the WAL
    // for it on every eviction would be pointless work on the scroll path.
    this.opts.onBeforeUnmount?.(id, snap ?? { json: null, metrics: EMPTY_METRICS }, this.opts.isDirty?.(id) ?? false)
    // The host has just been handed this section's bytes to persist, so the record is no
    // longer "not yet saved". Clearing it here rather than on a timer is what keeps
    // `dirty` a statement about the present instead of a permanent mark on a section's
    // birth — which is what made a split-created section impossible to prune.
    this.opts.onPersisted?.(id)
    this.opts.onUnmount?.(id, snap?.json)
    editor.destroy()
    this.mounted.delete(id)
  }

  unmountAll(): void {
    for (const id of [...this.mounted.keys()]) this.unmount(id)
  }

  // -- focus and windowing ----------------------------------------------

  /**
   * Focus a section, sliding the window so it is mounted and the neighbours are
   * available for boundary crossing.
   */
  focus(id: string): boolean {
    if (this.indexOf(id) < 0) return false
    this.focusedId = id

    const editor = this.mount(id)
    if (!editor) return false

    // Editable follows focus, per strategy C.
    for (const [sid, ed] of this.mounted) {
      if (!ed.isDestroyed) ed.setEditable(sid === id)
    }

    // Slide the window: keep the focused section and as many neighbours as fit.
    this.slideWindow(id)
    editor.commands.focus()
    return true
  }

  private slideWindow(centerId: string): void {
    const centre = this.indexOf(centerId)
    const half = Math.floor(this.windowSize / 2)
    const first = Math.max(0, Math.min(centre - half, this.sections.length - this.windowSize))
    const last = Math.min(this.sections.length - 1, first + this.windowSize - 1)

    const wanted = new Set<string>()
    for (let i = Math.max(0, first); i <= last; i++) {
      wanted.add(this.sections[i]!.id)
    }

    // Unmount anything outside the window, but never the focused section.
    for (const id of [...this.mounted.keys()]) {
      if (!wanted.has(id) && id !== centerId) this.unmount(id)
    }
    // Mount anything inside it that is not yet live.
    for (const id of wanted) this.mount(id)
  }

  /**
   * Place the caret at an edge of a section, mounting it if needed.
   *
   * The window is slid *before* the position is resolved, because the coordinate
   * path in `placeCaretAtEdge` reads the target's layout. A section that was
   * just mounted and then scrolled out of view has no reliable box, and the
   * crossing would silently fall back to a character offset.
   */
  focusEdge(id: string, edge: 'start' | 'end', target: CrossingTarget): boolean {
    if (this.indexOf(id) < 0) return false
    this.focusedId = id
    const editor = this.mount(id)
    if (!editor) return false
    for (const [sid, ed] of this.mounted) {
      if (!ed.isDestroyed) ed.setEditable(sid === id)
    }
    this.slideWindow(id)
    const ok = placeCaretAtEdge(editor, edge, target)
    if (ok) editor.commands.focus()
    return ok
  }

  // -- boundary merge ----------------------------------------------------

  /**
   * Merge the first block of `id` into the tail of `previousId`.
   *
   * # Insertion point
   *
   * The block goes at `content.size`, the position *after* the last child, not
   * `content.size - 1`. An earlier version used `size - 1` believing that was
   * the end of the last block; it is not, it is that block's closing token.
   * Inserting there appended the merged text *inside* the previous section's
   * final paragraph, so the previous section got smaller (137 -> 136) rather than
   * larger. The test asserted growth, which is what exposed it.
   *
   * Deliberately narrow. Full section merging, with word and mark threshold
   * rebalancing, belongs to the storage layer: this only handles the single
   * Backspace-at-position-0 case so the key does something sensible, and defers
   * the accounting to `onChange`.
   */
  mergeBackward(id: string, previousId: string): boolean {
    const target = this.mount(previousId)
    const source = this.mount(id)
    if (!target || !source) return false

    const firstBlock = source.state.doc.firstChild
    if (!firstBlock) return false

    // Only merge a textblock. A table, image, or equation at the seam is left
    // alone: splitting or moving those is a structural edit the user should
    // perform explicitly, and doing it implicitly risks data loss.
    if (!firstBlock.isTextblock) return false

    // Close the open group first, so one undo reverts the merge as a unit
    // rather than leaving half of it in the previous group.
    this.undo.commit()

    const insertAt = target.state.doc.content.size
    // How far into the source to delete to remove the block that was just moved.
    //
    // # This was a constant 2, and a block is 2 only when it is empty
    //
    // A node's size is `1 + content.size + 1`, so an *empty* paragraph is 2 and a
    // paragraph of 500 characters is 502. The constant therefore removed the block's opening
    // token and its first character and left the rest, which ProseMirror normalised into an
    // empty paragraph rather than removing anything.
    //
    // The visible result was a seam that duplicated content: the block appeared in the
    // previous section *and* as a stray empty paragraph at the top of the next one, so a
    // document that had been split and rejoined was permanently one block longer than it
    // started. Nothing reported an error — the merge returned `true`, which it should, since
    // the insert half had succeeded.
    //
    // Found by the seam test asserting that the document still holds the same number of
    // blocks it did before the split. It is worth stating that this is the kind of bug the
    // "blocks are conserved" assertion exists for: counting them before and after is a
    // stronger statement than checking that the right block arrived.
    const removeCount = source.state.doc.childCount > 1 ? firstBlock.nodeSize : 0

    // Two dispatches, not one transaction. They target two different documents,
    // so they cannot be composed; each must be recorded against the state it
    // actually applies to, which is what makes both halves undoable.
    //
    // `tr.insert` takes a Fragment built through the target's own schema; it
    // rejects both raw JSON and a Slice.
    const frag = jsonToFragment(target.state.doc, firstBlock.toJSON())
    if (!frag) return false

    // A merge is one user action, so both halves are recorded as a single undo
    // group. `beginGroup`/`endGroup` bracket the two dispatches so the
    // coordinator does not close the group between them, and the section set
    // stops each dispatch from opening a nested group of its own.
    this.undo.beginGroup()
    try {
      this.undo.dispatchAs(previousId, target.state.tr.insert(insertAt, frag))
    } catch {
      this.undo.endGroup()
      return false
    }
    if (removeCount > 0) {
      try {
        this.undo.dispatchAs(id, source.state.tr.delete(0, removeCount))
      } catch {
        // The insert already landed in the previous section. Report failure
        // rather than claiming a clean merge; do not attempt a cross-editor
        // rollback, which the architecture cannot express.
        this.undo.endGroup()
        return false
      }
    }
    this.undo.endGroup()

    // A merge has no column to preserve: the caret goes to the join point, which
    // is the end of the previous section by definition. An offset of 0 resolves
    // to exactly that, so the coordinate path is not attempted here.
    this.focusEdge(previousId, 'end', { kind: 'offset', chars: 0 })
    // After the editors have changed, and once, so the caller persists the state that
    // exists rather than the state that used to. See `onMerge`.
    this.opts.onMerge?.(id, previousId)
    return true
  }

  /**
   * Remove an empty section and hand the caret to the end of its predecessor.
   *
   * # Why this is not `mergeBackward`
   *
   * Merging moves a block from this section into the previous one. An empty section has
   * no block to move, so a merge against it either does nothing or deletes the section's
   * only paragraph and leaves it in place — one Backspace, and the section is still there
   * with one fewer empty paragraph. The user's next Backspace repeats the gesture and
   * gets nowhere. See `BoundaryHost.pruneSection`.
   *
   * # Ordering, and why it is this order
   *
   * Geometry first, then the registry, then focus. The reverse is what makes a half-applied
   * prune: if the registry is updated first and `reindex` throws, the geometry is still
   * sized for N sections while the document has N−1, and every offset below the removed
   * section is wrong by one row.
   *
   * # Why `onMerge` is reused
   *
   * Because it is the same *event* from the store's point of view: a section the editor
   * no longer shows has stopped being part of the document, and Rust's copy has to be told.
   * A distinct `onPrune` callback would send the same message twice with different names,
   * and the store would need both to do one thing.
   */
  pruneSection(id: string, previousId: string): boolean {
    // Refuse to prune the only section, or one that is not at the seam.
    //
    // A one-section document has no seam to be at, so this cannot be reached from the
    // keyboard — but a caller invoking it directly must not empty the document.
    if (this.sections.length <= 1) return false
    const index = this.indexOf(id)
    if (index <= 0) return false
    if (this.indexOf(previousId) !== index - 1) return false

    // The emptiness check, re-done here rather than trusted from the caller.
    //
    // `boundary.ts` already decides this from the live editor, but `pruneSection` is
    // public and the tests and any future caller invoke it directly. The structural
    // guards above were load-bearing and this one was not, which made every "must not
    // prune" case in the suite fail while the positive case passed — a prune that
    // removes content the user wrote, reported as working.
    //
    // # Why the section is mounted rather than falling back to the record
    //
    // The first fallback read `record.metrics.words`, which is *stale by design*: metrics are
    // written on commit and on a split, not on every keystroke, so a section the user has just
    // emptied still reports its old word count. An unmounted section therefore looked
    // non-empty and the prune was refused — the gesture silently doing nothing, which is the
    // exact failure the prune exists to remove. `test/oscillation.ts` found it by oscillating
    // across a split, where the new tail is not in the mount window.
    //
    // Mounting instead means there is exactly one definition of "empty" — `isSectionEmpty`,
    // the same predicate the Backspace handler uses — and no second copy to drift. The mount
    // is free: `remove` below unmounts it again, and a prune is a user gesture rather than a
    // per-keystroke path.
    const live = this.editorIfMounted(id) ?? this.mount(id)
    if (!live) return false
    if (!isSectionEmpty(live.state)) return false

    // No section may be pruned while it holds an unsaved change. The bytes would be gone
    // from the registry and still only in the editor, which is destroyed a line below.
    // `onBeforeUnmount` has already had its chance to persist them, and the prune is a
    // structural change rather than an eviction, so the guard is a refusal rather than a
    // flush: a caller that wants this gone can flush and retry.
    //
    // `record.dirty` is checked as well as the committer's own answer, but the two mean
    // different things and conflating them was a bug of its own.
    //
    // A section created by a split starts with `dirty: true` — its content has never been
    // saved — and nothing cleared it, because `dirty` was written in exactly one place and
    // read in one. So every split-created section was permanently unprunable, and
    // `test/oscillation.ts` could not complete a single cycle: split, then prune, and the
    // prune was refused forever on a flag that had lost its meaning.
    //
    // `markPersisted` is what restores the meaning, and `onBeforeUnmount` calls it because
    // that is the point where the host has just been handed the snapshot to write.
    if (this.record(id)?.dirty || this.opts.isDirty?.(id)) return false

    // Close the open undo group, so one undo reverts the prune as a unit rather than
    // leaving the caret's move half-recorded.
    this.undo.commit()

    // The removed section's row goes first, so the geometry re-keys against the ordering
    // the registry is about to have.
    // The section leaves the registry first, so the ordering it knows about is already the
    // new one.
    this.remove(id)

    // `onPrune` fires *after* the removal, which is what lets the host re-key the geometry
    // from `ids()` immediately. It used to fire before, so the host could only re-key
    // after a round trip to the store — during which the geometry was sized for N sections
    // while the document had N−1, and if the reply never came (no Tauri host in the
    // browser harness) it stayed that way. The symptom was the last section sitting at
    // zero height.
    //
    // The store's ordering is still authoritative and is reconciled against when it
    // arrives; this is about not being wrong in the meantime.
    this.opts.onPrune?.(id, previousId)

    // The predecessor gains the caret at its end.
    //
    // Not `focusEdge`, because the geometry is about to be rebuilt by `onPrune`'s caller
    // and the offsets `focusEdge` would resolve against are the pre-prune ones. Ordering
    // the two the other way round -- geometry first, then focus -- is what keeps the
    // caret in the right place; see `onPrune` in `main.ts`.
    //
    // A mounted previous editor is focused directly, which is the case that matters: the
    // user pressed Backspace in the section next to it, so it is on screen. If it is not
    // mounted the caret stays where the prune put it, and the next click resolves
    // normally -- better than focusing a section the user cannot see.
    const previous = this.mounted.get(previousId)
    if (previous && !previous.isDestroyed) {
      this.focusedId = previousId
      for (const [sid, ed] of this.mounted) ed.setEditable(sid === previousId)
      // An offset of 0 resolves to exactly the join point, which is the end of the
      // previous section by definition. There is no column to preserve across a section
      // that no longer exists, so the measured-coordinate path is not attempted.
      previous.commands.focus('end')
    }
    return true
  }

  // -- history plumbing --------------------------------------------------

  /**
   * Close the open undo group.
   *
   * Called before structural operations, where leaving a typing group open would
   * make a single undo straddle two unrelated edits.
   */
  commit(): void {
    this.undo.commit()
  }

  private flushPending(): void {
    // An all-zero metrics object for a flush with no content to describe. `blocks`
    // is 0 rather than 1 deliberately: this is a "nothing happened" signal, not a
    // section with zero blocks, and `estimateHeight` rejects it. Passing 1 here
    // would look like a real section and produce a plausible-looking height.
    this.opts.onChange?.(this.focusedId ?? '', null as any, { words: 0, marks: 0, chars: 0, blocks: 0 })
  }

  /** Total words across the document, from the manifest rather than the DOM. */
  totalWords(): number {
    return this.sections.reduce((a, s) => a + s.metrics.words, 0)
  }

  /** Sections whose metrics exceed a split threshold. */
  sectionsNeedingSplit(words: number, marks: number): Array<{ id: string; words: number; marks: number }> {
    return this.sections
      .filter(s => s.metrics.words > words || s.metrics.marks > marks)
      .map(s => ({ id: s.id, words: s.metrics.words, marks: s.metrics.marks }))
  }

  destroy(): void {
    this.unmountAll()
    this.undo.clear()
    this.sections = []
    this.focusedId = null
  }
}
