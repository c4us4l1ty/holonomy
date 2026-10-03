/**
 * Minimal WebKitWebDriver probe.
 *
 * The full webkit.ts battery times out during session setup or the first large
 * script call. This isolates which step hangs, with short timeouts, so the
 * problem can be identified rather than retried blindly.
 */
const DRIVER = 'http://localhost:4444'
const URL_ = process.env.HOLO_URL ?? 'http://localhost:5183/'

async function timed<T>(label: string, ms: number, fn: () => Promise<T>): Promise<T | null> {
  const t0 = Date.now()
  try {
    const r = await Promise.race([
      fn(),
      new Promise<never>((_, rej) => setTimeout(() => rej(new Error(`timeout ${ms}ms`)), ms)),
    ])
    console.log(`  ${label}: ok in ${Date.now() - t0}ms`)
    return r
  } catch (e: any) {
    console.log(`  ${label}: FAILED after ${Date.now() - t0}ms — ${e.message}`)
    return null
  }
}

const post = (path: string, body?: unknown) =>
  fetch(`${DRIVER}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  }).then(async r => ({ status: r.status, json: await r.json() }))

console.log('WebKitWebDriver probe')
console.log('======================')

const status = await timed('GET /status', 10_000, () =>
  fetch(`${DRIVER}/status`).then(r => r.json()),
)
console.log('  status:', JSON.stringify(status))

console.log('\ncreating session...')
const sess = await timed('POST /session', 60_000, () =>
  post('/session', {
    capabilities: {
      alwaysMatch: {
        'webkitgtk:browserOptions': {
          binary: '/usr/libexec/webkit2gtk-4.1/MiniBrowser',
        },
      },
    },
  }),
)
if (!sess) {
  console.log('\nSession creation failed or hung — that is the blocker.')
  process.exit(1)
}
console.log('  raw:', JSON.stringify(sess.json).slice(0, 400))

const sid = sess.json?.value?.sessionId
if (!sid) {
  console.log('\nNo sessionId in response. Driver may not support this capability shape.')
  process.exit(1)
}

const cmd = (path: string, body?: unknown) =>
  fetch(`${DRIVER}/session/${sid}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  }).then(async r => ({ status: r.status, json: await r.json() }))

console.log('\nnavigating to harness...')
const nav = await timed('POST /url', 60_000, () => cmd('/url', { url: URL_ }))
if (nav) console.log('  nav status:', nav.status)

console.log('\npolling for harness...')
for (let i = 0; i < 20; i++) {
  const r = await timed(`  poll ${i}`, 15_000, () => cmd('/execute/sync', {
    script: 'return typeof window.HOLO',
    args: [],
  }))
  if (r) {
    console.log('  value:', JSON.stringify(r.json).slice(0, 200))
    if (JSON.stringify(r.json).includes('object')) break
  }
  await new Promise(res => setTimeout(res, 1000))
}

console.log('\nsmall init (20k words)...')
const init = await timed('init', 120_000, () => cmd('/execute/sync', {
  script: 'return window.HOLO.init({targetWords: 20000, wordsPerSection: 1500, strategy: "C"})',
  args: [],
}))
if (init) console.log('  value:', JSON.stringify(init.json).slice(0, 300))

await timed('DELETE session', 20_000, () =>
  fetch(`${DRIVER}/session/${sid}`, { method: 'DELETE' }).then(r => r.json()),
)
console.log('\ndone')
