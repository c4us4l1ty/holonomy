/**
 * Sanity checks for the M0 harness.
 *
 * These exist because the first run produced confidently wrong numbers: an
 * empty document (docSize 2) reporting 0.1ms keystrokes. Every assertion here
 * is a guard against measuring nothing.
 */
import { chromium } from 'playwright'

const URL = process.env.HOLO_URL ?? 'http://localhost:5183/'

let failures = 0
function check(name: string, ok: boolean, detail?: unknown) {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail !== undefined ? `  ${JSON.stringify(detail)}` : ''}`)
  if (!ok) failures++
}

const browser = await chromium.launch()
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } })
page.on('pageerror', e => console.error('  [page error]', e.message))
page.on('console', m => {
  if (m.type() === 'error') console.error('  [console]', m.text())
})

await page.goto(URL, { waitUntil: 'load' })
await page.waitForFunction(() => !!(window as any).HOLO, null, { timeout: 30_000 })

for (const key of ['A', 'B', 'C']) {
  console.log(`\n--- strategy ${key} ---`)
  const init = await page.evaluate(
    o => (window as any).HOLO.init(o),
    { targetWords: 20_000, wordsPerSection: 1500, strategy: key },
  )
  await page.waitForTimeout(250)

  // 1. Document is actually populated.
  const stats = await page.evaluate(() => (window as any).HOLO.stats())
  check(`${key}: doc is non-empty`, stats.docSize > 1000, { docSize: stats.docSize })
  check(`${key}: DOM is populated`, stats.domNodes > 50, { domNodes: stats.domNodes })

  // 2. Real text is rendered, and it is OUR text (not a placeholder).
  const textProbe = await page.evaluate(() => {
    const el = document.querySelector('#viewport-host .ProseMirror') as HTMLElement
    return { chars: el.innerText.length, head: el.innerText.slice(0, 80) }
  })
  check(`${key}: renders real text`, textProbe.chars > 500, textProbe)

  // 3. Tables survive. The corpus generates them; StarterKit alone drops them
  //    with a RangeError, which silently produced a much smaller document.
  const hasTable = await page.evaluate(() => !!document.querySelector('#viewport-host table'))
  console.log(`  info  ${key}: tables rendered in first section = ${hasTable}`)

  // 4. The measurement path actually mutates the document. This is the check
  //    that would have caught the execCommand no-op.
  const ks = await page.evaluate(() => (window as any).HOLO.measureKeystrokes(20))
  check(`${key}: keystroke measurement is valid`, ks.valid === true, {
    docGrewBy: ks.docGrewBy,
    p50: ks.p50,
  })
  check(`${key}: keystrokes have plausible cost`, ks.p50 > 0.05 && ks.p50 < 50, { p50: ks.p50, p95: ks.p95 })

  // 5. Cursor can be placed and maps back to the DOM.
  const cur = await page.evaluate(() => (window as any).HOLO.testCursorSurvival(0, 5))
  check(`${key}: cursor lands exactly`, cur.exact === true, { requested: cur.requested, landed: cur.landed })
  check(`${key}: cursor maps to DOM`, cur.domBacked === true)

  // 6. Cross-boundary behaviour.
  const cross = await page.evaluate(() => (window as any).HOLO.testCrossBoundary())
  check(`${key}: cross-boundary typing mutates doc`, cross.docGrewBy > 0, {
    docGrewBy: cross.docGrewBy,
    ms: cross.typeMs,
  })

  // 7. Section identity is actually present in the schema. Without declared
  //    sectionIndex attributes the window strategy cannot see its own seams.
  const sel = await page.evaluate(() => (window as any).HOLO.testSelectionAcross())
  console.log(`  info  ${key}: selection probe = ${JSON.stringify(sel)}`)

  // 8. If the strategy claims to freeze non-focused sections, that claim must
  //    hold. This is the property the whole architecture rests on.
  const frozen = await page.evaluate(() => (window as any).HOLO.testFrozenEditBlocked())
  if (frozen.applicable) {
    check(`${key}: frozen sections reject edits`, frozen.blocked === true, frozen)
  } else {
    console.log(`  info  ${key}: no frozen state to test (${frozen.note})`)
  }

  await page.evaluate(() => (window as any).HOLO.teardown())
}

console.log(`\n${failures === 0 ? 'ALL CHECKS PASSED' : `${failures} CHECK(S) FAILED`}`)
await browser.close()
process.exit(failures === 0 ? 0 : 1)
