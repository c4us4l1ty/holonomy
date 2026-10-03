/**
 * Re-run the seam measurements in webkit2gtk, the engine Tauri actually uses
 * on Linux.
 *
 * The Chromium numbers are not sufficient on their own: earlier research
 * flagged webkit2gtk as the weakest of the three webviews Tauri targets
 * (large-DOM drag-select problems, tauri-apps/tauri#3988). If strategy C's
 * advantage is real it should survive the weaker engine, because C's whole
 * point is keeping the DOM small. If the ordering flips here, the Chromium
 * result does not transfer.
 *
 * Driven over W3C WebDriver via WebKitWebDriver rather than Playwright, which
 * has no WebKitGTK backend.
 */
import { setTimeout as sleep } from 'node:timers/promises'

const DRIVER = process.env.WK_DRIVER ?? 'http://localhost:4444'
const URL_ = process.env.HOLO_URL ?? 'http://localhost:5183/'
const STRATEGIES = (process.env.HOLO_STRATEGIES ?? 'A,B,C').split(',')
const SIZES = (process.env.HOLO_SIZES ?? '1500').split(',').map(Number)
const TOTAL_WORDS = Number(process.env.HOLO_WORDS ?? 700_000)

// WebKitWebDriver is markedly slower than Chromium at session setup and at
// large JSON payloads. undici's default 300s header timeout is not enough for
// a 1M-word corpus, and it aborts the whole run rather than one request.
const TIMEOUT_MS = Number(process.env.WK_TIMEOUT_MS ?? 1_800_000)

class WebDriver {
  private session: string | null = null

  /** Every request gets the long timeout; see TIMEOUT_MS. */
  private async req(url: string, init: RequestInit) {
    return fetch(url, { ...init, signal: AbortSignal.timeout(TIMEOUT_MS) })
  }

  async start() {
    const r = await this.req(`${DRIVER}/session`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        capabilities: {
          alwaysMatch: {
            // An empty binary string makes WebKitGTK fail to spawn
            // ("g_subprocess_launcher_spawnv: assertion 'argv != NULL'").
            // MiniBrowser ships with webkit2gtk-4.1 and is what this driver
            // expects on Linux.
            'webkitgtk:browserOptions': {
              binary: '/usr/libexec/webkit2gtk-4.1/MiniBrowser',
            },
          },
        },
      }),
    })
    const j: any = await r.json()
    if (j.value?.sessionId) {
      this.session = j.value.sessionId
      return
    }
    throw new Error(`session failed: ${JSON.stringify(j).slice(0, 500)}`)
  }

  private async cmd(method: string, suffix: string, body?: unknown) {
    const r = await this.req(`${DRIVER}/session/${this.session}${suffix}`, {
      method,
      headers: { 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
    const j: any = await r.json()
    if (j.value?.error) throw new Error(`${j.value.error}: ${j.value.message}`)
    return j.value
  }

  navigate(url: string) {
    return this.cmd('POST', '/url', { url })
  }
  script(src: string, args: unknown[] = []) {
    return this.cmd('POST', '/execute/sync', { script: src, args })
  }
  async stop() {
    if (this.session) await this.cmd('DELETE', '').catch(() => {})
    this.session = null
  }
}

interface Row {
  engine: string
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
  notes: string
}

async function measure(wd: WebDriver, strategy: string, wordsPerSection: number): Promise<Row> {
  await wd.navigate(URL_)
  // Poll for the harness rather than assuming a fixed load time.
  let ready = false
  for (let i = 0; i < 60; i++) {
    await sleep(500)
    try {
      const r = await wd.script('return typeof window.HOLO')
      if (r === 'object') { ready = true; break }
    } catch {
      /* page still loading */
    }
  }
  if (!ready) throw new Error('harness never became ready')

  const init: any = await wd.script(
    'const c = arguments[0]; return window.HOLO.init(c)',
    [{ targetWords: TOTAL_WORDS, wordsPerSection, strategy }],
  )
  await sleep(600)

  const stats: any = await wd.script('return window.HOLO.stats()')
  const ks: any = await wd.script('return window.HOLO.measureKeystrokes(40)')
  const loadIdx = Array.from({ length: 10 }, (_, i) =>
    Math.min(init.sections - 1, Math.floor((i * init.sections) / 10)),
  )
  const load: any = await wd.script(
    'const ix = arguments[0]; return window.HOLO.measureColdLoad(ix)',
    [loadIdx],
  )
  const swap: any = await wd.script('return window.HOLO.measureSwap(10)')
  const frozen: any = await wd.script('return window.HOLO.testFrozenEditBlocked()')
  const ua: string = await wd.script('return navigator.userAgent')

  await wd.script('window.HOLO.teardown()')

  return {
    engine: 'webkit2gtk',
    wordsPerSection,
    strategy: String(init.strategyName).split(':')[0],
    sections: init.sections,
    domNodes: stats.domNodes,
    docKB: Math.round(stats.docSize / 1024),
    ksP50: ks.p50,
    ksP95: ks.p95,
    loadP95: load.p95,
    swapP50: swap.p50,
    swapP95: swap.p95,
    frozen: frozen.applicable ? (frozen.blocked ? 'held' : 'BROKEN') : '-',
    notes: ks.valid ? '' : 'KEYSTROKE MEASUREMENT INVALID',
  }
}

function table(rows: Row[]) {
  const cols = Object.keys(rows[0]) as Array<keyof Row>
  const w = cols.map(c => Math.max(String(c).length, ...rows.map(r => String(r[c]).length)))
  const line = (v: string[]) => v.map((x, i) => x.padEnd(w[i])).join('  ')
  console.log(line(cols.map(String)))
  console.log(w.map(x => '─'.repeat(x)).join('  '))
  for (const r of rows) console.log(line(cols.map(c => String(r[c]))))
}

const wd = new WebDriver()
await wd.start()
const rows: Row[] = []
for (const size of SIZES) {
  for (const s of STRATEGIES) {
    process.stdout.write(`  webkit ${s} @ ${size} ... `)
    try {
      rows.push(await measure(wd, s, size))
      console.log('ok')
    } catch (e: any) {
      console.log(`FAILED: ${e.message}`)
    }
  }
}
console.log(`\n=== M0 in webkit2gtk: ${TOTAL_WORDS} words ===`)
table(rows)
await wd.stop()
