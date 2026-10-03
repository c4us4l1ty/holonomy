/**
 * Tests for the two M3 directives.
 *
 * 1. Global undo/redo across section editor instances — history must not trap
 *    inside the focused section.
 * 2. Boundary traversal — ArrowUp at position 0, ArrowDown at the end position,
 *    and Backspace at position 0 must cross section seams.
 *
 * These run in a real browser against real ProseMirror instances. A DOM shim
 * would test the shim: `handleKeyDown`, selection resolution, and the
 * interaction between an editor's view and its state are the behaviour under
 * test, and none of it is faithfully reproduced outside a browser.
 *
 * Run: npm test
 */

import { chromium, type Browser, type Page } from 'playwright'

// The M3 harness has its own page: `index.html` is the product surface (the
// scroll-driven scroller), and these tests exercise the older focus-driven one.
// Sharing a page would mean one set of globals for two different surfaces.
const URL = process.env.HOLO_APP_URL ?? 'http://localhost:5184/m3.html'

let passed = 0
let failed = 0
const failures: string[] = []

async function test(name: string, _page: Page, fn: () => Promise<unknown>): Promise<void> {
  try {
    const detail = await fn()
    passed++
    console.log(`PASS  ${name}${detail !== undefined ? `  ${JSON.stringify(detail)}` : ''}`)
  } catch (e: any) {
    failed++
    failures.push(name)
    console.log(`FAIL  ${name}\n        ${e.message}`)
  }
}

function ok(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg)
}

/**
 * Wait until `registry.focused` is `sectionId`, or fail saying where focus actually is.
 *
 * # Why this replaced `waitForTimeout`
 *
 * Every test that presses an arrow key at a seam used to sleep for a guessed interval and
 * then assert. The guess is a claim about how long a crossing takes, and it is wrong by
 * however busy the machine is — which is not a property of the code under test.
 *
 * The cost was measured, not imagined. `test/run.ts` passed 39/39 on this machine and, on a
 * GitHub runner, failed:
 *
 * ```text
 *   FAIL  directive 2: the seam is invisible — typing continues after crossing
 *         typing after crossing did not land in s2: Section 2Body 2.0 alpha beta ...
 * ```
 *
 * The typing landed in `s1` because `HOLO_APP.type` inserts into `registry.focused`, and at
 * that instant the crossing had not finished. The previous push failed the same family from
 * the other side — `ArrowDown at the end crosses into the next section` — so this is a race
 * that moves around rather than one bug with one symptom.
 *
 * Waiting on the postcondition is also a stronger assertion than the sleep was. `waitForFocus`
 * fails if the crossing *never* happens, which a sleep followed by a read cannot distinguish
 * from "it happened, just later".
 *
 * # Where this is deliberately not used
 *
 * The no-op tests — `ArrowUp at the document start`, `ArrowDown at the document end`,
 * `ArrowUp mid-section` — keep their sleeps. Their postcondition is that focus does *not*
 * change, so there is nothing to wait for, and shrinking their settle would make them pass
 * vacuously: they would read `focused` before the key handler had finished deciding.
 */
async function waitForFocus(page: Page, sectionId: string, timeout = 5_000): Promise<void> {
  try {
    await page.waitForFunction(
      (id: string) => (window as any).HOLO_APP.registry.focused === id,
      sectionId,
      { timeout },
    )
  } catch {
    const actual = await page.evaluate(() => (window as any).HOLO_APP.registry.focused)
    throw new Error(`expected focus on ${sectionId} within ${timeout}ms, but it is on ${actual}`)
  }
}

