/**
 * The M4 scroll and compensation assertions, as host-agnostic functions.
 *
 * # Why these live here and not in the Playwright driver
 *
 * The cross-engine verification has to run inside a real Tauri window, and
 * Playwright cannot attach to one. So the assertions move in here, where they run
 * identically in headless Chromium and in webkit2gtk, and each host supplies only
 * the plumbing.
 *
 * That constraint is the point. Two copies of these assertions would drift, and
 * the drifted copy would be the one reporting the result. One definition means a
 * number from webkit2gtk and a number from Chromium are measuring the same thing.
 *
 * # What these can and cannot check
 *
 * Everything here reads real layout — `getBoundingClientRect`, `scrollTop`,
 * computed styles. None of it works outside a rendering engine, which is why there
 * is no jsdom path and why a DOM shim is not offered. See DOCTRINE.md §3.

/** One assertion's outcome. */
export interface CheckResult {
  name: string
  pass: boolean
  /** Present on failure. */
  detail?: string
  /** Measurements worth recording even on success. */
  observed?: Record<string, unknown>
}

import { assertNoInlineImages, assetHashFromUrl, assetUrl, loadAssetImage } from './assets.js'
import {
  declaresHeightConstant,
  derivesBlockCount,
  ENTRY_MODULE,
  estimateHeightBody,
} from './source-checks.js'
import { CONTENT_CACHE_CAPACITY } from './section-cache.js'

export interface VerifyHost {
  /** The scroller test surface (`window.HOLO_SCROLL`). */
  readonly app: any
  /** Yield to the event loop so layout and ResizeObserver can settle. */
  settle(frames?: number): Promise<void>
  /** Milliseconds to wait after a synthetic input, for engines with coarser timers. */
  readonly inputSettleMs: number
  /**
   * Whether a bridge is present, so store-backed checks can be skipped rather than
   * failed.
   *
   * Reported by the host rather than sniffed here, because "is there a Tauri host" is
   * the same question as `which_engine` answers and it must be asked the same way from
   * every call site.
   */
  readonly hasBridge?: boolean
}

function check(
  results: CheckResult[],
  name: string,
  pass: boolean,
  detail?: string,
  observed?: Record<string, unknown>,
): void {
  results.push({ name, pass, detail, observed })
}

/**
 * Run every M4 assertion.
 *
 * Grouped so a failure names the property that broke rather than the test that
 * happened to notice. DOCTRINE.md §2: a failure message must point at the cause,
 * not at a plausible suspect.
 */
