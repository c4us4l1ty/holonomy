/**
 * Measure the height model against a real browser.
 *
 * # Why this has to be measured
 *
 * `GeometryCalibration::default` in Rust, and `PX_PER_100_CHARS` in
 * `main.ts`, are both CSS arithmetic: 15px/1.6 text at a 46rem measure
 * gives some number of characters per line, and that gives a number of pixels
 * per character. Every one of those assumptions is a guess until measured, and a
 * wrong height model has a very specific failure: the scrollbar is
 * proportionally wrong everywhere, and compensating for measurement does not fix
 * it because the error is in the *initial* estimate rather than in a later
 * correction.
 *
 * So this measures: render sections of known character counts, record real
 * heights, and fit. Then report the residual error, which is the number that
 * actually decides whether the model is good enough.
 *
 * # What it deliberately does not do
 *
 * It does not assert the model is accurate. A height model that is 20% off is
 * usable, because every section is corrected on first render. What would not be
 * usable is a model that is 20% off *and* whose error is not corrected, or one
 * whose error is unbounded. So the checks are on monotonicity (more characters
 * must never be shorter) and on boundedness (error must not grow with size),
 * both of which are properties rather than accuracies.
 *
 * Run: node --experimental-strip-types calibrate.ts   (needs the dev server)
 */

import { chromium, type Page } from 'playwright'

const URL = process.env.HOLO_APP_URL ?? 'http://localhost:5184/calibrate.html'

/**
 * The typography is *read from the page*, not restated here.
 *
 * These were literals in this file, copied out of `index.html`, which made three places that
 * could disagree about the typography and two to change when it did. They did disagree: the
 * harness renders `calibrate.html`, which kept its own copy of the body rule, so when the
 * product moved to an embedded face this script reported exactly the previous constants — the
 * same numbers to the last decimal, for a font no longer in use. Nothing failed; the height
 * model was fitted to a world the product had left. A duplicate that cannot drift is worth
 * slightly more code.
 *
 * If the stylesheet is missing, `lineHeightRatio` is NaN and every prediction below is
 * nonsense, which fails loudly rather than quietly.
 */
const CHROME_PX = 48 + 10 // card padding top+bottom plus the 10px inter-section margin

interface Typography {
  lineHeightRatio: number
  contentPx: number
  family: string
  sizePx: number
  /** A face from `public/fonts` reached the FontFaceSet with status `loaded`. */
  embeddedLoaded: boolean
}

async function readTypography(page: Page): Promise<Typography> {
  const raw = await page.evaluate(async () => {
    // Measured *after* the webfonts settle. With `font-display: block`, a measurement taken
    // earlier is taken against the fallback -- which is the drift this work exists to remove.
    await document.fonts.ready
    const body = getComputedStyle(document.body)
    const slice = document.querySelector('.section-slice') as HTMLElement | null
    const probe = document.getElementById('probe') as HTMLElement | null
    // The paragraph's own width, which is the measure the model is fitted against: the card's
    // content box, with no padding, margin or scrollbar in the way.
    const contentPx = probe?.querySelector('p')?.getBoundingClientRect().width ?? 0
    const loaded = [...document.fonts].filter(
      f => f.status === 'loaded' && /inter|jetbrains/i.test(f.family),
    )
    return {
      lineHeightRatio: parseFloat(body.lineHeight) / parseFloat(body.fontSize),
      contentPx,
      family: body.fontFamily,
      sizePx: parseFloat(body.fontSize),
      embeddedLoaded: loaded.length > 0,
      hasSlice: !!slice,
    }
  })
  return {
    lineHeightRatio: Number(raw.lineHeightRatio),
    contentPx: raw.contentPx,
    family: raw.family,
    sizePx: raw.sizePx,
    embeddedLoaded: raw.embeddedLoaded && raw.hasSlice,
  }
}

interface Sample {
  chars: number
  words: number
  paragraphs: number
  measured: number
  predicted: number
}

