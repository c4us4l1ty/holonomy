/**
 * The M3 harness page (`m3.html`): editor core, undo, boundary traversal.
 *
 * Not the application. This is a test surface with a synthetic document and no
 * bridge; the product is `main.ts`, which `index.html` loads and a packaged build
 * contains. The split is deliberate — the M3 assertions measure the editor and the
 * M4 assertions measure layout, and merging them would mean one page whose
 * behaviour depends on which suite is driving it.
 *
 * Kept deliberately thin: the registry owns section lifecycle, the undo
 * coordinator owns history, and this file only connects them to the DOM and
 * exposes a test surface. Everything worth testing lives in `src/core`.
 */

import { Editor } from '@tiptap/core'
import StarterKit from '@tiptap/starter-kit'
import { Table, TableRow, TableCell, TableHeader } from '@tiptap/extension-table'
import { SectionRegistry, type SectionRecord } from './core/registry.js'

/**
 * Extensions for a section editor.
 *
 * `UndoRedo` is deliberately **not** included. Per-instance history is what
 * makes undo trap inside a section; all history is routed through
 * `UndoCoordinator` instead. See `src/core/undo.ts`.
 */
function buildExtensions(): unknown[] {
  return [
    StarterKit.configure({
      // Per-instance undo off: global history only.
      undoRedo: false,
    }),
    Table,
    TableRow,
    TableCell,
    TableHeader,
  ]
}

const stack = document.getElementById('stack')!

export const registry = new SectionRegistry({
  windowSize: 3,
  buildExtensions,

  onMount(editor: Editor, sectionId: string) {
    const host = document.querySelector<HTMLElement>(`[data-section-id="${sectionId}"]`)
    if (!host) {
      const el = document.createElement('div')
      el.className = 'section-slice'
      el.dataset.sectionId = sectionId
      stack.appendChild(el)
    }
    const target = document.querySelector<HTMLElement>(`[data-section-id="${sectionId}"]`)!
    if (!target.contains(editor.view.dom as HTMLElement)) {
      target.appendChild(editor.view.dom as HTMLElement)
    }
    target.dataset.focused = String(sectionId === registry.focused)
    refresh()
  },

  onUnmount(sectionId: string, _json: unknown) {
    // Keep the serialised content on the record; the host element stays so the
    // section does not jump when it is re-mounted.
    const host = document.querySelector<HTMLElement>(`[data-section-id="${sectionId}"]`)
    if (host) host.innerHTML = ''
    refresh()
  },

  onChange(_sectionId: string, _json: unknown) {
    // In the real app this is where autosave would be scheduled. Here it just
    // keeps the word count current for the header.
    refresh()
  },
})

function refresh() {
  const focused = document.getElementById('focused')!
  const mounted = document.getElementById('mounted')!
  const undo = document.getElementById('undo')!
  const redo = document.getElementById('redo')!
  const words = document.getElementById('words')!
  focused.textContent = registry.focused ?? '—'
  mounted.textContent = String((registry as any).mounted?.size ?? 0)
  const d = registry.undo.depth
  undo.textContent = String(d.undo)
  redo.textContent = String(d.redo)
  words.textContent = String(registry.totalWords())
  for (const el of Array.from(document.querySelectorAll<HTMLElement>('.section-slice'))) {
    el.dataset.focused = String(el.dataset.sectionId === registry.focused)
  }
}

/** Build a synthetic document for tests. */
export function reset(sectionCount: number, paragraphsPer: number): void {
  const sections: SectionRecord[] = []
  for (let i = 0; i < sectionCount; i++) {
    const content: unknown[] = [
      { type: 'heading', attrs: { level: 2 }, content: [{ type: 'text', text: `Section ${i}` }] },
    ]
    for (let p = 0; p < paragraphsPer; p++) {
      content.push({
        type: 'paragraph',
        content: [{ type: 'text', text: `Body ${i}.${p} alpha beta gamma delta epsilon.` }],
      })
    }
    sections.push({
      id: `s${i}`,
      json: { type: 'doc', content },
      // 1 heading + `paragraphsPer` paragraphs = the top-level block count, which
      // is what the geometry needs. Counted rather than estimated: the M3 harness
      // does not render, so a wrong block count here would only ever show up as a
      // wrong scrollbar if this page ever grew one.
      metrics: { words: paragraphsPer * 7 + 2, marks: 0, chars: 400, blocks: paragraphsPer + 1 },
      // Complete by construction: the fixture built the content it is handing over.
      loaded: true,
      dirty: false,
    })
  }
  loadSections(sections)
}

/**
 * Load sections with caller-supplied content.
 *
 * Exists because some behaviour is only observable with controlled line lengths.
 * The default fixture's sections all have identically shaped lines, so a
 * character-offset caret placement and a coordinate-based one land on the same
 * character and the test cannot tell them apart. Varying the shapes is what makes
 * the two distinguishable.
 */