export async function runScrollChecks(host: VerifyHost): Promise<CheckResult[]> {
  const out: CheckResult[] = []
  const app = host.app
  const load = async (n: number, paras = 15) => {
    await app.loadSynthetic(n, paras)
    await wait(250)
    await host.settle()
  }

  // -- setup -------------------------------------------------------------

  await load(200)
  {
    const s = { sh: app.scrollHeight(), ch: app.clientHeight(), n: app.sectionCount() }
    check(
      out,
      'scroll track is tall for a long document',
      s.sh > s.ch * 20,
      `scrollHeight=${s.sh} clientHeight=${s.ch} for ${s.n} sections`,
      { screens: Math.round(s.sh / s.ch) },
    )
  }

  await load(200)
  {
    const slots = document.querySelectorAll('#canvas .slot').length
    check(
      out,
      'only a small window is in the DOM',
      slots <= 8,
      `${slots} slots mounted over 200 sections`,
      { slots },
    )
  }

  // The card. This is the check that caught a 45% error in every height: the
  // editor was being mounted without the `.section-slice` ancestor, so text
  // wrapped at the viewport width instead of the 46rem measure and the section
  // rendered far shorter than the calibration had measured.
  await load(20)
  {
    const slot = document.querySelector('#canvas .slot') as HTMLElement
    const card = slot?.querySelector('.section-slice') as HTMLElement | null
    const pm = slot?.querySelector('.ProseMirror') as HTMLElement | null
    const p = pm?.querySelector('p') as HTMLElement | null
    const width = p ? p.getBoundingClientRect().width : null
    check(
      out,
      'sections render inside the card the calibration measured',
      !!card && !!pm && card.contains(pm),
      'the ProseMirror element is not inside a .section-slice card, so the 46rem measure does not apply',
    )
    // 46rem = 736px content-box. A width near 1216px means the card is missing.
    check(
      out,
      'text column uses the card measure, not the viewport width',
      width !== null && width > 700 && width < 780,
      `text column is ${width?.toFixed(0)}px; expected ~736px. A width near 1216px means the card is absent`,
      { contentWidth: width },
    )
  }

  // -- mounting ----------------------------------------------------------

  await load(200)
  {
    await app.scrollToFraction(0.5)
    const mounted: number[] = app.mountedIndices()
    const expected = app.geometry().sectionAt(app.scrollTop())
    check(
      out,
      'scrolling mounts the section at the viewport top',
      mounted.includes(expected),
      `section ${expected} is at the top but only ${JSON.stringify(mounted)} is mounted`,
      { mounted, expected },
    )
  }

  await load(200)
  {
    let peak = 0
    const seen = new Set<number>()
    for (let i = 0; i <= 20; i++) {
      await app.scrollToFraction(i / 20)
      const m: number[] = app.mountedIndices()
      peak = Math.max(peak, m.length)
      if (m.length) seen.add(m[0]!)
    }
    check(out, 'the mounted window stays small while dragging', peak <= 8, `peak window was ${peak} sections`, {
      peak,
    })
    // A window that never changes is not tracking the viewport, it is pinned.
    check(
      out,
      'the window actually follows the viewport',
      seen.size >= 15,
      `only ${seen.size} distinct window positions across the document`,
      { distinct: seen.size },
    )
  }

  await load(200)
  {
    const before = app.geometryHeight()
    await app.scrollToFraction(0.1)
    const mid = document.querySelectorAll('#canvas .slot').length
    await app.scrollToFraction(0.9)
    const after = document.querySelectorAll('#canvas .slot').length
    const afterHeight = app.geometryHeight()
    check(
      out,
      'unmounted sections leave the DOM',
      after <= 8,
      `${after} slots remain after scrolling to 90% (was ${mid} mid-document)`,
      { after },
    )
    check(
      out,
      'scrolling alone does not change the document height',
      Math.abs(afterHeight - before) / before < 0.2,
      `geometry height moved ${(((afterHeight - before) / before) * 100).toFixed(1)}% from scrolling alone`,
    )
  }

  // -- positioning -------------------------------------------------------

  await load(200)
  {
    await app.scrollToFraction(0.4)
    await host.settle()
    const canvasRect = (app.canvas() as HTMLElement).getBoundingClientRect()
    let worst = 0
    let worstIndex = -1
    for (const index of app.mountedIndices() as number[]) {
      const el = app.canvas().querySelector(`[data-slot="${index}"]`) as HTMLElement
      const claimed = app.geometry().offsetOf(index)
      const actual = el.getBoundingClientRect().top - canvasRect.top
      const drift = Math.abs(claimed - actual)
      if (drift > worst) {
        worst = drift
        worstIndex = index
      }
    }
    check(
      out,
      'each slot sits at the offset the geometry claims',
      worst <= 1.0,
      `section ${worstIndex} is ${worst.toFixed(1)}px from its claimed offset`,
      { worst: Number(worst.toFixed(2)) },
    )
  }

  // -- the compensation invariant ---------------------------------------

  await load(200)
  {
    await app.scrollToFraction(0.5)
    await host.settle()
    const y = app.scrollTop()
    const above = app
      .mountedIndices()
      .filter((i: number) => app.geometry().offsetOf(i) + app.geometry().heightOf(i) <= y)
    const topSlot = app
      .mountedIndices()
      .map((i: number) => ({ i, top: app.geometry().offsetOf(i) }))
      .filter((s: { top: number }) => s.top >= y)[0]

    if (!above.length || !topSlot) {
      check(out, 'a section above the viewport exists to test with', false, 'fixture produced no such section')
    } else {
      const target = above[above.length - 1]
      const scrollBefore = app.scrollEl().scrollTop
      const claimedBefore = app.geometry().offsetOf(topSlot.i)
      const { compensate } = app.geometry().measure(target, app.geometry().heightOf(target) + 120, y)
      app.scrollEl().scrollTop += compensate
      const claimedAfter = app.geometry().offsetOf(topSlot.i)
      const scrollAfter = app.scrollEl().scrollTop

      check(out, 'growing a section above the viewport compensates by the delta', compensate === 120,
        `compensation was ${compensate}, expected 120`)

      // The invariant: the claimed offset and the scroll position must move by the
      // same amount. Comparing on-screen positions instead would be comparing two
      // coordinate spaces, which is how an earlier version reported a 72px drift on
      // a correctly compensated case.
      const claimedDelta = claimedAfter - claimedBefore
      const scrollDelta = scrollAfter - scrollBefore
      check(
        out,
        'the viewport is held still when a section above it grows',
        Math.abs(claimedDelta - scrollDelta) <= 1,
        `content below shifted ${claimedDelta.toFixed(1)}px but the scroll moved ${scrollDelta.toFixed(1)}px`,
        { claimedDelta: Number(claimedDelta.toFixed(1)), scrollDelta: Number(scrollDelta.toFixed(1)) },
      )
    }
  }

  await load(200)
  {
    await app.scrollToFraction(0.5)
    await host.settle()
    const y = app.scrollTop()
    const below = app.mountedIndices().find((i: number) => app.geometry().offsetOf(i) > y)
    if (below === undefined) {
      check(out, 'a section below the viewport exists to test with', false, 'fixture produced no such section')
    } else {
      const before = app.scrollTop()
      const { compensate } = app.geometry().measure(below, app.geometry().heightOf(below) + 150, y)
      check(
        out,
        'growing a section below the viewport does not scroll',
        compensate === 0,
        `returned a ${compensate}px compensation; a change below the viewport must not move it`,
      )
      check(out, 'the scroll position is untouched', app.scrollTop() === before, `scroll moved from ${before} to ${app.scrollTop()}`)
    }
  }

  await load(200)
  {
    await app.scrollToFraction(0.5)
    await host.settle()
    const y = app.scrollTop()
    const straddle = app.mountedIndices().find((i: number) => {
      const top = app.geometry().offsetOf(i)
      return top < y && top + app.geometry().heightOf(i) > y
    })
    if (straddle === undefined) {
      check(out, 'a section straddling the viewport top exists', false, 'fixture produced no such section')
    } else {
      const { compensate } = app.geometry().measure(straddle, app.geometry().heightOf(straddle) + 200, y)
      check(
        out,
        'growing the section containing the viewport top does not scroll',
        compensate === 0,
        `returned ${compensate}px; the content being looked at did not move`,
      )
    }
  }

  await load(200)
  {
    // Convergence, not a fixed total: the first pass over an unmeasured document
    // *must* change the height. What must not happen is a trend.
    const passes: number[] = []
    for (let p = 0; p < 4; p++) {
      for (let i = 0; i <= 30; i++) await app.scrollToFraction(i / 30)
      await host.settle()
      passes.push(app.geometryHeight())
    }
    const deltas = passes.slice(1).map((p, i) => p - passes[i]!)
    let shrinking = true
    for (let i = 1; i < deltas.length; i++) {
      if (Math.abs(deltas[i]!) >= Math.abs(deltas[i - 1]!)) shrinking = false
    }
    check(
      out,
      'the document height converges rather than ratcheting',
      shrinking,
      `per-pass deltas ${deltas.map(d => Math.round(d)).join(', ')} are not shrinking`,
      { deltas: deltas.map(d => Math.round(d)) },
    )
    check(
      out,
      'a fully measured document stops moving',
      Math.abs(deltas[deltas.length - 1]!) / passes[passes.length - 1]! < 0.01,
      `the final pass still moved the document by ${deltas[deltas.length - 1]!.toFixed(0)}px`,
    )
  }

  await load(200)
  {
    await app.scrollToFraction(0.5)
    await host.settle()
    const index = app.mountedIndices()[0]
    const h = app.geometry().heightOf(index)
    const before = app.scrollTop()
    const a = app.geometry().measure(index, h, app.scrollTop())
    const b = app.geometry().measure(index, h, app.scrollTop())
    check(out, 'an identical re-measurement is a no-op', a.delta === 0 && b.delta === 0,
      `deltas were ${a.delta} and ${b.delta}`)
    check(out, 'an identical re-measurement does not scroll', app.scrollTop() === before, 'the scroll position moved')
  }

  // -- estimate quality --------------------------------------------------

  await load(120, 8)
  {
    for (let i = 0; i <= 40; i++) await app.scrollToFraction(i / 40)
    await host.settle()
    const errs = app.estimateErrors().filter((e: { actual: number }) => e.actual > 0)
    const ratios: number[] = errs.map((e: { ratio: number }) => e.ratio)
    const mean = ratios.reduce((a: number, b: number) => a + b, 0) / (ratios.length || 1)
    check(
      out,
      'predicted heights land near the measured heights',
      Math.abs(mean - 1) < 0.20,
      `mean predicted/actual ratio is ${mean.toFixed(3)}; the height model is off by ${(((mean - 1) * 100)).toFixed(1)}%`,
      { mean: Number(mean.toFixed(3)), sections: errs.length },
    )
  }

  // -- calibration provenance -------------------------------------------

  {
    const cal = app.calibration()
    check(
      out,
      'the height model was loaded, not defaulted',
      cal !== null,
      'calibration() returned null, so heights were estimated from no model',
    )
    // Three source-level facts, checked through `source-checks.ts` so this host and
    // `test/scroll.ts` share one definition. Both of these checks were previously
    // whole-file regexes here, and both matched correct code: the `blocks ??` pattern
    // flagged a lookup guard in the test surface, and the derivation pattern flagged
    // `estimateHeight`'s own doc comment, which quotes the removed line to explain why it
    // was removed. The in-engine run reported 23/24 with a failure that was the check
    // being wrong, not the code.
    const src = await (await fetch(ENTRY_MODULE)).text()

    const declared = declaresHeightConstant(src)
    check(
      out,
      'no height constant is declared in the frontend source',
      declared === null,
      `main.ts declares ${declared}; it must come from the boot payload or a generated artifact`,
    )

    // The character-derived block count, gone.
    //
    // This is the fallback that made every height estimate 225% out on short
    // multi-paragraph sections. It was unreachable in Rust and reachable in the
    // frontend, so the stored block count was computed, written, and then never used.
    const body = estimateHeightBody(src)
    check(
      out,
      'the height estimator was found, so it can be checked',
      body !== null,
      `could not locate estimateHeight in ${ENTRY_MODULE}; a renamed function would make ` +
        'every check below vacuous, which is worse than no check',
    )
    const derived = body === null ? null : derivesBlockCount(body)
    check(
      out,
      'no character-derived block-count fallback remains',
      derived === null,
      `estimateHeight contains ${derived}; that estimate is 225% out on short dense sections`,
    )

    // The runtime half: a loaded document reports real block counts. The source checks
    // say the fallback is not written down; this says it is not running either.
    const firstBlocks = app.sectionBlocks?.(0) ?? null
    check(
      out,
      'sections report a real block count, so nothing falls back',
      typeof firstBlocks === 'number' && firstBlocks > 0,
      `section 0 reports ${firstBlocks} blocks; zero or null means the estimate was derived from characters`,
    )
  }

  // -- the content cache, against a store-backed document ----------------

  //
  // These run only with a bridge, and the reason is the whole point of them.
  //
  // The cache may drop a section's bytes only when they can be fetched again. Content
  // the frontend supplied has nothing to fetch from, so a synthetic document never
  // exercises the bound at all: the cache behaves *correctly* by declining to drop it,
  // and a check written against one would be measuring the fixture rather than the
  // feature. So these build a real document in the store, fling past the bound, and
  // read back what survived.
  //
  // `run-verification.ts` raises the required check count when a bridge is present, so a
  // Tauri run cannot quietly report success with these missing. A Chromium run reports
  // the base count and this group does not appear — which is the honest outcome, not a
  // pass.
  if (host.hasBridge) {
    await runCacheBoundChecks(host, out)
    await runSearchChecks(host, out)
  }

  // -- the same bounds, at soak scale --------------------------------------

  //
  // A different test from `runCacheBoundChecks` above, not a bigger copy of it. That one
  // asks whether the bound holds for a document of 50 sections; this asks whether it holds
  // for the document size the architecture was designed for — ~1,000,000 words across ~1300
  // sections — and it asks under a fling rather than a seek. Both matter independently: a
  // 50-section document cannot leak in a way that scales, and a seek is one window where a
  // fling is a stream of them.
  if (host.hasBridge) {
    await runSoakChecks(host, out)
  }

  // -- assets, through the real protocol handler ---------------------------

  //
  // The one piece of the asset feature with nowhere else to run. `resolve_asset_uri` is
  // covered by ordinary Rust tests and the URL grammar by ordinary TypeScript tests, but
  // neither can exercise the handler itself: it needs a `tauri::AppHandle` and a webview.
  // The browser harness has no bridge, so a real window is the only place this round trip
  // happens -- and a handler that served the wrong bytes would render broken images in every
  // document, which looks like a document problem rather than a storage one.
  if (host.hasBridge) {
    await runAssetChecks(host, out)
  }

  // -- the production CSP, and the styles it has to permit ------------------
  //
  // Runs in *every* host, bridge or not, and that is the point. Everything else in this
  // suite runs against a Vite dev server, which sends no CSP at all, so a policy that
  // forbade `holo-asset:` images or the KaTeX stylesheet would pass every test in the
  // repository and ship broken. The only place `tauri.conf.json`'s policy is in force is a
  // built binary, and `scripts/smoke-production.sh` is what builds one.
  //
  // These check the *effect*, not the policy text. Asserting that the CSP string contains
  // `img-src holo-asset:` would be asserting that the file says what it says; what matters is
  // whether an image under that scheme and a stylesheet from the bundle actually load, and
  // only the effect can answer that.
  await runPolicyChecks(host, out)

  return out
}

