/**
 * Runs the M4 scroll assertions against the page they live on, and reports the
 * result to whatever is hosting.
 *
 * # Why the verification runs on the product page
 *
 * The obvious design is a separate `verify.html`. It needs the same
 * `#scroller`/`#canvas` markup and the same stylesheet as the app, or it is not
 * verifying the shipping surface — it is verifying a lookalike.
 *
 * That mistake was made and caught here. The first verification page had no
 * `#canvas` element at all, so it died with `Cannot set properties of null` on a
 * line unrelated to what it was checking, and the only clue was that the error
 * named a DOM node nobody had put there. "Fixing" it by copying the app's markup
 * would have created two copies to keep in step — the exact arrangement that
 * produced the 45% height error described in DOCTRINE.md §3.
 *
 * So there is one page. `?verify=1` runs the assertions on it. A verification run
 * therefore measures the real DOM, the real stylesheet and the real boot path, and
 * there is nothing to drift.
 *
 * # Why the harness must be able to report its own failure
 *
 * A verification that reports success without having run the checks is the defect
 * this project keeps hitting. So:
 *
 *   - `min_expected` is declared up front and a short run is a hard failure. The
 *     first run of this harness reported `0/0 checks passed` and correctly exited
 *     non-zero, which is exactly the case that would otherwise read as a pass.
 *   - `ok` is re-derived in Rust from the individual results, not taken on trust.
 *   - the report is written even on failure.
 */

import { runScrollChecks, summarise, type CheckResult, type VerifyHost } from './verify.js'
import { hasBridge, identifyEngine, reportMounted, submitVerification } from './geometry-bridge.js'

/**
 * A floor, not an exact count: adding assertions must not require editing this,
 * but *losing* them must be caught. If `core/verify.ts` fails to load, this run
 * reports "ran 0 of at least N" and exits non-zero — a very different outcome from
 * "all passed".
 */
const MIN_EXPECTED_CHECKS = 21

/**
 * The floor with a bridge, which is higher because the checks are capability-gated.
 *
 * # Why one floor is not enough
 *
 * The cache-bound checks need a store to be bound against: they build a real document in
 * SQLite, because the cache may only drop a section's bytes when they can be fetched
 * again, and content the frontend supplied cannot be. In a browser there is no store, so
 * those checks do not run — correctly, since a fixture on the frontend would be
 * measuring itself.
 *
 * A single floor of 18 would let a Tauri run report "31 passed" while twelve of those
 * were the base set and the store-backed ones had silently not run, and it would let a
 * Chromium run report success having covered a bound it cannot test. So the floor is
 * per-host: a browser is held to the base count, and a bridge is held to the higher one,
 * which is exactly the number of checks that must be present for the run to mean what
 * it says.
 *
 * The consequence is deliberate and uncomfortable: adding a capability-gated check means
 * raising this, and forgetting to is a failing run rather than a quietly weaker one.
 */
// Raised by 4 for the search group. A floor is only worth having if it moves when the
// checks do: leaving it at 46 would let a Tauri run report success with the search
// checks silently absent, which is the failure mode this constant exists to prevent.
const MIN_EXPECTED_CHECKS_WITH_BRIDGE = 50

/**
 * How many animation frames to settle over.
 *
 * webkit2gtk schedules differently from Blink and there is no portable way to ask
 * how many frames a ResizeObserver callback will take. Six covers the
 * mount -> observe -> measure -> reposition chain on both engines. A check needing
 * more than that is asserting on timing rather than on behaviour.
 */
const SETTLE_FRAMES = 6

/**
 * Milliseconds to wait after a synthetic input, before settling.
 *
 * Not the "sleep until green" that DOCTRINE.md §2 forbids. It follows an explicit
 * `scrollToFraction`, and the assertions after it read positions rather than
 * outcomes. It exists because webkit2gtk delivers `scroll` events on a different
 * schedule than Blink and a scroll followed immediately by a read can observe the
 * pre-scroll position.
 */
const INPUT_SETTLE_MS = 80

export interface VerificationOutcome {
  engine: string
  engine_version: string | null
  results: CheckResult[]
  passed: number
  failed: number
  failures: string[]
  min_expected: number
  ran: number
  ok: boolean
  harness_error: string | null
}

/**
 * Yield for one frame, but never wait longer than `budgetMs`.
 *
 * # Why this is not `requestAnimationFrame`
 *
 * rAF fires when the compositor produces a frame, and a compositor that is not
 * producing frames — an occluded or unmapped window, a machine with no display, an
 * engine that throttles a background window — stops calling it entirely. The
 * harness then waits forever: no result, no report, and a run that has to be killed
 * by hand.
 *
 * That is what happened here. A `waitFor` built on rAF hung the first run after it
 * was introduced, and the symptom was an empty report file, which looks identical
 * to a harness that crashed before writing.
 *
 * Racing rAF against a timer makes progress unconditional. On a compositor that is
 * running, rAF wins and the wait is still frame-accurate; on one that is not, the
 * timer carries it and the harness still finishes.
 */
function nextFrame(budgetMs = 100): Promise<void> {
  return new Promise(resolve => {
    let done = false
    const finish = () => {
      if (done) return
      done = true
      resolve()
    }
    requestAnimationFrame(finish)
    // The floor. Without it a dead compositor hangs the run.
    setTimeout(finish, budgetMs)
  })
}