async function main() {
  const browser: Browser = await chromium.launch()
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } })

  page.on('pageerror', e => console.error(`  [page error] ${e.message}`))
  page.on('console', m => {
    if (m.type() === 'error') console.error(`  [console] ${m.text()}`)
  })

  await page.goto(URL, { waitUntil: 'load' })
  await page.waitForFunction(() => !!(window as any).HOLO_APP, null, { timeout: 30_000 })

  const reset = async (n = 5, p = 3) => {
    await page.evaluate(([a, b]) => (window as any).HOLO_APP.reset(a, b), [n, p] as const)
  }

  // ======================================================================
  // Setup sanity. Everything below depends on these.
  // ======================================================================

  await test('setup: document loads with sections', page, async () => {
    await reset(5)
    const ids = await page.evaluate(() => (window as any).HOLO_APP.sectionIds())
    ok(ids.length === 5, `expected 5 sections, got ${ids.length}`)
    return { sections: ids }
  })

  await test('setup: only the window is mounted', page, async () => {
    await reset(5)
    const mounted = await page.evaluate(() => (window as any).HOLO_APP.mountedIds())
    ok(
      mounted.length <= 3,
      `window should cap at 3 mounted editors, got ${mounted.length}: ${mounted}`,
    )
    return { mounted }
  })

  await test('setup: typing mutates the document', page, async () => {
    await reset(3)
    const before = await page.evaluate(() => (window as any).HOLO_APP.caret().size)
    const after = await page.evaluate(() => (window as any).HOLO_APP.type('ZZZ'))
    const size = await page.evaluate(() => (window as any).HOLO_APP.caret().size)
    ok(size > before, `doc did not grow: ${before} -> ${size}`)
    ok(after.includes('ZZZ'), 'typed text not present in document')
    return { before, size }
  })

  // ======================================================================
  // DIRECTIVE 1: global undo/redo
  // ======================================================================

  await test('directive 1: undo reverts an edit in the focused section', page, async () => {
    await reset(3)
    const original = await page.evaluate(() => (window as any).HOLO_APP.caret().text)
    await page.evaluate(() => (window as any).HOLO_APP.type('HELLO'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())
    ok(
      (await page.evaluate(() => (window as any).HOLO_APP.caret().text)).includes('HELLO'),
      'edit did not land',
    )

    const r = await page.evaluate(() => (window as any).HOLO_APP.undo())
    ok(r?.ok === true, `undo failed: ${JSON.stringify(r)}`)
    const now = await page.evaluate(() => (window as any).HOLO_APP.caret().text)
    ok(now === original, `undo did not restore text:\n  want ${original}\n  got  ${now}`)
    return { sectionId: r.sectionId }
  })

  await test('directive 1: undo crosses section boundaries (no history trap)', page, async () => {
    await reset(3)
    // Type in section 0, then move to section 1 and type there.
    await page.evaluate(() => (window as any).HOLO_APP.focus('s0'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.evaluate(() => (window as any).HOLO_APP.type('AAA'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())

    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.evaluate(() => (window as any).HOLO_APP.type('BBB'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())

    // Now undo twice while focused in section 1. The second undo must reach
    // back into section 0 — that is the whole point.
    const first = await page.evaluate(() => (window as any).HOLO_APP.undo())
    ok(first?.sectionId === 's1', `first undo should hit s1, got ${first?.sectionId}`)

    const second = await page.evaluate(() => (window as any).HOLO_APP.undo())
    ok(
      second?.sectionId === 's0',
      `second undo must cross into s0, got ${second?.sectionId} — history is trapped`,
    )
    ok(second?.ok === true, `cross-section undo failed: ${JSON.stringify(second)}`)

    // And the edit must actually be gone from section 0.
    const s0text = await page.evaluate(() => {
      const ed = (window as any).HOLO_APP.editor('s0')
      return ed ? ed.state.doc.textContent : null
    })
    ok(s0text !== null, 'section 0 editor unavailable')
    ok(!s0text!.includes('AAA'), `section 0 still contains its edit: ${s0text}`)
    return { first: first.sectionId, second: second.sectionId }
  })

  await test('directive 1: undo reaches an unmounted section', page, async () => {
    await reset(6)
    // Edit section 4, which will fall outside the window once focus moves.
    await page.evaluate(() => (window as any).HOLO_APP.focus('s4'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.evaluate(() => (window as any).HOLO_APP.type('MMM'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())

    // Move far away so s4 unmounts.
    await page.evaluate(() => (window as any).HOLO_APP.focus('s0'))
    await page.waitForTimeout(100)
    const mounted = await page.evaluate(() => (window as any).HOLO_APP.mountedIds())
    ok(!mounted.includes('s4'), `s4 should have unmounted, still mounted: ${mounted}`)

    // Undo must re-mount it to revert the edit.
    const r = await page.evaluate(() => (window as any).HOLO_APP.undo())
    ok(r?.ok === true, `undo into unmounted section failed: ${JSON.stringify(r)}`)
    ok(r?.sectionId === 's4', `expected undo to target s4, got ${r?.sectionId}`)

    const s4text = await page.evaluate(() => {
      const ed = (window as any).HOLO_APP.editor('s4')
      return ed ? ed.state.doc.textContent : null
    })
    ok(!s4text!.includes('MMM'), `s4 edit survived undo: ${s4text}`)
    return { remounted: true, sectionId: r.sectionId }
  })

  await test('directive 1: redo replays across sections', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s0'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.evaluate(() => (window as any).HOLO_APP.type('RRR'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())

    await page.evaluate(() => (window as any).HOLO_APP.undo())
    const after = await page.evaluate(() => (window as any).HOLO_APP.undo())

    const redone = await page.evaluate(() => (window as any).HOLO_APP.redo())
    ok(redone?.ok === true, `redo failed: ${JSON.stringify(redone)}`)
    ok(redone?.sectionId === 's0', `redo should target s0, got ${redone?.sectionId}`)
    void after

    const text = await page.evaluate(() => (window as any).HOLO_APP.caret().text)
    ok(text.includes('RRR'), `redo did not restore the edit: ${text}`)
    return { sectionId: redone.sectionId }
  })

  await test('directive 1: a new edit clears the redo branch', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.type('ONE'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())
    await page.evaluate(() => (window as any).HOLO_APP.undo())
    ok(
      (await page.evaluate(() => (window as any).HOLO_APP.historyState())).redoDepth === 1,
      'expected a redo entry after undo',
    )
    await page.evaluate(() => (window as any).HOLO_APP.type('TWO'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())
    const st = await page.evaluate(() => (window as any).HOLO_APP.historyState())
    ok(st.redoDepth === 0, `redo branch should be cleared, got ${st.redoDepth}`)
    return st
  })

  await test('directive 1: typing bursts group into one undo step', page, async () => {
    await reset(3)
    // A burst of separate transactions with no pause should collapse.
    await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      for (const ch of 'abcdefghij') app.type(ch)
      app.commit()
    })
    const st = await page.evaluate(() => (window as any).HOLO_APP.historyState())
    ok(
      st.undoDepth === 1,
      `10 keystrokes in one burst should be 1 undo step, got ${st.undoDepth}`,
    )
    return st
  })

  await test('directive 1: switching section breaks the undo group', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s0'))
    await page.evaluate(() => (window as any).HOLO_APP.type('aa'))
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.type('bb'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())
    const st = await page.evaluate(() => (window as any).HOLO_APP.historyState())
    ok(
      st.undoDepth === 2,
      `edits in two sections must be two undo steps, got ${st.undoDepth}`,
    )
    return st
  })

  // ======================================================================
  // DIRECTIVE 2: boundary traversal
  // ======================================================================

  await test('directive 2: ArrowUp at position 0 crosses into the previous section', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    // The key is dispatched to whatever currently has DOM focus, so the editor
    // must be focused and settled first. Without the wait this test is
    // order-dependent: it passes standalone and fails when a prior test has
    // left focus elsewhere.
    await page.waitForTimeout(150)
    const active = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      const ed = app.editor('s1')
      return document.activeElement === (ed.view.dom as HTMLElement)
    })
    ok(active, 'editor does not hold DOM focus before the keypress')

    await page.keyboard.press('ArrowUp')
    await waitForFocus(page, 's0')

    const focused = await page.evaluate(() => (window as any).HOLO_APP.registry.focused)
    ok(focused === 's0', `ArrowUp at start should focus s0, got ${focused}`)
    return { focused }
  })

  await test('directive 2: ArrowDown at the end crosses into the next section', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    // Put the caret at the very end of the section.
    const size = await page.evaluate(() => (window as any).HOLO_APP.caret().size)
    await page.evaluate(p => (window as any).HOLO_APP.setCaret(p), size - 1)

    await page.keyboard.press('ArrowDown')
    await waitForFocus(page, 's2')

    const focused = await page.evaluate(() => (window as any).HOLO_APP.registry.focused)
    ok(focused === 's2', `ArrowDown at end should focus s2, got ${focused}`)
    return { focused }
  })

  await test('directive 2: ArrowUp at the start of the first section is a no-op', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s0'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.keyboard.press('ArrowUp')
    await page.waitForTimeout(120)
    const focused = await page.evaluate(() => (window as any).HOLO_APP.registry.focused)
    ok(focused === 's0', `ArrowUp at document start must stay put, got ${focused}`)
    return { focused }
  })

  await test('directive 2: ArrowDown at the end of the last section is a no-op', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s2'))
    const size = await page.evaluate(() => (window as any).HOLO_APP.caret().size)
    await page.evaluate(p => (window as any).HOLO_APP.setCaret(p), size - 1)
    await page.keyboard.press('ArrowDown')
    await page.waitForTimeout(120)
    const focused = await page.evaluate(() => (window as any).HOLO_APP.registry.focused)
    ok(focused === 's2', `ArrowDown at document end must stay put, got ${focused}`)
    return { focused }
  })

  await test('directive 2: ArrowUp mid-section does not cross', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    // Caret in the middle of the first block, not at position 0.
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(20))
    await page.keyboard.press('ArrowUp')
    await page.waitForTimeout(120)
    const focused = await page.evaluate(() => (window as any).HOLO_APP.registry.focused)
    ok(focused === 's1', `ArrowUp mid-section must not cross, got ${focused}`)
    return { focused }
  })

  // ======================================================================
  // Visual column preservation (decision 2). The caret's screen column must
  // survive a crossing, resolved through the target's own layout rather than a
  // character-count guess.
  // ======================================================================

  /**
   * Two sections whose lines are deliberately different shapes.
   *
   * The default fixture gives every section the same line lengths, so a
   * character-offset caret placement and a coordinate-based one resolve to the
   * same character and the test cannot distinguish them. These do not: `a` ends
   * on a long line, `b` starts on a long line, so the column is expressible in
   * both, and a fixed offset would land in a visibly different place.
   */
  const LONG_A = 'alpha bravo charlie delta echo foxtrot golf hotel india juliet'
  const LONG_B = 'kilo lima mike november oscar papa quebec romeo sierra tango'
  const SHORT = 'tiny'

  async function resetAsymmetric(shortTarget = false): Promise<void> {
    await page.evaluate(
      ([a, target]) =>
        (window as any).HOLO_APP.loadRaw([
          {
            id: 's0',
            content: [
              { type: 'paragraph', content: [{ type: 'text', text: 'first of s0' }] },
              { type: 'paragraph', content: [{ type: 'text', text: a }] },
            ],
          },
          {
            id: 's1',
            content: [
              { type: 'paragraph', content: [{ type: 'text', text: 'start of s1' }] },
              { type: 'paragraph', content: [{ type: 'text', text: a }] },
            ],
          },
          {
            id: 's2',
            content: [{ type: 'paragraph', content: [{ type: 'text', text: target }] }],
          },
        ]),
      [LONG_A, shortTarget ? SHORT : LONG_B] as const,
    )
    await page.waitForTimeout(200)
  }

  /**
   * Focus a section, put the caret at its end, and confirm the editor really
   * holds DOM focus.
   *
   * The keypress goes to whatever `document.activeElement` is, so a test that
   * presses a key without checking focus is really testing whatever section
   * happened to be focused. Two of the tests below failed that way: the caret was
   * placed correctly but the key went to a stale editor, and the crossing simply
   * did not happen. It read like a layout bug.
   */
  async function focusAtEndOf(sectionId: string): Promise<void> {
    await page.evaluate(id => (window as any).HOLO_APP.focus(id), sectionId)
    await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      const ed = app.editor(app.registry.focused)
      return ed.commands.setTextSelection(ed.state.doc.content.size - 1)
    })
    await page.waitForTimeout(200)
    const held = await page.evaluate(id => {
      const app = (window as any).HOLO_APP
      const ed = app.editor(id)
      return document.activeElement === (ed?.view.dom as HTMLElement)
    }, sectionId)
    ok(held, `editor ${sectionId} does not hold DOM focus; the keypress would go elsewhere`)
  }

  await test('column: ArrowDown keeps the screen column of the last line', page, async () => {
    await resetAsymmetric()
    // ArrowDown crosses from the *end* of a section, so the source column is the
    // right edge of its last line. That is where column preservation is actually
    // observable: s2's first line is a different length but long enough to hold
    // the column, so a correct implementation lands at the same x rather than
    // clamping to the line end or resetting to the margin.
    await focusAtEndOf('s1')

    const before = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      return app.editor('s1').view.coordsAtPos(app.caret().from).left
    })
    const box = await page.evaluate(() => {
      const r = (document.querySelector('.section-slice') as HTMLElement).getBoundingClientRect()
      return { left: r.left, right: r.right, width: r.width }
    })
    // Guard: the source column must be well inside the box, or "preserved" is
    // indistinguishable from "clamped" and the assertion below proves nothing.
    ok(
      before > box.left + box.width * 0.4 && before < box.right - box.width * 0.1,
      `source column ${before.toFixed(0)} is not a good test column within [${box.left.toFixed(0)}, ${box.right.toFixed(0)}]`,
    )

    await page.keyboard.press('ArrowDown')
    await page.waitForTimeout(300)

    const after = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      const ed = app.editor(app.registry.focused)
      return { left: ed.view.coordsAtPos(app.caret().from).left, focused: app.registry.focused }
    })
    ok(after.focused === 's2', `ArrowDown at the end of s1 should focus s2, got ${after.focused}`)

    // Tolerance is one character width, not a magic pixel count. A caret can only
    // sit *between* characters, so resolving column X on a line of different
    // content snaps to whichever character boundary is nearest, which can be
    // nearly a full glyph away. A fixed 1px bound fails on that, and would also
    // pass a real jump on a narrow font. `coordsAtPos` reports the caret's own
    // width, which is the honest bound.
    const charWidth = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      const ed = app.editor('s2')
      // Measure a character in the middle of the target's first line, not the
      // caret. `coordsAtPos` at a line end returns a zero-width rect, so reading
      // the caret's own width reports 0 and the tolerance collapses to nothing.
      const probe = ed.view.posAtCoords({
        left: (ed.view.dom as HTMLElement).getBoundingClientRect().left + 30,
        top: (ed.view.dom as HTMLElement).getBoundingClientRect().top + 4,
      })
      if (!probe || probe.inside < 0) return null
      const a = ed.view.coordsAtPos(probe.pos)
      const b = ed.view.coordsAtPos(probe.pos + 1)
      return b.left > a.left ? b.left - a.left : null
    })
    ok(charWidth !== null, 'could not measure a character width in the target')
    const drift = Math.abs(after.left - before)
    ok(
      drift <= charWidth,
      `column drifted ${drift.toFixed(2)}px, more than one character width (${charWidth.toFixed(2)}px): ${before.toFixed(1)} -> ${after.left.toFixed(1)}`,
    )
    return {
      before: before.toFixed(1),
      after: after.left.toFixed(1),
      drift: drift.toFixed(2),
      charWidth: charWidth.toFixed(2),
    }
  })

  await test('column: the target comes from the layout, not a character offset', page, async () => {
    // The distinguishing assertion. A character-offset implementation would put
    // the caret at the same *character index* on the target line regardless of
    // layout. Here the two lines differ in length, so the coordinate answer and
    // the offset answer are different positions, and the caret must match the
    // coordinate one.
    await resetAsymmetric()
    await focusAtEndOf('s1')

    const sourceColumn = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      return app.editor('s1').view.coordsAtPos(app.caret().from).left
    })

    await page.keyboard.press('ArrowDown')
    await page.waitForTimeout(300)

    const result = await page.evaluate((left: number) => {
      const app = (window as any).HOLO_APP
      const ed = app.editor('s2')
      const rect = (ed.view.dom as HTMLElement).getBoundingClientRect()
      // Independently ask the layout where that column lands on s2's first line.
      const probe = ed.view.posAtCoords({ left, top: rect.top + 4 })
      // And where a character offset of 0 would have landed, for contrast.
      const offsetAnswer = ed.view.posAtCoords({ left: rect.left + 1, top: rect.top + 4 })
      return {
        focused: app.registry.focused,
        caretFrom: app.caret().from,
        layoutPos: probe?.pos ?? null,
        offsetPos: offsetAnswer?.pos ?? null,
      }
    }, sourceColumn)

    ok(result.focused === 's2', `expected s2, got ${result.focused}`)
    ok(result.layoutPos !== null, `posAtCoords found nothing on the target: ${JSON.stringify(result)}`)
    ok(
      result.caretFrom === result.layoutPos,
      `caret at ${result.caretFrom} but the layout says column ${sourceColumn.toFixed(0)} is position ${result.layoutPos}`,
    )
    // The contrast that proves the two strategies are not the same answer, so the
    // assertion above is not vacuous.
    ok(
      result.caretFrom !== result.offsetPos,
      `coordinate and offset answers coincide (${result.caretFrom}) — the fixture is not asymmetric enough to tell them apart`,
    )
    return { layoutPos: result.layoutPos, offsetPos: result.offsetPos, chose: result.caretFrom }
  })

  await test('column: ArrowUp lands on the last visual line of a wrapped paragraph', page, async () => {
    // ArrowUp only crosses from the *start* of a section, so its source column is
    // always the left margin. What the coordinate path buys here is vertical
    // rather than horizontal: with a final paragraph that wraps, the last visual
    // line is not the end of the text, and a character offset of 0 would put the
    // caret at the end of the block rather than at the start of the line the user
    // came from.
    await page.evaluate(() => {
      const long = new Array(24)
        .fill('bravo charlie delta echo foxtrot golf hotel india juliet')
        .join(' ')
      ;(window as any).HOLO_APP.loadRaw([
        { id: 's0', content: [{ type: 'paragraph', content: [{ type: 'text', text: long }] }] },
        { id: 's1', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'top of s1' }] }] },
      ])
    })
    await page.waitForTimeout(250)

    // Confirm the paragraph really wraps. If it fits on one line the test is
    // vacuous, because the start of the last line and the end of the text then
    // coincide.
    const paragraphHeight = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      const ed = app.editor('s0')
      return ed.view.coordsAtPos(ed.state.doc.content.size - 1).top - ed.view.coordsAtPos(1).top
    })
    ok(
      paragraphHeight > 20,
      `the paragraph is only ${paragraphHeight.toFixed(0)}px tall — it does not wrap, so this test proves nothing`,
    )

    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.waitForTimeout(200)

    await page.keyboard.press('ArrowUp')
    await page.waitForTimeout(300)

    const after = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      const ed = app.editor(app.registry.focused)
      return {
        focused: app.registry.focused,
        from: app.caret().from,
        docEnd: ed.state.doc.content.size - 1,
      }
    })
    ok(after.focused === 's0', `ArrowUp at the start of s1 should focus s0, got ${after.focused}`)
    ok(
      after.from !== after.docEnd,
      `caret landed at the end of the text (${after.from}); the coordinate path should place it at the start of the last visual line`,
    )
    return { from: after.from, docEnd: after.docEnd, paragraphHeight: paragraphHeight.toFixed(0) }
  })

  await test('column: a short target line clamps rather than landing off-layout', page, async () => {
    // When the source column is beyond the target line's width, the caret must
    // clamp to within the target's own layout rather than sit at a stale x or
    // fail to move at all.
    await resetAsymmetric(true)
    await focusAtEndOf('s1')
    await page.keyboard.press('ArrowDown')
    await page.waitForTimeout(300)

    // s2 is 'tiny', far shorter than s1's last line, so this must clamp.
    const result = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      const ed = app.editor('s2')
      const rect = (ed.view.dom as HTMLElement).getBoundingClientRect()
      const c = ed.view.coordsAtPos(app.caret().from)
      return { left: c.left, boxLeft: rect.left, boxRight: rect.right, focused: app.registry.focused }
    })
    ok(result.focused === 's2', `expected s2, got ${result.focused}`)
    ok(
      result.left >= result.boxLeft - 1 && result.left <= result.boxRight + 1,
      `caret at ${result.left.toFixed(1)} is outside the target box [${result.boxLeft.toFixed(1)}, ${result.boxRight.toFixed(1)}]`,
    )
    return { left: result.left.toFixed(1), boxWidth: (result.boxRight - result.boxLeft).toFixed(1) }
  })

  await test('column: an unlaid-out target still gets a caret (offset fallback)', page, async () => {
    // If the target cannot be measured, crossing must still move the caret.
    // Silently doing nothing would trap the user at the seam.
    await reset(3)
    const ok1 = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      // Explicitly request the degenerate path, which is what the host falls back
      // to when the target has no layout box.
      return (app.registry as any).focusEdge('s0', 'end', { kind: 'offset', chars: 3 })
    })
    ok(ok1 === true, `offset fallback did not place a caret: ${ok1}`)

    const at = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      const ed = app.editor('s0')
      return { from: app.caret().from, size: ed.state.doc.content.size }
    })
    ok(at.from >= 1 && at.from < at.size, `caret outside the section: ${JSON.stringify(at)}`)
    return at
  })

  await test('column: a coords hint into a laid-out target still succeeds', page, async () => {
    // The inverse of the fallback test: when a hint *is* given and the target is
    // laid out, the coordinates must win. If the offset path were taken here
    // instead, the caret would land at the section edge and the drift check in
    // the ArrowDown test above would be measuring the wrong thing.
    await reset(3)
    const result = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      app.focus('s0')
      const src = app.editor('s0')
      const left = src.view.coordsAtPos(30).left
      const target = app.editor('s1')
      const rect = (target.view.dom as HTMLElement).getBoundingClientRect()
      const fromCoords = (app.registry as any).focusEdge('s1', 'start', {
        kind: 'coords',
        hint: { left, side: 'start' },
      })
      return {
        fromCoords,
        caretFrom: app.caret().from,
        expected: target.view.posAtCoords({ left, top: rect.top + 4 })?.pos ?? null,
      }
    })
    ok(result.fromCoords === true, `focusEdge with a coords hint failed: ${result.fromCoords}`)
    ok(result.expected !== null, 'the target has no layout, so this test cannot discriminate')
    ok(
      result.caretFrom === result.expected,
      `caret at ${result.caretFrom} but the coordinates resolve to ${result.expected} — the offset path was taken`,
    )
    return { caretFrom: result.caretFrom, expected: result.expected }
  })

  await test('column: no dead column-guessing helpers remain', page, async () => {
    // The old implementation exported a candidate-offset list. If it is still
    // referenced anywhere, the coordinate path is not actually the live one.
    const src = await page.evaluate(async () => {
      // Built through a variable so this is a runtime specifier rather than a
      // compile-time import: the file exists for Vite, not for tsc, which has no
      // ambient declaration for a path served by the dev server.
      const spec = '/src/core/boundary.ts'
      const mod: any = await import(/* @vite-ignore */ spec)
      return {
        hasColumnAttempts: 'columnAttempts' in mod,
        exports: Object.keys(mod).sort(),
      }
    })
    ok(
      !src.hasColumnAttempts,
      'columnAttempts is still exported; the character-offset path is dead code, not removed',
    )
    return { exports: src.exports }
  })

  await test('directive 2: Backspace at position 0 merges into the previous section', page, async () => {
    await reset(3)
    // The key goes to whatever holds DOM focus. Without confirming the editor has
    // it, this test intermittently presses Backspace into a stale editor and reads
    // the failure as a broken merge — which is what it looked like across 8 runs
    // before this was checked.
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.waitForTimeout(150)
    const held = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      return document.activeElement === (app.editor('s1')?.view.dom as HTMLElement)
    })
    ok(held, 's1 does not hold DOM focus; the keypress would go elsewhere')

    const s0Before = await page.evaluate(() => {
      const ed = (window as any).HOLO_APP.editor('s0')
      return ed.state.doc.textContent.length
    })

    await page.keyboard.press('Backspace')
    await page.waitForTimeout(250)

    const s0After = await page.evaluate(() => {
      const ed = (window as any).HOLO_APP.editor('s0')
      return ed ? ed.state.doc.textContent.length : null
    })
    ok(s0After !== null, 's0 editor unavailable after merge')
    ok(
      s0After! > s0Before,
      `merge should have grown s0: ${s0Before} -> ${s0After}`,
    )
    return { s0Before, s0After }
  })

  await test('directive 2: Backspace mid-section deletes normally', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.type('XYZ'))
    const before = await page.evaluate(() => (window as any).HOLO_APP.caret().size)
    await page.keyboard.press('Backspace')
    await page.waitForTimeout(120)
    const after = await page.evaluate(() => (window as any).HOLO_APP.caret().size)
    const focused = await page.evaluate(() => (window as any).HOLO_APP.registry.focused)
    ok(focused === 's1', `Backspace mid-section must not cross, got ${focused}`)
    ok(after < before, `Backspace should have deleted: ${before} -> ${after}`)
    return { before, after }
  })

  await test('directive 2: the seam is invisible — typing continues after crossing', page, async () => {
    // The user-facing version of the ArrowDown test: cross the boundary, then
    // keep typing, and confirm the text lands in the new section.
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    const size = await page.evaluate(() => (window as any).HOLO_APP.caret().size)
    await page.evaluate(p => (window as any).HOLO_APP.setCaret(p), size - 1)

    await page.keyboard.press('ArrowDown')
    // The crossing has to have *happened* before typing, because `type` inserts into
    // whichever section is focused. Asserting afterwards that the text landed in s2 while
    // never establishing that focus arrived there is a test that passes or fails on timing.
    await waitForFocus(page, 's2')
    await page.evaluate(() => (window as any).HOLO_APP.type('CONT'))

    const s2 = await page.evaluate(() => {
      const ed = (window as any).HOLO_APP.editor('s2')
      return ed ? ed.state.doc.textContent : null
    })
    ok(s2 !== null, 's2 editor unavailable')
    ok(s2!.includes('CONT'), `typing after crossing did not land in s2: ${s2}`)
    return { s2head: s2!.slice(0, 60) }
  })

  await test('directive 2: crossing into an unmounted section mounts it', page, async () => {
    await reset(8)
    // Focus near the end so the window slides, then cross past its edge.
    await page.evaluate(() => (window as any).HOLO_APP.focus('s4'))
    await page.waitForTimeout(100)
    const size = await page.evaluate(() => (window as any).HOLO_APP.caret().size)
    await page.evaluate(p => (window as any).HOLO_APP.setCaret(p), size - 1)
    await page.keyboard.press('ArrowDown')
    await waitForFocus(page, 's5')
    const focused = await page.evaluate(() => (window as any).HOLO_APP.registry.focused)
    ok(focused === 's5', `expected to cross into s5, got ${focused}`)
    const mounted = await page.evaluate(() => (window as any).HOLO_APP.mountedIds())
    ok(mounted.includes('s5'), 's5 should be mounted after crossing')
    return { focused, mounted }
  })

  // ======================================================================
  // Interaction between the two directives
  // ======================================================================

  await test('interaction: a merge is undoable globally', page, async () => {
    await reset(3)
    // Same focus precondition as the Backspace test above, and for the same
    // reason: this presses Backspace and then asserts on the result.
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.waitForTimeout(150)
    const held = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      return document.activeElement === (app.editor('s1')?.view.dom as HTMLElement)
    })
    ok(held, 's1 does not hold DOM focus; the merge keypress would go elsewhere')

    const s0Before = await page.evaluate(() => {
      const ed = (window as any).HOLO_APP.editor('s0')
      return ed.state.doc.textContent.length
    })

    await page.keyboard.press('Backspace')
    await page.waitForTimeout(250)

    // Confirm the merge landed *before* undoing, rather than inferring it from the
    // undo's effect. Asserting only after undo means a failed merge and a failed
    // undo produce the same message, and the diagnostic points at the wrong one.
    const s0AfterMerge = await page.evaluate(() => {
      const ed = (window as any).HOLO_APP.editor('s0')
      return ed.state.doc.textContent.length
    })
    ok(s0AfterMerge > s0Before, `merge did not happen: ${s0Before} -> ${s0AfterMerge}`)

    await page.evaluate(() => (window as any).HOLO_APP.commit())

    const undone = await page.evaluate(() => (window as any).HOLO_APP.undo())
    ok(undone?.ok, `undo of the merge failed: ${JSON.stringify(undone)}`)
    await page.waitForTimeout(150)

    const s0AfterUndo = await page.evaluate(() => {
      const ed = (window as any).HOLO_APP.editor('s0')
      return ed ? ed.state.doc.textContent.length : null
    })
    ok(s0AfterUndo !== null, 's0 unavailable after undo')
    ok(
      s0AfterUndo! < s0AfterMerge,
      `undo should have shrunk s0 back: ${s0AfterMerge} -> ${s0AfterUndo}`,
    )
    return { s0Before, s0AfterMerge, s0AfterUndo }
  })

  // ======================================================================
  // Diagnosis. Both directive groups failed for a reason worth pinning down
  // rather than guessing at: undo captured nothing, and no boundary key was
  // ever handled.
  // ======================================================================

  await test('diagnose: what does the undo coordinator see?', page, async () => {
    await reset(3)
    const s = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      const ed = app.editor(app.registry.focused)
      app.type('Q')
      return {
        focused: app.registry.focused,
        // The coordinator is fed the dispatched transaction, so these show
        // pending captures rather than the current (empty) state transaction.
        history: app.historyState(),
        docSize: ed.state.doc.content.size,
      }
    })
    return s
  })

  await test('diagnose: are the boundary plugin and undo coordinator wired?', page, async () => {
    await reset(3)
    return await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      const ed = app.editor(app.registry.focused)
      const pluginKeys = ed.state.plugins.map((p: any) => p.spec.key?.key ?? '(anon)')
      return {
        hasBoundaryPlugin: pluginKeys.includes('holonomy-boundary$'),
        hasHandleKeyDown: typeof ed.view.someProp?.('handleKeyDown') === 'function',
        undoIsCoordinator: app.registry.undo?.constructor?.name,
        prevOf_s1: app.registry.previousOf('s1'),
        nextOf_s1: app.registry.nextOf('s1'),
      }
    })
  })

  await test('diagnose: what is the caret at the section start?', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    const c = await page.evaluate(() => (window as any).HOLO_APP.caret())
    return { from: c.from, parentOffset: c.parentOffset, depth: c.depth, index: c.index, size: c.size }
  })

  await test('diagnose: undo stack contents across two sections', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s0'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.evaluate(() => (window as any).HOLO_APP.type('AAA'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())

    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.evaluate(() => (window as any).HOLO_APP.type('BBB'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())

    return await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      const st = app.historyState()
      return {
        state: st,
        // Entries are now a list of per-section parts, so read through `parts`.
        undoEntries: (app.registry.undo as any).undoStack?.map((e: any) =>
          e.parts.map((p: any) => `${p.sectionId}:${p.undo.length}steps`).join(' + '),
        ),
        redoEntries: (app.registry.undo as any).redoStack?.map((e: any) =>
          e.parts.map((p: any) => p.sectionId).join('+'),
        ),
      }
    })
  })

  await test('diagnose: mergeBackward result', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    return await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      const ed0 = app.editor('s0')
      const ed1 = app.editor('s1')
      const before = {
        s0Size: ed0.state.doc.content.size,
        s0Text: ed0.state.doc.textContent.length,
        s0Children: ed0.state.doc.childCount,
        s1Children: ed1.state.doc.childCount,
        firstBlockIsText: ed1.state.doc.firstChild?.isTextblock,
      }
      // Call the host directly so we can see the return value and any throw.
      let result: any
      let threw: string | null = null
      try {
        result = (app.registry as any).boundaryHost.mergeBackward('s1', 's0')
      } catch (e: any) {
        threw = e.message
      }
      const ed0b = app.editor('s0')
      return {
        before,
        result,
        threw,
        after: {
          s0Size: ed0b ? ed0b.state.doc.content.size : null,
          s0Text: ed0b ? ed0b.state.doc.textContent.length : null,
          s0Children: ed0b ? ed0b.state.doc.childCount : null,
        },
      }
    })
  })

  await test('diagnose: where does a block insert actually land?', page, async () => {
    await reset(3)
    return await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      const out: Record<string, unknown> = {}
      // Fresh editor per candidate so probes cannot contaminate each other.
      for (const delta of [0, -1, -2]) {
        app.reset(3, 3)
        const ed = app.editor('s0')
        const size = ed.state.doc.content.size
        const pos = size + delta
        const rec: Record<string, unknown> = {
          size,
          pos,
          childrenBefore: ed.state.doc.childCount,
        }
        try {
          ed.view.dispatch(
            ed.state.tr.insert(pos, { type: 'paragraph', content: [{ type: 'text', text: 'X' }] }),
          )
          rec.ok = true
        } catch (e: any) {
          rec.ok = false
          rec.err = e.message
        }
        const after = app.editor('s0')
        rec.childrenAfter = after.state.doc.childCount
        rec.sizeAfter = after.state.doc.content.size
        rec.tail = after.state.doc.lastChild?.textContent?.slice(0, 20)
        out[`delta${delta}`] = rec
      }
      return out
    })
  })

  await test('FINAL directive 1: undo crosses section boundaries', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s0'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.evaluate(() => (window as any).HOLO_APP.type('AAA'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.evaluate(() => (window as any).HOLO_APP.type('BBB'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())

    const u1 = await page.evaluate(() => (window as any).HOLO_APP.undo())
    ok(u1?.sectionId === 's1' && u1.ok, `undo 1 wrong: ${JSON.stringify(u1)}`)

    const u2 = await page.evaluate(() => (window as any).HOLO_APP.undo())
    ok(
      u2?.sectionId === 's0' && u2.ok,
      `undo 2 must cross into s0 (history trapped?): ${JSON.stringify(u2)}`,
    )

    const s0 = await page.evaluate(() => (window as any).HOLO_APP.editor('s0').state.doc.textContent)
    const s1 = await page.evaluate(() => (window as any).HOLO_APP.editor('s1').state.doc.textContent)
    ok(!s0.includes('AAA'), `s0 edit survived: ${s0.slice(0, 40)}`)
    ok(!s1.includes('BBB'), `s1 edit survived: ${s1.slice(0, 40)}`)

    // A third undo must find nothing: undo must not have re-recorded itself.
    const u3 = await page.evaluate(() => (window as any).HOLO_APP.undo())
    ok(u3 === null, `undo should be exhausted, got ${JSON.stringify(u3)}`)
    return { u1: u1.sectionId, u2: u2.sectionId, u3 }
  })

  await test('FINAL directive 1: redo replays after a cross-section undo', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s0'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.evaluate(() => (window as any).HOLO_APP.type('AAA'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.evaluate(() => (window as any).HOLO_APP.type('BBB'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())

    await page.evaluate(() => (window as any).HOLO_APP.undo())
    await page.evaluate(() => (window as any).HOLO_APP.undo())

    // Redo replays in the opposite order to undo: undo popped s1 then s0, so
    // redo must reapply s0 then s1. The primary section reported is the
    // last-touched one, which for a single-section entry is that section.
    const r1 = await page.evaluate(() => (window as any).HOLO_APP.redo())
    ok(r1?.ok && r1.sectionId === 's0', `redo 1 wrong: ${JSON.stringify(r1)}`)
    const r2 = await page.evaluate(() => (window as any).HOLO_APP.redo())
    ok(r2?.ok && r2.sectionId === 's1', `redo 2 wrong: ${JSON.stringify(r2)}`)

    const s0 = await page.evaluate(() => (window as any).HOLO_APP.editor('s0').state.doc.textContent)
    const s1 = await page.evaluate(() => (window as any).HOLO_APP.editor('s1').state.doc.textContent)
    ok(s0.includes('AAA'), `redo did not restore s0: ${s0.slice(0, 40)}`)
    ok(s1.includes('BBB'), `redo did not restore s1: ${s1.slice(0, 40)}`)
    return { r1: r1.sectionId, r2: r2.sectionId }
  })

  await test('FINAL directive 2: ArrowUp crosses at position 0', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.waitForTimeout(80)
    await page.keyboard.press('ArrowUp')
    await page.waitForTimeout(150)
    const focused = await page.evaluate(() => (window as any).HOLO_APP.registry.focused)
    ok(focused === 's0', `ArrowUp at start should focus s0, got ${focused}`)
    return { focused }
  })

  await test('FINAL interaction: a merge is one undo step', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    const before = await page.evaluate(
      () => (window as any).HOLO_APP.editor('s0').state.doc.content.size,
    )

    await page.keyboard.press('Backspace')
    await page.waitForTimeout(250)

    const merged = await page.evaluate(
      () => (window as any).HOLO_APP.editor('s0').state.doc.content.size,
    )
    ok(merged > before, `merge should grow s0: ${before} -> ${merged}`)

    // One undo must revert the whole merge, both halves.
    await page.evaluate(() => (window as any).HOLO_APP.undo())
    await page.waitForTimeout(150)
    const reverted = await page.evaluate(
      () => (window as any).HOLO_APP.editor('s0').state.doc.content.size,
    )
    ok(
      reverted < merged,
      `undo must shrink s0 back from the merge: ${merged} -> ${reverted}`,
    )
    return { before, merged, reverted }
  })

  // ======================================================================
  // Diagnostics, kept because they are how the bugs above were found.
  // ======================================================================

  await test('diagnose: full two-section undo sequence step by step', page, async () => {
    await reset(3)
    const trace: any[] = []
    const snap = () =>
      page.evaluate(() => {
        const app = (window as any).HOLO_APP
        const st = app.historyState()
        return {
          focused: app.registry.focused,
          state: st,
          entries: (app.registry.undo as any).undoStack?.map((e: any) => ({
            sections: e.parts.map((p: any) => p.sectionId),
            steps: e.parts.map((p: any) => p.undo.length),
          })),
          s0hasAAA: (() => {
            const e = app.editor('s0')
            return e ? e.state.doc.textContent.includes('AAA') : null
          })(),
          s1hasBBB: (() => {
            const e = app.editor('s1')
            return e ? e.state.doc.textContent.includes('BBB') : null
          })(),
        }
      })

    trace.push({ step: 'initial', ...(await snap()) })

    await page.evaluate(() => (window as any).HOLO_APP.focus('s0'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.evaluate(() => (window as any).HOLO_APP.type('AAA'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())
    trace.push({ step: 'after AAA in s0', ...(await snap()) })

    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.evaluate(() => (window as any).HOLO_APP.type('BBB'))
    await page.evaluate(() => (window as any).HOLO_APP.commit())
    trace.push({ step: 'after BBB in s1', ...(await snap()) })

    const u1 = await page.evaluate(() => (window as any).HOLO_APP.undo())
    trace.push({ step: 'undo 1', result: u1, ...(await snap()) })

    const u2 = await page.evaluate(() => (window as any).HOLO_APP.undo())
    trace.push({ step: 'undo 2', result: u2, ...(await snap()) })

    const u3 = await page.evaluate(() => (window as any).HOLO_APP.undo())
    trace.push({ step: 'undo 3 (should be null)', result: u3, ...(await snap()) })

    return trace
  })

  await test('diagnose: ArrowUp crossing — is the key reaching the editor?', page, async () => {
    await reset(3)
    await page.evaluate(() => (window as any).HOLO_APP.focus('s1'))
    await page.evaluate(() => (window as any).HOLO_APP.setCaret(1))
    await page.waitForTimeout(100)

    const before = await page.evaluate(() => {
      const app = (window as any).HOLO_APP
      const ed = app.editor('s1')
      const dom = ed.view.dom as HTMLElement
      return {
        focused: app.registry.focused,
        caret: app.registry ? undefined : undefined,
        activeIsEditor: document.activeElement === dom,
        activeTag: document.activeElement?.className,
        selectionInEditor: (() => {
          const s = window.getSelection()
          return s && dom.contains(s.anchorNode) ? 'yes' : `no (${s?.anchorNode?.nodeName})`
        })(),
      }
    })

    await page.keyboard.press('ArrowUp')
    await page.waitForTimeout(200)
    const after = await page.evaluate(() => (window as any).HOLO_APP.registry.focused)
    return { before, after }
  })

  // ======================================================================
  console.log(`\n${passed} passed, ${failed} failed`)
  if (failed) {
    console.log(`failing: ${failures.join(', ')}`)
  }
  await browser.close()
  process.exit(failed === 0 ? 0 : 1)
}

main().catch(e => {
  console.error(e)
  process.exit(1)
})