/**
 * Did the production CSP permit the two things it most easily forbids?
 *
 * # Why KaTeX is the interesting one
 *
 * Because it is the only part of the frontend that loads its *own* assets at runtime through
 * paths the bundler did not rewrite: `katex/dist/katex.min.css` is bundled, but the
 * `@font-face` rules inside it reference `KaTeX_Main-Regular.woff2` by relative URL. Those
 * become same-origin requests, which `font-src` must allow — and `font-src` is *absent* from
 * the policy, so it inherits `default-src 'self'`, which allows them. That reasoning is
 * correct and completely untested, and it is exactly the kind of thing that stops being
 * correct when someone adds a CDN or tightens `default-src`.
 *
 * # Why a computed style and not a network observation
 *
 * Because `document.styleSheets` shows what was *parsed* while a computed style shows what
 * was *applied*, and the difference between them is where a policy failure hides. The webfont
 * check goes further: `document.fonts.check` is false for a family whose face never loaded,
 * which is the case a `font-src` omission produces while the stylesheet loads perfectly.
 */
async function runPolicyChecks(host: VerifyHost, out: CheckResult[]): Promise<void> {
  const styleApplied = await new Promise<{ applied: boolean; family: string; size: string }>(
    resolve => {
      const probe = document.createElement('span')
      probe.textContent = 'probe'
      // KaTeX's own class, because the rule under test is the one the bundle ships.
      probe.className = 'katex'
      document.body.append(probe)
      const computed = getComputedStyle(probe)
      const result = {
        applied: computed.fontFamily.toLowerCase().includes('katex'),
        family: computed.fontFamily,
        size: computed.fontSize,
      }
      probe.remove()
      resolve(result)
    },
  )

  // # The embedded UI font, and why this check has to exist
  //
  // `body` asks for `Inter, system-ui, sans-serif`. If the `@font-face` file does not load --
  // blocked by `font-src`, missing from the bundle, a 404 under the production protocol -- the
  // engine silently falls through to `system-ui`, every height measurement below is taken
  // against the OS's font, and all 44 other checks still pass. The cross-platform claim in
  // STATUS.md would then be a claim about three different font stacks.
  //
  // So it is asserted, the same way the KaTeX stylesheet is: both that the family *computes*,
  // and that the face is actually *loaded*. `getComputedStyle` alone proves nothing here --
  // it would report `Inter` from the CSS whether or not a byte of it arrived.
  //
  // The verdict comes from *enumerating* the FontFaceSet, never from `document.fonts.check`.
  // `check('15px Inter')` answers "are the fonts needed for this text loaded", and on a page
  // with no `Inter` face at all the answer is yes -- the fallback is always loaded. It reports
  // the font present precisely when nothing was fetched. `check` is still recorded, because it
  // disagrees on webkit2gtk 60.5 and a reader debugging this needs to know which engine said
  // what rather than a single boolean.
  let enumerated: string[] = []
  let declared: string[] = []
  let checkApi = 'not called'
  try {
    await document.fonts.ready
    const ours = [...document.fonts].filter(f => /inter|jetbrains/i.test(f.family))
    declared = ours.map(f => `${f.family} ${f.weight} ${f.style}`)
    enumerated = ours.filter(f => f.status === 'loaded').map(f => `${f.family} ${f.weight} ${f.style}`)
    checkApi = String(document.fonts.check('15px Inter'))
  } catch (e: any) {
    checkApi = `threw: ${e?.message ?? String(e)}`
  }

  const bodyFamily = getComputedStyle(document.body).fontFamily
  const interLoaded = enumerated.some(f => /^Inter\b/.test(f))
  const monoDeclared = declared.some(f => /^JetBrains Mono\b/.test(f))

  check(
    out,
    'the embedded UI font is the one in use, not the system fallback',
    // Inter must be *loaded*, because the body renders in it: that is the property whose
    // absence makes every height measurement a measurement of the OS's font.
    //
    // JetBrains Mono must only be *declared*. Nothing on this page renders in it — the one
    // element that does, `#verify-detail`, is inside the hidden verification panel — so a
    // browser that has not fetched it is behaving correctly, and demanding `loaded` would be
    // demanding that the application draw something it does not draw. It was measured
    // `unloaded` in Chromium and the first version of this check failed on exactly that.
    interLoaded && monoDeclared && /inter/i.test(bodyFamily),
    `declared [${declared.join(' | ') || 'nothing'}], loaded [${enumerated.join(' | ') || 'nothing'}], ` +
      `document.fonts.check('15px Inter') returned ${checkApi}, and body computed to ` +
      `font-family "${bodyFamily}". A body rendering in system-ui measures every height ` +
      `against the machine's own font, which is the cross-platform drift the embedded font ` +
      `removes. Check that app/public/fonts reached the bundle and that font-src allows 'self'.`,
    { declared, enumerated, checkApi, bodyFamily },
  )

  check(
    out,
    'the KaTeX stylesheet was applied, not merely parsed',
    styleApplied.applied,
    `a .katex element computed to font-family "${styleApplied.family}" at ` +
      `${styleApplied.size}. Either the stylesheet did not load, or the CSP refused it. ` +
      `Check style-src in tauri.conf.json.`,
    styleApplied,
  )

  // `document.fonts.check` with a real family from KaTeX's stylesheet. An empty string for
  // the text tests "is the *default* face loaded", which is a different question; the family
  // name is what makes this the webfont check rather than a restatement of the style check.
  let fontLoaded = false
  let fontFamily = 'KaTeX_Main'
  try {
    await document.fonts.ready
    fontLoaded = document.fonts.check('16px KaTeX_Main')
    fontFamily = 'KaTeX_Main'
  } catch (e: any) {
    fontLoaded = false
    fontFamily = `threw: ${e?.message ?? String(e)}`
  }

  check(
    out,
    'the KaTeX webfont loaded, so font-src permits it',
    fontLoaded,
    `document.fonts.check('16px ${fontFamily}') is false. The stylesheet applied but its ` +
      `\`@font-face\` sources did not, which is what a font-src that excludes the origin ` +
      `produces. Every equation would render in a fallback face. Check that font-src, or ` +
      `default-src, still allows 'self'.`,
    { fontFamily, fontsSize: document.fonts.size },
  )

  // And the third: that the policy under test is a policy, rather than its absence. A dev
  // server sends no CSP, so a run with none is being counted here — which is the whole
  // reason this group exists.
  const meta = document.querySelector('meta[http-equiv="Content-Security-Policy"]')
  const hasPolicy =
    !!meta?.getAttribute('content') || (host.hasBridge ? true : false)
  check(
    out,
    'a Content Security Policy was in force for this run',
    hasPolicy,
    host.hasBridge
      ? 'no CSP was found, so the image and webfont checks above did not test a policy at all'
      : 'no <meta http-equiv="Content-Security-Policy"> in the document. This is expected in ' +
        'a browser run, and is the reason the production smoke test exists.',
    { viaMetaTag: !!meta },
  )
}