export function loadRaw(
  docs: Array<{ id: string; content: unknown[] }>,
  focusFirst = true,
): void {
  loadSections(
    docs.map(d => ({
      id: d.id,
      json: { type: 'doc', content: d.content },
      // `docs` carries content, so the block count is countable. Anything the
      // caller passes is top-level ProseMirror content, which is exactly what the
      // geometry's per-block term is calibrated against.
      metrics: {
        words: 0,
        marks: 0,
        chars: 0,
        blocks: Math.max(1, d.content.length),
      },
      // Complete by construction, as above.
      loaded: true,
      dirty: false,
    })),
    focusFirst,
  )
}

function loadSections(sections: SectionRecord[], focusFirst = true): void {
  stack.innerHTML = ''
  registry.load(sections)
  for (const s of sections) {
    const el = document.createElement('div')
    el.className = 'section-slice'
    el.dataset.sectionId = s.id
    stack.appendChild(el)
  }
  if (focusFirst && sections.length) registry.focus(sections[0]!.id)
  refresh()
}

// Test surface. Deliberately explicit about what it exposes so tests cannot
// accidentally depend on internals.
;(window as any).HOLO_APP = {
  registry,
  reset,
  loadRaw,

  /** Editor for a section, mounting it if needed. */
  editor(sectionId: string) {
    return (registry as any).mounted.get(sectionId) ?? null
  },

  /** Focus a section. */
  focus(sectionId: string) {
    return registry.focus(sectionId)
  },

  /** Type `text` at the caret of the focused section. */
  type(text: string) {
    const ed = (registry as any).mounted.get(registry.focused)
    if (!ed) throw new Error('no focused editor')
    ed.commands.insertContent(text)
    return ed.state.doc.textContent
  },

  /** Delete backwards `n` times at the caret. */
  backspace(n = 1) {
    const ed = (registry as any).mounted.get(registry.focused)
    if (!ed) throw new Error('no focused editor')
    for (let i = 0; i < n; i++) ed.commands.deleteSelection()
    return ed.state.doc.textContent
  },

  /**
   * Build a synthetic document of `n` sections.
   *
   * Shared with the product surface so both use the same document shape.
   * Duplicated rather than imported because this page and `main.ts` must not depend
   * on each other: one is a harness, one is the app, and a shared fixture module
   * would be a third place for the document shape to live.
   */
  loadSynthetic(n: number, paragraphs = 15, charsPerPara = 620) {
    const sections: SectionRecord[] = []
    for (let i = 0; i < n; i++) {
      const content: unknown[] = []
      for (let p = 0; p < paragraphs; p++) {
        const text =
          `Body ${i}.${p} ` +
          'lorem ipsum dolor sit amet '.repeat(Math.ceil(charsPerPara / 27)).slice(0, charsPerPara)
        content.push({ type: 'paragraph', content: [{ type: 'text', text }] })
      }
      sections.push({
        id: `s${i}`,
        json: { type: 'doc', content },
        // Exactly `paragraphs`: one block per paragraph pushed above. Derived
        // from the same loop that built the content, so the two cannot disagree.
        metrics: {
          words: paragraphs * 90,
          marks: 0,
          chars: paragraphs * charsPerPara,
          blocks: paragraphs,
        },
        loaded: true,
        dirty: false,
      })
    }
    loadSections(sections)
  },

  /** Caret position within the focused section. */
  caret() {
    const ed = (registry as any).mounted.get(registry.focused)
    if (!ed) throw new Error('no focused editor')
    return {
      from: ed.state.selection.from,
      to: ed.state.selection.to,
      size: ed.state.doc.content.size,
      parentOffset: ed.state.selection.$from.parentOffset,
      depth: ed.state.selection.$from.depth,
      index: ed.state.selection.$from.index(0),
      text: ed.state.doc.textContent,
    }
  },

  /** Put the caret at an absolute position in the focused section. */
  setCaret(pos: number) {
    const ed = (registry as any).mounted.get(registry.focused)
    if (!ed) throw new Error('no focused editor')
    ed.commands.setTextSelection(pos)
    return ed.state.selection.from
  },

  /** Undo once. Returns which section it touched. */
  undo() {
    return registry.undo.undo()
  },

  redo() {
    return registry.undo.redo()
  },

  commit() {
    registry.undo.commit()
  },

  historyState() {
    return registry.undo.inspect()
  },

  mountedIds() {
    return [...((registry as any).mounted.keys() ?? [])]
  },

  sectionIds() {
    return registry.ids()
  },

  previousOf(id: string) {
    return registry.previousOf(id)
  },

  nextOf(id: string) {
    return registry.nextOf(id)
  },
}

console.log('[holonomy] ready')
