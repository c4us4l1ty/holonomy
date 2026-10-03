import { Editor, Extension } from '@tiptap/core'
import StarterKit from '@tiptap/starter-kit'
// In Tiptap 3 the table sub-extensions live inside @tiptap/extension-table
// behind subpath exports, and all of them are named exports rather than
// default exports.
import { Table, TableRow, TableCell, TableHeader } from '@tiptap/extension-table'
import { Plugin, PluginKey } from '@tiptap/pm/state'
import { Decoration, DecorationSet } from '@tiptap/pm/view'
import {
  Image,
  InlineEquation,
  TextStyle,
  Highlight,
  SectionParagraph,
  SectionHeading,
} from './schema.js'
import type { Section } from './store.js'

export interface SeamStrategy {
  readonly name: string
  readonly description: string
  readonly crossBoundaryMode: 'native' | 'swap'
  mount(el: HTMLElement): void
  destroy(): void
  focusSection(index: number): void
  onBoundaryCross(cb: (from: number, to: number) => void): void
  getDocSize(): number
  getSectionCount(): number
  /** The live editor, so the harness can drive real transactions. */
  getEditor(): Editor
}

/* ------------------------------------------------------------------ *
 * Extension sets
 * ------------------------------------------------------------------ */

const commonExtensions = [
  Image,
  InlineEquation,
  TextStyle,
  Highlight,
  Table.configure({ resizable: false }),
  TableRow,
  TableCell,
  TableHeader,
]

/**
 * Build the extension list.
 *
 * The section-aware paragraph/heading are always used, not just by the window
 * strategy. Two reasons:
 *
 *  1. The `doc` node's content expression is `paragraph block*`, so a
 *     `paragraph` node must exist in every schema. Disabling StarterKit's
 *     paragraph without supplying a replacement fails at schema construction.
 *  2. Having one schema for all strategies keeps the measurements
 *     comparable, since the strategies then differ only in how sections are
 *     mounted, not in what the document model can represent.
 */
function buildExtensions() {
  const starter = StarterKit.configure({
    paragraph: false,
    heading: false,
  } as any)
  return [
    ...(starter.config.addExtensions?.call(starter) ?? []),
    SectionParagraph,
    SectionHeading,
    ...commonExtensions,
  ]
}

/* ------------------------------------------------------------------ *
 * Strategy A — single editor, content swap
 * ------------------------------------------------------------------ */

export class SwapStrategy implements SeamStrategy {
  readonly name = 'A: content-swap'
  readonly description =
    'One editor instance. Focusing a different section calls setContent(). ' +
    'Cross-boundary typing needs an explicit swap plus cursor re-placement.'
  readonly crossBoundaryMode = 'swap' as const

  private editor: Editor
  private sections: Section[]
  private current = 0
  private boundaryCb: ((from: number, to: number) => void) | null = null
  private armed = true

  constructor(sections: Section[]) {
    this.sections = sections
    this.editor = new Editor({
      extensions: buildExtensions(),
      content: sections[0].json as any,
    })
  }

  getEditor() {
    return this.editor
  }

  mount(el: HTMLElement) {
    el.appendChild(this.editor.view.dom as HTMLElement)
  }

  destroy() {
    this.editor.destroy()
  }

  onBoundaryCross(cb: (from: number, to: number) => void) {
    this.boundaryCb = cb
  }

  focusSection(index: number) {
    this.current = index
    this.armed = true
    this.editor.commands.setContent(this.sections[index].json as any, false)
  }

  getDocSize() {
    return this.editor.state.doc.content.size
  }

  getSectionCount() {
    return 1
  }

  /**
   * The seam problem for strategy A: there is no seam in the document at all,
   * so the editor cannot tell you the user is about to leave the section. The
   * harness detects it externally by watching for a caret at the document
   * edge, which is exactly the fragility this strategy has to live with.
   */
  get hasNativeSeam(): boolean {
    return false
  }
}

/* ------------------------------------------------------------------ *
 * Strategy B — sliding window of sections in ONE editor document
 * ------------------------------------------------------------------ */

const windowKey = new PluginKey('section-window')

export class WindowStrategy implements SeamStrategy {
  readonly name = 'B: sliding-window'
  readonly description =
    'One editor whose document contains [prev, focused, next] as a single doc. ' +
    'Non-focused ranges are made non-editable, so the seam is a real document ' +
    'boundary and cross-boundary editing is native within the window.'
  readonly crossBoundaryMode = 'native' as const

  private editor: Editor
  private sections: Section[]
  private win = { first: 0, last: 1, focused: 0 }
  private boundaryCb: ((from: number, to: number) => void) | null = null
  private windowSize: number

