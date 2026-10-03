/**
 * Milestone 8, directive 3: seam oscillation.
 *
 * # What is being stressed
 *
 * Three pieces of machinery that were written independently and only ever met at a seam:
 *
 * - `core/rekey.ts` — re-keys measured heights across a structural change, by identity
 * - `registry.pruneSection` — removes an empty section and hands the caret back
 * - the slot DOM — `rebuildSlots` recreates `.slot` elements keyed by index
 *
 * Each is tested in isolation. This drives them together across the word boundary in both
 * directions fifty times, which is the only way to reach the failures that only appear when
 * a change is immediately followed by its inverse.
 *
 * # The specific hazard
 *
 * A split adds a section; a prune removes one. A cycle that ends where it started has to
 * leave the geometry *bit-identical*, because the scroller positions every section by
 * cumulative offset and a 1-ULP drift per cycle is 50 ULPs after fifty cycles — invisible
 * in a screenshot and exactly the kind of thing that accumulates into a scrollbar that is
 * a pixel out after a long session.
 *
 * The assertion is therefore on exact equality of the height array and the total, not on
 * "close enough". `toBeCloseTo` would pass a regression that doubles the drift per cycle,
 * which is precisely the bug.
 */

import { chromium, type Browser, type Page } from 'playwright'

const URL = process.env.HOLO_SCROLL_URL ?? 'http://localhost:5184/'

let passed = 0
let failed = 0
const failures: string[] = []

async function test(name: string, fn: () => Promise<unknown>): Promise<void> {
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
 * Install the local lifecycle transport *into the page*.
 *
 * It has to live in the browser context, not in this file: `page.evaluate` serialises its
 * arguments and cannot carry a function across, so a helper defined here is simply not
 * defined there. `ReferenceError: localSplitStore is not defined` is what that looks like.
 *
 * # Why the browser needs a transport at all
 *
 * `commitLifecycle` returns `applied: false` without a bridge, so a split cannot persist in
 * the harness. `test/scroll.ts` therefore splits by hand -- which exercises the seam rule
 * but not the reconciliation, and the reconciliation (caret anchor, registry insert, geometry
 * re-key, slot rebuild) is the machinery this file is about.
 *
 * # Why a fresh id per split
 *
 * Because the first version derived the tail's id as `${sectionId}-tail`, which collides the
 * moment anything splits twice in one run. Mounting a section dispatches transactions,
 * `onChange` calls `maybeSplitFocused`, and a second split produced a **duplicate**
 * `o-a-tail` in the registry. Every prune then addressed the wrong section and was correctly
 * refused for not being adjacent to its predecessor -- so the suite reported "the prune path
 * never ran" while actually testing a duplicate-id bug.
 *
 * `Store::split_section` allocates a ULID for exactly this reason. The counter is the honest
 * stand-in.
 */
async function installLocalSplitStore(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as any
    let seq = 0
    w.__localSplitStore = async (action: any) => {
      if (action.kind !== 'split') return { applied: false, section_ids: [], reason: 'test' }
      const ids: string[] = w.HOLO_SCROLL.registry.ids()
      const at = ids.indexOf(action.section_id)
      if (at < 0) return { applied: false, section_ids: [], reason: 'absent' }
      // The tail goes immediately after the section it came from, which is where
      // `Store::split_section` allocates its order key. Hard-coded rather than recomputed:
      // a test that derives its expectation with the same logic as the implementation
      // proves nothing.
      const tailId = `${action.section_id}-tail-${++seq}`
      return {
        applied: true,
        section_ids: [...ids.slice(0, at + 1), tailId, ...ids.slice(at + 1)],
        reason: null,
      }
    }
    // The tail, found by *position* rather than by name.
    //
    // Mounting a section dispatches transactions, `onChange` calls `maybeSplitFocused`, and
    // an extra split therefore lands between the test's explicit one and its assertion — so
    // there can be two sections matching `o-a-tail-*`. Position is unambiguous and is what
    // `pruneSection` requires anyway: the predecessor must be immediately before.
    // # Why the edit transport is part of *this* helper
    //
    // `pruneSection` refuses a section whose bytes have not been written. That is
    // data-loss protection and it is correct: the prune destroys the editor, so unsaved
    // text would go with it.
    //
    // But `EditCommitter.send` crosses the bridge, and the browser harness has no bridge,
    // so every send *throws* -- `flushNow` returns false, `markPersisted` is never called,
    // and the section born from the split stays `dirty: true` forever. Every prune was
    // then refused on a persist that had failed for want of a backend, which is the guard
    // working, and the suite was reporting it as "the prune path never ran".
    //
    // So the harness needs a persist that can actually succeed. This is the seam analogue
    // of `__localSplitStore`: the seam decision stays real, only the round trip is local.
    w.__okEditTransport = async () => null

    w.__tailAfter = (predecessorId: string) => {
      const ids: string[] = w.HOLO_SCROLL.registry.ids()
      const at = ids.indexOf(predecessorId)
      return at >= 0 ? (ids[at + 1] ?? '') : ''
    }
  })
}

