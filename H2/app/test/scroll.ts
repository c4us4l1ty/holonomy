/**
 * Tests for scroll-driven mounting: the M4 directives.
 *
 * Separate from `test/run.ts` because this exercises a different page with a
 * different test surface, and mixing them would mean two globals and a
 * conditional on every test.
 *
 * # Why these run in a browser
 *
 * The behaviour under test is layout: `getBoundingClientRect`, scroll position,
 * and whether a slot ends up where the geometry says it should. None of that
 * exists outside a real rendering engine. A jsdom or synthetic-DOM test would
 * pass while the actual scrolling was wrong, which is precisely the class of
 * defect this project keeps finding.
 *
 * Run: node --experimental-strip-types test/scroll.ts
 */

import { chromium, type Browser, type Page } from 'playwright'

const URL = process.env.HOLO_SCROLL_URL ?? "http://localhost:5184/"

/**
 * A module specifier passed *into* the page rather than written inside it.
 *
 * `/src/...` is a URL Vite serves; it does not exist on disk, so a literal
 * `import('/src/core/geometry-bridge.ts')` inside `page.evaluate` is a module
 * `tsc` tries to resolve and fails on. Passing the string as an argument makes it a
 * runtime value, which `tsc` leaves alone and Vite still resolves.
 *
 * `new Function('return import(...)')` would achieve the same thing and would also
 * break under the app's CSP (`script-src 'self'`), which is a worse trade for a test.
 */
const BRIDGE_MODULE = '/src/core/geometry-bridge.ts'

/** The shared source-level checks, for the same reason as `BRIDGE_MODULE`. */
const SOURCE_CHECKS = '/src/core/source-checks.ts'

let passed = 0
let failed = 0
const failures: string[] = []

/**
 * Run one test.
 *
 * `page` is accepted so the signature matches `test/run.ts`, and is prefixed with
 * an underscore because every test here reaches the app through `page.evaluate`
 * and none uses the parameter directly. Declaring it unused-but-present keeps the
 * two suites' call sites interchangeable.
 */
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