  constructor(sections: Section[], windowSize = 3) {
    this.sections = sections
    this.windowSize = windowSize
    const last = Math.min(windowSize - 1, sections.length - 1)
    this.win = { first: 0, last, focused: 0 }
    this.editor = new Editor({
      extensions: [...buildExtensions(), this.windowExtension()],
      content: this.buildDoc(0, last, 0) as any,
    })
  }

  getEditor() {
    return this.editor
  }

  mount(el: HTMLElement) {
    el.appendChild(this.editor.view.dom as HTMLElement)
  }

  destroy() {
    this.editor.destroy()
  }

  onBoundaryCross(cb: (from: number, to: number) => void) {
    this.boundaryCb = cb
  }

  getDocSize() {
    return this.editor.state.doc.content.size
  }

  getSectionCount() {
    return this.win.last - this.win.first + 1
  }

  focusSection(index: number) {
    const half = Math.floor(this.windowSize / 2)
    const first = Math.max(0, Math.min(index - half, this.sections.length - this.windowSize))
    const last = Math.min(this.sections.length - 1, first + this.windowSize - 1)
    this.win = { first: Math.max(0, first), last, focused: index }
    this.editor.commands.setContent(this.buildDoc(this.win.first, this.win.last, index) as any, false)
    this.placeCaretIn(index)
  }

  /**
   * Flatten sections [first..last] into one document, tagging every block with
   * the section it came from. The tags are what `decorations` and
   * `filterTransaction` key off.
   */
  private buildDoc(first: number, last: number, focused: number) {
    const content: any[] = []
    for (let i = first; i <= last; i++) {
      const blocks = (this.sections[i].json as any).content ?? []
      for (const b of blocks) {
        content.push({
          ...b,
          attrs: { ...(b.attrs ?? {}), sectionIndex: i, sectionFocused: i === focused },
        })
      }
    }
    return { type: 'doc', content }
  }

  /** Put the caret at the start of the focused section's first text block. */
  private placeCaretIn(index: number) {
    const { doc } = this.editor.state
    let target: number | null = null
    doc.descendants((node, pos) => {
      if (target !== null) return false
      if (node.isTextblock && (node.attrs as any).sectionIndex === index) {
        target = pos + 1
        return false
      }
      return true
    })
    if (target !== null) this.editor.commands.setTextSelection(target as number)
  }

  /**
   * Is `pos` inside the focused section?
   *
   * Takes the document explicitly rather than reading `editor.state.doc`,
   * because `filterTransaction` runs against the *incoming* transaction's
   * document, which already has the step applied. Resolving against the old
   * state would test positions in a document that no longer exists.
   */
  private isEditablePos(doc: any, pos: number): boolean {
    if (!doc || !doc.content) return false
    const clamped = Math.max(0, Math.min(pos, doc.content.size))
    let $pos: any
    try {
      $pos = doc.resolve(clamped)
    } catch {
      return false
    }
    // Walk ancestors looking for a focused block. A position between two
    // blocks resolves at depth 0, where there is no section to attribute it
    // to; those are treated as editable so the user can still type at a
    // boundary, and the window slides as a result.
    for (let d = $pos.depth; d > 0; d--) {
      const node = $pos.node(d)
      const attrs = node.attrs as any
      if (attrs?.sectionFocused === true) return true
      if (attrs?.sectionIndex != null) return false
    }
    return true
  }