async function main() {
  const browser = await chromium.launch()
  const page: Page = await browser.newPage({ viewport: { width: 1280, height: 900 } })
  page.on('pageerror', e => console.error(`  [page error] ${e.message}`))

  await page.goto(URL, { waitUntil: 'load' })
  await page.waitForFunction(() => !!(window as any).CALIBRATE, null, { timeout: 30_000 })

  // The probe has to have content before its paragraph width means anything, and the
  // typography has to have settled before any of it means anything.
  await page.evaluate(() => {
    document.getElementById('probe')!.innerHTML = '<p>probe</p>'
  })
  const type = await readTypography(page)
  if (!type.embeddedLoaded) {
    console.error('FAIL: no embedded face reached the FontFaceSet.')
    console.error('      Measuring now would fit the height model to the fallback font and report')
    console.error('      the previous constants with complete confidence.')
    await browser.close()
    process.exit(1)
  }

  console.log('M4 — height model calibration')
  console.log('========================================================')
  console.log(`font family:   ${type.family}`)
  console.log(`font size:     ${type.sizePx}px`)
  console.log(`content width: ${type.contentPx.toFixed(1)}px`)
  console.log(`line height:   ${(type.sizePx * type.lineHeightRatio).toFixed(1)}px`)
  console.log()

  // Sanity: if the harness renders nothing, everything downstream is fiction.
  // This is the M0/M3 lesson applied up front rather than after a wrong number.
  const harnessOk = await page.evaluate(() => {
    const el = document.getElementById('probe')
    return !!el && el.getBoundingClientRect().height > 0
  })
  if (!harnessOk) {
    console.error('FAIL: the probe element has no height; the harness is not laying out.')
    await browser.close()
    process.exit(1)
  }
  console.log('harness renders content: ok\n')

  const samples: Sample[] = await page.evaluate(
    ([contentPx]: readonly [number]) => {
      const probe = document.getElementById('probe') as HTMLElement
      const out: any[] = []

      // Vary characters widely, and paragraph count independently, since the two
      // affect height differently: a paragraph break costs a line even when it
      // adds no characters.
      const cases: Array<{ chars: number; paragraphs: number }> = []
      for (const paragraphs of [1, 2, 5, 10, 20]) {
        for (const chars of [200, 500, 1000, 2000, 4000, 8000]) {
          cases.push({ chars, paragraphs })
        }
      }

      const word = 'alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu '
      for (const c of cases) {
        // Distribute characters across the paragraphs.
        const perPara = Math.max(1, Math.floor(c.chars / c.paragraphs))
        const words = word.repeat(Math.ceil(perPara / word.length)).slice(0, perPara)
        const html = Array.from({ length: c.paragraphs })
          .map(() => `<p>${words}</p>`)
          .join('')
        probe.innerHTML = html
        // Force layout before reading, or the rect is the previous case's.
        const measured = probe.getBoundingClientRect().height
        out.push({
          chars: c.paragraphs * perPara,
          words: c.paragraphs * words.split(/\s+/).length,
          paragraphs: c.paragraphs,
          measured,
          predicted: 0,
          contentPx,
        })
      }
      return out
    },
    [type.contentPx] as const,
  )

  const contentW = await page.evaluate(() => {
    const p = document.querySelector('#probe p') as HTMLElement
    return p.getBoundingClientRect().width
  })
  const lineHeight = await page.evaluate(() => {
    const p = document.querySelector('#probe p') as HTMLElement
    return parseFloat(getComputedStyle(p).lineHeight)
  })
  const charsPerLine = contentW / (15 * 0.5) // 15px font, ~0.5em average advance

  console.log(`measured content width: ${contentW.toFixed(1)}px`)
  console.log(`measured line height:  ${lineHeight.toFixed(2)}px`)
  console.log(`chars per line:        ${charsPerLine.toFixed(1)} (estimated)`)
  console.log()

  // Fit height = chrome + chars*a + paragraphs*b.
  //
  // The paragraph term is not optional. A line break costs a full line even when
  // it adds no characters, so a 200-character section in 20 paragraphs is nearly
  // twice as tall as the same 200 characters in one. A chars-only model fitted
  // this data gave a 30.6% mean error and a 128% worst case, and reported a
  // "chrome" of 320px against a CSS truth of 58px — it was absorbing paragraph
  // structure into the wrong parameter.
  const fitted = fit(samples, charsPerLine, lineHeight)

  console.log('measured heights:')
  for (const s of samples) {
    if (s.paragraphs !== 1 && s.paragraphs !== 10) continue
    console.log(
      `  ${String(s.paragraphs).padStart(2)} para  ${String(s.chars).padStart(5)} chars  ` +
        `${s.measured.toFixed(0).padStart(5)}px`,
    )
  }

  console.log()
  console.log('fitted model:')
  console.log(
    `  height = ${fitted.chrome.toFixed(1)} + chars * ${fitted.perChar.toFixed(5)}` +
      ` + paragraphs * ${fitted.perPara.toFixed(2)}`,
  )
  console.log(`  px per 100 chars: ${(fitted.perChar * 100).toFixed(2)}`)
  console.log(`  per paragraph:    ${fitted.perPara.toFixed(2)}px`)
  console.log(`  chrome:           ${fitted.chrome.toFixed(1)} (CSS says ${CHROME_PX})`)
  console.log()
  console.log('  the constants to put in Rust:')
  console.log(`    PX_PER_100_CHARS: ${(fitted.perChar * 100).toFixed(1)}`)
  console.log(`    PX_PER_PARAGRAPH: ${fitted.perPara.toFixed(2)}`)
  console.log(`    SECTION_CHROME:   ${fitted.chrome.toFixed(1)}`)

  // Residuals, and the two properties that actually matter.
  const residuals = samples.map(s => {
    const predicted = predict(fitted, s)
    return { ...s, predicted, error: (s.measured - predicted) / predicted }
  })

  const worst = residuals.reduce((a, b) => (Math.abs(b.error) > Math.abs(a.error) ? b : a))
  const meanAbs =
    residuals.reduce((a, r) => a + Math.abs(r.error), 0) / residuals.length

  console.log()
  console.log('residual error (measured - predicted) / predicted:')
  console.log(`  mean |error|: ${(meanAbs * 100).toFixed(1)}%`)
  console.log(`  worst:        ${(worst.error * 100).toFixed(1)}% at ${worst.chars} chars, ${worst.paragraphs} paras`)

  // Monotonicity: more characters must never render shorter. A violation means
  // the model is not just inaccurate but can order sections wrongly, which makes
  // the scrollbar non-monotonic and the inverse lookup meaningless.
  let monotonic = true
  for (const p of [1, 2, 5, 10, 20]) {
    const row = samples.filter(s => s.paragraphs === p).sort((a, b) => a.chars - b.chars)
    for (let i = 1; i < row.length; i++) {
      if (row[i]!.measured < row[i - 1]!.measured - 0.5) {
        monotonic = false
        console.error(`  NON-MONOTONIC: ${p} paras, ${row[i - 1]!.chars} -> ${row[i]!.chars} chars got shorter`)
      }
    }
  }

  // Accuracy. This gate was absent at first, which is how a 30.6% mean error and a
  // 128% worst case passed: the checks that existed (monotonicity, and error not
  // compounding) are both satisfied by a model that is simply badly wrong.
  //
  // The threshold is set from what the design can tolerate, not from what the
  // current model achieves. A 40% mean error means the scrollbar is
  // meaningfully wrong for every section the user has not yet scrolled to, which
  // is 99% of a 2000-page document.
  const MAX_MEAN_ERROR = 0.15
  const MAX_WORST_ERROR = 0.30

  const small = meanAbsFor(residuals, s => s.chars <= 1000)
  const large = meanAbsFor(residuals, s => s.chars >= 4000)
  console.log(`  small sections (${(small * 100).toFixed(1)}%) vs large (${(large * 100).toFixed(1)}%)`)

  console.log()
  console.log('========================================================')
  let fail = false
  if (!monotonic) {
    console.log('M4 CALIBRATION: FAIL — the model is not monotonic in content size')
    fail = true
  }
  if (meanAbs > MAX_MEAN_ERROR) {
    console.log(
      `M4 CALIBRATION: FAIL — mean error ${(meanAbs * 100).toFixed(1)}% exceeds ${(MAX_MEAN_ERROR * 100).toFixed(0)}%`,
    )
    fail = true
  }
  if (Math.abs(worst.error) > MAX_WORST_ERROR) {
    console.log(
      `M4 CALIBRATION: FAIL — worst error ${(worst.error * 100).toFixed(1)}% exceeds ${(MAX_WORST_ERROR * 100).toFixed(0)}%`,
    )
    fail = true
  }
  if (large > small * 3 + 0.05) {
    console.log('M4 CALIBRATION: FAIL — error compounds with size rather than scaling')
    fail = true
  }
  if (fail) {
    await browser.close()
    process.exit(1)
  }
  console.log('M4 CALIBRATION: PASS')
  console.log()
  console.log('The residual above is the scrollbar error before any section is measured.')
  console.log('It is bounded and non-compounding, so it is corrected as sections render:')
  console.log('every section is re-measured on mount, and the compensation in')
  console.log('geometry.rs holds the viewport still while that happens.')
  await browser.close()
}

