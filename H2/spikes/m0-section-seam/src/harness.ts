/**
 * M0 spike harness.
 *
 * Exposes a `window.HOLO` API that the Playwright driver calls into, so the
 * measurements happen inside a real editor rather than being inferred from
 * the outside.
 */
import { generateCorpus, countWords, type Corpus } from './corpus.js'
import { SectionStore } from './store.js'
import { SwapStrategy, WindowStrategy, MultiInstanceStrategy, domNodeCount, type SeamStrategy } from './strategies.js'

interface Bench {
  corpus: Corpus
  store: SectionStore
  strategy: SeamStrategy
  host: HTMLElement
  boundaryLog: Array<{ from: number; to: number; t: number }>
}

declare global {
  interface Window {
    HOLO: {
      init(opts: { targetWords: number; wordsPerSection: number; strategy: string }): Promise<any>
      stats(): any
      measureKeystrokes(n: number): Promise<any>
      measureColdLoad(indices: number[]): any
      measureSwap(n: number): any
      testCrossBoundary(): any
      testCursorSurvival(section: number, offsetFromEnd: number): any
      testSelectionAcross(): any
      focusSection(i: number): void
      scrollToSection(i: number): void
      teardown(): void
      typingPaths(): any
    }
  }
}

let bench: Bench | null = null

function makeStrategy(name: string, sections: any[]): SeamStrategy {
  switch (name) {
    case 'A':
    case 'swap':
      return new SwapStrategy(sections)
    case 'B':
    case 'window':
      return new WindowStrategy(sections, 3)
    case 'C':
    case 'multi':
      return new MultiInstanceStrategy(sections, 3)
    default:
      throw new Error(`unknown strategy ${name}`)
  }
}

const host = () => document.getElementById('viewport-host')!

function updateChrome() {
  if (!bench) return
  const nameEl = document.getElementById('strategy-name')!
  const sizeEl = document.getElementById('doc-size')!
  const domEl = document.getElementById('dom-nodes')!
  nameEl.textContent = bench.strategy.name
  sizeEl.textContent = String(bench.strategy.getDocSize())
  domEl.textContent = String(domNodeCount(host()))
}

