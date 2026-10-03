/**
 * Sweep the two architectural dials against each strategy.
 *
 * wordsPerSection is the central trade-off in the whole design:
 *   - larger sections  -> fewer window slides, but a heavier DOM and a more
 *                         expensive cold parse
 *   - smaller sections  -> cheaper slides, but slides happen more often and
 *                         the manifest grows
 *
 * A section is also the sync, storage, and CRDT unit, so its size has
 * consequences well beyond rendering. This sweep is meant to find the knee.
 */
import { chromium, type Page } from 'playwright'

const URL = process.env.HOLO_URL ?? 'http://localhost:5183/'

const TOTAL_WORDS = Number(process.env.HOLO_WORDS ?? 700_000)
const SECTION_SIZES = (process.env.HOLO_SIZES ?? '500,1000,1500,3000,6000')
  .split(',')
  .map(Number)
const STRATEGIES = (process.env.HOLO_STRATEGIES ?? 'A,B,C').split(',')

interface Row {
  wordsPerSection: number
  strategy: string
  sections: number
  domNodes: number
  docKB: number
  ksP50: number
  ksP95: number
  loadP95: number
  swapP50: number
  swapP95: number
  frozen: string
}

async function run(page: Page, strategy: string, wordsPerSection: number): Promise<Row> {
  await page.goto(URL, { waitUntil: 'load' })
  await page.waitForFunction(() => !!(window as any).HOLO, null, { timeout: 30_000 })

  const init = await page.evaluate(
    o => (window as any).HOLO.init(o),
    { targetWords: TOTAL_WORDS, wordsPerSection, strategy },
  )
  await page.waitForTimeout(250)

  const stats = await page.evaluate(() => (window as any).HOLO.stats())
  const ks = await page.evaluate(() => (window as any).HOLO.measureKeystrokes(40))
  // Sample across the document proportionally. Hard-coded indices broke once
  // a section size produced fewer sections than the largest index we asked for.
  const loadIdx = Array.from({ length: 10 }, (_, i) =>
    Math.min(init.sections - 1, Math.floor((i * init.sections) / 10)),
  )
  const load = await page.evaluate(
    idx => (window as any).HOLO.measureColdLoad(idx),
    loadIdx,
  )
  const swap = await page.evaluate(() => (window as any).HOLO.measureSwap(10))
  const frozen = await page.evaluate(() => (window as any).HOLO.testFrozenEditBlocked())

  await page.evaluate(() => (window as any).HOLO.teardown())

  return {
    wordsPerSection,
    strategy: init.strategyName.split(':')[0],
    sections: init.sections,
    domNodes: stats.domNodes,
    docKB: Math.round(stats.docSize / 1024),
    ksP50: ks.p50,
    ksP95: ks.p95,
    loadP95: load.p95,
    swapP50: swap.p50,
    swapP95: swap.p95,
    frozen: frozen.applicable ? (frozen.blocked ? 'held' : 'BROKEN') : '-',
  }
}

function table(rows: Row[], groupBy: string) {
  const cols = Object.keys(rows[0]) as Array<keyof Row>
  const w = cols.map(c => Math.max(String(c).length, ...rows.map(r => String(r[c]).length)))
  const line = (v: string[]) => v.map((x, i) => x.padEnd(w[i])).join('  ')
  const groups = [...new Set(rows.map(r => String(r[groupBy])))].sort((a, b) => Number(a) - Number(b))
  for (const g of groups) {
    console.log(`\n--- ${groupBy} = ${g} ---`)
    console.log(line(cols.map(String)))
    console.log(w.map(x => '─'.repeat(x)).join('  '))
    for (const r of rows.filter(r => String(r[groupBy]) === g)) {
      console.log(line(cols.map(c => String(r[c]))))
    }
  }
}

const browser = await chromium.launch()
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } })
page.on('pageerror', e => console.error('[page error]', e.message))
page.on('console', m => {
  if (m.type() === 'error') console.error('[console]', m.text())
})

const rows: Row[] = []
for (const size of SECTION_SIZES) {
  for (const s of STRATEGIES) {
    process.stdout.write(`  ${s} @ ${size} words/section ... `)
    try {
      rows.push(await run(page, s, size))
      console.log('ok')
    } catch (e: any) {
      console.log(`FAILED: ${e.message}`)
    }
  }
}

console.log(`\n=== M0 sweep: ${TOTAL_WORDS} words total ===`)
table(rows, 'wordsPerSection')

await browser.close()
