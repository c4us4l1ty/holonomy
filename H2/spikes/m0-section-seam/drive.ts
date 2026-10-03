/**
 * Playwright driver for the M0 spike.
 *
 * Boots the vite dev server page, then runs the same measurement battery
 * against each seam strategy so the results are directly comparable.
 */
import { chromium, type Browser, type Page } from 'playwright'

const URL = process.env.HOLO_URL ?? 'http://localhost:5183/'

// A 2000-page document at ~350 words/page is ~700k words. HOLO_WORDS and
// HOLO_WORDS_PER_SECTION let us sweep sizes without editing the file.
const CORPUS = {
  targetWords: Number(process.env.HOLO_WORDS ?? 700_000),
  wordsPerSection: Number(process.env.HOLO_WORDS_PER_SECTION ?? 1500),
}
const STRATEGIES = [
  { key: 'A', label: 'content-swap' },
  { key: 'B', label: 'sliding-window' },
  { key: 'C', label: 'multi-instance' },
]

interface Row {
  strategy: string
  [k: string]: unknown
}

async function measure(page: Page, key: string) {
  await page.goto(URL, { waitUntil: 'load' })
  await page.waitForFunction(() => !!(window as any).HOLO, null, { timeout: 30_000 })

  const init = await page.evaluate(
    o => (window as any).HOLO.init(o),
    { ...CORPUS, strategy: key },
  )

  // Let layout settle before measuring anything.
  await page.waitForTimeout(300)

  const stats = await page.evaluate(() => (window as any).HOLO.stats())
  const keystrokes = await page.evaluate(() => (window as any).HOLO.measureKeystrokes(60))
  const coldLoad = await page.evaluate(
    () => (window as any).HOLO.measureColdLoad([0, 10, 20, 30, 40, 50, 60, 70, 80, 90]),
  )
  const swap = await page.evaluate(() => (window as any).HOLO.measureSwap(12))
  const cursor = await page.evaluate(() => (window as any).HOLO.testCursorSurvival(0, 5))
  const cross = await page.evaluate(() => (window as any).HOLO.testCrossBoundary())
  const selAcross = await page.evaluate(() => (window as any).HOLO.testSelectionAcross())
  const frozen = await page.evaluate(() => (window as any).HOLO.testFrozenEditBlocked())
  const scroll = await page.evaluate(() => (window as any).HOLO.scrollToSection(50))

  await page.evaluate(() => (window as any).HOLO.teardown())

  return {
    strategy: init.strategyName,
    mode: init.crossBoundaryMode,
    sections: init.sections,
    words: init.actualWords,
    manifestKB: Math.round(init.manifestBytes / 1024),
    totalMB: +(init.totalBytes / 1e6).toFixed(2),
    docSize: stats.docSize,
    domNodes: stats.domNodes,
    mounted: stats.mountedSections,
    // Keystroke cost. `valid` guards against measuring a no-op.
    ksP50: keystrokes.p50,
    ksP95: keystrokes.p95,
    ksMax: keystrokes.max,
    ksValid: keystrokes.valid,
    // Cold path: manifest hit -> parse -> usable section.
    loadP50: coldLoad.p50,
    loadP95: coldLoad.p95,
    // Window slide: the cost a user feels when moving between sections.
    swapP50: swap.p50,
    swapP95: swap.p95,
    swapMax: swap.max,
    cursorOK: cursor.exact && cursor.domBacked,
    crossOK: cross.docGrewBy > 0,
    selSpan: selAcross.spanIsContiguous ?? null,
    // "n/a" when the strategy has no frozen state to protect.
    frozenHeld: frozen.applicable ? (frozen.blocked ? 'yes' : 'NO') : 'n/a',
    scrollPx: Math.round(scroll.totalPx),
  } satisfies Row
}

function table(rows: Row[]) {
  if (!rows.length) return
  const cols = Object.keys(rows[0])
  const widths = cols.map(c => Math.max(c.length, ...rows.map(r => String(r[c]).length)))
  const line = (vals: string[]) => vals.map((v, i) => v.padEnd(widths[i])).join('  ')
  console.log(line(cols))
  console.log(widths.map(w => '─'.repeat(w)).join('  '))
  for (const r of rows) console.log(line(cols.map(c => String(r[c]))))
}

async function main() {
  const browser: Browser = await chromium.launch()
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } })
  page.on('pageerror', e => console.error('  [page error]', e.message))
  page.on('console', m => {
    if (m.type() === 'error') console.error('  [console error]', m.text())
  })

  const rows: Row[] = []
  for (const s of STRATEGIES) {
    process.stdout.write(`measuring strategy ${s.key} (${s.label}) ... `)
    try {
      const r = await measure(page, s.key)
      rows.push(r)
      console.log('ok')
    } catch (e: any) {
      console.log('FAILED')
      console.error('   ', e.message)
    }
  }

  console.log('\n=== M0 seam spike ===')
  console.log(`corpus: ${CORPUS.targetWords} target words / ${CORPUS.wordsPerSection} per section\n`)
  table(rows)

  await browser.close()
}

main().catch(e => {
  console.error(e)
  process.exit(1)
})