window.HOLO = {
  async init(opts) {
    if (bench) this.teardown()

    const t0 = performance.now()
    const corpus = generateCorpus({
      targetWords: opts.targetWords,
      wordsPerSection: opts.wordsPerSection,
      seed: 0x5eed,
    })
    const genMs = performance.now() - t0

    const store = new SectionStore(corpus.sections, 4)
    const loaded = corpus.sections.map((_, i) => store.load(i))
    // Blow away the LRU so strategies start from a cold, honest state.
    for (let i = 0; i < corpus.sections.length; i++) store.load(i)

    const strategy = makeStrategy(opts.strategy, loaded)
    strategy.mount(host())
    strategy.onBoundaryCross((from, to) => {
      bench!.boundaryLog.push({ from, to, t: performance.now() })
    })

    bench = { corpus, store, strategy, host: host(), boundaryLog: [] }
    updateChrome()

    return {
      genMs,
      sections: corpus.sections.length,
      claimedWords: corpus.totalWords,
      actualWords: corpus.sections.reduce((a, s) => a + countWords(s.json), 0),
      manifestBytes: store.manifestBytes(),
      totalBytes: store.totalBytes(),
      strategyName: strategy.name,
      crossBoundaryMode: strategy.crossBoundaryMode,
    }
  },

  stats() {
    if (!bench) throw new Error('not initialised')
    const h = host()
    return {
      docSize: bench.strategy.getDocSize(),
      domNodes: domNodeCount(h),
      mountedSections: bench.strategy.getSectionCount(),
      boundaryEvents: bench.boundaryLog.length,
      cachedSections: bench.store.cachedCount,
      // A cheap read of layout cost: how tall is the mounted content?
      contentHeight: h.getBoundingClientRect().height,
    }
  },

  /**
   * Type `n` characters and report the cost distribution.
   *
   * The first attempt used `document.execCommand('insertText')`, which turned
   * out to be a no-op against ProseMirror in this environment: the text
   * appeared in the DOM but the document never changed, so the harness
   * cheerfully reported 0.1ms keystrokes for an editor that was not editing
   * at all (see diag2.ts). We now drive real transactions through
   * `view.dispatch`, which is the code path a keystroke actually takes, and
   * we assert the document grew so a silent no-op cannot recur.
   */
  measureKeystrokes(n: number) {
    if (!bench) throw new Error('not initialised')
    const ed = bench.strategy.getEditor()
    const before = ed.state.doc.content.size
    const times: number[] = []

    for (let i = 0; i < n; i++) {
      const pos = ed.state.selection.from
      const t0 = performance.now()
      ed.view.dispatch(ed.state.tr.insertText('x', pos))
      // Force style/layout flush so we measure the real cost of the edit
      // rather than just the time to build a transaction.
      void (ed.view.dom as HTMLElement).getBoundingClientRect().height
      times.push(performance.now() - t0)
    }

    const after = ed.state.doc.content.size
    updateChrome()
    return {
      ...summarize(times),
      docGrewBy: after - before,
      // Guard, not a metric. The document grows by at least one position per
      // keystroke, but it can grow by more: typing at a point that splits a
      // text node adds the node boundary too. So we assert a lower bound
      // rather than exact equality.
      valid: after - before >= n,
    }
  },

  /** Time the cold path: manifest hit -> JSON.parse -> usable section. */
  measureColdLoad(indices: number[]) {
    if (!bench) throw new Error('not initialised')
    const out: number[] = []
    for (const i of indices) {
      out.push(bench.store.loadTimed(i).ms)
    }
    return summarize(out)
  },

  /**
   * Cost of moving the window to another section, measured end to end
   * including the DOM swap. This is the number that decides whether the
   * architecture is viable at all.
   */
  measureSwap(n: number) {
    if (!bench) throw new Error('not initialised')
    const times: number[] = []
    const count = bench.corpus.sections.length
    for (let i = 0; i < n; i++) {
      // Jump far enough that the LRU is guaranteed cold.
      const target = (i * 37) % count
      const t0 = performance.now()
      const { section } = bench.store.loadTimed(target)
      void section
      bench.strategy.focusSection(target)
      // Force layout so we measure the real cost, not just script time.
      void host().getBoundingClientRect().height
      times.push(performance.now() - t0)
    }
    updateChrome()
    return summarize(times)
  },

  /**
   * The critical UX test: place the caret at the end of the focused section
   * and type. Does the user experience a continuous document, or a stall?
   */
  testCrossBoundary() {
    if (!bench) throw new Error('not initialised')
    const ed = bench.strategy.getEditor()
    const before = ed.state.doc.content.size
    const eventsBefore = bench.boundaryLog.length

    // Put the caret at the very end of the mounted content, which is where a
    // user sits when they are about to type into the next section.
    const endPos = ed.state.doc.content.size - 2
    ed.commands.setTextSelection(endPos)

    const t0 = performance.now()
    ed.view.dispatch(ed.state.tr.insertText('Z', endPos))
    void (ed.view.dom as HTMLElement).getBoundingClientRect().height
    const typeMs = performance.now() - t0

    const after = ed.state.doc.content.size
    return {
      typeMs: +typeMs.toFixed(3),
      docGrewBy: after - before,
      boundaryFired: bench.boundaryLog.length > eventsBefore,
      events: bench.boundaryLog.slice(eventsBefore),
      domNodes: domNodeCount(host()),
    }
  },

  /**
   * Can we place the caret at a precise offset and have it survive? Returns
   * the resolved position so the driver can assert it landed where asked.
   */
  testCursorSurvival(section: number, offsetFromEnd: number) {
    if (!bench) throw new Error('not initialised')
    bench.strategy.focusSection(section)
    // Always measure against the focused editor. For the multi-instance
    // strategy the aggregate docSize spans several editors, so a position
    // derived from it would be meaningless for any one of them.
    const ed = bench.strategy.getEditor()
    const dom = ed.view.dom as HTMLElement
    dom.focus()
    const size = ed.state.doc.content.size
    const target = Math.max(1, size - offsetFromEnd)

    const t0 = performance.now()
    ed.commands.setTextSelection(target)
    const ms = performance.now() - t0

    // "Survival" means the selection resolves to a real text position that
    // ProseMirror can map back to the DOM, not merely that we set a number.
    const landed = ed.state.selection.from
    let domBacked = false
    try {
      const domPos = ed.view.domAtPos(landed)
      domBacked = !!domPos?.node
    } catch {
      domBacked = false
    }

    // The browser's own selection must agree, or the caret will not paint.
    const sel = window.getSelection()
    const browserAgrees = sel ? ed.view.dom.contains(sel.anchorNode ?? null) : false

    updateChrome()
    return {
      requested: target,
      landed,
      exact: landed === target,
      domBacked,
      browserAgrees,
      ms: +ms.toFixed(3),
      docSize: size,
      aggregateDocSize: bench.strategy.getDocSize(),
    }
  },

  /**
   * Try to select across a section boundary and report whether the engine
   * allowed a single contiguous selection to span two sections.
   */
  testSelectionAcross() {
    if (!bench) throw new Error('not initialised')
    const ed = bench.strategy.getEditor()
    const doc = ed.state.doc

    // Find section boundaries: text blocks carrying a sectionIndex.
    const bySection = new Map<number, number[]>()
    doc.descendants((node, pos) => {
      const idx = (node.attrs as any)?.sectionIndex
      if (idx != null && node.isTextblock) {
        if (!bySection.has(idx)) bySection.set(idx, [])
        bySection.get(idx)!.push(pos)
      }
      return true
    })

    const indices = [...bySection.keys()].sort((a, b) => a - b)
    if (indices.length < 2) {
      return { supported: false, note: 'no section boundaries in mounted window', indices }
    }

    // ProseMirror positions live in a single flat space, so a selection that
    // spans two sections is trivially expressible. The real question is
    // whether such a selection is *editable* without touching frozen content.
    const a = bySection.get(indices[0])![0]
    const b = bySection.get(indices[1])!.at(-1)!
    const crossed = a < b

    // Attempt a real cross-boundary selection and then a deletion.
    ed.commands.setTextSelection({ from: a + 1, to: b + 1 })
    const sel = ed.state.selection
    const before = doc.content.size
    const dispatchAccepted = ed.view.dispatch(ed.state.tr.delete(sel.from, sel.to))
    const after = ed.state.doc.content.size
    void dispatchAccepted

    return {
      supported: true,
      indices,
      boundaries: [a, b],
      spanIsContiguous: crossed,
      selectionFrom: sel.from,
      selectionTo: sel.to,
      // If the delete was rejected, the doc is unchanged and the seam held.
      deleteApplied: after < before,
      bytesDeleted: before - after,
    }
  },

  focusSection(i: number) {
    if (!bench) throw new Error('not initialised')
    bench.strategy.focusSection(i)
    updateChrome()
  },

  scrollToSection(i: number) {
    if (!bench) throw new Error('not initialised')
    // Stand-in for the Fenwick-tree scrollbar: jump the spacers.
    const top = document.getElementById('spacer-top')!
    const bottom = document.getElementById('spacer-bottom')!
    const m = bench.store.manifest
    const before = m.slice(0, i).reduce((a, e) => a + e.estimatedHeight, 0)
    const after = m.slice(i).reduce((a, e) => a + e.estimatedHeight, 0)
    top.style.height = `${before}px`
    bottom.style.height = `${after}px`
    return { topPx: before, bottomPx: after, totalPx: before + after }
  },

  /**
   * Establish which input path actually mutates the document. The first run
   * reported 0.1ms keystrokes because nothing was reaching ProseMirror at all,
   * so we need to know the cheapest path that represents a real edit.
   */
  typingPaths() {
    if (!bench) throw new Error('not initialised')
    const dom = host().querySelector('.ProseMirror') as HTMLElement
    const r: any = {}

    const ed = (bench.strategy as any).editor ?? (bench.strategy as any).editors?.[0]
    if (!ed) return { error: 'no editor handle exposed' }

    r.docSize0 = ed.state.doc.content.size

    // Path A: focus + collapse selection to end + execCommand
    dom.focus()
    const sel = window.getSelection()
    const range = document.createRange()
    range.selectNodeContents(dom)
    range.collapse(false)
    sel?.removeAllRanges()
    sel?.addRange(range)
    r.focused = document.activeElement === dom
    r.execReturn = document.execCommand('insertText', false, 'ZZZ')
    r.docSize_afterExec = ed.state.doc.content.size

    // Path B: beforeinput event
    dom.dispatchEvent(
      new InputEvent('beforeinput', { inputType: 'insertText', data: 'Q', bubbles: true, cancelable: true }),
    )
    r.docSize_afterBeforeInput = ed.state.doc.content.size

    // Path C: editor command
    ed.commands.insertContentAt(ed.state.doc.content.size - 2, 'API')
    r.docSize_afterCommand = ed.state.doc.content.size

    // Path D: raw transaction with a text step. This is the unit of edit cost
    // that actually depends on document size.
    const t0 = performance.now()
    ed.view.dispatch(ed.state.tr.insertText('K', ed.state.selection.from))
    r.dispatchMs = +(performance.now() - t0).toFixed(3)
    r.docSize_afterDispatch = ed.state.doc.content.size

    r.finalText = ed.state.doc.textContent.slice(-40)
    return r
  },

  /**
   * The load-bearing test for strategy B.
   *
   * B claims frozen (non-focused) sections cannot be edited. It enforces that
   * with `contenteditable=false` decorations plus a `filterTransaction` guard.
   * The decoration alone is only a UI hint, so this checks the guard
   * specifically: place the caret in a frozen block and try to type.
   */
  testFrozenEditBlocked() {
    if (!bench) throw new Error('not initialised')
    const ed = bench.strategy.getEditor()
    const doc = ed.state.doc

    // Find a text block that is present but NOT focused.
    let frozenPos: number | null = null
    let frozenSection: number | null = null
    doc.descendants((node, pos) => {
      if (frozenPos !== null) return false
      const idx = (node.attrs as any)?.sectionIndex
      if (idx != null && node.isTextblock && !node.attrs.sectionFocused && node.content.size > 0) {
        frozenPos = pos + 1
        frozenSection = idx
        return false
      }
      return true
    })

    if (frozenPos === null) {
      return { applicable: false, note: 'no frozen blocks mounted (strategy has no frozen state)' }
    }

    // Attempt the edit three ways, because a real user (or a paste) can reach
    // frozen content by more than one route. Each attempt is measured
    // separately: a guard that blocks inserts but not deletes is not a guard.
    const attempts: any[] = []
    const before = ed.state.doc.content.size

    const attempt = (name: string, fn: () => void) => {
      const b = ed.state.doc.content.size
      const bt = ed.state.doc.textContent
      let threw: string | null = null
      try {
        fn()
      } catch (e: any) {
        threw = e.message
      }
      const a = ed.state.doc.content.size
      attempts.push({
        name,
        threw,
        delta: a - b,
        textChanged: ed.state.doc.textContent !== bt,
        // A blocked attempt changes nothing at all.
        blocked: a === b && ed.state.doc.textContent === bt,
      })
    }

    // 1. Direct insert transaction at a frozen position.
    attempt('insert-at-frozen', () => {
      ed.view.dispatch(ed.state.tr.insertText('X', frozenPos))
    })

    // 2. Delete spanning a frozen range. This is the one that got through
    //    before, so it is the interesting case.
    attempt('delete-in-frozen', () => {
      const to = Math.min(frozenPos + 20, ed.state.doc.content.size - 1)
      ed.view.dispatch(ed.state.tr.delete(frozenPos, to))
    })

    // 3. API-level content injection, which is what paste and drag-drop use.
    attempt('insertContentAt-frozen', () => {
      ed.commands.insertContentAt(frozenPos, 'INJECTED')
    })

    const after = ed.state.doc.content.size
    return {
      applicable: true,
      frozenSection,
      frozenPos,
      sizeBefore: before,
      sizeAfter: after,
      attempts,
      blocked: attempts.every(a => a.blocked),
      domFrozen: !!ed.view.dom.querySelector('[data-frozen-section]'),
    }
  },

  /**
   * Why is sustained editing a no-op for some strategies?
   *
   * `measureSustained` edits at the document midpoint, which for a multi-section
   * window lands in a *neighbour* section rather than the focused one. Strategy B
   * correctly rejects that edit (the range is frozen), so it measures nothing.
   * That is the guard working, but it makes the sustained test meaningless for B.
   *
   * This places the caret in the focused section explicitly and reports whether
   * the edit landed, so the test can distinguish "fast" from "rejected".
   */
  measureSustainedFocused(n: number) {
    if (!bench) throw new Error('not initialised')
    const ed = bench.strategy.getEditor()
    const buckets = 10
    const per = Math.ceil(n / buckets)
    const out: any[] = []
    const before = ed.state.doc.content.size

    // Find a text position inside the focused section.
    const focusedPos = (): number | null => {
      let target: number | null = null
      ed.state.doc.descendants((node: any, pos: number) => {
        if (target !== null) return false
        const focused = node.attrs?.sectionFocused
        const isSectioned = node.attrs?.sectionIndex != null
        if (node.isTextblock && (focused === true || !isSectioned)) {
          target = pos + 1
          return false
        }
        return true
      })
      return target
    }

    const start = focusedPos()
    if (start === null) return { error: 'no focused text position found' }

    for (let b = 0; b < buckets; b++) {
      const times: number[] = []
      for (let i = 0; i < per; i++) {
        // Stay within the focused section: re-resolve each time, because each
        // insert shifts positions after it.
        const at = focusedPos() ?? start
        const t0 = performance.now()
        ed.view.dispatch(ed.state.tr.insertText('x', at))
        void (ed.view.dom as HTMLElement).getBoundingClientRect().height
        times.push(performance.now() - t0)
      }
      times.sort((a, c) => a - c)
      out.push({
        bucket: b,
        p50: round(times[Math.floor(times.length * 0.5)]),
        p95: round(times[Math.floor(times.length * 0.95)]),
        max: round(times[times.length - 1]),
      })
    }

    const after = ed.state.doc.content.size
    const first = out[0].p50
    const last = out[out.length - 1].p50
    updateChrome()
    return {
      buckets: out,
      totalEdits: n,
      docGrewBy: after - before,
      // Every edit should have landed. If not, the strategy is rejecting edits
      // to the focused section, which is a bug, not a fast path.
      accepted: after - before,
      valid: after - before >= n,
      drift: first > 0 ? round(last / first) : 0,
    }
  },

  /**
   * Sustained editing in one section, which is what a real session looks like.
   *
   * `measureKeystrokes` types 40 characters and reports a distribution. That
   * measures the cost of a short burst, not of a session: a user edits one place
   * for twenty minutes, and the question is whether the cost drifts upward as the
   * document accumulates edits, undo history, and decorations.
   *
   * This types continuously in one section and reports the cost in buckets, so a
   * trend is visible rather than hidden inside a single average.
   */
  measureSustained(n: number) {
    if (!bench) throw new Error('not initialised')
    const ed = bench.strategy.getEditor()
    const buckets = 10
    const per = Math.ceil(n / buckets)
    const out: Array<{ bucket: number; p50: number; p95: number; max: number }> = []
    const before = ed.state.doc.content.size

    for (let b = 0; b < buckets; b++) {
      const times: number[] = []
      for (let i = 0; i < per; i++) {
        // Edit in the middle of the document, where a user actually works,
        // rather than appending at the end.
        const mid = Math.max(1, Math.floor(ed.state.doc.content.size / 2))
        const t0 = performance.now()
        ed.view.dispatch(ed.state.tr.insertText('x', mid))
        void (ed.view.dom as HTMLElement).getBoundingClientRect().height
        times.push(performance.now() - t0)
      }
      times.sort((a, c) => a - c)
      out.push({
        bucket: b,
        p50: round(times[Math.floor(times.length * 0.5)]),
        p95: round(times[Math.floor(times.length * 0.95)]),
        max: round(times[times.length - 1]),
      })
    }

    const first = out[0].p50
    const last = out[out.length - 1].p50
    updateChrome()
    return {
      buckets: out,
      totalEdits: n,
      docGrewBy: ed.state.doc.content.size - before,
      valid: ed.state.doc.content.size - before >= n,
      // Drift is the number that matters: a flat cost means editing does not
      // degrade over a session.
      drift: first > 0 ? round(last / first) : 0,
    }
  },

  /**
   * Focused diagnosis of the frozen-edit guard.
   *
   * The sanity run showed edits landing in frozen sections, which should be
   * impossible. Two candidate causes: the decorations are not being applied
   * (domFrozen was false), or `filterTransaction` is not seeing the edit.
   * This separates them.
   */
  diagnoseFrozen() {
    if (!bench) throw new Error('not initialised')
    const ed = bench.strategy.getEditor()
    const doc = ed.state.doc
    const r: any = {}

    // How many blocks carry section attrs, and how many are focused?
    let withIndex = 0
    let focused = 0
    let frozen = 0
    doc.descendants((node: any) => {
      const idx = node.attrs?.sectionIndex
      if (idx != null) {
        withIndex++
        if (node.attrs.sectionFocused) focused++
        else frozen++
      }
      return true
    })
    r.blocksWithSectionIndex = withIndex
    r.blocksFocused = focused
    r.blocksFrozen = frozen
    r.windowState = { ...(bench.strategy as any).win }

    // Are the decorations present in the DOM?
    r.domFrozenNodes = ed.view.dom.querySelectorAll('[data-frozen-section]').length
    r.domEditableAttrs = [...ed.view.dom.querySelectorAll('[contenteditable]')].map(
      (n: any) => n.getAttribute('contenteditable'),
    ).slice(0, 5)

    // Is our plugin even registered?
    const keys = ed.state.plugins.map((p: any) => p.spec.key?.key ?? '(anonymous)')
    r.pluginKeys = keys

    // Walk to a frozen position and check what resolve() reports about ancestry.
    let fpos: number | null = null
    doc.descendants((node: any, pos: number) => {
      if (fpos !== null) return false
      const idx = node.attrs?.sectionIndex
      if (idx != null && !node.attrs.sectionFocused && node.isTextblock && node.content.size > 0) {
        fpos = pos + 1
        return false
      }
      return true
    })
    r.frozenPos = fpos
    if (fpos != null) {
      const $p = doc.resolve(fpos)
      r.ancestry = []
      for (let d = $p.depth; d >= 0; d--) {
        const n = $p.node(d)
        r.ancestry.push({ depth: d, type: n.type.name, attrs: n.attrs })
      }
    }
    return r
  },

  /** Step-by-step trace of a frozen-block edit attempt. */
  traceFrozenEdit() {
    if (!bench) throw new Error('not initialised')
    const ed = bench.strategy.getEditor()
    const doc = ed.state.doc
    const trace: any[] = []

    let fpos: number | null = null
    doc.descendants((node: any, pos: number) => {
      if (fpos !== null) return false
      const idx = node.attrs?.sectionIndex
      if (idx != null && !node.attrs.sectionFocused && node.isTextblock && node.content.size > 0) {
        fpos = pos + 1
        return false
      }
      return true
    })
    if (fpos === null) return { error: 'no frozen block' }

    // Inspect the position and its ancestry in detail.
    const $p = doc.resolve(fpos)
    const ancestry: any[] = []
    for (let d = $p.depth; d >= 0; d--) {
      const n = $p.node(d)
      ancestry.push({ depth: d, type: n.type.name, sectionIndex: n.attrs?.sectionIndex, focused: n.attrs?.sectionFocused })
    }
    trace.push({ step: 'resolve frozen pos', pos: fpos, ancestry, parentOffset: $p.parentOffset, depth: $p.depth })

    // Replicate the guard's own logic to see what it would decide.
    const isEditable = (d: any, p: number) => {
      const c = Math.max(0, Math.min(p, d.content.size))
      let $q: any
      try { $q = d.resolve(c) } catch { return { ok: false, reason: 'resolve threw' } }
      for (let dep = $q.depth; dep > 0; dep--) {
        const n = $q.node(dep)
        const a = n.attrs as any
        if (a?.sectionFocused === true) return { ok: true, reason: `focused at depth ${dep}` }
        if (a?.sectionIndex != null) return { ok: false, reason: `frozen at depth ${dep}` }
      }
      return { ok: true, reason: 'no section ancestor (treated editable)' }
    }
    trace.push({ step: 'guard decision at frozen pos', ...isEditable(doc, fpos) })
    // Where does the guard think the focused section starts?
    const startPositions: number[] = []
    doc.descendants((node: any, pos: number) => {
      if (node.attrs?.sectionFocused) startPositions.push(pos)
      return startPositions.length < 3
    })
    trace.push({ step: 'first focused block positions', startPositions })

    // Now actually try, capturing whether the transaction is accepted.
    const before = doc.content.size
    let accepted: boolean | null = null
    try {
      const tr = ed.state.tr.insertText('X', fpos)
      trace.push({
        step: 'built tr',
        steps: tr.steps.length,
        stepMap: tr.steps.map((s: any) => {
          const m = s.getMap()
          const ranges: any[] = []
          m.forEach((fa: number, ta: number, fb: number, tb: number) => ranges.push({ fa, ta, fb, tb }))
          return ranges
        }),
        guardSays: ranges => ranges,
      })
      const res = ed.view.dispatch(tr)
      accepted = res !== false
    } catch (e: any) {
      trace.push({ step: 'dispatch threw', error: e.message })
    }
    const after = ed.state.doc.content.size
    trace.push({ step: 'after dispatch', accepted, before, after, grew: after > before })
    trace.push({ step: 'text contains X', hasX: ed.state.doc.textContent.includes('§X§') })

    return { fpos, trace }
  },

  /**
   * Is `filterTransaction` reachable at all?
   *
   * The guard's decision logic is provably correct (it reports "frozen at
   * depth 1" for a position inside a frozen block), yet a transaction at that
   * exact position is still applied. So the prop itself is never consulted.
   * This counts invocations of each relevant prop to find where the chain
   * breaks.
   */
  probeFilterChain() {
    if (!bench) throw new Error('not initialised')
    const ed = bench.strategy.getEditor()
    const view = ed.view as any
    const w: any = window as any
    w.__probe = { filterCalls: 0, applyCalls: 0, dispatchCalls: 0, filterResults: [] as boolean[] }

    // Tap the state's filterTransaction by wrapping dispatchTransaction,
    // which is the single funnel every transaction passes through in Tiptap.
    const origDispatch = view._props?.dispatchTransaction
    w.__probe.origDispatchPresent = !!origDispatch
    w.__probe.viewProps = Object.keys(view._props ?? {})

    // Inspect how the state config exposes the filter.
    const cfg = ed.state.config
    w.__probe.hasFilterOnConfig = typeof cfg.filterTransaction
    w.__probe.configFilterIsFn = typeof cfg.filterTransaction === 'function'
    if (typeof cfg.filterTransaction === 'function') {
      // Call it directly with a transaction we know targets a frozen block.
      const doc = ed.state.doc
      let fpos: number | null = null
      doc.descendants((node: any, pos: number) => {
        if (fpos !== null) return false
        const idx = node.attrs?.sectionIndex
        if (idx != null && !node.attrs.sectionFocused && node.isTextblock && node.content.size > 0) {
          fpos = pos + 1
          return false
        }
        return true
      })
      const tr = ed.state.tr.insertText('X', fpos as number)
      const direct = cfg.filterTransaction.call(cfg, tr)
      w.__probe.directFilterResult = direct
      w.__probe.fpos = fpos
    }

    // Also check: does view.state differ from ed.state after a rejected tr?
    const tr = ed.state.tr.insertText('Y', 2)
    const before = ed.state.doc.content.size
    view.dispatch(tr)
    w.__probe.afterDispatchGrew = ed.state.doc.content.size - before

    return w.__probe
  },

  /** Why does a spanning delete slip past the guard? */
  traceDelete() {
    if (!bench) throw new Error('not initialised')
    const ed = bench.strategy.getEditor()
    const doc = ed.state.doc

    let fpos: number | null = null
    doc.descendants((node: any, pos: number) => {
      if (fpos !== null) return false
      const idx = node.attrs?.sectionIndex
      if (idx != null && !node.attrs.sectionFocused && node.isTextblock && node.content.size > 0) {
        fpos = pos + 1
        return false
      }
      return true
    })
    if (fpos === null) return { error: 'none' }

    const to = Math.min((fpos as number) + 20, doc.content.size - 1)
    const tr = ed.state.tr.delete(fpos as number, to)

    const isEditable = (d: any, p: number) => {
      const c = Math.max(0, Math.min(p, d.content.size))
      let $q: any
      try { $q = d.resolve(c) } catch { return { ok: false, why: 'resolve threw' } }
      const chain: any[] = []
      let verdict = 'editable (no section ancestor)'
      for (let dep = $q.depth; dep > 0; dep--) {
        const n = $q.node(dep)
        const a = n.attrs as any
        chain.push({ depth: dep, type: n.type.name, sectionIndex: a?.sectionIndex, focused: a?.sectionFocused })
        if (a?.sectionFocused === true) { verdict = 'EDITABLE'; break }
        if (a?.sectionIndex != null) { verdict = 'FROZEN'; break }
      }
      return { ok: verdict === 'EDITABLE', verdict, depth: $q.depth, parentOffset: $q.parentOffset, chain }
    }

    const after = tr.docs[tr.docs.length - 1]
    return {
      fpos,
      to,
      trDocsLength: tr.docs.length,
      from: isEditable(after, fpos),
      // The `to` end of a delete range resolves to the position *after* the
      // deleted content, which may sit in a different section entirely.
      to2: isEditable(after, to),
      // Show the neighbourhood so we can see the boundary.
      neighbourhood: (() => {
        const out: any[] = []
        doc.descendants((node: any, pos: number) => {
          if (pos < fpos! - 40 || pos > fpos! + 60) return true
          out.push({
            pos,
            type: node.type.name,
            sectionIndex: node.attrs?.sectionIndex,
            focused: node.attrs?.sectionFocused,
            size: node.nodeSize,
            text: node.textContent?.slice(0, 30),
          })
          return out.length < 8
        })
        return out
      })(),
    }
  },

  teardown() {
    if (!bench) return
    bench.strategy.destroy()
    host().innerHTML = ''
    bench = null
  },
}

function summarize(xs: number[]) {
  if (!xs.length) return { n: 0 }
  const s = [...xs].sort((a, b) => a - b)
  const q = (p: number) => s[Math.min(s.length - 1, Math.floor(p * s.length))]
  return {
    n: xs.length,
    min: round(s[0]),
    p50: round(q(0.5)),
    p95: round(q(0.95)),
    p99: round(q(0.99)),
    max: round(s[s.length - 1]),
    mean: round(xs.reduce((a, b) => a + b, 0) / xs.length),
  }
}

const round = (n: number) => Math.round(n * 1000) / 1000

console.log('[harness] ready')