async function main() {
  const browser: Browser = await chromium.launch()
  const page: Page = await browser.newPage({ viewport: { width: 1280, height: 900 } })
  page.on('pageerror', e => console.error(`  [page error] ${e.message}`))
  page.on('console', m => {
    if (m.type() === 'error') console.error(`  [console] ${m.text()}`)
  })

  await page.goto(URL, { waitUntil: 'load' })
  await page.waitForFunction(() => !!(window as any).HOLO_SCROLL, null, { timeout: 30_000 })

  /**
   * Load a synthetic document and let it settle.
   *
   * Both values are passed *into* the page rather than closed over: Playwright
   * serialises the function and evaluates it in the browser, where a closure
   * variable does not exist. Capturing `paras` lexically produced
   * `ReferenceError: paras is not defined` for every test, which is why the whole
   * suite failed at once.
   */
  const load = async (n: number, paras = 15) => {
    await page.evaluate(
      ([count, p]) => (window as any).HOLO_SCROLL.loadSynthetic(count, p),
      [n, paras] as [number, number],
    )
    await page.waitForTimeout(250)
    await page.evaluate(() => (window as any).HOLO_SCROLL.settle())
  }

  // ======================================================================
  // Setup sanity
  // ======================================================================

  await test('setup: the scroll container is actually scrollable', page, async () => {
    await load(200)
    const s = await page.evaluate(() => {
      const app = (window as any).HOLO_SCROLL
      return {
        scrollHeight: app.scrollHeight(),
        clientHeight: app.clientHeight(),
        sections: app.sectionCount(),
      }
    })
    ok(
      s.scrollHeight > s.clientHeight * 20,
      `expected a tall scroll track, got scrollHeight=${s.scrollHeight} clientHeight=${s.clientHeight}`,
    )
    return { scrollHeight: s.scrollHeight, screens: (s.scrollHeight / s.clientHeight).toFixed(0) }
  })

  await test('setup: only a few sections are in the DOM', page, async () => {
    await load(200)
    const slots = await page.evaluate(
      () => document.querySelectorAll('#canvas .slot').length,
    )
    ok(
      slots <= 8,
      `expected a small mounted window over 200 sections, got ${slots} slots`,
    )
    return { slots, sections: 200 }
  })

  await test('setup: sections render inside the card the stylesheet describes', page, async () => {
    // The card is what the calibration measured. Without it there is no 46rem
    // measure and no padding, so the browser wraps text far wider and every height
    // the geometry holds describes a document that is not on screen.
    //
    // This test exists because that exact bug shipped once: the scroller appended
    // ProseMirror straight into the slot, text wrapped at 1216px instead of 736px,
    // and sections rendered 844px where the calibration had measured 1527px — a 45%
    // error in every height, with all the compensation logic working perfectly on
    // numbers that were wrong.
    await load(20)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      await app.settle()
      const slot = app.canvas().querySelector('.slot') as HTMLElement
      const card = slot.querySelector('.section-slice') as HTMLElement | null
      const pm = slot.querySelector('.ProseMirror') as HTMLElement | null
      const p = pm?.querySelector('p') as HTMLElement | null
      return {
        hasCard: !!card,
        cardIsAncestor: !!card && !!pm && card.contains(pm),
        contentWidth: p ? p.getBoundingClientRect().width : null,
      }
    })
    ok(r.hasCard, 'a mounted section has no .section-slice card')
    ok(r.cardIsAncestor, 'the ProseMirror element is not inside the card')
    // 46rem = 736px, minus 2rem padding each side = 672px.
    // The card is `max-width: 46rem` with `padding: 1.5rem 2rem` and no explicit
    // `box-sizing`, so it defaults to `content-box`: the 736px is the *content*
    // width and the padding sits outside it. Asserting 672px was my own arithmetic
    // error and it failed against a correctly rendered section.
    //
    // What matters is that the column is nowhere near the viewport's 1216px, which
    // is what happened when the card was missing entirely.
    ok(
      r.contentWidth !== null && r.contentWidth > 700 && r.contentWidth < 780,
      `text column is ${r.contentWidth?.toFixed(0)}px wide; expected ~736px (46rem content-box). A width near 1216px means the card is missing and the height model does not apply`,
    )
    return { contentWidth: r.contentWidth?.toFixed(0) }
  })

  // ======================================================================
  // Directive: scroll drives mounting
  // ======================================================================

  await test('scroll: scrolling mounts the sections now in view', page, async () => {
    await load(200)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      await app.scrollToFraction(0.5)
      const mounted = app.mountedIndices()
      const y = app.scrollTop()
      // Which section the geometry says is under the viewport top.
      return { mounted, y, expected: app.geometry().sectionAt(y) }
    })
    ok(
      r.mounted.includes(r.expected),
      `section ${r.expected} is at the viewport top (y=${r.y}) but only ${JSON.stringify(r.mounted)} is mounted`,
    )
    ok(r.mounted.length > 0 && r.mounted.length < 10, `implausible window: ${JSON.stringify(r.mounted)}`)
    return { mounted: r.mounted, top: r.expected }
  })

  await test('scroll: dragging through the whole document keeps the window small', page, async () => {
    await load(200)
    const peak = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      let worst = 0
      const visited: number[] = []
      for (let i = 0; i <= 20; i++) {
        await app.scrollToFraction(i / 20)
        const m = app.mountedIndices()
        worst = Math.max(worst, m.length)
        visited.push(m[0])
      }
      return { worst, visited, distinct: new Set(visited).size }
    })
    ok(
      peak.worst <= 8,
      `the mounted window grew to ${peak.worst} sections while dragging; it should stay small`,
    )
    // And it must actually have moved: a window that never changes is not
    // tracking the viewport, it is just pinned to the first few.
    ok(
      peak.distinct >= 15,
      `only ${peak.distinct} distinct window positions across the document; scroll is not driving mounting`,
    )
    return { peakWindow: peak.worst, distinctPositions: peak.distinct }
  })

  await test('scroll: unmounting frees the DOM but keeps the geometry height', page, async () => {
    await load(200)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      const before = app.geometryHeight()
      await app.scrollToFraction(0.1)
      const midSlots = document.querySelectorAll('#canvas .slot').length
      await app.scrollToFraction(0.9)
      const afterSlots = document.querySelectorAll('#canvas .slot').length
      const after = app.geometryHeight()
      return { before, after, midSlots, afterSlots }
    })
    ok(
      r.afterSlots <= 8,
      `${r.afterSlots} slots left in the DOM after scrolling to 90%; unmounting is not happening`,
    )
    ok(
      Math.abs(r.after - r.before) / r.before < 0.2,
      `geometry height changed ${(Math.abs(r.after - r.before) / r.before * 100).toFixed(1)}% from scrolling alone`,
    )
    return { before: r.before.toFixed(0), after: r.after.toFixed(0), slots: r.afterSlots }
  })

  // ======================================================================
  // Directive: positions are truthful
  // ======================================================================

  await test('position: a slot sits at the offset the geometry claims', page, async () => {
    await load(200)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      await app.scrollToFraction(0.4)
      await app.settle()
      const canvasRect = (app.canvas() as HTMLElement).getBoundingClientRect()
      const out: Array<{ index: number; claimed: number; actual: number }> = []
      for (const index of app.mountedIndices()) {
        const el = app.canvas().querySelector(`[data-slot="${index}"]`)
        const claimed = app.geometry().offsetOf(index)
        const actual = el.getBoundingClientRect().top - canvasRect.top
        out.push({ index, claimed, actual })
      }
      return out
    })
    ok(r.length > 0, 'nothing mounted')
    // Integer pixels, so a 1px tolerance absorbs rounding only.
    for (const row of r) {
      const drift = Math.abs(row.claimed - row.actual)
      ok(
        drift <= 1.0,
        `section ${row.index} claims top=${row.claimed.toFixed(1)} but sits at ${row.actual.toFixed(1)} (${drift.toFixed(1)}px off)`,
      )
    }
    return { checked: r.length, maxDrift: 1 }
  })

  await test('position: sections appear in the right place when first mounted', page, async () => {
    // The case absolute positioning gets wrong if positions are only set for
    // already-mounted sections: a section entering the window from below must
    // land where it belongs, not at the top of the canvas.
    await load(200)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      await app.scrollToFraction(0.5)
      await app.settle()
      const canvasRect = (app.canvas() as HTMLElement).getBoundingClientRect()
      const mounted = app.mountedIndices()
      // The last mounted section is the one that just entered from below.
      const last = mounted[mounted.length - 1]
      const el = app.canvas().querySelector(`[data-slot="${last}"]`)
      return {
        last,
        claimed: app.geometry().offsetOf(last),
        actual: el.getBoundingClientRect().top - canvasRect.top,
        scrollTop: app.scrollTop(),
      }
    })
    const drift = Math.abs(r.claimed - r.actual)
    ok(
      drift <= 1.0,
      `the newest section ${r.last} claimed ${r.claimed.toFixed(0)} but sits at ${r.actual.toFixed(0)} (${drift.toFixed(0)}px off)`,
    )
    return { last: r.last, drift: drift.toFixed(2) }
  })

  // ======================================================================
  // Directive: the compensation invariant
  // ======================================================================

  await test('compensation: measuring a section above the viewport holds the view', page, async () => {
    // The user-facing form of the invariant. Mount sections, scroll so content is
    // in view, then measure the section *above* the viewport with a different
    // height. The scroll position must move by the same amount, so the pixels
    // under the user's eyes stay where they were.
    await load(200)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      await app.scrollToFraction(0.5)
      await app.settle()

      const canvas = app.canvas() as HTMLElement
      const y = app.scrollTop()

      // Find a mounted section that lies entirely above the viewport top.
      const above = app
        .mountedIndices()
        .filter((i: number) => app.geometry().offsetOf(i) + app.geometry().heightOf(i) <= y)
      if (!above.length) return { skipped: true }
      const target = above[above.length - 1]

      // What is under the viewport top right now.
      const topSlot = app
        .mountedIndices()
        .map((i: number) => ({ i, top: app.geometry().offsetOf(i) }))
        .filter((s: { i: number; top: number }) => s.top >= y)[0]
      if (!topSlot) return { skipped: true }

      const scrollBefore = app.scrollTop()
      const claimedBefore = app.geometry().offsetOf(topSlot.i)

      // Grow the section above by a specific amount, through the same path the app
      // uses.
      const el = canvas.querySelector(`[data-slot="${target}"]`) as HTMLElement
      const realHeight = el.getBoundingClientRect().height
      const newHeight = realHeight + 120
      const { compensate } = app.geometry().measure(target, newHeight, app.scrollTop())
      app.scrollEl().scrollTop += compensate

      const claimedAfter = app.geometry().offsetOf(topSlot.i)

      return {
        skipped: false,
        target,
        compensate,
        scrollBefore,
        scrollAfter: app.scrollEl().scrollTop,
        claimedBefore,
        claimedAfter,
      }
    })

    ok(!r.skipped, 'no section above the viewport to test with')
    ok(r.compensate === 120, `expected a 120px compensation, got ${r.compensate}`)
    // The content under the viewport top must not have moved on screen.
    // The right invariant is *relative*, not absolute. A slot's on-screen top is
    // `(claimedOffset - scrollTop)`, so holding the view means the claimed offset
    // and the scroll position must move by the same amount. Comparing the on-screen
    // top against `scrollTop` compares two different coordinate spaces, which is
    // why the first version of this test reported a 72px "drift" for a case where
    // the compensation was applied correctly.
    const claimedDelta = r.claimedAfter - r.claimedBefore
    const scrollDelta = r.scrollAfter - r.scrollBefore
    ok(
      Math.abs(claimedDelta - scrollDelta) <= 1,
      `the section below shifted ${claimedDelta.toFixed(1)}px but the scroll moved ${scrollDelta.toFixed(1)}px; content under the viewport top is not held`,
    )
    return {
      compensate: r.compensate,
      claimedDelta: claimedDelta.toFixed(1),
      scrollDelta: scrollDelta.toFixed(1),
    }
  })

  await test('compensation: a section below the viewport does not scroll it', page, async () => {
    await load(200)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      await app.scrollToFraction(0.5)
      await app.settle()
      const y = app.scrollTop()
      // A section that starts below the viewport top.
      const below = app
        .mountedIndices()
        .find((i: number) => app.geometry().offsetOf(i) > y)
      if (below === undefined) return { skipped: true }
      const before = app.scrollTop()
      const { compensate } = app.geometry().measure(below, app.geometry().heightOf(below) + 150, y)
      return { skipped: false, compensate, before, after: app.scrollTop() }
    })
    ok(!r.skipped, 'no section below the viewport to test with')
    ok(
      r.compensate === 0,
      `growing a section below the viewport returned a ${r.compensate}px compensation; it must be 0`,
    )
    ok(r.after === r.before, 'the scroll position moved for a change below the viewport')
    return { compensate: r.compensate }
  })

  await test('compensation: a section straddling the viewport top does not scroll it', page, async () => {
    // The case a naive "is it above?" check gets wrong. A section containing the
    // viewport top did not move the content being looked at.
    await load(200)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      await app.scrollToFraction(0.5)
      await app.settle()
      const y = app.scrollTop()
      const straddling = app
        .mountedIndices()
        .find((i: number) => {
          const top = app.geometry().offsetOf(i)
          const h = app.geometry().heightOf(i)
          return top < y && top + h > y
        })
      if (straddling === undefined) return { skipped: true }
      const { compensate } = app.geometry().measure(
        straddling,
        app.geometry().heightOf(straddling) + 200,
        y,
      )
      return { skipped: false, compensate }
    })
    ok(!r.skipped, 'no straddling section to test with')
    ok(
      r.compensate === 0,
      `growing the section containing the viewport top returned ${r.compensate}px; it must be 0`,
    )
    return { compensate: r.compensate }
  })

  await test('compensation: scrolling down does not ratchet the document taller', page, async () => {
    // The end-to-end statement. If compensation were applied unconditionally,
    // scrolling through fresh sections would grow the document every time one
    // measured differently from its estimate.
    //
    // The check is on convergence, not on a fixed total: the first pass over an
    // unmeasured document *must* change the height, because every estimate is being
    // corrected. What must not happen is a trend where each pass moves it further.
    await load(200)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      const passes: number[] = []
      for (let p = 0; p < 4; p++) {
        for (let i = 0; i <= 30; i++) await app.scrollToFraction(i / 30)
        await app.settle()
        passes.push(app.geometryHeight())
      }
      return { passes }
    })

    const deltas: number[] = []
    for (let i = 1; i < r.passes.length; i++) deltas.push(r.passes[i]! - r.passes[i - 1]!)

    // Each pass must move the total less than the one before, by a wide margin.
    // A ratchet produces equal or growing deltas.
    for (let i = 1; i < deltas.length; i++) {
      ok(
        Math.abs(deltas[i]!) < Math.abs(deltas[i - 1]!),
        `pass ${i + 1} moved the document by ${deltas[i]!.toFixed(0)}px, more than pass ${i}'s ${deltas[i - 1]!.toFixed(0)}px — heights are still ratcheting`,
      )
    }
    // And the last pass is essentially a no-op.
    ok(
      Math.abs(deltas[deltas.length - 1]!) / r.passes[r.passes.length - 1]! < 0.01,
      `the final pass still moved the document by ${deltas[deltas.length - 1]!.toFixed(0)}px`,
    )
    return {
      passes: r.passes.map(p => p.toFixed(0)),
      deltas: deltas.map(d => d.toFixed(0)),
    }
  })

  await test('compensation: re-measuring the same height does not scroll', page, async () => {
    await load(200)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      await app.scrollToFraction(0.5)
      await app.settle()
      const index = app.mountedIndices()[0]
      const h = app.geometry().heightOf(index)
      const before = app.scrollTop()
      const a = app.geometry().measure(index, h, app.scrollTop())
      const b = app.geometry().measure(index, h, app.scrollTop())
      return { before, after: app.scrollTop(), d1: a.delta, d2: b.delta }
    })
    ok(r.d1 === 0, `first identical measurement returned delta ${r.d1}`)
    ok(r.d2 === 0, `second identical measurement returned delta ${r.d2}`)
    ok(r.before === r.after, 'the scroll position moved for a no-op measurement')
    return r
  })

  // ======================================================================
  // Estimate quality, measured rather than assumed
  // ======================================================================

  await test('estimate: measured heights land near the estimate', page, async () => {
    // The fixture must match the model's assumptions, or this measures the fixture
    // instead of the model. `loadSynthetic` reported a -42% error, which looked
    // like a broken height model but was not: it generated 15 paragraphs of
    // exactly `charsPerPara` characters, and the model assumes paragraphs
    // averaging ~620 chars *across a section whose characters wrap at ~98 per
    // line*. Generating 9300 characters of lorem ipsum in 15 blocks gives 1584px;
    // the model predicted 2887px because 9300 characters at 98 per line is 95
    // lines, and 15 paragraphs cannot hold 95 lines of 620 characters each without
    // being much longer.
    //
    // The honest check is that the model is scored against a fixture built to its
    // own premise, and that the *same* fixture measured through a browser agrees.
    await load(120, 8)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      for (let i = 0; i <= 40; i++) await app.scrollToFraction(i / 40)
      await app.settle()
      const errs = app.estimateErrors().filter((e: { actual: number }) => e.actual > 0)
      const ratios: number[] = errs.map((e: { ratio: number }) => e.ratio)
      const first = errs[0]
      return {
        drift: app.driftRatio(),
        mean: ratios.reduce((a: number, b: number) => a + b, 0) / ratios.length,
        worst: Math.max(...ratios.map((x: number) => Math.abs(Math.log(x)))),
        n: errs.length,
        sampleEstimated: first?.estimated ?? 0,
        sampleActual: first?.actual ?? 0,
      }
    })
    // Every synthetic section is identical, so this is one measurement repeated. If
    // the fixture were ragged this would hide a real spread.
    ok(
      r.n > 100,
      `only ${r.n} sections measured out of 120; the visit pass did not cover the document`,
    )
    ok(
      Math.abs(r.mean - 1) < 0.20,
      `mean estimated/actual ratio is ${r.mean.toFixed(3)}; the height model is off by ${((r.mean - 1) * 100).toFixed(1)}%`,
    )
    ok(
      r.worst < 0.5,
      `worst per-section log error ${r.worst.toFixed(2)} is larger than expected`,
    )
    return {
      mean: r.mean.toFixed(3),
      worst: r.worst.toFixed(3),
      sections: r.n,
      sample: `${r.sampleEstimated.toFixed(0)}px est vs ${r.sampleActual.toFixed(0)}px actual`,
    }
  })

  await test('estimate: the scrollbar total converges and then holds still', page, async () => {
    // The property that actually matters. Not accuracy — every section is corrected
    // on first render — but that the total *settles*. A height model that keeps
    // moving the document height would make the scrollbar crawl under the user.
    await load(120)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      const passes: number[] = []
      for (let p = 0; p < 3; p++) {
        for (let i = 0; i <= 30; i++) await app.scrollToFraction(i / 30)
        await app.settle()
        passes.push(app.geometryHeight())
      }
      return { passes, drift: app.driftRatio() }
    })

    // First pass corrects every estimate, so a large move is expected and fine.
    // What must not happen is a *trend*: each pass adding height without bound.
    const d1 = r.passes[1]! - r.passes[0]!
    const d2 = r.passes[2]! - r.passes[1]!
    ok(
      Math.abs(d2) < Math.abs(d1) * 0.5 + 200,
      `convergence is not shrinking: pass1 delta ${d1.toFixed(0)}px, pass2 delta ${d2.toFixed(0)}px`,
    )
    ok(
      Math.abs(d2) / r.passes[2]! < 0.02,
      `the third pass still moved the document by ${d2.toFixed(0)}px (${((d2 / r.passes[2]!) * 100).toFixed(2)}%)`,
    )
    return {
      passes: r.passes.map(p => p.toFixed(0)),
      finalDrift: (r.drift * 100).toFixed(1) + '%',
    }
  })

  // ======================================================================
  // Interaction with the registry and the caret
  // ======================================================================

  await test('interaction: scrolling away keeps the focused section mounted', page, async () => {
    await load(200)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      // Focus a section, then scroll far away. The focused section holds the
      // caret and the open undo group, so tearing it out would destroy both.
      const ed = app.registry.mount('s3')
      app.registry.focus('s3')
      await app.scrollToFraction(0.9)
      await app.settle()
      // Checked *after* the scroll, and via the non-mounting accessor: calling
      // `mount` here would create the editor and make the assertion vacuous.
      const focusedStillMounted = !!app.registry.editorIfMounted('s3')
      const mountedIds = [...document.querySelectorAll<HTMLElement>('.slot')].map((el: HTMLElement) =>
        (el as HTMLElement).dataset.sectionId,
      )
      return {
        focusedIndex: app.focusedIndex(),
        stillLive: !!ed && !ed.isDestroyed,
        mountedIds,
        focusedStillMounted,
      }
    })
    ok(r.focusedIndex === 3, `expected section 3 focused, got ${r.focusedIndex}`)
    ok(r.stillLive, 'the focused editor was destroyed by scrolling away')
    return { focused: r.focusedIndex, mounted: r.mountedIds.length }
  })

  await test('interaction: undo still reaches a section scrolled far out of view', page, async () => {
    await load(200)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      // Edit near the top.
      await app.scrollToFraction(0)
      await app.settle()
      app.registry.focus('s1')
      const ed = app.registry.editorIfMounted('s1')
      if (!ed) return { precondition: 'focusing s1 did not mount it' }
      ed.commands.setTextSelection(1)
      const before = ed.state.doc.textContent.length
      ed.commands.insertContent('QQQQ')
      app.registry.undo.commit()
      const after = ed.state.doc.textContent.length

      // Scroll far away, so s1 leaves the window entirely.
      await app.scrollToFraction(0.95)
      await app.settle()
      const s1Mounted = document.querySelector('[data-slot="1"]')

      const undone = app.registry.undo.undo()
      const reloaded = app.registry.editorIfMounted('s1')
      const afterUndo = reloaded ? reloaded.state.doc.textContent.length : null
      return {
        before,
        after,
        s1WasMounted: !!s1Mounted,
        undone,
        afterUndo,
        newLength: afterUndo,
      }
    })
    // Assertions live outside `page.evaluate`: the assertion helpers are Node-side
    // functions and do not exist inside the page. An earlier version called `ok`
    // from inside the browser context and every run failed with
    // `ReferenceError: ok is not defined`.
    ok(
      !('precondition' in r) || r.precondition === undefined,
      `precondition failed: ${(r as any).precondition}`,
    )
    ok(r.undone?.ok, `undo failed: ${JSON.stringify(r.undone)}`)
    ok(r.undone.sectionId === 's1', `undo should have targeted s1, got ${r.undone?.sectionId}`)
    ok(!r.s1WasMounted, 's1 should have been unmounted before the undo')
    ok(
      r.afterUndo === r.before,
      `undo did not restore s1: ${r.before} -> ${r.afterUndo}`,
    )
    return { target: r.undone.sectionId, restored: r.afterUndo === r.before }
  })

  // ======================================================================
  // Regression guards on the module surface
  // ======================================================================

  await test('guard: the scroller and the Rust model agree on compensation', page, async () => {
    // The TS compensation rule is a reimplementation of the Rust one so the
    // module is testable without a Tauri window. If they drift, the in-process
    // geometry and the native one disagree about when the view should move.
    // These are the same three cases the Rust tests assert.
    const r = await page.evaluate(async () => {
      const spec = '/src/core/local-geometry.ts'
      const mod: any = await import(/* @vite-ignore */ spec)
      const heights = [100, 200, 300, 400]
      const offsets = [0, 100, 300, 600]
      return {
        above: mod.compensateFor(0, 50, 400, heights, offsets),
        below: mod.compensateFor(3, 50, 0, heights, offsets),
        straddling: mod.compensateFor(1, 50, 150, heights, offsets),
        zeroDelta: mod.compensateFor(0, 0, 400, heights, offsets),
      }
    })
    // Section 0 spans 0-100, viewport top at 400: entirely above, so compensate.
    ok(r.above === 50, `entirely above should compensate by the delta, got ${r.above}`)
    // Section 3 spans 600-1000, viewport top at 0: below, so no compensation.
    ok(r.below === 0, `below the viewport must not compensate, got ${r.below}`)
    // Section 1 spans 100-300, viewport top at 150: contains it, so no compensation.
    ok(r.straddling === 0, `a straddling section must not compensate, got ${r.straddling}`)
    ok(r.zeroDelta === 0, `a zero delta must never compensate, got ${r.zeroDelta}`)
    return r
  })

  await test('calibration: loaded from a generated artifact, not written in the source', page, async () => {
    // Deliberately NOT a parity test against literal values. That was the previous
    // arrangement — constants hand-copied into `main.ts` with a test
    // asserting the copies matched — and it was the wrong shape: two
    // hand-maintained numbers that must agree, failing as a stale comparison rather
    // than a visible error.
    //
    // What is asserted instead is the mechanism: the constants are fetched, not
    // declared. Staleness is then caught by `estimate: measured heights land near
    // the estimate`, which bounds the mean predicted-vs-measured error against real
    // rendered sections — an outcome check on the consequence, which is stronger
    // than checking two literals are equal.
    await load(40)
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      const res = await fetch('/calibration.json')
      const fromFile = await res.json()
      return { fromFile, inUse: app.calibration() }
    })
    ok(
      r.inUse !== null,
      'the calibration never loaded, so every height was estimated from a null model',
    )
    ok(
      JSON.stringify(r.fromFile) === JSON.stringify(r.inUse),
      'the running calibration differs from the generated artifact',
    )
    ok(
      typeof r.inUse.px_per_100_chars === 'number' &&
        typeof r.inUse.px_per_paragraph === 'number' &&
        typeof r.inUse.section_chrome_px === 'number',
      `calibration is not fully populated: ${JSON.stringify(r.inUse)}`,
    )
    return r.inUse
  })

  await test('calibration: no height model in the frontend source', page, async () => {
    // The regression guard that replaces the deleted parity test. It asserts the
    // *absence* of a thing rather than the equality of two things, so it cannot
    // itself become a duplicate to keep in step.
    // One definition, shared with the in-engine runner. Both of these checks were
    // separate whole-file regexes in two hosts, and both matched correct code at least
    // once — the in-engine run reported 23/24 with a failure caused by the check.
    const r = await page.evaluate(async (spec) => {
      const checks = (await import(spec)) as typeof import('../src/core/source-checks.js')
      const src = await (await fetch(checks.ENTRY_MODULE)).text()
      return {
        declared: checks.declaresHeightConstant(src),
        loadsArtifact: src.includes('/calibration.json') || src.includes('getDocumentBoot'),
      }
    }, SOURCE_CHECKS)
    ok(
      r.declared === null,
      `main.ts declares ${r.declared}; it must come from the boot payload or the generated artifact`,
    )
    ok(r.loadsArtifact, 'main.ts does not load the calibration from anywhere')
    return { declared: r.declared, loadsArtifact: r.loadsArtifact }
  })

  await test('calibration: estimating before load throws rather than defaulting', page, async () => {
    // A default would be a model that disagrees with the one every measurement was
    // fitted against, and the disagreement surfaces only as a subtly wrong
    // scrollbar. Throwing turns a boot-ordering bug into an obvious failure.
    const threw = await page.evaluate(async () => {
      const spec = '/src/core/boot.ts'
      const mod: any = await import(/* @vite-ignore */ spec)
      try {
        mod.requireCalibration(null)
        return 'no throw'
      } catch (e: any) {
        return e.message
      }
    })
    ok(threw !== 'no throw', 'requireCalibration(null) returned instead of throwing')
    ok(
      String(threw).includes('boot-ordering'),
      `the error should name the cause, got: ${threw}`,
    )
    return { message: String(threw).slice(0, 60) }
  })

  await test('block count: the character-derived fallback is unreachable', page, async () => {
    // # The defect this closes
    //
    // `metrics.blocks` was optional, and `estimateHeight` filled it in with
    // `Math.max(1, chars / 620)`. That fallback is not a small approximation: it
    // measures at 225% error on short multi-paragraph sections and 41% on long
    // single-paragraph ones, because the same character count can be one paragraph
    // or twenty and no constant density recovers that.
    //
    // It was unreachable in Rust — `block_count` is written by every store path —
    // but reachable in the frontend, where nothing carried the value at all. So the
    // good path was computed, stored and then never used.
    //
    // Three assertions, because "I removed the fallback" is a claim about code and
    // the only way to check it is to try to reach it.
    await load(40)

    // 1. The estimate function contains no fallback.
    //
    //    Scoped to the function body with comments stripped. A whole-file regex
    //    was the first attempt and it flagged
    //    `sections[index]?.metrics.blocks ?? null` — a lookup guard in the test
    //    surface, which is correct and unrelated. An assertion that matches correct
    //    code is worse than no assertion: it trains you to ignore it.
    //
    //    Comments are stripped because the function's own doc comment quotes the
    //    old `blocks ?? Math.max(1, chars / 620)` line in order to explain why it
    //    was removed. Leaving comments in would make the check fail on the very
    //    documentation that records the fix.
    // Through `source-checks.ts`, the same module the in-engine runner uses. The
    // implementation here once reimplemented the body-slicing and comment-stripping,
    // and the in-engine copy never got the fix at all.
    const src = await page.evaluate(async (spec) => {
      const checks = (await import(spec)) as typeof import('../src/core/source-checks.js')
      const text = await (await fetch(checks.ENTRY_MODULE)).text()
      const body = checks.estimateHeightBody(text)
      return {
        found: body !== null,
        derived: body === null ? null : checks.derivesBlockCount(body),
        optionalType: /blocks\?:\s*number/.test(text),
        body: body === null ? '' : body.slice(0, 140),
      }
    }, SOURCE_CHECKS)
    ok(
      src.found,
      `could not locate estimateHeight in main.ts; a renamed function would make this ` +
        'check vacuous, which is worse than not having it',
    )
    ok(
      src.derived === null,
      `estimateHeight still derives a block count (${src.derived}): ${src.body}`,
    )
    ok(!src.optionalType, 'a `blocks?: number` type is back; it must be required')

    // 2. That the type is *required* is a compile-time property, so it is checked
    //    in test/blocks-required.ts with the real TypeScript compiler.
    //
    //    Two earlier attempts at it lived here and both were wrong. A regex over
    //    registry.ts failed to match because the declaration is split across a doc
    //    comment and a type literal; reading the served module failed because
    //    esbuild erases interfaces, so `metrics: {` is not in the output at all.
    //    Neither could ever have worked — a browser runtime cannot observe whether
    //    a type is optional, and a check that can only fail for the wrong reason
    //    is a check that gets deleted.
    //
    //    What this suite can check is the runtime half: a section whose stored
    //    block count is unusable must be refused rather than estimated.

    // 3. And it rejects a zero or absent block count at runtime rather than
    //    estimating one.
    const refused = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      try {
        await app.loadSynthetic(4, 0)
        return 'accepted'
      } catch (e: any) {
        return e.message
      }
    })
    ok(
      refused !== 'accepted',
      `a section with zero blocks was accepted, so the estimate would be based on a derived count again: ${refused}`,
    )
    return { refused: refused.slice(0, 60) }
  })

  await test('block count: it changes the prediction, so a missing value is not harmless', page, async () => {
    // The reason the fallback mattered, measured rather than asserted. 30 short
    // paragraphs and 1 long one at the same character count must predict very
    // different heights; if they did not, the whole argument for `block_count`
    // would be decorative.
    const spread = await page.evaluate(() => {
      const cal = (window as any).HOLO_SCROLL.calibration()
      const height = (chars: number, blocks: number) =>
        cal.section_chrome_px + (chars / 100) * cal.px_per_100_chars + blocks * cal.px_per_paragraph
      const chars = 9300
      return {
        thirty: height(chars, 30),
        one: height(chars, 1),
        derived: height(chars, Math.max(1, chars / 620)),
      }
    })
    ok(
      spread.thirty - spread.one > 800,
      `30 paragraphs and 1 paragraph predict heights only ${(spread.thirty - spread.one).toFixed(0)}px apart`,
    )
    // The derived count matches neither, which is what made it unsafe.
    ok(
      Math.abs(spread.derived - spread.thirty) > 300 && Math.abs(spread.derived - spread.one) > 300,
      `the derived count (${spread.derived.toFixed(0)}) is close to one real value, so a missing block count would be hard to notice`,
    )
    return {
      thirty: spread.thirty.toFixed(0),
      one: spread.one.toFixed(0),
      derived: spread.derived.toFixed(0),
    }
  })

  // -----------------------------------------------------------------------
  // Persistence: nothing is lost when a section is evicted mid-typing

  await test('unmounting a section mid-typing persists what was typed', page, async () => {
    // The required test, and the one the whole persistence loop exists for.
    //
    // The sequence is the one that loses data: type into a section, then have the
    // scroller evict it *without* the throttle window having opened. After `destroy()`
    // the only copy of those bytes is in SQLite, so if the commit is merely *scheduled*
    // the edit is gone.
    //
    // Asserted against the real editor, the real registry and the real `unmount`
    // handler, with only the transport faked — because every other part of the chain is
    // exactly what has to work.
    const result = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      await app.loadSynthetic(40, 8, 400)

      const sent: Array<{ id: string; text: string }> = []
      app.setEditTransport(async (id: string, json: any) => {
        sent.push({ id, text: (json.content ?? [])
          .map((b: any) => (b.content ?? []).map((t: any) => t.text ?? '').join(''))
          .join('') })
        return {
          section_id: id, wal_row_id: sent.length, word_count: 0, char_count: 0,
          block_count: 0, mark_count: 0, pending: 1,
        }
      })

      // The victim: the focused section, so it is live and editable.
      const victimId = app.sectionIds()[3]
      app.registry.focus(victimId)
      await app.settle()

      // Type something a collapse of spaces could not have produced, so the assertion is
      // about *this* text and not about a normalised equivalent.
      const typed = 'zzq-unique-marker-4417'
      app.registry.editorIfMounted(victimId).commands.insertContent(typed)
      await app.settle()

      const pendingBefore = app.isPendingEdit(victimId)
      const liveText = app.registry.editorIfMounted(victimId).state.doc.textContent

      // Now evict it. `registry.unmount` is what the scroller calls, and it is the call
      // that runs `onBeforeUnmount`.
      app.registry.unmount(victimId)
      // The commit is started by the handler and not awaited by it, so give it the same
      // turns the real event loop would.
      await new Promise(r => setTimeout(r, 60))

      return {
        typed,
        liveText,
        pendingBefore,
        sent,
        editorGone: app.registry.editorIfMounted(victimId) === null,
        // What the registry kept after the unmount: its own snapshot, which is what a
        // later re-fetch must not disagree with.
        storedHasTyped: JSON.stringify(app.storedJson(victimId) ?? '').includes(typed),
        pendingAfter: app.isPendingEdit(victimId),
      }
    })

    ok(result.pendingBefore, 'typing should have marked the section as having an unsent edit')
    ok(result.liveText.includes(result.typed), `the editor should hold the typed text, got ${result.liveText.slice(-60)}`)
    ok(result.editorGone, 'the editor should have been destroyed')
    ok(
      result.sent.length === 1,
      `an eviction of a dirty section must send exactly one commit, got ${result.sent.length}`,
    )
    const carried = result.sent[0]
    ok(
      carried !== undefined && carried.text.includes(result.typed),
      `the commit carried different text than what was typed: ...${carried?.text.slice(-60)}`,
    )
    ok(!result.pendingAfter, 'the section should not still be pending after being flushed')
    ok(
      result.storedHasTyped,
      "the registry's own snapshot should also hold the typed text, or a later re-fetch " +
        'would revert the section',
    )
    return { sent: result.sent.length, chars: carried?.text.length ?? 0 }
  })

  await test('evicting a clean section costs no commit and no fold', page, async () => {
    // A scroll through a document nobody edited passes hundreds of sections. Folding the
    // WAL for each would put a write on the scroll path for nothing.
    const result = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      await app.loadSynthetic(20, 6, 300)
      let sends = 0
      app.setEditTransport(async (id: string) => {
        sends++
        return {
          section_id: id, wal_row_id: sends, word_count: 0, char_count: 0,
          block_count: 0, mark_count: 0, pending: 0,
        }
      })

      const id = app.sectionIds()[2]
      app.registry.mount(id)
      await app.settle()
      const pending = app.isPendingEdit(id)

      app.registry.unmount(id)
      await new Promise(r => setTimeout(r, 40))
      return { sends, pending, flushed: await app.flushSectionNow(id) }
    })
    ok(!result.pending, 'precondition: an untouched section must not be marked dirty')
    ok(result.sends === 0, `a clean eviction must send nothing, got ${result.sends}`)
    ok(!result.flushed, 'flushing a clean section should report nothing to send')
  })

  await test('an eviction sends the state at eviction time, not at keystroke time', page, async () => {
    // The payload is read through a closure so that what goes out is the state the user
    // last had. Capturing it when the keystroke happened would persist a state the user
    // has already typed past — data loss with extra steps, and the kind that only shows
    // up when someone types fast and scrolls immediately.
    const result = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      await app.loadSynthetic(20, 6, 300)
      const sent: string[] = []
      app.setEditTransport(async (_id: string, json: any) => {
        sent.push(
          (json.content ?? []).map((b: any) => (b.content ?? []).map((t: any) => t.text ?? '').join('')).join(''),
        )
        return {
          section_id: 'x', wal_row_id: 1, word_count: 0, char_count: 0,
          block_count: 0, mark_count: 0, pending: 0,
        }
      })

      const id = app.sectionIds()[5]
      app.registry.focus(id)
      await app.settle()
      const editor = app.registry.editorIfMounted(id)

      editor.commands.insertContent('FIRST')
      await app.settle()
      editor.commands.insertContent('SECOND')
      await app.settle()

      app.registry.unmount(id)
      await new Promise(r => setTimeout(r, 60))
      return { sent, hasFirst: sent[0]?.includes('FIRST'), hasSecond: sent[0]?.includes('SECOND') }
    })
    ok(result.sent.length === 1, `expected one commit, got ${result.sent.length}`)
    ok(result.hasFirst === true, 'the earlier edit should have been included')
    ok(
      result.hasSecond,
      'the later edit should have been included; the payload was captured too early, so ' +
        'the last thing typed would be lost on an immediate eviction',
    )
  })

  await test('the section cache is bounded and evicts by recency', page, async () => {
    // The bound is the property, and it is asserted on the real cache the app uses rather
    // than a fresh one, so a wiring mistake between them shows up here.
    const result = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      await app.loadSynthetic(60, 4, 200)
      app.setCacheCapacity(30)
      const cache = app.sectionCache()
      for (let i = 0; i < 60; i++) cache.set(`probe-${i}`, { type: 'doc', content: [] })
      return cache.inspect()
    })
    ok(result.capacity === 30, `the cap should be 30, got ${result.capacity}`)
    ok(result.size === 30, `60 sections should have left 30 resident, got ${result.size}`)
    ok(result.evictions === 30, `expected 30 evictions, got ${result.evictions}`)
    ok(result.nextToEvict !== null, 'something should be next to evict')
  })

  // -----------------------------------------------------------------------
  // Seams: a split must leave a boundary Backspace can cross back

  await test('a section split before a table can be rejoined with Backspace', page, async () => {
    // The loop that closes the seam rule.
    //
    // `chooseCutIndex` refuses a seam whose tail opens on an atom, because
    // `mergeBackward` -- the Backspace-at-position-0 gesture that undoes a split -- refuses
    // to move anything that is not a textblock. So a seam that put a table first in the
    // tail would create a section the user can type across and never undo.
    //
    // Node-tested in `test/lifecycle.ts`. This one uses a real editor, because the rule is
    // only meaningful if the registry's merge and the splitter's cut agree, and only the
    // registry has a ProseMirror instance.
    const result = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      // Eleven prose paragraphs and then a table: the true midpoint is block 5, and the
      // irreversible seam at 11 is the one a naive "even by size" search would not pick
      // anyway -- so this document has to *work*, not merely avoid the bad seam.
      const table = {
        type: 'table',
        content: [
          {
            type: 'tableRow',
            content: [
              {
                type: 'tableCell',
                content: [{ type: 'paragraph', content: [{ type: 'text', text: 'cell' }] }],
              },
            ],
          },
        ],
      }
      // Eleven paragraphs and then a table. The midpoint is block 6, so a correct splitter
      // cuts there and the tail opens on a paragraph -- which is the point: the irreversible
      // seam at block 11 exists in this document and must simply not be chosen.
      const paragraphs = Array.from({ length: 11 }, (_, i) => ({
        type: 'paragraph',
        content: [{ type: 'text', text: `body ${i} ` + 'lorem ipsum dolor '.repeat(30) }],
      }))
      const json = { type: 'doc', content: [...paragraphs, table] }
      await app.loadRecords([
        {
          id: 'seam-a',
          json,
          metrics: app.metricsOf(json),
          loaded: true,
          dirty: false,
        },
      ])
      const before = app.sectionCount()
      const cut = app.chooseCut(json)
      // The two halves are built exactly as `applySplit` builds them -- `nodes.slice` at the
      // chosen cut -- rather than by calling it.
      //
      // `applySplit` persists through `commit_section_lifecycle`, which needs a bridge, and
      // this suite runs in a browser. Splitting by hand keeps the assertion on the thing
      // that matters, which is whether the seam the splitter *chose* is one the registry's
      // merge can cross. Routing the cut through `applySplit` in the in-engine run instead
      // would test the bridge, not the seam.
      const nodes = (json as any).content
      const head = { type: 'doc', content: nodes.slice(0, cut) }
      const tail = { type: 'doc', content: nodes.slice(cut) }
      await app.loadRecords([
        { id: 'seam-a', json: head, metrics: app.metricsOf(head), loaded: true, dirty: false },
        { id: 'seam-b', json: tail, metrics: app.metricsOf(tail), loaded: true, dirty: false },
      ])
      await app.settle()

      const afterSplit = app.sectionCount()
      const tailId = 'seam-b'
      const tailJson = app.storedJson(tailId)
      const tailFirst = tailJson?.content?.[0]?.type ?? null

      // Now cross the seam back with a Backspace at position 0 of the tail.
      const merged = app.registry.mergeBackward('seam-b', 'seam-a')
      await app.settle()

      return {
        before,
        afterSplit,
        cut,
        tailFirst,
        merged,
        // The registry's merge moves content and reports; `applyMerge` is what drops the
        // section, and it needs the bridge. So the assertion is on the content, which is
        // where the seam actually is.
        headBlocks: app.registry.editorIfMounted('seam-a')?.state.doc.childCount ?? -1,
        tailLeft: app.registry.editorIfMounted('seam-b')?.state.doc.childCount ?? -1,
        headTypes: (app.registry.editorIfMounted('seam-a')?.getJSON()?.content ?? []).map(
          (b: any) => b.type,
        ),
        tailTypes: (app.registry.editorIfMounted('seam-b')?.getJSON()?.content ?? []).map(
          (b: any) => b.type,
        ),
      }
    })

    ok(result.before === 1, `precondition: one section, got ${result.before}`)
    ok(result.afterSplit === 2, `the seam should produce two sections, got ${result.afterSplit}`)
    ok(
      result.tailFirst !== 'table' && result.tailFirst !== null,
      `the new section opens on ${result.tailFirst}; that seam cannot be crossed back`,
    )
    ok(result.merged, 'Backspace at the seam must rejoin the two sections')

    // The cut is the *balanced* one, not the one before the table -- and that is correct:
    // the table carries almost no weight, so a midpoint at block 6 is a far better split
    // than one at 11. The invariant is not "cut before the table", it is "the tail does not
    // open on an atom", which is asserted above and is what makes the merge possible.
    ok(
      result.headBlocks === result.tailLeft + 1,
      `the merge should move exactly one block across: head ${result.headBlocks}, tail ${result.tailLeft}`,
    )
    // Nothing lost and nothing duplicated: the same twelve blocks are still there, split a
    // different way.
    ok(
      result.headTypes.length + result.tailTypes.length === 13,
      `expected 13 blocks across the two sections, got ` +
        `${result.headTypes.length} + ${result.tailTypes.length}`,
    )
    const tableInHead = result.headTypes.filter((t: string) => t === 'table').length
    const tableInTail = result.tailTypes.filter((t: string) => t === 'table').length
    ok(
      tableInHead + tableInTail === 1,
      `the table must exist exactly once: ${tableInHead} in the head, ${tableInTail} in the tail`,
    )
    return {
      cut: result.cut,
      headBlocks: result.headBlocks,
      tailLeft: result.tailLeft,
      tableInTail,
    }
  })

  await test('a seam in front of a table is refused when there is no other', page, async () => {
    // Two blocks: prose then a table. The only interior boundary is irreversible, so the
    // splitter must decline rather than create a one-way seam. Asserted here as well as in
    // Node because the refusal is what protects the merge, and the merge lives here.
    const result = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      const table = {
        type: 'table',
        content: [
          {
            type: 'tableRow',
            content: [
              {
                type: 'tableCell',
                content: [{ type: 'paragraph', content: [{ type: 'text', text: 'cell' }] }],
              },
            ],
          },
        ],
      }
      const json = {
        type: 'doc',
        content: [
          { type: 'paragraph', content: [{ type: 'text', text: 'a very long lead paragraph '.repeat(40) }] },
          table,
        ],
      }
      await app.loadRecords([
        { id: 'seam-c', json, metrics: app.metricsOf(json), loaded: true, dirty: false },
      ])
      await app.settle()
      return { chosen: app.chooseCut(json), sections: app.sectionCount() }
    })
    ok(result.chosen === null, `no reversible seam exists, so the cut should be null, got ${result.chosen}`)
    ok(result.sections === 1, `the section should be left whole, got ${result.sections}`)
  })

  await test('the atom list covers every atom the installed editor declares', page, async () => {
    // The real extension set, not the test fixture's schema. A block type the app installs
    // but `ATOMIC_BLOCK_TYPES` omits would be accepted by `chooseCutIndex` and then refused
    // by `mergeBackward` -- a seam chosen that cannot be crossed.
    const result = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      return app.atomicTypes()
    })
    const missing = result.schemaAtoms.filter((t: string) => !result.listed.includes(t))
    ok(
      missing.length === 0,
      `the installed schema declares atoms the list omits: ${missing.join(', ')}`,
    )
    ok(result.listed.includes('table'), 'the table extension is installed, so table must be listed')
    ok(
      result.listed.includes('codeBlock'),
      'StarterKit installs a code block, so codeBlock must be listed',
    )
    return { schemaAtoms: result.schemaAtoms }
  })

  // -----------------------------------------------------------------------
  // Equations: the half that needs a layout engine

  await test('a NodeView owns its subtree and re-renders without being recreated', page, async () => {
    // Why a NodeView and not `renderHTML`: KaTeX has to run *after* its spans exist in the
    // document, and a `renderHTML` result is re-parsed, so anything KaTeX produced there
    // would be flattened into ordinary nodes on the next parse and the equation node would
    // stop existing. The NodeView keeps its own DOM.
    //
    // The identity check is the point. `update` returning false makes ProseMirror destroy
    // the NodeView and build another, which throws the element away and rebuilds the subtree
    // -- a flicker on every keystroke inside an equation. So the *same* element has to
    // survive an edit, with new markup inside it.
    const result = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      const json = {
        type: 'doc',
        content: [
          {
            type: 'paragraph',
            content: [
              { type: 'text', text: 'before ' },
              { type: 'inlineMath', attrs: { latex: 'x^2' } },
              { type: 'text', text: ' after' },
            ],
          },
        ],
      }
      await app.loadRecords([
        { id: 'eq-1', json, metrics: app.metricsOf(json), loaded: true, dirty: false },
      ])
      await app.settle()
      app.registry.focus('eq-1')
      await app.settle()

      const editor = app.registry.editorIfMounted('eq-1')
      const dom = editor.view.dom.querySelector('.holo-math--inline') as HTMLElement | null
      if (!dom) return { found: false, before: null }

      const before = {
        hasKatex: !!dom.querySelector('.katex'),
        text: dom.textContent,
        latex: dom.getAttribute('data-latex'),
        // KaTeX hides the MathML from assistive tech and leaves the HTML visible, so a
        // node with the MathML hidden is rendered correctly.
        ariaHidden: !!dom.querySelector('[aria-hidden="true"]'),
      }

      // Change the TeX. The node must survive; only its contents change.
      // Change the TeX the way the UI would: a transaction that sets the node's attributes.
      //
      // The first version used `insertContent` at a NodeSelection, which *replaces* the
      // node — so it was measuring node replacement rather than an attribute change, and it
      // correctly failed. `setNodeMarkup` is the transaction ProseMirror issues when a
      // NodeView's attributes change, which is the path `update` exists for.
      //
      // And the position is found rather than assumed: position 1 inside the paragraph is
      // the leading *text* node, and `setNodeMarkup` there addresses the wrong thing.
      let mathPos = -1
      editor.state.doc.descendants((node: any, p: number) => {
        if (node.type.name === 'inlineMath' && mathPos < 0) mathPos = p
        return true
      })
      app.setNodeAttrs('eq-1', mathPos, { latex: '\\frac{1}{2}' })
      await app.settle()

      const domAfter = editor.view.dom.querySelector('.holo-math--inline') as HTMLElement | null
      return {
        found: true,
        before,
        mathPos,
        sameElement: domAfter === dom,
        afterLatex: domAfter?.getAttribute('data-latex') ?? null,
        afterHasFraction: !!domAfter?.querySelector('.mfrac, .frac-line'),
        latexInJson: JSON.stringify(editor.getJSON()).includes('frac{1}{2}'),
        // An atom has no content hole, so ProseMirror must not have put a paragraph inside.
        contentEditable: domAfter?.getAttribute('contenteditable'),
      }
    })

    ok(result.found, 'no .holo-math--inline NodeView was rendered')
    const before = result.before!
    ok(before.hasKatex, 'the NodeView did not run KaTeX: no .katex in its subtree')
    ok(before.ariaHidden, "KaTeX's MathML should be aria-hidden with the HTML visible")
    ok(before.latex === 'x^2', `data-latex was ${before.latex}`)
    ok(
      result.sameElement,
      'the NodeView was destroyed and recreated on an edit, so the element identity changed; ' +
        'that is the flicker the update path exists to avoid',
    )
    // A single backslash: the TeX source, not a JS escape that survived into the document.
    ok(result.afterLatex === '\\frac{1}{2}', `data-latex should be the new TeX, got ${result.afterLatex}`)
    ok(result.afterHasFraction, 'the new TeX was not re-rendered')
    ok(result.latexInJson, 'the TeX must be in the document, not only in the DOM')
    ok(
      result.contentEditable !== 'true',
      `an atom has no content hole; the NodeView claimed contenteditable=${result.contentEditable}`,
    )
    return { before: before.latex, after: result.afterLatex }
  })

  await test('a display equation is a block and an inline one is not', page, async () => {
    const result = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      const json = {
        type: 'doc',
        content: [
          { type: 'paragraph', content: [{ type: 'text', text: 'lead' }] },
          { type: 'mathBlock', attrs: { latex: '\\sum_{i=1}^{n} i' } },
          { type: 'paragraph', content: [{ type: 'text', text: 'trail' }] },
        ],
      }
      await app.loadRecords([
        { id: 'eq-2', json, metrics: app.metricsOf(json), loaded: true, dirty: false },
      ])
      await app.settle()
      const dom = app.registry.editorIfMounted('eq-2')?.view.dom
      const block = dom?.querySelector('.holo-math--block') as HTMLElement | null
      return {
        found: !!block,
        displayClass: !!block?.querySelector('.katex-display'),
        tag: block?.tagName ?? null,
        parentIsDoc: block?.parentElement?.classList.contains('ProseMirror') ?? false,
        inlineCount: dom?.querySelectorAll('.holo-math--inline').length ?? -1,
      }
    })
    ok(result.found, 'no .holo-math--block NodeView was rendered')
    ok(result.displayClass, 'a block equation should render in KaTeX display mode')
    ok(result.tag === 'DIV', `a block equation should be a div, got ${result.tag}`)
    ok(result.parentIsDoc, 'a block equation belongs to the doc, not to a paragraph')
    ok(result.inlineCount === 0, 'a mathBlock must not also render as an inline equation')
  })

  await test("an equation's serialised HTML carries the TeX and not KaTeX's markup", page, async () => {
    // The half `test/math.ts` cannot run: `DOMSerializer` needs a DOM.
    //
    // Both properties matter. `data-latex` is what carries the equation through a
    // round trip. And KaTeX's markup must be *absent*, because a serialised result is parsed
    // back -- spans in there would become ordinary nodes and the equation node would stop
    // existing, which reads as the equation silently turning into a pile of italic letters.
    const result = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      const doc = {
        type: 'doc',
        content: [
          {
            type: 'paragraph',
            content: [{ type: 'inlineMath', attrs: { latex: 'a+b' } }],
          },
          { type: 'mathBlock', attrs: { latex: 'c+d' } },
        ],
      }
      await app.loadRecords([
        { id: 'eq-3', json: doc, metrics: app.metricsOf(doc), loaded: true, dirty: false },
      ])
      await app.settle()
      const editor = app.registry.editorIfMounted('eq-3')!
      const state = editor.state.doc
      const html = app.serializeHtml('eq-3')
      // And it must round trip: the same two nodes with the same TeX.
      const parsed = editor.schema.nodeFromJSON(JSON.parse(JSON.stringify(state.toJSON())))
      const json = JSON.stringify(parsed.toJSON())
      return {
        html,
        roundTripInline: json.includes('a+b'),
        roundTripBlock: json.includes('c+d'),
      }
    })
    ok(result.html.includes('data-latex="a+b"'), `the inline TeX is missing: ${result.html}`)
    ok(result.html.includes('data-latex="c+d"'), `the block TeX is missing: ${result.html}`)
    ok(result.html.includes('data-math="inline"'), 'the inline node should be marked')
    ok(result.html.includes('data-math="block"'), 'the block node should be marked')
    ok(
      !result.html.includes('katex'),
      `KaTeX markup reached the serialised form and would be re-parsed as spans: ${result.html.slice(0, 200)}`,
    )
    ok(result.roundTripInline && result.roundTripBlock, 'the TeX did not survive a JSON round trip')
  })

  await test('a shared figure is revoked only by the last section holding it', page, async () => {
    // The memory claim, closed.
    //
    // A `Blob` holds the whole decoded image in the renderer's heap. A document with two
    // hundred figures, scrolled through, would hold two hundred of them for the life of the
    // session -- in a project whose entire argument is that memory does not track document
    // length. So the *last* holder must revoke, and no earlier one may.
    //
    // Three sections share one figure -- the logo in a header and in two figures. Evicting the
    // first two must not revoke it; evicting the third must. A presence flag rather than a
    // count would pass the first two assertions and fail the third, and a release-all would
    // fail the first: it would revoke while two sections were still showing the figure.
    const result = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      app.setAssetSource(async (hash: string) => ({ mime: 'image/png', bytes: Uint8Array.from([hash.length]) }))

      const shared = 'a'.repeat(64)
      const json = {
        type: 'doc',
        content: [
          { type: 'paragraph', content: [{ type: 'text', text: 'text' }] },
          { type: 'image', attrs: { src: `holo-asset://${shared}`, alt: 'logo' } },
        ],
      }
      await app.loadRecords([
        { id: 'sh-1', json, metrics: app.metricsOf(json), loaded: true, dirty: false },
        { id: 'sh-2', json, metrics: app.metricsOf(json), loaded: true, dirty: false },
        { id: 'sh-3', json, metrics: app.metricsOf(json), loaded: true, dirty: false },
      ])
      await app.settle()

      const read = () => ({
        refs: app.assetRefs(shared)?.refs ?? 0,
        revoked: app.revokedAssets().length,
        assets: app.assetCount().assets,
        // How many `<img>` elements the resolver actually rendered. Proves the refcount
        // corresponds to something on screen rather than to bookkeeping.
        rendered: document.querySelectorAll('#canvas img.holo-image').length,
      })

      const before = read()
      const evicted: Array<ReturnType<typeof read>> = []
      for (const id of ['sh-1', 'sh-2', 'sh-3']) {
        app.registry.unmount(id)
        await app.settle()
        evicted.push(read())
      }
      return { before, evicted }
    })

    ok(
      result.before.refs === 3,
      `three mounted sections should hold three references, got ${result.before.refs}`,
    )
    ok(result.before.rendered === 3, `three figures should be rendered, got ${result.before.rendered}`)

    // The two early evictions: the count falls, nothing is revoked.
    ok(result.evicted[0]!.refs === 2, `expected 2 refs after the first eviction, got ${result.evicted[0]!.refs}`)
    ok(result.evicted[1]!.refs === 1, `expected 1 ref after the second, got ${result.evicted[1]!.refs}`)
    ok(
      result.evicted[0]!.revoked === 0 && result.evicted[1]!.revoked === 0,
      `nothing may be revoked while two sections still show the figure; saw ` +
        `${result.evicted[0]!.revoked} and ${result.evicted[1]!.revoked}`,
    )
    ok(result.evicted[1]!.assets === 1, `the entry should still be held, got ${result.evicted[1]!.assets}`)

    // The last one.
    ok(result.evicted[2]!.refs === 0, `expected 0 refs, got ${result.evicted[2]!.refs}`)
    ok(
      result.evicted[2]!.revoked === 1,
      `the last eviction must revoke exactly once, got ${result.evicted[2]!.revoked}`,
    )
    ok(result.evicted[2]!.assets === 0, `nothing should be held, got ${result.evicted[2]!.assets}`)
    return { before: result.before.refs, revocations: result.evicted[2]!.revoked }
  })

  await test('an asset in a section that is never edited is still released', page, async () => {
    // The case a "release only dirty sections" shortcut would fail. A scroll through a
    // document nobody edited passes hundreds of sections, and if the release were gated on
    // `dirty` — which is the obvious optimisation, since dirty sections are the ones worth
    // the trouble — every figure in the document would leak.
    const result = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      const hash = 'c'.repeat(64)
      const json = {
        type: 'doc',
        content: [
          { type: 'paragraph', content: [{ type: 'text', text: 'text' }] },
          { type: 'image', attrs: { src: `holo-asset://${hash}`, alt: '' } },
        ],
      }
      app.setAssetSource(async (hash: string) => ({ mime: 'image/png', bytes: Uint8Array.from([hash.length]) }))
      await app.loadRecords([
        { id: 'as-3', json, metrics: app.metricsOf(json), loaded: true, dirty: false },
      ])
      await app.settle()
      await app.settle()

      const held = app.assetRefs(hash)
      const dirty = app.isPendingEdit('as-3')
      app.registry.unmount('as-3')
      await app.settle()
      return { held, dirty, after: app.assetRefs(hash) }
    })
    ok(!result.dirty, 'precondition: the section was never edited')
    ok(result.held?.refs === 1, `the figure should be held once, got ${result.held?.refs}`)
    ok(result.after === null, `a clean section's figure must still be released, got ${JSON.stringify(result.after)}`)
  })

  await test('bridge: the boot payload decodes from every response shape Tauri sends', page, async () => {
    // # The risk this covers
    //
    // `get_document_boot` returns `tauri::ipc::Response::new(bytes)` so the payload
    // crosses with no serialisation at all. What lands in the renderer is then a
    // question about the engine and the payload size: `ArrayBuffer` from webkit2gtk
    // and WKWebView, `Uint8Array` from WebView2, and a plain `number[]` if anything
    // on the path ever re-encodes as JSON.
    //
    // Two of those three engines have never been run on this machine. A decoder that
    // handles only the shape Chromium hands over would pass every test here and then
    // fail at boot on macOS or Windows — which is the worst place to find out, and
    // the worst place for a user to find out.
    //
    // So all three are asserted here, by installing a fake `invoke` that returns each
    // in turn. The fake is the point: it makes the engine difference a loop variable
    // instead of a machine.
    const shapes = await page.evaluate(async (spec) => {
      const mod = (await import(spec)) as typeof import('../src/core/geometry-bridge.js')
      const payload = {
        document_id: 'D',
        title: 'T',
        calibration: { px_per_100_chars: 24.83, px_per_paragraph: 34.89, section_chrome_px: 54.8 },
        sections: [{ id: 's0', order_key: 1024, title: null, word_count: 1500, mark_count: 0,
                     char_count: 9000, block_count: 7, created_at: 0, updated_at: 0 }],
        visible: [{ id: 's0', content_zstd: new Uint8Array([1, 2, 3]) }],
        focused_section_id: null,
        scroll_top: null,
      }
      const bytes = mod.encodeBootPayloadForTest(payload)
      const cases: Record<string, unknown> = {
        arrayBuffer: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
        uint8Array: bytes,
        numberArray: Array.from(bytes),
      }
      const out: Record<string, string> = {}
      for (const [name, raw] of Object.entries(cases)) {
        ;(window as any).__TAURI__ = { core: { invoke: async () => raw } }
        try {
          const got = await mod.getDocumentBoot()
          // Non-null assertions rather than optional chaining: a decode that produced
          // no sections would otherwise be recorded as 'wrong data' by the check below
          // only by accident, and a `?.` here would silently turn a broken decode into
          // a data mismatch.
          out[name] =
            got.sections[0]!.block_count === 7 && got.document_id === 'D' ? 'ok' : 'wrong data'
        } catch (e: any) {
          out[name] = `threw: ${e.message.slice(0, 60)}`
        }
      }
      delete (window as any).__TAURI__
      return out
    }, BRIDGE_MODULE)
    for (const [shape, result] of Object.entries(shapes)) {
      ok(result === 'ok', `an ${shape} response did not decode: ${result}`)
    }
    return shapes
  })

  await test('bridge: a payload that is not bytes fails with a message naming the type', page, async () => {
    // A raw `typeof` in the error is what makes a boot failure diagnosable from a
    // log. Without it, "the MessagePack decoder failed" on a platform that has never
    // been tested is indistinguishable from a corrupt document.
    const message = await page.evaluate(async (spec) => {
      const mod = (await import(spec)) as typeof import('../src/core/geometry-bridge.js')
      ;(window as any).__TAURI__ = { core: { invoke: async () => ({ not: 'bytes' }) } }
      try {
        await mod.getDocumentBoot()
        return 'accepted an object'
      } catch (e: any) {
        return e.message
      } finally {
        delete (window as any).__TAURI__
      }
    }, BRIDGE_MODULE)
    ok(!message.includes('accepted'), `a plain object was accepted as a payload: ${message}`)
    ok(
      /object/.test(message) && /not bytes/.test(message),
      `the error should name what arrived and what was needed, got: ${message}`,
    )
    return { message: message.slice(0, 70) }
  })

  await test('bridge: a browser with no Tauri says so rather than hanging', page, async () => {
    // The harness has no bridge and builds its own document. Reaching for the command
    // anyway should produce an error that says why, not an undefined-property
    // exception from three frames down.
    const message = await page.evaluate(async (spec) => {
      const mod = (await import(spec)) as typeof import('../src/core/geometry-bridge.js')
      try {
        await mod.getDocumentBoot()
        return 'no error'
      } catch (e: any) {
        return e.message
      }
    }, BRIDGE_MODULE)
    ok(message !== 'no error', 'getDocumentBoot resolved in a browser with no Tauri host')
    ok(
      /no Tauri host/.test(message),
      `the error should say there is no host, got: ${message}`,
    )
    return { message: message.slice(0, 70) }
  })

  await test('guard: every measurement is funnelled through one compensation path', page, async () => {
    // Two paths applying compensation independently is how a virtual scroller
    // stutters: every section that mounts shifts the viewport twice.
    const r = await page.evaluate(async () => {
      const res = await fetch('/src/main.ts')
      const src = await res.text()

      // Every `scrollTop +=` in the app file must be inside the scroller's
      // compensation path or the ResizeObserver batch. A third would mean a
      // height correction can move the viewport from somewhere else, which is
      // how the same change gets compensated twice and the view stutters.
      //
      // The scroller's own mutation is asserted separately below, in
      // `scroller.ts`, since that is where the funnel lives.
      const mutations = (src.match(/scrollTop\s*\+=/g) ?? []).length
      const usesFunnel = src.includes('compensationPending')

      const scrollerSrc = await (await fetch('/src/core/scroller.ts')).text()
      const scrollerMutations = (scrollerSrc.match(/scrollTop\s*\+=/g) ?? []).length
      const hasFunnel = scrollerSrc.includes('applyCompensation')
      return { mutations, usesFunnel, scrollerMutations, hasFunnel }
    })
    ok(r.hasFunnel, 'scroller.ts has no single compensation funnel')
    ok(
      r.scrollerMutations === 1,
      `${r.scrollerMutations} places mutate scrollTop in scroller.ts; expected exactly one funnel`,
    )
    ok(r.usesFunnel, 'main.ts does not batch its observer compensation')
    ok(
      r.mutations === 1,
      `${r.mutations} places mutate scrollTop in main.ts; expected only the observer batch`,
    )
    return r
  })

  // ======================================================================
  // Directive 2: an empty section between two populated ones must be prunable
  // ======================================================================

  await test('an empty section is pruned rather than merged into its predecessor', page, async () => {
    // The case the requirement describes: three sections where the middle one holds
    // nothing. Backspace at position 0 of the middle section should remove it, not merge
    // an empty paragraph into the section above and leave the empty section in place.
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      const para = (t: string) => ({ type: 'paragraph', content: [{ type: 'text', text: t }] })

      await app.loadRecords([
        { id: 'e-a', json: { type: 'doc', content: [para('first section body')] }, metrics: app.metricsOf({ type: 'doc', content: [para('first section body')] }), loaded: true, dirty: false },
        // The empty section: one paragraph, no text.
        { id: 'e-b', json: { type: 'doc', content: [para('')] }, metrics: app.metricsOf({ type: 'doc', content: [para('')] }), loaded: true, dirty: false },
        { id: 'e-c', json: { type: 'doc', content: [para('third section body')] }, metrics: app.metricsOf({ type: 'doc', content: [para('third section body')] }), loaded: true, dirty: false },
      ])
      await app.settle()

      const before = {
        count: app.sectionCount(),
        ids: app.registry.ids(),
        totalHeight: app.geometry().totalHeight(),
      }

      const pruned = app.registry.pruneSection('e-b', 'e-a')
      await app.settle()

      return {
        pruned,
        before,
        after: {
          count: app.sectionCount(),
          ids: app.registry.ids(),
          totalHeight: app.geometry().totalHeight(),
          focused: app.registry.focused,
        },
        aBody: app.storedJson('e-a')?.content?.[0]?.content?.[0]?.text ?? null,
        cBody: app.storedJson('e-c')?.content?.[0]?.content?.[0]?.text ?? null,
      }
    })

    ok(r.pruned, 'the empty section should have been pruned')
    ok(r.before.count === 3, `the fixture should hold 3 sections, got ${r.before.count}`)
    ok(r.after.count === 2, `pruning should leave 2 sections, got ${r.after.count}`)
    ok(!r.after.ids.includes('e-b'), `e-b should be gone, ids are ${r.after.ids.join(',')}`)
    ok(
      r.after.ids.join() === 'e-a,e-c',
      `the two populated sections must survive in order, got ${r.after.ids.join()}`,
    )
    // The neighbours are untouched. A prune that appended the empty paragraph into `e-a`
    // would satisfy every count assertion above while silently changing a section's body.
    ok(r.aBody === 'first section body', `e-a's body must be untouched, got ${r.aBody}`)
    ok(r.cBody === 'third section body', `e-c's body must be untouched, got ${r.cBody}`)
    return { before: r.before.count, after: r.after.count, focused: r.after.focused }
  })

  await test('a non-empty section is NOT pruned, however short it is', page, async () => {
    // The guard that stops the fix from becoming a data-loss bug: "empty" cannot be
    // "short". A section holding one word must survive.
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      const para = (t: string) => ({ type: 'paragraph', content: [{ type: 'text', text: t }] })
      const one = { type: 'doc', content: [para('x')] }

      await app.loadRecords([
        { id: 'n-a', json: { type: 'doc', content: [para('previous')] }, metrics: app.metricsOf({ type: 'doc', content: [para('previous')] }), loaded: true, dirty: false },
        { id: 'n-b', json: one, metrics: app.metricsOf(one), loaded: true, dirty: false },
      ])
      await app.settle()

      const pruned = app.registry.pruneSection('n-b', 'n-a')
      await app.settle()
      return {
        pruned,
        count: app.sectionCount(),
        body: app.storedJson('n-b')?.content?.[0]?.content?.[0]?.text ?? null,
      }
    })

    ok(!r.pruned, 'a section holding one word must not be prunable')
    ok(r.count === 2, `the section must survive, count is ${r.count}`)
    ok(r.body === 'x', `its content must be intact, got ${r.body}`)
    return { pruned: r.pruned, count: r.count }
  })

  await test('a section holding only an image is not prunable', page, async () => {
    // `isSectionEmpty` counts characters, and a figure has none. Pruning it would delete a
    // user's image — the one failure the sweep and this gesture share in shape.
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      const para = (t: string) => ({ type: 'paragraph', content: [{ type: 'text', text: t }] })
      const withImage = {
        type: 'doc',
        content: [{ type: 'image', attrs: { src: 'holo-asset://' + 'a'.repeat(64), alt: 'figure' } }],
      }

      await app.loadRecords([
        { id: 'i-a', json: { type: 'doc', content: [para('previous')] }, metrics: app.metricsOf({ type: 'doc', content: [para('previous')] }), loaded: true, dirty: false },
        { id: 'i-b', json: withImage, metrics: app.metricsOf(withImage), loaded: true, dirty: false },
      ])
      await app.settle()

      const pruned = app.registry.pruneSection('i-b', 'i-a')
      await app.settle()
      return { pruned, count: app.sectionCount() }
    })

    ok(!r.pruned, 'a section whose only content is an image must not be prunable')
    ok(r.count === 2, `the image section must survive, count is ${r.count}`)
    return { pruned: r.pruned }
  })

  await test('pruning never removes the last section, or a section with unsaved changes', page, async () => {
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      const para = (t: string) => ({ type: 'paragraph', content: [{ type: 'text', text: t }] })
      const empty = { type: 'doc', content: [para('')] }

      // One section, empty. There is no seam to be at, so pruning it would empty the
      // document.
      await app.loadRecords([
        { id: 'only', json: empty, metrics: app.metricsOf(empty), loaded: true, dirty: false },
      ])
      await app.settle()
      const single = app.registry.pruneSection('only', 'only')
      const singleCount = app.sectionCount()

      // A section that is not immediately after the named predecessor.
      await app.loadRecords([
        { id: 'p-a', json: empty, metrics: app.metricsOf(empty), loaded: true, dirty: false },
        { id: 'p-b', json: { type: 'doc', content: [para('middle')] }, metrics: app.metricsOf({ type: 'doc', content: [para('middle')] }), loaded: true, dirty: false },
        { id: 'p-c', json: empty, metrics: app.metricsOf(empty), loaded: true, dirty: false },
      ])
      await app.settle()
      // p-c is not directly after p-a; p-b is in between.
      const nonAdjacent = app.registry.pruneSection('p-c', 'p-a')

      // A dirty section: its bytes are still only in the editor.
      await app.loadRecords([
        { id: 'd-a', json: { type: 'doc', content: [para('previous')] }, metrics: app.metricsOf({ type: 'doc', content: [para('previous')] }), loaded: true, dirty: false },
        { id: 'd-b', json: empty, metrics: app.metricsOf(empty), loaded: true, dirty: true },
      ])
      await app.settle()
      const dirty = app.registry.pruneSection('d-b', 'd-a')

      return { single, singleCount, nonAdjacent, dirty, dirtyCount: app.sectionCount() }
    })

    ok(!r.single, 'the only section in a document must not be prunable')
    ok(r.singleCount === 1, 'the document must still hold a section')
    ok(!r.nonAdjacent, 'a section that is not adjacent to the named predecessor must be refused')
    ok(!r.dirty, 'a section with unsaved changes must not be prunable')
    ok(r.dirtyCount === 2, 'the dirty section must survive')
    return r
  })

  await test('pruning re-keys the geometry so no section is left at zero height', page, async () => {
    // The geometry consequence. Removing a section shortens the document, and every offset
    // below the removed section must move up. If heights were re-keyed by index instead
    // of identity, the last section would inherit a stale entry or collapse to zero —
    // the same bug `core/rekey.ts` was written for.
    const r = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      const trace: any[] = []
      const snap = (label: string) =>
        trace.push({
          label,
          ids: app.registry.ids(),
          geomIds: app.geometryIds(),
          heights: app.geometry().snapshotHeights().map((h: number) => Math.round(h ?? 0)),
        })
      snap('start')
      const para = (t: string) => ({ type: 'paragraph', content: [{ type: 'text', text: t }] })
      const doc = (t: string) => ({
        type: 'doc',
        content: [para(`${t} ` + 'lorem ipsum dolor sit amet '.repeat(20))],
      })
      // The four sections' content is kept so the document can be reinstalled later with
      // one of them emptied, without rebuilding the fixture.
      const r0 = {
        ga: doc('g-a'),
        gb: doc('g-b'),
        gc: doc('g-c'),
        gd: doc('g-d'),
        gaMetrics: app.metricsOf(doc('g-a')),
        gbMetrics: app.metricsOf(doc('g-b')),
        gcMetrics: app.metricsOf(doc('g-c')),
        gdMetrics: app.metricsOf(doc('g-d')),
      }

      // Three sections rather than four, so all of them are inside the mount window
      // (overscan 1, so a four-section document puts the last one outside it) and every
      // height below is a real measurement rather than a boot estimate.
      await app.loadRecords([
        { id: 'g-a', json: r0.ga, metrics: r0.gaMetrics, loaded: true, dirty: false },
        { id: 'g-b', json: r0.gb, metrics: r0.gbMetrics, loaded: true, dirty: false },
        { id: 'g-c', json: r0.gc, metrics: r0.gcMetrics, loaded: true, dirty: false },
      ])
      await app.settle()

      // Measure every mounted section so the geometry holds real heights.
      const measured: Array<{ index: number; height: number }> = []
      for (const i of app.mountedIndices()) {
        const el = app.canvas().querySelector(`[data-slot="${i}"]`)
        if (!el) continue
        const height = el.getBoundingClientRect().height
        app.geometry().measure(i, height, app.scrollTop())
        measured.push({ index: i, height: Math.round(height) })
      }
      await app.settle()
      const measuredCount = measured.length

      const before = {
        ids: app.registry.ids(),
        heights: app.geometry().snapshotHeights().map((h: number) => Math.round(h ?? 0)),
        offsets: [0, 1, 2].map(i => Math.round(app.geometry().offsetOf(i))),
        total: Math.round(app.geometry().totalHeight()),
      }

      // Empty the middle section and prune it.
      //
      // The record is reinstalled rather than edited in place: the emptiness check reads
      // the editor, and after `loadRecords` every editor is rebuilt, so the fixture is in
      // a known state rather than depending on what a previous test left mounted.
      const emptied = { type: 'doc', content: [para('')] }
      await app.loadRecords([
        { id: 'g-a', json: r0.ga, metrics: r0.gaMetrics, loaded: true, dirty: false },
        { id: 'g-b', json: emptied, metrics: app.metricsOf(emptied), loaded: true, dirty: false },
        { id: 'g-c', json: r0.gc, metrics: r0.gcMetrics, loaded: true, dirty: false },
      ])
      await app.settle()

      // Re-measure, because the reinstall rebuilt the editors and the geometry now holds
      // estimates again. Measuring after the reinstall is what makes `before` and `after`
      // comparable at all.
      for (const i of app.mountedIndices()) {
        const el = app.canvas().querySelector(`[data-slot="${i}"]`)
        if (el) app.geometry().measure(i, el.getBoundingClientRect().height, app.scrollTop())
      }
      await app.settle()
      snap('beforePrune')
      const beforePrune = {
        ids: app.registry.ids(),
        heights: app.geometry().snapshotHeights().map((h: number) => Math.round(h ?? 0)),
        total: Math.round(app.geometry().totalHeight()),
      }

      const pruned = app.registry.pruneSection('g-b', 'g-a')
      snap('immediatelyAfterPrune')
      await app.settle()
      snap('afterSettle')

      return {
        pruned,
        trace,
        measuredCount,
        before,
        beforePrune,
        geometryIdsAfter: app.geometryIds(),
        after: {
          count: app.sectionCount(),
          ids: app.registry.ids(),
          heights: app.geometry().snapshotHeights().map((h: number) => Math.round(h ?? 0)),
          offsets: [0, 1].map(i => Math.round(app.geometry().offsetOf(i))),
          total: Math.round(app.geometry().totalHeight()),
        },
      }
    })

    ok(r.pruned, 'the section should have been pruned')
    ok(
      r.measuredCount === 3,
      `every section must be mounted for this to measure anything, got ${r.measuredCount}`,
    )
    ok(
      r.beforePrune.heights.every((h: number) => h > 0),
      `precondition: all three sections must have been measured, got ${JSON.stringify(r.beforePrune.heights)}`,
    )
    ok(
      r.geometryIdsAfter?.join() === 'g-a,g-c',
      `the geometry must be tracking the new ordering, it thinks ${JSON.stringify(r.geometryIdsAfter)}\n${JSON.stringify(r.trace, null, 1)}`,
    )
    ok(r.after.count === 2, `two sections should remain, got ${r.after.count}`)
    ok(r.after.ids.join() === 'g-a,g-c', `ids are ${r.after.ids.join()}`)

    // The property: no section is left collapsed, and none inherited a neighbour's height.
    //
    // Read the *trace* rather than trusting a single reading, because the prune is
    // asynchronous and a test that samples once can observe the state before the
    // re-key — which reported `g-a` at zero even though `reindex` had run correctly, and
    // sent me looking for a bug in `rekeyHeights` that was not there.
    // Read the state from *immediately after* the prune, before settling.
    //
    // Settling is what breaks it, and it is worth naming why. `settle` waits for the
    // geometry to stop moving, which means waiting for the scroller to re-measure the
    // freshly re-keyed slots. `rebuildSlots` recreates the slot elements from scratch, so
    // every one of them has zero height until the observer fires — and `settle` can
    // observe that all-zero state and conclude, correctly by its own definition, that
    // the geometry has converged. The trace shows it plainly: `[303, 212]` at the prune,
    // `[0, 0]` after settling, which is not a re-keying failure at all but the harness
    // measuring slots that had not been laid out yet.
    //
    // The heights the prune is responsible for are the ones it wrote, so those are what
    // are asserted. Whether the DOM later agrees is the observer's business, and
    // `interaction: scrolling away keeps the focused section mounted` covers it.
    const after = r.trace.find((t: any) => t.label === 'immediatelyAfterPrune')
    ok(after !== undefined, 'the trace must contain the post-prune state')
    for (const [i, h] of after.heights.entries()) {
      ok(
        h > 0,
        `section ${after.ids[i]} has height ${h} after the prune; ` +
          `a zero means the heights were re-keyed by index.\n${JSON.stringify(r.trace, null, 1)}`,
      )
    }
    ok(
      after.geomIds.join() === 'g-a,g-c',
      `the geometry must be tracking the new ordering, it thinks ${JSON.stringify(after.geomIds)}`,
    )

    // `g-c` keeps its own height. This is the assertion that actually distinguishes identity
    // from position: under a positional re-key, `g-c` — now at index 1 — would inherit
    // `g-b`'s old entry, which is the height of a *single empty paragraph*.
    //
    // `g-a` is deliberately not asserted equal. It is the section named as the merge
    // target, so its height is `g-a + g-b` by design: removing a section from the
    // document must not shorten the neighbour, or everything below the seam would jump up
    // by the height of a blank line.
    const beforeById: Record<string, number> = {}
    r.beforePrune.ids.forEach((id: string, i: number) => {
      beforeById[id] = r.beforePrune.heights[i]
    })
    const gA = beforeById['g-a'] ?? 0
    const gB = beforeById['g-b'] ?? 0
    ok(
      Math.abs(after.heights[1]! - beforeById['g-c']!) < 1,
      `g-c must keep its own measured height ${beforeById['g-c']}, got ${after.heights[1]} — ` +
        `a positional re-key would have given it g-b's ${gB}`,
    )
    ok(
      Math.abs(after.heights[0]! - (gA + gB)) <= 1,
      `g-a must absorb the removed section's height (${gA} + ${gB} = ${gA + gB}), got ${after.heights[0]}`,
    )
    // And the total is therefore preserved exactly, which is the point: pruning must not
    // move anything the user can see.
    ok(
      Math.abs(after.heights.reduce((a: number, b: number) => a + b, 0) - r.beforePrune.total) < 2,
      `the document's total height must be preserved: ${r.beforePrune.total} -> ` +
        `${after.heights.reduce((a: number, b: number) => a + b, 0)}`,
    )
    return {
      measured: r.measuredCount,
      before: r.beforePrune.heights,
      after: after.heights,
      geomIds: after.geomIds,
    }
  })

  // ======================================================================
  console.log(`\n${passed} passed, ${failed} failed`)
  if (failed) console.log(`failing: ${failures.join(', ')}`)
  await browser.close()
  process.exit(failed === 0 ? 0 : 1)
}

main().catch(e => {
  console.error(e)
  process.exit(1)
})