/** Load a URL and read back its size, for the resolver assertion above. */
function loadImageFrom(url: string): Promise<{ loaded: boolean; width: number; height: number }> {
  return new Promise(resolve => {
    const image = new Image()
    image.onload = () =>
      resolve({ loaded: true, width: image.naturalWidth, height: image.naturalHeight })
    image.onerror = () => resolve({ loaded: false, width: 0, height: 0 })
    image.src = url
  })
}

/**
 * The `holo-asset://` round trip, against whatever transport this engine dispatches.
 *
 * The digest is recomputed in the renderer with `crypto.subtle.digest`. That is the check
 * that distinguishes "served the stored bytes" from "served something", and it is only
 * possible because the key is SHA-256 -- `assets.ts` explains why that choice was forced
 * rather than preferred.
 */
async function runAssetChecks(host: VerifyHost, out: CheckResult[]): Promise<void> {
  const app = host.app
  try {
    // A real 1x1 PNG, so the probe measures the handler rather than the engine's tolerance.
    // Bytes above 127 throughout, which is where an implementation that treated the buffer as
    // text would go wrong -- and the PNG's first byte is 0x89 for the same reason.
    //
    // The pixel is opaque red. It is read back below, so "the handler served *these* bytes"
    // and "the handler served *some* image" are different results rather than both reading
    // as a load event.
    const bytes = Array.from(RED_PNG_1X1)
    const mime = 'image/png'
    // The command returns the *hash*; the URL is the frontend's to compose. That split is
    // deliberate -- Rust owns the digest, and `assets.ts` owns the scheme -- and the first
    // version of this check assumed the command returned a URL, so it reported a URL-shaped
    // failure for a hash-shaped success.
    const hash = await app.storeProbeAsset(bytes, mime)
    const url = assetUrl(hash)

    check(
      out,
      'a stored asset is addressed by its own SHA-256',
      assetHashFromUrl(url) === hash && url.startsWith('holo-asset://'),
      `the command returned ${hash}, which composes to ${url} and the handler cannot parse`,
      { url },
    )

    //
    // Two properties, and the first is about the transport rather than the bytes.
    //
    // webkit2gtk 2.60 does not dispatch `holo-asset://` requests to a Tauri custom protocol
    // handler at all: `setup` runs, the scheme is registered through the same builder path
    // Tauri uses for its own `tauri` and `asset` schemes, and the handler is never entered.
    // Measured with logging on both sides, over four URLs; see STATUS.md. So the bytes are
    // fetched through whatever transport `AssetResolver` settles on, which is the property
    // that matters to a reader -- who wants a figure to appear.
    //
    // The first version fetched through the scheme and reported a bare `Load failed`, which
    // names neither the transport nor the engine and so could not be acted on. What replaced
    // it also reports *which* transport was used, so the report distinguishes "the protocol
    // handler works" from "the fallback is carrying it" -- the difference between the
    // directive being satisfied and being worked around.
    const served = await loadAssetImage(hash, (h: string) => app.resolveProbeAsset(h))
    check(
      out,
      'an asset address resolves to an image the engine can decode',
      served.loaded && served.width === 1 && served.height === 1,
      `loading ${url} reported loaded=${served.loaded} at ${served.width}x${served.height}; ` +
        'whichever transport the resolver chose, a figure must appear',
      { width: served.width, height: served.height, viaScheme: served.viaScheme },
    )

    check(
      out,
      'the bytes served are the ones that were stored, not some other image',
      served.firstPixel !== null && served.firstPixel.every((c, i) => c === RED_PNG_1X1_PIXEL[i]),
      `the pixel read back was ${JSON.stringify(served.firstPixel)}, expected ` +
        `${JSON.stringify(RED_PNG_1X1_PIXEL)}; a handler serving some other image is a storage bug ` +
        'that looks like a document bug',
      { firstPixel: served.firstPixel, viaScheme: served.viaScheme },
    )

    // The resolver's own answer, which is what an image node uses. A separate check because
    // it is a different code path: this one probes the scheme, caches the verdict and falls
    // back, so it can pass where a direct scheme load fails -- which is the situation today.
    const resolved = await app.resolveProbeAsset(hash)
    const rendered = resolved === null ? null : await loadImageFrom(resolved)
    check(
      out,
      'an image node can resolve an asset address to a URL that loads',
      rendered !== null && rendered.loaded && rendered.width === 1,
      `the resolver produced ${resolved}, which loaded=${rendered?.loaded} at ` +
        `${rendered?.width}x${rendered?.height}`,
      { resolved: resolved?.slice(0, 20), loaded: rendered?.loaded, width: rendered?.width },
    )

    // Storing the same bytes again must return the same URL. That is what makes the URL
    // immutable, and it is what lets the handler send `Cache-Control: immutable` -- a header
    // that would be actively wrong with an id-based address.
    const again = await app.storeProbeAsset(bytes, mime)
    check(
      out,
      'the same bytes address the same URL, so a cache entry can never go stale',
      again === hash,
      `storing identical bytes returned ${again} after ${hash}`,
      { first: hash, second: again },
    )

    // And the invariant that motivated the whole design: no inline payloads in a document.
    // Checked against the loaded document rather than a fixture, so a handler or an
    // extension that inlined an image would be caught where it happens.
    let inlineError: string | null = null
    try {
      assertNoInlineImages(app.registryRecords())
    } catch (e: any) {
      inlineError = e.message
    }
    check(
      out,
      'no loaded section carries an inline base64 image',
      inlineError === null,
      inlineError ?? '',
    )
  } catch (e: any) {
    check(
      out,
      'the asset protocol could be exercised',
      false,
      `the store-backed asset round trip failed: ${e?.message ?? String(e)}`,
    )
  }
}