/** Cycles to run. Fifty is the directive's figure and is not arbitrary. */
const CYCLES = 50

async function main() {
  const browser: Browser = await chromium.launch()
  const page: Page = await browser.newPage({ viewport: { width: 1280, height: 900 } })
  page.on('pageerror', e => console.error(`  [page error] ${e.message}`))

  await page.goto(URL, { waitUntil: 'load' })
  await page.waitForFunction(() => !!(window as any).HOLO_SCROLL, null, { timeout: 30_000 })
  await installLocalSplitStore(page)

  console.log('seam oscillation: split and prune across the 1,500-word boundary')
  console.log('='.repeat(72))

  await test(`${CYCLES} split/prune cycles leave the geometry bit-identical`, async () => {
    const r = await page.evaluate(async (cycles: number) => {
      const app = (window as any).HOLO_SCROLL
      const para = (t: string) => ({ type: 'paragraph', content: [{ type: 'text', text: t }] })

      // A section comfortably over the 1,500-word limit, so it splits, and comfortably
      // under once emptied, so it prunes. The split threshold is asserted rather than
      // assumed — a fixture that quietly stopped triggering the split would make the whole
      // cycle a no-op that passes.
      const words = 1700;
      (globalThis as any).__w = words
      const big = {
        type: 'doc',
        content: Array.from({ length: 20 }, (_, i) =>
          para(`block ${i} ` + Array.from({ length: 90 }, (_, w) => `w${w}`).join(' ')),
        ),
      }
      const empty = { type: 'doc', content: [para('')] }

      await app.loadRecords([
        { id: 'o-a', json: big, metrics: app.metricsOf(big), loaded: true, dirty: false },
        { id: 'o-b', json: empty, metrics: app.metricsOf(empty), loaded: true, dirty: false },
        { id: 'o-c', json: big, metrics: app.metricsOf(big), loaded: true, dirty: false },
      ])
      await app.settle()

      // Drive the *real* `applySplit`, not a hand-built one.
      //
      // `test/scroll.ts` splits by hand because there is no bridge in a browser and
      // `commitLifecycle` returns `applied: false`. That exercises the seam rule but not the
      // reconciliation — the anchor, the registry insert, and the geometry re-key happening
      // together — which is exactly the machinery this test is about. So the harness is
      // given a lifecycle transport that answers locally, and the split runs for real.
      //
      // The fake computes the resulting ordering the way `Store::split_section` does: the
      // tail goes immediately after the section it came from. Hard-coding that is
      // deliberate — a test that recomputes the expectation with the same logic as the
      // implementation proves nothing.
      // A fresh id per split.
      //
      // The first version derived the tail's id as `${sectionId}-tail`, which collides the
      // moment anything splits twice in one run: mounting a section dispatches
      // transactions, `onChange` calls `maybeSplitFocused`, and a second split produced a
      // *duplicate* `o-a-tail` in the registry. Every prune then addressed the wrong section
      // and was correctly refused for not being adjacent to its predecessor.
      //
      // `Store::split_section` allocates a ULID, so the honest fake allocates a counter.
      app.setLifecycleTransport((window as any).__localSplitStore)
        app.setEditTransport((window as any).__okEditTransport)

      const snapshot = () => ({
        ids: app.registry.ids(),
        geomIds: app.geometryIds(),
        heights: app.geometry().snapshotHeights().map((h: number) => h ?? 0),
        total: app.geometry().totalHeight(),
        offsets: app.registry.ids().map((_: string, i: number) => app.geometry().offsetOf(i)),
      })

      // Measure once so the geometry holds real heights rather than boot estimates. Without
      // this the test proves estimates round-trip, which is a weaker claim and would pass
      // against a re-key that ignored measurements entirely.
      for (const i of app.mountedIndices()) {
        const el = app.canvas().querySelector(`[data-slot="${i}"]`)
        if (el) app.geometry().measure(i, el.getBoundingClientRect().height, app.scrollTop())
      }
      await app.settle()

      const baseline = snapshot()
      const baselineWordCount = app.metricsOf(big).words

      let splits = 0
      let prunes = 0
      let splitFailures = 0
      let pruneFailures = 0
      let firstPruneFailure: any = null
      const firstDrift: string[] = []

      for (let cycle = 0; cycle < cycles; cycle++) {
        // --- grow past the limit, which must split ---
        const before = app.registry.ids().length
        const ok1 = await app.maybeSplit('o-a')
        await app.settle()
        const after = app.registry.ids().length
        if (ok1 && after === before + 1) splits++
        else splitFailures++

        // --- empty the tail and prune, which must remove it ---
        //
        // The *tail*, not the head. A prune needs a predecessor, and after a split the head
        // is at index 0 with nothing before it — pruning it would be correctly refused for
        // sitting at the start of the document, which would make the cycle a no-op that
        // passes. The tail is the section the Backspace-at-position-0 gesture actually acts
        // on, so it is also the realistic target.
        // The tail's id, discovered rather than assumed — `applySplit` decides it.
        const tailId = (window as any).__tailAfter('o-a')
        // Mounted before emptying, because a new tail is usually *outside* the scroller's
        // window: `setContent` on a section that is not mounted is a no-op, so the prune was
        // then correctly refused for a section that was not empty — and the suite reported
        // "the prune path never ran" rather than "the fixture never emptied anything".
        // This is also what a user does: they scroll to it.
        const tailEditor = app.registry.mount(tailId)
        if (tailEditor) {
          tailEditor.commands.setContent({ type: 'doc', content: [para('')] })
        }
        // Committed before the prune: `pruneSection` refuses a section with unsaved changes,
        // whose bytes exist only in the editor it is about to destroy. Correct behaviour, and
        // it is why a real Backspace press works (the debounce has fired) while this test did
        // not — `setContent` marks the section dirty immediately.
        await app.flushSectionNow(tailId)
        // Pruned *without* an intervening `settle`. Settling lets the scroller reconcile, and
        // a reconcile unmounts a section outside the window and remounts it from `record.json`
        // — which still holds the tail's ten blocks, so the emptiness the test just created was
        // silently undone and the prune was correctly refused. The settle belongs *after*.
        const pruned = app.registry.pruneSection(tailId, 'o-a')
        await app.settle()
        if (pruned) prunes++
        else {
          firstPruneFailure = firstPruneFailure ?? ((globalThis as any).__probe ?? []).slice(-2)
          pruneFailures++
          if (!firstPruneFailure) {
            firstPruneFailure = {
              cycle,
              tailId,
              ids: app.registry.ids(),
              tailMounted: !!app.registry.editorIfMounted(tailId),
              tailDirty: app.isPendingEdit(tailId),
              tailJson: JSON.stringify(app.storedJson(tailId)).slice(0, 100),
              tailEditorText: (app.registry.editorIfMounted(tailId)?.getJSON?.()?.content ?? [])
                .map((n: any) => n.content?.[0]?.text ?? n.type)
                .join('|')
                .slice(0, 80),
              indexOfTail: app.registry.ids().indexOf(tailId),
              indexOfPrev: app.registry.ids().indexOf('o-a'),
            }
          }
        }

        // Restore so the next cycle starts identically. `loadRecords` rebuilds every
        // editor, so the fixture is deterministic rather than depending on what the last
        // split left mounted.
        await app.loadRecords([
          { id: 'o-a', json: big, metrics: app.metricsOf(big), loaded: true, dirty: false },
          { id: 'o-b', json: empty, metrics: app.metricsOf(empty), loaded: true, dirty: false },
          { id: 'o-c', json: big, metrics: app.metricsOf(big), loaded: true, dirty: false },
        ])
        await app.settle()

        const now = snapshot()
        if (
          now.heights.length !== baseline.heights.length ||
          now.heights.some((h: number, i: number) => h !== baseline.heights[i]) ||
          now.total !== baseline.total
        ) {
          if (firstDrift.length === 0) {
            firstDrift.push(
              `cycle ${cycle}: baseline ${JSON.stringify(baseline.heights)}/${baseline.total} ` +
                `now ${JSON.stringify(now.heights)}/${now.total}`,
            )
          }
        }
      }

      const final = snapshot()
      return {
        baseline,
        final,
        splits,
        prunes,
        splitFailures,
        pruneFailures,
        firstPruneFailure,
        baselineWordCount,
        firstDrift,
        exact: JSON.stringify(final.heights) === JSON.stringify(baseline.heights) &&
          final.total === baseline.total,
      }
    }, CYCLES)

    // The fixture must actually be exercising both paths. A suite where `splits` is zero
    // asserts nothing about a split.
    ok(r.baselineWordCount > 1500, `the fixture must exceed the split threshold, got ${r.baselineWordCount} words`)
    ok(
      r.splits > 0,
      `the split path never ran (${r.splitFailures} failures) — this test would prove nothing`,
    )
    ok(
      r.prunes > 0,
      `the prune path never ran (${r.pruneFailures} failures) — this test would prove nothing.\n` +
        `  first failure: ${JSON.stringify(r.firstPruneFailure, null, 1)}`,
    )
    ok(
      r.splitFailures === 0,
      `${r.splitFailures} of ${CYCLES} splits failed to add a section`,
    )
    ok(
      r.pruneFailures === 0,
      `${r.pruneFailures} of ${CYCLES} prunes failed to remove the section.\n` +
        `  first failure: ${JSON.stringify(r.firstPruneFailure, null, 1)}`,
    )

    // The headline claim: exact, not approximate.
    ok(
      r.exact,
      `the geometry drifted across oscillation\n  baseline ${JSON.stringify(r.baseline.heights)} total ${r.baseline.total}\n` +
        `  final    ${JSON.stringify(r.final.heights)} total ${r.final.total}\n` +
        (r.firstDrift[0] ? `  first divergence: ${r.firstDrift[0]}` : ''),
    )
    ok(r.firstDrift.length === 0, `drift appeared mid-run: ${r.firstDrift[0]}`)
    ok(
      r.final.geomIds.join() === r.baseline.geomIds.join(),
      `the geometry's id map drifted: ${r.baseline.geomIds.join()} -> ${r.final.geomIds.join()}`,
    )

    return {
      cycles: CYCLES,
      splits: r.splits,
      prunes: r.prunes,
      heights: r.final.heights,
      total: r.final.total,
    }
  })

  await test('slot keys stay unique and contiguous through oscillation', async () => {
    // The DOM half of the same claim. `rebuildSlots` recreates slots keyed by *index*, so
    // a structural change is exactly when a slot can end up pointing at the wrong section,
    // and a duplicated key would silently merge two sections' geometry.
    const r = await page.evaluate(async (cycles: number) => {
      const app = (window as any).HOLO_SCROLL
      const para = (t: string) => ({ type: 'paragraph', content: [{ type: 'text', text: t }] })
      const big = {
        type: 'doc',
        content: Array.from({ length: 20 }, (_, i) =>
          para(`block ${i} ` + Array.from({ length: 90 }, (_, w) => `w${w}`).join(' ')),
        ),
      }
      const empty = { type: 'doc', content: [para('')] }

      const worst = { duplicateKeys: 0, gaps: 0, orphans: 0, extraSlices: 0, worstCycle: -1, unordered: -1 }

      app.setLifecycleTransport((window as any).__localSplitStore)
        app.setEditTransport((window as any).__okEditTransport)

      for (let cycle = 0; cycle < cycles; cycle++) {
        await app.loadRecords([
          { id: 'o-a', json: big, metrics: app.metricsOf(big), loaded: true, dirty: false },
          { id: 'o-b', json: empty, metrics: app.metricsOf(empty), loaded: true, dirty: false },
          { id: 'o-c', json: big, metrics: app.metricsOf(big), loaded: true, dirty: false },
        ])
        await app.settle()

        await app.maybeSplit('o-a')
        await app.settle()

        const tailId = (window as any).__tailAfter('o-a')
        const tailEditor = app.registry.mount(tailId)
        if (tailEditor) tailEditor.commands.setContent({ type: 'doc', content: [para('')] })
        await app.flushSectionNow(tailId)
        await app.registry.pruneSection(tailId, 'o-a')
        await app.settle()

        // Read the DOM, not the model: the model being correct says nothing about whether
        // the slots on screen still agree with it.
        const keys = Array.from(app.canvas().querySelectorAll('[data-slot]')).map((el: any) =>
          Number(el.dataset.slot),
        )
        const slices = app.canvas().querySelectorAll('.section-slice').length

        const dupes = keys.length - new Set(keys).size
        // Keys must be ascending and in range. *Contiguity* is deliberately not asserted:
        // the scroller mounts a window and `pruneSlots` removes a slot as soon as its
        // section unmounts, so a mid-window unmount legitimately leaves a gap. The first
        // version demanded `0..n-1` and reported two gaps on a healthy document — the two
        // sections the virtual scroller had correctly unmounted.
        const ascending = keys.every((k: number, i: number) => i === 0 || k > keys[i - 1]!)
        const outOfRange = keys.filter((k: number) => k < 0 || k >= app.sectionCount()).length

        // A `.section-slice` with no `[data-slot]` ancestor is an orphaned editor card: the
        // section it belonged to has been pruned but the DOM outlived it.
        //
        // Checked with `closest`, not `.section-slice:not([data-slot])` — the slice is a
        // *child* of the slot, so it never carries the attribute itself and that selector
        // matched every slice on the page. The first version reported three orphans on a
        // document that had none, and the "three" was exactly the number of sections.
        const orphans = Array.from(app.canvas().querySelectorAll('.section-slice')).filter(
          (el: any) => !el.closest('[data-slot]'),
        ).length

        if (dupes > 0 || !ascending || outOfRange > 0 || orphans > 0) {
          worst.worstCycle = cycle
          worst.duplicateKeys = Math.max(worst.duplicateKeys, dupes)
          worst.gaps = Math.max(worst.gaps, outOfRange)
          worst.orphans = Math.max(worst.orphans, orphans)
          if (!ascending) worst.unordered = cycle
        }
        worst.extraSlices = Math.max(worst.extraSlices, slices)
      }

      const finalKeys = Array.from(app.canvas().querySelectorAll('[data-slot]')).map((el: any) =>
        Number(el.dataset.slot),
      )
      return { worst, finalKeys, sectionCount: app.sectionCount() }
    }, CYCLES)

    ok(
      r.worst.duplicateKeys === 0,
      `slot keys duplicated (${r.worst.duplicateKeys}) on cycle ${r.worst.worstCycle}`,
    )
    ok(
      r.worst.gaps === 0,
      `slot keys out of range for the section count (${r.worst.gaps}) on cycle ${r.worst.worstCycle}`,
    )
    ok(
      r.worst.unordered === -1,
      `slot keys were not in ascending document order on cycle ${r.worst.unordered}, which is ` +
        'what a slot pointing at the wrong section looks like',
    )
    ok(
      r.worst.orphans === 0,
      `${r.worst.orphans} orphaned .section-slice nodes with no slot on cycle ${r.worst.worstCycle}`,
    )
    // Fewer slots than sections is *correct*: the scroller mounts a window and `pruneSlots`
    // removes the slot of anything it unmounts, so a 4-section document legitimately has 3
    // slots. What must hold is that every key is in range, unique, and ascending — which is
    // what `worst.gaps`/`worst.duplicateKeys` already check. Asserting an equal count is
    // what the first version did, and it failed on a healthy document.
    ok(
      r.finalKeys.every((k: number) => k >= 0 && k < r.sectionCount),
      `slot keys out of range: ${JSON.stringify(r.finalKeys)} for ${r.sectionCount} sections`,
    )
    ok(
      r.finalKeys.length === new Set(r.finalKeys).size,
      `slot keys duplicated at the end of the run: ${JSON.stringify(r.finalKeys)}`,
    )
    return { slots: r.finalKeys, sections: r.sectionCount }
  })

  await test('an image at the split boundary keeps its refcount balanced', async () => {
    // The refcount invariant. `AssetImage`'s NodeView owns one reference and releases it in
    // `destroy`; a split destroys an editor and a prune destroys another, so this is where
    // a leaked or double-released reference would show up.
    //
    // Asserted as *zero outstanding* after the cycle rather than "the count is right",
    // because the count is only meaningful against a known starting point and the
    // interesting failure is a monotonic climb that a single-cycle check would miss.
    const r = await page.evaluate(async (cycles: number) => {
      const app = (window as any).HOLO_SCROLL
      const para = (t: string) => ({ type: 'paragraph', content: [{ type: 'text', text: t }] })
      const hash = 'd'.repeat(64)
      const big = {
        type: 'doc',
        content: Array.from({ length: 20 }, (_, i) =>
          para(`block ${i} ` + Array.from({ length: 90 }, (_, w) => `w${w}`).join(' ')),
        ),
      }
      // The image sits at the *top* of the section, so a split has to decide which half it
      // belongs to — the case where a NodeView is destroyed on one side while the reference
      // was taken on the other.
      const withImage = {
        type: 'doc',
        content: [
          { type: 'image', attrs: { src: `holo-asset://${hash}`, alt: 'boundary figure' } },
          ...big.content,
        ],
      }
      const empty = { type: 'doc', content: [para('')] }

      const snapshotRefs = () => {
        const st = app.assetRefState?.(hash)
        // `null` means "no entry", which is *zero references held* — a legitimate state, and
        // the one the teardown assertions expect. It is not the same as the surface being
        // absent, and conflating the two produced a test that reported "not checked at all"
        // on a page that had the surface and simply held nothing.
        return st ? st.refs : 0
      }

      if (typeof app.assetRefState !== 'function') {
        throw new Error('the page exposes no asset refcount surface (`app.assetRefState`)')
      }

      // Without a byte source the resolver cannot acquire anything, so no reference is
      // ever taken and the invariant would hold vacuously. Stubbed so the figure really is
      // fetched and really does hold a reference while its section is mounted.
      app.setAssetSource(async () => ({ mime: 'image/png', bytes: Uint8Array.from([1, 2, 3, 4]) }))

      // The split needs the same local lifecycle transport as the geometry test.
      app.setLifecycleTransport((window as any).__localSplitStore)
        app.setEditTransport((window as any).__okEditTransport)

      const samples: Array<number | null> = []

      for (let cycle = 0; cycle < cycles; cycle++) {
        await app.loadRecords([
          { id: 'r-a', json: withImage, metrics: app.metricsOf(withImage), loaded: true, dirty: false },
          { id: 'r-b', json: empty, metrics: app.metricsOf(empty), loaded: true, dirty: false },
        ])
        await app.settle()

        await app.maybeSplit('r-a')
        await app.settle()
        samples.push(snapshotRefs())

        const editor: any = app.registry.editorIfMounted('r-a')
        if (editor) editor.commands.setContent({ type: 'doc', content: [para('')] })
        await app.settle()
        await app.registry.pruneSection('r-a', 'r-b')
        await app.settle()
        samples.push(snapshotRefs())

        // Full teardown: everything must go back to zero.
        await app.loadRecords([
          { id: 'r-b', json: empty, metrics: app.metricsOf(empty), loaded: true, dirty: false },
        ])
        await app.settle()
        samples.push(snapshotRefs())
      }

      const nums = samples.filter((n): n is number => typeof n === 'number')
      return { samples, peak: nums.length ? Math.max(...nums) : 0, totalRefs: app.assetTotalRefs?.() ?? null }
    }, CYCLES)

    const observed = r.samples.filter((n): n is number => typeof n === 'number')
    ok(observed.length > 0, 'no refcount samples were taken, so nothing was checked')

    for (const [i, n] of observed.entries()) {
      ok(n >= 0, `refcount went negative (${n}) at sample ${i}: a reference was released twice`)
    }
    ok(
      r.peak > 0,
      'the figure never held a reference, so the acquire/release cycle was never exercised ' +
        'and this test proved nothing',
    )
    ok(
      r.totalRefs === 0,
      `${r.totalRefs} references are still held after ${CYCLES} cycles; every NodeView should ` +
        'have been destroyed',
    )
    return { samples: observed.length, peak: r.peak, totalRefs: r.totalRefs }
  })

  console.log('='.repeat(72))
  console.log(`${passed} passed, ${failed} failed`)
  if (failed) console.log(`failing: ${failures.join(', ')}`)
  await browser.close()
  process.exit(failed === 0 ? 0 : 1)
}

main().catch(e => {
  console.error(e)
  process.exit(1)
})