/**
 * Global undo/redo across section editor instances.
 *
 * # The problem
 *
 * M0 chose strategy C: one Tiptap editor per mounted section. That gives cheap
 * window slides, but it means each editor has its own `UndoRedo` extension and
 * therefore its own history. Undo pressed while section 4 is focused would only
 * walk back through section 4's edits, stranding the user inside one section
 * forever. That is the "history traps inside an inactive section" failure.
 *
 * # The approach
 *
 * Tiptap/ProseMirror already know how to invert a transaction: every `Step` has
 * an `invert(doc)` method. So rather than storing document snapshots (which
 * would be ~24KB per keystroke) or reimplementing inverse operations, this
 * coordinator captures the *steps* of each transaction and derives the inverse
 * steps when undo is pressed.
 *
 * An entry is therefore: which section it belongs to, the steps to reapply, and
 * the inverse steps to roll back. Undo pops the newest entry, focuses that
 * entry's section (mounting it if it has scrolled out of the window), and
 * dispatches the inverse steps into that editor.
 *
 * # Grouping
 *
 * Typing a word should be one undo step, not eight. Entries are grouped while
 * they are adjacent in the stack, touch the same section, and arrive within
 * `mergeWindowMs`. A selection jump, a click into another section, or a pause
 * breaks the group. This mirrors what a user expects from a single document even
 * though the document is physically split across editors.
 *
 * # Interaction with ProseMirror's own history
 *
 * Per-instance `UndoRedo` is disabled. If both were active, a single Ctrl+Z would
 * be ambiguous: the editor might consume it first. All history flows through
 * here, which is the only way to make it global.
 */

import type { Editor } from '@tiptap/core'
import type { Transaction } from '@tiptap/pm/state'
import { Mapping, type Step } from '@tiptap/pm/transform'

/**
 * One section's contribution to an undo entry.
 *
 * # Why an entry can span sections
 *
 * A boundary merge is a single user action that necessarily changes two
 * documents: it appends a block to the previous section and removes it from the
 * current one. It cannot be expressed as two independent undo steps, because
 * undoing only one half leaves the text duplicated or lost.
 *
 * An earlier version modelled an entry as one section plus flat step arrays and
 * suppressed capture for coordinator-initiated transactions, so a merge was
 * never recorded at all and could not be undone. So entries are now a list of
 * per-section parts, applied in reverse on undo.
 */
export interface UndoPart {
  sectionId: string
  /** Steps to re-apply on redo. */
  redo: Step[]
  /** Inverted steps to roll back on undo. */
  undo: Step[]
  /**
   * How this section changed *after* this entry was created.
   *
   * A `Step` carries absolute positions and is only valid against the exact
   * document state it was computed from. Undo applies entries newest-first, so
   * by the time an older entry is reached, every newer undo has already run and
   * the document is back to where the entry was created — provided each undo was
   * itself rebased correctly.
   */
  sinceThen: Mapping
}

/** A single undoable action, possibly spanning several sections. */
export interface UndoEntry {
  /**
   * Sections this entry touches, in the order they were edited. Undo focuses the
   * last one, since that is where the user was working.
   */
  parts: UndoPart[]
  /** Epoch ms of the first transaction in this group. */
  at: number
}

/** One captured transaction, before grouping. */
interface Capture {
  sectionId: string
  step: Step
  /** The document as it was *before* this step, needed to invert it. */
  invert: Step
  at: number
  /** Selection after the transaction, restored on redo so the caret returns. */
  selectionAnchor: number | null
  selectionHead: number | null
}

export interface CoordinatorOptions {
  /**
   * Consecutive edits to the same section within this window collapse into one
   * undo entry. 400ms is roughly a typing burst; longer merges feel wrong
   * because the user has moved on.
   */
  mergeWindowMs?: number
  /** Hard cap on retained entries, to bound memory. */
  maxEntries?: number
  /** Called when undo/redo needs a section focused. */
  onFocusSection?: (sectionId: string) => void
  /** Resolve an editor for a section, mounting it if necessary. */
  resolveEditor: (sectionId: string) => Editor | null
  /** Called after any change so the caller can mark the document dirty. */
  onChange?: () => void
}

const DEFAULT_MERGE_MS = 400
const DEFAULT_MAX_ENTRIES = 500

export class UndoCoordinator {
  private undoStack: UndoEntry[] = []
  private redoStack: UndoEntry[] = []
  private pending: Capture[] = []
  /** Section of the pending group, so a section change breaks the group. */
  private pendingSection: string | null = null
  private lastCaptureAt = 0