async function settle(frames = SETTLE_FRAMES): Promise<void> {
  for (let i = 0; i < frames; i++) {
    await nextFrame()
  }
  await new Promise(r => setTimeout(r, INPUT_SETTLE_MS))
}

/**
 * Render a thrown value as text, keeping the *message* even when the stack is thin.
 *
 * webkit2gtk returns a stack of exactly one frame for a cross-module throw, so
 * `e.stack` alone produced a report containing a file and a line number and no
 * explanation of what went wrong — the worst possible diagnostic, because it looks
 * like a real location and identifies no cause. The message is the part that
 * matters, so it leads.
 */
function describe(e: any): string {
  if (e instanceof Error) {
    const message = e.message || '(no message)'
    const stack = (e.stack ?? '').split('\n').slice(1).join('\n').trim()
    return stack ? `${message}\n${stack}` : message
  }
  if (typeof e === 'string') return e
  try {
    return JSON.stringify(e)
  } catch {
    return String(e)
  }
}

/**
 * Run the checks and report.
 *
 * `app` is passed in rather than read from `window`. An earlier version polled
 * `window.HOLO_SCROLL` for 60 frames and reported it missing — while the very
 * error message it constructed listed `HOLO_SCROLL` among the window's own keys.
 * Reached for the fix twice: waiting longer, then reading it back.
 *
 * The caller already has the object in hand. Passing it makes the dependency
 * explicit, and a null becomes an immediate, honest error rather than a
 * sixty-frame mystery.
 */
export async function runVerification(app: any): Promise<VerificationOutcome> {
  // Reveal the panel. Hidden in markup so a normal run shows no extra chrome.
  document.getElementById('verify-panel')?.removeAttribute('hidden')

  if (!app) {
    throw new Error('runVerification called without a scroller surface')
  }
  let engine = 'chromium-harness'
  let engineVersion: string | null = navigator.userAgent.match(/Chrome\/([\d.]+)/)?.[1] ?? null
  let harnessError: string | null = null
  let results: CheckResult[] = []
  const bridged = hasBridge()

  try {
    // From Rust, never from the user agent. webkit2gtk's UA is not a reliable
    // engine identity, and mis-attributing an engine is how a Chromium number ends
    // up quoted as a webkit2gtk one.
    //
    // `identifyEngine` is the single place that does this. It was duplicated here
    // and in `main.ts`, each with its own copy of the version regex, so the
    // two could report different versions for the same window.
    if (bridged) {
      const identity = await identifyEngine()
      engine = identity.engine ?? engine
      engineVersion = identity.version
      await reportMounted()
    }
    if (!app) throw new Error('HOLO_SCROLL missing: the scroller surface did not mount')

    const host: VerifyHost = {
      app,
      settle,
      inputSettleMs: INPUT_SETTLE_MS,
      hasBridge: bridged,
    }
    results = await runScrollChecks(host)
  } catch (e: any) {
    harnessError = describe(e)
  }

  const { passed, failed, failures } = summarise(results)
  const ran = results.length
  const minExpected = bridged ? MIN_EXPECTED_CHECKS_WITH_BRIDGE : MIN_EXPECTED_CHECKS
  const ok = harnessError === null && failed === 0 && ran >= minExpected

  const outcome: VerificationOutcome = {
    engine,
    engine_version: engineVersion,
    results,
    passed,
    failed,
    failures,
    min_expected: minExpected,
    ran,
    ok,
    harness_error: harnessError,
  }

  // Printed as well as reported. A verification that only speaks when it fails
  // cannot be told apart from one that never ran.
  const line = `[verify] ${engine}${engineVersion ? ` ${engineVersion}` : ''}: ${passed}/${ran} passed, ${failed} failed${ok ? '' : ' — FAILING'}`
  console.log(line)
  for (const f of results.filter(r => !r.pass)) {
    console.error(`[verify] FAIL ${f.name}: ${f.detail}`)
  }
  if (harnessError) console.error(`[verify] HARNESS ERROR: ${harnessError}`)

  render(outcome)

  if (bridged) {
    try {
      await submitVerification(outcome)
    } catch (e: any) {
      // Losing the report means the run cannot count. Recorded loudly rather than
      // swallowed, because a verification that silently failed to report is
      // indistinguishable from one that passed.
      console.error('[verify] could not hand the report to the shell', e)
    }
  }

  return outcome
}

/** Put the result on the page, so a human watching the window sees it too. */
function render(o: VerificationOutcome): void {
  const status = document.getElementById('verify-status')
  if (status) {
    status.textContent =
      `${o.engine}: ${o.passed}/${o.ran} passed, ${o.failed} failed` + (o.harness_error ? ` — harness error` : '')
    status.style.color = o.ok ? '#14532d' : '#7f1d1d'
  }
  const detail = document.getElementById('verify-detail')
  if (detail) {
    // Detail only on failures. Showing a passing row's `detail` next to a green
    // PASS is actively misleading: those strings are written to explain a *failed*
    // assertion ("section 99 is at the top but only [...] is mounted"), so a
    // reader sees a FAIL-shaped sentence under PASS.
    detail.textContent =
      o.results
        .map(r => (r.pass ? `PASS  ${r.name}` : `FAIL  ${r.name}\n        ${r.detail ?? '(no detail)'}`))
        .join('\n') || '(no checks ran)'
  }
}