  /**
   * The frozen-section guard, as a real Tiptap `Extension`.
   *
   * The first attempt passed a bare ProseMirror `Plugin` in the `extensions`
   * array. Tiptap accepts only `Extension` instances there and silently
   * discarded the plugin, so the guard never ran and edits landed in frozen
   * sections (see diag3.ts: `pluginKeys` had no `section-window` entry,
   * `domFrozenNodes: 0`, and a delete inside a frozen block succeeded).
   * ProseMirror plugins must be returned from `addProseMirrorPlugins`.
   */
  private windowExtension() {
    const self = this
    return Extension.create({
      name: 'sectionWindow',

      addProseMirrorPlugins() {
        return [
          new Plugin({
            key: windowKey,

            props: {
              /**
               * Freeze non-focused ranges. `contenteditable=false` is only a
               * UI hint on its own, so it is paired with the plugin-level
               * `filterTransaction` below.
               */
              decorations(state) {
                const decos: Decoration[] = []
                state.doc.descendants((node, pos) => {
                  const idx = (node.attrs as any)?.sectionIndex
                  if (idx == null || idx === self.win.focused) return true
                  if (node.isTextblock) {
                    decos.push(
                      Decoration.node(pos, pos + node.nodeSize, {
                        contenteditable: 'false',
                        'data-frozen-section': String(idx),
                      }),
                    )
                  }
                  return true
                })
                return DecorationSet.create(state.doc, decos)
              },
            },

            /**
             * Reject edits into frozen ranges.
             *
             * This lives at the plugin-spec level, as a sibling of `props`,
             * because that is where ProseMirror looks for it
             * (`prosemirror-state`: `plugin.spec.filterTransaction`). Nesting
             * it inside `props` compiles and runs without error but is never
             * consulted, which is exactly the failure this spike caught: the
             * guard reported "frozen at depth 1" and the edit was applied
             * anyway.
             *
             * Returning false drops the transaction and leaves the DOM
             * untouched, keeping model and DOM in sync.
             */
            filterTransaction(tr, state) {
              if (!tr.docChanged) return true
              // `tr.docs` is [before, afterStep0, afterStep1, ...]. For a
              // single-step transaction that is length 2, so the document
              // containing the step's mapped positions is `tr.docs[1]`.
              // Falling back to `tr.doc` is only safe because a delete's
              // mapped range is resolved against the post-step document.
              let violated = false
              tr.steps.forEach((step, i) => {
                if (violated) return
                const doc = tr.docs[i + 1] ?? tr.doc
                step.getMap().forEach((_fromA, _toA, fromB, toB) => {
                  if (violated) return
                  // A pure deletion maps to an empty range in the resulting
                  // document, so `fromB === toB` is common and skipping it
                  // would let deletes through unguarded. Instead of skipping,
                  // resolve the *pre-step* position for deletions.
                  let probeFrom = fromB
                  let probeTo = toB
                  if (fromB === toB) {
                    const before = tr.docs[i] ?? tr.before
                    const map = step.getMap()
                    map.forEach((fromA, toA) => {
                      if (fromA <= fromB && fromB <= toA) {
                        probeFrom = fromA
                        probeTo = toA
                      }
                    })
                    if (!self.isEditablePos(before, probeFrom) || !self.isEditablePos(before, probeTo)) {
                      violated = true
                    }
                    return
                  }
                  if (!self.isEditablePos(doc, probeFrom) || !self.isEditablePos(doc, probeTo)) {
                    violated = true
                  }
                })
              })
              if (violated) {
                // Surface the rejected edit so the caller can decide whether to
                // slide the window (a boundary crossing) or ignore it.
                queueMicrotask(() => self.boundaryCb?.(self.win.focused, self.win.focused))
              }
              return !violated
            },
          }),
        ]
      },
    })
  }
}

/* ------------------------------------------------------------------ *
 * Strategy C — one editor instance per mounted section
 * ------------------------------------------------------------------ */

export class MultiInstanceStrategy implements SeamStrategy {
  readonly name = 'C: multi-instance'
  readonly description =
    'One editor instance per visible section, each in its own container. ' +
    'No cross-boundary editing; focus moves between instances.'
  readonly crossBoundaryMode = 'swap' as const

  private editors: Editor[] = []
  private hosts: HTMLElement[] = []
  private sections: Section[]
  private focused = 0
  private boundaryCb: ((from: number, to: number) => void) | null = null

  constructor(sections: Section[], private windowSize = 3) {
    this.sections = sections
    const n = Math.min(windowSize, sections.length)
    for (let i = 0; i < n; i++) {
      const host = document.createElement('div')
      host.dataset.section = String(i)
      this.hosts.push(host)
      this.editors.push(
        new Editor({
          extensions: buildExtensions(),
          content: sections[i].json as any,
          editable: i === 0,
        }),
      )
    }
  }

  getEditor() {
    return this.editors[this.focused] ?? this.editors[0]
  }

  mount(el: HTMLElement) {
    this.hosts.forEach((h, i) => {
      h.appendChild(this.editors[i].view.dom as HTMLElement)
      el.appendChild(h)
    })
  }

  destroy() {
    this.editors.forEach(e => e.destroy())
  }

  onBoundaryCross(cb: (from: number, to: number) => void) {
    this.boundaryCb = cb
  }

  getDocSize() {
    return this.editors.reduce((a, e) => a + e.state.doc.content.size, 0)
  }

  getSectionCount() {
    return this.editors.length
  }

  focusSection(index: number) {
    this.focused = index
    this.editors.forEach((e, i) => e.setEditable(this.hosts[i].dataset.section === String(index)))
  }
}

/** Count real DOM nodes under a root, as a proxy for layout/paint cost. */
export function domNodeCount(root: HTMLElement): number {
  return root.querySelectorAll('*').length
}