  /**
   * Sections currently having a coordinator-driven transaction applied.
   *
   * # Why this exists
   *
   * Undo and redo dispatch real transactions through the editor, which fires the
   * same `transaction` event that `capture()` listens to. Without a guard, undo
   * records *itself* as a new edit: the trace showed `undoDepth: 1` but
   * `pending: 1, pendingSection: "s1"` immediately after the first undo, and a
   * second undo popped the s1 entry again and re-applied it, so `BBB` came back
   * and a third undo did the same thing. Undo appeared to work once and then
   * looped.
   *
   * The fix is to record the change against the stack (so older entries re-base
   * correctly) but not open a new capture group for it. Sections are tracked
   * rather than using a single boolean because a merge dispatches into two
   * editors and a nested call must not clear the outer one.
   */
  private applyingTo = new Set<string>()

  /**
   * Depth of an explicitly bracketed group (a boundary merge).
   *
   * While > 0, a section change does not close the group, because a merge is one
   * user action that spans two documents by definition.
   */
  private groupDepth = 0

  private readonly mergeWindowMs: number
  private readonly maxEntries: number
  private readonly opts: CoordinatorOptions

  constructor(opts: CoordinatorOptions) {
    this.opts = opts
    this.mergeWindowMs = opts.mergeWindowMs ?? DEFAULT_MERGE_MS
    this.maxEntries = opts.maxEntries ?? DEFAULT_MAX_ENTRIES
  }

  /**
   * Record a transaction from a section editor.
   *
   * # The transaction must be passed in, not read from `editor.state`
   *
   * An earlier version took only the editor and read `editor.state.tr`. That is
   * the *current* state's transaction, which after a dispatch is a **fresh empty
   * transaction**, not the one that was just applied. So `docChanged` was always
   * false, nothing was ever captured, and `undoDepth` stayed at 0 — undo silently
   * did nothing, with no error anywhere.
   *
   * The registry therefore passes the dispatched `transaction` explicitly, and
   * `before` is the document state prior to it, which is what `invert` needs.
   *
   * Selection-only transactions are ignored: they produce no steps, and undoing
   * them would move the caret unexpectedly.
   */
  capture(sectionId: string, _editor: Editor, tr: Transaction): void {
    if (!tr.docChanged) return
    const steps = tr.steps
    if (!steps || steps.length === 0) return

    // Older entries must learn about this change before we push a new one, so
    // their steps stay re-basable. This applies to undo/redo too.
    this.recordChange(sectionId, tr)

    // Undo and redo dispatch through the same path, so this transaction is the
    // coordinator's own work, not a user edit. Record the mapping above, then
    // stop: opening a capture group here is what made undo re-apply itself.
    if (this.applyingTo.has(sectionId)) return

    const now = Date.now()

    // A new section, or a pause, closes the open group — unless an explicit
    // group is open (a boundary merge), which deliberately spans sections.
    if (this.groupDepth === 0 && this.pendingSection !== null && this.pendingSection !== sectionId) {
      this.flush()
    }
    if (this.pendingSection === sectionId && now - this.lastCaptureAt > this.mergeWindowMs) {
      this.flush()
    }

    // Invert each step against the document state that preceded it. Steps
    // within one transaction apply in order, so the inverse of step N is
    // computed against the document as it was before that step.
    let doc = tr.before
    for (const step of steps) {
      let inverse: Step
      try {
        inverse = step.invert(doc)
      } catch {
        // A step that cannot be inverted (a malformed remote patch, say) must
        // not corrupt the history. Skip the transaction rather than guess.
        continue
      }
      this.pending.push({
        sectionId,
        step,
        invert: inverse,
        at: now,
        selectionAnchor: tr.selection.anchor,
        selectionHead: tr.selection.head,
      })
      // Advance the document so the next step inverts against the right state.
      // `Step.apply` returns null when the step does not fit, which would mean
      // the remaining steps cannot be inverted either, so stop there.
      const applied = step.apply(doc)
      if (!applied) break
      const nextDoc = applied.doc
      if (!nextDoc) break
      doc = nextDoc
    }

    this.pendingSection = sectionId
    this.lastCaptureAt = now
  }

  /** Close the open group into a stack entry. */
  private flush(): void {
    if (this.pending.length === 0) {
      this.pendingSection = null
      return
    }

    // Group the pending captures by section, preserving first-seen order. A
    // typing group has exactly one; a merge has one part per section it touched.
    const bySection = new Map<string, Capture[]>()
    for (const c of this.pending) {
      if (!bySection.has(c.sectionId)) bySection.set(c.sectionId, [])
      bySection.get(c.sectionId)!.push(c)
    }

    const parts: UndoPart[] = []
    for (const [sectionId, captures] of bySection) {
      parts.push({
        sectionId,
        redo: captures.map(c => c.step),
        // Inverses apply in reverse to undo a forward sequence.
        undo: captures.map(c => c.invert).reverse(),
        // Nothing has happened to this section since the group was opened.
        sinceThen: new Mapping(),
      })
    }

    this.undoStack.push({ parts, at: this.pending[0]!.at })
    if (this.undoStack.length > this.maxEntries) {
      this.undoStack.shift()
    }
    // Any new edit invalidates the redo branch.
    this.redoStack.length = 0
    this.pending = []
    this.pendingSection = null
    this.opts.onChange?.()
  }

