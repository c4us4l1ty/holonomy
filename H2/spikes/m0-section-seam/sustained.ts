/**
 * Sustained-editing check for the M0 seam decision.
 *
 * M0's numbers come from short bursts: 40 keystrokes, 10 window slides. A real
 * session is neither. The user opens a document, edits one place for twenty
 * minutes, then scrolls. The question this answers is whether editing cost drifts
 * upward as a section accumulates changes, because a strategy that is fast for
 * the first minute and unusable by the tenth is not fast.
 *
 * Also re-runs the M0 comparison at a smaller corpus, so the ordering can be
 * checked for consistency rather than trusted from one run.
 *
 * Run: node --experimental-strip-types sustained.ts
 */
import { chromium } from 'playwright'

const URL = process.env.HOLO_URL ?? 'http://localhost:5183/'
const EDITS = Number(process.env.HOLO_EDITS ?? 2000)
const CORPUS = {
  targetWords: Number(process.env.HOLO_WORDS ?? 200_000),
  wordsPerSection: Number(process.env.HOLO_WORDS_PER_SECTION ?? 1500),
}

interface Row {
  strategy: string
  domNodes: number
  firstP50: number
  lastP50: number
  drift: number
  worstP95: number
  accepted: number
  of: number
  valid: boolean
}

const browser = await chromium.launch()
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } })
page.on('pageerror', e => console.error('[page error]', e.message))
page.on('console', m => {
  if (m.type() === 'error') console.error('[console]', m.text())
})

const rows: Row[] = []

for (const key of ['A', 'B', 'C']) {
  process.stdout.write(`  ${key}: `)
  await page.goto(URL, { waitUntil: 'load' })
  await page.waitForFunction(() => !!(window as any).HOLO, null, { timeout: 30_000 })

  const init = await page.evaluate(
    o => (window as any).HOLO.init(o),
    { ...CORPUS, strategy: key },
  )
  await page.waitForTimeout(250)

  // Use the focused-section variant: editing at the raw document midpoint lands
  // in a frozen neighbour for the window strategy, so its edits are rejected and
  // the measurement reads as "0ms" rather than "fast".
  const s = await page.evaluate(n => (window as any).HOLO.measureSustainedFocused(n), EDITS)
  const stats = await page.evaluate(() => (window as any).HOLO.stats())
  await page.evaluate(() => (window as any).HOLO.teardown())

  const first = s.buckets[0].p50
  const last = s.buckets[s.buckets.length - 1].p50
  rows.push({
    strategy: init.strategyName,
    domNodes: stats.domNodes,
    firstP50: first,
    lastP50: last,
    drift: s.drift,
    worstP95: Math.max(...s.buckets.map((b: any) => b.p95)),
    accepted: s.accepted,
    of: s.totalEdits,
    valid: s.valid,
  })

  console.log(
    `accepted ${s.accepted}/${s.totalEdits}  drift ${s.drift}x  ` +
      `(p50 ${first} -> ${last}ms)  worst p95 ${rows.at(-1)!.worstP95}ms  ` +
      `${s.valid ? 'valid' : 'EDITS REJECTED'}`,
  )
  const curve = s.buckets.map((b: any) => b.p50.toFixed(2)).join(' ')
  console.log(`       p50 by decile: ${curve}`)
}

console.log(`\n=== sustained editing: ${EDITS} edits, ${CORPUS.targetWords} words ===`)
const cols: Array<keyof Row> = [
  'strategy', 'domNodes', 'accepted', 'firstP50', 'lastP50', 'drift', 'worstP95', 'valid',
]
const widths = cols.map(c => Math.max(c.length, ...rows.map(r => String(r[c]).length)))
const line = (v: string[]) => v.map((x, i) => x.padEnd(widths[i])).join('  ')
console.log(line(cols.map(String)))
console.log(widths.map(w => '─'.repeat(w)).join('  '))
for (const r of rows) console.log(line(cols.map(c => String(r[c]))))

const invalid = rows.filter(r => !r.valid)
if (invalid.length) {
  console.log(`\nWARNING: ${invalid.length} strategy measurement(s) were invalid.`)
}

await browser.close()
