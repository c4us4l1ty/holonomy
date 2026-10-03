import { chromium } from 'playwright'
const URL = process.env.HOLO_URL ?? 'http://localhost:5183/'
const browser = await chromium.launch()
const page = await browser.newPage()
page.on('pageerror', e => console.error('[pageerror]', e.message))
await page.goto(URL, { waitUntil: 'load' })
await page.waitForFunction(() => !!(window as any).HOLO, null, { timeout: 30_000 })
await page.evaluate(() => (window as any).HOLO.init({ targetWords: 20000, wordsPerSection: 1500, strategy: 'B' }))
await page.waitForTimeout(300)
console.log(JSON.stringify(await page.evaluate(() => (window as any).HOLO.traceDelete()), null, 2))
await browser.close()