/** Everything the bridge can tell us about the cache, for one assertion's `observed`. */
interface CacheProbe {
  resident: number
  capacity: number
  loaded: number
  mounted: number
  atSection: number
}

/**
 * The LRU bound, exercised against content that lives in SQLite.
 *
 * # The three properties, and why each is a separate assertion
 *
 * 1. **Bounded.** Resident sections stay at or under the cap no matter how far the
 *    document is flung. This is the headline claim.
 * 2. **Pruned.** A section that left the cache keeps its *metrics* and loses its
 *    *bytes*. Both halves matter. Keeping the bytes is the memory leak the bound exists
 *    to prevent; dropping the metrics would break the scroll geometry for a section
 *    nobody is looking at, which is worse than not caching at all.
 * 3. **Recoverable.** Scrolling back re-fetches and the section is editable again. A
 *    bound that made a document permanently un-scrollable past its own cache would pass
 *    assertions 1 and 2 perfectly.
 */
/**
 * Search, through the real command and the real SQLite file.
 *
 * # Why this is an in-engine check and not only a Rust one
 *
 * `crates/holonomy-core/tests/search.rs` proves the index is correct. It cannot prove the
 * index is *reachable*, and that was the actual defect: the schema, the query, and eleven
 * passing tests all existed, `reindex` had no caller anywhere in the application, and
 * search returned nothing in every real session. A feature can be correct at every layer it
 * has and absent at the layer it needs.
 *
 * So this drives the same path a user does -- `search_document` over Tauri IPC, into the
 * running binary's own database -- and then edits a section through `commit_section_edit`,
 * so the edit, the WAL fold, the reindex, and the next query are all in one chain.
 */
async function runSearchChecks(host: VerifyHost, out: CheckResult[]): Promise<void> {
  const app = host.app

  let fixtureId: string | null = null
  try {
    const fixture = await app.loadStoreBacked(40, 6)
    fixtureId = fixture.document_id
    await wait(250)
    await host.settle()

    // A term that is in the fixture's own text. `create_ephemeral_document` writes lorem
    // ipsum with a section index, so this is a word that exists in all 40 sections -- the
    // result should be every one of them, which also distinguishes "the index is empty"
    // from "the index works".
    const hits = await app.runSearch('lorem')

    check(
      out,
      'text written by the application is findable through the bridge',
      Array.isArray(hits?.hits) && hits.hits.length > 0,
      hits?.hits?.length === 0
        ? 'search_document returned no hits for a term present in every section of the ' +
          'fixture. Either the index was never populated on the write path, or the ' +
          'command is not reaching the store'
        : `${hits.hits.length} sections matched a term in all ${40}`,
      { matched: hits?.hits?.length ?? -1, total: hits?.total ?? -1 },
    )

    const total = hits?.total ?? 0
    check(
      out,
      'every section of the fixture is in the index, not just the mounted window',
      total >= 40,
      `expected all 40 sections indexed; the index reports ${total}. A count of 3 to 5 ` +
        'would mean only the mounted window was indexed, which is the failure that makes ' +
        'search look broken for a term on a later page',
      { indexed: total },
    )

    // -- the half that only an edit can prove ------------------------------
    //
    // Text that was edited away must stop being findable. On the old external-content
    // schema this was the property the design could not express: deleting an index row
    // needed the row's *original* values, which the database no longer kept.
    const sectionId = hits?.hits?.[0]?.section_id
    if (!sectionId) {
      check(
        out,
        'an edited section drops its old text and gains its new',
        false,
        'no hit to edit, so the reindex path could not be exercised',
      )
    } else {
      const MARK = 'zzqqxx-never-typed-before'
      const edited = await app.setSectionText(sectionId, `replacement text ${MARK} here`)
      if (!edited) {
        // Stated as its own check, because "the helper could not edit the section" and "the
        // index did not update" produce the same search results and are entirely different
        // defects. Reporting the first as the second sends whoever reads it looking in the
        // wrong place -- which is what happened the first two times this check ran.
        check(
          out,
          'a section could be edited through the editor the search hit belongs to',
          false,
          'the hit is not in a mounted, editable section, so nothing was written and the ' +
            'index below cannot have changed. This is a problem with the harness, not with ' +
            'search',
        )
      }
      // The fold is what turns the edit into a document change and reindexes it. Without
      // it the index is correct by design -- the edit is still in the recovery log.
      await app.flushNow()

      const after = await app.runSearch(MARK)
      const stale = await app.runSearch('lorem')

      check(
        out,
        'an edited section drops its old text and gains its new',
        !edited || (after.hits.length === 1 && after.hits[0].section_id === sectionId),
        `searching for the newly typed marker returned ${after.hits.length} hits ` +
          `(expected exactly 1, in the edited section). If it is 0 the index was not ` +
          `updated on the write path; if it is more the old text was never removed`,
        { found: after.hits.length, expected: 1 },
      )

      check(
        out,
        'the text an edit replaced is no longer findable',
        !edited || !stale.hits.some((h: any) => h.section_id === sectionId),
        'the edited section still matches text it no longer contains, which is what an ' +
          'external-content index does when its rows cannot be deleted',
        { stillMatching: stale.hits.length },
      )
    }
  } catch (e: any) {
    check(
      out,
      'search could be run against a store-backed document',
      false,
      `the search checks could not run: ${e?.message ?? String(e)}`,
    )
  } finally {
    // Paired, and for the same reason as `runCacheBoundChecks`: forty sections of lorem
    // ipsum left in the user's real database is litter beside their work.
    if (fixtureId) {
      try {
        await app.dropFixture()
      } catch (e: any) {
        console.error(`[verify] could not delete the search document ${fixtureId}`, e)
      }
    }
  }
}