  /**
   * Record that `tr` changed a section, so every older entry touching that
   * section can re-base its steps.
   *
   * Called for every document change, including the ones undo itself performs.
   * That looks circular but is exactly what keeps the stack consistent: when an
   * entry is applied, everything below it must learn that positions moved,
   * otherwise its stored steps point into the wrong place.
   */
  private recordChange(sectionId: string, tr: Transaction): void {
    if (!tr.docChanged) return
    if (!tr.mapping || tr.mapping.maps.length === 0) return
    for (const stack of [this.undoStack, this.redoStack]) {
      for (const e of stack) {
        for (const part of e.parts) {
          if (part.sectionId === sectionId) part.sinceThen.appendMapping(tr.mapping)
        }
      }
    }
  }

  /**
   * Commit the open group.
   *
   * Called on a timer or before a structural operation, so that a pause in
   * typing produces one undo step rather than one per keystroke.
   */
  commit(): void {
    this.flush()
  }

  /**
   * Open a group that spans several transactions in different sections.
   *
   * A section change normally breaks a group, which is right for typing but wrong
   * for a merge: a merge is one user action that necessarily touches two
   * documents. This suppresses that for the bracketed span.
   */
  beginGroup(): void {
    this.flush()
    this.groupDepth++
  }

  /** Close a group opened by `beginGroup`, committing it as one entry. */
  endGroup(): void {
    if (this.groupDepth > 0) this.groupDepth--
    this.flush()
  }

  /**
   * Dispatch a coordinator-initiated transaction into a section's editor.
   *
   * Used by boundary merges, which are a single user action spanning two
   * documents. Bracketed by `beginGroup`/`endGroup`, both halves land in one
   * entry with one part per section, so a single undo reverts the whole merge.
   *
   * Capture is deliberately *not* suppressed here. An earlier version marked the
   * section as "applying" and skipped capture, which made the merge
   * unrecorded and therefore impossible to undo — the text merged into the
   * previous section and could never be taken back out.
   */
  dispatchAs(sectionId: string, tr: Transaction): void {
    const editor = this.opts.resolveEditor(sectionId)
    if (!editor) throw new Error(`no editor for section ${sectionId}`)
    editor.view.dispatch(tr)
  }

  get canUndo(): boolean {
    return this.undoStack.length > 0 || this.pending.length > 0
  }

  get canRedo(): boolean {
    return this.redoStack.length > 0
  }

  get depth(): { undo: number; redo: number } {
    return { undo: this.undoStack.length, redo: this.redoStack.length }
  }

  /**
   * Re-base a step array onto the current document.
   *
   * `part.sinceThen` accumulates every change made to the section after the entry
   * was created, including changes made by later undos. Mapping each step
   * through it yields steps valid against the document as it stands now.
   *
   * Returns null if any step fails to map, which means the entry can no longer
   * be applied safely and must be reported rather than guessed at.
   */
  private rebase(steps: Step[], part: UndoPart): Step[] | null {
    const out: Step[] = []
    // `Step.map` takes a **Mapping**, and returns a Step whose `getMap()` yields a
    // StepMap. To chain steps, the StepMap has to be rewrapped in a Mapping —
    // a StepMap is not one, and passing it directly is a type error. Seeding from
    // `part.sinceThen.maps[0]` instead would silently discard every accumulated
    // change after the first, which is the whole point of tracking them.
    let m: Mapping = part.sinceThen
    for (const step of steps) {
      let mapped: Step | null
      try {
        mapped = step.map(m)
      } catch {
        return null
      }
      // `Step.map` returns null when the step does not fit, meaning the entry can
      // no longer be applied to the current document.
      if (!mapped) return null
      out.push(mapped)
      m = new Mapping([mapped.getMap()])
    }
    return out
  }

