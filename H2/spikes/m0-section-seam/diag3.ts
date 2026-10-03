import { chromium } from 'playwright'

const URL = process.env.HOLO_URL ?? 'http://localhost:5183/'
const browser = await chromium.launch()
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } })
page.on('pageerror', e => console.error('[pageerror]', e.message))
page.on('console', m => {
  const t = m.text()
  if (!t.includes('[vite]') && !t.includes('[harness]')) console.log(`[${m.type()}]`, t)
})

await page.goto(URL, { waitUntil: 'load' })
await page.waitForFunction(() => !!(window as any).HOLO, null, { timeout: 30_000 })

await page.evaluate(() => (window as any).HOLO.init({ targetWords: 20000, wordsPerSection: 1500, strategy: 'B' }))
await page.waitForTimeout(300)

const diag = await page.evaluate(() => (window as any).HOLO.diagnoseFrozen())
console.log('=== frozen diagnosis ===')
console.log(JSON.stringify(diag, null, 2))

const frozen = await page.evaluate(() => (window as any).HOLO.testFrozenEditBlocked())
console.log('\n=== frozen edit attempt ===')
console.log(JSON.stringify(frozen, null, 2))

await browser.close()