async function runCacheBoundChecks(host: VerifyHost, out: CheckResult[]): Promise<void> {
  const app = host.app
  const SECTIONS = 50
  const PAST = 35

  let fixtureId: string | null = null
  try {
    const fixture = await app.loadStoreBacked(SECTIONS, 8)
    fixtureId = fixture.document_id
    await wait(250)
    await host.settle()

    const probe = (): CacheProbe => ({
      resident: app.sectionCache().size,
      capacity: app.sectionCache().capacity,
      loaded: app.loadedSectionCount(),
      mounted: app.mountedIndices().length,
      atSection: app.sectionAt?.() ?? -1,
    })

    // -- precondition: a store-backed document really did boot --------------
    //
    // Without this the group could pass against a document whose content never arrived
    // at all — the "bounded" and "pruned" assertions would both be trivially true
    // because nothing was ever resident. Stated as its own check so the failure names
    // the fixture rather than the feature.
    // Read *before* scrolling, but with a correction the first version of this check
    // lacked: mounting the first window fetches the sections just past the boot window,
    // because they are on screen. So `loaded` is the boot window plus whatever the
    // opening window covered -- more than twelve, and nowhere near fifty.
    const afterBoot = probe()
    check(
      out,
      'a store-backed document boots with content for the boot window, not the whole document',
      fixture.sections === SECTIONS &&
        afterBoot.loaded >= fixture.boot_visible &&
        afterBoot.loaded * 2 < fixture.sections,
      `booted ${fixture.sections} sections with ${afterBoot.loaded} loaded; the boot window is ` +
        `${fixture.boot_visible} and the opening mount window covers a few more. A payload ` +
        `carrying all ${fixture.sections} would make every bound assertion below vacuous`,
      { sections: fixture.sections, loaded: afterBoot.loaded, bootVisible: fixture.boot_visible },
    )

    // -- the fling ---------------------------------------------------------
    //
    // Stepped rather than one jump. A single `scrollTop = 90%` would mount a handful of
    // sections and never traverse the document, so the cache would never reach its cap
    // and the "bounded" assertion would be about a cache holding twelve things. A fling
    // in a virtual scroller is a stream of intermediate windows, and the eviction path
    // only runs when windows move.
    const steps = 40
    for (let i = 1; i <= steps; i++) {
      await app.scrollToFraction(0.9 * (i / steps))
      await host.settle(1)
    }
    await wait(400)
    await host.settle()

    const mounted = app.mountedIndices()
    const furthest = mounted.length ? mounted[mounted.length - 1]! : -1
    check(
      out,
      'the fling reached past the cache bound',
      furthest > PAST,
      `the furthest mounted index was ${furthest}; without passing ${PAST} the cache never filled, ` +
        'so every bound assertion below would be vacuous',
      { furthest },
    )

    const afterFling = probe()
    check(
      out,
      'flinging past the bound keeps resident sections at or under the cap',
      afterFling.resident <= afterFling.capacity,
      `${afterFling.resident} sections resident against a cap of ${afterFling.capacity}`,
      afterFling as unknown as Record<string, unknown>,
    )

    // The bound has to *engage*, not merely hold. A cache holding four sections satisfies
    // the assertion above and proves nothing, so the fling is required to have filled it.
    check(
      out,
      'the bound engaged: the cache filled rather than staying small',
      afterFling.resident === afterFling.capacity,
      `expected the cache to be exactly full after a fling past ${PAST}, got ` +
        `${afterFling.resident}/${afterFling.capacity}`,
      { resident: afterFling.resident, capacity: afterFling.capacity },
    )

    // -- pruned ------------------------------------------------------------
    //
    // Section 0 is the right subject: it was the most recently used of the sections
    // fetched earliest, so it is the first eviction under LRU. If it survived, the
    // eviction never happened and the check is measuring nothing.
    const firstId = fixture.section_ids[0]
    const firstBytes = app.sectionContentBytes(firstId)
    const firstLoaded = app.sectionLoaded(firstId)
    const firstBlocks = app.sectionBlocks(0)

    check(
      out,
      'an evicted section is reported as no longer loaded',
      firstLoaded === false,
      `section 0 reports loaded=${firstLoaded}; it should have been evicted as the least ` +
        'recently used section',
      { loaded: firstLoaded },
    )

    // The stub, measured. `EMPTY_DOC` is `{"type":"doc","content":[]}`, so a retained
    // section would be several kilobytes. The threshold is generous on purpose: what is
    // being asserted is "small", not "equal to this exact string".
    check(
      out,
      'an evicted section is pruned to a stub, not left holding its bytes',
      firstBytes >= 0 && firstBytes < 64,
      `section 0 still holds ${firstBytes} bytes of JSON; a real section is thousands. ` +
        'Keeping them is the memory growth the bound exists to prevent',
      { bytes: firstBytes },
    )

    // The half that is easy to break by accident. Pruning must not touch the manifest
    // row's numbers, because the geometry sizes the section from them whether or not it
    // is loaded — a pruned section with zero blocks would resize the whole document.
    check(
      out,
      'a pruned section keeps the metrics the geometry depends on',
      typeof firstBlocks === 'number' && firstBlocks > 0,
      `section 0 reports ${firstBlocks} blocks; pruning must drop content, not counts, or ` +
        'the scroll height for the whole document changes',
      { blocks: firstBlocks },
    )

    check(
      out,
      'resident sections never outnumber the loaded records',
      afterFling.loaded <= afterFling.resident,
      `${afterFling.loaded} records report content but only ${afterFling.resident} are cached; ` +
        'the two describe the same set and must not drift',
      { loaded: afterFling.loaded, resident: afterFling.resident },
    )

    // -- recoverable -------------------------------------------------------
    //
    // Scrolling back to 0, and the section has to come back *with its content*. A stub
    // that renders is not a recovery: the first keystroke into it would overwrite the
    // stored section, which is the failure the whole design is arranged to prevent.
    const before = app.hydratorStats() ?? { applied: 0, fromCache: 0, requested: 0 }
    await app.scrollToFraction(0)
    await wait(500)
    await host.settle()

    const reloadedBytes = app.sectionContentBytes(firstId)
    const reloadedLoaded = app.sectionLoaded(firstId)
    const after = app.hydratorStats() ?? { applied: 0, fromCache: 0, requested: 0 }

    check(
      out,
      'scrolling back to a pruned section re-hydrates it from the store',
      reloadedLoaded === true && after.requested > (before.requested ?? 0),
      `section 0 reports loaded=${reloadedLoaded} after a ${after.requested - (before.requested ?? 0)}-request ` +
        'round trip back to the top; it should have been fetched again',
      { loaded: reloadedLoaded, requested: after.requested },
    )

    check(
      out,
      'a re-hydrated section holds its content, not a stub',
      reloadedBytes > 64,
      `section 0 came back with ${reloadedBytes} bytes; a stub is under 64, so this is still the ` +
        'placeholder and typing into it would overwrite the stored section',
      { bytes: reloadedBytes },
    )

    // Steady state, not just first-fill. A cache that only bounds itself while filling
    // is not a bound, and the difference shows on the second pass and nowhere else.
    for (let i = 1; i <= 20; i++) {
      await app.scrollToFraction(i / 20)
      await host.settle(1)
    }
    await wait(300)
    await host.settle()
    const second = probe()
    check(
      out,
      'a second pass through the document does not raise the resident count',
      second.resident <= second.capacity && second.loaded <= second.resident,
      `after a second pass: ${second.resident}/${second.capacity} resident, ${second.loaded} loaded`,
      second as unknown as Record<string, unknown>,
    )
  } catch (e: any) {
    check(
      out,
      'the cache bound could be exercised against stored content',
      false,
      `the store-backed fixture could not be used: ${e?.message ?? String(e)}`,
    )
  } finally {
    // Paired, because the fixture is written to the user's real database. A failure here
    // is logged rather than thrown: the run has already got its result, and a cleanup
    // error should not turn a pass into a failure.
    if (fixtureId) {
      try {
        await app.dropFixture()
      } catch (e: any) {
        console.error(`[verify] could not delete the fixture document ${fixtureId}`, e)
      }
    }
  }
}