  /**
   * Build the transaction for one part, without dispatching it.
   *
   * Separated from dispatch so that a multi-section entry can be fully planned
   * before anything is applied. If any part fails, none are dispatched and the
   * entry stays intact, rather than leaving a merge half-undone.
   */
  private plan(
    part: UndoPart,
    direction: 'undo' | 'redo',
  ): { sectionId: string; tr: Transaction; editor: Editor } | { error: string; sectionId: string } {
    const editor = this.opts.resolveEditor(part.sectionId)
    if (!editor) {
      return { error: `section ${part.sectionId} unavailable`, sectionId: part.sectionId }
    }
    const steps = direction === 'undo' ? part.undo : part.redo
    const rebased = this.rebase(steps, part)
    if (!rebased) {
      return { error: `steps in ${part.sectionId} unmappable`, sectionId: part.sectionId }
    }
    const tr = editor.state.tr
    for (const step of rebased) {
      try {
        tr.step(step)
      } catch (e: any) {
        return { error: e.message, sectionId: part.sectionId }
      }
    }
    return { sectionId: part.sectionId, tr, editor }
  }

  private commitAll(
    planned: Array<{ sectionId: string; tr: Transaction; editor: Editor }>,
  ): void {
    for (const p of planned) {
      this.applyingTo.add(p.sectionId)
      try {
        p.editor.view.dispatch(p.tr)
      } finally {
        this.applyingTo.delete(p.sectionId)
      }
    }
  }

  /**
   * Undo the most recent entry, wherever it lives.
   *
   * This is the core of the global-history requirement: the entry may belong to a
   * section that is not currently focused, and possibly not even mounted, so each
   * part focuses and mounts its section before dispatching.
   *
   * Parts are applied in reverse order, so a merge's two halves roll back in the
   * opposite order to how they were applied.
   */
  undo(): { sectionId: string; ok: boolean; reason?: string } | null {
    this.flush()
    const entry = this.undoStack.pop()
    if (!entry || entry.parts.length === 0) return null

    // The last-touched section is where the user was working.
    const primary = entry.parts[entry.parts.length - 1]!.sectionId

    const planned = []
    for (const part of [...entry.parts].reverse()) {
      const r = this.plan(part, 'undo')
      if ('error' in r) {
        this.undoStack.push(entry)
        return { sectionId: primary, ok: false, reason: r.error }
      }
      planned.push(r)
    }

    this.commitAll(planned)
    this.opts.onFocusSection?.(primary)
    this.redoStack.push(entry)
    this.opts.onChange?.()
    return { sectionId: primary, ok: true }
  }

  /** Redo the most recently undone entry, focusing its section first. */
  redo(): { sectionId: string; ok: boolean; reason?: string } | null {
    const entry = this.redoStack.pop()
    if (!entry || entry.parts.length === 0) return null

    const primary = entry.parts[entry.parts.length - 1]!.sectionId

    // Forward order on redo: the inverse of reverse-order undo.
    const planned = []
    for (const part of entry.parts) {
      const r = this.plan(part, 'redo')
      if ('error' in r) {
        this.redoStack.push(entry)
        return { sectionId: primary, ok: false, reason: r.error }
      }
      planned.push(r)
    }

    this.commitAll(planned)
    this.opts.onFocusSection?.(primary)
    this.undoStack.push(entry)
    this.opts.onChange?.()
    return { sectionId: primary, ok: true }
  }


  /**
   * Drop all history.
   *
   * Called when a document is closed or reloaded from disk. Retained entries
   * reference steps against documents that no longer exist, so they are not
   * merely useless but unsafe to apply.
   */
  clear(): void {
    this.undoStack = []
    this.redoStack = []
    this.pending = []
    this.pendingSection = null
  }

  /**
   * Drop history for one section, without touching the rest.
   *
   * Needed when a section is deleted or reloaded from storage: its steps refer
   * to a document state that is being discarded.
   */
  forgetSection(sectionId: string): void {
    // Drop the part, not the whole entry, unless the entry no longer touches any
    // section. A merge entry spans two sections and must stay undoable if only
    // one of them is being discarded.
    const stripParts = (stack: UndoEntry[]) =>
      stack
        .map(e => ({ ...e, parts: e.parts.filter(p => p.sectionId !== sectionId) }))
        .filter(e => e.parts.length > 0)
    this.undoStack = stripParts(this.undoStack)
    this.redoStack = stripParts(this.redoStack)
    this.pending = this.pending.filter(c => c.sectionId !== sectionId)
    if (this.pendingSection === sectionId) this.pendingSection = null
  }

  /** Diagnostics for the status bar and for tests. */
  inspect(): {
    undoDepth: number
    redoDepth: number
    pending: number
    pendingSection: string | null
    undoSections: string[]
  } {
    return {
      undoDepth: this.undoStack.length,
      redoDepth: this.redoStack.length,
      pending: this.pending.length,
      pendingSection: this.pendingSection,
      undoSections: this.undoStack.map(e => e.parts.map(p => p.sectionId).join('+')),
    }
  }
}