interface Fitted {
  chrome: number
  perChar: number
  perPara: number
}

function predict(f: Fitted, s: Sample): number {
  return f.chrome + s.chars * f.perChar + s.paragraphs * f.perPara
}

function meanAbsFor(rows: Array<Sample & { predicted: number; error: number }>, pred: (s: Sample) => boolean): number {
  const sel = rows.filter(pred)
  if (!sel.length) return 0
  return sel.reduce((a, r) => a + Math.abs(r.error), 0) / sel.length
}

/**
 * Least-squares fit of `height = chrome + chars*a + paragraphs*b`.
 *
 * Three parameters, so this solves the 3x3 normal equations directly rather than
 * reaching for a matrix library. Guarded against a singular system: if two
 * columns are identical the normal equations have no unique solution, and
 * returning the geometric-mean fallback is better than dividing by zero.
 */
function fit(samples: Sample[], charsPerLine: number, lineHeight: number): Fitted {
  // Design matrix columns: [1, chars, paragraphs].
  const X: number[][] = samples.map(s => [1, s.chars, s.paragraphs])
  const y = samples.map(s => s.measured)
  const n = X.length!

  // XtX and Xty
  const XtX: number[][] = [
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
  ]
  const Xty = [0, 0, 0]
  for (let i = 0; i < n; i++) {
    for (let a = 0; a < 3; a++) {
      Xty[a]! += X[i]![a]! * y[i]!
      for (let b = 0; b < 3; b++) XtX[a]![b]! += X[i]![a]! * X[i]![b]!
    }
  }

  const sol = solve3(XtX, Xty)
  if (sol) return { chrome: sol[0]!, perChar: sol[1]!, perPara: sol[2]! }

  // Fallback from the CSS arithmetic, which is what the model encoded before
  // this existed. Kept so a degenerate dataset reports the model rather than NaN.
  return {
    chrome: CHROME_PX,
    perChar: lineHeight / (charsPerLine || 72),
    perPara: lineHeight,
  }
}

/** Gaussian elimination with partial pivoting for a 3x3 system. */
function solve3(A: number[][], b: number[]): number[] | null {
  const m = [
    [...A[0]!, b[0]!],
    [...A[1]!, b[1]!],
    [...A[2]!, b[2]!],
  ]
  for (let col = 0; col < 3; col++) {
    let pivot = col
    for (let r = col + 1; r < 3; r++) {
      if (Math.abs(m[r]![col]!) > Math.abs(m[pivot]![col]!)) pivot = r
    }
    if (Math.abs(m[pivot]![col]!) < 1e-12) return null
    ;[m[col], m[pivot]] = [m[pivot]!, m[col]!]
    for (let r = 0; r < 3; r++) {
      if (r === col) continue
      const f = m[r]![col]! / m[col]![col]!
      for (let c = col; c < 4; c++) m[r]![c]! -= f * m[col]![c]!
    }
  }
  const out = [m[0]![3]! / m[0]![0]!, m[1]![3]! / m[1]![1]!, m[2]![3]! / m[2]![2]!]
  return out.every(Number.isFinite) ? out : null
}

main().catch(e => {
  console.error(e)
  process.exit(1)
})