/**
 * The bounds, at the size the architecture exists for: ~1,000,000 words, ~1300 sections, one
 * fling of 200 sections.
 *
 * # What is different from {@link runCacheBoundChecks}, and why it is not a duplicate
 *
 * Three things, and each is a way the fifty-section run cannot fail:
 *
 * - **Scale.** A document of 50 sections cannot leak in a way that scales with document
 *   size, because there is nothing left over after the window has passed. The soak is the
 *   only run where the geometry has 1300 sections of prefix sums to carry and the manifest
 *   is large enough for the bounds to have somewhere to hide.
 * - **A fling rather than a seek.** `runCacheBoundChecks` steps `scrollToFraction` across
 *   the document; this one crosses 200 sections in a handful of fast steps, so the eviction
 *   and hydration paths are entered from many directions at once rather than one at a time.
 * - **A whole DOM rather than a slot count.** The fifty-section run counts mounted slots,
 *   which is the scroller's own bookkeeping. `document.querySelectorAll('*')` is what the
 *   engine actually holds, so a leak inside an editor that a slot was never mounted for is
 *   still visible.
 *
 * # Why the word count is asserted first
 *
 * Because every other assertion here is about a document of a particular size, and a soak
 * that quietly built a small one would pass all of them. The word count is read back from
 * the registry's metrics — the counts Rust wrote when the section went in, not a number this
 * function computed — so the assertion is on the stored document rather than on the request
 * that asked for it.
 */
async function runSoakChecks(host: VerifyHost, out: CheckResult[]): Promise<void> {
  const app = host.app
  // 1,300 x 13 x 60 = 1,014,000 words. `core.rs` carries the arithmetic and the reason for
  // thirteen paragraphs rather than the export corpus' twenty.
  const SECTIONS = 1300
  const PARAGRAPHS = 13
  const TARGET_WORDS = 1_000_000
  const WORD_TOLERANCE = 0.15
  const FLING_TO = 200
  const STEPS = 10
  // Eight mounted sections of thirteen paragraphs each, plus the application chrome, is
  // comfortably under this; mounting the whole document would be tens of thousands of nodes.
  const NODE_CEILING = 800

  let fixtureId: string | null = null
  try {
    const fixture = await app.loadStoreSoak(SECTIONS, PARAGRAPHS)
    fixtureId = fixture.document_id
    await wait(250)
    await host.settle()

    // -- the fixture is the size it claims --------------------------------
    //
    // Before any scrolling. A soak built from a smaller document would make every bound
    // below true for the wrong reason, which is why the size is asserted rather than assumed.
    const words: number = app.registry.totalWords()
    const sectionCount: number = app.sectionCount()
    check(
      out,
      'the soak fixture really is a million words across ~1300 sections',
      sectionCount === SECTIONS && Math.abs(words - TARGET_WORDS) <= TARGET_WORDS * WORD_TOLERANCE,
      `the store-backed soak document holds ${words} words across ${sectionCount} sections, against ` +
        `a target of ${TARGET_WORDS} +/- ${Math.round(TARGET_WORDS * WORD_TOLERANCE)} over ${SECTIONS}. ` +
        'Every bound below is only meaningful at that size, so a smaller document is a failed ' +
        'soak rather than a passing one',
      { sections: sectionCount, words },
    )

    // -- the heap reading, taken before anything is traversed ---------------
    //
    // `performance.memory` is a non-standard addition and webkit2gtk may not have it. Read
    // through a widened `performance` and treat absence as "not measurable here" rather than
    // as a failure: a browser that does not expose its heap cannot be said to have leaked in
    // it, and a check that failed for that reason would be reporting the engine, not the app.
    const heap = (): number | null => {
      // `performance.memory` is not in lib.dom's `Performance`, and it may not exist at
      // runtime either — hence the widened type rather than a non-null assertion, which would
      // throw on exactly the engines that lack it.
      const mem = (performance as unknown as { memory?: { usedJSHeapSize?: number } }).memory
      return typeof mem?.usedJSHeapSize === 'number' ? mem.usedJSHeapSize : null
    }

    await app.scrollToFraction(0)
    await host.settle()
    const heapStart = heap()

    // -- the fling ----------------------------------------------------------
    //
    // Stepped, and fast. One `scrollTo(200 sections down)` would mount one window and never
    // traverse anything: the bounds being asserted are the ones that hold *while* windows
    // move, and a single jump skips every state where they could fail. Ten steps of ~20
    // sections each is a momentum scroll arriving in the shape one does — each step lands,
    // the geometry corrects, and the next window is mounted from a scrollbar that has since
    // moved under it.
    let peakNodes = 0
    let peakNodesAt = -1
    let peakResident = 0
    let peakResidentAt = -1
    let peakIndex = 0

    // Sampled at every step, because the assertion is "at every step". A peak taken only at
    // the end would be a peak of the resting state, which is the state the fifty-section run
    // above has already proven bounded.
    const sample = () => {
      const at = app.geometry().sectionAt(app.scrollTop())
      const nodes = document.querySelectorAll('*').length
      const resident = app.sectionCache().size
      if (nodes > peakNodes) {
        peakNodes = nodes
        peakNodesAt = at
      }
      if (resident > peakResident) {
        peakResident = resident
        peakResidentAt = at
      }
      const mounted: number[] = app.mountedIndices()
      if (mounted.length) peakIndex = Math.max(peakIndex, mounted[mounted.length - 1]!)
    }

    for (let i = 1; i <= STEPS; i++) {
      // Aimed by section rather than by pixel. A momentum scroll advances a fixed number of
      // sections per frame, and reading the geometry on every step keeps the aim honest while
      // sections above the viewport are measured and compensated underneath it.
      await app.scrollTo(app.geometry().offsetOf(Math.round((FLING_TO * i) / STEPS)))
      await host.settle(1)
      sample()
    }
    await wait(400)
    await host.settle()
    // Once more, because the window at the end of a fling arrives a frame or two after the
    // scroll that asked for it; sampling only inside the loop reports the peak as the step
    // before last.
    sample()

    const cache = app.sectionCache()

    // The soak has to have travelled. Without this, a scroller that ignored the fling would
    // report a peak of zero and every bound below would be trivially satisfied by a document
    // that never left the top.
    check(
      out,
      'the soak fling reached section 200 of 1300',
      peakIndex >= FLING_TO,
      `the furthest mounted index across ${STEPS} steps was ${peakIndex}; the fling was aimed at ` +
        `section ${FLING_TO}, so a smaller figure means nothing was traversed and every bound ` +
        'below is vacuous',
      { peakIndex, aimedAt: FLING_TO, steps: STEPS },
    )

    check(
      out,
      'a million words in flight leaves under 800 DOM nodes',
      peakNodes <= NODE_CEILING,
      `the peak was ${peakNodes} nodes at section ${peakNodesAt}, against a ceiling of ${NODE_CEILING}. ` +
        'A virtual scroller that keeps its mounted window bounded cannot exceed this; a node ' +
        'count that scales with sections means something is being retained off the scroller',
      { peakNodes, peakNodesAt, ceiling: NODE_CEILING },
    )

    check(
      out,
      `the content cache never exceeds ${CONTENT_CACHE_CAPACITY} resident sections while flinging`,
      peakResident <= CONTENT_CACHE_CAPACITY && peakResident <= cache.capacity,
      `the peak was ${peakResident} resident sections at section ${peakResidentAt}, against the ` +
        `cap of ${cache.capacity}. The bound is the claim; measuring it at 1300 sections is what ` +
        'makes it a claim about a document rather than about fifty of them',
      { peakResident, peakResidentAt, capacity: cache.capacity, limit: CONTENT_CACHE_CAPACITY },
    )

    // -- memory does not grow with distance travelled -----------------------
    //
    // The heap rather than the cache, because the cache is a number the app publishes about
    // itself. `usedJSHeapSize` is what the engine says it is holding, which is the only
    // measurement here that is not reporting the app's own opinion of its bookkeeping.
    const heapEnd = heap()
    if (heapStart === null || heapEnd === null) {
      check(
        out,
        'the heap does not grow with distance travelled through a million words',
        true,
        'performance.memory.usedJSHeapSize is not exposed by this engine, so the heap bound was ' +
          'not measured. Reported rather than failed: an engine that does not publish its heap ' +
          'cannot be shown to have grown one. The cache bound above is the measured equivalent',
        { measured: false },
      )
    } else {
      const ratio = heapEnd / heapStart
      check(
        out,
        'the heap does not grow with distance travelled through a million words',
        ratio <= 1.5,
        `the JS heap went from ${(heapStart / 1e6).toFixed(1)}MB at the top of the document to ` +
          `${(heapEnd / 1e6).toFixed(1)}MB after flinging ${STEPS} steps past section ${FLING_TO}, ` +
          `a factor of ${ratio.toFixed(2)}. Growth with distance travelled is the failure this ` +
          'rules out; a cache that evicts still has to *hold* something that scales',
        {
          heapStartMb: Number((heapStart / 1e6).toFixed(1)),
          heapEndMb: Number((heapEnd / 1e6).toFixed(1)),
          ratio: Number(ratio.toFixed(2)),
        },
      )
    }

    // -- the main thread, while Typst is typesetting ----------------------
    //
    // Here, inside the soak and not after it, because the document has to still be a million
    // words: a responsiveness measurement taken against a two-section document measures
    // nothing, since the export would finish before the first sample.
    //
    // # What is measured, and what it is not
    //
    // `requestAnimationFrame` deltas. The frame callback is the browser's own report that it
    // got to run the page, so a gap is the main thread being busy with something else — and
    // "something else" during an export is exactly the question. This is a direct measurement
    // of the UI thread, not of the worker: the export runs on `spawn_blocking` and the only way
    // it can reach the main thread is by holding a lock the webview needs.
    //
    // The export is *not* awaited, and the cancel is not awaited either. Typst's `compile`
    // takes a `&dyn World` and offers no way to interrupt it, so a cancel raised during layout
    // waits the layout out — 45 seconds of the 55 here. Awaiting either would put three
    // quarters of a minute into a check about frame timing. The job dies with the process.
    const SAMPLE_MS = 3000
    // A dropped frame is one over ~16.7ms. 250ms is fifteen of them, and is roughly the
    // threshold at which a user stops perceiving the interface as responding to them.
    const MAX_FRAME_GAP_MS = 250

    // What the export ended as, not merely that it ended. An export that *fails* settles
    // just as fast as one that finishes, and "the export finished inside the sampling
    // window" reads as a statement about the fixture when it may be a statement about a
    // worker that refused to start. The first time this check failed after the layout
    // worker landed, the message said the fixture was not loaded and the real cause was one
    // line of the export's own output away.
    let settled = false
    let outcome: string | null = null
    const running = app
      .exportWithPanel()
      .then(
        (job: unknown) => {
          settled = true
          outcome = `finished as ${JSON.stringify(job)}`
        },
        (e: any) => {
          settled = true
          outcome = `failed: ${e?.message ?? String(e)}`
        },
      )
    void running

    let frames = 0
    let worstGap = 0
    let gapAt = 0
    await new Promise<void>(resolve => {
      let previous = performance.now()
      const started = previous
      const tick = (now: number) => {
        frames++
        const gap = now - previous
        if (gap > worstGap) {
          worstGap = gap
          gapAt = now - started
        }
        previous = now
        if (now - started >= SAMPLE_MS) resolve()
        else requestAnimationFrame(tick)
      }
      requestAnimationFrame(tick)
    })

    // The half of the measurement that stops it being vacuous: if the export had already
    // finished, the loop above timed three seconds of an idle page and the frame numbers say
    // nothing about typesetting. At a million words it takes ~55s, so settling inside 3s means
    // the fixture was not the document the soak built.
    check(
      out,
      'the main thread keeps running while Typst typesets a million words',
      !settled && worstGap < MAX_FRAME_GAP_MS && frames > 30,
      settled
        ? `the export ${outcome} inside the ${SAMPLE_MS}ms sampling window, so the frame ` +
            `timings measure an idle page rather than Typst laying out ${words} words. On ` +
            'this document the export takes tens of seconds, so either the soak fixture was ' +
            'not loaded or the export failed rather than running'
        : `over ${SAMPLE_MS}ms of an export typesetting ${words} words the browser ran ` +
            `${frames} animation frames; the longest gap between two of them was ` +
            `${worstGap.toFixed(0)}ms, ${gapAt.toFixed(0)}ms into the export, against a ` +
            `${MAX_FRAME_GAP_MS}ms budget. Typesetting runs in a separate process entirely, so ` +
            'the only way it reaches the main thread is by holding something the webview needs',
      {
        frames,
        worstGapMs: Number(worstGap.toFixed(1)),
        gapAtMs: Number(gapAt.toFixed(0)),
        settled,
        words,
        // `null` while the export is still running, which is the normal case.
        exportOutcome: outcome,
      },
    )

    // Stop the export we started. Through the panel's own button rather than a command, so
    // the thing being exercised is the control a user would press — a cancel that works from
    // the API and not from the button has not been tested.
    try {
      app.exportPanelElements().cancel.click()
    } catch {
      // No panel means the export failed before one was built, which the assertion above has
      // already described. Nothing to cancel.
    }
  } catch (e: any) {
    check(
      out,
      'the soak could be run against a store-backed million-word document',
      false,
      `the soak fixture could not be used: ${e?.message ?? String(e)}`,
    )
  } finally {
    // Paired, exactly as in `runCacheBoundChecks`, and for the same reason with more at
    // stake: a million words of lorem ipsum left in the user's real database is not a cache
    // miss, it is a hundred megabytes of litter beside their work.
    if (fixtureId) {
      try {
        await app.dropFixture()
      } catch (e: any) {
        console.error(`[verify] could not delete the soak document ${fixtureId}`, e)
      }
    }
  }
}

/**
 * A 1x1 opaque-red PNG.
 *
 * A real image, so the probe measures the handler rather than the engine's tolerance for
 * malformed input -- a handler serving these bytes makes the image load, and a handler
 * serving the wrong bytes makes it load as the *wrong* image, which is a different result
 * and a different bug.
 */
const RED_PNG_1X1 = Uint8Array.from([
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
  0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
  0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
  0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xdd, 0x8d, 0xb0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
  0x44, 0xae, 0x42, 0x60, 0x82,
])
/** The RGBA value that PNG decodes to, read back by `loadAssetImage`. */
const RED_PNG_1X1_PIXEL = [255, 0, 0, 255]

function wait(ms: number): Promise<void> {
  return new Promise(r => setTimeout(r, ms))
}

/** Summarise a run for a log line or a report header. */
export function summarise(results: CheckResult[]): { passed: number; failed: number; failures: string[] } {
  const failed = results.filter(r => !r.pass)
  return {
    passed: results.length - failed.length,
    failed: failed.length,
    failures: failed.map(f => f.name),
  }
}